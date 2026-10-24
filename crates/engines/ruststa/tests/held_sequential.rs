// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Sequential cells and clock networks next to logic constants: a flop whose asynchronous clear is
//! held active (`STA-REQ-0162`), clock gate outputs (`STA-REQ-0162`), constant pins pass no
//! arrival (`STA-REQ-0164`), launched signals that enter a propagated clock network
//! (`STA-REQ-0714`) and ideal clock networks stopped by a constant (`STA-REQ-0822`).
//! These rules replace STA-REQ-0116, STA-REQ-0151, STA-REQ-0161 and STA-REQ-0818; each test
//! states where the superseded rule gave a different result.
//!
//! The library has tables that are linear in the input transition `s` and the load `c`
//! (`v = base + 0.5 s + 2 c`, base per arc, index points 0 and 1 by 0 and 0.1), so that the
//! bilinear evaluation is exact and every expected value follows by hand; the derivations are in
//! the comments of each test. Wires are ideal; every input pin has the capacitance 0.001 pF.

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::graph::constants::{CheckStatus, ConstantPropagation, LogicVal};
use ruststa::engine::graph::{ArcKind, DisableReason, TimingGraph};
use ruststa::engine::limits::{CornerLimits, LimitKind};
use ruststa::engine::prop::clocks::ClockModel;
use ruststa::engine::types::{An, ClockId, NodeId, Tr};

/// A timing group of `related` with the delay base `d` and transition base `t` (both
/// transitions), `v = base + 0.5 s + 2 c`; `kind` is a `timing_sense` or `timing_type` line.
fn arc(related: &str, kind: &str, d: f64, t: f64) -> String {
    let v = |b: f64| {
        format!(
            "values (\"{b}, {}\", \"{}, {}\");",
            b + 0.2,
            b + 0.5,
            b + 0.7
        )
    };
    format!(
        "timing () {{ related_pin : \"{related}\"; {kind}
        cell_rise (tr_t) {{ {} }} cell_fall (tr_t) {{ {} }}
        rise_transition (tr_t) {{ {} }} fall_transition (tr_t) {{ {} }} }}",
        v(d),
        v(d),
        v(t),
        v(t)
    )
}

const CHECKS: &str = r#"
      timing () { related_pin : "CK"; timing_type : setup_rising;
        rise_constraint (ck_t) { values ("0.05, 0.05", "0.15, 0.15"); }
        fall_constraint (ck_t) { values ("0.05, 0.05", "0.15, 0.15"); } }
      timing () { related_pin : "CK"; timing_type : hold_rising;
        rise_constraint (ck_t) { values ("0.02, 0.02", "0.12, 0.12"); }
        fall_constraint (ck_t) { values ("0.02, 0.02", "0.12, 0.12"); } }"#;

fn lib() -> Library {
    let edge = arc("CK", "timing_type : rising_edge;", 0.3, 0.08);
    let text = format!(
        r#"library (hs) {{
  time_unit : "1ns"; capacitive_load_unit (1, pf);
  lu_table_template (tr_t) {{ variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance; index_1 ("0, 1"); index_2 ("0, 0.1"); }}
  lu_table_template (ck_t) {{ variable_1 : related_pin_transition;
    variable_2 : constrained_pin_transition; index_1 ("0, 1"); index_2 ("0, 1"); }}
  cell (TIEH) {{ pin (Z) {{ direction : output; function : "1"; }} }}
  cell (TIEL) {{ pin (ZN) {{ direction : output; function : "0"; }} }}
  cell (BUF) {{ pin (A) {{ direction : input; capacitance : 0.001; }}
    pin (Y) {{ direction : output; function : "A"; {buf} }} }}
  cell (AND2) {{ pin (A) {{ direction : input; capacitance : 0.001; }}
    pin (B) {{ direction : input; capacitance : 0.001; }}
    pin (Y) {{ direction : output; function : "A&B"; {and_a} {and_b} }} }}
  cell (DFF) {{ ff (IQ, IQN) {{ next_state : "D"; clocked_on : "CK"; }}
    pin (CK) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001; {checks} }}
    pin (Q) {{ direction : output; function : "IQ"; {edge} }} }}
  cell (DFFR) {{ ff (IQ, IQN) {{ next_state : "D"; clocked_on : "CK"; clear : "!RN"; }}
    pin (CK) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (RN) {{ direction : input; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001; {checks} }}
    pin (Q) {{ direction : output; function : "IQ"; {edge} {clear} }} }}
  cell (DFFRW) {{ ff (IQ, IQN) {{ next_state : "D"; clocked_on : "CK"; clear : "!RN"; }}
    pin (CK) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (RN) {{ direction : input; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001; {checks_when} }}
    pin (Q) {{ direction : output; function : "IQ"; {edge} }} }}
  cell (ICG) {{ clock_gating_integrated_cell : "latch_posedge_precontrol";
    statetable ("CK EN TE", "IQ") {{
      table : "L L L : - : L, L H - : - : H, L - H : - : H, H - - : - : N"; }}
    pin (CK) {{ direction : input; clock : true; clock_gate_clock_pin : true; capacitance : 0.001; }}
    pin (EN) {{ direction : input; clock_gate_enable_pin : true; capacitance : 0.001; }}
    pin (TE) {{ direction : input; clock_gate_test_pin : true; capacitance : 0.001; }}
    pin (IQ) {{ direction : internal; internal_node : "IQ"; }}
    pin (GCK) {{ direction : output; clock_gate_out_pin : true; state_function : "CK & IQ";
      {icg} }} }}
}}
"#,
        buf = arc("A", "timing_sense : positive_unate;", 0.1, 0.05),
        and_a = arc("A", "timing_sense : positive_unate;", 0.1, 0.06),
        and_b = arc("B", "timing_sense : positive_unate;", 0.1, 0.07),
        checks = CHECKS,
        checks_when = CHECKS.replace("timing_type :", "when : \"RN\"; timing_type :"),
        edge = edge,
        clear = arc(
            "RN",
            "timing_type : clear; timing_sense : positive_unate;",
            0.2,
            0.05
        ),
        icg = arc("CK", "timing_sense : positive_unate;", 0.15, 0.06),
    );
    read_text(&text, "hs.lib").expect("synthetic library")
}

fn freeze(verilog: &str, lib: &Library) -> FrozenNetlist<'static> {
    read_structural(verilog, "top.v", &|cell_name| {
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
    })
    .expect("netlist")
    .freeze()
}

fn analyze<'a>(frozen: &'a FrozenNetlist<'a>, lib: &Library, sdc: &str) -> EngineAnalysis<'a> {
    EngineAnalysis::analyze_single_corner(
        frozen,
        std::slice::from_ref(lib),
        sdc,
        &AnalysisOptions::ideal(),
    )
    .expect("analysis")
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-12, "{what}: {a} vs {b}");
}

/// The constant propagation of the analysis' netlist and constraints, on a fresh graph.
fn consts(a: &EngineAnalysis<'_>, lib: &Library) -> ConstantPropagation {
    let libs = std::slice::from_ref(lib);
    let mut g = TimingGraph::build(&a.netview, libs).expect("graph");
    ConstantPropagation::apply(&a.netview, libs, Some(&a.constraints), &mut g)
}

fn node(a: &EngineAnalysis<'_>, pin: &str) -> NodeId {
    a.find_node(pin).unwrap_or_else(|| panic!("no pin {pin}"))
}

/// `[late, early]` transition of `pin` for `tr`.
fn slew(a: &EngineAnalysis<'_>, pin: &str, tr: Tr) -> [f64; 2] {
    let n = node(a, pin);
    [
        a.corners[0].slew(n, tr, An::Late),
        a.corners[0].slew(n, tr, An::Early),
    ]
}

fn arrival(a: &EngineAnalysis<'_>, pin: &str, tr: Tr, an: An) -> Option<f64> {
    a.corners[0]
        .worst_arrival(node(a, pin), tr, an)
        .map(|(_, x)| x)
}

fn drv_pins(l: &CornerLimits, kind: LimitKind, a: &EngineAnalysis<'_>) -> Vec<String> {
    let mut v: Vec<String> = l
        .rows_of(kind)
        .map(|r| a.netview.node_name_str(r.node).into_owned())
        .collect();
    v.sort();
    v
}

fn cell_arc_reason(a: &EngineAnalysis<'_>, src: &str, dst: &str) -> Option<DisableReason> {
    let (s, d) = (node(a, src), node(a, dst));
    let i = a
        .graph
        .arcs()
        .iter()
        .position(|g| g.src == s && g.dst == d && !matches!(g.kind, ArcKind::Net { .. }))
        .unwrap_or_else(|| panic!("no arc {src} -> {dst}"));
    a.graph.arc_disable_reason(i)
}

fn net_arc_reason(a: &EngineAnalysis<'_>, src: &str, dst: &str) -> Option<DisableReason> {
    let (s, d) = (node(a, src), node(a, dst));
    let i = a
        .graph
        .arcs()
        .iter()
        .position(|g| g.src == s && g.dst == d && matches!(g.kind, ArcKind::Net { .. }))
        .unwrap_or_else(|| panic!("no wire {src} -> {dst}"));
    a.graph.arc_disable_reason(i)
}

// STA-REQ-0162: rr is a DFFR whose clear RN is tied low (held active). Its stored state is not a
// constant: rr/Q stays X, its clock-to-output arc launches, and the AND2 g below it is live.
// Ideal clock with transition 0.1; rr/Q drives g/A (0.001):
// - CK -> Q: delay 0.3 + 0.5 * 0.1 + 2 * 0.001 = 0.352, transition 0.08 + 0.05 + 0.002 = 0.132
//   (the clear arc RN -> Q is an asynchronous arc that the timing graph does not propagate,
//   STA-REQ-0017, so it adds no transition, late or early);
// - g/Y (load o1, no load): late via A 0.352 + 0.1 + 0.5 * 0.132 = 0.518; via B (input delay 0,
//   transition 0.2) 0.1 + 0.1 = 0.2; early the smaller 0.2.
// Setup slack at o1 (output delay 0.5): 10 - 0.5 - 0.518 = 8.982; hold 0.2 + 0.5 = 0.7.
// rr/Q is a driver of a live net, checked for capacitance and fanout. (STA-REQ-0151 made rr/Q a
// constant 0, so g/Y was a constant 0 too: o1 was untimed and rr/Q not checked.)
#[test]
fn held_clear_leaves_the_flop_output_live() {
    let lib = lib();
    let netlist = "module top (input clk, input a, input b, output o1);\n\
        wire lo, n1;\nTIEL tl (.ZN(lo));\n\
        DFFR rr (.CK(clk), .D(a), .RN(lo), .Q(n1));\n\
        AND2 g (.A(n1), .B(b), .Y(o1));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_clock_transition 0.1 [get_clocks c]\n\
        set_input_transition 0.2 [get_ports {a b}]\n\
        set_input_delay 0 -clock c [get_ports {a b}]\n\
        set_output_delay 0.5 -clock c [get_ports o1]\n\
        set_max_capacitance 0.00001 [current_design]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    let cp = consts(&a, &lib);
    assert_eq!(cp.node_val(node(&a, "rr/Q")), LogicVal::X);
    assert_eq!(cp.node_val(node(&a, "rr/RN")), LogicVal::Zero);
    assert_eq!(cell_arc_reason(&a, "g/A", "g/Y"), None);
    for tr in Tr::ALL {
        close(
            arrival(&a, "rr/Q", tr, An::Late).expect("launch"),
            0.352,
            "rr/Q",
        );
        let [late, early] = slew(&a, "rr/Q", tr);
        close(late, 0.132, "rr/Q late transition");
        close(early, 0.132, "rr/Q early transition");
        close(
            arrival(&a, "o1", tr, An::Late).expect("late"),
            0.518,
            "o1 late",
        );
        close(
            arrival(&a, "o1", tr, An::Early).expect("early"),
            0.2,
            "o1 early",
        );
    }
    close(a.setup_slack_for("o1").expect("setup"), 8.982, "o1 setup");
    close(a.hold_slack_for("o1").expect("hold"), 0.7, "o1 hold");
    // The checks of rr/D have no `when`: timed (input delay 0, ideal clock latency 0).
    assert!(a.setup_slack_for("rr/D").is_some());
    let cap = drv_pins(&a.corners[0].limits, LimitKind::MaxCapacitance, &a);
    assert!(cap.contains(&"rr/Q".to_string()), "{cap:?}");
    assert!(!cap.contains(&"tl/ZN".to_string()), "{cap:?}");
}

// STA-REQ-0162: a clock gate output is the conjunction of the clock and enable inputs; the test
// enable, the statetable and the state function take no part. i0 (EN tied low, TE tied high) and
// i2 (clock tied low) give a constant 0: their CK -> GCK arc is disabled, the flops below are
// unclocked. i1 (EN tied high) passes the clock. (STA-REQ-0151 gave no clock gate output a value:
// i0/GCK and i2/GCK were live, checked for capacitance and fanout, and r0 was clocked.)
#[test]
fn clock_gate_outputs_follow_clock_and_enable() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output o0, output o1, output o2);\n\
        wire lo, hi, g0, g1, g2;\nTIEL tl (.ZN(lo));\nTIEH th (.Z(hi));\n\
        ICG i0 (.CK(clk), .EN(lo), .TE(hi), .GCK(g0));\nDFF r0 (.CK(g0), .D(a), .Q(o0));\n\
        ICG i1 (.CK(clk), .EN(hi), .TE(lo), .GCK(g1));\nDFF r1 (.CK(g1), .D(a), .Q(o1));\n\
        ICG i2 (.CK(lo), .EN(a), .TE(lo), .GCK(g2));\nDFF r2 (.CK(g2), .D(a), .Q(o2));\n\
        endmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_input_transition 0.2 [get_ports {clk a}]\n\
        set_input_delay 1 -clock c [get_ports a]\n\
        set_propagated_clock [get_clocks c]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    let cp = consts(&a, &lib);
    for (gate, val) in [
        ("i0/GCK", LogicVal::Zero),
        ("i1/GCK", LogicVal::X),
        ("i2/GCK", LogicVal::Zero),
    ] {
        assert_eq!(cp.node_val(node(&a, gate)), val, "{gate}");
    }
    assert_eq!(
        cell_arc_reason(&a, "i0/CK", "i0/GCK"),
        Some(DisableReason::Constant)
    );
    assert_eq!(cell_arc_reason(&a, "i1/CK", "i1/GCK"), None);
    for (ci, ch) in a.graph.checks().iter().enumerate() {
        let d = a.netview.node_name_str(ch.data_pin).into_owned();
        let status = cp.check_status(ci);
        match d.as_str() {
            "r0/D" | "r2/D" => assert_eq!(status, CheckStatus::Unclocked, "{d}"),
            "r1/D" => assert_eq!(status, CheckStatus::Active, "{d}"),
            _ => {}
        }
    }
    assert!(a.setup_slack_for("r0/D").is_none());
    assert!(a.setup_slack_for("r1/D").is_some());
    // The disabled CK -> GCK arc still carries its transition (STA-REQ-0117): i0/GCK has
    // 0.06 + 0.5 * 0.2 + 2 * 0.001 = 0.162, without an arrival.
    for tr in Tr::ALL {
        close(slew(&a, "i0/GCK", tr)[0], 0.162, "i0/GCK transition");
        assert_eq!(arrival(&a, "i0/GCK", tr, An::Late), None);
    }
}

// STA-REQ-0164: the port k is held by case analysis and has an input delay: k keeps its arrival
// (its own design rules see the clock), the wire to its load bk/A passes none. A case analysis
// on the output pin bb/Y stops the arc into it; one on the flop data pin rd/D stops the wire
// into it, and no check is timed there.
#[test]
fn constant_pins_pass_no_arrival() {
    let lib = lib();
    let netlist = "module top (input clk, input a, input k, output o1, output o2, output o3);\n\
        wire n1;\nBUF bk (.A(k), .Y(o1));\nBUF bb (.A(a), .Y(n1));\nBUF bc (.A(n1), .Y(o2));\n\
        DFF rd (.CK(clk), .D(a), .Q(o3));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_input_transition 0.2 [get_ports {clk a k}]\n\
        set_input_delay 1 -clock c [get_ports {a k}]\n\
        set_output_delay 0.5 -clock c [get_ports {o1 o2}]\n\
        set_case_analysis 1 [get_ports k]\n\
        set_case_analysis 0 [get_pins bb/Y]\n\
        set_case_analysis 0 [get_pins rd/D]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    for tr in Tr::ALL {
        close(
            arrival(&a, "k", tr, An::Late).expect("port"),
            1.0,
            "k arrival",
        );
        assert_eq!(arrival(&a, "bk/A", tr, An::Late), None, "bk/A");
        assert_eq!(arrival(&a, "bb/Y", tr, An::Late), None, "bb/Y");
        assert_eq!(arrival(&a, "rd/D", tr, An::Late), None, "rd/D");
        // The transitions still pass: bk/A has the port's 0.2; bb/Y 0.05 + 0.1 + 0.002.
        close(slew(&a, "bk/A", tr)[0], 0.2, "bk/A transition");
        close(slew(&a, "bb/Y", tr)[0], 0.152, "bb/Y transition");
    }
    assert_eq!(
        net_arc_reason(&a, "k", "bk/A"),
        Some(DisableReason::CaseAnalysis)
    );
    assert_eq!(
        cell_arc_reason(&a, "bb/A", "bb/Y"),
        Some(DisableReason::CaseAnalysis)
    );
    assert_eq!(
        net_arc_reason(&a, "a", "rd/D"),
        Some(DisableReason::CaseAnalysis)
    );
    assert!(a.setup_slack_for("o1").is_none());
    assert!(a.setup_slack_for("o2").is_none());
    assert!(a.setup_slack_for("rd/D").is_none());
    assert!(a.hold_slack_for("rd/D").is_none());
}

// STA-REQ-0714: a propagated clock gated by an AND2 whose enable comes from the flop ren. The
// transition of a clock pin merges the launched enable as in the data propagation:
// - clk (transition 0.2) -> cb/Y (loads ren/CK, cg/A: 0.002): transition 0.05 + 0.1 + 0.004 =
//   0.154, delay 0.1 + 0.1 + 0.004 = 0.204;
// - ren/Q (load cg/B): transition 0.08 + 0.5 * 0.154 + 0.002 = 0.159;
// - cg/Y (load cb2/A): from A 0.06 + 0.077 + 0.002 = 0.139, from B 0.07 + 0.0795 + 0.002 =
//   0.1515: late 0.1515, early 0.139; arrival 0.204 + 0.1 + 0.077 + 0.002 = 0.383;
// - cb2/Y (load rg/CK): late transition 0.05 + 0.07575 + 0.002 = 0.12775, late arrival
//   0.383 + 0.1 + 0.07575 + 0.002 = 0.56075 (without the enable: 0.5545 and 0.1215).
// Hold at rg/D (a: input delay 1): 1 - (0.56075 + 0.02 + 0.1 * 0.12775) = 0.406475.
// (STA-REQ-0116 seeded the clock propagation from the ports and ties only: 0.5545, 0.1215 and a
// hold slack of 1 - (0.5545 + 0.02 + 0.01215) = 0.41335, 6.9 ps optimistic.)
#[test]
fn launched_enable_merges_into_the_propagated_clock() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output o);\n\
        wire c1, en, gc, gc2;\nBUF cb (.A(clk), .Y(c1));\nDFF ren (.CK(c1), .D(a), .Q(en));\n\
        AND2 cg (.A(c1), .B(en), .Y(gc));\nBUF cb2 (.A(gc), .Y(gc2));\n\
        DFF rg (.CK(gc2), .D(a), .Q(o));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_propagated_clock [get_clocks c]\n\
        set_input_transition 0.2 [get_ports {clk a}]\n\
        set_input_delay 1 -clock c [get_ports a]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    let cm = &a.corners[0].clock_model;
    let late = cm
        .arrival(ClockId(0), Tr::Rise, node(&a, "rg/CK"), Tr::Rise, An::Late)
        .expect("clock at rg/CK");
    close(late.latency, 0.56075, "rg/CK late clock arrival");
    close(late.slew, 0.12775, "rg/CK late clock transition");
    close(
        a.hold_slack_for("rg/D").expect("hold"),
        0.406475,
        "rg/D hold slack",
    );
}

// STA-REQ-0822: an ideal clock stopped by the AND2 cg (B tied low). The pins that the clock
// reaches through the disabled arc (cg/Y, r/CK) belong to the ideal clock network: no design rule
// is checked there. The flop r launches no arrival; its output transition is its clock-to-output
// arc at the ideal clock transition 0.1 (load bq/A): 0.08 + 0.05 + 0.002 = 0.132 (the propagated
// cg/Y transition 0.112 would give 0.138). (STA-REQ-0818 left only the pins that the clock reaches
// through enabled arcs out: cg/Y and r/CK were checked as data pins.)
#[test]
fn ideal_clock_network_stopped_by_a_constant() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output o);\n\
        wire lo, gc, q;\nTIEL tl (.ZN(lo));\nAND2 cg (.A(clk), .B(lo), .Y(gc));\n\
        DFF r (.CK(gc), .D(a), .Q(q));\nBUF bq (.A(q), .Y(o));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_clock_transition 0.1 [get_clocks c]\n\
        set_input_transition 0.2 [get_ports {clk a}]\n\
        set_max_transition 0.0001 [current_design]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    let l = &a.corners[0].limits;
    let tr_rows = drv_pins(l, LimitKind::MaxTransition, &a);
    for p in ["cg/Y", "r/CK", "cg/A"] {
        assert!(!tr_rows.contains(&p.to_string()), "{p} in {tr_rows:?}");
    }
    assert!(tr_rows.contains(&"r/Q".to_string()), "{tr_rows:?}");
    assert!(l.ideal_clock_pins >= 3, "{}", l.ideal_clock_pins);
    for tr in Tr::ALL {
        let [late, early] = slew(&a, "r/Q", tr);
        close(late, 0.132, "r/Q late transition");
        close(early, 0.132, "r/Q early transition");
        assert_eq!(arrival(&a, "r/Q", tr, An::Late), None);
    }
}

// STA-REQ-0163, STA-REQ-0155: rw is a DFFRW whose setup and hold checks hold only `when : "RN"`;
// RN is tied low, so they are disabled (false_when) and not timed although the arrival of a
// reaches rw/D; the endpoint is counted as disabled. rr (no `when`) is timed. (STA-REQ-0161 did
// not time a check at a constant pin, but a check that a false `when` disabled was timed wherever
// arrivals reached both its pins.)
#[test]
fn false_when_checks_are_not_timed() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output o1, output o2);\n\
        wire lo;\nTIEL tl (.ZN(lo));\n\
        DFFRW rw (.CK(clk), .D(a), .RN(lo), .Q(o1));\n\
        DFFR rr (.CK(clk), .D(a), .RN(lo), .Q(o2));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_input_transition 0.2 [get_ports {clk a}]\n\
        set_input_delay 1 -clock c [get_ports a]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    for tr in Tr::ALL {
        close(
            arrival(&a, "rw/D", tr, An::Late).expect("arrival"),
            1.0,
            "rw/D arrival",
        );
    }
    assert!(a.setup_slack_for("rw/D").is_none());
    assert!(a.hold_slack_for("rw/D").is_none());
    assert!(a.setup_slack_for("rr/D").is_some());
    let ep = a.corners[0]
        .endpoint(node(&a, "rw/D"))
        .expect("rw/D endpoint");
    assert_eq!(ep.status.as_str(), "disabled");
}
