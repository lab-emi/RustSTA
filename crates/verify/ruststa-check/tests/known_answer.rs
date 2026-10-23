// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Known-answer derivations (`STA-REQ-0501`) and metamorphic properties (`STA-REQ-0503`) for the
//! independent checker (`crates/verify/ruststa-check/src/rev`).

use std::fmt::Write as _;
use std::path::Path;

use rusteda_core::liberty::Library;
use rusteda_formats::liberty::read_text;
use ruststa_check::rev::{
    Analysis, EndpointEval, EndpointStatus, MultiCornerAnalysis, Trans, analyze_inputs,
};

#[derive(Debug, Clone, Copy)]
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
    const fn scale(self, k: f64) -> Self {
        Self::new(
            self.d0 * k,
            self.ks * k,
            self.kc * k,
            self.s0 * k,
            self.ms * k,
            self.mc * k,
        )
    }
    fn delay(self, slew: f64, load: f64) -> f64 {
        self.d0 + self.ks * slew + self.kc * load
    }
    fn slew(self, slew: f64, load: f64) -> f64 {
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

fn table2(out: &mut String, kind: &str, v: impl Fn(f64, f64) -> f64) {
    let _ = writeln!(
        out,
        "        {kind} (t2) {{ values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\"); }}",
        v(0.0, 0.0),
        v(0.0, 1.0),
        v(1.0, 0.0),
        v(1.0, 1.0)
    );
}

fn table1(out: &mut String, kind: &str, v: impl Fn(f64) -> f64) {
    let _ = writeln!(
        out,
        "        {kind} (t1) {{ values (\"{:.9}, {:.9}\"); }}",
        v(0.0),
        v(1.0)
    );
}

fn arc(out: &mut String, related: &str, sense: &str, ty: &str, r: Lin, f: Lin) {
    let _ = writeln!(
        out,
        "      timing () {{\n        related_pin : \"{related}\";\n        timing_sense : {sense};\n        timing_type : {ty};"
    );
    table2(out, "cell_rise", |s, c| r.delay(s, c));
    table2(out, "rise_transition", |s, c| r.slew(s, c));
    table2(out, "cell_fall", |s, c| f.delay(s, c));
    table2(out, "fall_transition", |s, c| f.slew(s, c));
    let _ = writeln!(out, "      }}");
}

fn check_arc(out: &mut String, related: &str, ty: &str, v: f64) {
    let _ = writeln!(
        out,
        "      timing () {{\n        related_pin : \"{related}\";\n        timing_type : {ty};\n        \
         rise_constraint (scalar) {{ values (\"{v}\"); }}\n        \
         fall_constraint (scalar) {{ values (\"{v}\"); }}\n      }}"
    );
}

fn input_pin(out: &mut String, p: &str, clock: bool) {
    let ck = if clock { " clock : true;" } else { "" };
    let _ = writeln!(
        out,
        "    pin ({p}) {{ direction : input; capacitance : {CIN};{ck} }}"
    );
}

pub fn build_synthetic_lib(name: &str, k: f64, header_extra: &str) -> Library {
    let mut o = format!(
        "library ({name}) {{\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n{header_extra}  \
         lu_table_template (t1) {{ variable_1 : input_net_transition; index_1 (\"0, 1\"); }}\n  \
         lu_table_template (t2) {{\n    variable_1 : input_net_transition;\n    \
         variable_2 : total_output_net_capacitance;\n    index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }}\n"
    );

    for (cname, ins, func, sense, r, f, cout) in [
        ("INV", vec!["A"], "!A", "negative_unate", INV_R, INV_F, 0.0),
        ("BUF", vec!["A"], "A", "positive_unate", BUF_R, BUF_F, 0.0),
        (
            "BUFO",
            vec!["A"],
            "A",
            "positive_unate",
            BUF_R,
            BUF_F,
            COUT_O,
        ),
        (
            "AND2",
            vec!["A", "B"],
            "A&B",
            "positive_unate",
            BUF_R,
            BUF_F,
            0.0,
        ),
        (
            "OR2",
            vec!["A", "B"],
            "A|B",
            "positive_unate",
            BUF_R,
            BUF_F,
            0.0,
        ),
        (
            "XOR2",
            vec!["A", "B"],
            "A^B",
            "non_unate",
            XOR_R,
            XOR_F,
            0.0,
        ),
        ("XTR", vec!["A"], "A", "positive_unate", XTR_R, XTR_F, 0.0),
    ] {
        let _ = writeln!(o, "  cell ({cname}) {{\n    area : 1;");
        for i in &ins {
            input_pin(&mut o, i, false);
        }
        let cap_str = if cout > 0.0 {
            format!("      capacitance : {cout};\n")
        } else {
            String::new()
        };
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n{cap_str}      function : \"{func}\";"
        );
        for i in &ins {
            arc(&mut o, i, sense, "combinational", r.scale(k), f.scale(k));
        }
        let _ = writeln!(o, "    }}\n  }}");
    }

    let _ = writeln!(o, "  cell (MUX2) {{\n    area : 2;");
    for p in ["A0", "A1", "S"] {
        input_pin(&mut o, p, false);
    }
    let _ = writeln!(
        o,
        "    pin (Y) {{\n      direction : output;\n      function : \"(A0&!S)|(A1&S)\";"
    );
    arc(
        &mut o,
        "A0",
        "positive_unate",
        "combinational",
        BUF_R.scale(k),
        BUF_F.scale(k),
    );
    arc(
        &mut o,
        "A1",
        "positive_unate",
        "combinational",
        BUF_R.scale(k),
        BUF_F.scale(k),
    );
    arc(
        &mut o,
        "S",
        "non_unate",
        "combinational",
        XOR_R.scale(k),
        XOR_F.scale(k),
    );
    let _ = writeln!(o, "    }}\n  }}");

    for (cname, val) in [("TIELO", "0"), ("TIEHI", "1")] {
        let _ = writeln!(
            o,
            "  cell ({cname}) {{ area : 1; pin (Y) {{ direction : output; function : \"{val}\"; }} }}"
        );
    }

    let _ = writeln!(o, "  cell (BUFZ) {{\n    area : 2;");
    input_pin(&mut o, "A", false);
    input_pin(&mut o, "EN", false);
    let _ = writeln!(
        o,
        "    pin (Z) {{\n      direction : output;\n      capacitance : {COUT_Z};\n      function : \"A\";\n      three_state : \"!EN\";"
    );
    arc(
        &mut o,
        "A",
        "positive_unate",
        "combinational",
        BUF_R.scale(k),
        BUF_F.scale(k),
    );
    arc(
        &mut o,
        "EN",
        "positive_unate",
        "three_state_enable",
        TSE_R.scale(k),
        TSE_F.scale(k),
    );
    let _ = writeln!(
        o,
        "      timing () {{\n        related_pin : \"EN\";\n        timing_sense : negative_unate;\n        timing_type : three_state_disable;"
    );
    let (dr, df) = (TSD_R.scale(k), TSD_F.scale(k));
    table1(&mut o, "cell_rise", |s| dr.delay(s, 0.0));
    table1(&mut o, "rise_transition", |s| dr.slew(s, 0.0));
    table1(&mut o, "cell_fall", |s| df.delay(s, 0.0));
    table1(&mut o, "fall_transition", |s| df.slew(s, 0.0));
    let _ = writeln!(o, "      }}\n    }}\n  }}");

    let (setup, hold) = (0.10 * k, 0.05 * k);
    for (cname, ck, edge, group) in [
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
            "  cell ({cname}) {{\n    area : 4;\n    {group}\n    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        check_arc(&mut o, ck, &format!("setup_{edge}"), setup);
        check_arc(&mut o, ck, &format!("hold_{edge}"), hold);
        let _ = writeln!(o, "    }}");
        input_pin(&mut o, ck, true);
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        arc(
            &mut o,
            ck,
            "non_unate",
            &format!("{edge}_edge"),
            CKQ_R.scale(k),
            CKQ_F.scale(k),
        );
        let _ = writeln!(o, "    }}\n  }}");
    }

    let (rec, rem) = (0.12 * k, 0.06 * k);
    let _ = writeln!(
        o,
        "  cell (DFFR) {{\n    area : 5;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }}\n    \
         pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    check_arc(&mut o, "CK", "setup_rising", setup);
    check_arc(&mut o, "CK", "hold_rising", hold);
    let _ = writeln!(
        o,
        "    }}\n    pin (RN) {{\n      direction : input;\n      capacitance : {CIN};\n      \
         timing () {{ related_pin : \"CK\"; timing_type : recovery_rising; rise_constraint (scalar) {{ values (\"{rec}\"); }} }}\n      \
         timing () {{ related_pin : \"CK\"; timing_type : removal_rising; rise_constraint (scalar) {{ values (\"{rem}\"); }} }}\n    }}"
    );
    input_pin(&mut o, "CK", true);
    let _ = writeln!(
        o,
        "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    arc(
        &mut o,
        "CK",
        "non_unate",
        "rising_edge",
        CKQ_R.scale(k),
        CKQ_F.scale(k),
    );
    let _ = writeln!(o, "    }}\n  }}");

    for (cname, dq_r, dq_f) in [("LAT", DQ_R, DQ_F), ("LATS", DQS_R, DQS_F)] {
        let _ = writeln!(
            o,
            "  cell ({cname}) {{\n    area : 3;\n    latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }}\n    \
             pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        check_arc(&mut o, "G", "setup_falling", setup);
        check_arc(&mut o, "G", "hold_falling", hold);
        let _ = writeln!(o, "    }}");
        input_pin(&mut o, "G", true);
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        arc(
            &mut o,
            "D",
            "positive_unate",
            "combinational",
            dq_r.scale(k),
            dq_f.scale(k),
        );
        arc(
            &mut o,
            "G",
            "non_unate",
            "rising_edge",
            CKQ_R.scale(k),
            CKQ_F.scale(k),
        );
        let _ = writeln!(o, "    }}\n  }}");
    }

    o.push_str("}\n");
    read_text(&o, &format!("{name}.lib")).expect("valid synthetic library")
}

fn lib_default() -> Library {
    build_synthetic_lib("lin", 1.0, "")
}

fn solve_single(netlist: &str, sdc: &str, lib: Library) -> MultiCornerAnalysis {
    analyze_inputs(&[("n.v", netlist)], None, &[("default", vec![lib])], sdc)
        .expect("checker analysis must succeed")
}

fn ep<'a>(sa: &'a MultiCornerAnalysis, corner: &str, pin: &str) -> &'a EndpointEval {
    sa.corners[corner]
        .endpoints
        .iter()
        .find(|e| e.endpoint == pin)
        .unwrap_or_else(|| panic!("missing endpoint {pin} in corner {corner}"))
}

fn ep_group<'a>(
    sa: &'a MultiCornerAnalysis,
    corner: &str,
    pin: &str,
    path_group: &str,
) -> &'a EndpointEval {
    sa.corners[corner]
        .endpoints
        .iter()
        .find(|e| e.endpoint == pin && e.path_group == path_group)
        .unwrap_or_else(|| panic!("missing endpoint {pin} ({path_group}) in corner {corner}"))
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    let diff = (actual - expected).abs();
    assert!(
        diff <= 1e-9,
        "{label}: actual {actual:.9} != expected {expected:.9} (diff {diff:.3e})"
    );
}

fn check_pin_slacks(sa: &MultiCornerAnalysis, corner: &str, pin: &str, exp: &serde_json::Value) {
    let e = ep(sa, corner, pin);
    let s_key = format!("{pin} setup");
    let h_key = format!("{pin} hold");
    assert_close(
        e.setup_slack_ns.unwrap(),
        exp[&s_key].as_f64().unwrap(),
        &s_key,
    );
    assert_close(
        e.hold_slack_ns.unwrap(),
        exp[&h_key].as_f64().unwrap(),
        &h_key,
    );
}

fn load_expected(case: &str) -> serde_json::Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta")
        .join(case)
        .join("expected.json");
    let text = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", p.display()));
    serde_json::from_str(&text).expect("valid expected.json")
}

#[test]
fn ka_01_tristate_bus() {
    let exp = &load_expected("tristate_bus")["base"];
    let netlist = "
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
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a b}]
set_output_delay -clock clk 0.3 [get_ports {y q}]
set_load 0.02 [get_ports y]
";
    let sa = solve_single(netlist, sdc, lib_default());
    let c = &sa.corners["default"];
    let y_idx = c.node_map["y"];
    for (tr, an, key) in [
        (Trans::Rise, Analysis::Late, "y slew rise late"),
        (Trans::Rise, Analysis::Early, "y slew rise early"),
        (Trans::Fall, Analysis::Late, "y slew fall late"),
        (Trans::Fall, Analysis::Early, "y slew fall early"),
    ] {
        assert_close(
            c.slews[y_idx][tr.idx()][an.idx()].unwrap(),
            exp[key].as_f64().unwrap(),
            key,
        );
    }
    check_pin_slacks(&sa, "default", "y", exp);
    check_pin_slacks(&sa, "default", "r3/D", exp);
}

#[test]
fn ka_02_two_corners() {
    let exp = load_expected("two_corners");
    let netlist = "
module m (input clk, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\n";
    let fast_lib = build_synthetic_lib("lin_fast", 1.0, "");
    let slow_lib = build_synthetic_lib("lin_slow", 2.0, "");
    let sa = analyze_inputs(
        &[("n.v", netlist)],
        None,
        &[("fast", vec![fast_lib]), ("slow", vec![slow_lib])],
        sdc,
    )
    .expect("two_corners analysis");
    for corner in ["fast", "slow"] {
        check_pin_slacks(&sa, corner, "r2/D", &exp[corner]);
    }
}

#[test]
fn ka_03_thresholds() {
    let exp = &load_expected("thresholds")["base"];
    let netlist = "
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_output_delay -clock clk 0.3 [get_ports q]
set_load 0.02 [get_ports q]
";
    let thr_hdr = "  input_threshold_pct_rise : 40;\n  input_threshold_pct_fall : 60;\n  \
                   output_threshold_pct_rise : 40;\n  output_threshold_pct_fall : 60;\n  \
                   slew_lower_threshold_pct_rise : 30;\n  slew_lower_threshold_pct_fall : 30;\n  \
                   slew_upper_threshold_pct_rise : 70;\n  slew_upper_threshold_pct_fall : 70;\n  \
                   slew_derate_from_library : 0.5;\n";
    let sa = solve_single(netlist, sdc, build_synthetic_lib("lin_thr", 1.0, thr_hdr));
    for pin in ["r2/D", "r1/D", "q"] {
        check_pin_slacks(&sa, "default", pin, exp);
    }
}

#[test]
fn ka_04_per_pin_slew() {
    let exp = &load_expected("per_pin_slew")["base"];
    let netlist = "
module m (input clk, input a, input s, output q);
 wire n1, n2, n3;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 MUX2 x (.A0(n1), .A1(n1), .S(s), .Y(n2));
 BUF b (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_input_transition 0.4 [get_ports s]
";
    let sa = solve_single(netlist, sdc, lib_default());
    let c = &sa.corners["default"];
    let y_idx = c.node_map["x/Y"];
    for (tr, an, key) in [
        (Trans::Rise, Analysis::Late, "Y slew rise late"),
        (Trans::Fall, Analysis::Late, "Y slew fall late"),
        (Trans::Rise, Analysis::Early, "Y slew rise early"),
        (Trans::Fall, Analysis::Early, "Y slew fall early"),
    ] {
        assert_close(
            c.slews[y_idx][tr.idx()][an.idx()].unwrap(),
            exp[key].as_f64().unwrap(),
            key,
        );
    }
    check_pin_slacks(&sa, "default", "r2/D", exp);
}

#[test]
fn ka_05_slew_clamp() {
    let exp = &load_expected("slew_clamp")["base"];
    let netlist = "
module m (input clk, input a, output q);
 wire n1, n2;
 XTR x (.A(a), .Y(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r1 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.3 [get_ports a]
set_input_transition 2.0 [get_ports a]
";
    let sa = solve_single(netlist, sdc, lib_default());
    let c = &sa.corners["default"];
    let x_y = c.node_map["x/Y"];
    assert_close(
        c.slews[x_y][Trans::Rise.idx()][Analysis::Late.idx()].unwrap(),
        exp["XTR transition"].as_f64().unwrap(),
        "XTR transition clamped to 0",
    );
    check_pin_slacks(&sa, "default", "r1/D", exp);
}

#[test]
fn ka_06_driver_pin_cap() {
    let exp = &load_expected("driver_pin_cap")["base"];
    let netlist = "
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
    let sa = solve_single(netlist, sdc, lib_default());
    check_pin_slacks(&sa, "default", "r2/D", exp);
}

#[test]
fn ka_07_wireload_lumped() {
    let exp = &load_expected("wireload_lumped")["base"];
    let netlist = "
module m (input clk, input a, output y, output z);
 wire n1, n2, n3, w3, w4, w5;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 BUF b2 (.A(n2), .Y(n3));
 BUF x1 (.A(n2), .Y(y));
 DFF r2 (.D(n3), .CK(clk), .Q());
 BUF x2 (.A(n3), .Y(z));
 BUF x3 (.A(n3), .Y(w3));
 BUF x4 (.A(n3), .Y(w4));
 BUF x5 (.A(n3), .Y(w5));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_output_delay -clock clk 0.2 [get_ports {y z}]
set_load 0.004 [get_ports y]
";
    let wl_hdr = "  wire_load (\"wl_lin\") {\n    capacitance : 0.001;\n    slope : 2.0;\n    \
                  fanout_length (1, 2.0);\n    fanout_length (3, 5.0);\n  }\n  \
                  default_wire_load : \"wl_lin\";\n  default_wire_load_mode : top;\n";
    let sa = solve_single(netlist, sdc, build_synthetic_lib("lin_wl", 1.0, wl_hdr));
    check_pin_slacks(&sa, "default", "r2/D", exp);
    check_pin_slacks(&sa, "default", "y", exp);
}

#[test]
fn ka_08_const_tielo_and2() {
    let exp = &load_expected("const_tielo_and2")["base"];
    let netlist = "
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, n1, n2;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 AND2 g1 (.A(qa), .B(lo), .Y(n1));
 OR2 g2 (.A(n1), .B(qb), .Y(n2));
 DFF f1 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a b}]\n";
    let sa = solve_single(netlist, sdc, lib_default());
    check_pin_slacks(&sa, "default", "f1/D", exp);
}

#[test]
fn ka_09_const_xor_tiehi() {
    let exp = &load_expected("const_xor_tiehi")["base"];
    let netlist = "
module m (input clk, input c, output q);
 wire hi, qc, n1;
 TIEHI t1 (.Y(hi));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 XOR2 g (.A(qc), .B(hi), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports c]\n";
    let sa = solve_single(netlist, sdc, lib_default());
    check_pin_slacks(&sa, "default", "f/D", exp);
}

#[test]
fn ka_10_const_flop_d() {
    let exp = &load_expected("const_flop_d")["base"];
    let netlist = "
module m (input clk, input a, output y);
 wire lo, hi, qd, q2;
 TIELO t0 (.Y(lo));
 TIEHI t1 (.Y(hi));
 DFF f (.D(lo), .CK(clk), .Q(qd));
 BUF g (.A(qd), .Y(y));
 DFF f2 (.D(qd), .CK(hi), .Q(q2));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_output_delay -clock clk 0.2 [get_ports y]\n";
    let sa = solve_single(netlist, sdc, lib_default());
    assert_eq!(
        ep(&sa, "default", "f/D").status,
        EndpointStatus::NoDataArrival
    );
    assert_eq!(ep(&sa, "default", "f2/D").status, EndpointStatus::Unclocked);
    check_pin_slacks(&sa, "default", "y", exp);
}

#[test]
fn ka_11_const_mux_select() {
    let exp = &load_expected("const_mux_select")["base"];
    let netlist = "
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, nb, n1;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 INV i (.A(qb), .Y(nb));
 MUX2 g (.A0(qa), .A1(nb), .S(lo), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a b}]\n";
    let sa = solve_single(netlist, sdc, lib_default());
    check_pin_slacks(&sa, "default", "f/D", exp);
}

#[test]
fn ka_12_case_analysis() {
    let exp = load_expected("case_analysis");
    let netlist = "
module m (input clk, input a, input c, input s, output q);
 wire qa, qc, nc, n1;
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 INV i (.A(qc), .Y(nc));
 MUX2 g (.A0(qa), .A1(nc), .S(s), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
";
    let base_sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a c s}]\n";
    for (variant, extra) in [
        ("live", ""),
        ("case1", "set_case_analysis 1 [get_ports s]\n"),
        ("logic0", "set_logic_zero [get_ports s]\n"),
    ] {
        let sa = solve_single(netlist, &format!("{base_sdc}{extra}"), lib_default());
        check_pin_slacks(&sa, "default", "f/D", &exp[variant]);
    }
    let sa_dis = solve_single(
        netlist,
        &format!(
            "{base_sdc}set_case_analysis 1 [get_ports s]\nset_disable_timing -from A1 -to Y [get_cells g]\n"
        ),
        lib_default(),
    );
    assert_eq!(
        ep(&sa_dis, "default", "f/D").status,
        EndpointStatus::NoDataArrival
    );
}

#[test]
fn ka_13_io_delay_latency() {
    let exp = load_expected("io_delay_latency");
    let netlist = "
module m (input clk, input d, input e, input f, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(q));
 BUF b2 (.A(e), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(p));
 BUF b3 (.A(f), .Y(n3));
 DFF r3 (.D(n3), .CK(clk), .Q());
endmodule
";
    let sdc_base = "
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
    let sa_base = solve_single(netlist, sdc_base, lib_default());
    for pin in ["r1/D", "r2/D", "r3/D", "q", "p"] {
        check_pin_slacks(&sa_base, "default", pin, &exp["base"]);
    }

    let sdc_rf = "
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency -rise 0.3 [get_clocks clk]
set_clock_latency -fall 0.2 [get_clocks clk]
set_input_delay -clock clk -clock_fall 0.5 [get_ports d]
set_input_delay -clock clk 0.5 [get_ports e]
set_input_delay -clock clk 0.5 [get_ports f]
set_output_delay -clock clk -clock_fall 0.4 [get_ports q]
set_output_delay -clock clk 0.4 [get_ports p]
";
    let sa_rf = solve_single(netlist, sdc_rf, lib_default());
    for pin in ["r1/D", "r2/D", "q", "p"] {
        check_pin_slacks(&sa_rf, "default", pin, &exp["rf"]);
    }
}

#[test]
fn ka_14_one_sided_delays() {
    let exp = &load_expected("one_sided_delays")["base"];
    let netlist = "
module m (input clk, input d, input e, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(n2));
 BUF b2 (.A(n2), .Y(q));
 BUF b3 (.A(e), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(p));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk -max 0.5 [get_ports d]
set_input_delay -clock clk -min 0.1 [get_ports e]
set_output_delay -clock clk -min -0.1 [get_ports q]
set_output_delay -clock clk -max 0.4 [get_ports p]
";
    let sa = solve_single(netlist, sdc, lib_default());
    assert_close(
        ep(&sa, "default", "r1/D").setup_slack_ns.unwrap(),
        exp["r1/D setup"].as_f64().unwrap(),
        "r1/D setup",
    );
    assert_eq!(ep(&sa, "default", "r1/D").hold_slack_ns, None);
    assert_eq!(ep(&sa, "default", "r2/D").setup_slack_ns, None);
    assert_close(
        ep(&sa, "default", "r2/D").hold_slack_ns.unwrap(),
        exp["r2/D hold"].as_f64().unwrap(),
        "r2/D hold",
    );
    assert_eq!(ep(&sa, "default", "q").setup_slack_ns, None);
    assert_close(
        ep(&sa, "default", "q").hold_slack_ns.unwrap(),
        exp["q hold"].as_f64().unwrap(),
        "q hold",
    );
    assert_close(
        ep(&sa, "default", "p").setup_slack_ns.unwrap(),
        exp["p setup"].as_f64().unwrap(),
        "p setup",
    );
    assert_eq!(ep(&sa, "default", "p").hold_slack_ns, None);
}

#[test]
fn ka_15_two_clocks_hold() {
    let exp = load_expected("two_clocks_hold");
    let netlist = "
module m (input ca, input cb, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(ca), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(cb), .Q(q));
endmodule
";
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
        let sa = solve_single(netlist, sdc, lib_default());
        check_pin_slacks(&sa, "default", "r2/D", &exp[variant]);
    }
}

#[test]
fn ka_16_latch_open_uncertainty() {
    let exp = load_expected("latch_open_uncertainty");
    let netlist = "
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
    let base_sdc = "
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
";
    for (variant, extra) in [
        ("base", ""),
        ("mtb", "set_max_time_borrow 0.3 [get_cells l2]\n"),
    ] {
        let sa = solve_single(netlist, &format!("{base_sdc}{extra}"), lib_default());
        for pin in ["l1/D", "l2/D", "l3/D"] {
            check_pin_slacks(&sa, "default", pin, &exp[variant]);
        }
    }
}

#[test]
fn ka_17_latch_relaunch() {
    let exp = &load_expected("latch_relaunch")["base"];
    let netlist = "
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
    let sdc = "
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
";
    let sa = solve_single(netlist, sdc, lib_default());
    for pin in ["r2/D", "r4/D", "r6/D"] {
        check_pin_slacks(&sa, "default", pin, exp);
    }
}

#[test]
fn ka_18_recovery_removal() {
    let exp = &load_expected("recovery_removal")["base"];
    let netlist = "
module m (input clk, input rst, input d, output q);
 wire rq, r1, r2, rn;
 DFF r0 (.D(rst), .CK(clk), .Q(rq));
 BUF b1 (.A(rq), .Y(r1));
 BUF b2 (.A(r1), .Y(r2));
 BUF b3 (.A(r2), .Y(rn));
 DFFR f1 (.D(d), .CK(clk), .RN(rn), .Q(q));
endmodule
";
    let sdc = "
create_clock -name clk -period 1 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {rst d}]
set_output_delay -clock clk 0.3 [get_ports q]
";
    let sa = solve_single(netlist, sdc, lib_default());
    let rn = ep_group(&sa, "default", "f1/RN", "async_default");
    assert_close(
        rn.setup_slack_ns.unwrap(),
        exp["f1/RN recovery"].as_f64().unwrap(),
        "f1/RN recovery",
    );
    assert_close(
        rn.hold_slack_ns.unwrap(),
        exp["f1/RN removal"].as_f64().unwrap(),
        "f1/RN removal",
    );
    for pin in ["f1/D", "r0/D", "q"] {
        check_pin_slacks(&sa, "default", pin, exp);
    }
    assert_close(
        sa.corners["default"].metrics.wns_setup_ns,
        exp["setup WNS"].as_f64().unwrap(),
        "setup WNS",
    );
    assert_eq!(sa.corners["default"].paths[0].endpoint, "f1/RN");
}

#[test]
fn ka_19_clock_gating() {
    let exp = &load_expected("clock_gating")["base"];
    let netlist = "
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
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a d}]\n";
    let sa = solve_single(netlist, sdc, lib_default());
    for pin in ["c1/B", "c2/B", "c3/B", "c4/B", "r2/D"] {
        check_pin_slacks(&sa, "default", pin, exp);
    }
}

#[test]
fn ka_20_endpoint_status() {
    let exp = &load_expected("endpoint_status")["base"];
    let netlist = "
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
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a rn}]
set_output_delay -clock clk 0.3 [get_ports y]
";
    let sa = solve_single(netlist, sdc, lib_default());
    assert_eq!(ep(&sa, "default", "r3/D").status, EndpointStatus::Unclocked);
    assert_eq!(
        ep(&sa, "default", "r4/D").status,
        EndpointStatus::NoDataArrival
    );
    assert_eq!(
        ep(&sa, "default", "r5/D").status,
        EndpointStatus::NoDataArrival
    );
    assert_eq!(
        ep(&sa, "default", "z").status,
        EndpointStatus::UnconstrainedOutput
    );
    for (pin, grp, s_key, h_key) in [
        ("r1/D", "clk", "r1/D setup", "r1/D hold"),
        ("r2/D", "clk", "r2/D setup", "r2/D hold"),
        ("r6/D", "clk", "r6/D setup", "r6/D hold"),
        ("r6/RN", "async_default", "r6/RN recovery", "r6/RN removal"),
        ("y", "clk", "y setup", "y hold"),
    ] {
        let e = ep_group(&sa, "default", pin, grp);
        assert_eq!(e.status, EndpointStatus::Constrained);
        assert_close(
            e.setup_slack_ns.unwrap(),
            exp[s_key].as_f64().unwrap(),
            s_key,
        );
        assert_close(
            e.hold_slack_ns.unwrap(),
            exp[h_key].as_f64().unwrap(),
            h_key,
        );
    }
}

#[test]
fn ka_baseline_reconvergent_xor_and_falling_edge_capture() {
    let net_xor = "
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 XOR2 x1 (.A(n1), .B(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let sa_xor = solve_single(
        net_xor,
        "create_clock -name clk -period 2 [get_ports clk]\n",
        lib_default(),
    );
    assert_close(
        ep(&sa_xor, "default", "r2/D").setup_slack_ns.unwrap(),
        0.9175,
        "reconvergent_xor setup",
    );
    assert_close(
        ep(&sa_xor, "default", "r2/D").hold_slack_ns.unwrap(),
        0.638,
        "reconvergent_xor hold",
    );

    let net_fall = "
module m (input clk, input d, output q);
 wire n1, n2;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 DFFN r2 (.D(n2), .CKN(clk), .Q(q));
endmodule
";
    let sa_fall = solve_single(
        net_fall,
        "create_clock -name clk -period 2 -waveform {0 0.6} [get_ports clk]\n",
        lib_default(),
    );
    assert_close(
        ep(&sa_fall, "default", "r2/D").setup_slack_ns.unwrap(),
        -0.129,
        "falling_edge_capture setup",
    );
    assert_close(
        ep(&sa_fall, "default", "r2/D").hold_slack_ns.unwrap(),
        1.909,
        "falling_edge_capture hold",
    );
}

#[test]
fn metamorphic_properties_sta_req_0503() {
    let net1 = "
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let net2 = "
module m (input clk, input d, output q);
 wire n3, n2, n1;
 DFF r2 (.D(n3), .CK(clk), .Q(q));
 INV u2 (.A(n2), .Y(n3));
 INV u1 (.A(n1), .Y(n2));
 DFF r1 (.D(d), .CK(clk), .Q(n1));
endmodule
";
    let sdc_t2 = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.5 [get_ports d]\nset_output_delay -clock clk 0.3 [get_ports q]\n";
    let sa1 = solve_single(net1, sdc_t2, lib_default());
    let sa2 = solve_single(net2, sdc_t2, lib_default());
    assert_close(
        ep(&sa1, "default", "r2/D").setup_slack_ns.unwrap(),
        ep(&sa2, "default", "r2/D").setup_slack_ns.unwrap(),
        "permutation setup invariance",
    );
    assert_close(
        ep(&sa1, "default", "r2/D").hold_slack_ns.unwrap(),
        ep(&sa2, "default", "r2/D").hold_slack_ns.unwrap(),
        "permutation hold invariance",
    );

    let sdc_t35 = "create_clock -name clk -period 3.5 [get_ports clk]\nset_input_delay -clock clk 0.5 [get_ports d]\nset_output_delay -clock clk 0.3 [get_ports q]\n";
    let sa_t35 = solve_single(net1, sdc_t35, lib_default());
    assert_close(
        ep(&sa_t35, "default", "r2/D").setup_slack_ns.unwrap()
            - ep(&sa1, "default", "r2/D").setup_slack_ns.unwrap(),
        1.5,
        "period shift +1.5 ns setup slack delta",
    );
    assert_close(
        ep(&sa_t35, "default", "r2/D").hold_slack_ns.unwrap(),
        ep(&sa1, "default", "r2/D").hold_slack_ns.unwrap(),
        "period shift hold slack invariance",
    );

    let net_buf = "
module m (input clk, input d, output q);
 wire n1, n2, n3, n4;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 BUF b_extra (.A(n3), .Y(n4));
 DFF r2 (.D(n4), .CK(clk), .Q(q));
endmodule
";
    let sa_buf = solve_single(net_buf, sdc_t2, lib_default());
    assert!(
        ep(&sa_buf, "default", "r2/D").setup_slack_ns.unwrap()
            < ep(&sa1, "default", "r2/D").setup_slack_ns.unwrap()
    );
    assert!(
        ep(&sa_buf, "default", "r2/D").hold_slack_ns.unwrap()
            > ep(&sa1, "default", "r2/D").hold_slack_ns.unwrap()
    );
}
