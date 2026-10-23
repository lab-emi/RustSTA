// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Verification suite `sta-constants` (owner: E7; `STA-REQ-0150` to `STA-REQ-0157`).
//!
//! Validates constant sources (`1'b0`, `1'b1`, `TIELO`, `TIEHI`, `assign`, `set_case_analysis`,
//! `set_logic_zero`, `set_logic_one`), three-valued Kleene propagation, cofactor-based arc
//! disabling, `non_unate` sense refinement (`positive_unate` / `negative_unate`), sequential
//! constant data/clock/async pins, multiplexer select and Liberty `when` conditions,
//! `set_disable_timing`, and completeness accounting against the five analytic cases in
//! `verification/analytic/sta/`.

mod support;

use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::Func;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::delay::load::{NetLoadOverrides, NetLoads};
use ruststa::engine::delay::wire::Ideal;
use ruststa::engine::graph::constants::{
    CheckStatus, CofactorEffect, ConstOrigin, ConstantPropagation, LogicVal, PinConst,
    analyze_arc_cofactors, disable_reason_str, eval_func, eval_func_with_origin,
};
use ruststa::engine::graph::{ArcKind, ArcSense, DisableReason, TimingGraph};
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::constr::CompiledConstraints;
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::prop::lower::{
    BackendKind, IdealClockSpec, IdealSchedule, InputDelaySpec, LoweredCorner, LoweredResult,
    Precision,
};
use ruststa::engine::types::{An, NodeId, Tr};

const TOL: f64 = 1e-9;

fn assert_close(actual: f64, expected: f64, label: &str) {
    let diff = (actual - expected).abs();
    assert!(
        diff <= TOL,
        "{label}: actual {actual:.12} != expected {expected:.12} (diff {diff:.3e} > {TOL:.1e})"
    );
}

fn check_exp(actual: f64, exp: &serde_json::Value, key: &str) {
    let val = exp
        .get(key)
        .or_else(|| exp.get("base").and_then(|b| b.get(key)))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("missing key '{key}' in {exp}"));
    assert_close(actual, val, key);
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn analytic_expected(case: &str) -> serde_json::Value {
    let p = repo_root()
        .join("verification/analytic/sta")
        .join(case)
        .join("expected.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&text).expect("valid expected.json")
}

#[derive(Clone, Copy)]
struct LinArc {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    ms: f64,
    mc: f64,
}

const fn lin(d0: f64, ks: f64, kc: f64, s0: f64, ms: f64, mc: f64) -> LinArc {
    LinArc {
        d0,
        ks,
        kc,
        s0,
        ms,
        mc,
    }
}

impl LinArc {
    fn delay(self, s: f64, c: f64) -> f64 {
        self.d0 + self.ks * s + self.kc * c
    }

    fn slew(self, s: f64, c: f64) -> f64 {
        self.s0 + self.ms * s + self.mc * c
    }
}

const INV_R: LinArc = lin(0.10, 0.20, 2.0, 0.05, 0.0, 1.5);
const INV_F: LinArc = lin(0.08, 0.15, 1.5, 0.04, 0.0, 1.2);
const BUF_R: LinArc = lin(0.20, 0.10, 1.0, 0.06, 0.0, 1.0);
const BUF_F: LinArc = lin(0.18, 0.10, 1.2, 0.05, 0.0, 1.1);
const XOR_R: LinArc = lin(0.30, 0.25, 2.5, 0.07, 0.0, 1.8);
const XOR_F: LinArc = lin(0.28, 0.20, 2.2, 0.06, 0.0, 1.6);
const CKQ_R: LinArc = lin(0.40, 0.00, 1.0, 0.08, 0.0, 1.0);
const CKQ_F: LinArc = lin(0.35, 0.00, 0.9, 0.07, 0.0, 1.0);

fn analytic_library() -> Library {
    let mut o = String::from(
        "library (analytic_const) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    let push_table = |out: &mut String, kind: &str, f: &dyn Fn(f64, f64) -> f64| {
        let _ = writeln!(
            out,
            "        {kind} (t2) {{ values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\"); }}",
            f(0.0, 0.0),
            f(0.0, 1.0),
            f(1.0, 0.0),
            f(1.0, 1.0)
        );
    };
    let push_arc = |out: &mut String,
                    related: &str,
                    sense: &str,
                    ty: &str,
                    when_cond: Option<&str>,
                    r: LinArc,
                    f: LinArc| {
        let _ = writeln!(
            out,
            "      timing () {{\n        related_pin : \"{related}\";\n        \
             timing_sense : {sense};\n        timing_type : {ty};"
        );
        if let Some(w) = when_cond {
            let _ = writeln!(out, "        when : \"{w}\";");
        }
        push_table(out, "cell_rise", &|s, c| r.delay(s, c));
        push_table(out, "rise_transition", &|s, c| r.slew(s, c));
        push_table(out, "cell_fall", &|s, c| f.delay(s, c));
        push_table(out, "fall_transition", &|s, c| f.slew(s, c));
        let _ = writeln!(out, "      }}");
    };

    for (name, val) in [("TIELO", "0"), ("TIEHI", "1")] {
        let _ = writeln!(
            o,
            "  cell ({name}) {{\n    area : 1;\n    \
             pin (Y) {{ direction : output; function : \"{val}\"; }}\n  }}"
        );
    }

    for (name, func, sense, r, f) in [
        ("INV", "!A", "negative_unate", INV_R, INV_F),
        ("BUF", "A", "positive_unate", BUF_R, BUF_F),
    ] {
        let _ = writeln!(
            o,
            "  cell ({name}) {{\n    area : 1;\n    \
             pin (A) {{ direction : input; capacitance : 0.01; }}\n    \
             pin (Y) {{\n      direction : output;\n      function : \"{func}\";"
        );
        push_arc(&mut o, "A", sense, "combinational", None, r, f);
        let _ = writeln!(o, "    }}\n  }}");
    }

    for (name, func, sense, r, f) in [
        ("AND2", "A & B", "positive_unate", BUF_R, BUF_F),
        ("OR2", "A | B", "positive_unate", BUF_R, BUF_F),
        ("XOR2", "A ^ B", "non_unate", XOR_R, XOR_F),
    ] {
        let _ = writeln!(
            o,
            "  cell ({name}) {{\n    area : 2;\n    \
             pin (A) {{ direction : input; capacitance : 0.01; }}\n    \
             pin (B) {{ direction : input; capacitance : 0.01; }}\n    \
             pin (Y) {{\n      direction : output;\n      function : \"{func}\";"
        );
        push_arc(&mut o, "A", sense, "combinational", None, r, f);
        push_arc(&mut o, "B", sense, "combinational", None, r, f);
        let _ = writeln!(o, "    }}\n  }}");
    }

    let _ = writeln!(
        o,
        "  cell (MUX2) {{\n    area : 2;\n    \
         pin (A0) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (A1) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (S) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (Y) {{\n      direction : output;\n      function : \"(A0 & !S) | (A1 & S)\";"
    );
    push_arc(
        &mut o,
        "A0",
        "positive_unate",
        "combinational",
        None,
        BUF_R,
        BUF_F,
    );
    push_arc(
        &mut o,
        "A1",
        "positive_unate",
        "combinational",
        None,
        BUF_R,
        BUF_F,
    );
    push_arc(
        &mut o,
        "S",
        "non_unate",
        "combinational",
        None,
        XOR_R,
        XOR_F,
    );
    let _ = writeln!(o, "    }}\n  }}");

    // MUX2W: MUX2 with explicit `when` conditions on A0 -> Y (!S) and A1 -> Y (S).
    let _ = writeln!(
        o,
        "  cell (MUX2W) {{\n    area : 2;\n    \
         pin (A0) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (A1) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (S) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (Y) {{\n      direction : output;\n      function : \"(A0 & !S) | (A1 & S)\";"
    );
    push_arc(
        &mut o,
        "A0",
        "positive_unate",
        "combinational",
        Some("!S"),
        BUF_R,
        BUF_F,
    );
    push_arc(
        &mut o,
        "A1",
        "positive_unate",
        "combinational",
        Some("S"),
        BUF_R,
        BUF_F,
    );
    push_arc(
        &mut o,
        "S",
        "non_unate",
        "combinational",
        None,
        XOR_R,
        XOR_F,
    );
    let _ = writeln!(o, "    }}\n  }}");

    let _ = writeln!(
        o,
        "  cell (DFF) {{\n    area : 4;\n    \
         ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; }}\n    \
         pin (D) {{\n      direction : input;\n      capacitance : 0.01;\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : setup_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.10\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.10\"); }}\n      }}\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : hold_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.05\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.05\"); }}\n      }}\n    }}\n    \
         pin (CK) {{ direction : input; capacitance : 0.01; clock : true; }}\n    \
         pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    push_arc(&mut o, "CK", "non_unate", "rising_edge", None, CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}");

    // DFFSR with active-low async clear (!RN) and preset (!SN).
    let _ = writeln!(
        o,
        "  cell (DFFSR) {{\n    area : 5;\n    \
         ff (IQ, IQN) {{\n      next_state : \"D\";\n      clocked_on : \"CK\";\n      \
         clear : \"!RN\";\n      preset : \"!SN\";\n      \
         clear_preset_var1 : L;\n      clear_preset_var2 : H;\n    }}\n    \
         pin (D) {{\n      direction : input;\n      capacitance : 0.01;\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : setup_rising;\n        \
         when : \"RN & SN\";\n        \
         rise_constraint (scalar) {{ values (\"0.10\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.10\"); }}\n      }}\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : hold_rising;\n        \
         when : \"RN & SN\";\n        \
         rise_constraint (scalar) {{ values (\"0.05\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.05\"); }}\n      }}\n    }}\n    \
         pin (RN) {{\n      direction : input;\n      capacitance : 0.01;\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : recovery_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.12\"); }}\n      }}\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : removal_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.06\"); }}\n      }}\n    }}\n    \
         pin (SN) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (CK) {{ direction : input; capacitance : 0.01; clock : true; }}\n    \
         pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    push_arc(
        &mut o,
        "CK",
        "non_unate",
        "rising_edge",
        Some("RN & SN"),
        CKQ_R,
        CKQ_F,
    );
    let _ = writeln!(o, "    }}\n  }}\n}}");

    read_text(&o, "analytic_const.lib").expect("parse analytic_const.lib")
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
    read_structural(verilog_src, "<analytic.v>", &cell_pins)
        .expect("structural verilog")
        .freeze()
}

fn find_node(nv: &NetlistView<'_>, name: &str) -> NodeId {
    for i in 0..nv.num_nodes() {
        let nid = NodeId(i as u32);
        if nv.node_name_str(nid) == name {
            return nid;
        }
    }
    panic!("node '{name}' not found");
}

fn find_comb_arc(nv: &NetlistView<'_>, graph: &TimingGraph, src: &str, dst: &str) -> usize {
    let (s, d) = (find_node(nv, src), find_node(nv, dst));
    graph
        .arcs()
        .iter()
        .position(|a| a.src == s && a.dst == d && !matches!(a.kind, ArcKind::Net { .. }))
        .unwrap_or_else(|| panic!("cell arc {src} -> {dst} not found"))
}

fn propagate_with_inputs(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    tlib: &TimingLib,
    frozen: &FrozenNetlist<'_>,
    clk_port: &str,
    period: f64,
    in_ports: &[(&str, f64)],
) -> LoweredResult {
    let loads = NetLoads::compute(frozen, tlib, &Ideal, &NetLoadOverrides::default());
    let clk_node = find_node(nv, clk_port);
    let sched = IdealSchedule {
        clocks: vec![IdealClockSpec {
            name: clk_port.to_string(),
            period,
            waveform: [0.0, period * 0.5],
            sources: vec![clk_node],
            latency: [0.0, 0.0],
            transition: [0.0, 0.0],
            uncertainty: [0.0, 0.0],
        }],
        input_delays: in_ports
            .iter()
            .map(|&(p, d)| InputDelaySpec {
                node: find_node(nv, p),
                clock: 0,
                edge: Tr::Rise,
                delay: [d, d],
                transition: [0.0, 0.0],
            })
            .collect(),
        untagged_transitions: Vec::new(),
        max_time_borrow: Default::default(),
    };
    let lowered: LoweredCorner<'_> = sched.lower(nv, graph, tlib, &loads).expect("lower corner");
    lowered
        .propagate(BackendKind::Cpu, Precision::F64, Ctx::new(None, None))
        .expect("cpu propagation")
}

fn node_arr(res: &LoweredResult, graph: &TimingGraph, node: NodeId, tr: Tr, an: An) -> f64 {
    res.arrival(graph, node, 0, tr, an)
}

fn worst_arr(res: &LoweredResult, graph: &TimingGraph, node: NodeId, an: An) -> f64 {
    let r = node_arr(res, graph, node, Tr::Rise, an);
    let f = node_arr(res, graph, node, Tr::Fall, an);
    match an {
        An::Late => r.max(f),
        An::Early => r.min(f),
    }
}

// STA-REQ-0150, STA-REQ-0151, STA-REQ-0152: Analytic case `const_tielo_and2`
#[test]
fn analytic_const_tielo_and2() {
    let exp = analytic_expected("const_tielo_and2");
    let lib = analytic_library();
    let libs = [lib];
    let tlib = TimingLib::from_corner("base", &libs).expect("timing lib");
    let verilog = r#"
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, n1, n2;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 AND2 g1 (.A(qa), .B(lo), .Y(n1));
 OR2 g2 (.A(n1), .B(qb), .Y(n2));
 DFF f1 (.D(n2), .CK(clk), .Q(q));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");

    // 1. Without constant propagation: ra -> g1 -> g2 -> f1/D is active.
    let g_no_const = TimingGraph::build(&nv, &libs).expect("build graph");
    let res_no_const = propagate_with_inputs(
        &nv,
        &g_no_const,
        &tlib,
        &frozen,
        "clk",
        2.0,
        &[("a", 0.1), ("b", 0.1)],
    );
    let f1_d = find_node(&nv, "f1/D");
    let setup_no_const = 2.0 - 0.10 - worst_arr(&res_no_const, &g_no_const, f1_d, An::Late);
    check_exp(setup_no_const, &exp, "without constants f1/D setup");

    // 2. With constant propagation: t0/Y = 0 -> g1/B = 0 -> g1/Y = 0 -> g2/A = 0.
    let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
    let cp = ConstantPropagation::apply_netlist_only(&nv, &libs, &mut g);

    for pin in ["t0/Y", "g1/B", "g1/Y", "g2/A"] {
        let nid = find_node(&nv, pin);
        assert_eq!(cp.node_val(nid), LogicVal::Zero, "{pin} should be 0");
        assert_eq!(
            cp.node_const(nid).origin,
            Some(ConstOrigin::Netlist),
            "{pin} origin"
        );
    }
    for x_pin in ["g1/A", "g2/B", "g2/Y", "f1/D"] {
        assert_eq!(
            cp.node_val(find_node(&nv, x_pin)),
            LogicVal::X,
            "{x_pin} should be X"
        );
    }

    let g1_ay = find_comb_arc(&nv, &g, "g1/A", "g1/Y");
    let g1_by = find_comb_arc(&nv, &g, "g1/B", "g1/Y");
    let g2_ay = find_comb_arc(&nv, &g, "g2/A", "g2/Y");
    let g2_by = find_comb_arc(&nv, &g, "g2/B", "g2/Y");
    assert_eq!(g.arc_disable_reason(g1_ay), Some(DisableReason::Constant));
    assert_eq!(g.arc_disable_reason(g1_by), Some(DisableReason::Constant));
    assert_eq!(g.arc_disable_reason(g2_ay), Some(DisableReason::Constant));
    assert_eq!(g.arc_disable_reason(g2_by), None);
    assert_eq!(cp.disabled_arc_counts().constant, 3);

    let res = propagate_with_inputs(
        &nv,
        &g,
        &tlib,
        &frozen,
        "clk",
        2.0,
        &[("a", 0.1), ("b", 0.1)],
    );
    let setup = 2.0 - 0.10 - worst_arr(&res, &g, f1_d, An::Late);
    let hold = worst_arr(&res, &g, f1_d, An::Early) - 0.05;
    check_exp(setup, &exp, "f1/D setup");
    check_exp(hold, &exp, "f1/D hold");
}

// STA-REQ-0153: Analytic case `const_xor_tiehi` and unate sense refinement
#[test]
fn analytic_const_xor_tiehi_and_sense_refinement() {
    let exp = analytic_expected("const_xor_tiehi");
    let lib = analytic_library();
    let libs = [lib];
    let tlib = TimingLib::from_corner("base", &libs).expect("timing lib");
    let verilog = r#"
module m (input clk, input c, output q);
 wire hi, qc, n1;
 TIEHI t1 (.Y(hi));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 XOR2 g (.A(qc), .B(hi), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");
    let f_d = find_node(&nv, "f/D");

    // 1. Without sense refinement (non-unate A -> Y):
    let g_unrefined = TimingGraph::build(&nv, &libs).expect("build graph");
    let res_unrefined =
        propagate_with_inputs(&nv, &g_unrefined, &tlib, &frozen, "clk", 2.0, &[("c", 0.1)]);
    check_exp(
        2.0 - 0.10 - worst_arr(&res_unrefined, &g_unrefined, f_d, An::Late),
        &exp,
        "non-unate f/D setup",
    );
    check_exp(
        worst_arr(&res_unrefined, &g_unrefined, f_d, An::Early) - 0.05,
        &exp,
        "non-unate f/D hold",
    );

    // 2. With constant propagation: B = 1 refines g: A -> Y from non_unate to negative_unate.
    let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
    let g_ay = find_comb_arc(&nv, &g, "g/A", "g/Y");
    let g_by = find_comb_arc(&nv, &g, "g/B", "g/Y");
    assert_eq!(g.arcs()[g_ay].sense, ArcSense::NonUnate);

    let cp = ConstantPropagation::apply_netlist_only(&nv, &libs, &mut g);
    assert_eq!(cp.node_val(find_node(&nv, "t1/Y")), LogicVal::One);
    assert_eq!(cp.node_val(find_node(&nv, "g/B")), LogicVal::One);
    assert_eq!(g.arc_disable_reason(g_by), Some(DisableReason::Constant));
    assert_eq!(g.arc_disable_reason(g_ay), None);
    assert_eq!(g.arcs()[g_ay].sense, ArcSense::NegativeUnate);
    assert_eq!(cp.refined_arcs().len(), 1);
    assert_eq!(cp.refined_arcs()[0].arc_idx, g_ay);
    assert_eq!(cp.refined_arcs()[0].old_sense, ArcSense::NonUnate);
    assert_eq!(cp.refined_arcs()[0].new_sense, ArcSense::NegativeUnate);

    let res = propagate_with_inputs(&nv, &g, &tlib, &frozen, "clk", 2.0, &[("c", 0.1)]);
    check_exp(node_arr(&res, &g, f_d, Tr::Fall, An::Late), &exp, "Y fall");
    check_exp(node_arr(&res, &g, f_d, Tr::Rise, An::Early), &exp, "Y rise");
    check_exp(
        2.0 - 0.10 - worst_arr(&res, &g, f_d, An::Late),
        &exp,
        "f/D setup",
    );
    check_exp(worst_arr(&res, &g, f_d, An::Early) - 0.05, &exp, "f/D hold");

    // 3. Also verify XOR2 with B = 0 refines A -> Y to PositiveUnate, and (A ^ B) & C with B = 1, C = X
    // refines A -> Y to NegativeUnate (r|_{A=0} = C >= 0 = r|_{A=1}).
    let xor_fn = Func::Xor(
        Box::new(Func::Var("A".into())),
        Box::new(Func::Var("B".into())),
    );
    assert_eq!(
        analyze_arc_cofactors(&xor_fn, "A", &|n| if n == "B" {
            PinConst::netlist(false)
        } else {
            PinConst::X
        }),
        CofactorEffect::PositiveUnate
    );
    let gated_xor = Func::And(Box::new(xor_fn.clone()), Box::new(Func::Var("C".into())));
    assert_eq!(
        analyze_arc_cofactors(&gated_xor, "A", &|n| if n == "B" {
            PinConst::netlist(true)
        } else {
            PinConst::X
        }),
        CofactorEffect::NegativeUnate
    );
}

// STA-REQ-0155: Analytic case `const_mux_select` and Liberty `when` condition disabling
#[test]
fn analytic_const_mux_select_and_when_arcs() {
    let exp = analytic_expected("const_mux_select");
    let lib = analytic_library();
    let libs = [lib];
    let tlib = TimingLib::from_corner("base", &libs).expect("timing lib");
    let verilog = r#"
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, nb, n1;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 INV i (.A(qb), .Y(nb));
 MUX2 g (.A0(qa), .A1(nb), .S(lo), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");
    let f_d = find_node(&nv, "f/D");

    // 1. Without constants (A1 enabled):
    let g_all = TimingGraph::build(&nv, &libs).expect("build graph");
    let res_all = propagate_with_inputs(
        &nv,
        &g_all,
        &tlib,
        &frozen,
        "clk",
        2.0,
        &[("a", 0.1), ("b", 0.1)],
    );
    check_exp(
        node_arr(&res_all, &g_all, f_d, Tr::Rise, An::Late),
        &exp,
        "A1 rise",
    );
    check_exp(
        node_arr(&res_all, &g_all, f_d, Tr::Fall, An::Late),
        &exp,
        "A1 fall",
    );
    check_exp(
        2.0 - 0.10 - worst_arr(&res_all, &g_all, f_d, An::Late),
        &exp,
        "with A1 enabled f/D setup",
    );

    // 2. With constant S = 0: A1 -> Y and S -> Y disabled with `constant`, A0 -> Y enabled.
    let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
    let cp = ConstantPropagation::apply_netlist_only(&nv, &libs, &mut g);
    let a0_y = find_comb_arc(&nv, &g, "g/A0", "g/Y");
    let a1_y = find_comb_arc(&nv, &g, "g/A1", "g/Y");
    let s_y = find_comb_arc(&nv, &g, "g/S", "g/Y");
    assert_eq!(g.arc_disable_reason(a0_y), None);
    assert_eq!(g.arc_disable_reason(a1_y), Some(DisableReason::Constant));
    assert_eq!(g.arc_disable_reason(s_y), Some(DisableReason::Constant));
    assert_eq!(cp.disabled_arc_counts().constant, 2);

    let res = propagate_with_inputs(
        &nv,
        &g,
        &tlib,
        &frozen,
        "clk",
        2.0,
        &[("a", 0.1), ("b", 0.1)],
    );
    check_exp(node_arr(&res, &g, f_d, Tr::Rise, An::Late), &exp, "A0 rise");
    check_exp(
        node_arr(&res, &g, f_d, Tr::Fall, An::Early),
        &exp,
        "A0 fall",
    );
    check_exp(
        2.0 - 0.10 - worst_arr(&res, &g, f_d, An::Late),
        &exp,
        "f/D setup",
    );
    check_exp(worst_arr(&res, &g, f_d, An::Early) - 0.05, &exp, "f/D hold");

    // 3. MUX2W with explicit Liberty `when : "!S"` on A0 -> Y and `when : "S"` on A1 -> Y:
    // tying S = 1 (TIEHI) disables A0 -> Y with `DisableReason::FalseWhen` (`false_when`).
    let verilog_when = r#"
module m (input clk, input a, input b, output q);
 wire hi, qa, qb, n1;
 TIEHI t1 (.Y(hi));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 MUX2W g (.A0(qa), .A1(qb), .S(hi), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
"#;
    let frozen_w = parse_verilog(verilog_when, &libs[0]);
    let nv_w = NetlistView::new(&frozen_w, &libs).expect("netlist view");
    let mut g_w = TimingGraph::build(&nv_w, &libs).expect("build graph");
    let cp_w = ConstantPropagation::apply_netlist_only(&nv_w, &libs, &mut g_w);
    let w_a0_y = find_comb_arc(&nv_w, &g_w, "g/A0", "g/Y");
    let w_a1_y = find_comb_arc(&nv_w, &g_w, "g/A1", "g/Y");
    let w_s_y = find_comb_arc(&nv_w, &g_w, "g/S", "g/Y");
    assert_eq!(
        g_w.arc_disable_reason(w_a0_y),
        Some(DisableReason::FalseWhen)
    );
    assert_eq!(g_w.arc_disable_reason(w_a1_y), None);
    assert_eq!(g_w.arc_disable_reason(w_s_y), Some(DisableReason::Constant));
    assert_eq!(cp_w.disabled_arc_counts().false_when, 1);
    assert_eq!(cp_w.disabled_arc_counts().constant, 1);
}

// STA-REQ-0154: Analytic case `const_flop_d` and sequential async clear/preset constants
#[test]
fn analytic_const_flop_d_and_sequential_async() {
    let exp = analytic_expected("const_flop_d");
    let lib = analytic_library();
    let libs = [lib];
    let tlib = TimingLib::from_corner("base", &libs).expect("timing lib");
    let verilog = r#"
module m (input clk, input a, output y);
 wire lo, hi, qd, q2;
 TIELO t0 (.Y(lo));
 TIEHI t1 (.Y(hi));
 DFF f (.D(lo), .CK(clk), .Q(qd));
 BUF g (.A(qd), .Y(y));
 DFF f2 (.D(qd), .CK(hi), .Q(q2));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");
    let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
    let cp = ConstantPropagation::apply_netlist_only(&nv, &libs, &mut g);

    // f/D is constant 0 -> setup/hold checks on f/D have status NoDataArrival,
    // but f/Q is NOT constant and f: CK -> Q launch remains enabled!
    assert_eq!(cp.node_val(find_node(&nv, "f/D")), LogicVal::Zero);
    assert_eq!(cp.node_val(find_node(&nv, "f/Q")), LogicVal::X);
    let f_launch = g
        .launches()
        .iter()
        .position(|l| l.out_pin == find_node(&nv, "f/Q"))
        .unwrap();
    let f2_launch = g
        .launches()
        .iter()
        .position(|l| l.out_pin == find_node(&nv, "f2/Q"))
        .unwrap();
    assert!(!g.is_launch_disabled(f_launch));
    assert_eq!(
        g.launch_disable_reason(f2_launch),
        Some(DisableReason::Constant)
    );

    for (ci, ch) in g.checks().iter().enumerate() {
        if ch.data_pin == find_node(&nv, "f/D") {
            assert_eq!(cp.check_status(ci), CheckStatus::NoDataArrival);
            assert_eq!(cp.check_status(ci).as_str(), "no_data_arrival");
        } else if ch.data_pin == find_node(&nv, "f2/D") {
            assert_eq!(cp.check_status(ci), CheckStatus::Unclocked);
            assert_eq!(cp.check_status(ci).as_str(), "unclocked");
        }
    }

    let res = propagate_with_inputs(&nv, &g, &tlib, &frozen, "clk", 2.0, &[]);
    let y_node = find_node(&nv, "y");
    let y_setup = 2.0 - 0.20 - worst_arr(&res, &g, y_node, An::Late);
    let y_hold = worst_arr(&res, &g, y_node, An::Early) + 0.20;
    check_exp(y_setup, &exp, "y setup");
    check_exp(y_hold, &exp, "y hold");

    // Verify DFFSR with active async clear (RN = 0) vs inactive async clear/preset (RN = 1, SN = 1).
    let verilog_async = r#"
module m (input clk, input d, output q_clr, output q_live);
 wire lo, hi;
 TIELO t0 (.Y(lo));
 TIEHI t1 (.Y(hi));
 DFFSR f_clr (.D(d), .CK(clk), .RN(lo), .SN(hi), .Q(q_clr));
 DFFSR f_live (.D(d), .CK(clk), .RN(hi), .SN(hi), .Q(q_live));
endmodule
"#;
    let frozen_a = parse_verilog(verilog_async, &libs[0]);
    let nv_a = NetlistView::new(&frozen_a, &libs).expect("netlist view");
    let mut g_a = TimingGraph::build(&nv_a, &libs).expect("build graph");
    let cp_a = ConstantPropagation::apply_netlist_only(&nv_a, &libs, &mut g_a);

    // f_clr has RN = 0 (clear active) -> f_clr/Q is constant 0, its launch is disabled (`FalseWhen`
    // because `when : "RN & SN"` is false), and its setup/hold checks on D have `when` false (`Disabled(FalseWhen)`).
    assert_eq!(cp_a.node_val(find_node(&nv_a, "f_clr/Q")), LogicVal::Zero);
    assert_eq!(cp_a.node_val(find_node(&nv_a, "q_clr")), LogicVal::Zero);
    // f_live has RN = 1, SN = 1 (neither active) -> f_live/Q is X, D checks are Active, RN recovery/removal is NoDataArrival.
    assert_eq!(cp_a.node_val(find_node(&nv_a, "f_live/Q")), LogicVal::X);
    for (ci, ch) in g_a.checks().iter().enumerate() {
        if ch.data_pin == find_node(&nv_a, "f_clr/D") {
            assert_eq!(
                cp_a.check_status(ci),
                CheckStatus::Disabled(DisableReason::FalseWhen)
            );
            assert_eq!(cp_a.check_status(ci).as_str(), "disabled");
        } else if ch.data_pin == find_node(&nv_a, "f_live/RN") {
            assert_eq!(cp_a.check_status(ci), CheckStatus::NoDataArrival);
        } else if ch.data_pin == find_node(&nv_a, "f_live/D") {
            assert_eq!(cp_a.check_status(ci), CheckStatus::Active);
            assert_eq!(cp_a.check_status(ci).as_str(), "constrained");
        }
    }
}

// STA-REQ-0156, STA-REQ-0157: Analytic case `case_analysis` (all 4 SDC variants: live, case1, logic0, case1dis)
#[test]
fn analytic_case_analysis_variants() {
    let exp = analytic_expected("case_analysis");
    let lib = analytic_library();
    let libs = [lib];
    let tlib = TimingLib::from_corner("base", &libs).expect("timing lib");
    let verilog = r#"
module m (input clk, input a, input c, input s, output q);
 wire qa, qc, nc, n1;
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 INV i (.A(qc), .Y(nc));
 MUX2 g (.A0(qa), .A1(nc), .S(s), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");
    let f_d = find_node(&nv, "f/D");

    let run_variant = |sdc_extra: &str| {
        let sdc_text = format!(
            "create_clock -name clk -period 2 [get_ports clk]\n\
             set_input_delay -clock clk 0.1 [get_ports {{a c s}}]\n{sdc_extra}"
        );
        let (sdc, diags) = rusteda_formats::sdc::read_str(&sdc_text);
        assert!(diags.is_empty(), "SDC parse diags: {diags:?}");
        let compiled = CompiledConstraints::compile_single_corner(&sdc, &frozen, &libs)
            .expect("compile constraints");
        assert!(
            compiled.diags.is_empty(),
            "compile diags: {:?}",
            compiled.diags
        );

        let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
        let cp = ConstantPropagation::apply(&nv, &libs, Some(&compiled), &mut g);
        let res = propagate_with_inputs(
            &nv,
            &g,
            &tlib,
            &frozen,
            "clk",
            2.0,
            &[("a", 0.1), ("c", 0.1), ("s", 0.1)],
        );
        (g, cp, res)
    };

    // Variant 1: `live`
    {
        let (g, cp, res) = run_variant("");
        let f_setup = 2.0 - 0.10 - worst_arr(&res, &g, f_d, An::Late);
        let f_hold = worst_arr(&res, &g, f_d, An::Early) - 0.05;
        check_exp(f_setup, &exp["live"], "f/D setup");
        check_exp(f_hold, &exp["live"], "f/D hold");
        assert_eq!(cp.disabled_arc_counts().total(), 0);
        for (ci, ch) in g.checks().iter().enumerate() {
            if ch.data_pin == f_d {
                assert_eq!(cp.check_status(ci), CheckStatus::Active);
            }
        }
    }

    // Variant 2: `case1` (`set_case_analysis 1 [get_ports s]`)
    {
        let (g, cp, res) = run_variant("set_case_analysis 1 [get_ports s]\n");
        let a0_y = find_comb_arc(&nv, &g, "g/A0", "g/Y");
        let a1_y = find_comb_arc(&nv, &g, "g/A1", "g/Y");
        let s_y = find_comb_arc(&nv, &g, "g/S", "g/Y");
        assert_eq!(
            g.arc_disable_reason(a0_y),
            Some(DisableReason::CaseAnalysis)
        );
        assert_eq!(g.arc_disable_reason(a1_y), None);
        assert_eq!(g.arc_disable_reason(s_y), Some(DisableReason::CaseAnalysis));
        assert_eq!(cp.disabled_arc_counts().case_analysis, 2);

        let f_setup = 2.0 - 0.10 - worst_arr(&res, &g, f_d, An::Late);
        let f_hold = worst_arr(&res, &g, f_d, An::Early) - 0.05;
        check_exp(f_setup, &exp["case1"], "f/D setup");
        check_exp(f_hold, &exp["case1"], "f/D hold");
    }

    // Variant 3: `logic0` (`set_logic_zero [get_ports s]`)
    {
        let (g, cp, res) = run_variant("set_logic_zero [get_ports s]\n");
        let a0_y = find_comb_arc(&nv, &g, "g/A0", "g/Y");
        let a1_y = find_comb_arc(&nv, &g, "g/A1", "g/Y");
        let s_y = find_comb_arc(&nv, &g, "g/S", "g/Y");
        assert_eq!(g.arc_disable_reason(a0_y), None);
        assert_eq!(
            g.arc_disable_reason(a1_y),
            Some(DisableReason::CaseAnalysis)
        );
        assert_eq!(g.arc_disable_reason(s_y), Some(DisableReason::CaseAnalysis));
        assert_eq!(cp.disabled_arc_counts().case_analysis, 2);

        let f_setup = 2.0 - 0.10 - worst_arr(&res, &g, f_d, An::Late);
        let f_hold = worst_arr(&res, &g, f_d, An::Early) - 0.05;
        check_exp(f_setup, &exp["logic0"], "f/D setup");
        check_exp(f_hold, &exp["logic0"], "f/D hold");
    }

    // Variant 4: `case1dis` (`set_case_analysis 1 [get_ports s]` + `set_disable_timing -from A1 -to Y [get_cells g]`)
    {
        let (g, cp, res) = run_variant(
            "set_case_analysis 1 [get_ports s]\n\
             set_disable_timing -from A1 -to Y [get_cells g]\n",
        );
        let a0_y = find_comb_arc(&nv, &g, "g/A0", "g/Y");
        let a1_y = find_comb_arc(&nv, &g, "g/A1", "g/Y");
        let s_y = find_comb_arc(&nv, &g, "g/S", "g/Y");
        assert_eq!(
            g.arc_disable_reason(a0_y),
            Some(DisableReason::CaseAnalysis)
        );
        assert_eq!(
            g.arc_disable_reason(a1_y),
            Some(DisableReason::UserDisabled)
        );
        assert_eq!(g.arc_disable_reason(s_y), Some(DisableReason::CaseAnalysis));
        assert_eq!(cp.disabled_arc_counts().case_analysis, 2);
        assert_eq!(cp.disabled_arc_counts().user_disabled, 1);
        assert_eq!(cp.disabled_arc_counts().total(), 3);

        assert_eq!(exp["case1dis"]["f/D"].as_str(), Some("no arrival"));
        assert!(worst_arr(&res, &g, f_d, An::Late).is_infinite());
        assert!(worst_arr(&res, &g, f_d, An::Early).is_infinite());
        for (ci, ch) in g.checks().iter().enumerate() {
            if ch.data_pin == f_d {
                assert_eq!(cp.check_status(ci), CheckStatus::NoDataArrival);
                assert_eq!(cp.check_status(ci).as_str(), "no_data_arrival");
            }
        }
    }
}

// STA-REQ-0150, STA-REQ-0151, STA-REQ-0156, STA-REQ-0157: Netlist 1'b0/1'b1 literals,
// continuous assigns, transition case analysis, and disable_reason_str taxonomy.
#[test]
fn verilog_literals_assigns_and_disable_taxonomy() {
    let lib = analytic_library();
    let libs = [lib];
    let verilog = r#"
module m (input clk, input a, input s_tr, output q);
 wire c0, c1, w_c0, qa, n1;
 assign c0 = 1'b0;
 assign c1 = 1'b1;
 assign w_c0 = c0;
 DFF ra (.D(a), .CK(clk), .Q(qa));
 MUX2 g (.A0(qa), .A1(c1), .S(w_c0), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
"#;
    let frozen = parse_verilog(verilog, &libs[0]);
    let nv = NetlistView::new(&frozen, &libs).expect("netlist view");
    let sdc_text = "create_clock -name clk -period 2 [get_ports clk]\n\
                    set_case_analysis rising [get_ports s_tr]\n\
                    set_disable_timing [get_pins f/D]\n";
    let (sdc, diags) = rusteda_formats::sdc::read_str(sdc_text);
    assert!(diags.is_empty());
    let compiled =
        CompiledConstraints::compile_single_corner(&sdc, &frozen, &libs).expect("compile");

    let mut g = TimingGraph::build(&nv, &libs).expect("build graph");
    let cp = ConstantPropagation::apply(&nv, &libs, Some(&compiled), &mut g);

    // 1'b0 propagated through `assign c0 = 1'b0; assign w_c0 = c0;` to `g/S`:
    assert_eq!(cp.node_val(find_node(&nv, "g/S")), LogicVal::Zero);
    assert_eq!(
        cp.node_const(find_node(&nv, "g/S")).origin,
        Some(ConstOrigin::Netlist)
    );
    // 1'b1 propagated through `assign c1 = 1'b1` to `g/A1`:
    assert_eq!(cp.node_val(find_node(&nv, "g/A1")), LogicVal::One);

    // `set_case_analysis rising` keeps logic value at X while recording Tr::Rise:
    let s_tr = find_node(&nv, "s_tr");
    assert_eq!(cp.node_val(s_tr), LogicVal::X);
    assert_eq!(cp.transition_case(s_tr), Some(Tr::Rise));

    // `set_disable_timing [get_pins f/D]` disables setup & hold checks on f/D:
    for (ci, ch) in g.checks().iter().enumerate() {
        if ch.data_pin == find_node(&nv, "f/D") {
            assert_eq!(
                cp.check_status(ci),
                CheckStatus::Disabled(DisableReason::UserDisabled)
            );
            assert_eq!(cp.check_status(ci).as_str(), "disabled");
        }
    }
    assert_eq!(cp.total_disabled_counts().user_disabled, 2);

    // Verify canonical completeness strings and Kleene truth tables:
    assert_eq!(disable_reason_str(DisableReason::LoopBreak), "cycle_break");
    assert_eq!(disable_reason_str(DisableReason::Constant), "constant");
    assert_eq!(
        disable_reason_str(DisableReason::CaseAnalysis),
        "case_analysis"
    );
    assert_eq!(disable_reason_str(DisableReason::FalseWhen), "false_when");
    assert_eq!(
        disable_reason_str(DisableReason::UserDisabled),
        "user_disabled"
    );

    let f_expr = Func::Or(
        Box::new(Func::And(
            Box::new(Func::Var("A".into())),
            Box::new(Func::Var("B".into())),
        )),
        Box::new(Func::And(
            Box::new(Func::Not(Box::new(Func::Var("A".into())))),
            Box::new(Func::Var("C".into())),
        )),
    );
    assert_eq!(
        eval_func(&f_expr, &|v| match v {
            "B" => LogicVal::One,
            "C" => LogicVal::One,
            _ => LogicVal::X,
        }),
        LogicVal::X
    );
    assert_eq!(
        eval_func_with_origin(&f_expr, &|v| match v {
            "B" => PinConst::netlist(true),
            "C" => PinConst::case_analysis(true),
            _ => PinConst::X,
        }),
        PinConst::case_analysis(true)
    );
}

fn full_analysis_slacks(
    libs: &[Library],
    verilog: &str,
    sdc: &str,
    pins: &[&str],
) -> Vec<(Option<f64>, Option<f64>)> {
    let frozen = parse_verilog(verilog, &libs[0]);
    let ea = EngineAnalysis::analyze_single_corner(&frozen, libs, sdc, &AnalysisOptions::ideal())
        .expect("full analysis");
    pins.iter()
        .map(|p| (ea.setup_slack_for(p), ea.hold_slack_for(p)))
        .collect()
}

fn check_full(actual: (Option<f64>, Option<f64>), exp: &serde_json::Value, pin: &str) {
    check_exp(actual.0.expect("timed setup"), exp, &format!("{pin} setup"));
    check_exp(actual.1.expect("timed hold"), exp, &format!("{pin} hold"));
}

// STA-REQ-0150 to STA-REQ-0157: the hand derivations of the constant cases hold for the full
// analysis of `ruststa run`, which applies netlist constants, case analysis and
// `set_disable_timing` to the graph before it propagates arrival times.
#[test]
fn analytic_constant_cases_in_full_analysis() {
    let libs = [analytic_library()];
    let clk = "create_clock -name clk -period 2 [get_ports clk]\n";

    let tielo = full_analysis_slacks(
        &libs,
        "module m (input clk, input a, input b, output q);\n wire lo, qa, qb, n1, n2;\n \
         TIELO t0 (.Y(lo));\n DFF ra (.D(a), .CK(clk), .Q(qa));\n DFF rb (.D(b), .CK(clk), .Q(qb));\n \
         AND2 g1 (.A(qa), .B(lo), .Y(n1));\n OR2 g2 (.A(n1), .B(qb), .Y(n2));\n \
         DFF f1 (.D(n2), .CK(clk), .Q(q));\nendmodule\n",
        &format!("{clk}set_input_delay -clock clk 0.1 [get_ports {{a b}}]\n"),
        &["f1/D"],
    );
    check_full(
        tielo[0],
        &analytic_expected("const_tielo_and2")["base"],
        "f1/D",
    );

    let xor = full_analysis_slacks(
        &libs,
        "module m (input clk, input c, output q);\n wire hi, qc, n1;\n TIEHI t1 (.Y(hi));\n \
         DFF rc (.D(c), .CK(clk), .Q(qc));\n XOR2 g (.A(qc), .B(hi), .Y(n1));\n \
         DFF f (.D(n1), .CK(clk), .Q(q));\nendmodule\n",
        &format!("{clk}set_input_delay -clock clk 0.1 [get_ports c]\n"),
        &["f/D"],
    );
    check_full(xor[0], &analytic_expected("const_xor_tiehi")["base"], "f/D");

    let mux = full_analysis_slacks(
        &libs,
        "module m (input clk, input a, input b, output q);\n wire lo, qa, qb, nb, n1;\n \
         TIELO t0 (.Y(lo));\n DFF ra (.D(a), .CK(clk), .Q(qa));\n DFF rb (.D(b), .CK(clk), .Q(qb));\n \
         INV i (.A(qb), .Y(nb));\n MUX2 g (.A0(qa), .A1(nb), .S(lo), .Y(n1));\n \
         DFF f (.D(n1), .CK(clk), .Q(q));\nendmodule\n",
        &format!("{clk}set_input_delay -clock clk 0.1 [get_ports {{a b}}]\n"),
        &["f/D"],
    );
    check_full(
        mux[0],
        &analytic_expected("const_mux_select")["base"],
        "f/D",
    );

    let flop = full_analysis_slacks(
        &libs,
        "module m (input clk, input a, output y);\n wire lo, hi, qd, q2;\n TIELO t0 (.Y(lo));\n \
         TIEHI t1 (.Y(hi));\n DFF f (.D(lo), .CK(clk), .Q(qd));\n BUF g (.A(qd), .Y(y));\n \
         DFF f2 (.D(qd), .CK(hi), .Q(q2));\nendmodule\n",
        &format!("{clk}set_output_delay -clock clk 0.2 [get_ports y]\n"),
        &["y", "f/D", "f2/D"],
    );
    check_full(flop[0], &analytic_expected("const_flop_d")["base"], "y");
    assert_eq!(flop[1], (None, None), "constant f/D is not timed");
    assert_eq!(
        flop[2],
        (None, None),
        "f2/D with a constant clock is not timed"
    );

    let exp = analytic_expected("case_analysis");
    let case_v = "module m (input clk, input a, input c, input s, output q);\n wire qa, qc, nc, n1;\n \
                  DFF ra (.D(a), .CK(clk), .Q(qa));\n DFF rc (.D(c), .CK(clk), .Q(qc));\n \
                  INV i (.A(qc), .Y(nc));\n MUX2 g (.A0(qa), .A1(nc), .S(s), .Y(n1));\n \
                  DFF f (.D(n1), .CK(clk), .Q(q));\nendmodule\n";
    let case_sdc =
        |extra: &str| format!("{clk}set_input_delay -clock clk 0.1 [get_ports {{a c s}}]\n{extra}");
    for (variant, extra) in [
        ("live", ""),
        ("case1", "set_case_analysis 1 [get_ports s]\n"),
        ("logic0", "set_logic_zero [get_ports s]\n"),
    ] {
        let r = full_analysis_slacks(&libs, case_v, &case_sdc(extra), &["f/D"]);
        check_full(r[0], &exp[variant], "f/D");
    }
    let dis = full_analysis_slacks(
        &libs,
        case_v,
        &case_sdc(
            "set_case_analysis 1 [get_ports s]\nset_disable_timing -from A1 -to Y [get_cells g]\n",
        ),
        &["f/D"],
    );
    assert_eq!(exp["case1dis"]["f/D"].as_str(), Some("no arrival"));
    assert_eq!(dis[0], (None, None), "case1dis f/D has no arrival");
}

// STA-REQ-0158: `set_case_analysis rising|falling` keeps the pin at `X` and is reported with
// warning `T0040`, because the analysis times both transitions.
#[test]
fn transition_case_analysis_is_reported() {
    let libs = [analytic_library()];
    let frozen = parse_verilog(
        "module m (input clk, input a, input s, output q);\n wire qa, n1;\n \
         DFF ra (.D(a), .CK(clk), .Q(qa));\n AND2 g (.A(qa), .B(s), .Y(n1));\n \
         DFF f (.D(n1), .CK(clk), .Q(q));\nendmodule\n",
        &libs[0],
    );
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\n\
               set_input_delay -clock clk 0.1 [get_ports {a s}]\n\
               set_case_analysis rising [get_ports s]\n";
    let ea = EngineAnalysis::analyze_single_corner(&frozen, &libs, sdc, &AnalysisOptions::ideal())
        .expect("full analysis");
    let t0040: Vec<_> = ea.diags.iter().filter(|d| d.code == "T0040").collect();
    assert_eq!(t0040.len(), 1, "{:?}", ea.diags);
    assert!(t0040[0].message.contains("'s'"), "{}", t0040[0].message);
    assert!(
        ea.setup_slack_for("f/D").is_some(),
        "the AND gate stays enabled"
    );
}
