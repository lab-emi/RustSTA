// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for Phase 2a timing exceptions (`-from`/`-to`, `-rise_`/`-fall_`,
//! `-setup`/`-hold`, precedence and specificity, multicycle setup/hold shifts, max/min delay,
//! unclocked ends, `-ignore_clock_latency`, clock groups, latch borrowing under exceptions,
//! clock-gating checks, node state budget `T0060`, and `D0102`/`D0110`..`D0114` diagnostics).
//!
//! Covers all Phase 2a analytic derivations and `expected.json` files in
//! `verification/analytic/sta-exceptions/` to `1e-9 ns` tolerance plus the five acceptance
//! invariants of package `E8`.

use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::diag::Severity;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::prop::exceptions::through::ThroughPoints;
use ruststa::engine::prop::exceptions::{
    DEFAULT_NODE_EXCEPTION_STATE_BUDGET, EndpointStatus, ExceptionEvalOptions, ExceptionEvaluation,
    evaluate_exceptions,
};

const TOL: f64 = 1e-9;

type Coeff = (f64, f64, f64, f64, f64);
const INV_R: Coeff = (0.10, 0.20, 2.0, 0.05, 1.5);
const INV_F: Coeff = (0.08, 0.15, 1.5, 0.04, 1.2);
const BUF_R: Coeff = (0.20, 0.10, 1.0, 0.06, 1.0);
const BUF_F: Coeff = (0.18, 0.10, 1.2, 0.05, 1.1);
const XOR_R: Coeff = (0.30, 0.25, 2.5, 0.07, 1.8);
const XOR_F: Coeff = (0.28, 0.20, 2.2, 0.06, 1.6);
const CKQ_R: Coeff = (0.40, 0.00, 1.0, 0.08, 1.0);
const CKQ_F: Coeff = (0.35, 0.00, 0.9, 0.07, 1.0);
const DQ_R: Coeff = (0.25, 0.10, 1.0, 0.06, 1.0);
const DQ_F: Coeff = (0.22, 0.10, 1.0, 0.05, 1.0);
const CIN: f64 = 0.01;
const SETUP: f64 = 0.10;
const HOLD: f64 = 0.05;

fn analytic_expected(case: &str) -> serde_json::Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf();
    let p = root
        .join("verification/analytic/sta-exceptions")
        .join(case)
        .join("expected.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let v: serde_json::Value = serde_json::from_str(&text).expect("valid expected.json");
    assert_eq!(v["case"].as_str(), Some(case));
    v
}

fn table(out: &mut String, kind: &str, v00: f64, v01: f64, v10: f64, v11: f64) {
    let _ = writeln!(
        out,
        "        {kind} (t2) {{ values (\"{v00:.9}, {v01:.9}\", \"{v10:.9}, {v11:.9}\"); }}"
    );
}

fn arc(out: &mut String, rel: &str, sense: &str, ty: &str, r: Coeff, f: Coeff) {
    let _ = writeln!(
        out,
        "      timing () {{ related_pin : \"{rel}\"; timing_sense : {sense}; timing_type : {ty};"
    );
    table(out, "cell_rise", r.0, r.0 + r.2, r.0 + r.1, r.0 + r.1 + r.2);
    table(out, "rise_transition", r.3, r.3 + r.4, r.3, r.3 + r.4);
    table(out, "cell_fall", f.0, f.0 + f.2, f.0 + f.1, f.0 + f.1 + f.2);
    table(out, "fall_transition", f.3, f.3 + f.4, f.3, f.3 + f.4);
    let _ = writeln!(out, "      }}");
}

fn check_arc(out: &mut String, rel: &str, ty: &str, v: f64) {
    let _ = writeln!(
        out,
        "      timing () {{ related_pin : \"{rel}\"; timing_type : {ty};\n        \
         rise_constraint (scalar) {{ values (\"{v}\"); }}\n        \
         fall_constraint (scalar) {{ values (\"{v}\"); }} }}"
    );
}

fn input_pin(out: &mut String, p: &str, clock: bool) {
    let ck = if clock { " clock : true;" } else { "" };
    let _ = writeln!(
        out,
        "    pin ({p}) {{ direction : input; capacitance : {CIN};{ck} }}"
    );
}

fn linear_library() -> Library {
    let mut o = String::from(
        "library (lin) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    for (name, ins, func, sense, r, f) in [
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
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 1;");
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
        let _ = writeln!(o, "    pin (D) {{ direction : input; capacitance : {CIN};");
        check_arc(&mut o, ck, &format!("setup_{edge}"), SETUP);
        check_arc(&mut o, ck, &format!("hold_{edge}"), HOLD);
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
        "  cell (LAT) {{\n    area : 3;\n    latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }}\n    \
         pin (D) {{ direction : input; capacitance : {CIN};"
    );
    check_arc(&mut o, "G", "setup_falling", SETUP);
    check_arc(&mut o, "G", "hold_falling", HOLD);
    let _ = writeln!(o, "    }}");
    input_pin(&mut o, "G", true);
    let _ = writeln!(o, "    pin (Q) {{ direction : output; function : \"IQ\";");
    arc(&mut o, "D", "positive_unate", "combinational", DQ_R, DQ_F);
    arc(&mut o, "G", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}\n}}");
    read_text(&o, "lin.lib").expect("linear library")
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

const RECONV_V: &str =
    "module m (clk, a, b, y); input clk, a, b; output y; wire q1, q2, n1, n2, n3;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFF r2 (.D(b), .CK(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(n1), .Y(n2));
  AND2 g (.A(n2), .B(q2), .Y(n3)); DFF r3 (.D(n3), .CK(clk), .Q(y)); endmodule";
const RECONV_SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const EDGES_V: &str =
    "module m (clk, a, b, y, z); input clk, a, b; output y, z; wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFFN r2 (.D(b), .CKN(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(q2), .Y(n2));
  DFFN r3 (.D(n1), .CKN(clk), .Q(y)); DFF r4 (.D(n2), .CK(clk), .Q(z)); endmodule";
const EDGES_SDC: &str = "create_clock -name clk -period 2 -waveform {0 0.8} [get_ports clk]\n";

const TWOCLK_V: &str = "module m (ca, cb, a, b, y, z); input ca, cb, a, b; output y, z; wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(ca), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2)); BUF b2 (.A(q2), .Y(n2)); DFF r4 (.D(n2), .CK(ca), .Q(z)); endmodule";
const TWOCLK_41_SDC: &str = "create_clock -name A -period 4 [get_ports ca]\ncreate_clock -name B -period 1 [get_ports cb]\n";
const TWOCLK_32_SDC: &str = "create_clock -name A -period 3 [get_ports ca]\ncreate_clock -name B -period 2 [get_ports cb]\n";

const GROUPS_V: &str = "module m (ca, cb, cc, a, b, y, z, w); input ca, cb, cc, a, b; output y, z, w; wire q1, q2, q3, n1, n2, n3;
  DFF r1 (.D(a), .CK(ca), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2)); BUF b2 (.A(q2), .Y(n2)); DFF r4 (.D(n2), .CK(cc), .Q(z));
  DFF r5 (.D(n2), .CK(ca), .Q(q3)); BUF b3 (.A(q3), .Y(n3)); DFF r6 (.D(n3), .CK(cc), .Q(w)); endmodule";
const GROUPS_SDC: &str = "create_clock -name A -period 4 [get_ports ca]\ncreate_clock -name B -period 1 [get_ports cb]\ncreate_clock -name C -period 2 [get_ports cc]\n";

const PD_V: &str = "module m (clk, a, in3, in4, out1, out3, out4); input clk, a, in3, in4; output out1, out3, out4; wire q1, n1, q2, n3, q3;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); DFF r2 (.D(n1), .CK(clk), .Q(q2)); BUF b2 (.A(q2), .Y(out1));
  BUF b3 (.A(in3), .Y(n3)); DFF r3 (.D(n3), .CK(clk), .Q(q3)); BUF b4 (.A(q3), .Y(out4)); BUF b5 (.A(in4), .Y(out3)); endmodule";
const PD_SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\nset_clock_latency 0.1 [get_clocks clk]\nset_clock_uncertainty -setup 0.05 [get_clocks clk]\nset_clock_uncertainty -hold 0.02 [get_clocks clk]\nset_output_delay 0.2 -clock clk [get_ports out1]\n";

const XBAR_V: &str =
    "module m (clk, a, b, y, z); input clk, a, b; output y, z; wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); DFF r2 (.D(b), .CK(clk), .Q(q2));
  AND2 g1 (.A(q1), .B(q2), .Y(n1)); XOR2 g2 (.A(q1), .B(q2), .Y(n2));
  DFF r3 (.D(n1), .CK(clk), .Q(y)); DFF r4 (.D(n2), .CK(clk), .Q(z)); endmodule";
const XBAR_SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const LATCH_V: &str = "module m (clk, a, y); input clk, a; output y; wire q1, n1, n2, n3, n4;
  DFF r1 (.D(a), .CK(clk), .Q(q1)); BUF b1 (.A(q1), .Y(n1)); BUF b2 (.A(n1), .Y(n2));
  LAT l (.D(n2), .G(clk), .Q(n3)); INV u (.A(n3), .Y(n4)); DFF r2 (.D(n4), .CK(clk), .Q(y)); endmodule";
const LATCH_SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

const GATE_V: &str = "module m (clk, a, en, y); input clk, a, en; output y; wire q1, e1, gck, q2, n1;
  DFF r1 (.D(en), .CK(clk), .Q(q1)); BUF be (.A(q1), .Y(e1)); AND2 g (.A(clk), .B(e1), .Y(gck));
  DFF r0 (.D(a), .CK(clk), .Q(q2)); BUF bd (.A(q2), .Y(n1)); DFF r2 (.D(n1), .CK(gck), .Q(y)); endmodule";
const GATE_SDC: &str = "create_clock -name clk -period 2 [get_ports clk]\n";

fn eval_with_opts(
    verilog: &str,
    base_sdc: &str,
    extra_sdc: &str,
    opts: &ExceptionEvalOptions,
) -> Result<ExceptionEvaluation, rusteda_core::diag::Diag> {
    let lib = linear_library();
    let netlist = build_netlist(verilog, &lib);
    let tlib = TimingLib::from_corner("default", std::slice::from_ref(&lib)).expect("TimingLib");
    let sdc = format!("{base_sdc}\n{extra_sdc}\n");
    let libs = [lib];
    let ev = evaluate_exceptions(&netlist, &libs, &tlib, &sdc, "<sdc>", opts)?;
    if opts.max_states_per_node == DEFAULT_NODE_EXCEPTION_STATE_BUDGET {
        cross_check_analysis(&netlist, &libs, &sdc, &ev);
    }
    Ok(ev)
}

// STA-REQ-0271: the full analysis of `ruststa run` gives every endpoint of each case the slacks and
// the timed status of the reference evaluation, so the hand derivations hold for it too. Constructs
// it does not analyse yet are skipped by their diagnostic: `-ignore_clock_latency` (`T0071`),
// rise- or fall-specific exceptions through a latch (`T0073`) and unclocked ends (`T0072`).
fn cross_check_analysis(
    netlist: &FrozenNetlist<'_>,
    libs: &[Library],
    sdc: &str,
    ev: &ExceptionEvaluation,
) {
    let ea = match EngineAnalysis::analyze_single_corner(
        netlist,
        libs,
        sdc,
        &AnalysisOptions::ideal(),
    ) {
        Ok(ea) => ea,
        Err(diags) if diags.iter().any(|d| d.code == "T0071" || d.code == "T0073") => return,
        Err(diags) => panic!("full analysis rejected the case: {diags:?}"),
    };
    if ea.diags.iter().any(|d| d.code == "T0072") {
        return;
    }
    for e in &ev.endpoints {
        assert_opt_f64(
            ea.setup_slack_for(&e.pin),
            e.setup_slack,
            &format!("analysis {} setup", e.pin),
        );
        assert_opt_f64(
            ea.hold_slack_for(&e.pin),
            e.hold_slack,
            &format!("analysis {} hold", e.pin),
        );
    }
    for (node, rec) in &ea.default_corner().endpoints.endpoints {
        if rec.setup.is_some() || rec.hold.is_some() {
            let name = ea.netview.node_name_str(*node);
            assert!(
                ev.by_pin.contains_key(name.as_ref()),
                "the analysis times endpoint {name}, the reference evaluation does not"
            );
        }
    }
}

fn eval(verilog: &str, base_sdc: &str, extra_sdc: &str) -> ExceptionEvaluation {
    eval_with_opts(
        verilog,
        base_sdc,
        extra_sdc,
        &ExceptionEvalOptions::default(),
    )
    .expect("exception evaluation should succeed")
}

fn assert_opt_f64(actual: Option<f64>, expected: Option<f64>, ctx: &str) {
    match (actual, expected) {
        (Some(a), Some(e)) => assert!(
            (a - e).abs() <= TOL,
            "{ctx}: expected Some({e:.6}), got Some({a:.6})"
        ),
        (None, None) => {}
        _ => panic!("{ctx}: expected {expected:?}, got {actual:?}"),
    }
}

fn assert_slack(ev: &ExceptionEvaluation, pin: &str, s: Option<f64>, h: Option<f64>, ctx: &str) {
    let (actual_s, actual_h) = ev.slack(pin);
    assert_opt_f64(actual_s, s, &format!("{ctx} {pin} setup"));
    assert_opt_f64(actual_h, h, &format!("{ctx} {pin} hold"));
}

fn eval_reconv(
    extra_sdc: &str,
    s1: Option<f64>,
    h1: Option<f64>,
    s2: Option<f64>,
    h2: Option<f64>,
    ctx: &str,
) -> ExceptionEvaluation {
    let ev = eval(RECONV_V, RECONV_SDC, extra_sdc);
    let exp_s = s1.zip(s2).map(|(a, b)| a.min(b)).or(s1).or(s2);
    let exp_h = h1.zip(h2).map(|(a, b)| a.min(b)).or(h1).or(h2);
    assert_slack(&ev, "r3/D", exp_s, exp_h, &format!("{ctx} combined"));
    let ev1 = eval(
        RECONV_V,
        RECONV_SDC,
        &format!("{extra_sdc}\nset_false_path -from [get_pins r2/CK]"),
    );
    assert_slack(&ev1, "r3/D", s1, h1, &format!("{ctx} r1->r3"));
    let ev2 = eval(
        RECONV_V,
        RECONV_SDC,
        &format!("{extra_sdc}\nset_false_path -from [get_pins r1/CK]"),
    );
    assert_slack(&ev2, "r3/D", s2, h2, &format!("{ctx} r2->r3"));
    ev
}

fn chk_reconv(doc: &serde_json::Value, key: &str, extra_sdc: &str) -> ExceptionEvaluation {
    let row = &doc["expected"][key];
    eval_reconv(
        extra_sdc,
        row["s1"].as_f64(),
        row["h1"].as_f64(),
        row["s2"].as_f64(),
        row["h2"].as_f64(),
        key,
    )
}

#[test]
fn t2_analyses_derivation() {
    let doc = analytic_expected("analyses");
    chk_reconv(&doc, "base", "");
    chk_reconv(
        &doc,
        "fp_setup",
        "set_false_path -setup -from [get_pins r1/CK]",
    );
    chk_reconv(
        &doc,
        "fp_hold",
        "set_false_path -hold -from [get_pins r1/CK]",
    );
    let noflag = chk_reconv(
        &doc,
        "mcp2_noflag",
        "set_multicycle_path 2 -from [get_pins r1/CK]",
    );
    assert!(!noflag.has_diag("D0113"));
    let both = chk_reconv(
        &doc,
        "mcp2_both",
        "set_multicycle_path 2 -setup -hold -from [get_pins r1/CK]",
    );
    assert!(both.has_diag("D0113"));
    chk_reconv(
        &doc,
        "mcp_h1",
        "set_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    chk_reconv(
        &doc,
        "mcp0",
        "set_multicycle_path 0 -setup -from [get_pins r1/CK]",
    );
}

#[test]
fn t2_path_ends_derivation() {
    let doc = analytic_expected("path_ends");
    chk_reconv(&doc, "base", "");
    chk_reconv(&doc, "fp_from_pin", "set_false_path -from [get_pins r1/CK]");
    let fq = chk_reconv(&doc, "fp_from_q", "set_false_path -from [get_pins r1/Q]");
    assert!(fq.has_diag("D0102") && fq.has_diag("D0110"));
    let fto = chk_reconv(&doc, "fp_to_pin", "set_false_path -to [get_pins r3/D]");
    assert_eq!(fto.status("r3/D"), Some(EndpointStatus::FalsePathOnly));
    let fint = chk_reconv(&doc, "fp_to_internal", "set_false_path -to [get_pins g/Y]");
    assert!(fint.has_diag("D0102") && fint.has_diag("D0110"));
}

#[test]
fn t2_from_objects_derivation() {
    let doc = analytic_expected("from_objects");
    let fcell = "set_false_path -from [get_cells r1]";
    eval_reconv(
        fcell,
        None,
        None,
        Some(1.2710),
        Some(0.5090),
        "fp_from_cell",
    );
    eval_reconv(
        "set_false_path -from [get_clocks clk]",
        None,
        None,
        None,
        None,
        "fp_from_clk",
    );
    for (key, cmd) in [
        ("edges_base", ""),
        (
            "rise_from_clk",
            "set_false_path -rise_from [get_clocks clk]",
        ),
        (
            "fall_from_clk",
            "set_false_path -fall_from [get_clocks clk]",
        ),
        (
            "rise_from_ckn",
            "set_false_path -rise_from [get_pins r2/CKN]",
        ),
        (
            "fall_from_ckn",
            "set_false_path -fall_from [get_pins r2/CKN]",
        ),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(EDGES_V, EDGES_SDC, cmd);
        assert_slack(&ev, "r3/D", row["s13"].as_f64(), row["h13"].as_f64(), key);
        assert_slack(&ev, "r4/D", row["s24"].as_f64(), row["h24"].as_f64(), key);
        if key == "rise_from_ckn" {
            assert!(ev.has_diag("D0112"));
        }
    }
}

#[test]
fn t2_to_objects_derivation() {
    let doc = analytic_expected("to_objects");
    eval_reconv(
        "set_false_path -to [get_cells r3]",
        None,
        None,
        None,
        None,
        "fp_to_cell",
    );
    chk_reconv(
        &doc,
        "fp_rise_to",
        "set_false_path -rise_to [get_pins r3/D]",
    );
    chk_reconv(
        &doc,
        "fp_fall_to",
        "set_false_path -fall_to [get_pins r3/D]",
    );
    chk_reconv(
        &doc,
        "fp_rise_flag",
        "set_false_path -rise -from [get_pins r1/CK]",
    );
    for (key, cmd) in [
        (
            "edges_rise_to_clk",
            "set_false_path -rise_to [get_clocks clk]",
        ),
        (
            "edges_fall_to_clk",
            "set_false_path -fall_to [get_clocks clk]",
        ),
        (
            "edges_mcp2_fall_to_clk",
            "set_multicycle_path 2 -setup -fall_to [get_clocks clk]",
        ),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(EDGES_V, EDGES_SDC, cmd);
        assert_slack(&ev, "r3/D", row["s13"].as_f64(), row["h13"].as_f64(), key);
        assert_slack(&ev, "r4/D", row["s24"].as_f64(), row["h24"].as_f64(), key);
    }
}

#[test]
fn t2_through_phase2b_stub_and_matching_derivation() {
    assert_eq!(ThroughPoints::new().unwrap_err().planned, "E13");
    let _thr_doc = analytic_expected("through");
    let doc = analytic_expected("matching");
    for (key, extra) in [
        (
            "xbar_fromto_and",
            "set_false_path -from [get_pins r1/CK] -to [get_pins r4/D]",
        ),
        (
            "xbar_lists_or",
            "set_false_path -from [get_pins {r1/CK r2/CK}] -to [get_pins r4/D]",
        ),
    ] {
        let row = &doc["expected"][key];
        let r1 = eval(
            XBAR_V,
            XBAR_SDC,
            &format!("{extra}\nset_false_path -from [get_pins r2/CK]"),
        );
        let r2 = eval(
            XBAR_V,
            XBAR_SDC,
            &format!("{extra}\nset_false_path -from [get_pins r1/CK]"),
        );
        assert_opt_f64(r1.slack("r3/D").0, row["s13"].as_f64(), key);
        assert_opt_f64(r1.slack("r4/D").0, row["s14"].as_f64(), key);
        assert_opt_f64(r2.slack("r3/D").0, row["s23"].as_f64(), key);
        assert_opt_f64(r2.slack("r4/D").0, row["s24"].as_f64(), key);
    }
    chk_reconv(
        &doc,
        "reconv_s_mixed",
        "set_multicycle_path 3 -setup -from [list [get_clocks clk] [get_pins r1/CK]]\n\
         set_multicycle_path 2 -setup -to [get_pins r3/D]",
    );
}

#[test]
fn t2_type_order_derivation() {
    let doc = analytic_expected("type_order");
    chk_reconv(
        &doc,
        "p_fp_over_maxd",
        "set_max_delay 1.5 -from [get_pins r1/CK] -to [get_pins r3/D]\n\
         set_false_path -from [get_clocks clk]",
    );
    chk_reconv(
        &doc,
        "p_maxd_over_mcp",
        "set_max_delay 1.5 -from [get_clocks clk]\n\
         set_multicycle_path 2 -setup -from [get_pins r1/CK] -to [get_pins r3/D]",
    );
    chk_reconv(
        &doc,
        "p_mind_over_mcp",
        "set_min_delay 0.8 -from [get_clocks clk]\n\
         set_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    chk_reconv(
        &doc,
        "p_fps_mcp",
        "set_false_path -setup -from [get_pins r1/CK]\n\
         set_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
}

#[test]
fn t2_specificity_derivation() {
    let doc = analytic_expected("specificity");
    chk_reconv(
        &doc,
        "s_pin_vs_clk",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
         set_multicycle_path 3 -setup -from [get_clocks clk]",
    );
    chk_reconv(
        &doc,
        "s_from_vs_to",
        "set_multicycle_path 2 -setup -to [get_pins r3/D]\n\
         set_multicycle_path 3 -setup -from [get_pins r1/CK]",
    );
    chk_reconv(
        &doc,
        "s_clkpin_vs_topin",
        "set_multicycle_path 3 -setup -from [get_clocks clk] -to [get_pins r3/D]\n\
         set_multicycle_path 2 -setup -to [get_pins r3/D]",
    );
    chk_reconv(
        &doc,
        "s_topin_vs_clkclk",
        "set_multicycle_path 3 -setup -to [get_pins r3/D]\n\
         set_multicycle_path 2 -setup -from [get_clocks clk] -to [get_clocks clk]",
    );
    chk_reconv(
        &doc,
        "s_fromclk_vs_toclk",
        "set_multicycle_path 3 -setup -to [get_clocks clk]\n\
         set_multicycle_path 2 -setup -from [get_clocks clk]",
    );
}

#[test]
fn t2_ties_derivation() {
    let doc = analytic_expected("ties");
    let s_spec = chk_reconv(
        &doc,
        "o_specific_setup",
        "set_multicycle_path 3 -setup -from [get_pins r1/CK]\n\
         set_multicycle_path 2 -from [get_pins r1/CK]",
    );
    assert!(!s_spec.has_diag("D0111"));
    let h_spec = chk_reconv(
        &doc,
        "o_specific_hold",
        "set_multicycle_path 1 -hold -from [get_pins r1/CK]\n\
         set_multicycle_path 3 -from [get_pins r1/CK]",
    );
    assert!(!h_spec.has_diag("D0111"));
    for (key, c1, c2) in [
        (
            "o_later_wins",
            "set_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]",
            "set_multicycle_path 2 -setup -from [get_pins r1/CK]",
        ),
        (
            "o_later_wins_rev",
            "set_multicycle_path 2 -setup -from [get_pins r1/CK]",
            "set_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]",
        ),
        (
            "o_max_later",
            "set_max_delay 1.2 -from [get_pins {r1/CK r2/CK}]",
            "set_max_delay 1.5 -from [get_pins r1/CK]",
        ),
        (
            "o_hold_later",
            "set_multicycle_path 2 -hold -from [get_pins {r1/CK r2/CK}]",
            "set_multicycle_path 1 -hold -from [get_pins r1/CK]",
        ),
    ] {
        let ev = chk_reconv(&doc, key, &format!("{c1}\n{c2}"));
        assert!(ev.has_diag("D0111"), "{key} should emit D0111");
    }
}

#[test]
fn t2_false_path_derivation() {
    let doc = analytic_expected("false_path");
    chk_reconv(&doc, "base", "");
    chk_reconv(&doc, "fp_from_pin", "set_false_path -from [get_pins r1/CK]");
    let fto_from = chk_reconv(
        &doc,
        "fp_from_to",
        "set_false_path -from [get_pins r1/CK] -to [get_pins r3/D]",
    );
    assert_eq!(fto_from.status("r3/D"), Some(EndpointStatus::Constrained));
    let fto = chk_reconv(&doc, "fp_to_pin", "set_false_path -to [get_pins r3/D]");
    assert_eq!(fto.status("r3/D"), Some(EndpointStatus::FalsePathOnly));
    assert_eq!(fto.scope.false_path_only_endpoints, 1);
}

#[test]
fn t2_multicycle_setup_derivation() {
    let doc = analytic_expected("multicycle_setup");
    for (prefix, base) in [("twoclk41", TWOCLK_41_SDC), ("twoclk32", TWOCLK_32_SDC)] {
        for (dir, c_from, c_to) in [("ab", "A", "B"), ("ba", "B", "A")] {
            for (suf, flag) in [("_s3", ""), ("_s3_start", " -start")] {
                let key = format!("{prefix}_{dir}{suf}");
                let cmd = format!(
                    "set_multicycle_path 3 -setup{flag} -from [get_clocks {c_from}] -to [get_clocks {c_to}]"
                );
                let row = &doc["expected"][&key];
                let ev = eval(TWOCLK_V, base, &cmd);
                assert_opt_f64(ev.slack("r2/D").0, row["ab"].as_f64(), &key);
                assert_opt_f64(ev.slack("r4/D").0, row["ba"].as_f64(), &key);
            }
        }
    }
}

#[test]
fn t2_multicycle_hold_derivation() {
    let doc = analytic_expected("multicycle_hold");
    let ab = "-from [get_clocks A] -to [get_clocks B]";
    for (key, cmd) in [
        (
            "twoclk41_ab_h2",
            format!("set_multicycle_path 2 -hold {ab}"),
        ),
        (
            "twoclk41_ab_h2_end",
            format!("set_multicycle_path 2 -hold -end {ab}"),
        ),
        (
            "twoclk41_ab_s3_h2",
            format!("set_multicycle_path 3 -setup {ab}\nset_multicycle_path 2 -hold {ab}"),
        ),
        (
            "twoclk41_ab_s3start_h2end",
            format!(
                "set_multicycle_path 3 -setup -start {ab}\nset_multicycle_path 2 -hold -end {ab}"
            ),
        ),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(TWOCLK_V, TWOCLK_41_SDC, &cmd);
        assert_slack(&ev, "r2/D", row["ab"].as_f64(), row["abh"].as_f64(), key);
        assert_slack(&ev, "r4/D", row["ba"].as_f64(), row["bah"].as_f64(), key);
    }
    chk_reconv(
        &doc,
        "reconv_mcp2_h1",
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
         set_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
    let n_maxd = chk_reconv(
        &doc,
        "reconv_n_maxd_mcp",
        "set_max_delay 1.5 -from [get_pins r1/CK]\n\
         set_multicycle_path 2 -setup -from [get_pins r1/CK]",
    );
    assert_eq!(n_maxd.scope.unused, 0);
    chk_reconv(
        &doc,
        "reconv_n_clk_setup_pin_hold",
        "set_multicycle_path 2 -setup -from [get_clocks clk]\n\
         set_multicycle_path 1 -hold -from [get_pins r1/CK]",
    );
}

#[test]
fn t2_max_delay_and_min_delay_derivations() {
    let max_doc = analytic_expected("max_delay");
    for (key, cmd) in [
        ("pd_base", ""),
        (
            "pd_max_rr",
            "set_max_delay 1.0 -from [get_pins r1/CK] -to [get_pins r2/D]",
        ),
        (
            "pd_max_ro",
            "set_max_delay 1.0 -from [get_pins r2/CK] -to [get_ports out1]",
        ),
    ] {
        let row = &max_doc["expected"][key];
        let ev = eval(PD_V, PD_SDC, cmd);
        assert_slack(&ev, "r2/D", row["rr"].as_f64(), row["rrh"].as_f64(), key);
        assert_slack(&ev, "out1", row["ro"].as_f64(), row["roh"].as_f64(), key);
    }
    let max_e = eval(
        EDGES_V,
        EDGES_SDC,
        "set_max_delay 1.0 -from [get_pins r2/CKN] -to [get_pins r4/D]",
    );
    assert_opt_f64(max_e.slack("r4/D").0, Some(0.2710), "max_edges r4/D setup");

    let min_doc = analytic_expected("min_delay");
    for (key, cmd) in [
        ("base", ""),
        (
            "min_rr",
            "set_min_delay 0.5 -from [get_pins r1/CK] -to [get_pins r2/D]",
        ),
        (
            "min_ro",
            "set_min_delay 0.5 -from [get_pins r2/CK] -to [get_ports out1]",
        ),
    ] {
        let row = &min_doc["expected"][key];
        let ev = eval(PD_V, PD_SDC, cmd);
        assert_slack(&ev, "r2/D", row["rr"].as_f64(), row["rrh"].as_f64(), key);
        assert_slack(&ev, "out1", row["ro"].as_f64(), row["roh"].as_f64(), key);
    }
}

#[test]
fn t2_unclocked_ends_and_ignore_clock_latency_derivations() {
    let unc_doc = analytic_expected("unclocked_ends");
    for (suf, from, to, pin) in [
        ("ur", "[get_ports in3]", "[get_pins r3/D]", "r3/D"),
        ("ru", "[get_pins r3/CK]", "[get_ports out4]", "out4"),
        ("uu", "[get_ports in4]", "[get_ports out3]", "out3"),
    ] {
        for (pre, kw, val) in [
            ("max", "set_max_delay", "1.0"),
            ("min", "set_min_delay", "0.5"),
        ] {
            let key = format!("{pre}_{suf}");
            let cmd = format!("{kw} {val} -from {from} -to {to}");
            let row = &unc_doc["expected"][&key];
            let ev = eval(PD_V, PD_SDC, &cmd);
            assert_slack(
                &ev,
                pin,
                row[suf].as_f64(),
                row[&format!("{suf}h")].as_f64(),
                &key,
            );
        }
    }

    let icl_doc = analytic_expected("ignore_clock_latency");
    for (suf, from, to, pin) in [
        ("rr", "[get_pins r1/CK]", "[get_pins r2/D]", "r2/D"),
        ("ro", "[get_pins r2/CK]", "[get_ports out1]", "out1"),
        ("ur", "[get_ports in3]", "[get_pins r3/D]", "r3/D"),
        ("ru", "[get_pins r3/CK]", "[get_ports out4]", "out4"),
    ] {
        for (pre, kw, val) in [
            ("maxi", "set_max_delay", "1.0"),
            ("mini", "set_min_delay", "0.5"),
        ] {
            let key = format!("{pre}_{suf}");
            let cmd = format!("{kw} {val} -ignore_clock_latency -from {from} -to {to}");
            let row = &icl_doc["expected"][&key];
            let ev = eval(PD_V, PD_SDC, &cmd);
            assert_slack(
                &ev,
                pin,
                row[suf].as_f64(),
                row[&format!("{suf}h")].as_f64(),
                &key,
            );
        }
    }
}

#[test]
fn t2_clock_groups_derivation() {
    let doc = analytic_expected("clock_groups");
    let g_ab = "-group [get_clocks A] -group [get_clocks B]";
    for (key, cmd) in [
        ("base", String::new()),
        (
            "async",
            "set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks {B C}]".into(),
        ),
        (
            "one_group",
            "set_clock_groups -asynchronous -group [get_clocks A]".into(),
        ),
        (
            "logical",
            format!("set_clock_groups -logically_exclusive {g_ab}"),
        ),
        (
            "over_maxd",
            format!(
                "set_clock_groups -asynchronous {g_ab}\nset_max_delay 0.8 -from [get_pins r1/CK] -to [get_pins r2/D]"
            ),
        ),
        (
            "allow_paths",
            format!("set_clock_groups -asynchronous -allow_paths {g_ab}"),
        ),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(GROUPS_V, GROUPS_SDC, &cmd);
        assert_slack(&ev, "r2/D", row["ab"].as_f64(), row["abh"].as_f64(), key);
        assert_slack(&ev, "r4/D", row["bc"].as_f64(), row["bch"].as_f64(), key);
        assert_slack(&ev, "r5/D", row["ba"].as_f64(), row["bah"].as_f64(), key);
        assert_slack(&ev, "r6/D", row["ac"].as_f64(), row["ach"].as_f64(), key);
        if key == "allow_paths" {
            assert!(ev.has_diag("D0114"));
        }
    }
}

#[test]
fn t2_latch_derivation() {
    let doc = analytic_expected("latch");
    for (key, cmd) in [
        ("base", ""),
        ("fp_from_r1", "set_false_path -from [get_pins r1/CK]"),
        ("fp_from_r1", "set_false_path -to [get_pins l/D]"),
        ("fp_from_lg", "set_false_path -from [get_pins l/G]"),
        ("fp_from_lg", "set_false_path -from [get_cells l]"),
        (
            "fp_r1_r2",
            "set_false_path -from [get_pins r1/CK] -to [get_pins r2/D]",
        ),
        (
            "mcp2_r1",
            "set_multicycle_path 2 -setup -from [get_pins r1/CK]",
        ),
        ("mcp2_r1", "set_multicycle_path 2 -setup -to [get_pins l/D]"),
        (
            "maxd_r1_ld",
            "set_max_delay 0.5 -from [get_pins r1/CK] -to [get_pins l/D]",
        ),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(LATCH_V, LATCH_SDC, cmd);
        assert_slack(&ev, "l/D", row["ld_s"].as_f64(), row["ld_h"].as_f64(), cmd);
        assert_slack(&ev, "r2/D", row["r2_s"].as_f64(), row["r2_h"].as_f64(), cmd);
        if key == "fp_r1_r2" {
            assert!(ev.has_diag("D0112"));
        }
    }
}

#[test]
fn t2_check_kinds_derivation() {
    let doc = analytic_expected("check_kinds");
    for (key, cmd) in [
        ("gate_base", ""),
        ("gate_fp_to_gB", "set_false_path -to [get_pins g/B]"),
        ("gate_fp_to_gB", "set_false_path -to [get_cells g]"),
        (
            "gate_mcp2_to_gB",
            "set_multicycle_path 2 -setup -to [get_pins g/B]",
        ),
        ("gate_maxd_to_gB", "set_max_delay 0.5 -to [get_pins g/B]"),
    ] {
        let row = &doc["expected"][key];
        let ev = eval(GATE_V, GATE_SDC, cmd);
        assert_slack(&ev, "g/B", row["sg"].as_f64(), row["hg"].as_f64(), cmd);
        assert_slack(&ev, "r2/D", row["s02"].as_f64(), row["h02"].as_f64(), cmd);
    }
    let row = &doc["expected"]["pd_mcp2_out1"];
    let ev = eval(
        PD_V,
        PD_SDC,
        "set_multicycle_path 2 -setup -to [get_ports out1]",
    );
    assert_slack(
        &ev,
        "out1",
        row["ro"].as_f64(),
        row["roh"].as_f64(),
        "pd_mcp2_out1",
    );
}

#[test]
fn t2_path_semantics_and_diagnostics_derivations() {
    let ps_doc = analytic_expected("path_semantics");
    let _diag_doc = analytic_expected("diagnostics");
    for (key, cmd) in [
        ("base", ""),
        (
            "mcp2_h1",
            "set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
             set_multicycle_path 1 -hold -from [get_pins r1/CK]",
        ),
        ("fp_from_pin", "set_false_path -from [get_pins r1/CK]"),
        (
            "s_pin_vs_clk",
            "set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
             set_multicycle_path 3 -setup -from [get_clocks clk]",
        ),
    ] {
        let row = &ps_doc["expected"][key];
        let ev = eval(RECONV_V, RECONV_SDC, cmd);
        assert_slack(
            &ev,
            "r3/D",
            row["ep_setup"].as_f64(),
            row["ep_hold"].as_f64(),
            key,
        );
    }

    let scope_ev = eval(
        RECONV_V,
        RECONV_SDC,
        "set_false_path -to [get_pins r3/D]\n\
         set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
         set_max_delay 1.5 -from [get_pins r2/CK]\n\
         set_min_delay 0.2 -from [get_pins r2/CK]\n\
         set_clock_groups -asynchronous -allow_paths -group [get_clocks clk]",
    );
    assert_eq!(scope_ev.scope.false_path, 1);
    assert_eq!(scope_ev.scope.multicycle_path, 1);
    assert_eq!(scope_ev.scope.max_delay, 1);
    assert_eq!(scope_ev.scope.min_delay, 1);
    assert_eq!(scope_ev.scope.clock_groups, 1);
    assert_eq!(scope_ev.scope.unused, 3);
    assert_eq!(scope_ev.scope.false_path_only_endpoints, 1);
}

#[test]
fn metamorphic_false_path_never_adds_violations() {
    let base = eval(RECONV_V, RECONV_SDC, "");
    for fp_cmd in [
        "set_false_path -from [get_pins r1/CK]",
        "set_false_path -from [get_pins r2/CK]",
        "set_false_path -setup -from [get_pins r1/CK]",
        "set_false_path -hold -from [get_pins r2/CK]",
        "set_false_path -rise_to [get_pins r3/D]",
        "set_false_path -fall_to [get_pins r3/D]",
        "set_false_path -to [get_pins r3/D]",
    ] {
        let with_fp = eval(RECONV_V, RECONV_SDC, fp_cmd);
        assert!(with_fp.violating_setup <= base.violating_setup);
        assert!(with_fp.violating_hold <= base.violating_hold);
        for (b, f) in base.endpoints.iter().zip(with_fp.endpoints.iter()) {
            if let (Some(bs), Some(fs)) = (b.setup_slack, f.setup_slack) {
                assert!(fs + TOL >= bs, "{fp_cmd}: setup {bs} -> {fs}");
            }
            if let (Some(bh), Some(fh)) = (b.hold_slack, f.hold_slack) {
                assert!(fh + TOL >= bh, "{fp_cmd}: hold {bh} -> {fh}");
            }
        }
    }
}

#[test]
fn metamorphic_multicycle_shifts_setup_slack_by_n_minus_1_t() {
    let base = eval(TWOCLK_V, TWOCLK_41_SDC, "");
    let (base_ab, base_ba) = (base.slack("r2/D").0.unwrap(), base.slack("r4/D").0.unwrap());
    for n in [0i32, 1, 2, 3, 4] {
        let dn = f64::from(n - 1);
        let ev_end = eval(
            TWOCLK_V,
            TWOCLK_41_SDC,
            &format!(
                "set_multicycle_path {n} -setup -end -from [get_clocks A] -to [get_clocks B]\n\
                 set_multicycle_path {n} -setup -end -from [get_clocks B] -to [get_clocks A]"
            ),
        );
        assert_opt_f64(
            ev_end.slack("r2/D").0,
            Some(base_ab + dn * 1.0),
            "N -end A->B",
        );
        assert_opt_f64(
            ev_end.slack("r4/D").0,
            Some(base_ba + dn * 4.0),
            "N -end B->A",
        );

        let ev_start = eval(
            TWOCLK_V,
            TWOCLK_41_SDC,
            &format!(
                "set_multicycle_path {n} -setup -start -from [get_clocks A] -to [get_clocks B]\n\
                 set_multicycle_path {n} -setup -start -from [get_clocks B] -to [get_clocks A]"
            ),
        );
        assert_opt_f64(
            ev_start.slack("r2/D").0,
            Some(base_ab + dn * 4.0),
            "N -start A->B",
        );
        assert_opt_f64(
            ev_start.slack("r4/D").0,
            Some(base_ba + dn * 1.0),
            "N -start B->A",
        );
    }
}

#[test]
fn metamorphic_max_delay_equals_relation_replacement() {
    let base_rr = eval(PD_V, PD_SDC, "").slack("r2/D").0.unwrap();
    for v in [0.5, 1.0, 1.5, 2.5] {
        let cmd = format!("set_max_delay {v} -from [get_pins r1/CK] -to [get_pins r2/D]");
        let ev = eval(PD_V, PD_SDC, &cmd);
        assert_opt_f64(ev.slack("r2/D").0, Some(base_rr + (v - 2.0)), &cmd);
    }
}

#[test]
fn metamorphic_clock_groups_remove_cross_group_endpoints() {
    let cmd = "set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks B] -group [get_clocks C]";
    let ev = eval(GROUPS_V, GROUPS_SDC, cmd);
    for ep in ["r2/D", "r4/D", "r5/D", "r6/D"] {
        assert_eq!(ev.slack(ep), (None, None), "cross-group {ep}");
    }
}

#[test]
fn budget_exceeded_emits_t0060_error_naming_node() {
    assert_eq!(DEFAULT_NODE_EXCEPTION_STATE_BUDGET, 64);
    let err_small = eval_with_opts(
        RECONV_V,
        RECONV_SDC,
        "set_multicycle_path 2 -setup -from [get_pins r1/CK]\n\
         set_multicycle_path 3 -setup -from [get_pins r2/CK]",
        &ExceptionEvalOptions {
            max_states_per_node: 1,
            ..ExceptionEvalOptions::default()
        },
    )
    .expect_err("budget of 1 should fail with T0060");
    assert_eq!(err_small.code, "T0060");
    assert_eq!(err_small.severity, Severity::Error);
    assert!(err_small.message.contains("g/Y") || err_small.message.contains("r3/D"));

    let mut v = String::from("module m (clk, y");
    for i in 0..65 {
        let _ = write!(v, ", a{i}");
    }
    v.push_str(");\n  input clk");
    for i in 0..65 {
        let _ = write!(v, ", a{i}");
    }
    v.push_str(";\n  output y;\n");
    for i in 0..65 {
        let _ = writeln!(
            v,
            "  wire q{i};\n  DFF r{i} (.D(a{i}), .CK(clk), .Q(q{i}));"
        );
    }
    let mut cur: Vec<String> = (0..65).map(|i| format!("q{i}")).collect();
    let mut gid = 0usize;
    while cur.len() > 1 {
        let mut next = Vec::new();
        for pair in cur.chunks(2) {
            if pair.len() == 2 {
                let w = format!("t{gid}");
                let (p0, p1) = (&pair[0], &pair[1]);
                let _ = writeln!(
                    v,
                    "  wire {w};\n  AND2 g{gid} (.A({p0}), .B({p1}), .Y({w}));"
                );
                next.push(w);
                gid += 1;
            } else {
                next.push(pair[0].clone());
            }
        }
        cur = next;
    }
    let _ = writeln!(
        v,
        "  DFF rout (.D({}), .CK(clk), .Q(y));\nendmodule",
        cur[0]
    );

    let mut sdc65 = String::new();
    for i in 0..65 {
        let _ = writeln!(
            sdc65,
            "set_multicycle_path {} -setup -from [get_pins r{i}/CK]",
            (i % 5) + 2
        );
    }
    let err65 = eval_with_opts(
        &v,
        "create_clock -name clk -period 10 [get_ports clk]",
        &sdc65,
        &ExceptionEvalOptions::default(),
    )
    .expect_err("65 states should exceed default budget of 64");
    assert_eq!(err65.code, "T0060");
    assert_eq!(err65.severity, Severity::Error);
}
