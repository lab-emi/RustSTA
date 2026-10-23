// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-engine-legacy-equiv` (`STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0211`,
//! `STA-REQ-0212`, `STA-REQ-0214`, `STA-REQ-0216`):
//! equivalence of [`EngineAnalysis`] endpoint setup and hold slacks, WNS, TNS, and violating
//! counts against the legacy engine (`ruststa::analyze`) on single-clock and same-period circuits.

use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};

const EPS: f64 = 1e-9;

#[derive(Clone, Copy)]
struct Lin {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    mc: f64,
}

impl Lin {
    fn delay(self, slew: f64, load: f64) -> f64 {
        self.d0 + self.ks * slew + self.kc * load
    }

    fn slew(self, load: f64) -> f64 {
        self.s0 + self.mc * load
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
const RECOVERY: f64 = 0.12;
const REMOVAL: f64 = 0.06;

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

fn write_check_arc(out: &mut String, related: &str, ty: &str, v: f64) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_type : {ty};");
    let _ = writeln!(
        out,
        "        rise_constraint (scalar) {{ values (\"{v}\"); }}"
    );
    let _ = writeln!(
        out,
        "        fall_constraint (scalar) {{ values (\"{v}\"); }}"
    );
    let _ = writeln!(out, "      }}");
}

fn linear_library() -> Library {
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
    for (name, ck, edge, group) in [
        (
            "DFF",
            "CK",
            "rising",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }",
        ),
        (
            "DFFN",
            "CKN",
            "falling",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"!CKN\"; }",
        ),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 4;\n    {group}");
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(&mut o, ck, &format!("setup_{edge}"), SETUP);
        write_check_arc(&mut o, ck, &format!("hold_{edge}"), HOLD);
        let _ = writeln!(o, "    }}");
        let _ = writeln!(
            o,
            "    pin ({ck}) {{ direction : input; capacitance : {CIN}; clock : true; }}"
        );
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        write_arc(
            &mut o,
            ck,
            "non_unate",
            &format!("{edge}_edge"),
            CKQ_R,
            CKQ_F,
        );
        let _ = writeln!(o, "    }}\n  }}");
    }
    let _ = writeln!(
        o,
        "  cell (DFFR) {{\n    area : 5;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }}"
    );
    let _ = writeln!(
        o,
        "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    write_check_arc(&mut o, "CK", "setup_rising", SETUP);
    write_check_arc(&mut o, "CK", "hold_rising", HOLD);
    let _ = writeln!(o, "    }}");
    let _ = writeln!(
        o,
        "    pin (RN) {{\n      direction : input;\n      capacitance : {CIN};\n      timing () {{\n        related_pin : \"CK\";\n        timing_type : recovery_rising;\n        rise_constraint (scalar) {{ values (\"{RECOVERY}\"); }}\n      }}\n      timing () {{\n        related_pin : \"CK\";\n        timing_type : removal_rising;\n        rise_constraint (scalar) {{ values (\"{REMOVAL}\"); }}\n      }}\n    }}"
    );
    let _ = writeln!(
        o,
        "    pin (CK) {{ direction : input; capacitance : {CIN}; clock : true; }}"
    );
    let _ = writeln!(
        o,
        "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    write_arc(&mut o, "CK", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}");
    o.push_str("}\n");
    read_text(&o, "lin.lib").expect("parse linear library")
}

fn build_netlist(verilog_src: &str, lib: &Library) -> FrozenNetlist<'static> {
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
    read_structural(verilog_src, "<test.v>", &cell_pins)
        .expect("structural verilog")
        .freeze()
}

fn assert_opt_close(got: Option<f64>, want: Option<f64>, label: &str) {
    match (got, want) {
        (None, None) => {}
        (Some(g), Some(w)) => {
            assert!(
                (g - w).abs() < EPS,
                "{label}: engine got {g:.12}, legacy got {w:.12} (diff {:.3e})",
                (g - w).abs()
            );
        }
        _ => panic!("{label}: engine got {got:?}, legacy got {want:?}"),
    }
}

fn compare_with_legacy(case: &str, verilog: &str, sdc: &str, lib: &Library) {
    let opts = ruststa::StaOptions {
        netlist: ruststa::Source::Texts(vec![("n.v".into(), verilog.into())]),
        top: None,
        sdc: sdc.into(),
        paths: 5,
        path_to: vec![],
        backend: ruststa::Backend::Cpu,
        sdc_name: String::new(),
    };
    let legacy = ruststa::analyze(&opts, std::slice::from_ref(lib))
        .unwrap_or_else(|e| panic!("{case}: legacy analyze failed: {e:?}"));

    let netlist = build_netlist(verilog, lib);
    let libs = [lib.clone()];
    let engine =
        EngineAnalysis::analyze_single_corner(&netlist, &libs, sdc, &AnalysisOptions::ideal())
            .unwrap_or_else(|e| panic!("{case}: EngineAnalysis failed: {e:?}"));

    for ep in &legacy.endpoints {
        assert_opt_close(
            engine.setup_slack_for(&ep.pin),
            ep.setup_slack,
            &format!("{case} {} setup", ep.pin),
        );
        assert_opt_close(
            engine.hold_slack_for(&ep.pin),
            ep.hold_slack,
            &format!("{case} {} hold", ep.pin),
        );
    }

    let sum = &engine.sta_summary;
    assert_opt_close(
        sum.wns_setup_ns.map(|w| w.min(0.0)),
        Some(legacy.setup.wns),
        &format!("{case} wns_setup"),
    );
    assert_opt_close(
        Some(sum.tns_setup_ns),
        Some(legacy.setup.tns),
        &format!("{case} tns_setup"),
    );
    assert_opt_close(
        sum.wns_hold_ns.map(|w| w.min(0.0)),
        Some(legacy.hold.wns),
        &format!("{case} wns_hold"),
    );
    assert_opt_close(
        Some(sum.tns_hold_ns),
        Some(legacy.hold.tns),
        &format!("{case} tns_hold"),
    );
    assert_eq!(
        sum.violating_setup as usize, legacy.setup.violating,
        "{case} violating_setup"
    );
    assert_eq!(
        sum.violating_hold as usize, legacy.hold.violating,
        "{case} violating_hold"
    );
}

// STA-REQ-0200, STA-REQ-0201, STA-REQ-0211, STA-REQ-0212, STA-REQ-0214, STA-REQ-0216:
// parity between EngineAnalysis and legacy analyze on linear-library single-clock and same-period circuits.
#[test]
fn legacy_equivalence_linear_circuits() {
    let lib = linear_library();

    compare_with_legacy(
        "inverter_chain",
        "module m (input clk, input d, output q);\n wire n1, n2, n3;\n\
         DFF r1 (.D(d), .CK(clk), .Q(n1));\n INV u1 (.A(n1), .Y(n2));\n\
         INV u2 (.A(n2), .Y(n3));\n DFF r2 (.D(n3), .CK(clk), .Q(q));\nendmodule\n",
        "create_clock -name clk -period 2 [get_ports clk]\n\
         set_input_delay -clock clk 0.5 [get_ports d]\n\
         set_output_delay -clock clk 0.3 [get_ports q]\nset_load 0.02 [get_ports q]\n",
        &lib,
    );

    compare_with_legacy(
        "reconvergent_xor",
        "module m (input clk, input a, output q);\n wire n1, n2, n3;\n\
         DFF r1 (.D(a), .CK(clk), .Q(n1));\n BUF b1 (.A(n1), .Y(n2));\n\
         XOR2 x (.A(n1), .B(n2), .Y(n3));\n DFF r2 (.D(n3), .CK(clk), .Q(q));\nendmodule\n",
        "create_clock -name clk -period 2 [get_ports clk]\n",
        &lib,
    );

    compare_with_legacy(
        "falling_edge_capture",
        "module m (input clk, input a, output q);\n wire n1, n2;\n\
         DFF r1 (.D(a), .CK(clk), .Q(n1));\n INV u (.A(n1), .Y(n2));\n\
         DFFN r2 (.D(n2), .CKN(clk), .Q(q));\nendmodule\n",
        "create_clock -name clk -period 2 -waveform {0 0.6} [get_ports clk]\n",
        &lib,
    );

    compare_with_legacy(
        "clock_gating_and2",
        "module m (input clk, input a, input d, output q);\n wire e, e2, g;\n\
         DFF r1 (.D(a), .CK(clk), .Q(e));\n BUF b (.A(e), .Y(e2));\n\
         AND2 c (.A(clk), .B(e2), .Y(g));\n DFF r2 (.D(d), .CK(g), .Q(q));\nendmodule\n",
        "create_clock -name clk -period 2 [get_ports clk]\n",
        &lib,
    );

    compare_with_legacy(
        "recovery_removal",
        "module m (input clk, input rst, input d, output q);\n wire rq, r1, r2, rn;\n\
         DFF r0 (.D(rst), .CK(clk), .Q(rq));\n BUF b1 (.A(rq), .Y(r1));\n\
         BUF b2 (.A(r1), .Y(r2));\n BUF b3 (.A(r2), .Y(rn));\n\
         DFFR f1 (.D(d), .CK(clk), .RN(rn), .Q(q));\nendmodule\n",
        "create_clock -name clk -period 1 [get_ports clk]\n\
         set_input_delay -clock clk 0.2 [get_ports {rst d}]\n\
         set_output_delay -clock clk 0.3 [get_ports q]\n",
        &lib,
    );

    compare_with_legacy(
        "same_period_two_clocks",
        "module m (input ca, input cb, input a, output q);\n wire n1, n2;\n\
         DFF r1 (.D(a), .CK(ca), .Q(n1));\n BUF b (.A(n1), .Y(n2));\n\
         DFF r2 (.D(n2), .CK(cb), .Q(q));\nendmodule\n",
        "create_clock -name A -period 2 [get_ports ca]\n\
         create_clock -name B -period 2 [get_ports cb]\n",
        &lib,
    );
}

// STA-REQ-0200, STA-REQ-0211, STA-REQ-0212, STA-REQ-0214:
// parity on synthetic STA fixtures (chain_8, fanout_64) when driver output-pin capacitance
// (STA-REQ-0121, not modelled by the Phase 1 legacy engine) is zeroed.
#[test]
fn legacy_equivalence_synthetic_fixtures() {
    let sta_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/fixtures/synthetic/sta");
    let lib_text = std::fs::read_to_string(sta_dir.join("lib/wt_15.lib")).expect("wt_15.lib");
    let mut lib = read_text(&lib_text, "wt_15.lib").expect("parse wt_15.lib");
    for cell in &mut lib.cells {
        for pin in &mut cell.pins {
            if pin.direction == Direction::Output {
                pin.capacitance = 0.0;
                pin.rise_capacitance = None;
                pin.fall_capacitance = None;
            }
            for arc in &mut pin.arcs {
                for tbl in [&mut arc.rise_transition, &mut arc.fall_transition]
                    .into_iter()
                    .flatten()
                {
                    for v in &mut tbl.values {
                        *v += 0.06;
                    }
                }
            }
        }
    }

    for name in ["chain_8", "fanout_64"] {
        let verilog =
            std::fs::read_to_string(sta_dir.join(format!("net/{name}.v"))).expect("netlist");
        let sdc = std::fs::read_to_string(sta_dir.join(format!("sdc/{name}.sdc"))).expect("sdc");
        compare_with_legacy(name, &verilog, &sdc, &lib);
    }
}
