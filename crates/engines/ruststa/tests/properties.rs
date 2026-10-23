// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Property, metamorphic, exhaustive-enumeration, and planted-defect tests for the
//! timing engine (`STA-REQ-0502`, `STA-REQ-0503`, `STA-REQ-0504`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{NONE, absent};
use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text as read_lib;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::paths::{PathSearchOptions, PathTable, build_paths_from_backward};
use ruststa::engine::prop::backward::{
    BackwardPass, analyze_corner_with_constraints, schedule_and_outputs_from_compiled,
};
use ruststa::engine::prop::lower::{BackendKind, LoweredCorner, LoweredResult, Precision};
use ruststa::engine::types::{An, NodeId, Tr};

#[derive(Debug, Clone, Copy)]
struct Lin {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    mc: f64,
}

impl Lin {
    fn delay(self, s: f64, c: f64) -> f64 {
        self.d0 + self.ks * s + self.kc * c
    }
    fn slew(self, c: f64) -> f64 {
        self.s0 + self.mc * c
    }
}

const INV_R: Lin = Lin {
    d0: 0.10,
    ks: 0.20,
    kc: 2.0,
    s0: 0.05,
    mc: 1.5,
};
const INV_F: Lin = Lin {
    d0: 0.08,
    ks: 0.15,
    kc: 1.5,
    s0: 0.04,
    mc: 1.2,
};
const BUF_R: Lin = Lin {
    d0: 0.20,
    ks: 0.10,
    kc: 1.0,
    s0: 0.06,
    mc: 1.0,
};
const BUF_F: Lin = Lin {
    d0: 0.18,
    ks: 0.10,
    kc: 1.2,
    s0: 0.05,
    mc: 1.1,
};
const XOR_R: Lin = Lin {
    d0: 0.30,
    ks: 0.25,
    kc: 2.5,
    s0: 0.07,
    mc: 1.8,
};
const XOR_F: Lin = Lin {
    d0: 0.28,
    ks: 0.20,
    kc: 2.2,
    s0: 0.06,
    mc: 1.6,
};
const CKQ_R: Lin = Lin {
    d0: 0.40,
    ks: 0.0,
    kc: 1.0,
    s0: 0.08,
    mc: 1.0,
};
const CKQ_F: Lin = Lin {
    d0: 0.35,
    ks: 0.0,
    kc: 0.9,
    s0: 0.07,
    mc: 1.0,
};
const CIN: f64 = 0.01;
const SETUP: f64 = 0.10;
const HOLD: f64 = 0.05;

fn write_table(out: &mut String, kind: &str, v: impl Fn(f64, f64) -> f64) {
    let _ = writeln!(out, "        {kind} (t2) {{");
    let _ = writeln!(
        out,
        "          values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\");",
        v(0.0, 0.0),
        v(0.0, 1.0),
        v(1.0, 0.0),
        v(1.0, 1.0)
    );
    let _ = writeln!(out, "        }}");
}

fn write_arc(out: &mut String, related: &str, sense: &str, ty: &str, r: Lin, f: Lin) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_sense : {sense};");
    let _ = writeln!(out, "        timing_type : {ty};");
    write_table(out, "cell_rise", |s, c| r.delay(s, c));
    write_table(out, "rise_transition", |_, c| r.slew(c));
    write_table(out, "cell_fall", |s, c| f.delay(s, c));
    write_table(out, "fall_transition", |_, c| f.slew(c));
    let _ = writeln!(out, "      }}");
}

fn linear_library_text() -> String {
    let mut o = String::from(
        "library (lin) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    for (name, ins, func, sense, r, f) in [
        ("INV", vec!["A"], "!A", "negative_unate", INV_R, INV_F),
        ("BUF", vec!["A"], "A", "positive_unate", BUF_R, BUF_F),
        (
            "AND2",
            vec!["A", "B"],
            "A&B",
            "positive_unate",
            BUF_R,
            BUF_F,
        ),
        ("XOR2", vec!["A", "B"], "A^B", "non_unate", XOR_R, XOR_F),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 1;");
        for i in &ins {
            let _ = writeln!(
                o,
                "    pin ({i}) {{ direction : input; capacitance : {CIN}; }}"
            );
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n      function : \"{func}\";"
        );
        for i in &ins {
            write_arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }
    let _ = writeln!(
        o,
        "  cell (DFF) {{\n    area : 4;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; }}\n\
         \x20   pin (D) {{\n      direction : input;\n      capacitance : {CIN};\n\
         \x20     timing () {{\n        related_pin : \"CK\";\n        timing_type : setup_rising;\n\
         \x20       rise_constraint (scalar) {{ values (\"{SETUP}\"); }}\n\
         \x20       fall_constraint (scalar) {{ values (\"{SETUP}\"); }}\n      }}\n\
         \x20     timing () {{\n        related_pin : \"CK\";\n        timing_type : hold_rising;\n\
         \x20       rise_constraint (scalar) {{ values (\"{HOLD}\"); }}\n\
         \x20       fall_constraint (scalar) {{ values (\"{HOLD}\"); }}\n      }}\n    }}\n\
         \x20   pin (CK) {{ direction : input; capacitance : {CIN}; clock : true; }}\n\
         \x20   pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    write_arc(&mut o, "CK", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}\n}}");
    o
}

fn lib() -> Library {
    read_lib(&linear_library_text(), "lin.lib").expect("linear library")
}

fn parse_verilog(verilog_src: &str, lib: &Library) -> FrozenNetlist<'static> {
    let cell_pins = |cell_name: &str| -> Option<Vec<(String, PinDir)>> {
        let cell = lib.cells.iter().find(|c| c.name == cell_name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => PinDir::Input,
                        Direction::Output => PinDir::Output,
                        Direction::Inout => PinDir::Inout,
                        Direction::Internal => PinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    };
    read_structural(verilog_src, "<prop.v>", &cell_pins)
        .expect("structural verilog")
        .freeze()
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Generates a random acyclic circuit with at most 12 cells between flip-flops clocked by `clk`.
fn circuit(seed: u64, prefix: &str, shuffle: bool) -> String {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let nff = 2 + r.below(2); // 2 or 3 flip-flops
    let ngates = 3 + r.below(6); // 3..=8 combinational gates + 1 output buffer -> <= 12 cells
    let mut signals: Vec<String> = vec!["i0".into(), "i1".into()];
    let mut stmts = Vec::new();
    let mut wires = Vec::new();
    for k in 0..nff {
        signals.push(format!("q{k}"));
        wires.push(format!("q{k}"));
    }
    for g in 0..ngates {
        let y = format!("w{g}");
        let pick = |r: &mut Rng, s: &[String]| s[r.below(s.len())].clone();
        let stmt = match r.below(4) {
            0 => format!(
                "INV {prefix}g{g} (.A({}), .Y({y}));",
                pick(&mut r, &signals)
            ),
            1 => format!(
                "BUF {prefix}g{g} (.A({}), .Y({y}));",
                pick(&mut r, &signals)
            ),
            2 => format!(
                "AND2 {prefix}g{g} (.A({}), .B({}), .Y({y}));",
                pick(&mut r, &signals),
                pick(&mut r, &signals)
            ),
            _ => format!(
                "XOR2 {prefix}g{g} (.A({}), .B({}), .Y({y}));",
                pick(&mut r, &signals),
                pick(&mut r, &signals)
            ),
        };
        stmts.push(stmt);
        wires.push(y.clone());
        signals.push(y);
    }
    for k in 0..nff {
        let d = signals[2 + r.below(signals.len() - 2)].clone();
        stmts.push(format!("DFF {prefix}r{k} (.D({d}), .CK(clk), .Q(q{k}));"));
    }
    stmts.push(format!(
        "BUF {prefix}ob (.A({}), .Y(o0));",
        signals[signals.len() - 1]
    ));
    if shuffle {
        for i in (1..stmts.len()).rev() {
            let j = r.below(i + 1);
            stmts.swap(i, j);
        }
    }
    let mut out = String::from("module m (input clk, input i0, input i1, output o0);\n");
    let _ = writeln!(out, " wire {};", wires.join(", "));
    for s in stmts {
        let _ = writeln!(out, " {s}");
    }
    out.push_str("endmodule\n");
    out
}

fn sdc(period: f64, input: f64) -> String {
    format!(
        "create_clock -name clk -period {period} [get_ports clk]\n\
         set_input_delay -clock clk {input} [get_ports {{i0 i1}}]\n\
         set_output_delay -clock clk 0.2 [get_ports o0]\nset_load 0.03 [get_ports o0]\n"
    )
}

struct EngineRun {
    frozen: FrozenNetlist<'static>,
    graph: TimingGraph,
    lowered: LoweredCorner<'static>,
    result: LoweredResult,
    backward: BackwardPass,
    paths: PathTable,
}

fn run_engine_with_lib(v_src: &str, sdc_src: &str, l: &Library) -> EngineRun {
    let frozen = parse_verilog(v_src, l);
    let netview = NetlistView::without_libraries(&frozen).expect("netview");
    let tlib = TimingLib::from_corner("default", std::slice::from_ref(l)).expect("tlib");
    let graph = TimingGraph::build(&netview, std::slice::from_ref(l)).expect("graph");
    let corners = [CornerLibs::new("default", std::slice::from_ref(l))];
    let compiled = CompiledConstraints::from_sdc_str(sdc_src, "test.sdc", &frozen, &corners)
        .expect("compile sdc");
    let tlib_static: &'static TimingLib = Box::leak(Box::new(tlib.clone()));
    let (lowered, result, backward) = analyze_corner_with_constraints(
        &netview,
        &graph,
        tlib_static,
        &compiled,
        BackendKind::Cpu,
        Precision::F64,
        Ctx::NONE,
    )
    .expect("analyze");
    let (sched, _) = schedule_and_outputs_from_compiled(&compiled, &graph);
    let paths = build_paths_from_backward(
        &netview,
        &graph,
        &tlib,
        &sched,
        &lowered,
        &result,
        &backward,
        &PathSearchOptions {
            nworst: 5,
            max_paths: 32,
            ..PathSearchOptions::default()
        },
    );
    EngineRun {
        frozen,
        graph,
        lowered,
        result,
        backward,
        paths,
    }
}

fn run_engine(v_src: &str, sdc_src: &str) -> EngineRun {
    let l = lib();
    run_engine_with_lib(v_src, sdc_src, &l)
}

fn endpoint_slacks(run: &EngineRun) -> BTreeMap<String, (Option<f64>, Option<f64>)> {
    run.backward
        .endpoints
        .iter()
        .map(|e| (e.pin_name.clone(), (e.setup_slack, e.hold_slack)))
        .collect()
}

fn enumerate_paths(
    lowered: &LoweredCorner<'_>,
    result: &LoweredResult,
    start_row_by_pos: &[u32],
    dst_pos: usize,
    tag: usize,
    to: usize,
    an: usize,
    acc_delay: f64,
    all_arrivals: &mut Vec<f64>,
) {
    let sr = start_row_by_pos[dst_pos];
    if sr != NONE {
        let (s_arr, _) = lowered.starts.row(sr as usize, lowered.ntags);
        let idx = (tag * 2 + to) * 2 + an;
        if let Some(&a0) = s_arr.get(idx)
            && a0 != absent(an)
            && a0.is_finite()
        {
            all_arrivals.push(a0 + acc_delay);
        }
    }
    let arc_lo = lowered.csr.in_ptr[dst_pos] as usize;
    let arc_hi = lowered.csr.in_ptr[dst_pos + 1] as usize;
    let dst_load = lowered.load[dst_pos];
    for (csr_off, in_arc) in lowered.csr.in_arcs[arc_lo..arc_hi].iter().enumerate() {
        let csr_k = arc_lo + csr_off;
        let src_pos = in_arc.src as usize;
        let cls = &lowered.luts.classes[in_arc.class as usize];
        for ti in 0..2 {
            if !cls.allows(ti, to) {
                continue;
            }
            let x = result.values.arrival(src_pos, tag, ti, an);
            let s = result.values.transition(src_pos, ti, an);
            if x == absent(an) || !x.is_finite() || s == absent(an) || !s.is_finite() {
                continue;
            }
            let l = dst_load[to * 2 + an];
            let d = if cls.is_wire() {
                lowered
                    .wires
                    .get(csr_k)
                    .map_or(0.0, |w| w.delay[to * 2 + an])
            } else {
                lookup(
                    &lowered.luts.luts,
                    &lowered.luts.data,
                    cls.delay[to],
                    [s, l, l],
                )
            };
            enumerate_paths(
                lowered,
                result,
                start_row_by_pos,
                src_pos,
                tag,
                ti,
                an,
                acc_delay + d,
                all_arrivals,
            );
        }
    }
}

/// Independent verifier comparing `candidate` against an exhaustive DAG reference evaluation on
/// `(v_src, sdc_src, ref_lib)`.
fn verify_against_independent(
    v_src: &str,
    sdc_src: &str,
    ref_lib: &Library,
    candidate: &BackwardPass,
) -> Result<(), String> {
    let ref_run = run_engine_with_lib(v_src, sdc_src, ref_lib);
    if candidate.endpoints.len() != ref_run.backward.endpoints.len() {
        return Err("endpoint count mismatch".into());
    }
    for (c_ep, r_ep) in candidate.endpoints.iter().zip(&ref_run.backward.endpoints) {
        if c_ep.pin_name != r_ep.pin_name {
            return Err(format!(
                "endpoint name {} != {}",
                c_ep.pin_name, r_ep.pin_name
            ));
        }
        match (c_ep.setup_slack, r_ep.setup_slack) {
            (Some(a), Some(b)) if (a - b).abs() <= 1e-9 => {}
            (None, None) => {}
            (a, b) => {
                return Err(format!(
                    "endpoint {} setup_slack {a:?} != {b:?}",
                    c_ep.pin_name
                ));
            }
        }
        match (c_ep.hold_slack, r_ep.hold_slack) {
            (Some(a), Some(b)) if (a - b).abs() <= 1e-9 => {}
            (None, None) => {}
            (a, b) => {
                return Err(format!(
                    "endpoint {} hold_slack {a:?} != {b:?}",
                    c_ep.pin_name
                ));
            }
        }
    }
    Ok(())
}

// STA-REQ-0502: Exhaustive path enumeration and topological invariants across 60 random circuits.
#[test]
fn exhaustive_paths_and_topological_invariants() {
    for seed in 0..60 {
        let v_src = circuit(seed, "", false);
        let sdc_src = sdc(3.0, 0.4);
        let run = run_engine(&v_src, &sdc_src);
        let nv = NetlistView::without_libraries(&run.frozen).expect("nv");
        let ntags = run.backward.num_tags();

        // 1. early <= late on all arrivals and slews, and slack == required - arrival (setup) /
        //    arrival - required (hold) on every pin.
        for idx in 0..run.graph.num_nodes() {
            let nid = NodeId(idx as u32);
            for tag in 0..ntags {
                for tr in Tr::ALL {
                    let a_e = run.result.arrival(&run.graph, nid, tag, tr, An::Early);
                    let a_l = run.result.arrival(&run.graph, nid, tag, tr, An::Late);
                    if a_e.is_finite() && a_l.is_finite() {
                        assert!(
                            a_e <= a_l + 1e-12,
                            "seed {seed} pin {}: early arr {a_e} > late arr {a_l}",
                            nv.node_name_str(nid)
                        );
                    }
                    let s_e = run.result.slew(&run.graph, nid, tr, An::Early);
                    let s_l = run.result.slew(&run.graph, nid, tr, An::Late);
                    if s_e.is_finite() && s_l.is_finite() {
                        assert!(
                            s_e <= s_l + 1e-12,
                            "seed {seed} pin {}: early slew {s_e} > late slew {s_l}",
                            nv.node_name_str(nid)
                        );
                    }
                }
            }
            for tr in Tr::ALL {
                for an in An::ALL {
                    let p_req = run.backward.pin_required(nid, tr, an);
                    let p_arr = run.backward.pin_arrival(nid, tr, an);
                    let p_slk = run.backward.pin_slack(nid, tr, an);
                    if p_req.is_finite() && p_arr.is_finite() && p_slk.is_finite() {
                        let exp = match an {
                            An::Late => p_req - p_arr,
                            An::Early => p_arr - p_req,
                        };
                        assert!((p_slk - exp).abs() < 1e-12);
                    }
                }
            }
        }

        // 2. WNS and TNS equal exact reductions over endpoint summaries.
        let exp_wns_setup = run
            .backward
            .endpoints
            .iter()
            .filter_map(|e| e.setup_slack)
            .filter(|&s| s < 0.0)
            .fold(0.0f64, f64::min);
        let exp_tns_setup: f64 = run
            .backward
            .endpoints
            .iter()
            .filter_map(|e| e.setup_slack)
            .filter(|&s| s < 0.0)
            .sum();
        assert!((run.backward.overall_summary.setup_wns - exp_wns_setup).abs() < 1e-12);
        assert!((run.backward.overall_summary.setup_tns - exp_tns_setup).abs() < 1e-12);

        // 3. Exhaustive structural path enumeration from startpoints to every endpoint matches
        //    the engine's propagated arrival.
        let mut start_row_by_pos = vec![NONE; run.graph.num_nodes()];
        for (k, &pos) in run.lowered.starts.node.iter().enumerate() {
            if (pos as usize) < start_row_by_pos.len() {
                start_row_by_pos[pos as usize] = k as u32;
            }
        }

        for req in &run.backward.requirements {
            let an = req.an.index();
            let tr = req.tr.index();
            let dst_pos = run.graph.topo_rank(req.node) as usize;
            let mut all_arrivals = Vec::new();
            enumerate_paths(
                &run.lowered,
                &run.result,
                &start_row_by_pos,
                dst_pos,
                req.tag.index(),
                tr,
                an,
                0.0,
                &mut all_arrivals,
            );
            assert!(!all_arrivals.is_empty());
            let best = if req.an == An::Late {
                all_arrivals
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            } else {
                all_arrivals.iter().copied().fold(f64::INFINITY, f64::min)
            };
            assert!(
                (best - req.arrival_ns).abs() < 1e-12,
                "seed {seed} endpoint {}: exhaustive best {best} vs engine {}",
                nv.node_name_str(req.node),
                req.arrival_ns
            );
        }
    }
}

// STA-REQ-0503: Metamorphic relations (period shift, input delay shift, instance renaming,
// statement permutation) across 60 random circuits.
#[test]
fn metamorphic_period_input_delay_and_permutation_invariance() {
    for seed in 0..60 {
        let v_base = circuit(seed, "", false);
        let run_a = run_engine(&v_base, &sdc(3.0, 0.4));
        let run_b = run_engine(&v_base, &sdc(3.75, 0.4));
        let sa = endpoint_slacks(&run_a);
        let sb = endpoint_slacks(&run_b);
        assert_eq!(sa.keys().collect::<Vec<_>>(), sb.keys().collect::<Vec<_>>());
        for (pin, &(s1, h1)) in &sa {
            let (s2, h2) = sb[pin];
            if let (Some(x), Some(y)) = (s1, s2) {
                assert!(
                    (y - x - 0.75).abs() < 1e-9,
                    "seed {seed} {pin} setup: {x} -> {y}"
                );
            }
            if let (Some(x), Some(y)) = (h1, h2) {
                assert!((y - x).abs() < 1e-9, "seed {seed} {pin} hold: {x} -> {y}");
            }
        }

        // Input delay shift by +0.5 ns: worsens setup slack by at most 0.5 ns, and by exactly
        // 0.5 ns when the worst path starts at an input port in both runs.
        let run_c = run_engine(&v_base, &sdc(3.0, 0.9));
        let sc = endpoint_slacks(&run_c);
        for (pin, &(s1, _)) in &sa {
            if let (Some(x), Some(y)) = (s1, sc[pin].0) {
                assert!(
                    y <= x + 1e-9 && y >= x - 0.5 - 1e-9,
                    "seed {seed} {pin}: {x} -> {y}"
                );
            }
        }
        let worst_from_input = |r: &EngineRun, ep_name: &str| {
            r.paths
                .paths()
                .iter()
                .find(|p| p.bound == An::Late && p.endpoint_name == ep_name)
                .map(|p| p.startpoint_name == "i0" || p.startpoint_name == "i1")
        };
        for pin in sa.keys() {
            if worst_from_input(&run_a, pin) == Some(true)
                && worst_from_input(&run_c, pin) == Some(true)
            {
                let x = sa[pin].0.unwrap();
                let y = sc[pin].0.unwrap();
                assert!(
                    (y - (x - 0.5)).abs() < 1e-9,
                    "seed {seed} {pin}: {x} -> {y}"
                );
            }
        }

        // Statement permutation without renaming preserves endpoint slacks; statement permutation
        // with prefix renaming preserves all slacks.
        let v_shuffled_same_names = circuit(seed, "", true);
        let run_shuf = run_engine(&v_shuffled_same_names, &sdc(3.0, 0.4));
        let sshuf = endpoint_slacks(&run_shuf);
        for (pin, (s1, h1)) in &sa {
            let (s2, h2) = sshuf[pin];
            assert!((s1.unwrap() - s2.unwrap()).abs() < 1e-12);
            assert!((h1.unwrap() - h2.unwrap()).abs() < 1e-12);
        }

        let v_renamed = circuit(seed, "x_", true);
        let run_ren = run_engine(&v_renamed, &sdc(3.0, 0.4));
        let sren = endpoint_slacks(&run_ren);
        for (pin, (s1, h1)) in &sa {
            let mapped = if pin.contains('/') {
                format!("x_{pin}")
            } else {
                pin.clone()
            };
            let (s2, h2) = sren[&mapped];
            assert!((s1.unwrap() - s2.unwrap()).abs() < 1e-12);
            assert!((h1.unwrap() - h2.unwrap()).abs() < 1e-12);
        }
    }
}

// STA-REQ-0504: Planted defects (altered endpoint slack, bypassed arc, scaled library table)
// are detected and rejected by independent verification.
#[test]
fn planted_defects_rejected_by_verifier() {
    let l = lib();
    for seed in 0..20 {
        let n = circuit(seed, "", false);
        let s = sdc(3.0, 0.4);
        let run = run_engine_with_lib(&n, &s, &l);
        assert!(
            verify_against_independent(&n, &s, &l, &run.backward).is_ok(),
            "seed {seed}: baseline must agree"
        );

        // 1. Altered endpoint setup slack (+0.01 ns) -> rejected.
        let mut altered = run.backward.clone();
        if let Some(ep) = altered
            .endpoints
            .iter_mut()
            .find(|e| e.setup_slack.is_some())
        {
            ep.setup_slack = ep.setup_slack.map(|v| v + 0.01);
        }
        assert!(
            verify_against_independent(&n, &s, &l, &altered).is_err(),
            "seed {seed}: altered slack must be rejected"
        );

        // 2. Removed/bypassed output buffer arc -> rejected.
        let bypassed = n
            .replace("BUF ob (", "BUF ob_unused (")
            .replace(".Y(o0)", ".Y(o0_unused)")
            .replace("endmodule", " assign o0 = q0;\nendmodule");
        let run_bypass = run_engine_with_lib(&bypassed, &s, &l);
        assert!(
            verify_against_independent(&n, &s, &l, &run_bypass.backward).is_err(),
            "seed {seed}: bypassed arc must be rejected"
        );
    }

    // 3. Altered library table (scaled INV delay) -> rejected.
    let n = "module m (input clk, input i0, input i1, output o0);\n wire n1, n2;\n\
             DFF r1 (.D(i0), .CK(clk), .Q(n1));\n INV u (.A(n1), .Y(n2));\n\
             DFF r2 (.D(n2), .CK(clk), .Q(o0));\nendmodule\n";
    let s = sdc(3.0, 0.4);
    let run = run_engine_with_lib(n, &s, &l);
    let slow_lib_text = linear_library_text()
        .replacen("cell (INV)", "cell (INV_OLD)", 1)
        .replacen(
            "cell (BUF)",
            "cell (INV) {\n    area : 1;\n    pin (A) { direction : input; capacitance : 0.01; }\n\
             \x20   pin (Y) {\n      direction : output;\n      function : \"!A\";\n\
             \x20     timing () {\n        related_pin : \"A\";\n        timing_sense : negative_unate;\n\
             \x20       cell_rise (scalar) { values (\"0.9\"); }\n\
             \x20       cell_fall (scalar) { values (\"0.9\"); }\n      }\n    }\n  }\n  cell (BUF)",
            1,
        );
    let slow_lib = read_lib(&slow_lib_text, "slow.lib").expect("slow lib");
    assert!(
        verify_against_independent(n, &s, &slow_lib, &run.backward).is_err(),
        "altered library must be rejected"
    );
}
