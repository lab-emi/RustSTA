// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Transitions next to logic constants (`STA-REQ-0117`, `STA-REQ-0159`), latches with a constant
//! enable (`STA-REQ-0163`, `STA-REQ-0230`) and the related pin transition of the timing checks
//! (`STA-REQ-0229`).
//!
//! The library has tables that are linear in the input transition `s` and the load `c`
//! (`v = base + 0.5 s + 2 c`, base per arc and transition, index points 0 and 1 by 0 and 0.1), so
//! that the bilinear evaluation is exact and every expected value follows by hand; the
//! derivations are in the comments of each test. Wires are ideal.

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::graph::{ArcKind, DisableReason};
use ruststa::engine::types::{An, Tr};

/// A combinational arc of `related` with the delay base `d` and transition base `t` (both
/// transitions), `v = base + 0.5 s + 2 c`.
fn arc(related: &str, sense: &str, d: f64, t: f64) -> String {
    let v = |b: f64| {
        format!(
            "values (\"{b}, {}\", \"{}, {}\");",
            b + 0.2,
            b + 0.5,
            b + 0.7
        )
    };
    format!(
        "timing () {{ related_pin : \"{related}\"; timing_sense : {sense};
        cell_rise (tr_t) {{ {} }} cell_fall (tr_t) {{ {} }}
        rise_transition (tr_t) {{ {} }} fall_transition (tr_t) {{ {} }} }}",
        v(d),
        v(d),
        v(t),
        v(t)
    )
}

fn lib() -> Library {
    let edge = |related: &str| {
        arc(related, "non_unate", 0.3, 0.08)
            .replace("timing_sense : non_unate;", "timing_type : rising_edge;")
    };
    let text = format!(
        r#"library (ct) {{
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
  cell (XOR2) {{ pin (A) {{ direction : input; capacitance : 0.001; }}
    pin (B) {{ direction : input; capacitance : 0.001; }}
    pin (Y) {{ direction : output; function : "A^B"; {xa_p} {xa_n} {xb_p} {xb_n} }} }}
  cell (LAT) {{ latch (IQ, IQN) {{ enable : "G"; data_in : "D"; }}
    pin (G) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001;
      timing () {{ related_pin : "G"; timing_type : setup_falling;
        rise_constraint (scalar) {{ values ("0.05"); }} fall_constraint (scalar) {{ values ("0.05"); }} }}
      timing () {{ related_pin : "G"; timing_type : hold_falling;
        rise_constraint (scalar) {{ values ("0.02"); }} fall_constraint (scalar) {{ values ("0.02"); }} }} }}
    pin (Q) {{ direction : output; function : "IQ"; {lat_d} {lat_g} }} }}
  cell (LATI) {{ latch (IQ, IQN) {{ enable : "G"; data_in : "!D"; }}
    pin (G) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001;
      timing () {{ related_pin : "G"; timing_type : setup_falling;
        rise_constraint (scalar) {{ values ("0.05"); }} fall_constraint (scalar) {{ values ("0.05"); }} }} }}
    pin (Q) {{ direction : output; function : "IQN"; {lat_d} {lat_g} }} }}
  cell (DFF) {{ ff (IQ, IQN) {{ next_state : "D"; clocked_on : "CK"; }}
    pin (CK) {{ direction : input; clock : true; capacitance : 0.001; }}
    pin (D) {{ direction : input; capacitance : 0.001;
      timing () {{ related_pin : "CK"; timing_type : setup_rising;
        rise_constraint (ck_t) {{ values ("0.05, 0.05", "0.15, 0.15"); }}
        fall_constraint (ck_t) {{ values ("0.05, 0.05", "0.15, 0.15"); }} }}
      timing () {{ related_pin : "CK"; timing_type : hold_rising;
        rise_constraint (ck_t) {{ values ("0.02, 0.02", "0.12, 0.12"); }}
        fall_constraint (ck_t) {{ values ("0.02, 0.02", "0.12, 0.12"); }} }} }}
    pin (Q) {{ direction : output; function : "IQ"; {dff_ck} }} }}
}}
"#,
        buf = arc("A", "positive_unate", 0.1, 0.05),
        and_a = arc("A", "positive_unate", 0.1, 0.06),
        and_b = arc("B", "positive_unate", 0.1, 0.07),
        xa_p = arc("A", "positive_unate", 0.5, 0.3),
        xa_n = arc("A", "negative_unate", 0.1, 0.05),
        xb_p = arc("B", "positive_unate", 0.5, 0.3),
        xb_n = arc("B", "negative_unate", 0.1, 0.05),
        lat_d = arc("D", "positive_unate", 0.2, 0.05),
        lat_g = edge("G"),
        dff_ck = edge("CK"),
    );
    read_text(&text, "ct.lib").expect("synthetic library")
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

/// `[late, early]` transition of `pin` for `tr`.
fn slew(a: &EngineAnalysis<'_>, pin: &str, tr: Tr) -> [f64; 2] {
    let n = a.find_node(pin).unwrap_or_else(|| panic!("no pin {pin}"));
    [
        a.corners[0].slew(n, tr, An::Late),
        a.corners[0].slew(n, tr, An::Early),
    ]
}

fn late_arrival(a: &EngineAnalysis<'_>, pin: &str, tr: Tr) -> Option<f64> {
    let n = a.find_node(pin).unwrap_or_else(|| panic!("no pin {pin}"));
    a.corners[0].worst_arrival(n, tr, An::Late).map(|(_, x)| x)
}

fn early_arrival(a: &EngineAnalysis<'_>, pin: &str, tr: Tr) -> Option<f64> {
    let n = a.find_node(pin).unwrap_or_else(|| panic!("no pin {pin}"));
    a.corners[0].worst_arrival(n, tr, An::Early).map(|(_, x)| x)
}

const NETLIST: &str = "module top (input clk, input a, input k, output q1, output q2, output q3, \
    output q4);\n\
    wire hi, lo, n1, n2, n3;\n\
    TIEH th (.Z(hi));\nTIEL tl (.ZN(lo));\n\
    AND2 g1 (.A(k), .B(a), .Y(n1));\nDFF r1 (.CK(clk), .D(n1), .Q(q1));\n\
    XOR2 x1 (.A(hi), .B(a), .Y(n2));\nDFF r2 (.CK(clk), .D(n2), .Q(q2));\n\
    DFF rt (.CK(lo), .D(a), .Q(n3));\nAND2 g3 (.A(n3), .B(a), .Y(q3));\n\
    BUF bq (.A(q1), .Y(q4));\nendmodule\n";

const SDC: &str = "create_clock -name c -period 10 [get_ports clk]\n\
    set_input_transition 0.2 [get_ports {clk a}]\nset_input_transition 0.3 [get_ports k]\n\
    set_input_delay 1 -clock c [get_ports a]\nset_case_analysis 1 [get_ports k]\n";

// The transition tables of an arc are equal for both output transitions, so every expected value
// holds for rise and fall.
//
// STA-REQ-0117: the delay calculation ignores logic values. g1 is an AND2 whose input A is held
// at 1 by case analysis (k, transition 0.3) and whose input B carries a (transition 0.2); its load
// is r1/D (0.001 pF). The arc A -> Y carries no arrival but its transition: late rise
// 0.06 + 0.5 * 0.3 + 2 * 0.001 = 0.212 (the arc B -> Y alone gives 0.07 + 0.1 + 0.002 = 0.172),
// early the smaller 0.172. The arrival passes B only: 1 + 0.1 + 0.5 * 0.2 + 0.002 = 1.202.
//
// STA-REQ-0159: x1 is an XOR2 with A tied high, so Y = !B: its positive unate arc B -> Y cannot
// switch Y and is disabled (constant); the arrival passes the negative unate arc only:
// 1 + 0.1 + 0.1 + 0.002 = 1.202 in both transitions (the positive arc would give 1.602). The
// transition of Y merges every arc with its library sense, the disabled ones included: late
// 0.3 + 0.1 + 0.002 = 0.402 (B positive at 0.2), early 0.05 + 0 + 0.002 = 0.052 (A negative at
// the tie transition 0).
//
// STA-REQ-0117: rt is clocked by the tie-low net: its clock-to-output arc launches no arrival but
// carries the tie transition 0 to Q into g3/A (0.001): 0.08 + 0.002 = 0.082; g3/Y (no load) then
// merges the arc from A at 0.082 (0.06 + 0.041 = 0.101) with the arc from B at 0.2 (0.07 + 0.1 =
// 0.17): late 0.17, early 0.101.
#[test]
fn constant_side_inputs_carry_transitions_not_arrivals() {
    let lib = lib();
    let frozen = freeze(NETLIST, &lib);
    let a = analyze(&frozen, &lib, SDC);

    for tr in Tr::ALL {
        let [late, early] = slew(&a, "g1/Y", tr);
        close(late, 0.212, "g1/Y late transition");
        close(early, 0.172, "g1/Y early transition");
        close(
            late_arrival(&a, "g1/Y", tr).expect("arrival"),
            1.202,
            "g1/Y late arrival",
        );
    }
    let [k_late, _] = slew(&a, "k", Tr::Rise);
    close(k_late, 0.3, "case analysis keeps the port transition");
    assert_eq!(late_arrival(&a, "k", Tr::Rise), None);

    let x1 = a.find_node("x1/B").expect("x1/B");
    let y = a.find_node("x1/Y").expect("x1/Y");
    let reasons: Vec<(bool, Option<DisableReason>)> = a
        .graph
        .arcs()
        .iter()
        .enumerate()
        .filter(|(_, g)| g.src == x1 && g.dst == y && matches!(g.kind, ArcKind::Comb { .. }))
        .map(|(i, g)| {
            (
                g.sense == ruststa::engine::graph::ArcSense::PositiveUnate,
                a.graph.arc_disable_reason(i),
            )
        })
        .collect();
    assert!(
        reasons.contains(&(true, Some(DisableReason::Constant))),
        "{reasons:?}"
    );
    assert!(reasons.contains(&(false, None)), "{reasons:?}");
    for tr in Tr::ALL {
        close(
            late_arrival(&a, "x1/Y", tr).expect("arrival"),
            1.202,
            "x1/Y late arrival",
        );
        let [late, early] = slew(&a, "x1/Y", tr);
        close(late, 0.402, "x1/Y late transition");
        close(early, 0.052, "x1/Y early transition");
    }

    for tr in Tr::ALL {
        let [late, early] = slew(&a, "rt/Q", tr);
        close(late, 0.082, "rt/Q transition");
        close(early, 0.082, "rt/Q early transition");
        assert_eq!(
            late_arrival(&a, "rt/Q", tr),
            None,
            "rt/Q launches no arrival"
        );
        let [late, early] = slew(&a, "g3/Y", tr);
        close(late, 0.17, "g3/Y late transition");
        close(early, 0.101, "g3/Y early transition");
    }
}

// STA-REQ-0163: lt is a LAT whose enable G is tied high, so it is transparent: the late arrival of
// a passes D -> Q in its own tag (1 + 0.2 + 0.1 + 2 * 0.001 = 1.302 at lt/Q, load bo/A), no early
// arrival passes, and the output o1 has a setup check and no hold check. lo is a LAT whose enable
// is tied low: opaque, its Q carries the D -> Q transition (0.05 + 0.1 + 0.002 = 0.152) and no
// arrival. STA-REQ-0230: li is a LATI whose latch group stores the inverted data pin; it is
// timed with the positive unate sense of its arc and reported with note T0150.
#[test]
fn latches_with_a_constant_enable() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output o1, output o2, output o3);\n\
        wire hi, lo, n1, n2, n3;\n\
        TIEH th (.Z(hi));\nTIEL tl (.ZN(lo));\n\
        LAT lt (.D(a), .G(hi), .Q(n1));\nBUF bo (.A(n1), .Y(o1));\n\
        LAT lz (.D(a), .G(lo), .Q(n2));\nBUF bp (.A(n2), .Y(o2));\n\
        LATI li (.D(a), .G(clk), .Q(n3));\nBUF bi (.A(n3), .Y(o3));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_input_transition 0.2 [get_ports {clk a}]\n\
        set_input_delay 1 -clock c [get_ports a]\n\
        set_output_delay 0.5 -clock c [get_ports {o1 o2 o3}]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    assert!(
        a.graph
            .transparent_latch_arcs()
            .iter()
            .any(|&ai| a.graph.arcs()[ai as usize].dst == a.find_node("lt/Q").expect("lt/Q")),
        "lt is transparent"
    );
    for tr in Tr::ALL {
        close(
            late_arrival(&a, "lt/Q", tr).expect("late arrival through lt"),
            1.302,
            "lt/Q late arrival",
        );
        assert_eq!(
            early_arrival(&a, "lt/Q", tr),
            None,
            "no hold path passes lt"
        );
        let [late, _] = slew(&a, "lz/Q", tr);
        close(late, 0.152, "lz/Q transition");
        assert_eq!(late_arrival(&a, "lz/Q", tr), None, "lz is opaque");
    }
    assert!(a.setup_slack_for("o1").is_some());
    assert!(a.hold_slack_for("o1").is_none());
    assert!(a.setup_slack_for("o2").is_none());
    let t0150: Vec<&str> = a
        .diags
        .iter()
        .filter(|d| d.code == "T0150")
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(t0150.len(), 1, "{t0150:?}");
    assert!(t0150[0].contains("LATI"), "{}", t0150[0]);
    assert!(!t0150[0].contains("LAT,"), "{}", t0150[0]);
}

// STA-REQ-0229: the related pin transition of a check is the one of the capture clock path: the
// early transition (set_clock_transition -min 0.1) for setup, the late one (-max 0.5) for hold.
// r1 captures a (arrival 1 + 0, transition 0.2): setup 0.05 + 0.1 * 0.1 = 0.06, slack
// 10 - 0.06 - 1 = 8.94; hold 0.02 + 0.1 * 0.5 = 0.07, slack 1 - 0.07 = 0.93.
#[test]
fn checks_use_the_capture_clock_path_transition() {
    let lib = lib();
    let netlist = "module top (input clk, input a, output q);\n\
        DFF r1 (.CK(clk), .D(a), .Q(q));\nendmodule\n";
    let sdc = "create_clock -name c -period 10 [get_ports clk]\n\
        set_clock_transition -min 0.1 [get_clocks c]\n\
        set_clock_transition -max 0.5 [get_clocks c]\n\
        set_input_transition 0.2 [get_ports a]\n\
        set_input_delay 1 -clock c [get_ports a]\n";
    let frozen = freeze(netlist, &lib);
    let a = analyze(&frozen, &lib, sdc);
    close(
        a.setup_slack_for("r1/D").expect("setup"),
        8.94,
        "setup slack",
    );
    close(a.hold_slack_for("r1/D").expect("hold"), 0.93, "hold slack");
}
