// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Analytic exception derivations (`STA-REQ-0250` to `0270`, `STA-REQ-0501`), workspace revision
//! agreement (`STA-REQ-0500`, `0502`), and planted exception defects (`STA-REQ-0504`) for the
//! independent checker (`crates/verify/ruststa-check/src/rev`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rusteda_core::liberty::Library;
use rusteda_formats::liberty::read_text;
use ruststa_check::rev::{
    CheckKindEval, CornerAnalysis, DEFAULT_TOLERANCE_NS, EndpointStatus, MultiCornerAnalysis,
    analyze_inputs, build_timing_and_scope_json, check_against_reports, check_revision,
    create_timed_workspace,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch_ws_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "ruststa_check_exc_{}_{tag}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[derive(Debug, Clone, Copy)]
struct Lin(f64, f64, f64, f64, f64);

impl Lin {
    fn delay(self, s: f64, c: f64) -> f64 {
        self.0 + self.1 * s + self.2 * c
    }
    fn slew(self, c: f64) -> f64 {
        self.3 + self.4 * c
    }
}

const INV_R: Lin = Lin(0.10, 0.20, 2.0, 0.05, 1.5);
const INV_F: Lin = Lin(0.08, 0.15, 1.5, 0.04, 1.2);
const BUF_R: Lin = Lin(0.20, 0.10, 1.0, 0.06, 1.0);
const BUF_F: Lin = Lin(0.18, 0.10, 1.2, 0.05, 1.1);
const XOR_R: Lin = Lin(0.30, 0.25, 2.5, 0.07, 1.8);
const XOR_F: Lin = Lin(0.28, 0.20, 2.2, 0.06, 1.6);
const CKQ_R: Lin = Lin(0.40, 0.0, 1.0, 0.08, 1.0);
const CKQ_F: Lin = Lin(0.35, 0.0, 0.9, 0.07, 1.0);
const DQ_R: Lin = Lin(0.25, 0.10, 1.0, 0.06, 1.0);
const DQ_F: Lin = Lin(0.22, 0.10, 1.0, 0.05, 1.0);

fn table2(out: &mut String, kind: &str, v: impl Fn(f64, f64) -> f64) {
    let (v00, v01, v10, v11) = (v(0.0, 0.0), v(0.0, 1.0), v(1.0, 0.0), v(1.0, 1.0));
    let _ = writeln!(
        out,
        "        {kind} (t2) {{ values (\"{v00:.9}, {v01:.9}\", \"{v10:.9}, {v11:.9}\"); }}"
    );
}

fn arc(out: &mut String, rel: &str, sense: &str, ty: &str, r: Lin, f: Lin) {
    let _ = writeln!(
        out,
        "      timing () {{ related_pin : \"{rel}\"; timing_sense : {sense}; timing_type : {ty};"
    );
    table2(out, "cell_rise", |s, c| r.delay(s, c));
    table2(out, "rise_transition", |_, c| r.slew(c));
    table2(out, "cell_fall", |s, c| f.delay(s, c));
    table2(out, "fall_transition", |_, c| f.slew(c));
    let _ = writeln!(out, "      }}");
}

fn check_arc(out: &mut String, rel: &str, ty: &str, v: f64) {
    let _ = writeln!(
        out,
        "      timing () {{ related_pin : \"{rel}\"; timing_type : {ty}; \
         rise_constraint (scalar) {{ values (\"{v}\"); }} \
         fall_constraint (scalar) {{ values (\"{v}\"); }} }}"
    );
}

fn input_pin(out: &mut String, p: &str, clock: bool) {
    let ck = if clock { " clock : true;" } else { "" };
    let _ = writeln!(
        out,
        "    pin ({p}) {{ direction : input; capacitance : 0.01;{ck} }}"
    );
}

fn build_synthetic_lib_text() -> String {
    let mut o = String::from(
        "library (lin) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    let comb_cells = [
        ("INV", &["A"][..], "!A", "negative_unate", INV_R, INV_F),
        ("BUF", &["A"][..], "A", "positive_unate", BUF_R, BUF_F),
        (
            "AND2",
            &["A", "B"][..],
            "A&B",
            "positive_unate",
            BUF_R,
            BUF_F,
        ),
        ("XOR2", &["A", "B"][..], "A^B", "non_unate", XOR_R, XOR_F),
    ];
    for (cname, ins, func, sense, r, f) in comb_cells {
        let _ = writeln!(o, "  cell ({cname}) {{ area : 1;");
        for i in ins {
            input_pin(&mut o, i, false);
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{ direction : output; function : \"{func}\";"
        );
        for i in ins {
            arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }
    let seq_cells = [
        ("DFF", "CK", "rising", "CK"),
        ("DFFN", "CKN", "falling", "!CKN"),
    ];
    for (cname, ck, edge, ck_expr) in seq_cells {
        let _ = writeln!(
            o,
            "  cell ({cname}) {{ area : 4; ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"{ck_expr}\"; }} \
             pin (D) {{ direction : input; capacitance : 0.01;"
        );
        check_arc(&mut o, ck, &format!("setup_{edge}"), 0.10);
        check_arc(&mut o, ck, &format!("hold_{edge}"), 0.05);
        let _ = writeln!(o, "    }}");
        input_pin(&mut o, ck, true);
        let _ = writeln!(o, "    pin (Q) {{ direction : output; function : \"IQ\";");
        arc(
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
        "  cell (LAT) {{ area : 3; latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }} \
         pin (D) {{ direction : input; capacitance : 0.01;"
    );
    check_arc(&mut o, "G", "setup_falling", 0.10);
    check_arc(&mut o, "G", "hold_falling", 0.05);
    let _ = writeln!(o, "    }}");
    input_pin(&mut o, "G", true);
    let _ = writeln!(o, "    pin (Q) {{ direction : output; function : \"IQ\";");
    arc(&mut o, "D", "positive_unate", "combinational", DQ_R, DQ_F);
    arc(&mut o, "G", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}\n}}\n");
    o
}

fn lib_default() -> Library {
    read_text(&build_synthetic_lib_text(), "lin.lib").expect("valid synthetic library")
}

fn solve(netlist: &str, sdc: &str) -> MultiCornerAnalysis {
    analyze_inputs(
        &[("n.v", netlist)],
        None,
        &[("default", vec![lib_default()])],
        sdc,
    )
    .expect("checker analysis must succeed")
}

fn corner(sa: &MultiCornerAnalysis) -> &CornerAnalysis {
    &sa.corners["default"]
}

fn load_expected(case: &str) -> serde_json::Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta-exceptions")
        .join(case)
        .join("expected.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&text).expect("valid expected.json")
}

fn assert_opt_close(actual: Option<f64>, exp_val: &serde_json::Value, label: &str) {
    if exp_val.is_null() {
        assert!(actual.is_none(), "{label}: expected None, got {actual:?}");
    } else {
        let exp = exp_val.as_f64().unwrap();
        let act = actual.unwrap_or_else(|| panic!("{label}: expected {exp:.9}, got None"));
        let diff = (act - exp).abs();
        assert!(
            diff <= 1e-9,
            "{label}: actual {act:.9} != expected {exp:.9} (diff {diff:.3e})"
        );
    }
}

const C_RECONV: &str = "module m (input clk, input a, input b, output y);
  wire q1, q2, n1, n2, n3;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFF r2 (.D(b), .CK(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(n1), .Y(n2));
  AND2 g (.A(n2), .B(q2), .Y(n3)); DFF r3 (.D(n3), .CK(clk), .Q(y));
endmodule\n";
const SDC_RECONV_BASE: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const C_EDGES: &str = "module m (input clk, input a, input b, output y, output z);
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFFN r2 (.D(b), .CKN(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(q2), .Y(n2));
  DFFN r3 (.D(n1), .CKN(clk), .Q(y)); DFF r4 (.D(n2), .CK(clk), .Q(z));
endmodule\n";
const SDC_EDGES_BASE: &str = "create_clock -name clk -period 2 -waveform {0 0.8} [get_ports clk]\n";

const C_TWOCLK: &str = "module m (input ca, input cb, input a, input b, output y, output z);
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(ca), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2)); BUF b2 (.A(q2), .Y(n2)); DFF r4 (.D(n2), .CK(ca), .Q(z));
endmodule\n";
const SDC_TWOCLK41_BASE: &str = "create_clock -name A -period 4 [get_ports ca]\ncreate_clock -name B -period 1 [get_ports cb]\n";
const SDC_TWOCLK32_BASE: &str = "create_clock -name A -period 3 [get_ports ca]\ncreate_clock -name B -period 2 [get_ports cb]\n";

const C_GROUPS: &str =
    "module m (input ca, input cb, input cc, input a, input b, output y, output z, output w);
  wire q1, q2, q3, n1, n2, n3;
  DFF r1 (.D(a), .CK(ca), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2)); BUF b2 (.A(q2), .Y(n2)); DFF r4 (.D(n2), .CK(cc), .Q(z));
  DFF r5 (.D(n2), .CK(ca), .Q(q3)); BUF b3 (.A(q3), .Y(n3)); DFF r6 (.D(n3), .CK(cc), .Q(w));
endmodule\n";
const SDC_GROUPS_BASE: &str = "create_clock -name A -period 4 [get_ports ca]\n\
create_clock -name B -period 1 [get_ports cb]\ncreate_clock -name C -period 2 [get_ports cc]\n";

const C_PD: &str =
    "module m (input clk, input a, input in3, input in4, output out1, output out3, output out4);
  wire q1, n1, q2, n3, q3;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); BUF b1 (.A(q1), .Y(n1));
  DFF r2 (.D(n1), .CK(clk), .Q(q2)); BUF b2 (.A(q2), .Y(out1));
  BUF b3 (.A(in3), .Y(n3)); DFF r3 (.D(n3), .CK(clk), .Q(q3));
  BUF b4 (.A(q3), .Y(out4)); BUF b5 (.A(in4), .Y(out3));
endmodule\n";
const SDC_PD_BASE: &str = "create_clock -name clk -period 2 [get_ports clk]\n\
set_clock_latency 0.1 [get_clocks clk]\nset_clock_uncertainty -setup 0.05 [get_clocks clk]\n\
set_clock_uncertainty -hold 0.02 [get_clocks clk]\nset_output_delay 0.2 -clock clk [get_ports out1]\n";

const C_XBAR: &str = "module m (input clk, input a, input b, output y, output z);
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFF r2 (.D(b), .CK(clk), .Q(q2));
  AND2 g1 (.A(q1), .B(q2), .Y(n1)); XOR2 g2 (.A(q1), .B(q2), .Y(n2));
  DFF r3 (.D(n1), .CK(clk), .Q(y)); DFF r4 (.D(n2), .CK(clk), .Q(z));
endmodule\n";
const SDC_XBAR_BASE: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const C_LATCH: &str = "module m (input clk, input a, output y);
  wire q1, n1, n2, n3, n4;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(n1), .Y(n2));
  LAT l (.D(n2), .G(clk), .Q(n3)); INV u (.A(n3), .Y(n4)); DFF r2 (.D(n4), .CK(clk), .Q(y));
endmodule\n";
const SDC_LATCH_BASE: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const C_GATE: &str = "module m (input clk, input a, input en, output y);
  wire q1, e1, gck, q2, n1;
  DFF r1 (.D(en), .CK(clk), .Q(q1)); BUF be (.A(q1), .Y(e1));
  AND2 g (.A(clk), .B(e1), .Y(gck)); DFF r0 (.D(a), .CK(clk), .Q(q2));
  BUF bd (.A(q2), .Y(n1)); DFF r2 (.D(n1), .CK(gck), .Q(y));
endmodule\n";
const SDC_GATE_BASE: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

fn check_reconv_variant(case: &str, variant: &str, extra_sdc: &str, exp: &serde_json::Value) {
    let sa = solve(C_RECONV, &format!("{SDC_RECONV_BASE}{extra_sdc}\n"));
    let ca = corner(&sa);
    let (s1, h1) = ca.path_slack("r1/CK", "r3/D");
    let (s2, h2) = ca.path_slack("r2/CK", "r3/D");
    let v = &exp["expected"][variant];
    for (a, k) in [(s1, "s1"), (h1, "h1"), (s2, "s2"), (h2, "h2")] {
        assert_opt_close(a, &v[k], &format!("{case}/{variant}/{k}"));
    }
}

fn check_xbar_setup_variant(case: &str, variant: &str, extra_sdc: &str, exp: &serde_json::Value) {
    let sa = solve(C_XBAR, &format!("{SDC_XBAR_BASE}{extra_sdc}\n"));
    let ca = corner(&sa);
    let v = &exp["expected"][variant];
    let pairs = [
        ("r1/CK", "r3/D", "s13"),
        ("r1/CK", "r4/D", "s14"),
        ("r2/CK", "r3/D", "s23"),
        ("r2/CK", "r4/D", "s24"),
    ];
    for (sp, ep, k) in pairs {
        let (s, _) = ca.path_slack(sp, ep);
        assert_opt_close(s, &v[k], &format!("{case}/{variant}/{k}"));
    }
}

fn check_edges_variant(case: &str, variant: &str, extra_sdc: &str, exp: &serde_json::Value) {
    let sa = solve(C_EDGES, &format!("{SDC_EDGES_BASE}{extra_sdc}\n"));
    let ca = corner(&sa);
    let (s13, h13) = ca.path_slack("r1/CK", "r3/D");
    let (s24, h24) = ca.path_slack("r2/CKN", "r4/D");
    let v = &exp["expected"][variant];
    for (a, k) in [(s13, "s13"), (h13, "h13"), (s24, "s24"), (h24, "h24")] {
        assert_opt_close(a, &v[k], &format!("{case}/{variant}/{k}"));
    }
}

fn check_pd_variant(case: &str, variant: &str, extra_sdc: &str, exp: &serde_json::Value) {
    let sa = solve(C_PD, &format!("{SDC_PD_BASE}{extra_sdc}\n"));
    let ca = corner(&sa);
    let (rr, rrh) = ca.path_slack("r1/CK", "r2/D");
    let (ro, roh) = ca.path_slack("r2/CK", "out1");
    let ev = &exp["expected"][variant];
    for (a, k) in [(rr, "rr"), (rrh, "rrh"), (ro, "ro"), (roh, "roh")] {
        assert_opt_close(a, &ev[k], &format!("{case}/{variant}/{k}"));
    }
}

fn check_pd_pair(
    case: &str,
    v: &str,
    sdc: &str,
    sp: &str,
    ep: &str,
    ks: &str,
    kh: &str,
    exp: &serde_json::Value,
) {
    let sa = solve(C_PD, &format!("{SDC_PD_BASE}{sdc}\n"));
    let (s, h) = corner(&sa).path_slack(sp, ep);
    let ev = &exp["expected"][v];
    assert_opt_close(s, &ev[ks], &format!("{case}/{v}/{ks}"));
    assert_opt_close(h, &ev[kh], &format!("{case}/{v}/{kh}"));
}

#[test]
fn exc_01_analyses_sta_req_0250() {
    let exp = load_expected("analyses");
    let c = |v, sdc| check_reconv_variant("analyses", v, sdc, &exp);
    c("base", "");
    c("fp_setup", "set_false_path -setup -from [get_pins r1/CK]");
    c("fp_hold", "set_false_path -hold -from [get_pins r1/CK]");
    c(
        "mcp2_noflag",
        "set_multicycle_path 2 -from [get_pins r1/CK]",
    );
    c(
        "mcp2_both",
        "set_multicycle_path 2 -setup -hold -from [get_pins r1/CK]",
    );
    c(
        "mcp_h1",
        "set_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    c(
        "mcp0",
        "set_multicycle_path 0 -setup -from [get_pins r1/CK]",
    );
}

#[test]
fn exc_02_path_ends_sta_req_0251() {
    let exp = load_expected("path_ends");
    let c = |v, sdc| check_reconv_variant("path_ends", v, sdc, &exp);
    c("base", "");
    c("fp_from_pin", "set_false_path -from [get_pins r1/CK]");
    c("fp_from_q", "set_false_path -from [get_pins r1/Q]");
    c("fp_to_pin", "set_false_path -to [get_pins r3/D]");
    c("fp_to_internal", "set_false_path -to [get_pins g/Y]");
}

#[test]
fn exc_03_from_objects_sta_req_0252() {
    let exp = load_expected("from_objects");
    let c = |v, sdc| check_edges_variant("from_objects", v, sdc, &exp);
    c("edges_base", "");
    c(
        "rise_from_clk",
        "set_false_path -rise_from [get_clocks clk]",
    );
    c(
        "fall_from_clk",
        "set_false_path -fall_from [get_clocks clk]",
    );
    c(
        "rise_from_ckn",
        "set_false_path -rise_from [get_pins r2/CKN]",
    );
    c(
        "fall_from_ckn",
        "set_false_path -fall_from [get_pins r2/CKN]",
    );

    let sa = solve(
        C_RECONV,
        &format!("{SDC_RECONV_BASE}set_false_path -from [get_cells r1]\n"),
    );
    let ca = corner(&sa);
    assert_eq!(ca.path_slack("r1/CK", "r3/D"), (None, None));
    let (s2, h2) = ca.path_slack("r2/CK", "r3/D");
    assert_opt_close(s2, &serde_json::json!(1.271), "from_cell/s2");
    assert_opt_close(h2, &serde_json::json!(0.509), "from_cell/h2");
}

#[test]
fn exc_04_to_objects_sta_req_0253() {
    let exp = load_expected("to_objects");
    let cr = |v, sdc| check_reconv_variant("to_objects", v, sdc, &exp);
    cr("fp_rise_to", "set_false_path -rise_to [get_pins r3/D]");
    cr("fp_fall_to", "set_false_path -fall_to [get_pins r3/D]");
    cr(
        "fp_rise_flag",
        "set_false_path -rise -from [get_pins r1/CK]",
    );

    let ce = |v, sdc| check_edges_variant("to_objects", v, sdc, &exp);
    ce(
        "edges_rise_to_clk",
        "set_false_path -rise_to [get_clocks clk]",
    );
    ce(
        "edges_fall_to_clk",
        "set_false_path -fall_to [get_clocks clk]",
    );
    ce(
        "edges_mcp2_fall_to_clk",
        "set_multicycle_path 2 -setup -fall_to [get_clocks clk]",
    );
}

#[test]
fn exc_05_through_sta_req_0254() {
    let exp = load_expected("through");
    let c = |v, sdc| check_xbar_setup_variant("through", v, sdc, &exp);
    c("base", "");
    c("thr_pin", "set_false_path -through [get_pins g1/A]");
    c("thr_out", "set_false_path -through [get_pins g1/Y]");
    c("thr_net", "set_false_path -through [get_nets n1]");
    c("thr_cell", "set_false_path -through [get_cells g1]");
    c("thr_rise", "set_false_path -rise_through [get_pins g2/A]");
    c(
        "thr_order",
        "set_false_path -through [get_pins g1/A] -through [get_pins g1/Y]",
    );
    c(
        "thr_order_rev",
        "set_false_path -through [get_pins g1/Y] -through [get_pins g1/A]",
    );
}

#[test]
fn exc_06_matching_sta_req_0255() {
    let exp = load_expected("matching");
    let cx = |v, sdc| check_xbar_setup_variant("matching", v, sdc, &exp);
    cx(
        "xbar_fromto_and",
        "set_false_path -from [get_pins r1/CK] -to [get_pins r4/D]",
    );
    cx(
        "xbar_lists_or",
        "set_false_path -from [get_pins {r1/CK r2/CK}] -to [get_pins r4/D]",
    );
    cx(
        "xbar_thr_list",
        "set_false_path -through [get_pins {g1/A g2/B}]",
    );
    check_reconv_variant(
        "matching",
        "reconv_s_mixed",
        "set_multicycle_path 3 -setup -from [list [get_clocks clk] [get_pins r1/CK]]\n\
         set_multicycle_path 2 -setup -to [get_pins r3/D]",
        &exp,
    );
}

#[test]
fn exc_07_type_order_sta_req_0256() {
    let exp = load_expected("type_order");
    let c = |v, sdc| check_reconv_variant("type_order", v, sdc, &exp);
    c(
        "p_fp_over_maxd",
        "set_max_delay 1.5 -from [get_pins r1/CK] -to [get_pins r3/D]\nset_false_path -from [get_clocks clk]",
    );
    c(
        "p_maxd_over_mcp",
        "set_max_delay 1.5 -from [get_clocks clk]\nset_multicycle_path 2 -setup -from [get_pins r1/CK] -to [get_pins r3/D]",
    );
    c(
        "p_mind_over_mcp",
        "set_min_delay 0.8 -from [get_clocks clk]\nset_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    c(
        "p_fps_mcp",
        "set_false_path -setup -from [get_pins r1/CK]\nset_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
}

#[test]
fn exc_08_specificity_sta_req_0257() {
    let exp = load_expected("specificity");
    let cr = |v, sdc| check_reconv_variant("specificity", v, sdc, &exp);
    cr(
        "s_pin_vs_clk",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 3 -setup -from [get_clocks clk]",
    );
    cr(
        "s_from_vs_to",
        "set_multicycle_path 2 -setup -to [get_pins r3/D]\nset_multicycle_path 3 -setup -from [get_pins r1/CK]",
    );
    cr(
        "s_clkpin_vs_topin",
        "set_multicycle_path 3 -setup -from [get_clocks clk] -to [get_pins r3/D]\nset_multicycle_path 2 -setup -to [get_pins r3/D]",
    );
    cr(
        "s_topin_vs_clkclk",
        "set_multicycle_path 3 -setup -to [get_pins r3/D]\nset_multicycle_path 2 -setup -from [get_clocks clk] -to [get_clocks clk]",
    );
    cr(
        "s_fromclk_vs_toclk",
        "set_multicycle_path 3 -setup -to [get_clocks clk]\nset_multicycle_path 2 -setup -from [get_clocks clk]",
    );

    let cx = |v, sdc| check_xbar_setup_variant("specificity", v, sdc, &exp);
    cx(
        "xbar_thr_vs_fromclk",
        "set_multicycle_path 3 -setup -through [get_pins g1/A]\nset_multicycle_path 2 -setup -from [get_clocks clk] -to [get_clocks clk]",
    );
    cx(
        "xbar_thr_vs_topin",
        "set_multicycle_path 3 -setup -through [get_pins g1/A]\nset_multicycle_path 2 -setup -to [get_pins r3/D]",
    );
}

#[test]
fn exc_09_ties_sta_req_0258() {
    let exp = load_expected("ties");
    let c = |v, sdc| check_reconv_variant("ties", v, sdc, &exp);
    c(
        "o_specific_setup",
        "set_multicycle_path 3 -setup -from [get_pins r1/CK]\nset_multicycle_path 2 -from [get_pins r1/CK]",
    );
    c(
        "o_specific_hold",
        "set_multicycle_path 1 -hold -from [get_pins r1/CK]\nset_multicycle_path 3 -from [get_pins r1/CK]",
    );
    c(
        "o_later_wins",
        "set_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]\nset_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
    c(
        "o_later_wins_rev",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]",
    );
    c(
        "o_max_later",
        "set_max_delay 1.2 -from [get_pins {r1/CK r2/CK}]\nset_max_delay 1.5 -from [get_pins r1/CK]",
    );
    c(
        "o_hold_later",
        "set_multicycle_path 2 -hold -from [get_pins {r1/CK r2/CK}]\nset_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
}

#[test]
fn exc_10_false_path_sta_req_0259() {
    let exp = load_expected("false_path");
    let c = |v, sdc, st| {
        check_reconv_variant("false_path", v, sdc, &exp);
        let sa = solve(C_RECONV, &format!("{SDC_RECONV_BASE}{sdc}\n"));
        assert_eq!(corner(&sa).endpoint_status("r3/D"), Some(st));
    };
    c("base", "", EndpointStatus::Constrained);
    c(
        "fp_from_pin",
        "set_false_path -from [get_pins r1/CK]",
        EndpointStatus::Constrained,
    );
    c(
        "fp_from_to",
        "set_false_path -from [get_pins r1/CK] -to [get_pins r3/D]",
        EndpointStatus::Constrained,
    );
    c(
        "fp_to_pin",
        "set_false_path -to [get_pins r3/D]",
        EndpointStatus::FalsePathOnly,
    );
}

#[test]
fn exc_11_multicycle_setup_sta_req_0260() {
    let exp = load_expected("multicycle_setup");
    let c = |v: &str, base: &str, sdc: &str| {
        let sa = solve(C_TWOCLK, &format!("{base}{sdc}\n"));
        let ca = corner(&sa);
        let (ab, _) = ca.path_slack("r1/CK", "r2/D");
        let (ba, _) = ca.path_slack("r3/CK", "r4/D");
        assert_opt_close(
            ab,
            &exp["expected"][v]["ab"],
            &format!("multicycle_setup/{v}/ab"),
        );
        assert_opt_close(
            ba,
            &exp["expected"][v]["ba"],
            &format!("multicycle_setup/{v}/ba"),
        );
    };
    c(
        "twoclk41_ab_s3",
        SDC_TWOCLK41_BASE,
        "set_multicycle_path 3 -setup -from A -to B",
    );
    c(
        "twoclk41_ab_s3_start",
        SDC_TWOCLK41_BASE,
        "set_multicycle_path 3 -setup -start -from A -to B",
    );
    c(
        "twoclk41_ba_s3",
        SDC_TWOCLK41_BASE,
        "set_multicycle_path 3 -setup -from B -to A",
    );
    c(
        "twoclk41_ba_s3_start",
        SDC_TWOCLK41_BASE,
        "set_multicycle_path 3 -setup -start -from B -to A",
    );
    c(
        "twoclk32_ab_s3",
        SDC_TWOCLK32_BASE,
        "set_multicycle_path 3 -setup -from A -to B",
    );
    c(
        "twoclk32_ab_s3_start",
        SDC_TWOCLK32_BASE,
        "set_multicycle_path 3 -setup -start -from A -to B",
    );
    c(
        "twoclk32_ba_s3",
        SDC_TWOCLK32_BASE,
        "set_multicycle_path 3 -setup -from B -to A",
    );
    c(
        "twoclk32_ba_s3_start",
        SDC_TWOCLK32_BASE,
        "set_multicycle_path 3 -setup -start -from B -to A",
    );
}

#[test]
fn exc_12_multicycle_hold_sta_req_0261() {
    let exp = load_expected("multicycle_hold");
    let ct = |v: &str, sdc: &str| {
        let sa = solve(C_TWOCLK, &format!("{SDC_TWOCLK41_BASE}{sdc}\n"));
        let ca = corner(&sa);
        let (ab, abh) = ca.path_slack("r1/CK", "r2/D");
        let (ba, bah) = ca.path_slack("r3/CK", "r4/D");
        let ev = &exp["expected"][v];
        for (a, k) in [(ab, "ab"), (abh, "abh"), (ba, "ba"), (bah, "bah")] {
            assert_opt_close(a, &ev[k], &format!("multicycle_hold/{v}/{k}"));
        }
    };
    ct(
        "twoclk41_ab_h2",
        "set_multicycle_path 2 -hold -from A -to B",
    );
    ct(
        "twoclk41_ab_h2_end",
        "set_multicycle_path 2 -hold -end -from A -to B",
    );
    ct(
        "twoclk41_ab_s3_h2",
        "set_multicycle_path 3 -setup -from A -to B\nset_multicycle_path 2 -hold -from A -to B",
    );
    ct(
        "twoclk41_ab_s3start_h2end",
        "set_multicycle_path 3 -setup -start -from A -to B\nset_multicycle_path 2 -hold -end -from A -to B",
    );

    let cr = |v, sdc| check_reconv_variant("multicycle_hold", v, sdc, &exp);
    cr(
        "reconv_mcp2_h1",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    cr(
        "reconv_n_maxd_mcp",
        "set_max_delay 1.5 -from [get_pins r1/CK]\nset_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
    cr(
        "reconv_n_clk_setup_pin_hold",
        "set_multicycle_path 2 -setup -from [get_clocks clk]\nset_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
}

#[test]
fn exc_13_max_delay_sta_req_0262() {
    let exp = load_expected("max_delay");
    let c = |v, sdc| check_pd_variant("max_delay", v, sdc, &exp);
    c("pd_base", "");
    c(
        "pd_max_rr",
        "set_max_delay 1.0 -from [get_pins r1/CK] -to [get_pins r2/D]",
    );
    c(
        "pd_max_ro",
        "set_max_delay 1.0 -from [get_pins r2/CK] -to [get_ports out1]",
    );

    let sa = solve(
        C_EDGES,
        &format!("{SDC_EDGES_BASE}set_max_delay 1.0 -from [get_pins r2/CKN] -to [get_pins r4/D]\n"),
    );
    let (s24, _) = corner(&sa).path_slack("r2/CKN", "r4/D");
    assert_opt_close(s24, &serde_json::json!(0.271), "max_delay/edges_r2_r4");
}

#[test]
fn exc_14_min_delay_sta_req_0263() {
    let exp = load_expected("min_delay");
    let c = |v, sdc| check_pd_variant("min_delay", v, sdc, &exp);
    c("base", "");
    c(
        "min_rr",
        "set_min_delay 0.5 -from [get_pins r1/CK] -to [get_pins r2/D]",
    );
    c(
        "min_ro",
        "set_min_delay 0.5 -from [get_pins r2/CK] -to [get_ports out1]",
    );
}

#[test]
fn exc_15_unclocked_ends_sta_req_0264() {
    let exp = load_expected("unclocked_ends");
    let c = |v, sdc, sp, ep, ks, kh| check_pd_pair("unclocked_ends", v, sdc, sp, ep, ks, kh, &exp);
    c(
        "max_ur",
        "set_max_delay 1.0 -from in3 -to r3/D",
        "in3",
        "r3/D",
        "ur",
        "urh",
    );
    c(
        "min_ur",
        "set_min_delay 0.5 -from in3 -to r3/D",
        "in3",
        "r3/D",
        "ur",
        "urh",
    );
    c(
        "max_ru",
        "set_max_delay 1.0 -from r3/CK -to out4",
        "r3/CK",
        "out4",
        "ru",
        "ruh",
    );
    c(
        "min_ru",
        "set_min_delay 0.5 -from r3/CK -to out4",
        "r3/CK",
        "out4",
        "ru",
        "ruh",
    );
    c(
        "max_uu",
        "set_max_delay 1.0 -from in4 -to out3",
        "in4",
        "out3",
        "uu",
        "uuh",
    );
    c(
        "min_uu",
        "set_min_delay 0.5 -from in4 -to out3",
        "in4",
        "out3",
        "uu",
        "uuh",
    );
}

#[test]
fn exc_16_ignore_clock_latency_sta_req_0265() {
    let exp = load_expected("ignore_clock_latency");
    let c = |v, sdc, sp, ep, ks, kh| {
        check_pd_pair("ignore_clock_latency", v, sdc, sp, ep, ks, kh, &exp)
    };
    c(
        "maxi_rr",
        "set_max_delay 1.0 -ignore_clock_latency -from r1/CK -to r2/D",
        "r1/CK",
        "r2/D",
        "rr",
        "rrh",
    );
    c(
        "mini_rr",
        "set_min_delay 0.5 -ignore_clock_latency -from r1/CK -to r2/D",
        "r1/CK",
        "r2/D",
        "rr",
        "rrh",
    );
    c(
        "maxi_ro",
        "set_max_delay 1.0 -ignore_clock_latency -from r2/CK -to out1",
        "r2/CK",
        "out1",
        "ro",
        "roh",
    );
    c(
        "mini_ro",
        "set_min_delay 0.5 -ignore_clock_latency -from r2/CK -to out1",
        "r2/CK",
        "out1",
        "ro",
        "roh",
    );
    c(
        "maxi_ur",
        "set_max_delay 1.0 -ignore_clock_latency -from in3 -to r3/D",
        "in3",
        "r3/D",
        "ur",
        "urh",
    );
    c(
        "mini_ur",
        "set_min_delay 0.5 -ignore_clock_latency -from in3 -to r3/D",
        "in3",
        "r3/D",
        "ur",
        "urh",
    );
    c(
        "maxi_ru",
        "set_max_delay 1.0 -ignore_clock_latency -from r3/CK -to out4",
        "r3/CK",
        "out4",
        "ru",
        "ruh",
    );
    c(
        "mini_ru",
        "set_min_delay 0.5 -ignore_clock_latency -from r3/CK -to out4",
        "r3/CK",
        "out4",
        "ru",
        "ruh",
    );
}

#[test]
fn exc_17_clock_groups_sta_req_0266() {
    let exp = load_expected("clock_groups");
    let c = |v: &str, sdc: &str| {
        let sa = solve(C_GROUPS, &format!("{SDC_GROUPS_BASE}{sdc}\n"));
        let ca = corner(&sa);
        let (ab, abh) = ca.path_slack("r1/CK", "r2/D");
        let (bc, bch) = ca.path_slack("r3/CK", "r4/D");
        let (ba, bah) = ca.path_slack("r3/CK", "r5/D");
        let (ac, ach) = ca.path_slack("r5/CK", "r6/D");
        let ev = &exp["expected"][v];
        for (a, k) in [
            (ab, "ab"),
            (abh, "abh"),
            (bc, "bc"),
            (bch, "bch"),
            (ba, "ba"),
            (bah, "bah"),
            (ac, "ac"),
            (ach, "ach"),
        ] {
            assert_opt_close(a, &ev[k], &format!("clock_groups/{v}/{k}"));
        }
    };
    c("base", "");
    c(
        "async",
        "set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks {B C}]",
    );
    c(
        "one_group",
        "set_clock_groups -asynchronous -group [get_clocks A]",
    );
    c(
        "logical",
        "set_clock_groups -logically_exclusive -group [get_clocks A] -group [get_clocks B]",
    );
    c(
        "over_maxd",
        "set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks B]\nset_max_delay 0.8 -from [get_pins r1/CK] -to [get_pins r2/D]",
    );
    c(
        "allow_paths",
        "set_clock_groups -asynchronous -allow_paths -group [get_clocks A] -group [get_clocks B]",
    );
}

#[test]
fn exc_18_latch_sta_req_0267() {
    let exp = load_expected("latch");
    let c = |v: &str, sdc: &str| {
        let sa = solve(C_LATCH, &format!("{SDC_LATCH_BASE}{sdc}\n"));
        let ca = corner(&sa);
        let (ld_s, ld_h) = ca.endpoint_slack("l/D");
        let (r2_s, r2_h) = ca.endpoint_slack("r2/D");
        let ev = &exp["expected"][v];
        for (a, k) in [
            (ld_s, "ld_s"),
            (ld_h, "ld_h"),
            (r2_s, "r2_s"),
            (r2_h, "r2_h"),
        ] {
            assert_opt_close(a, &ev[k], &format!("latch/{v}/{k}"));
        }
    };
    c("base", "");
    c("fp_from_r1", "set_false_path -from [get_pins r1/CK]");
    c("fp_from_lg", "set_false_path -from [get_pins l/G]");
    c(
        "fp_r1_r2",
        "set_false_path -from [get_pins r1/CK] -to [get_pins r2/D]",
    );
    c(
        "mcp2_r1",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
    c(
        "maxd_r1_ld",
        "set_max_delay 0.5 -from [get_pins r1/CK] -to [get_pins l/D]",
    );
}

#[test]
fn exc_19_check_kinds_sta_req_0268() {
    let exp = load_expected("check_kinds");
    let c = |v: &str, sdc: &str| {
        let sa = solve(C_GATE, &format!("{SDC_GATE_BASE}{sdc}\n"));
        let ca = corner(&sa);
        let (sg, hg) = ca.endpoint_slack_kind("g/B", CheckKindEval::ClockGating);
        let (s02, h02) = ca.endpoint_slack("r2/D");
        let ev = &exp["expected"][v];
        for (a, k) in [(sg, "sg"), (hg, "hg"), (s02, "s02"), (h02, "h02")] {
            assert_opt_close(a, &ev[k], &format!("check_kinds/{v}/{k}"));
        }
    };
    c("gate_base", "");
    c("gate_fp_to_gB", "set_false_path -to [get_pins g/B]");
    c(
        "gate_mcp2_to_gB",
        "set_multicycle_path 2 -setup -to [get_pins g/B]",
    );
    c("gate_maxd_to_gB", "set_max_delay 0.5 -to [get_pins g/B]");

    let sa = solve(
        C_PD,
        &format!("{SDC_PD_BASE}set_multicycle_path 2 -setup -to [get_ports out1]\n"),
    );
    let (ro, roh) = corner(&sa).endpoint_slack("out1");
    let ev = &exp["expected"]["pd_mcp2_out1"];
    assert_opt_close(ro, &ev["ro"], "check_kinds/pd_mcp2_out1/ro");
    assert_opt_close(roh, &ev["roh"], "check_kinds/pd_mcp2_out1/roh");
}

#[test]
fn exc_20_path_semantics_sta_req_0269() {
    let exp = load_expected("path_semantics");
    let c = |v: &str, sdc: &str| {
        let sa = solve(C_RECONV, &format!("{SDC_RECONV_BASE}{sdc}\n"));
        let (ep_s, ep_h) = corner(&sa).endpoint_slack("r3/D");
        let ev = &exp["expected"][v];
        assert_opt_close(
            ep_s,
            &ev["ep_setup"],
            &format!("path_semantics/{v}/ep_setup"),
        );
        assert_opt_close(ep_h, &ev["ep_hold"], &format!("path_semantics/{v}/ep_hold"));
    };
    c("base", "");
    c(
        "mcp2_h1",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    c("fp_from_pin", "set_false_path -from [get_pins r1/CK]");
    c(
        "s_pin_vs_clk",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 3 -setup -from [get_clocks clk]",
    );
}

#[test]
fn exc_21_diagnostics_and_scope_sta_req_0270() {
    let exp = load_expected("diagnostics");
    assert_eq!(exp["requirement"], "STA-REQ-0270");

    let sa_q = solve(
        C_RECONV,
        &format!("{SDC_RECONV_BASE}set_false_path -from [get_pins r1/Q]\n"),
    );
    assert!(sa_q.diagnostics.iter().any(|d| d.code == "D0102"));
    assert!(sa_q.diagnostics.iter().any(|d| d.code == "D0110"));

    let sa_tie = solve(
        C_RECONV,
        &format!(
            "{SDC_RECONV_BASE}set_multicycle_path 3 -setup -from [get_pins {{r1/CK r2/CK}}]\n\
             set_multicycle_path 2 -setup -from [get_pins r1/CK]\n"
        ),
    );
    assert!(sa_tie.diagnostics.iter().any(|d| d.code == "D0111"));

    let sa_unused = solve(
        C_LATCH,
        &format!("{SDC_LATCH_BASE}set_false_path -from [get_pins r1/CK] -to [get_pins r2/D]\n"),
    );
    assert!(sa_unused.diagnostics.iter().any(|d| d.code == "D0112"));
    assert_eq!(corner(&sa_unused).scope.unused_exceptions, 1);

    let sa_both = solve(
        C_RECONV,
        &format!("{SDC_RECONV_BASE}set_multicycle_path 2 -setup -hold -from [get_pins r1/CK]\n"),
    );
    assert!(sa_both.diagnostics.iter().any(|d| d.code == "D0113"));

    let sa_allow = solve(
        C_GROUPS,
        &format!(
            "{SDC_GROUPS_BASE}set_clock_groups -asynchronous -allow_paths -group [get_clocks A] -group [get_clocks B]\n"
        ),
    );
    assert!(sa_allow.diagnostics.iter().any(|d| d.code == "D0114"));

    let sa_fp = solve(
        C_RECONV,
        &format!("{SDC_RECONV_BASE}set_false_path -to [get_pins r3/D]\n"),
    );
    let sc = &corner(&sa_fp).scope;
    assert_eq!(sc.exceptions.get("false_path"), Some(&1));
    assert_eq!(sc.unused_exceptions, 0);
    assert_eq!(sc.unconstrained_by_reason.get("false_path_only"), Some(&1));
}

#[test]
fn exc_22_planted_exception_defects_and_workspace_revision_sta_req_0500_0504() {
    let lib_text = build_synthetic_lib_text();
    let sdc_with_exc = format!(
        "{SDC_RECONV_BASE}set_false_path -from [get_pins r1/CK]\n\
         set_multicycle_path 2 -setup -from [get_pins r2/CK]\n\
         set_multicycle_path 1 -hold -from [get_pins r2/CK]\n"
    );
    let analysis = solve(C_RECONV, &sdc_with_exc);
    let (base_timing, base_scope) = build_timing_and_scope_json(&analysis);

    let ok = check_against_reports(
        &analysis,
        &base_timing,
        Some(&base_scope),
        DEFAULT_TOLERANCE_NS,
    );
    assert!(ok.verdict.is_agree(), "diags: {:?}", ok.diagnostics);

    let ws_path = scratch_ws_path("exc_agree");
    create_timed_workspace(
        &ws_path,
        C_RECONV,
        &[("default", "lin", &lib_text)],
        &sdc_with_exc,
        &base_timing,
        &base_scope,
    )
    .expect("create timed workspace");
    let ws_rep = check_revision(&ws_path, None, Some(DEFAULT_TOLERANCE_NS));
    let _ = std::fs::remove_dir_all(&ws_path);
    assert!(ws_rep.verdict.is_agree(), "diags: {:?}", ws_rep.diagnostics);

    let check_disagree =
        |ref_a: &MultiCornerAnalysis, t: &serde_json::Value, s: Option<&serde_json::Value>| {
            assert!(
                check_against_reports(ref_a, t, s, DEFAULT_TOLERANCE_NS)
                    .verdict
                    .is_disagree()
            );
        };

    // 1. Planted defect: false path not masked
    let (no_fp_t, no_fp_s) = build_timing_and_scope_json(&solve(C_RECONV, SDC_RECONV_BASE));
    check_disagree(&analysis, &no_fp_t, Some(&no_fp_s));

    // 2. Planted defect: wrong multicycle hold shift
    let bad_mcp = solve(
        C_RECONV,
        &format!(
            "{SDC_RECONV_BASE}set_false_path -from [get_pins r1/CK]\nset_multicycle_path 2 -setup -from [get_pins r2/CK]\n"
        ),
    );
    let (bad_mcp_t, bad_mcp_s) = build_timing_and_scope_json(&bad_mcp);
    check_disagree(&analysis, &bad_mcp_t, Some(&bad_mcp_s));

    // 3. Planted defect: wrong specificity / precedence (clock multicycle overriding pin multicycle)
    let spec_a = solve(
        C_RECONV,
        &format!(
            "{SDC_RECONV_BASE}set_multicycle_path 2 -setup -from [get_pins r1/CK]\nset_multicycle_path 3 -setup -from [get_clocks clk]\n"
        ),
    );
    let mut bad_spec_t = build_timing_and_scope_json(&spec_a).0;
    for ep in bad_spec_t["corners"]["default"]["endpoints"]
        .as_array_mut()
        .unwrap()
    {
        if ep["endpoint"] == "r3/D" {
            ep["setup_slack"] = serde_json::json!(5.271);
        }
    }
    check_disagree(&spec_a, &bad_spec_t, None);

    // 4. Planted defect: clock group not applied
    let cg_a = solve(
        C_GROUPS,
        &format!("{SDC_GROUPS_BASE}set_clock_groups -asynchronous -group A -group B\n"),
    );
    let (no_cg_t, no_cg_s) = build_timing_and_scope_json(&solve(C_GROUPS, SDC_GROUPS_BASE));
    check_disagree(&cg_a, &no_cg_t, Some(&no_cg_s));

    // 5. Planted defect: wrong `false_path_only` endpoint status in report
    let fp_all_a = solve(
        C_RECONV,
        &format!("{SDC_RECONV_BASE}set_false_path -to r3/D\n"),
    );
    let (mut fp_all_t, fp_all_s) = build_timing_and_scope_json(&fp_all_a);
    for ep in fp_all_t["corners"]["default"]["endpoints"]
        .as_array_mut()
        .unwrap()
    {
        if ep["endpoint"] == "r3/D" {
            ep["status"] = serde_json::json!("unclocked");
        }
    }
    check_disagree(&fp_all_a, &fp_all_t, Some(&fp_all_s));
}
