// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-engine-known-answer` (`STA-REQ-0015`, `STA-REQ-0016`, `STA-REQ-0108` to `0125`,
//! `STA-REQ-0229`, `STA-REQ-0212`, `STA-REQ-0214`, `STA-REQ-0216` to `0226`, `STA-REQ-0501`):
//! end-to-end analytic known-answer verification of [`EngineAnalysis`] against the closed-form
//! derivations in `verification/analytic/sta/*/DERIVATION.md` and `expected.json`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::input::constr::CornerLibs;

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
const XTR_R: Lin = Lin::new(0.05, -0.04, 1.0, 0.10, -0.08, 0.5);
const XTR_F: Lin = Lin::new(0.05, -0.04, 1.0, 0.10, -0.08, 0.5);

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

fn analytic_liberty_with_header(name: &str, k: f64, extra_header: &str) -> String {
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
    let xtr_r = XTR_R.scale(k);
    let xtr_f = XTR_F.scale(k);
    let setup = SETUP * k;
    let hold = HOLD * k;
    let rec = RECOVERY * k;
    let rem = REMOVAL * k;

    let mut o = format!(
        "library ({name}) {{\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n{extra_header}  lu_table_template (t2) {{\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }}\n"
    );

    for (cell, ins, func, sense, r, f, cout) in [
        ("INV", vec!["A"], "!A", "negative_unate", inv_r, inv_f, 0.0),
        ("BUF", vec!["A"], "A", "positive_unate", buf_r, buf_f, 0.0),
        (
            "BUFO",
            vec!["A"],
            "A",
            "positive_unate",
            buf_r,
            buf_f,
            COUT_O,
        ),
        ("XTR", vec!["A"], "A", "positive_unate", xtr_r, xtr_f, 0.0),
        (
            "AND2",
            vec!["A", "B"],
            "A&B",
            "positive_unate",
            buf_r,
            buf_f,
            0.0,
        ),
        (
            "OR2",
            vec!["A", "B"],
            "A|B",
            "positive_unate",
            buf_r,
            buf_f,
            0.0,
        ),
        (
            "XOR2",
            vec!["A", "B"],
            "A^B",
            "non_unate",
            xor_r,
            xor_f,
            0.0,
        ),
    ] {
        let _ = writeln!(o, "  cell ({cell}) {{\n    area : 1;");
        for i in &ins {
            let _ = writeln!(
                o,
                "    pin ({i}) {{ direction : input; capacitance : {CIN}; }}"
            );
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n      capacitance : {cout};\n      function : \"{func}\";"
        );
        for i in &ins {
            write_arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }

    // MUX2 (A0, A1 positive_unate BUF; S non_unate XOR).
    let _ = writeln!(
        o,
        "  cell (MUX2) {{\n    area : 2;\n    \
         pin (A0) {{ direction : input; capacitance : {CIN}; }}\n    \
         pin (A1) {{ direction : input; capacitance : {CIN}; }}\n    \
         pin (S) {{ direction : input; capacitance : {CIN}; }}\n    \
         pin (Y) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"(A0 & !S) | (A1 & S)\";"
    );
    write_arc(
        &mut o,
        "A0",
        "positive_unate",
        "combinational",
        buf_r,
        buf_f,
    );
    write_arc(
        &mut o,
        "A1",
        "positive_unate",
        "combinational",
        buf_r,
        buf_f,
    );
    write_arc(&mut o, "S", "non_unate", "combinational", xor_r, xor_f);
    let _ = writeln!(o, "    }}\n  }}");

    for (cell, func) in [("TIELO", "0"), ("TIEHI", "1")] {
        let _ = writeln!(
            o,
            "  cell ({cell}) {{\n    area : 1;\n    pin (Y) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"{func}\";\n    }}\n  }}"
        );
    }

    let _ = writeln!(o, "  cell (BUFZ) {{\n    area : 2;");
    let _ = writeln!(
        o,
        "    pin (A) {{ direction : input; capacitance : {CIN}; }}"
    );
    let _ = writeln!(
        o,
        "    pin (EN) {{ direction : input; capacitance : {CIN}; }}"
    );
    let _ = writeln!(
        o,
        "    pin (Z) {{\n      direction : output;\n      capacitance : {COUT_Z};\n      function : \"A\";\n      three_state : \"!EN\";"
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
        let _ = writeln!(o, "  cell ({cell}) {{\n    area : 4;\n    {group}");
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(&mut o, ck, &format!("setup_{edge}"), setup);
        write_check_arc(&mut o, ck, &format!("hold_{edge}"), hold);
        let _ = writeln!(o, "    }}");
        let _ = writeln!(
            o,
            "    pin ({ck}) {{ direction : input; capacitance : {CIN}; clock : true; }}"
        );
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      capacitance : 0.0;\n      function : \"IQ\";"
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

struct CaseRunner {
    netlist: Box<FrozenNetlist<'static>>,
    libs: Vec<Library>,
}

impl CaseRunner {
    fn new(verilog: &str) -> Self {
        Self::with_header(verilog, "")
    }

    fn with_header(verilog: &str, extra_header: &str) -> Self {
        let lib = parse_lib(&analytic_liberty_with_header("lin", 1.0, extra_header));
        let netlist = Box::new(build_netlist(verilog, &lib));
        Self {
            netlist,
            libs: vec![lib],
        }
    }

    fn analyze<'a>(&'a self, sdc: &str) -> EngineAnalysis<'a> {
        EngineAnalysis::analyze_single_corner(
            &self.netlist,
            &self.libs,
            sdc,
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

fn assert_opt_close(got: Option<f64>, want: f64, label: &str) {
    let g = got.unwrap_or_else(|| panic!("{label}: expected Some({want}), got None"));
    assert!(
        (g - want).abs() < EPS,
        "{label}: got {g:.12}, want {want:.12} (diff {:.3e})",
        (g - want).abs()
    );
}

// STA-REQ-0110 to 0114, STA-REQ-0229, STA-REQ-0212, STA-REQ-0501: inverter_chain.
#[test]
fn known_answer_inverter_chain() {
    let verilog = "\
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_output_delay -clock clk 0.3 [get_ports q]
set_load 0.02 [get_ports q]
";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    assert_opt_close(a.setup_slack_for("r2/D"), 1.2511, "r2/D setup");
    assert_opt_close(a.hold_slack_for("r2/D"), 0.54975, "r2/D hold");
    assert_opt_close(a.setup_slack_for("r1/D"), 1.40, "r1/D setup");
    assert_opt_close(a.hold_slack_for("r1/D"), 0.45, "r1/D hold");
    assert_opt_close(a.setup_slack_for("q"), 1.28, "q setup");
    assert_opt_close(a.hold_slack_for("q"), 0.668, "q hold");
}

// STA-REQ-0012, STA-REQ-0112, STA-REQ-0501: reconvergent_xor.
#[test]
fn known_answer_reconvergent_xor() {
    let verilog = "\
module m (input clk, input a, output q);
 wire n1, n2, n3;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 XOR2 x (.A(n1), .B(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\n";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    let qr = CKQ_R.delay(0.0, 2.0 * CIN);
    let sr = CKQ_R.raw_slew(0.0, 2.0 * CIN);
    let qf = CKQ_F.delay(0.0, 2.0 * CIN);
    let sf = CKQ_F.raw_slew(0.0, 2.0 * CIN);
    let br = qr + BUF_R.delay(sr, CIN);
    let br_s = BUF_R.raw_slew(sr, CIN);
    let bf = qf + BUF_F.delay(sf, CIN);
    let bf_s = BUF_F.raw_slew(sf, CIN);
    let ins = [(qr, sr), (qf, sf), (br, br_s), (bf, bf_s)];
    let late = ins
        .iter()
        .flat_map(|(arr, s)| [arr + XOR_R.delay(*s, CIN), arr + XOR_F.delay(*s, CIN)])
        .fold(f64::MIN, f64::max);
    let early = ins
        .iter()
        .flat_map(|(arr, s)| [arr + XOR_R.delay(*s, CIN), arr + XOR_F.delay(*s, CIN)])
        .fold(f64::MAX, f64::min);
    assert_opt_close(a.setup_slack_for("r2/D"), 2.0 - SETUP - late, "r2/D setup");
    assert_opt_close(a.hold_slack_for("r2/D"), early - HOLD, "r2/D hold");
}

// STA-REQ-0229, STA-REQ-0501: falling_edge_capture.
#[test]
fn known_answer_falling_edge_capture() {
    let verilog = "\
module m (input clk, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 INV u (.A(n1), .Y(n2));
 DFFN r2 (.D(n2), .CKN(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 -waveform {0 0.6} [get_ports clk]\n";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    let qr = CKQ_R.delay(0.0, CIN);
    let sr = CKQ_R.raw_slew(0.0, CIN);
    let qf = CKQ_F.delay(0.0, CIN);
    let sf = CKQ_F.raw_slew(0.0, CIN);
    let late = (qr + INV_F.delay(sr, CIN)).max(qf + INV_R.delay(sf, CIN));
    let early = (qr + INV_F.delay(sr, CIN)).min(qf + INV_R.delay(sf, CIN));
    assert_opt_close(a.setup_slack_for("r2/D"), 0.6 - SETUP - late, "r2/D setup");
    assert_opt_close(
        a.hold_slack_for("r2/D"),
        early - (0.6 - 2.0 + HOLD),
        "r2/D hold",
    );
}

// STA-REQ-0016, STA-REQ-0501: thresholds.
#[test]
fn known_answer_thresholds() {
    let exp = load_expected("thresholds");
    let base = &exp["base"];
    let header = "\
  input_threshold_pct_rise : 40;
  input_threshold_pct_fall : 60;
  output_threshold_pct_rise : 40;
  output_threshold_pct_fall : 60;
  slew_lower_threshold_pct_rise : 30;
  slew_lower_threshold_pct_fall : 30;
  slew_upper_threshold_pct_rise : 70;
  slew_upper_threshold_pct_fall : 70;
  slew_derate_from_library : 0.5;
";
    let verilog = "\
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_output_delay -clock clk 0.3 [get_ports q]
set_load 0.02 [get_ports q]
";
    let run = CaseRunner::with_header(verilog, header);
    let a = run.analyze(sdc);
    for ep in ["r1/D", "r2/D", "q"] {
        assert_opt_close(
            a.setup_slack_for(ep),
            expect_f64(base, &format!("{ep} setup")),
            &format!("{ep} setup"),
        );
        assert_opt_close(
            a.hold_slack_for(ep),
            expect_f64(base, &format!("{ep} hold")),
            &format!("{ep} hold"),
        );
    }
}

// STA-REQ-0108, STA-REQ-0109, STA-REQ-0501: per_pin_slew.
#[test]
fn known_answer_per_pin_slew() {
    let exp = load_expected("per_pin_slew");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input a, input s, output q);
 wire n1, n2, n3;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 MUX2 x (.A0(n1), .A1(n1), .S(s), .Y(n2));
 BUF b (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_input_transition 0.4 [get_ports s]
";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    assert_opt_close(
        a.setup_slack_for("r2/D"),
        expect_f64(base, "r2/D setup"),
        "r2/D setup",
    );
    assert_opt_close(
        a.hold_slack_for("r2/D"),
        expect_f64(base, "r2/D hold"),
        "r2/D hold",
    );
}

// STA-REQ-0120, STA-REQ-0501: slew_clamp.
#[test]
fn known_answer_slew_clamp() {
    let exp = load_expected("slew_clamp");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input a, output q);
 wire n1, n2;
 XTR x (.A(a), .Y(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r1 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.3 [get_ports a]
set_input_transition 2.0 [get_ports a]
";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    assert_opt_close(
        a.setup_slack_for("r1/D"),
        expect_f64(base, "r1/D setup"),
        "r1/D setup",
    );
    assert_opt_close(
        a.hold_slack_for("r1/D"),
        expect_f64(base, "r1/D hold"),
        "r1/D hold",
    );
}

// STA-REQ-0121, STA-REQ-0501: driver_pin_cap.
#[test]
fn known_answer_driver_pin_cap() {
    let exp = load_expected("driver_pin_cap");
    let base = &exp["base"];
    let verilog = "\
module m (input clk, input a, output q);
 wire n1, n2, n3, n4;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUFO b1 (.A(n1), .Y(n2));
 INV u1 (.A(n2), .Y(n3));
 INV u2 (.A(n2), .Y(n4));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\n";
    let run = CaseRunner::new(verilog);
    let a = run.analyze(sdc);
    assert_opt_close(
        a.setup_slack_for("r2/D"),
        expect_f64(base, "r2/D setup"),
        "r2/D setup",
    );
    assert_opt_close(
        a.hold_slack_for("r2/D"),
        expect_f64(base, "r2/D hold"),
        "r2/D hold",
    );
}

// STA-REQ-0122 to 0125, STA-REQ-0501: wireload_lumped.
#[test]
fn known_answer_wireload_lumped() {
    let exp = load_expected("wireload_lumped");
    let base = &exp["base"];
    let header = "\
  wire_load (wl_lin) {
    capacitance : 0.001;
    slope : 2.0;
    fanout_length (1, 2.0);
    fanout_length (3, 5.0);
  }
  default_wire_load : \"wl_lin\";
  default_wire_load_mode : \"top\";
";
    let verilog = "\
module m (input clk, input a, output y, output z);
 wire n1, n2, n3, w3, w4, w5, q2;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 BUF b2 (.A(n2), .Y(n3));
 BUF x1 (.A(n2), .Y(y));
 DFF r2 (.D(n3), .CK(clk), .Q(q2));
 BUF x2 (.A(n3), .Y(z));
 BUF x3 (.A(n3), .Y(w3));
 BUF x4 (.A(n3), .Y(w4));
 BUF x5 (.A(n3), .Y(w5));
endmodule
";
    let sdc = "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_output_delay -clock clk 0.2 [get_ports {y z}]
set_load 0.004 [get_ports y]
";
    let run = CaseRunner::with_header(verilog, header);
    let a = run.analyze(sdc);
    for ep in ["r2/D", "y"] {
        assert_opt_close(
            a.setup_slack_for(ep),
            expect_f64(base, &format!("{ep} setup")),
            &format!("{ep} setup"),
        );
        assert_opt_close(
            a.hold_slack_for(ep),
            expect_f64(base, &format!("{ep} hold")),
            &format!("{ep} hold"),
        );
    }
}

// STA-REQ-0015, STA-REQ-0217 to 0226, STA-REQ-0501: all E5 analytic derivations.
#[test]
fn known_answer_e5_analytic_cases() {
    // 1. io_delay_latency
    let exp = load_expected("io_delay_latency");
    let run = CaseRunner::new(
        "\
module m (input clk, input d, input e, input f, output q, output p);
 wire n1, n2, n3, q3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(q));
 BUF b2 (.A(e), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(p));
 BUF b3 (.A(f), .Y(n3));
 DFF r3 (.D(n3), .CK(clk), .Q(q3));
endmodule
",
    );
    let a = run.analyze(
        "\
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
",
    );
    for ep in ["r1/D", "r2/D", "r3/D", "q", "p"] {
        assert_opt_close(
            a.setup_slack_for(ep),
            expect_f64(&exp["base"], &format!("{ep} setup")),
            &format!("io_delay_latency {ep} setup"),
        );
        assert_opt_close(
            a.hold_slack_for(ep),
            expect_f64(&exp["base"], &format!("{ep} hold")),
            &format!("io_delay_latency {ep} hold"),
        );
    }

    // 2. one_sided_delays
    let exp = load_expected("one_sided_delays");
    let run = CaseRunner::new(
        "\
module m (input clk, input d, input e, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(n2));
 BUF b2 (.A(n2), .Y(q));
 BUF b3 (.A(e), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(p));
endmodule
",
    );
    let a = run.analyze(
        "\
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk -max 0.5 [get_ports d]
set_input_delay -clock clk -min 0.1 [get_ports e]
set_output_delay -clock clk -min -0.1 [get_ports q]
set_output_delay -clock clk -max 0.4 [get_ports p]
",
    );
    assert_opt_close(
        a.setup_slack_for("r1/D"),
        expect_f64(&exp["base"], "r1/D setup"),
        "one_sided r1/D setup",
    );
    assert_opt_close(
        a.hold_slack_for("r2/D"),
        expect_f64(&exp["base"], "r2/D hold"),
        "one_sided r2/D hold",
    );
    assert_opt_close(
        a.hold_slack_for("q"),
        expect_f64(&exp["base"], "q hold"),
        "one_sided q hold",
    );
    assert_opt_close(
        a.setup_slack_for("p"),
        expect_f64(&exp["base"], "p setup"),
        "one_sided p setup",
    );

    // 3. two_corners
    let exp = load_expected("two_corners");
    let fast_lib = parse_lib(&analytic_liberty_with_header("lin_fast", 1.0, ""));
    let slow_lib = parse_lib(&analytic_liberty_with_header("lin_slow", 2.0, ""));
    let netlist = build_netlist(
        "module top (input clk, d, output q); wire n1, n2; DFF r1 (.D(d), .CK(clk), .Q(n1)); BUF u1 (.A(n1), .Y(n2)); DFF r2 (.D(n2), .CK(clk), .Q(q)); endmodule",
        &fast_lib,
    );
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
    let a = EngineAnalysis::analyze_sdc(
        &netlist,
        &corners,
        "create_clock -name clk -period 2.0 [get_ports clk]\n",
        "two_corners.sdc",
        &AnalysisOptions::default(),
        Ctx::NONE,
    )
    .expect("two_corners");
    assert_opt_close(
        a.setup_slack_for("r2/D"),
        expect_f64(&exp["slow"], "r2/D setup"),
        "two_corners worst setup",
    );
    assert_opt_close(
        a.hold_slack_for("r2/D"),
        expect_f64(&exp["fast"], "r2/D hold"),
        "two_corners worst hold",
    );
}
