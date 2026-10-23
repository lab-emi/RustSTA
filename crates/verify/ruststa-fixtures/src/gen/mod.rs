// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Deterministic SplitMix64 PRNG, sorted-cell catalog, and seeded `GateNetlist` / structural
//! Verilog topology generators.

pub mod mutate;

use std::collections::BTreeMap;
use std::fmt::Write;

use rusteda_core::liberty::{Direction, Library, SeqKind};
use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;

pub use mutate::{Mutation, apply_mutations, gen_mutations, shrink_mutations};

/// Deterministic 64-bit SplitMix64 pseudo-random number generator (zero external dependencies).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Creates a new generator with the given `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Returns the next 64-bit pseudo-random word.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Returns a deterministic index in `0..bound` (or `0` when `bound == 0`).
    pub fn next_usize(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            0
        } else {
            (self.next_u64() as usize) % bound
        }
    }

    /// Returns a deterministic float in `[0.0, 1.0)`.
    pub fn next_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
    }

    /// Fisher-Yates shuffle of `slice`.
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

/// Pin layout of a library cell sorted by cell name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSpec {
    pub name: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Sorted catalog of standard cells extracted from a [`Library`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellCatalog {
    pub all_sorted: Vec<CellSpec>,
    pub inverters: Vec<String>,
    pub buffers: Vec<String>,
    pub gates2: Vec<String>,
    pub muxes: Vec<String>,
    pub dffs_pos: Vec<String>,
    pub dffs_neg: Vec<String>,
    pub latches_pos: Vec<String>,
    pub latches_neg: Vec<String>,
    pub icgs: Vec<String>,
    pub tristates: Vec<String>,
    pub tie_hi: Vec<String>,
    pub tie_lo: Vec<String>,
    pin_map: BTreeMap<String, Vec<(String, PinDir)>>,
}

impl CellCatalog {
    /// Extracts a deterministic cell catalog from `lib`, sorted by cell name.
    #[must_use]
    pub fn from_library(lib: &Library) -> Self {
        let mut cells: Vec<_> = lib
            .cells
            .iter()
            .filter(|c| !c.dont_use && !c.pins.is_empty())
            .collect();
        cells.sort_by(|a, b| a.name.cmp(&b.name));

        let mut all_sorted = Vec::new();
        let mut pin_map = BTreeMap::new();
        let mut inverters = Vec::new();
        let mut buffers = Vec::new();
        let mut gates2 = Vec::new();
        let mut muxes = Vec::new();
        let mut dffs_pos = Vec::new();
        let mut dffs_neg = Vec::new();
        let mut latches_pos = Vec::new();
        let mut latches_neg = Vec::new();
        let mut icgs = Vec::new();
        let mut tristates = Vec::new();
        let mut tie_hi = Vec::new();
        let mut tie_lo = Vec::new();

        for c in cells {
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            let mut pins = Vec::new();
            for p in &c.pins {
                let dir = match p.direction {
                    Direction::Input | Direction::Internal => {
                        inputs.push(p.name.clone());
                        PinDir::Input
                    }
                    Direction::Output => {
                        outputs.push(p.name.clone());
                        PinDir::Output
                    }
                    Direction::Inout => {
                        outputs.push(p.name.clone());
                        PinDir::Inout
                    }
                };
                pins.push((p.name.clone(), dir));
            }
            pin_map.insert(c.name.clone(), pins);
            all_sorted.push(CellSpec {
                name: c.name.clone(),
                inputs: inputs.clone(),
                outputs: outputs.clone(),
            });

            let lname = c.name.to_ascii_lowercase();
            let has_ff = c.sequentials.iter().any(|s| s.kind == SeqKind::FlipFlop);
            let has_latch = c.sequentials.iter().any(|s| s.kind == SeqKind::Latch);
            if c.is_clock_gating || lname.contains("icg") {
                icgs.push(c.name.clone());
            } else if has_ff {
                if lname.contains("_n") {
                    dffs_neg.push(c.name.clone());
                } else {
                    dffs_pos.push(c.name.clone());
                }
            } else if has_latch {
                if lname.contains("_n") {
                    latches_neg.push(c.name.clone());
                } else {
                    latches_pos.push(c.name.clone());
                }
            } else if c.pins.iter().any(|p| p.three_state.is_some()) || lname.contains("tbuf") {
                tristates.push(c.name.clone());
            } else if inputs.is_empty() && outputs.len() == 1 {
                if lname.contains("hi") {
                    tie_hi.push(c.name.clone());
                } else if lname.contains("lo") {
                    tie_lo.push(c.name.clone());
                }
            } else if inputs.len() == 1 && outputs.len() == 1 {
                if lname.contains("inv") {
                    inverters.push(c.name.clone());
                } else if lname.contains("buf") {
                    buffers.push(c.name.clone());
                }
            } else if inputs.len() == 2 && outputs.len() == 1 {
                gates2.push(c.name.clone());
            } else if inputs.len() == 3 && outputs.len() == 1 && lname.contains("mux") {
                muxes.push(c.name.clone());
            }
        }

        Self {
            all_sorted,
            inverters,
            buffers,
            gates2,
            muxes,
            dffs_pos,
            dffs_neg,
            latches_pos,
            latches_neg,
            icgs,
            tristates,
            tie_hi,
            tie_lo,
            pin_map,
        }
    }

    /// Returns the catalog for the closed-form `wt_15` library.
    #[must_use]
    pub fn default_wt15() -> Self {
        let text = crate::liberty_closed::wt_15();
        let lib = read_text(&text, "wt_15").expect("wt_15 parses cleanly");
        Self::from_library(&lib)
    }

    /// Looks up the pin list of `cell_name` for [`read_structural`].
    #[must_use]
    pub fn cell_pins(&self, cell_name: &str) -> Option<Vec<(String, PinDir)>> {
        self.pin_map.get(cell_name).cloned()
    }
}

/// One instance in a generated netlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstSpec {
    pub inst_name: String,
    pub cell_name: String,
    pub conns: Vec<(String, String)>,
}

/// A generated structural netlist fixture with both Verilog text and `GateNetlist` conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetlistFixture {
    pub name: String,
    pub top: String,
    pub clocks: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub wires: Vec<String>,
    pub instances: Vec<InstSpec>,
    pub verilog: String,
}

impl NetlistFixture {
    /// Builds the `verilog` source from the structured port/wire/instance lists.
    #[must_use]
    pub fn from_parts(
        name: impl Into<String>,
        top: impl Into<String>,
        clocks: Vec<String>,
        inputs: Vec<String>,
        outputs: Vec<String>,
        wires: Vec<String>,
        instances: Vec<InstSpec>,
    ) -> Self {
        let name = name.into();
        let top = top.into();
        let mut f = Self {
            name,
            top,
            clocks,
            inputs,
            outputs,
            wires,
            instances,
            verilog: String::new(),
        };
        f.rebuild_verilog();
        f
    }

    /// Recomputes `self.verilog` from the current ports, wires, and instances.
    pub fn rebuild_verilog(&mut self) {
        let mut v = String::new();
        let _ = writeln!(v, "// Generated by ruststa-fixtures; do not edit.");
        let mut all_ports = Vec::new();
        for c in &self.clocks {
            all_ports.push(format!("input {c}"));
        }
        for inp in &self.inputs {
            all_ports.push(format!("input {inp}"));
        }
        for outp in &self.outputs {
            all_ports.push(format!("output {outp}"));
        }
        let _ = writeln!(v, "module {} ({});", self.top, all_ports.join(", "));
        if !self.wires.is_empty() {
            for chunk in self.wires.chunks(8) {
                let _ = writeln!(v, "  wire {};", chunk.join(", "));
            }
        }
        for inst in &self.instances {
            let conns: Vec<String> = inst
                .conns
                .iter()
                .map(|(pin, net)| format!(".{pin}({net})"))
                .collect();
            let _ = writeln!(
                v,
                "  {} {} ({});",
                inst.cell_name,
                inst.inst_name,
                conns.join(", ")
            );
        }
        let _ = writeln!(v, "endmodule");
        self.verilog = v;
    }

    /// Total number of gate instances.
    #[must_use]
    pub fn gate_count(&self) -> usize {
        self.instances.len()
    }

    /// Parses `self.verilog` into a frozen [`GateNetlist`] using the `wt_15` catalog.
    #[must_use]
    pub fn to_gate_netlist(&self) -> GateNetlist {
        let cat = CellCatalog::default_wt15();
        read_structural(&self.verilog, &format!("{}.v", self.name), &|c| {
            cat.cell_pins(c)
        })
        .unwrap_or_else(|e| panic!("{}: failed to read structural verilog: {e}", self.name))
    }

    /// Builds a frozen [`GateNetlist`] after shuffling the insertion order of wires and instances
    /// with `seed`. Because `GateNetlist::freeze` sorts internal tables, the resulting multiset of
    /// cells, nets, and instance connections is invariant to insertion order.
    #[must_use]
    pub fn to_gate_netlist_shuffled(&self, seed: u64) -> GateNetlist {
        let mut copy = self.clone();
        let mut rng = SplitMix64::new(seed);
        rng.shuffle(&mut copy.wires);
        rng.shuffle(&mut copy.instances);
        copy.rebuild_verilog();
        copy.to_gate_netlist()
    }
}

/// Generates a combinational inverter/buffer chain of `stages` gates (`stages >= 1`).
#[must_use]
pub fn gen_chain(seed: u64, stages: usize) -> NetlistFixture {
    let stages = stages.max(1);
    let mut rng = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut wires = Vec::new();
    let mut instances = Vec::new();
    for i in 0..stages {
        let in_net = if i == 0 {
            "in0".to_string()
        } else {
            format!("n{i}")
        };
        let out_net = if i + 1 == stages {
            "out0".to_string()
        } else {
            let w = format!("n{}", i + 1);
            wires.push(w.clone());
            w
        };
        let cell = if rng.next_usize(2) == 0 {
            cat.buffers[0].clone()
        } else {
            cat.inverters[0].clone()
        };
        instances.push(InstSpec {
            inst_name: format!("u{i}"),
            cell_name: cell,
            conns: vec![("A".into(), in_net), ("Y".into(), out_net)],
        });
    }
    NetlistFixture::from_parts(
        format!("chain_{stages}"),
        format!("chain_{stages}"),
        vec!["clk".into()],
        vec!["in0".into()],
        vec!["out0".into()],
        wires,
        instances,
    )
}

/// Generates a seeded multi-input combinational and registered DAG with reconvergent paths.
#[must_use]
pub fn gen_dag(seed: u64, num_gates: usize, max_fanin: usize) -> NetlistFixture {
    let num_gates = num_gates.max(4);
    let _ = max_fanin;
    let mut rng = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let inputs: Vec<String> = (0..4).map(|i| format!("in{i}")).collect();
    let outputs: Vec<String> = (0..2).map(|i| format!("out{i}")).collect();
    let mut available_nets = inputs.clone();
    let mut wires = Vec::new();
    let mut instances = Vec::new();

    for i in 0..num_gates {
        let out_net = if i + 2 >= num_gates {
            outputs[num_gates - 1 - i].clone()
        } else {
            let w = format!("n{i}");
            wires.push(w.clone());
            w
        };
        let a = available_nets[rng.next_usize(available_nets.len())].clone();
        let b = available_nets[rng.next_usize(available_nets.len())].clone();
        let gate_idx = rng.next_usize(cat.gates2.len());
        let cell = cat.gates2[gate_idx].clone();
        instances.push(InstSpec {
            inst_name: format!("g{i}"),
            cell_name: cell,
            conns: vec![
                ("A".into(), a),
                ("B".into(), b),
                ("Y".into(), out_net.clone()),
            ],
        });
        available_nets.push(out_net);
    }

    NetlistFixture::from_parts(
        format!("dag_{num_gates}"),
        format!("dag_{num_gates}"),
        vec!["clk".into()],
        inputs,
        outputs,
        wires,
        instances,
    )
}

/// Generates a high-fanout net driving `fanout` load buffers (`fanout >= 1`).
#[must_use]
pub fn gen_fanout_tree(seed: u64, fanout: usize) -> NetlistFixture {
    let fanout = fanout.max(1);
    let mut rng = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut wires = vec!["drv_net".to_string()];
    let mut instances = Vec::with_capacity(fanout + 1);
    instances.push(InstSpec {
        inst_name: "u_drv".into(),
        cell_name: cat.buffers[cat.buffers.len() - 1].clone(),
        conns: vec![("A".into(), "in0".into()), ("Y".into(), "drv_net".into())],
    });
    let mut outputs = Vec::new();
    for i in 0..fanout {
        let cell = if rng.next_usize(2) == 0 {
            cat.buffers[0].clone()
        } else {
            cat.inverters[0].clone()
        };
        let out_net = if i < 4 {
            let o = format!("out{i}");
            outputs.push(o.clone());
            o
        } else {
            let w = format!("sink_{i}");
            wires.push(w.clone());
            w
        };
        instances.push(InstSpec {
            inst_name: format!("u_load_{i}"),
            cell_name: cell,
            conns: vec![("A".into(), "drv_net".into()), ("Y".into(), out_net)],
        });
    }
    NetlistFixture::from_parts(
        format!("fanout_{fanout}"),
        format!("fanout_{fanout}"),
        vec!["clk".into()],
        vec!["in0".into()],
        outputs,
        wires,
        instances,
    )
}

/// Generates a high-fanout net with 5,000 loads on a single net (`drv_net`).
#[must_use]
pub fn gen_high_fanout_5k(seed: u64) -> NetlistFixture {
    gen_fanout_tree(seed, 5000)
}

/// Generates a two-clock domain crossing topology with both posedge and negedge flip-flops.
#[must_use]
pub fn gen_clock_crossing(seed: u64, paths: usize) -> NetlistFixture {
    let paths = paths.max(2);
    let mut rng = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut inputs = vec!["rst_n".to_string()];
    let mut outputs = Vec::new();
    let mut wires = Vec::new();
    let mut instances = Vec::new();

    for i in 0..paths {
        let d_in = format!("in{i}");
        let q_out = format!("out{i}");
        let n_launch = format!("n_l{i}");
        let n_mid = format!("n_m{i}");
        inputs.push(d_in.clone());
        outputs.push(q_out.clone());
        wires.push(n_launch.clone());
        wires.push(n_mid.clone());

        // Alternate posedge and negedge launch/capture across clk_a and clk_b.
        if i % 2 == 0 {
            instances.push(InstSpec {
                inst_name: format!("ff_launch_{i}"),
                cell_name: cat.dffs_pos[0].clone(),
                conns: vec![
                    ("CLK".into(), "clk_a".into()),
                    ("D".into(), d_in),
                    ("Q".into(), n_launch.clone()),
                ],
            });
            let g_cell = cat.gates2[rng.next_usize(cat.gates2.len())].clone();
            instances.push(InstSpec {
                inst_name: format!("g_comb_{i}"),
                cell_name: g_cell,
                conns: vec![
                    ("A".into(), n_launch),
                    ("B".into(), "rst_n".into()),
                    ("Y".into(), n_mid.clone()),
                ],
            });
            instances.push(InstSpec {
                inst_name: format!("ff_cap_{i}"),
                cell_name: cat.dffs_neg[0].clone(),
                conns: vec![
                    ("CLK".into(), "clk_b".into()),
                    ("D".into(), n_mid),
                    ("RST_N".into(), "rst_n".into()),
                    ("Q".into(), q_out),
                ],
            });
        } else {
            instances.push(InstSpec {
                inst_name: format!("ff_launch_{i}"),
                cell_name: cat.dffs_neg[0].clone(),
                conns: vec![
                    ("CLK".into(), "clk_b".into()),
                    ("D".into(), d_in),
                    ("RST_N".into(), "rst_n".into()),
                    ("Q".into(), n_launch.clone()),
                ],
            });
            instances.push(InstSpec {
                inst_name: format!("g_comb_{i}"),
                cell_name: cat.buffers[0].clone(),
                conns: vec![("A".into(), n_launch), ("Y".into(), n_mid.clone())],
            });
            instances.push(InstSpec {
                inst_name: format!("ff_cap_{i}"),
                cell_name: cat.dffs_pos[0].clone(),
                conns: vec![
                    ("CLK".into(), "clk_a".into()),
                    ("D".into(), n_mid),
                    ("Q".into(), q_out),
                ],
            });
        }
    }

    NetlistFixture::from_parts(
        "clock_crossing",
        "clock_crossing",
        vec!["clk_a".into(), "clk_b".into()],
        inputs,
        outputs,
        wires,
        instances,
    )
}

/// Generates an alternating active-high / active-low transparent latch pipeline.
#[must_use]
pub fn gen_latch_pipeline(seed: u64, stages: usize) -> NetlistFixture {
    let stages = stages.max(2);
    let _ = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut wires = Vec::new();
    let mut instances = Vec::new();

    for i in 0..stages {
        let d_net = if i == 0 {
            "in0".to_string()
        } else {
            format!("b{i}")
        };
        let l_out = format!("lq{i}");
        wires.push(l_out.clone());
        let next_net = if i + 1 == stages {
            "out0".to_string()
        } else {
            let w = format!("b{}", i + 1);
            wires.push(w.clone());
            w
        };
        if i % 2 == 0 {
            instances.push(InstSpec {
                inst_name: format!("lat_{i}"),
                cell_name: cat.latches_pos[0].clone(),
                conns: vec![
                    ("G".into(), "clk".into()),
                    ("D".into(), d_net),
                    ("Q".into(), l_out.clone()),
                ],
            });
        } else {
            instances.push(InstSpec {
                inst_name: format!("lat_{i}"),
                cell_name: cat.latches_neg[0].clone(),
                conns: vec![
                    ("GN".into(), "clk".into()),
                    ("D".into(), d_net),
                    ("Q".into(), l_out.clone()),
                ],
            });
        }
        instances.push(InstSpec {
            inst_name: format!("buf_{i}"),
            cell_name: cat.buffers[0].clone(),
            conns: vec![("A".into(), l_out), ("Y".into(), next_net)],
        });
    }

    NetlistFixture::from_parts(
        "latch_pipeline",
        "latch_pipeline",
        vec!["clk".into()],
        vec!["in0".into()],
        vec!["out0".into()],
        wires,
        instances,
    )
}

/// Generates an integrated clock-gating (ICG) topology gating a register bank.
#[must_use]
pub fn gen_clock_gated(seed: u64, width: usize) -> NetlistFixture {
    let width = width.max(2);
    let _ = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut inputs = vec!["en".to_string()];
    let mut outputs = Vec::new();
    let mut wires = vec!["gclk".to_string()];
    let mut instances = vec![InstSpec {
        inst_name: "u_icg".into(),
        cell_name: cat.icgs[0].clone(),
        conns: vec![
            ("CLK".into(), "clk".into()),
            ("EN".into(), "en".into()),
            ("GCLK".into(), "gclk".into()),
        ],
    }];

    for i in 0..width {
        let inp = format!("in{i}");
        let outp = format!("out{i}");
        let mid = format!("d_mid_{i}");
        inputs.push(inp.clone());
        outputs.push(outp.clone());
        wires.push(mid.clone());
        instances.push(InstSpec {
            inst_name: format!("u_buf_{i}"),
            cell_name: cat.buffers[0].clone(),
            conns: vec![("A".into(), inp), ("Y".into(), mid.clone())],
        });
        instances.push(InstSpec {
            inst_name: format!("u_ff_{i}"),
            cell_name: cat.dffs_pos[0].clone(),
            conns: vec![
                ("CLK".into(), "gclk".into()),
                ("D".into(), mid),
                ("Q".into(), outp),
            ],
        });
    }

    NetlistFixture::from_parts(
        "clock_gated",
        "clock_gated",
        vec!["clk".into()],
        inputs,
        outputs,
        wires,
        instances,
    )
}

/// Generates a shared tri-state bus with `drivers >= 2` tri-state buffers and `loads` receivers.
#[must_use]
pub fn gen_tristate_bus(seed: u64, drivers: usize, loads: usize) -> NetlistFixture {
    let drivers = drivers.max(2);
    let loads = loads.max(1);
    let _ = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let wires = vec!["bus_net".to_string()];
    let mut instances = Vec::new();

    for i in 0..drivers {
        let d_in = format!("din{i}");
        let oe_in = format!("oe{i}");
        inputs.push(d_in.clone());
        inputs.push(oe_in.clone());
        instances.push(InstSpec {
            inst_name: format!("u_tbuf_{i}"),
            cell_name: cat.tristates[0].clone(),
            conns: vec![
                ("A".into(), d_in),
                ("OE".into(), oe_in),
                ("Y".into(), "bus_net".into()),
            ],
        });
    }
    for j in 0..loads {
        let outp = format!("dout{j}");
        outputs.push(outp.clone());
        instances.push(InstSpec {
            inst_name: format!("u_rcv_{j}"),
            cell_name: cat.buffers[0].clone(),
            conns: vec![("A".into(), "bus_net".into()), ("Y".into(), outp)],
        });
    }

    NetlistFixture::from_parts(
        "tristate_bus",
        "tristate_bus",
        vec!["clk".into()],
        inputs,
        outputs,
        wires,
        instances,
    )
}

/// Generates a topology with `wt_tiehi` / `wt_tielo` tie cells and `1'b0` / `1'b1` literal
/// constants feeding combinational gating logic (`wt_and2_1`, `wt_or2_1`, `wt_mux2_1`).
#[must_use]
pub fn gen_constant_tied(seed: u64, width: usize) -> NetlistFixture {
    let width = width.max(2);
    let _ = SplitMix64::new(seed);
    let cat = CellCatalog::default_wt15();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let wires = vec!["tie_one".to_string(), "tie_zero".to_string()];
    let mut instances = vec![
        InstSpec {
            inst_name: "u_tiehi".into(),
            cell_name: cat.tie_hi[0].clone(),
            conns: vec![("Y".into(), "tie_one".into())],
        },
        InstSpec {
            inst_name: "u_tielo".into(),
            cell_name: cat.tie_lo[0].clone(),
            conns: vec![("Y".into(), "tie_zero".into())],
        },
    ];

    for i in 0..width {
        let inp = format!("in{i}");
        let outp = format!("out{i}");
        inputs.push(inp.clone());
        outputs.push(outp.clone());
        if i % 2 == 0 {
            instances.push(InstSpec {
                inst_name: format!("u_mux_{i}"),
                cell_name: cat.muxes[0].clone(),
                conns: vec![
                    ("A".into(), inp),
                    ("B".into(), "tie_zero".into()),
                    ("S".into(), "1'b0".into()),
                    ("Y".into(), outp),
                ],
            });
        } else {
            instances.push(InstSpec {
                inst_name: format!("u_and_{i}"),
                cell_name: "wt_and2_1".into(),
                conns: vec![
                    ("A".into(), inp),
                    ("B".into(), "tie_one".into()),
                    ("Y".into(), outp),
                ],
            });
        }
    }

    NetlistFixture::from_parts(
        "constant_tied",
        "constant_tied",
        vec!["clk".into()],
        inputs,
        outputs,
        wires,
        instances,
    )
}

/// Directly constructs a frozen [`GateNetlist`] with `target_gates` instances (used for 500k and
/// 1M gate scale verification without intermediate Verilog serialization overhead).
#[must_use]
pub fn gen_scale_netlist(seed: u64, target_gates: usize) -> GateNetlist {
    let target_gates = target_gates.max(1);
    let mut rng = SplitMix64::new(seed);
    let mut nl = GateNetlist::new(&format!("scale_{target_gates}"))
        .expect("scale module name must be valid");
    let buf_cell = nl
        .add_cell_type("wt_buf_1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .expect("wt_buf_1 cell must be valid");
    let inv_cell = nl
        .add_cell_type("wt_inv_1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .expect("wt_inv_1 cell must be valid");
    nl.add_port("in0", PortDir::Input, None)
        .expect("in0 port must be valid");
    nl.add_port("out0", PortDir::Output, None)
        .expect("out0 port must be valid");
    let in_net = nl.find_net("in0", None).expect("in0 net must exist");
    let out_net = nl.find_net("out0", None).expect("out0 net must exist");

    let mut prev = in_net;
    for i in 0..target_gates {
        let next = if i + 1 == target_gates {
            out_net
        } else {
            nl.add_net(Some(&format!("n{i}")), None)
                .expect("wire net must be valid")
        };
        let cell = if rng.next_u64() & 1 == 0 {
            buf_cell
        } else {
            inv_cell
        };
        let inst = nl
            .add_instance(&format!("u{i}"), cell)
            .expect("instance must be valid");
        nl.connect(inst, 0u32, Conn::Net(prev))
            .expect("input pin connection must be valid");
        nl.connect(inst, 1u32, Conn::Net(next))
            .expect("output pin connection must be valid");
        prev = next;
    }
    nl
}

/// Constructs a 500,000-gate scale [`GateNetlist`].
#[must_use]
pub fn gen_scale_500k(seed: u64) -> GateNetlist {
    gen_scale_netlist(seed, 500_000)
}

/// Constructs a 1,000,000-gate scale [`GateNetlist`].
#[must_use]
pub fn gen_scale_1m(seed: u64) -> GateNetlist {
    gen_scale_netlist(seed, 1_000_000)
}

/// Returns the canonical tracked synthetic netlists written to
/// `verification/fixtures/synthetic/sta/net/*.v`.
#[must_use]
pub fn synthetic_netlists() -> Vec<NetlistFixture> {
    vec![
        gen_chain(0x1001, 8),
        gen_dag(0x1002, 32, 2),
        gen_fanout_tree(0x1003, 64),
        gen_clock_crossing(0x1004, 4),
        gen_latch_pipeline(0x1005, 4),
        gen_clock_gated(0x1006, 4),
        gen_tristate_bus(0x1007, 3, 2),
        gen_constant_tied(0x1008, 4),
    ]
}
