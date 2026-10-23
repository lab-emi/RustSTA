// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Random designs from the cells of a library and random mutation campaigns of timing sessions
//! (`STA-REQ-0458`, `STA-REQ-0605`).
//!
//! A campaign applies random sizing, buffering, rewiring and gate edits to a session and compares
//! it with a full analysis of its netlist after every `verify_every` steps; a mismatch is shrunk
//! to a minimal failing edit list with `ddmin`.

use std::collections::BTreeMap;

use rusteda_core::liberty::{Direction, Library, SeqKind};
use rusteda_core::netlist::{CellId, Conn, GateNetlist, InstId, NetId, PinDir, PinId, PortDir};
use rusteda_formats::sdc::{self, Sdc};
use ruststa::engine::analysis::AnalysisOptions;
use ruststa::engine::incr::{SessionEdit, TimingSession, shrink_failing_edits};

/// A deterministic SplitMix64 generator.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn gen_range(&mut self, upper: usize) -> usize {
        assert!(upper > 0);
        (self.next_u64() as usize) % upper
    }
}

/// The cells of a library that random designs use.
#[derive(Debug, Clone)]
pub struct LibCatalog {
    /// One-input cells: `(cell, input, output)`.
    pub unary_cells: Vec<(String, String, String)>,
    /// Combinational cells with one to three inputs: `(cell, inputs, output)`.
    pub comb_cells: Vec<(String, Vec<String>, String)>,
    /// Cells with the same pins, which a swap exchanges.
    pub swap_families: Vec<Vec<String>>,
    /// A rising-edge flip-flop: `(cell, clock, data, output)`.
    pub dff: (String, String, String, String),
    /// A latch with a positive enable: `(cell, enable, data, output)`, if the library has one.
    pub latch: Option<(String, String, String, String)>,
}

impl LibCatalog {
    pub fn from_library(lib: &Library) -> Self {
        let mut unary_cells = Vec::new();
        let mut comb_cells = Vec::new();
        let mut by_sig: BTreeMap<Vec<(String, u8)>, Vec<String>> = BTreeMap::new();
        let mut dff: Option<(String, String, String, String)> = None;
        let mut latch: Option<(String, String, String, String)> = None;

        for cell in &lib.cells {
            if cell.dont_use
                || cell.pad_cell
                || cell.is_macro_cell
                || cell.is_clock_gating
                || cell
                    .pins
                    .iter()
                    .any(|p| !matches!(p.direction, Direction::Input | Direction::Output))
            {
                continue;
            }
            let inputs: Vec<_> = cell
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Input)
                .collect();
            let outputs: Vec<_> = cell
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Output)
                .collect();

            if latch.is_none()
                && inputs.len() == 2
                && outputs.len() == 1
                && let [seq] = cell.sequentials.as_slice()
                && seq.kind == SeqKind::Latch
                && let Some(en) = inputs.iter().find(|p| p.name == seq.clock_text.trim())
                && let Some(d) = inputs.iter().find(|p| p.name != en.name)
            {
                latch = Some((
                    cell.name.clone(),
                    en.name.clone(),
                    d.name.clone(),
                    outputs[0].name.clone(),
                ));
            }
            if dff.is_none()
                && inputs.len() == 2
                && !outputs.is_empty()
                && cell.sequentials.iter().any(|s| s.kind == SeqKind::FlipFlop)
            {
                for out_pin in &outputs {
                    if let Some(re_t) = out_pin.arcs.iter().find(|t| t.timing_type == "rising_edge")
                        && let Some(clk_p) = inputs.iter().find(|p| p.name == re_t.related_pin)
                        && let Some(d_p) = inputs.iter().find(|p| p.name != clk_p.name)
                    {
                        dff = Some((
                            cell.name.clone(),
                            clk_p.name.clone(),
                            d_p.name.clone(),
                            out_pin.name.clone(),
                        ));
                        break;
                    }
                }
            }

            if !cell.sequentials.is_empty()
                || cell.has_statetable
                || outputs.len() != 1
                || !(1..=3).contains(&inputs.len())
                || inputs.iter().any(|p| p.is_clock)
                || outputs[0].three_state.is_some()
            {
                continue;
            }
            let out_pin = outputs[0];
            let all_inputs_comb = inputs.iter().all(|inp| {
                out_pin.arcs.iter().any(|t| {
                    matches!(
                        t.timing_type.as_str(),
                        "" | "combinational" | "combinational_rise" | "combinational_fall"
                    ) && (t.cell_rise.is_some() || t.cell_fall.is_some())
                        && t.related_pin == inp.name
                })
            });
            if !all_inputs_comb {
                continue;
            }

            let mut sig: Vec<(String, u8)> = cell
                .pins
                .iter()
                .map(|p| (p.name.clone(), u8::from(p.direction == Direction::Output)))
                .collect();
            sig.sort();
            by_sig.entry(sig).or_default().push(cell.name.clone());

            let in_names: Vec<String> = inputs.iter().map(|p| p.name.clone()).collect();
            if in_names.len() == 1 {
                unary_cells.push((cell.name.clone(), in_names[0].clone(), out_pin.name.clone()));
            }
            comb_cells.push((cell.name.clone(), in_names, out_pin.name.clone()));
        }

        let swap_families: Vec<Vec<String>> =
            by_sig.into_values().filter(|v| v.len() >= 2).collect();
        assert!(!unary_cells.is_empty() && !comb_cells.is_empty() && !swap_families.is_empty());
        Self {
            unary_cells,
            comb_cells,
            swap_families,
            dff: dff.expect("rising-edge DFF"),
            latch,
        }
    }

    pub fn register_in_netlist(&self, netlist: &mut GateNetlist, lib: &Library) {
        for cell in &lib.cells {
            if netlist.find_cell(&cell.name).is_some()
                || cell
                    .pins
                    .iter()
                    .any(|p| !matches!(p.direction, Direction::Input | Direction::Output))
            {
                continue;
            }
            let pins: Vec<(&str, PinDir)> = cell
                .pins
                .iter()
                .map(|p| {
                    let dir = if p.direction == Direction::Input {
                        PinDir::Input
                    } else {
                        PinDir::Output
                    };
                    (p.name.as_str(), dir)
                })
                .collect();
            if !pins.is_empty() {
                let _ = netlist.add_cell_type(&cell.name, &pins);
            }
        }
    }

    pub fn build_swap_map(&self, g: &GateNetlist) -> BTreeMap<CellId, Vec<CellId>> {
        let mut map = BTreeMap::new();
        for fam in &self.swap_families {
            let ids: Vec<CellId> = fam.iter().filter_map(|name| g.find_cell(name)).collect();
            if ids.len() >= 2 {
                for &id in &ids {
                    map.insert(id, ids.clone());
                }
            }
        }
        map
    }
}

/// The shape of a random design: ports, registers, gates and clocks.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub inputs: usize,
    pub regs: usize,
    pub comb: usize,
    pub outputs: usize,
    /// Clock ports `clk`, then `clk2`; register `i` is clocked by clock `i % clocks`.
    pub clocks: usize,
    /// The last registers that are latches, when the library has a latch.
    pub latches: usize,
}

impl Shape {
    /// A one-clock shape of `(inputs, regs, comb, outputs)`.
    pub fn of((inputs, regs, comb, outputs): (usize, usize, usize, usize)) -> Self {
        Self {
            inputs,
            regs,
            comb,
            outputs,
            clocks: 1,
            latches: 0,
        }
    }
}

pub fn parse_sdc(text: &str) -> Sdc {
    let (ast, diags) = sdc::read_str(text);
    if let Some(d) = diags.iter().find(|d| d.severity == sdc::Severity::Error) {
        panic!("SDC error {}: {}", d.code, d.message);
    }
    ast
}

/// A random design of `shape` from the cells of `lib`: ports `clk`, `in<i>`, `out<i>` (and
/// `clk2` last), gates `u_c0_<i>` and `u_c1_<i>`, registers `u_reg_<i>` and output drivers
/// `u_out_drv_<i>`.
pub fn build_netlist(lib: &Library, seed: u64, shape: &Shape) -> GateNetlist {
    let cat = LibCatalog::from_library(lib);
    let mut rng = Rng::new(seed);
    let mut g = GateNetlist::new("top").expect("GateNetlist::new");
    cat.register_in_netlist(&mut g, lib);

    let clk_idx = g.add_port("clk", PortDir::Input, None).expect("clk port");
    let mut clk_nets = vec![g.ports()[clk_idx].bits[0]];

    let mut signal_nets: Vec<NetId> = (0..shape.inputs)
        .map(|i| {
            let idx = g.add_port(&format!("in{i}"), PortDir::Input, None).unwrap();
            g.ports()[idx].bits[0]
        })
        .collect();

    let half_comb = shape.comb / 2;
    for i in 0..half_comb {
        let out_net =
            add_random_comb_gate(&mut g, &cat, &mut rng, &signal_nets, &format!("u_c0_{i}"));
        signal_nets.push(out_net);
    }

    let mut out_ports = Vec::with_capacity(shape.outputs);
    for i in 0..shape.outputs {
        let idx = g
            .add_port(&format!("out{i}"), PortDir::Output, None)
            .unwrap();
        out_ports.push(g.ports()[idx].bits[0]);
    }
    if shape.clocks > 1 {
        let idx = g.add_port("clk2", PortDir::Input, None).expect("clk2 port");
        clk_nets.push(g.ports()[idx].bits[0]);
    }

    for i in 0..shape.regs {
        let is_latch = i + shape.latches >= shape.regs && cat.latch.is_some();
        let (cell, clk_pin, d_pin, q_pin) = match (&cat.latch, is_latch) {
            (Some(l), true) => l,
            _ => &cat.dff,
        };
        let ct = g.find_cell(cell).expect("register cell type");
        let d_net = signal_nets[rng.gen_range(signal_nets.len())];
        let q_net = g.add_net(Some(&format!("n_rq_{i}")), None).unwrap();
        let ff_id = g.add_instance(&format!("u_reg_{i}"), ct).unwrap();
        g.connect(
            ff_id,
            clk_pin.as_str(),
            Conn::Net(clk_nets[i % clk_nets.len()]),
        )
        .unwrap();
        g.connect(ff_id, d_pin.as_str(), Conn::Net(d_net)).unwrap();
        g.connect(ff_id, q_pin.as_str(), Conn::Net(q_net)).unwrap();
        signal_nets.push(q_net);
    }

    for i in half_comb..shape.comb {
        let out_net =
            add_random_comb_gate(&mut g, &cat, &mut rng, &signal_nets, &format!("u_c1_{i}"));
        signal_nets.push(out_net);
    }

    for (i, &port_net) in out_ports.iter().enumerate() {
        let src_net = signal_nets[signal_nets.len() - 1 - (i % signal_nets.len())];
        let (u_cell, u_in, u_out) = &cat.unary_cells[i % cat.unary_cells.len()];
        let drv_id = g
            .add_instance(&format!("u_out_drv_{i}"), g.find_cell(u_cell).unwrap())
            .unwrap();
        g.connect(drv_id, u_in.as_str(), Conn::Net(src_net))
            .unwrap();
        g.connect(drv_id, u_out.as_str(), Conn::Net(port_net))
            .unwrap();
    }
    g
}

/// Clocks, input delays and output delays of a design of `shape`.
pub fn base_sdc(shape: &Shape) -> String {
    let mut sdc = String::from("create_clock -name clk -period 10.0 [get_ports clk]\n");
    if shape.clocks > 1 {
        sdc.push_str("create_clock -name clk2 -period 7.0 [get_ports clk2]\n");
    }
    for i in 0..shape.inputs {
        sdc.push_str(&format!(
            "set_input_delay -clock clk 0.5 [get_ports in{i}]\n"
        ));
    }
    for i in 0..shape.outputs {
        sdc.push_str(&format!(
            "set_output_delay -clock clk 0.5 [get_ports out{i}]\n"
        ));
    }
    sdc
}

/// [`base_sdc`] with timing exceptions, clock groups and case analysis on a design of `shape`
/// (at least 4 inputs, 6 flip-flops before the latches, 3 outputs and 2 clocks) from the cells of
/// `cat`
/// (`STA-REQ-0605`). Wildcards also name the gates and buffers a campaign adds.
pub fn exception_sdc(shape: &Shape, cat: &LibCatalog) -> String {
    assert!(
        shape.inputs >= 4
            && shape.regs >= 6 + shape.latches
            && shape.outputs >= 3
            && shape.clocks >= 2
    );
    let d = &cat.dff.2;
    let ck = &cat.dff.1;
    let mut sdc = base_sdc(shape);
    sdc.push_str(&format!(
        "set_false_path -from [get_ports in0]\n\
         set_false_path -to [get_pins u_reg_2/{d}]\n\
         set_false_path -from [get_pins u_reg_4/{ck}] -to [get_ports out1]\n\
         set_multicycle_path 2 -setup -to [get_pins u_reg_1/{d}]\n\
         set_multicycle_path 1 -hold -to [get_pins u_reg_1/{d}]\n\
         set_multicycle_path 3 -setup -from [get_clocks clk2] -to [get_clocks clk]\n\
         set_max_delay 3.5 -from [get_ports in1] -to [get_ports out0]\n\
         set_min_delay 0.2 -from [get_ports in2] -to [get_pins u_reg_3/{d}]\n\
         set_max_delay 6.0 -to [get_pins u_reg_5/{d}]\n\
         set_false_path -to [get_pins u_*d*/*]\n\
         set_clock_groups -logically_exclusive -group [get_clocks clk2] -group [get_clocks clk]\n\
         set_case_analysis 0 [get_ports in3]\n"
    ));
    sdc
}

pub fn add_random_comb_gate(
    g: &mut GateNetlist,
    cat: &LibCatalog,
    rng: &mut Rng,
    signal_nets: &[NetId],
    inst_name: &str,
) -> NetId {
    let out_net = g.add_net(Some(&format!("n_{inst_name}")), None).unwrap();
    let (c_name, in_ps, out_p) = &cat.comb_cells[rng.gen_range(cat.comb_cells.len())];
    let id = g
        .add_instance(inst_name, g.find_cell(c_name).unwrap())
        .unwrap();
    for p in in_ps {
        let n = signal_nets[rng.gen_range(signal_nets.len())];
        g.connect(id, p.as_str(), Conn::Net(n)).unwrap();
    }
    g.connect(id, out_p.as_str(), Conn::Net(out_net)).unwrap();
    out_net
}

pub fn net_sink_pins(g: &GateNetlist, net_id: NetId) -> Vec<PinId> {
    g.net(net_id)
        .unwrap()
        .pins
        .iter()
        .copied()
        .filter(|&pid| {
            let p = g.pin(pid).unwrap();
            let ct = g.cell(g.instance(p.inst).unwrap().cell).unwrap();
            ct.pins[p.cell_pin as usize].dir == PinDir::Input
        })
        .collect()
}

pub fn cell_in_pin(g: &GateNetlist, cid: CellId) -> String {
    let cp = g
        .cell(cid)
        .unwrap()
        .pins
        .iter()
        .find(|cp| cp.dir == PinDir::Input)
        .unwrap();
    g.symbols().resolve(cp.name).to_owned()
}

pub fn pick_random_swap(
    g: &GateNetlist,
    swap_map: &BTreeMap<CellId, Vec<CellId>>,
    rng: &mut Rng,
) -> (InstId, CellId) {
    let candidates: Vec<(InstId, CellId)> = g
        .instances()
        .filter_map(|(id, inst)| swap_map.contains_key(&inst.cell).then_some((id, inst.cell)))
        .collect();
    let (inst_id, cur_ct) = candidates[rng.gen_range(candidates.len())];
    let alts: Vec<CellId> = swap_map[&cur_ct]
        .iter()
        .copied()
        .filter(|&c| c != cur_ct)
        .collect();
    (inst_id, alts[rng.gen_range(alts.len())])
}

fn ed_conn(inst: InstId, pin: &str, net: NetId) -> SessionEdit {
    SessionEdit::ConnectPin {
        inst,
        pin_name: pin.to_owned(),
        net,
    }
}

fn ed_disc(inst: InstId, pin: &str) -> SessionEdit {
    SessionEdit::DisconnectPin {
        inst,
        pin_name: pin.to_owned(),
    }
}

fn ed_move(inst: InstId, pin: &str, new_net: NetId) -> SessionEdit {
    SessionEdit::MovePin {
        inst,
        pin_name: pin.to_owned(),
        new_net,
    }
}

fn new_session(netlist: GateNetlist, lib: &Library, sdc: &Sdc) -> TimingSession {
    TimingSession::from_sdc(
        netlist,
        vec![lib.clone()],
        sdc.clone(),
        AnalysisOptions::default(),
    )
    .expect("session")
}

/// Generates and applies `num_steps` random netlist mutations on a session of `base_netlist`,
/// verifying bitwise equality against a full analysis every `verify_every` steps and at
/// `num_steps` (`STA-REQ-0458`, `STA-REQ-0605`). Returns the forward node evaluations of the
/// steps and the nodes of the design, so that a test can check that the cones were local.
pub fn run_random_mutation_campaign(
    label: &str,
    (base_netlist, sdc): (GateNetlist, Sdc),
    lib: &Library,
    (seed, num_steps, verify_every): (u64, usize, usize),
) -> (usize, usize) {
    let cat = LibCatalog::from_library(lib);
    let mut rng = Rng::new(seed);
    let mut session = new_session(base_netlist.clone(), lib, &sdc);
    session
        .verify_against_full()
        .expect("initial session equals a full analysis");

    let swap_map = cat.build_swap_map(session.netlist());
    let unary_cts: Vec<(CellId, String, String)> = cat
        .unary_cells
        .iter()
        .filter_map(|(name, ip, op)| {
            Some((session.netlist().find_cell(name)?, ip.clone(), op.clone()))
        })
        .collect();
    let clock_nets: Vec<NetId> = ["clk", "clk2"]
        .iter()
        .filter_map(|c| session.netlist().find_net(c, None))
        .collect();
    let safe_src_nets: Vec<NetId> = session
        .netlist()
        .ports()
        .iter()
        .filter(|p| p.dir == PortDir::Input && !clock_nets.contains(&p.bits[0]))
        .map(|p| p.bits[0])
        .collect();

    let mut inserted_buffers: Vec<(InstId, String, String)> = Vec::new();
    let mut dynamic_gates: Vec<(InstId, NetId)> = Vec::new();
    let mut applied_history: Vec<SessionEdit> = Vec::with_capacity(num_steps);
    let mut next_uid = 0usize;
    let mut evaluated = 0usize;

    for step in 1..=num_steps {
        let roll = rng.gen_range(100);
        let mut did_edit = false;

        if roll < 50 {
            let (inst_id, new_ct) = pick_random_swap(session.netlist(), &swap_map, &mut rng);
            session.swap_cell(inst_id, new_ct).expect("swap_cell");
            applied_history.push(SessionEdit::SwapCell {
                inst: inst_id,
                new_cell: new_ct,
            });
            did_edit = true;
        } else if roll < 70 && inserted_buffers.len() < 24 {
            let candidate_nets: Vec<_> = session
                .netlist()
                .nets()
                .filter_map(|(nid, _net)| {
                    (!clock_nets.contains(&nid))
                        .then(|| net_sink_pins(session.netlist(), nid))
                        .filter(|sinks| !sinks.is_empty())
                        .map(|sinks| (nid, sinks))
                })
                .collect();
            if !candidate_nets.is_empty() {
                let (nid, sinks) = &candidate_nets[rng.gen_range(candidate_nets.len())];
                let chosen = sinks[..1 + rng.gen_range(sinks.len())].to_vec();
                let (buf_ct, in_p, out_p) = &unary_cts[rng.gen_range(unary_cts.len())];
                next_uid += 1;
                let (buf_name, net_name) =
                    (format!("u_rbuf_{next_uid}"), format!("n_rbuf_{next_uid}"));
                let ed = SessionEdit::InsertBuffer {
                    net: *nid,
                    loads: chosen,
                    buf_name: buf_name.clone(),
                    buf_cell: *buf_ct,
                    in_pin: in_p.clone(),
                    out_pin: out_p.clone(),
                    new_net_name: Some(net_name),
                };
                session
                    .apply_edits(std::slice::from_ref(&ed))
                    .expect("insert_buffer");
                let b_inst = session.netlist().find_instance(&buf_name).unwrap();
                inserted_buffers.push((b_inst, in_p.clone(), out_p.clone()));
                applied_history.push(ed);
                did_edit = true;
            }
        } else if roll < 85 && !inserted_buffers.is_empty() {
            let (b_inst, in_p, out_p) =
                inserted_buffers.swap_remove(rng.gen_range(inserted_buffers.len()));
            if session.netlist().instance(b_inst).is_ok() {
                let ed = SessionEdit::RemoveBuffer {
                    buf_inst: b_inst,
                    in_pin: in_p,
                    out_pin: out_p,
                };
                session
                    .apply_edits(std::slice::from_ref(&ed))
                    .expect("remove_buffer");
                applied_history.push(ed);
                did_edit = true;
            }
        } else if roll < 95 && !safe_src_nets.is_empty() {
            let g_ref = session.netlist();
            let movable: Vec<(InstId, String)> = g_ref
                .instances()
                .filter(|(_, inst)| g_ref.symbols().resolve(inst.name).starts_with("u_c"))
                .flat_map(|(inst_id, inst)| {
                    let ct = g_ref.cell(inst.cell).unwrap();
                    inst.connected.iter().filter_map(move |&pid| {
                        let cp = &ct.pins[g_ref.pin(pid).unwrap().cell_pin as usize];
                        (cp.dir == PinDir::Input)
                            .then(|| (inst_id, g_ref.symbols().resolve(cp.name).to_owned()))
                    })
                })
                .collect();
            if !movable.is_empty() {
                let (inst_id, pin_name) = movable[rng.gen_range(movable.len())].clone();
                let new_net = safe_src_nets[rng.gen_range(safe_src_nets.len())];
                let batch = if rng.gen_range(2) == 0 {
                    vec![ed_move(inst_id, &pin_name, new_net)]
                } else {
                    vec![
                        ed_disc(inst_id, &pin_name),
                        ed_conn(inst_id, &pin_name, new_net),
                    ]
                };
                session.apply_edits(&batch).expect("rewire pin");
                applied_history.extend(batch);
                did_edit = true;
            }
        } else if dynamic_gates.len() < 8 && (dynamic_gates.is_empty() || rng.gen_range(2) == 0) {
            next_uid += 1;
            let (net_name, inst_name) = (format!("n_dyn_{next_uid}"), format!("u_dyn_{next_uid}"));
            let (ct, in_p, out_p) = &unary_cts[rng.gen_range(unary_cts.len())];
            let src_net = session.netlist().ports()[1].bits[0];
            let (new_net, _) = session.add_net(Some(net_name.clone())).expect("add_net");
            let (new_inst, _) = session.add_instance(&inst_name, *ct).expect("add_instance");
            let batch = [
                ed_conn(new_inst, in_p, src_net),
                ed_conn(new_inst, out_p, new_net),
            ];
            session.apply_edits(&batch).expect("connect dyn");
            dynamic_gates.push((new_inst, new_net));
            applied_history.extend([
                SessionEdit::AddNet {
                    name: Some(net_name),
                },
                SessionEdit::AddInstance {
                    name: inst_name,
                    cell: *ct,
                },
            ]);
            applied_history.extend(batch);
            did_edit = true;
        } else if !dynamic_gates.is_empty() {
            let (d_inst, d_net) = dynamic_gates.swap_remove(rng.gen_range(dynamic_gates.len()));
            let batch = [
                SessionEdit::RemoveInstance { inst: d_inst },
                SessionEdit::RemoveNet { net: d_net },
            ];
            session.apply_edits(&batch).expect("remove dyn");
            applied_history.extend(batch);
            did_edit = true;
        }

        if !did_edit {
            let (inst_id, new_ct) = pick_random_swap(session.netlist(), &swap_map, &mut rng);
            session.swap_cell(inst_id, new_ct).expect("fallback swap");
            applied_history.push(SessionEdit::SwapCell {
                inst: inst_id,
                new_cell: new_ct,
            });
        }
        evaluated += session.last_stats().forward_nodes_evaluated;

        if step % verify_every == 0 || step == num_steps {
            if let Err(mismatch) = session.verify_against_full() {
                let shrunk = shrink_failing_edits(&applied_history, |subset| {
                    let mut probe = new_session(base_netlist.clone(), lib, &sdc);
                    probe.apply_edits(subset).is_ok() && probe.verify_against_full().is_err()
                });
                panic!(
                    "{label}: bitwise mismatch at step {step}/{num_steps}: {mismatch:?}\n\
                     shrunk minimal failing edits ({}): {shrunk:#?}",
                    shrunk.len()
                );
            }
        }
    }
    (evaluated, session.graph().num_nodes() * num_steps)
}
