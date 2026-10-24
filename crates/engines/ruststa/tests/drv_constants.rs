// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Design rules on constant nets, on unclocked sequential outputs and on untimed pins
//! (`STA-REQ-0809`), on unconnected instance pins (`STA-REQ-0819`), with the limits table and the
//! statuses that carry their counts (`STA-REQ-0806`, `STA-REQ-0813`).
//!
//! The library has transition tables that are linear in the input transition `s` and the load
//! `c` (`t = base + 0.5 s + 2 c`, base per arc and transition), so that the bilinear table
//! evaluation is exact and every expected value follows by hand; the derivations are in the
//! comments of each test. Wires are ideal.

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_workspace::rbin::Rbin;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::limits::status::{CheckStatus, limit_statuses};
use ruststa::engine::limits::table::{LimitsData, LimitsTable};
use ruststa::engine::limits::{CornerLimits, LimitKind, LimitRow};
use ruststa::engine::store::from_analysis::corner_limits_report;
use ruststa::engine::types::Tr;

const LIB: &str = r#"library (cst) {
  time_unit : "1ns"; capacitive_load_unit (1, pf); default_fanout_load : 1;
  lu_table_template (tr_t) { variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance; index_1 ("0, 1"); index_2 ("0, 0.1"); }
  cell (TIEH) { pin (Z) { direction : output; function : "1"; max_capacitance : 0.001; } }
  cell (TIEL) { pin (ZN) { direction : output; function : "0"; } }
  cell (TIE2) { pin (HI) { direction : output; function : "1"; }
    pin (LO) { direction : output; function : "0"; } }
  cell (BUF) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Y) { direction : output; function : "A";
      timing () { related_pin : "A"; timing_sense : positive_unate;
        cell_rise (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        cell_fall (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        rise_transition (tr_t) { values ("0.05, 0.25", "0.55, 0.75"); }
        fall_transition (tr_t) { values ("0.04, 0.24", "0.54, 0.74"); } } }
  }
  cell (AND2) {
    pin (A) { direction : input; capacitance : 0.001; }
    pin (B) { direction : input; capacitance : 0.001; }
    pin (Y) { direction : output; function : "A&B";
      timing () { related_pin : "A"; timing_sense : positive_unate;
        cell_rise (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        cell_fall (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        rise_transition (tr_t) { values ("0.06, 0.26", "0.56, 0.76"); }
        fall_transition (tr_t) { values ("0.05, 0.25", "0.55, 0.75"); } }
      timing () { related_pin : "B"; timing_sense : positive_unate;
        cell_rise (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        cell_fall (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        rise_transition (tr_t) { values ("0.07, 0.27", "0.57, 0.77"); }
        fall_transition (tr_t) { values ("0.06, 0.26", "0.56, 0.76"); } } }
  }
  cell (DFF) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001; }
    pin (D) { direction : input; capacitance : 0.001;
      timing () { related_pin : "CK"; timing_type : setup_rising;
        rise_constraint (scalar) { values ("0.05"); } fall_constraint (scalar) { values ("0.05"); } }
      timing () { related_pin : "CK"; timing_type : hold_rising;
        rise_constraint (scalar) { values ("0.02"); } fall_constraint (scalar) { values ("0.02"); } } }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        cell_fall (tr_t) { values ("0.1, 0.3", "0.6, 0.8"); }
        rise_transition (tr_t) { values ("0.08, 0.28", "0.58, 0.78"); }
        fall_transition (tr_t) { values ("0.07, 0.27", "0.57, 0.77"); } } }
  }
}
"#;

const NETLIST: &str = "module top (input clk, input a, input k, output q0, output q1, \
    output q2, output q3, output q4, output y);\n\
    wire c1, c0, n0, n1, n2, nx;\n\
    TIEH th (.Z(c1));\nBUF b0 (.A(c1), .Y(n0));\nDFF r0 (.CK(clk), .D(n0), .Q(q0));\n\
    BUF b1 (.A(c1), .Y(y));\n\
    TIEL tl (.ZN(c0));\nAND2 g (.A(c0), .B(a), .Y(n1));\nDFF r1 (.CK(clk), .D(n1), .Q(q1));\n\
    BUF l1 (.A(1'b1), .Y(n2));\nDFF r2 (.CK(clk), .D(n2), .Q(q2));\n\
    DFF r3 (.CK(k), .D(a), .Q(q3));\n\
    BUF bx (.A(a), .Y(nx));\nDFF r4 (.CK(clk), .D(nx), .Q(q4));\nendmodule\n";

const SDC: &str = "create_clock -name c -period 10 [get_ports clk]\nset_propagated_clock [get_clocks c]\n\
    set_input_transition 0.2 [get_ports {clk a}]\nset_input_transition 0.3 [get_ports k]\n\
    set_input_delay 1 -clock c [get_ports a]\nset_load 0.01 [get_ports {q0 q1 q2 q3 q4 y}]\n\
    set_max_fanout 1 [current_design]\nset_max_capacitance 0.0001 [current_design]\n\
    set_max_transition 0.001 [current_design]\n";

fn lib() -> Library {
    read_text(LIB, "cst.lib").expect("synthetic library")
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

fn rows<'l>(
    l: &'l CornerLimits,
    kind: LimitKind,
    a: &'l EngineAnalysis<'_>,
) -> impl Iterator<Item = (String, &'l LimitRow)> + 'l {
    l.rows_of(kind)
        .map(move |r| (a.netview.node_name_str(r.node).into_owned(), r))
}

fn names(
    l: &CornerLimits,
    kind: LimitKind,
    a: &EngineAnalysis<'_>,
    violating: bool,
) -> Vec<String> {
    let mut v: Vec<String> = rows(l, kind, a)
        .filter(|(_, r)| !violating || r.slack < 0.0)
        .map(|(n, _)| n)
        .collect();
    v.sort();
    v
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-12, "{what}: {a} vs {b}");
}

// STA-REQ-0809: the drivers of constant nets (the tie outputs th/Z and tl/ZN, the buffers b0, b1
// of the tie-high net, the gate g whose input A is the controlling 0, and the buffer l1 of the
// literal 1'b1) are not checked for capacitance and fanout: 6 drivers, note T0143. The delay
// calculation ignores logic values (STA-REQ-0116), so the propagation gives the constant nets
// their transitions, without arrivals:
// - th/Z and tl/ZN: tie outputs, 0; the pins of their nets (b0/A, b1/A, g/A) 0 by ideal wire;
// - b0/Y = r0/D: rise 0.05 + 0.5 * 0 + 2 * 0.001 (r0/D) = 0.052, fall 0.042;
// - b1/Y = y: the port load 0.01: rise 0.07, fall 0.06;
// - g/Y = r1/D: the larger of the arc from A (s = 0: rise 0.06 + 0.002) and from B (the port
//   transition 0.2: rise 0.07 + 0.1 + 0.002 = 0.172; fall 0.06 + 0.1 + 0.002 = 0.162);
// - r3/Q = q3: no clock reaches r3/CK (k is not a clock), so the clock-to-output arc carries the
//   transition of k (0.3) into the port load 0.01: rise 0.08 + 0.15 + 0.02 = 0.25, fall 0.24.
// Only the literal and its cone (l1/A, l1/Y and r2/D) are reached by no transition: they never
// switch and are checked with the static transition 0 (3 pins).
// With the design limit 0.001 every pin with a positive transition violates and the zero pins
// meet it (slack 0.001). Nothing is untimed.
#[test]
fn constant_nets_unclocked_outputs_and_literals() {
    let lib = lib();
    let frozen = freeze(NETLIST, &lib);
    let a = analyze(&frozen, &lib, SDC);
    let l = &a.corners[0].limits;
    assert_eq!(l.constant_drivers, 6);
    assert_eq!(l.static_transition_pins, 3);
    assert_eq!(l.untimed_pins, 0);
    assert_eq!(l.ideal_clock_pins, 0);
    let codes: Vec<&str> = a.diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"T0143"), "{codes:?}");
    assert!(!codes.contains(&"T0145"), "{codes:?}");

    let constant_drivers = ["b0/Y", "b1/Y", "g/Y", "l1/Y", "th/Z", "tl/ZN"];
    for kind in [LimitKind::MaxCapacitance, LimitKind::MaxFanout] {
        let checked = names(l, kind, &a, false);
        assert!(
            !checked
                .iter()
                .any(|n| constant_drivers.contains(&n.as_str())),
            "{} {checked:?}",
            kind.name()
        );
    }
    // clk drives three clock pins and a drives g/B, r3/D and bx/A: fanout over 1.
    assert_eq!(names(l, LimitKind::MaxFanout, &a, true), ["a", "clk"]);

    let tr = |pin: &str| -> (f64, Option<Tr>) {
        let (_, r) = rows(l, LimitKind::MaxTransition, &a)
            .find(|(n, _)| n == pin)
            .unwrap_or_else(|| panic!("no transition row for {pin}"));
        (r.value, r.tr)
    };
    for (pin, value) in [
        ("th/Z", 0.0),
        ("tl/ZN", 0.0),
        ("b0/A", 0.0),
        ("g/A", 0.0),
        ("b0/Y", 0.052),
        ("r0/D", 0.052),
        ("b1/Y", 0.07),
        ("y", 0.07),
        ("g/Y", 0.172),
        ("r1/D", 0.172),
        ("l1/A", 0.0),
        ("l1/Y", 0.0),
        ("r2/D", 0.0),
        ("r3/Q", 0.25),
        ("q3", 0.25),
    ] {
        let (v, t) = tr(pin);
        close(v, value, pin);
        if value > 0.0 {
            assert_eq!(
                t,
                Some(Tr::Rise),
                "{pin}: the rise transition has the worst slack"
            );
        }
    }
    let viol = names(l, LimitKind::MaxTransition, &a, true);
    for pin in ["b0/Y", "r0/D", "b1/Y", "y", "g/Y", "r1/D", "r3/Q", "q3"] {
        assert!(viol.iter().any(|v| v == pin), "{pin} violates: {viol:?}");
    }
    for pin in [
        "th/Z", "tl/ZN", "b0/A", "b1/A", "g/A", "l1/A", "l1/Y", "r2/D",
    ] {
        assert!(!viol.iter().any(|v| v == pin), "{pin} meets the limit");
    }
    assert_eq!(l.kind(LimitKind::MaxTransition).unevaluated, 0);

    // The counts reach the scope and the limits table (STA-REQ-0806, STA-REQ-0807).
    let report = corner_limits_report(&a.corners[0]);
    assert_eq!(
        (
            report.constant_drivers,
            report.static_transition_pins,
            report.untimed_pins
        ),
        (6, 3, 0)
    );
    let bytes = LimitsData::from_analysis(&a, 100)
        .to_bytes()
        .expect("bytes");
    let t = LimitsTable::from_rbin(Rbin::from_bytes(bytes, "LIMITS", 1..=1).expect("rbin"))
        .expect("table");
    assert_eq!(
        t.drv_pin_counts().expect("counts"),
        vec![[6, 3, 0, 0, 0, 0]]
    );
}

// STA-REQ-0809, STA-REQ-0813: an arc disabled by set_disable_timing leaves bx/Y and r4/D without
// any transition (they are not constant and nothing evaluates the arc): two untimed pins whose
// limited transition checks are unevaluated, a warning T0145 (not T0137), and the rule is partial
// although nothing violates under a loose limit.
#[test]
fn untimed_pins_are_counted_and_not_evaluated() {
    let lib = lib();
    let frozen = freeze(NETLIST, &lib);
    let sdc = SDC
        .replace("set_max_transition 0.001", "set_max_transition 5")
        .replace("set_max_fanout 1", "set_max_fanout 10")
        .replace("set_max_capacitance 0.0001", "set_max_capacitance 1")
        + "set_disable_timing -from A -to Y [get_cells bx]\n";
    let a = analyze(&frozen, &lib, &sdc);
    let l = &a.corners[0].limits;
    assert_eq!(l.untimed_pins, 2);
    assert_eq!(l.kind(LimitKind::MaxTransition).unevaluated, 2);
    assert_eq!(l.kind(LimitKind::MaxTransition).violating, 0);
    let checked = names(l, LimitKind::MaxTransition, &a, false);
    assert!(
        !checked.iter().any(|n| n == "bx/Y" || n == "r4/D"),
        "{checked:?}"
    );
    let codes: Vec<&str> = a.diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"T0145"), "{codes:?}");
    assert!(!codes.contains(&"T0137"), "{codes:?}");
    let report = corner_limits_report(&a.corners[0]);
    let s = limit_statuses(Some(&[&report]));
    assert_eq!(s.drv[0].1, CheckStatus::Partial);
    assert_eq!(s.drv_status, CheckStatus::Partial);
}

// STA-REQ-0819: library pins that the netlist leaves unconnected are not pins of the design: the
// unused output HI of the two-output tie t2, the output Q of r5 written as `.Q()` and the output
// of b9 that is not listed are not checked (no row, nothing unevaluated), they are counted (3),
// and a note T0148 names them. Their rule stays met under loose limits: they drive no net.
#[test]
fn unconnected_output_pins_are_counted_not_checked() {
    let lib = lib();
    let netlist = NETLIST
        .replace("output y);", "output y, output q5);")
        .replace(
            "endmodule",
            "wire c3;\nTIE2 t2 (.LO(c3));\nBUF b8 (.A(c3), .Y(q5));\n\
             DFF r5 (.CK(clk), .D(a), .Q());\nBUF b9 (.A(a));\nendmodule",
        );
    let frozen = freeze(&netlist, &lib);
    let sdc = SDC
        .replace("set_max_transition 0.001", "set_max_transition 5")
        .replace("set_max_fanout 1", "set_max_fanout 10")
        .replace("set_max_capacitance 0.0001", "set_max_capacitance 1")
        .replace("{q0 q1 q2 q3 q4 y}", "{q0 q1 q2 q3 q4 y q5}");
    let a = analyze(&frozen, &lib, &sdc);
    let l = &a.corners[0].limits;
    assert_eq!(l.unconnected_output_pins, 3);
    for kind in [
        LimitKind::MaxTransition,
        LimitKind::MaxCapacitance,
        LimitKind::MaxFanout,
    ] {
        let checked = names(l, kind, &a, false);
        for pin in ["t2/HI", "r5/Q", "b9/Y"] {
            assert!(!checked.iter().any(|n| n == pin), "{pin}: {checked:?}");
        }
        assert_eq!(l.kind(kind).unevaluated, 0, "{kind:?}");
    }
    let t0148: Vec<&str> = a
        .diags
        .iter()
        .filter(|d| d.code == "T0148")
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(t0148.len(), 1, "{t0148:?}");
    for pin in ["t2/HI", "r5/Q", "b9/Y"] {
        assert!(t0148[0].contains(pin), "{pin}: {}", t0148[0]);
    }
    let report = corner_limits_report(&a.corners[0]);
    assert_eq!(report.unconnected_output_pins, 3);
    let s = limit_statuses(Some(&[&report]));
    assert_eq!(s.drv_status, CheckStatus::Met);
}
