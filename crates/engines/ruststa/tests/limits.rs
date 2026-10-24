// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Pin limit checks: minimum pulse width, minimum period and the electrical design rules
//! (`STA-REQ-0800` to `STA-REQ-0810`, `STA-REQ-0815`, with the pin pulse attributes of
//! LIBERTY-REQ-0049), and per-corner extracted parasitics (`STA-REQ-0820`).
//!
//! The library has constant tables (one table with a related-pin-transition axis) and ideal
//! clocks, so that every expected value follows by hand from the specification
//! (`docs/specs/sta/limits.md`); the derivations are in the comments of each test.

use std::sync::Arc;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::spef;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::delay::spef::ParasiticInput;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::limits::table::LimitsData;
use ruststa::engine::limits::{ClockLimitScope, CornerLimits, LimitKind, LimitRow, LimitSource};
use ruststa::engine::types::Tr;

const LIB: &str = r#"library (sig) {
  time_unit : "1ns"; capacitive_load_unit (1, pf);
  default_max_fanout : 4; default_fanout_load : 1;
  lu_table_template (mpw_t) { variable_1 : related_pin_transition; index_1 ("0.1, 0.5"); }
  cell (BUF) {
    pin (A) { direction : input; capacitance : 0.002; fanout_load : 2; max_transition : 0.5; }
    pin (Y) { direction : output; function : "A"; max_capacitance : 0.009;
      timing () { related_pin : "A"; timing_sense : positive_unate;
        cell_rise (scalar) { values ("0.1"); } cell_fall (scalar) { values ("0.1"); }
        rise_transition (scalar) { values ("0.2"); } fall_transition (scalar) { values ("0.2"); } } }
  }
  cell (INV) {
    pin (A) { direction : input; capacitance : 0.001; }
    pin (Y) { direction : output; function : "!A";
      timing () { related_pin : "A"; timing_sense : negative_unate;
        cell_rise (scalar) { values ("0.05"); } cell_fall (scalar) { values ("0.05"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
  cell (DFF) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001;
      timing () { related_pin : "CK"; timing_type : min_pulse_width;
        rise_constraint (mpw_t) { index_1 ("0.1, 0.5"); values ("0.3, 0.7"); }
        fall_constraint (scalar) { values ("0.5"); } }
      timing () { related_pin : "CK"; timing_type : minimum_period; when : "D";
        rise_constraint (scalar) { values ("1.2"); } }
      timing () { related_pin : "CK"; timing_type : minimum_period; when : "!D";
        rise_constraint (scalar) { values ("1.5"); } } }
    pin (D) { direction : input; capacitance : 0.001;
      timing () { related_pin : "CK"; timing_type : setup_rising;
        rise_constraint (scalar) { values ("0.05"); } fall_constraint (scalar) { values ("0.05"); } }
      timing () { related_pin : "CK"; timing_type : hold_rising;
        rise_constraint (scalar) { values ("0.02"); } fall_constraint (scalar) { values ("0.02"); } } }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
}
"#;

fn lib() -> Library {
    read_text(LIB, "sig.lib").expect("synthetic library")
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

fn analyze<'a>(
    frozen: &'a FrozenNetlist<'a>,
    libs: &[Library],
    sdc: &str,
    options: &AnalysisOptions,
) -> EngineAnalysis<'a> {
    EngineAnalysis::analyze_single_corner(frozen, libs, sdc, options).expect("analysis")
}

fn row<'l>(
    l: &'l CornerLimits,
    kind: LimitKind,
    pin: &str,
    a: &EngineAnalysis<'_>,
) -> &'l LimitRow {
    l.rows_of(kind)
        .find(|r| a.netview.node_name_str(r.node) == pin)
        .unwrap_or_else(|| panic!("no {} row for {pin}", kind.name()))
}

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() < 1e-12, "{what}: {a} vs {b}");
}

const PULSE_NETLIST: &str = "module top (input clk, input d, output q1, output q2);\n\
    wire nck;\n\
    DFF f1 (.CK(clk), .D(d), .Q(q1));\n\
    INV i1 (.A(clk), .Y(nck));\n\
    DFF f2 (.CK(nck), .D(d), .Q(q2));\nendmodule\n";

const PULSE_SDC: &str = "create_clock -name c -period 10 -waveform {0 4} [get_ports clk]\n\
    set_clock_latency -min 0.1 [get_clocks c]\nset_clock_latency -max 0.3 [get_clocks c]\n\
    set_clock_transition 0.2 [get_clocks c]\n\
    set_input_delay -clock c 1 [get_ports d]\nset_output_delay -clock c 1 [get_ports {q1 q2}]\n";

// STA-REQ-0800, STA-REQ-0801: pulse widths with the latest opening and the earliest closing
// arrival, required widths at the opening transition time, and the worst conditional period.
// Ideal clock c: period 10, rise 0, fall 4, latency early 0.1 / late 0.3, transition 0.2.
// f1/CK (positive sense): high = (4 + 0.1) - (0 + 0.3) = 3.8, required = 0.3 + (0.2 - 0.1) / 0.4 *
// 0.4 = 0.4, slack 3.4; low = (10 + 0.1) - (4 + 0.3) = 5.8, required 0.5, slack 5.3.
// f2/CK (through an inverter): the pin rises at the source fall, so high = 5.8 (slack 5.4) and
// low = 3.8 (slack 3.3). Period: max(1.2, 1.5) = 1.5 over the two `when` groups, slack 8.5.
#[test]
fn pulse_widths_and_periods_follow_the_clock_waveform() {
    let lib = lib();
    let frozen = freeze(PULSE_NETLIST, &lib);
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        PULSE_SDC,
        &AnalysisOptions::ideal(),
    );
    let c = &a.corners[0];
    let l = &c.limits;
    assert!(
        c.unanalyzed_arcs == 0,
        "pulse groups are checks, not unanalyzed arcs"
    );
    for (pin, kind, value, limit, slack, tr) in [
        (
            "f1/CK",
            LimitKind::MinPulseWidthHigh,
            3.8,
            0.4,
            3.4,
            Tr::Rise,
        ),
        (
            "f1/CK",
            LimitKind::MinPulseWidthLow,
            5.8,
            0.5,
            5.3,
            Tr::Fall,
        ),
        (
            "f2/CK",
            LimitKind::MinPulseWidthHigh,
            5.8,
            0.4,
            5.4,
            Tr::Rise,
        ),
        (
            "f2/CK",
            LimitKind::MinPulseWidthLow,
            3.8,
            0.5,
            3.3,
            Tr::Fall,
        ),
        ("f1/CK", LimitKind::MinPeriod, 10.0, 1.5, 8.5, Tr::Rise),
        ("f2/CK", LimitKind::MinPeriod, 10.0, 1.5, 8.5, Tr::Rise),
    ] {
        let r = row(l, kind, pin, &a);
        close(r.value, value, &format!("{pin} {} value", kind.name()));
        close(r.limit, limit, &format!("{pin} {} limit", kind.name()));
        close(r.slack, slack, &format!("{pin} {} slack", kind.name()));
        assert_eq!(r.tr, Some(tr));
        assert_eq!(r.source, LimitSource::Liberty);
        assert!(r.clock.is_some());
    }
    assert_eq!(l.kind(LimitKind::MinPulseWidthHigh).checked, 2);
    assert_eq!(l.kind(LimitKind::MinPeriod).violating, 0);
    assert_eq!(l.conditional_pulse_groups, 4);
    assert_eq!(l.unclocked_pulse_pins, 0);
    assert!(a.diags.iter().any(|d| d.code == "T0132"));
    // Kept rows are ordered by kind, then slack.
    let lows: Vec<f64> = l
        .rows_of(LimitKind::MinPulseWidthLow)
        .map(|r| r.slack)
        .collect();
    assert!(lows.windows(2).all(|w| w[0] <= w[1]));
}

// STA-REQ-0800: a narrow high pulse violates; the slack is negative and counted.
#[test]
fn narrow_pulse_violates() {
    let lib = lib();
    let frozen = freeze(PULSE_NETLIST, &lib);
    let sdc = PULSE_SDC.replace("-waveform {0 4}", "-waveform {0 0.5}");
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        &sdc,
        &AnalysisOptions::ideal(),
    );
    let l = &a.corners[0].limits;
    // f1/CK high = (0.5 + 0.1) - (0 + 0.3) = 0.3 < 0.4: slack -0.1.
    let r = row(l, LimitKind::MinPulseWidthHigh, "f1/CK", &a);
    close(r.slack, -0.1, "narrow high pulse");
    assert_eq!(l.kind(LimitKind::MinPulseWidthHigh).violating, 1);
    // f2/CK: its low pulse is the source high pulse, 0.3 < 0.5: slack -0.2.
    close(
        row(l, LimitKind::MinPulseWidthLow, "f2/CK", &a).slack,
        -0.2,
        "inverted low pulse",
    );
}

// STA-REQ-0802: a pulse pin reached by no clock is counted as unchecked, with T0133.
#[test]
fn unclocked_pulse_pins_are_reported_unchecked() {
    let lib = lib();
    let frozen = freeze(PULSE_NETLIST, &lib);
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        "set_input_delay 1 [get_ports d]\n",
        &AnalysisOptions::ideal(),
    );
    let l = &a.corners[0].limits;
    assert_eq!(l.unclocked_pulse_pins, 2);
    assert_eq!(l.kind(LimitKind::MinPulseWidthHigh).checked, 0);
    assert!(a.diags.iter().any(|d| d.code == "T0133"));
}

/// Two flip-flops whose clock pins state the pin pulse attributes (LIBERTY-REQ-0049): `DFA/CK`
/// only the attributes, `DFB/CK` the attributes and `min_pulse_width` groups.
const ATTR_LIB: &str = r#"library (attr) {
  time_unit : "1ns"; capacitive_load_unit (1, pf);
  lu_table_template (mpw_t) { variable_1 : related_pin_transition; index_1 ("0.1, 0.5"); }
  cell (DFA) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001;
      min_pulse_width_high : 0.6; min_pulse_width_low : 0.45; min_period : 2.0; }
    pin (D) { direction : input; capacitance : 0.001; }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
  cell (DFB) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001;
      min_pulse_width_high : 0.5; min_pulse_width_low : 0.3;
      timing () { related_pin : "CK"; timing_type : min_pulse_width; when : "D";
        rise_constraint (mpw_t) { index_1 ("0.1, 0.5"); values ("0.3, 0.7"); }
        fall_constraint (scalar) { values ("0.55"); } } }
    pin (D) { direction : input; capacitance : 0.001; }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
}
"#;

// LIBERTY-REQ-0049, STA-REQ-0801, STA-REQ-0815: pin pulse attributes are checked, alone or with
// the groups (the largest constraint applies), and an ideal clock's latency by transition moves
// the edges of the pulse. Ideal clock c: period 10, rise 0, fall 4, latency rise 0.2, fall 0.25,
// transition 0.2.
// High pulse (positive sense): (4 + 0.25) - (0 + 0.2) = 4.05; low: (10 + 0.2) - (4 + 0.25) = 5.95.
// a/CK (DFA, attributes only): high required 0.6, slack 3.45; low required 0.45, slack 5.5;
// period required 2.0, slack 8.0.
// b/CK (DFB): high required max(0.5, table 0.3 + (0.2 - 0.1) / 0.4 * 0.4 = 0.4) = 0.5, slack 3.55;
// low required max(0.3, 0.55) = 0.55, slack 5.4; no period check.
#[test]
fn pin_pulse_attributes_are_checked_on_an_ideal_clock() {
    let lib = read_text(ATTR_LIB, "attr.lib").expect("attribute library");
    let frozen = freeze(
        "module top (input clk, input d, output q1, output q2);\n\
         DFA a (.CK(clk), .D(d), .Q(q1));\nDFB b (.CK(clk), .D(d), .Q(q2));\nendmodule\n",
        &lib,
    );
    let sdc = "create_clock -name c -period 10 -waveform {0 4} [get_ports clk]\n\
        set_clock_latency -rise 0.2 [get_clocks c]\nset_clock_latency -fall 0.25 [get_clocks c]\n\
        set_clock_transition 0.2 [get_clocks c]\n\
        set_input_delay -clock c 1 [get_ports d]\nset_output_delay -clock c 1 [get_ports {q1 q2}]\n";
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        sdc,
        &AnalysisOptions::ideal(),
    );
    let l = &a.corners[0].limits;
    for (pin, kind, value, limit, slack) in [
        ("a/CK", LimitKind::MinPulseWidthHigh, 4.05, 0.6, 3.45),
        ("a/CK", LimitKind::MinPulseWidthLow, 5.95, 0.45, 5.5),
        ("a/CK", LimitKind::MinPeriod, 10.0, 2.0, 8.0),
        ("b/CK", LimitKind::MinPulseWidthHigh, 4.05, 0.5, 3.55),
        ("b/CK", LimitKind::MinPulseWidthLow, 5.95, 0.55, 5.4),
    ] {
        let r = row(l, kind, pin, &a);
        close(r.value, value, &format!("{pin} {} value", kind.name()));
        close(r.limit, limit, &format!("{pin} {} limit", kind.name()));
        close(r.slack, slack, &format!("{pin} {} slack", kind.name()));
    }
    assert_eq!(l.kind(LimitKind::MinPeriod).checked, 1);
    // Only the group of b/CK has a condition; both pins are on an ideal clock (T0139).
    assert_eq!(l.conditional_pulse_groups, 1);
    assert_eq!(l.ideal_clock_pulse_pins, 2);
    assert!(a.diags.iter().any(|d| d.code == "T0139"));
    assert_eq!(a.corners[0].unanalyzed_arcs, 0);

    // A propagated clock is not noted.
    let prop = format!("{sdc}set_propagated_clock [get_clocks c]\n");
    let p = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        &prop,
        &AnalysisOptions::ideal(),
    );
    assert_eq!(p.corners[0].limits.ideal_clock_pulse_pins, 0);
    assert!(!p.diags.iter().any(|d| d.code == "T0139"));
}

const DRV_NETLIST: &str = "module top (input a, output y, output o2, output o3, output o4);\n\
    wire n1;\n\
    BUF u1 (.A(a), .Y(n1));\nBUF u2 (.A(n1), .Y(o2));\nBUF u3 (.A(n1), .Y(o3));\n\
    BUF u4 (.A(n1), .Y(o4));\nassign y = n1;\nendmodule\n";

const DRV_SDC: &str = "set_input_transition 0.6 [get_ports a]\nset_load 0.004 [get_ports y]\n\
    set_fanout_load 1.5 [get_ports y]\nset_max_fanout 5 [current_design]\n\
    set_max_capacitance 0.001 [get_ports a]\nset_max_transition 0.55 [get_ports a]\n";

// STA-REQ-0803 to STA-REQ-0805: the tightest applicable limit, its source, and the values.
// u1/Y drives u2/A, u3/A, u4/A (fanout_load 2 each) and the port y (set_fanout_load 1.5):
// fanout 7.5 against min(default_max_fanout 4, design 5) = 4 (library), slack -3.5. Its net has
// 3 x 0.002 + 0.004 = 0.010 pF against max_capacitance 0.009: slack -0.001. Port a drives
// u1/A (0.002 pF) against its set_max_capacitance 0.001: slack -0.001; its fanout 2 against the
// design 5: slack 3. Transitions: a = 0.6 against its port limit 0.55 (-0.05), u1/A = 0.6
// against the pin limit 0.5 (-0.1), u2/A = 0.2 (0.3); u1/Y and the output ports have no limit.
#[test]
fn design_rules_take_the_tightest_limit() {
    let lib = lib();
    let frozen = freeze(DRV_NETLIST, &lib);
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        DRV_SDC,
        &AnalysisOptions::ideal(),
    );
    let l = &a.corners[0].limits;
    let f = row(l, LimitKind::MaxFanout, "u1/Y", &a);
    close(f.value, 7.5, "fanout of u1/Y");
    close(f.limit, 4.0, "fanout limit of u1/Y");
    close(f.slack, -3.5, "fanout slack of u1/Y");
    assert_eq!(f.source, LimitSource::Liberty);
    let fa = row(l, LimitKind::MaxFanout, "a", &a);
    close(fa.slack, 3.0, "fanout slack of a");
    assert_eq!(fa.source, LimitSource::SdcDesign);
    let c = row(l, LimitKind::MaxCapacitance, "u1/Y", &a);
    close(c.value, 0.010, "capacitance of n1");
    close(c.slack, -0.001, "capacitance slack of u1/Y");
    let ca = row(l, LimitKind::MaxCapacitance, "a", &a);
    close(ca.value, 0.002, "capacitance of a");
    assert_eq!(ca.source, LimitSource::SdcPort);
    for (pin, slack, source) in [
        ("a", -0.05, LimitSource::SdcPort),
        ("u1/A", -0.1, LimitSource::Liberty),
        ("u2/A", 0.3, LimitSource::Liberty),
    ] {
        let t = row(l, LimitKind::MaxTransition, pin, &a);
        close(t.slack, slack, &format!("transition slack of {pin}"));
        assert_eq!(t.source, source, "{pin}");
    }
    let violators = |kind| {
        let mut v: Vec<String> = l
            .rows_of(kind)
            .filter(|r| r.slack < 0.0)
            .map(|r| a.netview.node_name_str(r.node).into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(violators(LimitKind::MaxTransition), ["a", "u1/A"]);
    assert_eq!(violators(LimitKind::MaxCapacitance), ["a", "u1/Y"]);
    assert_eq!(violators(LimitKind::MaxFanout), ["u1/Y"]);
    // u1/Y, u2/Y..u4/Y and the four output ports have a transition time but no limit.
    assert_eq!(l.kind(LimitKind::MaxTransition).unlimited, 8);
    // The analyzed design-rule commands carry no `D0021` (STA-REQ-0805).
    assert!(!a.diags.iter().any(|d| d.code == "D0021"));
}

// STA-REQ-0805: clock-domain slots: -clock_path reaches the clock network pins only and
// -data_path the data pins of the domain only; -rise and -fall limit one transition. The clock is
// propagated, with the port transition 0.2 at its source (STA-REQ-0818 leaves an ideal clock
// network out, see below).
#[test]
fn clock_domain_limits_follow_the_path_options() {
    let lib = lib();
    let frozen = freeze(PULSE_NETLIST, &lib);
    let prop = format!(
        "{PULSE_SDC}set_propagated_clock [get_clocks c]\nset_input_transition 0.2 [get_ports clk]\n"
    );
    let violators = |sdc: &str, opts: &AnalysisOptions| -> (Vec<String>, Vec<String>) {
        let a = analyze(&frozen, std::slice::from_ref(&lib), sdc, opts);
        let mut v: Vec<String> = a.corners[0]
            .limits
            .rows_of(LimitKind::MaxTransition)
            .filter(|r| r.slack < 0.0)
            .map(|r| a.netview.node_name_str(r.node).into_owned())
            .collect();
        v.sort();
        (v, a.diags.iter().map(|d| d.code.clone()).collect())
    };
    let ideal = AnalysisOptions::ideal();
    let domain = AnalysisOptions {
        clock_limits: ClockLimitScope::Domain,
        ..AnalysisOptions::ideal()
    };

    let sdc = format!("{prop}set_max_transition 0.15 -clock_path [get_clocks c]\n");
    // The clock source and the pins it reaches by wire carry the port transition 0.2 > 0.15;
    // behind the inverter the transition is its table value 0.1. Data pins (Q pins, d) are
    // outside the clock network.
    let (viol, codes) = violators(&sdc, &ideal);
    assert_eq!(viol, ["clk", "f1/CK", "i1/A"]);
    assert!(!codes.iter().any(|c| c == "T0144"), "{codes:?}");

    let sdc = format!("{prop}set_max_transition 0.05 -data_path [get_clocks c]\n");
    // Q pins (0.1) and the ports they drive are data pins of c; clock pins are not.
    let (viol, _) = violators(&sdc, &ideal);
    assert_eq!(viol, ["f1/Q", "f2/Q", "q1", "q2"]);

    let sdc = format!("{prop}set_max_transition 0.05 -rise [get_clocks c]\n");
    let a = analyze(&frozen, std::slice::from_ref(&lib), &sdc, &ideal);
    let r = row(&a.corners[0].limits, LimitKind::MaxTransition, "f1/Q", &a);
    assert_eq!(r.tr, Some(Tr::Rise));
    assert_eq!(r.source, LimitSource::SdcClock);
    // Without a path option both slots are equal: no pin depends on the scope.
    assert_eq!(a.corners[0].limits.clock_scope_pins, 0);
    assert!(!a.diags.iter().any(|d| d.code == "T0147"));

    // STA-REQ-0808: the five clock network pins (clk, i1/A, i1/Y, f1/CK, f2/CK) have a clock
    // whose clock-path and data-path slots differ, under either scope: counted, note T0147.
    for (cmd, opts) in [
        ("-clock_path 0.15", &ideal),
        ("-data_path 0.05", &ideal),
        ("-clock_path 0.15", &domain),
        ("-data_path 0.05", &domain),
    ] {
        let sdc = format!("{prop}set_max_transition {cmd} [get_clocks c]\n");
        let a = analyze(&frozen, std::slice::from_ref(&lib), &sdc, opts);
        assert_eq!(a.corners[0].limits.clock_scope_pins, 5, "{cmd}");
        assert!(a.diags.iter().any(|d| d.code == "T0147"), "{cmd}");
    }

    // STA-REQ-0808: under the scope `domain` the data-path limit covers the clock network too,
    // and a -clock_path limit alone applies nowhere (T0144).
    let sdc = format!("{prop}set_max_transition 0.15 -clock_path [get_clocks c]\n");
    let (viol, codes) = violators(&sdc, &domain);
    assert!(viol.is_empty(), "{viol:?}");
    assert!(codes.iter().any(|c| c == "T0144"), "{codes:?}");
    let sdc = format!("{prop}set_max_transition 0.05 -data_path [get_clocks c]\n");
    let (viol, codes) = violators(&sdc, &domain);
    assert_eq!(
        viol,
        [
            "clk", "f1/CK", "f1/Q", "f2/CK", "f2/Q", "i1/A", "i1/Y", "q1", "q2"
        ]
    );
    assert!(!codes.iter().any(|c| c == "T0144"), "{codes:?}");
    // A later -clock_path command does not replace the data-path slot under `domain`; a
    // -data_path command replaces the slot of a command without option.
    let sdc = format!(
        "{prop}set_max_transition 0.05 [get_clocks c]\nset_max_transition 0.5 -clock_path [get_clocks c]\n"
    );
    let (viol, _) = violators(&sdc, &domain);
    assert_eq!(viol.len(), 9, "{viol:?}");
    let sdc = format!(
        "{prop}set_max_transition 0.05 [get_clocks c]\nset_max_transition 0.5 -data_path [get_clocks c]\n"
    );
    let (viol, _) = violators(&sdc, &domain);
    assert!(viol.is_empty(), "{viol:?}");
}

// STA-REQ-0818: the design rules of an ideal clock network are not checked. clk, f1/CK, i1/A,
// i1/Y and f2/CK form the network of the ideal clock c: no transition row, and their limited
// checks are unevaluated (the -clock_path transition limit on all five; the library's
// default_max_fanout on the driver i1/Y; clk has no fanout or capacitance limit). The design
// rules are then unchecked or partial, never met, and a warning T0146 names the count.
#[test]
fn ideal_clock_network_is_left_out_of_the_design_rules() {
    let lib = lib();
    let frozen = freeze(PULSE_NETLIST, &lib);
    let sdc = format!("{PULSE_SDC}set_max_transition 0.15 -clock_path [get_clocks c]\n");
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        &sdc,
        &AnalysisOptions::ideal(),
    );
    let l = &a.corners[0].limits;
    assert_eq!(l.ideal_clock_pins, 5);
    assert_eq!(l.kind(LimitKind::MaxTransition).unevaluated, 5);
    assert_eq!(l.kind(LimitKind::MaxFanout).unevaluated, 1);
    assert_eq!(l.kind(LimitKind::MaxCapacitance).unevaluated, 0);
    let network = ["clk", "f1/CK", "i1/A", "i1/Y", "f2/CK"];
    assert!(
        !l.rows
            .iter()
            .any(|r| r.kind.is_drv() && network.contains(&a.netview.node_name_str(r.node).as_ref())),
        "no design-rule row of the ideal network (its pulse checks stay, STA-REQ-0815)"
    );
    let codes: Vec<&str> = a.diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"T0146"), "{codes:?}");
    assert!(!codes.contains(&"T0137"), "{codes:?}");
    let report = ruststa::engine::store::from_analysis::corner_limits_report(&a.corners[0]);
    assert_eq!(report.ideal_clock_pins, 5);
    // Every limited transition check is on the ideal network: unchecked, not unconstrained
    // (STA-REQ-0813); the fanout rule has a checked driver (f1/Q) and is partial.
    let s = ruststa::engine::limits::status::limit_statuses(Some(&[&report]));
    use ruststa::engine::limits::status::CheckStatus;
    assert_eq!(s.drv[0].1, CheckStatus::Unchecked);
    assert_eq!(s.drv[2].1, CheckStatus::Partial);

    // Propagated, the same pins are checked.
    let prop = format!("{sdc}set_propagated_clock [get_clocks c]\n");
    let p = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        &prop,
        &AnalysisOptions::ideal(),
    );
    let l = &p.corners[0].limits;
    assert_eq!(l.ideal_clock_pins, 0);
    assert_eq!(l.kind(LimitKind::MaxTransition).unevaluated, 0);
    for pin in network {
        row(l, LimitKind::MaxTransition, pin, &p);
    }
}

// STA-REQ-0805: options that do not apply and objects that are not checked are reported.
#[test]
fn inapplicable_design_rule_forms_are_reported() {
    let lib = lib();
    let frozen = freeze(DRV_NETLIST, &lib);
    let sdc = "create_clock -name c -period 10 [get_ports a]\n\
        set_max_transition 0.1 -rise [current_design]\n\
        set_max_capacitance 0.1 [get_ports y]\nset_max_fanout 3 [get_clocks c]\n\
        set_fanout_load 2 [get_ports a]\nset_min_capacitance 0.001 [current_design]\n";
    let a = analyze(
        &frozen,
        std::slice::from_ref(&lib),
        sdc,
        &AnalysisOptions::ideal(),
    );
    let codes: Vec<&str> = a.diags.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"T0130"), "{codes:?}");
    assert_eq!(
        codes.iter().filter(|c| **c == "T0131").count(),
        3,
        "{codes:?}"
    );
    // set_min_capacitance is still recorded only.
    assert!(codes.contains(&"D0021"), "{codes:?}");
}

// STA-REQ-0810: the row budget bounds the kept rows; the counts cover every check.
#[test]
fn row_budget_keeps_the_worst_rows() {
    let lib = lib();
    let frozen = freeze(DRV_NETLIST, &lib);
    let opts = AnalysisOptions {
        max_limit_rows: 1,
        ..AnalysisOptions::ideal()
    };
    let a = analyze(&frozen, std::slice::from_ref(&lib), DRV_SDC, &opts);
    let l = &a.corners[0].limits;
    let t = l.kind(LimitKind::MaxTransition);
    assert_eq!((t.stored, t.violating), (1, 2));
    assert!(t.checked > 1);
    let kept: Vec<&LimitRow> = l.rows_of(LimitKind::MaxTransition).collect();
    close(kept[0].slack, -0.1, "worst transition row kept");
    assert!(a.diags.iter().any(|d| d.code == "T0134"));
    // The table of the analysis is deterministic.
    let b = analyze(&frozen, std::slice::from_ref(&lib), DRV_SDC, &opts);
    assert_eq!(
        LimitsData::from_analysis(&a, 1).to_bytes().unwrap(),
        LimitsData::from_analysis(&b, 1).to_bytes().unwrap()
    );
}

fn spef_text(cap: f64) -> String {
    format!(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n\
         *D_NET a 0.003\n*CONN\n*P a I\n*I u:A I\n*CAP\n1 a 0.001\n2 u:A 0.002\n*RES\n1 a u:A 2\n*END\n\
         *D_NET y {cap}\n*CONN\n*P y O\n*I u:Y O\n*CAP\n1 u:Y {cap}\n*RES\n1 u:Y y 3\n*END\n"
    )
}

// STA-REQ-0820: each corner takes its own extracted parasitics; a corner without them is an
// explicit error (`T0070`), never ideal wires. The synthetic cell has load-dependent tables, as
// the extracted waveform model needs (STA-REQ-0720).
#[test]
fn every_corner_binds_its_own_parasitics() {
    let lib = read_text(
        &ruststa_fixtures::liberty::library(ruststa_fixtures::liberty::Variant::Wt15),
        "wt_15.lib",
    )
    .unwrap();
    let verilog = "module top (input a, output y);\nwt_buf_1 u (.A(a), .Y(y));\nendmodule\n";
    let frozen = freeze(verilog, &lib);
    let libs = [lib];
    let corners = [
        CornerLibs::new("fast", &libs),
        CornerLibs::new("slow", &libs),
    ];
    let input = |corner: &str, cap: f64| {
        Arc::new(ParasiticInput {
            corner: corner.into(),
            data: spef::read(&spef_text(cap), spef::Limits::default()).unwrap(),
        })
    };
    let sdc = "set_input_transition 0.1 [get_ports a]\nset_max_capacitance 1 [current_design]\n";
    let compile = || CompiledConstraints::from_sdc_str(sdc, "c.sdc", &frozen, &corners).unwrap();
    let opts = AnalysisOptions {
        parasitics: vec![input("slow", 0.008)],
        ..AnalysisOptions::default()
    };
    let err = EngineAnalysis::analyze(&frozen, &corners, compile(), &opts, Ctx::NONE).unwrap_err();
    assert!(
        err.iter()
            .any(|d| d.code == "T0070" && d.message.contains("fast")),
        "{err:?}"
    );
    let opts = AnalysisOptions {
        parasitics: vec![input("fast", 0.001), input("slow", 0.008)],
        ..AnalysisOptions::default()
    };
    let a = EngineAnalysis::analyze(&frozen, &corners, compile(), &opts, Ctx::NONE).unwrap();
    let cap = |c: usize| {
        a.corners[c]
            .limits
            .rows_of(LimitKind::MaxCapacitance)
            .find(|r| a.netview.node_name_str(r.node) == "u/Y")
            .unwrap()
            .value
    };
    // The net y holds the SPEF capacitance of its corner (the port has no load).
    let pin_cap = cap(0) - 0.001;
    close(
        cap(1) - 0.008,
        pin_cap,
        "the same pin capacitance in both corners",
    );
    assert!((0.0..0.001).contains(&pin_cap), "{pin_cap}");
    for c in &a.corners {
        assert_eq!(c.wire_model.scope().wire_model, "spef_rc");
    }
}
