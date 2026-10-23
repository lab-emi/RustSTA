// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-checks` (`STA-REQ-0015`, `STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0211`,
//! `STA-REQ-0212`, `STA-REQ-0214`, `STA-REQ-0216` to `STA-REQ-0226`):
//! ideal clock networks, launch tags, input/output delay latency flags, `set_driving_cell`,
//! one-sided I/O delays, LCM setup/hold relations, latch transparency and `D -> Q` re-launch,
//! three-state bus merging, recovery/removal checks (`async_default`), active-high and active-low
//! clock-gating checks, endpoint status taxonomy, and multi-corner orchestration.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::diag::Severity;
use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::input::constr::CornerLibs;
use ruststa::engine::prop::checks::EndpointCheckKind;
use ruststa::engine::prop::endpoints::EndpointStatus;
use ruststa::engine::types::{An, ClockId, NodeId, Tr};

const EPS: f64 = 1e-9;

#[derive(Clone, Copy)]
struct Lin {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    ms: f64,
    mc: f64,
}

impl Lin {
    const fn new(d0: f64, ks: f64, kc: f64, s0: f64, ms: f64, mc: f64) -> Self {
        Self {
            d0,
            ks,
            kc,
            s0,
            ms,
            mc,
        }
    }

    fn scale(self, k: f64) -> Self {
        Self {
            d0: self.d0 * k,
            ks: self.ks * k,
            kc: self.kc * k,
            s0: self.s0 * k,
            ms: self.ms * k,
            mc: self.mc * k,
        }
    }

    fn delay(self, slew: f64, load: f64) -> f64 {
        self.d0 + self.ks * slew + self.kc * load
    }

    fn raw_slew(self, slew: f64, load: f64) -> f64 {
        self.s0 + self.ms * slew + self.mc * load
    }
}

const INV_R: Lin = Lin::new(0.10, 0.20, 2.0, 0.05, 0.0, 1.5);
const INV_F: Lin = Lin::new(0.08, 0.15, 1.5, 0.04, 0.0, 1.2);
const BUF_R: Lin = Lin::new(0.20, 0.10, 1.0, 0.06, 0.0, 1.0);
const BUF_F: Lin = Lin::new(0.18, 0.10, 1.2, 0.05, 0.0, 1.1);
const XOR_R: Lin = Lin::new(0.30, 0.25, 2.5, 0.07, 0.0, 1.8);
const XOR_F: Lin = Lin::new(0.28, 0.20, 2.2, 0.06, 0.0, 1.6);
const CKQ_R: Lin = Lin::new(0.40, 0.0, 1.0, 0.08, 0.0, 1.0);
const CKQ_F: Lin = Lin::new(0.35, 0.0, 0.9, 0.07, 0.0, 1.0);
const DQ_R: Lin = Lin::new(0.25, 0.10, 1.0, 0.06, 0.0, 1.0);
const DQ_F: Lin = Lin::new(0.22, 0.10, 1.0, 0.05, 0.0, 1.0);
const DQS_R: Lin = Lin::new(0.25, 0.10, 1.0, 0.12, 0.0, 1.0);
const DQS_F: Lin = Lin::new(0.22, 0.10, 1.0, 0.11, 0.0, 1.0);
const TSE_R: Lin = Lin::new(0.15, 0.10, 1.0, 0.06, 0.0, 1.0);
const TSE_F: Lin = Lin::new(0.13, 0.10, 1.0, 0.05, 0.0, 1.0);
const TSD_R: Lin = Lin::new(0.09, 0.05, 0.0, 0.0, 0.0, 0.0);
const TSD_F: Lin = Lin::new(0.07, 0.05, 0.0, 0.0, 0.0, 0.0);

const CIN: f64 = 0.01;
const COUT_O: f64 = 0.004;
const COUT_Z: f64 = 0.005;
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
    write_table(out, "rise_transition", |s, c| r.raw_slew(s, c));
    write_table(out, "cell_fall", |s, c| f.delay(s, c));
    write_table(out, "fall_transition", |s, c| f.raw_slew(s, c));
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

/// Builds the Phase 2a synthetic Liberty library scaled by `k` (`k = 1.0` for `fast`/`base`,
/// `k = 2.0` for `slow`).
fn analytic_liberty(name: &str, k: f64) -> String {
    let inv_r = INV_R.scale(k);
    let inv_f = INV_F.scale(k);
    let buf_r = BUF_R.scale(k);
    let buf_f = BUF_F.scale(k);
    let xor_r = XOR_R.scale(k);
    let xor_f = XOR_F.scale(k);
    let ckq_r = CKQ_R.scale(k);
    let ckq_f = CKQ_F.scale(k);
    let dq_r = DQ_R.scale(k);
    let dq_f = DQ_F.scale(k);
    let dqs_r = DQS_R.scale(k);
    let dqs_f = DQS_F.scale(k);
    let tse_r = TSE_R.scale(k);
    let tse_f = TSE_F.scale(k);
    let tsd_r = TSD_R.scale(k);
    let tsd_f = TSD_F.scale(k);
    let setup = SETUP * k;
    let hold = HOLD * k;
    let rec = RECOVERY * k;
    let rem = REMOVAL * k;

    let mut o = format!(
        "library ({name}) {{\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {{\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }}\n"
    );

    for (cell, ins, func, sense, r, f, cout) in [
        ("INV", &["A"][..], "!A", "negative_unate", inv_r, inv_f, 0.0),
        ("BUF", &["A"][..], "A", "positive_unate", buf_r, buf_f, 0.0),
        (
            "BUFO",
            &["A"][..],
            "A",
            "positive_unate",
            buf_r,
            buf_f,
            COUT_O,
        ),
        (
            "AND2",
            &["A", "B"][..],
            "A&B",
            "positive_unate",
            buf_r,
            buf_f,
            0.0,
        ),
        (
            "OR2",
            &["A", "B"][..],
            "A|B",
            "positive_unate",
            buf_r,
            buf_f,
            0.0,
        ),
        (
            "XOR2",
            &["A", "B"][..],
            "A^B",
            "non_unate",
            xor_r,
            xor_f,
            0.0,
        ),
    ] {
        let _ = writeln!(o, "  cell ({cell}) {{\n    area : 1;");
        for i in ins {
            let _ = writeln!(
                o,
                "    pin ({i}) {{ direction : input; capacitance : {CIN}; }}"
            );
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n      capacitance : {cout};\n      function : \"{func}\";"
        );
        for i in ins {
            write_arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }

    for (cell, func) in [("TIELO", "0"), ("TIEHI", "1")] {
        let _ = writeln!(
            o,
            "  cell ({cell}) {{ area : 1; pin (Y) {{ direction : output; capacitance : 0.0; function : \"{func}\"; }} }}"
        );
    }

    let _ = writeln!(
        o,
        "  cell (BUFZ) {{\n    area : 2;\n    pin (A) {{ direction : input; capacitance : {CIN}; }}\n    pin (EN) {{ direction : input; capacitance : {CIN}; }}\n    pin (Z) {{\n      direction : output;\n      capacitance : {COUT_Z};\n      function : \"A\";\n      three_state : \"!EN\";"
    );
    write_arc(&mut o, "A", "positive_unate", "combinational", buf_r, buf_f);
    write_arc(
        &mut o,
        "EN",
        "positive_unate",
        "three_state_enable",
        tse_r,
        tse_f,
    );
    write_arc(
        &mut o,
        "EN",
        "negative_unate",
        "three_state_disable",
        tsd_r,
        tsd_f,
    );
    let _ = writeln!(o, "    }}\n  }}");

    for (cell, ck, edge, group) in [
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
        let _ = writeln!(
            o,
            "  cell ({cell}) {{\n    area : 4;\n    {group}\n    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(&mut o, ck, &format!("setup_{edge}"), setup);
        write_check_arc(&mut o, ck, &format!("hold_{edge}"), hold);
        let _ = writeln!(
            o,
            "    }}\n    pin ({ck}) {{ direction : input; capacitance : {CIN}; clock : true; }}\n    pin (Q) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"IQ\";"
        );
        write_arc(
            &mut o,
            ck,
            "non_unate",
            &format!("{edge}_edge"),
            ckq_r,
            ckq_f,
        );
        let _ = writeln!(o, "    }}\n  }}");
    }

    // Asynchronous reset flip-flop DFFR (recovery_rising / removal_rising on RN with rise_constraint only).
    let _ = writeln!(
        o,
        "  cell (DFFR) {{\n    area : 5;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }}"
    );
    let _ = writeln!(
        o,
        "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    write_check_arc(&mut o, "CK", "setup_rising", setup);
    write_check_arc(&mut o, "CK", "hold_rising", hold);
    let _ = writeln!(o, "    }}");
    let _ = writeln!(
        o,
        "    pin (RN) {{\n      direction : input;\n      capacitance : {CIN};\n      timing () {{\n        related_pin : \"CK\";\n        timing_type : recovery_rising;\n        rise_constraint (scalar) {{ values (\"{rec}\"); }}\n      }}\n      timing () {{\n        related_pin : \"CK\";\n        timing_type : removal_rising;\n        rise_constraint (scalar) {{ values (\"{rem}\"); }}\n      }}\n    }}"
    );
    let _ = writeln!(
        o,
        "    pin (CK) {{ direction : input; capacitance : {CIN}; clock : true; }}"
    );
    let _ = writeln!(
        o,
        "    pin (Q) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"IQ\";"
    );
    write_arc(&mut o, "CK", "non_unate", "rising_edge", ckq_r, ckq_f);
    let _ = writeln!(o, "    }}\n  }}");

    // Latches LAT (DQ_R/DQ_F) and LATS (DQS_R/DQS_F).
    for (cell, d_r, d_f) in [("LAT", dq_r, dq_f), ("LATS", dqs_r, dqs_f)] {
        let _ = writeln!(
            o,
            "  cell ({cell}) {{\n    area : 3;\n    latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }}"
        );
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(&mut o, "G", "setup_falling", setup);
        write_check_arc(&mut o, "G", "hold_falling", hold);
        let _ = writeln!(o, "    }}");
        let _ = writeln!(
            o,
            "    pin (G) {{ direction : input; capacitance : {CIN}; clock : true; }}"
        );
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"IQ\";"
        );
        write_arc(&mut o, "D", "positive_unate", "combinational", d_r, d_f);
        write_arc(&mut o, "G", "non_unate", "rising_edge", ckq_r, ckq_f);
        let _ = writeln!(o, "    }}\n  }}");
    }

    o.push_str("}\n");
    o
}

fn parse_lib(text: &str) -> Library {
    read_text(text, "lin.lib").expect("parse synthetic Liberty")
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

struct TestRun {
    netlist: Box<FrozenNetlist<'static>>,
    libs: Vec<Library>,
}

impl TestRun {
    fn new(verilog: &str) -> Self {
        let lib = parse_lib(&analytic_liberty("lin", 1.0));
        let netlist = Box::new(build_netlist(verilog, &lib));
        Self {
            netlist,
            libs: vec![lib],
        }
    }

    fn analyze<'a>(&'a self, sdc_text: &str) -> EngineAnalysis<'a> {
        EngineAnalysis::analyze_single_corner(
            &self.netlist,
            &self.libs,
            sdc_text,
            &AnalysisOptions::default(),
        )
        .expect("analyze_single_corner")
    }
}

#[derive(serde::Deserialize)]
struct ExpectedMap(BTreeMap<String, BTreeMap<String, serde_json::Value>>);

fn load_expected(case: &str) -> BTreeMap<String, BTreeMap<String, serde_json::Value>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta")
        .join(case)
        .join("expected.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let map: ExpectedMap =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    map.0
}

fn expect_f64(section: &BTreeMap<String, serde_json::Value>, key: &str) -> f64 {
    section
        .get(key)
        .unwrap_or_else(|| panic!("missing key '{key}'"))
        .as_f64()
        .unwrap_or_else(|| panic!("key '{key}' is not f64"))
}

fn assert_close(got: f64, want: f64, label: &str) {
    assert!(
        (got - want).abs() < EPS,
        "{label}: got {got:.12}, want {want:.12} (diff {:.3e})",
        (got - want).abs()
    );
}

fn assert_opt_close(got: Option<f64>, want: f64, label: &str) {
    let g = got.unwrap_or_else(|| panic!("{label}: expected Some({want}), got None"));
    assert_close(g, want, label);
}

fn check_ep(analysis: &EngineAnalysis<'_>, sec: &BTreeMap<String, serde_json::Value>, ep: &str) {
    assert_opt_close(
        analysis.setup_slack_for(ep),
        expect_f64(sec, &format!("{ep} setup")),
        &format!("{ep} setup"),
    );
    assert_opt_close(
        analysis.hold_slack_for(ep),
        expect_f64(sec, &format!("{ep} hold")),
        &format!("{ep} hold"),
    );
}

fn node_id(analysis: &EngineAnalysis<'_>, name: &str) -> NodeId {
    analysis
        .find_node(name)
        .unwrap_or_else(|| panic!("missing node '{name}'"))
}

// STA-REQ-0217, STA-REQ-0218, STA-REQ-0211, STA-REQ-0212: input/output delay clock latencies,
// -source_latency_included, -network_latency_included, and set_driving_cell.
#[test]
fn io_delay_latency_base_and_rf() {
    let exp = load_expected("io_delay_latency");
    let verilog = "\
module m (input clk, input d, input e, input f, output q, output p);
 wire n1, n2, n3, q3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(q));
 BUF b2 (.A(e), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(p));
 BUF b3 (.A(f), .Y(n3));
 DFF r3 (.D(n3), .CK(clk), .Q(q3));
endmodule
";
    let sdc_base = "\
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency -source 0.2 [get_clocks clk]
set_clock_latency -max 0.3 [get_clocks clk]
set_clock_latency -min 0.1 [get_clocks clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_input_delay -clock clk 0.5 -network_latency_included [get_ports e]
set_input_delay -clock clk 0.5 [get_ports f]
set_driving_cell -lib_cell BUF -pin Y [get_ports f]
set_output_delay -clock clk 0.4 [get_ports q]
set_output_delay -clock clk 0.4 -source_latency_included [get_ports p]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc_base);
    let base = &exp["base"];

    let corner = analysis.default_corner();
    let d_port = node_id(&analysis, "d");
    let f_port = node_id(&analysis, "f");
    let tag_clk_r = corner
        .tags
        .base_tag(ClockId(0), Tr::Rise)
        .expect("clk rise tag");

    assert_close(
        corner.arrival(d_port, tag_clk_r, Tr::Rise, An::Late),
        expect_f64(base, "d start late"),
        "d start late",
    );
    assert_close(
        corner.arrival(d_port, tag_clk_r, Tr::Fall, An::Early),
        expect_f64(base, "d start early"),
        "d start early",
    );
    assert_close(
        corner.slew(f_port, Tr::Rise, An::Late),
        expect_f64(base, "f slew rise"),
        "f slew rise",
    );
    assert_close(
        corner.slew(f_port, Tr::Fall, An::Late),
        expect_f64(base, "f slew fall"),
        "f slew fall",
    );
    assert_close(
        corner.arrival(f_port, tag_clk_r, Tr::Rise, An::Late) - expect_f64(base, "d start late"),
        expect_f64(base, "f drive delay rise"),
        "f drive delay rise",
    );
    assert_close(
        corner.arrival(f_port, tag_clk_r, Tr::Fall, An::Early) - expect_f64(base, "d start early"),
        expect_f64(base, "f drive delay fall"),
        "f drive delay fall",
    );

    for ep in ["r1/D", "r2/D", "r3/D", "q", "p"] {
        check_ep(&analysis, base, ep);
    }

    // Variant rf: per-edge (-rise / -fall) clock latencies with -clock_fall I/O delays.
    let sdc_rf = "\
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency -rise 0.3 [get_clocks clk]
set_clock_latency -fall 0.2 [get_clocks clk]
set_input_delay -clock clk -clock_fall 0.5 [get_ports d]
set_input_delay -clock clk 0.5 [get_ports e]
set_input_delay -clock clk 0.5 [get_ports f]
set_output_delay -clock clk -clock_fall 0.4 [get_ports q]
set_output_delay -clock clk 0.4 [get_ports p]
";
    let analysis_rf = run.analyze(sdc_rf);
    let rf = &exp["rf"];
    for ep in ["r1/D", "r2/D", "q", "p"] {
        check_ep(&analysis_rf, rf, ep);
    }
}

// STA-REQ-0219, STA-REQ-0212: one-sided -max and -min input and output delays.
#[test]
fn one_sided_delays() {
    let exp = load_expected("one_sided_delays");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input d, input e, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(n2));
 BUF b2 (.A(n2), .Y(q));
 BUF b3 (.A(e), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(p));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk -max 0.5 [get_ports d]
set_input_delay -clock clk -min 0.1 [get_ports e]
set_output_delay -clock clk -min -0.1 [get_ports q]
set_output_delay -clock clk -max 0.4 [get_ports p]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);

    assert_opt_close(
        analysis.setup_slack_for("r1/D"),
        expect_f64(base, "r1/D setup"),
        "r1/D setup",
    );
    assert_eq!(analysis.hold_slack_for("r1/D"), None, "r1/D has no hold");

    assert_eq!(analysis.setup_slack_for("r2/D"), None, "r2/D has no setup");
    assert_opt_close(
        analysis.hold_slack_for("r2/D"),
        expect_f64(base, "r2/D hold"),
        "r2/D hold",
    );

    assert_eq!(analysis.setup_slack_for("q"), None, "q has no setup");
    assert_opt_close(
        analysis.hold_slack_for("q"),
        expect_f64(base, "q hold"),
        "q hold",
    );

    assert_opt_close(
        analysis.setup_slack_for("p"),
        expect_f64(base, "p setup"),
        "p setup",
    );
    assert_eq!(analysis.hold_slack_for("p"), None, "p has no hold");

    // STA-REQ-0219: each one-sided I/O constraint is recorded with a diagnostic note.
    let notes: Vec<_> = analysis
        .diags
        .iter()
        .filter(|d| d.severity == Severity::Note && d.code == "T0031")
        .collect();
    assert_eq!(
        notes.len(),
        4,
        "expected 4 one-sided I/O diagnostic notes (d, e, q, p), got: {notes:?}"
    );
}

// STA-REQ-0220, STA-REQ-0211: setup (D_s) and hold (D_h) relations over the common period.
#[test]
fn two_clocks_hold_relations_and_ratio_limit() {
    let exp = load_expected("two_clocks_hold");
    let verilog = "\
module m (input ca, input cb, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(ca), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(cb), .Q(q));
endmodule
";
    let run = TestRun::new(verilog);

    for (variant, sdc) in [
        (
            "p32",
            "create_clock -name A -period 3 [get_ports ca]\ncreate_clock -name B -period 2 [get_ports cb]\n",
        ),
        (
            "p23",
            "create_clock -name A -period 2 [get_ports ca]\ncreate_clock -name B -period 3 [get_ports cb]\n",
        ),
        (
            "off",
            "create_clock -name A -period 3 -waveform {0.5 2} [get_ports ca]\ncreate_clock -name B -period 2 [get_ports cb]\n",
        ),
    ] {
        let analysis = run.analyze(sdc);
        check_ep(&analysis, &exp[variant], "r2/D");
    }

    // STA-REQ-0220: common period P > 1000 * max(T_L, T_C) emits a T0021 error.
    let sdc_excessive = "\
create_clock -name A -period 1.001 [get_ports ca]
create_clock -name B -period 1.003 [get_ports cb]
";
    let err = EngineAnalysis::analyze_single_corner(
        &run.netlist,
        &run.libs,
        sdc_excessive,
        &AnalysisOptions::default(),
    )
    .expect_err("expected T0021 LCM ratio error");
    assert!(
        err.iter()
            .any(|d| d.severity == Severity::Error && d.code == "T0021"),
        "expected T0021 LCM ratio error, got: {err:?}"
    );
}

// STA-REQ-0221, STA-REQ-0211: latch transparency window, opening-edge uncertainty, and
// set_max_time_borrow.
#[test]
fn latch_open_uncertainty_and_max_time_borrow() {
    let exp = load_expected("latch_open_uncertainty");
    let verilog = "\
module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);
 wire n1, n2, m1, m2, k1, k2;
 DFF r1 (.D(a), .CK(cb), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 LAT l1 (.D(n2), .G(ca), .Q(q1));
 DFF r2 (.D(b), .CK(ca), .Q(m1));
 BUF b2 (.A(m1), .Y(m2));
 LAT l2 (.D(m2), .G(ca), .Q(q2));
 DFFN r3 (.D(c), .CKN(ca), .Q(k1));
 BUF b3 (.A(k1), .Y(k2));
 LAT l3 (.D(k2), .G(ca), .Q(q3));
endmodule
";
    let sdc_base = "\
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
";
    let run = TestRun::new(verilog);
    let analysis_base = run.analyze(sdc_base);
    let base = &exp["base"];
    for ep in ["l1/D", "l2/D", "l3/D"] {
        check_ep(&analysis_base, base, ep);
    }

    let sdc_mtb = format!("{sdc_base}set_max_time_borrow 0.3 [get_cells l2]\n");
    let analysis_mtb = run.analyze(&sdc_mtb);
    let mtb = &exp["mtb"];
    for ep in ["l1/D", "l2/D", "l3/D"] {
        check_ep(&analysis_mtb, mtb, ep);
    }
}

// STA-REQ-0222: latch D -> Q re-launch in the opening-edge tag and Q slew from G -> Q.
#[test]
fn latch_relaunch_and_q_slew() {
    let exp = load_expected("latch_relaunch");
    let base = &exp["base"];
    let verilog = "\
module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);
 wire n1, n2, n3, n4, m1, m2, m3, m4, k1, k2, k3, k4;
 DFF r1 (.D(a), .CK(cb), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 LAT l1 (.D(n2), .G(ca), .Q(n3));
 INV u1 (.A(n3), .Y(n4));
 DFF r2 (.D(n4), .CK(ca), .Q(q1));
 DFF r3 (.D(b), .CK(ca), .Q(m1));
 BUF b2 (.A(m1), .Y(m2));
 LATS l2 (.D(m2), .G(ca), .Q(m3));
 BUF u2 (.A(m3), .Y(m4));
 DFF r4 (.D(m4), .CK(ca), .Q(q2));
 DFFN r5 (.D(c), .CKN(ca), .Q(k1));
 BUF b3 (.A(k1), .Y(k2));
 LAT l3 (.D(k2), .G(ca), .Q(k3));
 INV u3 (.A(k3), .Y(k4));
 DFF r6 (.D(k4), .CK(ca), .Q(q3));
endmodule
";
    let sdc = "\
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);
    let corner = analysis.default_corner();

    let l1_q = node_id(&analysis, "l1/Q");
    let l2_q = node_id(&analysis, "l2/Q");
    let tag_ca_r = corner
        .tags
        .base_tag(ClockId(0), Tr::Rise)
        .expect("CA rise tag");

    for (node, tr, an, key) in [
        (l1_q, Tr::Rise, An::Late, "l1 Q rise late"),
        (l1_q, Tr::Fall, An::Late, "l1 Q fall late"),
        (l1_q, Tr::Rise, An::Early, "l1 Q rise early"),
        (l1_q, Tr::Fall, An::Early, "l1 Q fall early"),
        (l2_q, Tr::Rise, An::Late, "l2 Q rise late"),
        (l2_q, Tr::Fall, An::Late, "l2 Q fall late"),
    ] {
        assert_close(
            corner.arrival(node, tag_ca_r, tr, an),
            expect_f64(base, key),
            key,
        );
    }

    // Verify LATS l2/Q slew is the G -> Q enable arc slew (0.09 rise, 0.08 fall), not D -> Q (0.13, 0.12).
    assert_close(
        corner.slew(l2_q, Tr::Rise, An::Late),
        0.09,
        "l2/Q slew rise",
    );
    assert_close(
        corner.slew(l2_q, Tr::Fall, An::Late),
        0.08,
        "l2/Q slew fall",
    );

    for ep in ["r2/D", "r4/D", "r6/D"] {
        check_ep(&analysis, base, ep);
    }
}

// STA-REQ-0223: three-state enable/disable propagation and bus merging.
#[test]
fn tristate_bus_enable_disable_and_merging() {
    let exp = load_expected("tristate_bus");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input a, input b, output y, output q);
 wire qa, qe, qe2, n1;
 DFF r1 (.D(a), .CK(clk), .Q(qa));
 DFF r2 (.D(b), .CK(clk), .Q(qe));
 INV i1 (.A(qe), .Y(qe2));
 BUFZ z1 (.A(qa), .EN(qe), .Z(y));
 BUFZ z2 (.A(qa), .EN(qe2), .Z(y));
 INV i2 (.A(y), .Y(n1));
 DFF r3 (.D(n1), .CK(clk), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a b}]
set_output_delay -clock clk 0.3 [get_ports {y q}]
set_load 0.02 [get_ports y]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);
    let corner = analysis.default_corner();
    let tag = corner
        .tags
        .base_tag(ClockId(0), Tr::Rise)
        .expect("clk rise tag");

    let y_net = analysis.find_net("y").expect("bus net y");
    assert_close(
        corner.net_cap(y_net, Tr::Rise, An::Late),
        expect_f64(base, "C_bus"),
        "C_bus",
    );

    let y_node = node_id(&analysis, "y");
    for (tr, an, arr_key, slew_key) in [
        (Tr::Rise, An::Late, "y arr rise late", "y slew rise late"),
        (Tr::Rise, An::Early, "y arr rise early", "y slew rise early"),
        (Tr::Fall, An::Late, "y arr fall late", "y slew fall late"),
        (Tr::Fall, An::Early, "y arr fall early", "y slew fall early"),
    ] {
        assert_close(
            corner.arrival(y_node, tag, tr, an),
            expect_f64(base, arr_key),
            arr_key,
        );
        assert_close(
            corner.slew(y_node, tr, an),
            expect_f64(base, slew_key),
            slew_key,
        );
    }

    for ep in ["y", "r3/D"] {
        check_ep(&analysis, base, ep);
    }
}

// STA-REQ-0224, STA-REQ-0214: recovery and removal checks and the async_default path group.
#[test]
fn recovery_removal_and_async_default_group() {
    let exp = load_expected("recovery_removal");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input rst, input d, output q);
 wire rq, r1, r2, rn;
 DFF r0 (.D(rst), .CK(clk), .Q(rq));
 BUF b1 (.A(rq), .Y(r1));
 BUF b2 (.A(r1), .Y(r2));
 BUF b3 (.A(r2), .Y(rn));
 DFFR f1 (.D(d), .CK(clk), .RN(rn), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 1 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {rst d}]
set_output_delay -clock clk 0.3 [get_ports q]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);

    for ep in ["r0/D", "f1/D", "q"] {
        check_ep(&analysis, base, ep);
    }
    assert_opt_close(
        analysis.setup_slack_for("f1/RN"),
        expect_f64(base, "f1/RN recovery"),
        "f1/RN recovery",
    );
    assert_opt_close(
        analysis.hold_slack_for("f1/RN"),
        expect_f64(base, "f1/RN removal"),
        "f1/RN removal",
    );
    assert_opt_close(
        analysis.sta_summary.wns_setup_ns,
        expect_f64(base, "setup WNS"),
        "setup WNS",
    );

    // Verify f1/RN check records carry Recovery/Removal kinds and async_default path group.
    let corner = analysis.default_corner();
    let rn_id = node_id(&analysis, "f1/RN");
    let rn_ep = corner.endpoint(rn_id).expect("f1/RN endpoint");
    let rec = rn_ep.setup.as_ref().expect("f1/RN setup/recovery record");
    let rem = rn_ep.hold.as_ref().expect("f1/RN hold/removal record");
    assert_eq!(rec.path_group, "async_default");
    assert_eq!(rem.path_group, "async_default");
    assert_eq!(rec.kind, EndpointCheckKind::Recovery);
    assert_eq!(rem.kind, EndpointCheckKind::Removal);
    assert!(
        corner.endpoints.path_groups.contains_key("async_default"),
        "async_default group present in corner path groups"
    );
}

// STA-REQ-0216, STA-REQ-0225, STA-REQ-0200: active-high (AND2) and active-low (OR2) clock-gating
// checks and inverted clock sense.
#[test]
fn clock_gating_active_high_low_and_inverted_sense() {
    let exp = load_expected("clock_gating");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input a, input d, output q1, output q2, output q3, output q4);
 wire e, e2, f, f2, g1, g2, g3, g4;
 DFF r1 (.D(a), .CK(clk), .Q(e));
 BUF b (.A(e), .Y(e2));
 DFFN r4 (.D(a), .CKN(clk), .Q(f));
 BUF bf (.A(f), .Y(f2));
 AND2 c1 (.A(clk), .B(e2), .Y(g1));
 OR2 c2 (.A(clk), .B(e2), .Y(g2));
 AND2 c3 (.A(clk), .B(f2), .Y(g3));
 OR2 c4 (.A(clk), .B(f2), .Y(g4));
 DFF r2 (.D(d), .CK(g1), .Q(q1));
 DFF r3 (.D(d), .CK(g2), .Q(q2));
 DFF r5 (.D(d), .CK(g3), .Q(q3));
 DFF r6 (.D(d), .CK(g4), .Q(q4));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a d}]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);
    for ep in ["c1/B", "c2/B", "c3/B", "c4/B", "r2/D"] {
        check_ep(&analysis, base, ep);
    }

    // Also test clock-gating through an inverted clock network (Negative sense on gating clock pin).
    let verilog_inv = "\
module top_inv (input clk, a, d, output q);
  wire clk_n, e, en, g;
  INV  u_inv (.A(clk),   .Y(clk_n));
  DFF  r1    (.D(a),     .CK(clk), .Q(e));
  BUF  b1    (.A(e),     .Y(en));
  AND2 cg    (.A(clk_n), .B(en),   .Y(g));
  DFF  r2    (.D(d),     .CK(g),   .Q(q));
endmodule
";
    let sdc_inv = "\
create_clock -name clk -period 2.0 -waveform {0.0 0.8} [get_ports clk]
set_clock_uncertainty -setup 0.05 [get_clocks clk]
set_clock_uncertainty -hold  0.02 [get_clocks clk]
";
    let run_inv = TestRun::new(verilog_inv);
    let analysis_inv = run_inv.analyze(sdc_inv);
    let inv_setup = analysis_inv.setup_slack_for("cg/B").expect("cg/B setup");
    let inv_hold = analysis_inv.hold_slack_for("cg/B").expect("cg/B hold");
    assert_close(inv_setup, 0.8 - 0.05 - 0.629, "inverted cg/B setup");
    assert_close(inv_hold, 0.559 - (0.0 + 0.02), "inverted cg/B hold");
}

// STA-REQ-0226: endpoint status taxonomy and completeness counts.
#[test]
fn endpoint_status_taxonomy_and_completeness() {
    let exp = load_expected("endpoint_status");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input ck2, input a, input u, input rn, output y, output z);
 wire lo, n1, n2, q2, q3, q4;
 TIELO t0 (.Y(lo));
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(q2));
 DFF r3 (.D(n2), .CK(ck2), .Q(q3));
 DFF r4 (.D(u), .CK(clk), .Q(q4));
 DFF r5 (.D(lo), .CK(clk), .Q(z));
 DFFR r6 (.D(n2), .CK(clk), .RN(rn), .Q(y));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a rn}]
set_output_delay -clock clk 0.3 [get_ports y]
";
    let run = TestRun::new(verilog);
    let analysis = run.analyze(sdc);

    for ep in ["r1/D", "r2/D", "r6/D", "y"] {
        check_ep(&analysis, base, ep);
    }
    assert_opt_close(
        analysis.setup_slack_for("r6/RN"),
        expect_f64(base, "r6/RN recovery"),
        "r6/RN recovery",
    );
    assert_opt_close(
        analysis.hold_slack_for("r6/RN"),
        expect_f64(base, "r6/RN removal"),
        "r6/RN removal",
    );

    let status_of = |name: &str| -> EndpointStatus {
        let id = node_id(&analysis, name);
        analysis
            .default_corner()
            .endpoint(id)
            .unwrap_or_else(|| panic!("missing endpoint '{name}'"))
            .status
    };

    assert_eq!(status_of("r1/D"), EndpointStatus::Constrained);
    assert_eq!(status_of("r2/D"), EndpointStatus::Constrained);
    assert_eq!(status_of("r6/D"), EndpointStatus::Constrained);
    assert_eq!(status_of("r6/RN"), EndpointStatus::Constrained);
    assert_eq!(status_of("y"), EndpointStatus::Constrained);
    assert_eq!(status_of("r3/D"), EndpointStatus::Unclocked);
    assert_eq!(status_of("r4/D"), EndpointStatus::NoArrival);
    assert_eq!(status_of("r5/D"), EndpointStatus::NoArrival);
    assert_eq!(status_of("z"), EndpointStatus::UnconstrainedOutput);

    let summary = &analysis.sta_summary;
    assert_eq!(summary.endpoints, 5);
    assert_eq!(summary.unconstrained_endpoints, 4);
    let unc = &analysis.completeness.unconstrained_endpoints;
    assert_eq!(unc.unclocked, 1);
    assert_eq!(unc.no_arrival, 2);
    assert_eq!(unc.unconstrained_output, 1);
    assert_eq!(unc.disabled, 0);

    // Also verify Disabled status when check arcs on r2 are disabled via set_disable_timing.
    let sdc_dis = format!("{sdc}set_disable_timing [get_cells r2]\n");
    let analysis_dis = run.analyze(&sdc_dis);
    let r2_d = node_id(&analysis_dis, "r2/D");
    let r2_ep = analysis_dis
        .default_corner()
        .endpoint(r2_d)
        .expect("r2/D endpoint");
    assert_eq!(r2_ep.status, EndpointStatus::Disabled);
    assert_eq!(
        analysis_dis.completeness.unconstrained_endpoints.disabled,
        1
    );
}

// STA-REQ-0015, STA-REQ-0214: multi-corner orchestration and cross-corner validation.
#[test]
fn two_corners_orchestration_and_validation() {
    let exp = load_expected("two_corners");
    let fast_exp = &exp["fast"];
    let slow_exp = &exp["slow"];
    let verilog = "\
module top (input clk, d, output q);
  wire n1, n2;
  DFF r1 (.D(d),  .CK(clk), .Q(n1));
  BUF u1 (.A(n1), .Y(n2));
  DFF r2 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let sdc_text = "create_clock -name clk -period 2.0 [get_ports clk]\n";
    let fast_lib = parse_lib(&analytic_liberty("lin_fast", 1.0));
    let slow_lib = parse_lib(&analytic_liberty("lin_slow", 2.0));
    let netlist = build_netlist(verilog, &fast_lib);
    let fast_libs = [fast_lib];
    let slow_libs = [slow_lib];
    let corners = [
        CornerLibs {
            name: "fast",
            libs: &fast_libs,
        },
        CornerLibs {
            name: "slow",
            libs: &slow_libs,
        },
    ];

    let analysis = EngineAnalysis::analyze_sdc(
        &netlist,
        &corners,
        sdc_text,
        "two_corners.sdc",
        &AnalysisOptions::default(),
        Ctx::NONE,
    )
    .expect("two_corners EngineAnalysis");

    assert_eq!(analysis.corners.len(), 2);
    let r2_d = node_id(&analysis, "r2/D");

    let fast_corner = analysis.corner("fast").expect("fast corner");
    assert_opt_close(
        fast_corner.setup_slack(r2_d),
        expect_f64(fast_exp, "r2/D setup"),
        "fast r2/D setup",
    );
    assert_opt_close(
        fast_corner.hold_slack(r2_d),
        expect_f64(fast_exp, "r2/D hold"),
        "fast r2/D hold",
    );

    let slow_corner = analysis.corner("slow").expect("slow corner");
    assert_opt_close(
        slow_corner.setup_slack(r2_d),
        expect_f64(slow_exp, "r2/D setup"),
        "slow r2/D setup",
    );
    assert_opt_close(
        slow_corner.hold_slack(r2_d),
        expect_f64(slow_exp, "r2/D hold"),
        "slow r2/D hold",
    );

    // Worst across corners: setup from slow (0.552), hold from fast (0.506).
    assert_opt_close(
        analysis.setup_slack_for("r2/D"),
        expect_f64(slow_exp, "r2/D setup"),
        "overall r2/D setup",
    );
    assert_opt_close(
        analysis.hold_slack_for("r2/D"),
        expect_f64(fast_exp, "r2/D hold"),
        "overall r2/D hold",
    );
}
