// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The rules of the pulse checks: the choice among the groups and the attribute of a pin
//! (`STA-REQ-0801`, `STA-REQ-0816`) and the pins that are checked (`STA-REQ-0817`), with the pin
//! pulse attributes of LIBERTY-REQ-0049.
//!
//! The library has scalar constraint tables and the clock is ideal without latency, so every
//! expected value follows by hand from the specification (`docs/specs/sta/limits.md`): the high
//! pulse of clock `c` (period 10, waveform {0 4}) is 4.0 wide and its low pulse 6.0 at every pin
//! it reaches, and a period check compares 10 with the required period.

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_workspace::rbin::Rbin;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::limits::status::{CheckStatus, limit_statuses};
use ruststa::engine::limits::table::{LimitsData, LimitsTable};
use ruststa::engine::limits::{CornerLimits, LimitKind, PulseGroupRule, PulsePinScope, PulseRules};
use ruststa::engine::store::CornerLimitsReport;

/// `DFL/CK`: attributes high 0.4, low 0.35, period 1.8; width groups `G1` (rise 0.5, fall 0.6)
/// and `G2` (rise 0.3 only); period groups `P1` (rise 2.0, fall 2.5) and `P2` (fall 1.0 only).
/// `DFM/CK`: the high attribute 0.7 alone and a group with a fall constraint 0.45 only.
/// `GATER/CLK`: a low-width group 0.25 on a pin whose clock passes on to `GCLK`.
const RULES_LIB: &str = r#"library (rules) {
  time_unit : "1ns"; capacitive_load_unit (1, pf);
  cell (DFL) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001;
      min_pulse_width_high : 0.4; min_pulse_width_low : 0.35; min_period : 1.8;
      @GROUPS@ }
    pin (D) { direction : input; capacitance : 0.001; }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
  cell (DFM) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001; min_pulse_width_high : 0.7;
      timing () { related_pin : "CK"; timing_type : min_pulse_width; when : "D";
        fall_constraint (scalar) { values ("0.45"); } } }
    pin (D) { direction : input; capacitance : 0.001; }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
  cell (GATER) {
    pin (CLK) { direction : input; clock : true; capacitance : 0.001;
      timing () { related_pin : "CLK"; timing_type : min_pulse_width;
        fall_constraint (scalar) { values ("0.25"); } } }
    pin (GATE) { direction : input; capacitance : 0.001; }
    pin (GCLK) { direction : output; function : "CLK & GATE";
      timing () { related_pin : "CLK"; timing_sense : positive_unate;
        cell_rise (scalar) { values ("0.1"); } cell_fall (scalar) { values ("0.1"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } }
      timing () { related_pin : "GATE"; timing_sense : positive_unate;
        cell_rise (scalar) { values ("0.1"); } cell_fall (scalar) { values ("0.1"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
}
"#;

const G1: &str = r#"timing () { related_pin : "CK"; timing_type : min_pulse_width; when : "D";
        rise_constraint (scalar) { values ("0.5"); } fall_constraint (scalar) { values ("0.6"); } }"#;
const G2: &str = r#"timing () { related_pin : "CK"; timing_type : min_pulse_width; when : "!D";
        rise_constraint (scalar) { values ("0.3"); } }"#;
const P1: &str = r#"timing () { related_pin : "CK"; timing_type : minimum_period; when : "D";
        rise_constraint (scalar) { values ("2.0"); } fall_constraint (scalar) { values ("2.5"); } }"#;
const P2: &str = r#"timing () { related_pin : "CK"; timing_type : minimum_period; when : "!D";
        fall_constraint (scalar) { values ("1.0"); } }"#;

const NETLIST: &str = "module top (input clk, input en, input d, output q1, output q2, output q3);\n\
    wire gck;\n\
    DFL f1 (.CK(clk), .D(d), .Q(q1));\n\
    DFM f2 (.CK(clk), .D(d), .Q(q2));\n\
    GATER g (.CLK(clk), .GATE(en), .GCLK(gck));\n\
    DFL f3 (.CK(gck), .D(d), .Q(q3));\nendmodule\n";

const SDC: &str = "create_clock -name c -period 10 -waveform {0 4} [get_ports clk]\n\
    set_input_delay -clock c 1 [get_ports {d en}]\nset_output_delay -clock c 1 [get_ports {q1 q2 q3}]\n";

fn library(groups: &[&str]) -> Library {
    read_text(
        &RULES_LIB.replace("@GROUPS@", &groups.join("\n      ")),
        "rules.lib",
    )
    .expect("synthetic library")
}

fn freeze(lib: &Library) -> FrozenNetlist<'static> {
    read_structural(NETLIST, "top.v", &|cell_name| {
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

/// The limits of the analysis of `groups` (the `DFL/CK` groups in library order) under `rules`,
/// with the codes of its diagnostics.
fn limits(groups: &[&str], rules: PulseRules) -> (CornerLimits, Vec<String>, Vec<u8>) {
    let lib = library(groups);
    let frozen = freeze(&lib);
    let opts = AnalysisOptions {
        pulse_rules: rules,
        ..AnalysisOptions::ideal()
    };
    let a = EngineAnalysis::analyze_single_corner(&frozen, std::slice::from_ref(&lib), SDC, &opts)
        .expect("analysis");
    let table = LimitsData::from_analysis(&a, 100)
        .to_bytes()
        .expect("table");
    let codes = a.diags.iter().map(|d| d.code.to_string()).collect();
    (a.corners[0].limits.clone(), codes, table)
}

/// The required value of `kind` at `pin`, `None` without a row.
fn required(l: &CornerLimits, kind: LimitKind, pin: &str) -> Option<f64> {
    let node = pin_node(pin);
    l.rows_of(kind).find(|r| r.node.0 == node).map(|r| r.limit)
}

/// Node ids of the pins in netlist order: ports first (clk en d q1 q2 q3), then the instance
/// pins in instance and cell pin order.
fn pin_node(pin: &str) -> u32 {
    let order = [
        "f1/CK", "f1/D", "f1/Q", "f2/CK", "f2/D", "f2/Q", "g/CLK", "g/GATE", "g/GCLK", "f3/CK",
        "f3/D", "f3/Q",
    ];
    6 + order.iter().position(|p| *p == pin).expect("pin") as u32
}

fn close(a: Option<f64>, b: f64, what: &str) {
    let a = a.unwrap_or_else(|| panic!("{what}: no row"));
    assert!((a - b).abs() < 1e-12, "{what}: {a} vs {b}");
}

const WORST: PulseRules = PulseRules {
    groups: PulseGroupRule::Worst,
    pins: PulsePinScope::All,
};
const LAST: PulseRules = PulseRules {
    groups: PulseGroupRule::Last,
    pins: PulsePinScope::All,
};

// STA-REQ-0801, LIBERTY-REQ-0049: under `worst` the required value is the largest of every group
// and the attribute, whatever their order. DFL/CK: high max(0.4, 0.5, 0.3) = 0.5, low max(0.35,
// 0.6) = 0.6, period max(1.8, 2.0, 2.5, 1.0) = 2.5. DFM/CK: high 0.7 (the attribute), low 0.45.
#[test]
fn worst_rule_takes_the_largest_constraint_in_any_order() {
    for groups in [[G1, G2, P1, P2], [G2, G1, P2, P1]] {
        let (l, _, _) = limits(&groups, WORST);
        for pin in ["f1/CK", "f3/CK"] {
            close(required(&l, LimitKind::MinPulseWidthHigh, pin), 0.5, pin);
            close(required(&l, LimitKind::MinPulseWidthLow, pin), 0.6, pin);
            close(required(&l, LimitKind::MinPeriod, pin), 2.5, pin);
        }
        close(
            required(&l, LimitKind::MinPulseWidthHigh, "f2/CK"),
            0.7,
            "f2 high",
        );
        close(
            required(&l, LimitKind::MinPulseWidthLow, "f2/CK"),
            0.45,
            "f2 low",
        );
        assert_eq!(l.unused_period_fall_tables, 0);
        // The gater's clock pin is checked: low 6.0 - 0.25.
        let g = l
            .rows_of(LimitKind::MinPulseWidthLow)
            .find(|r| r.node.0 == pin_node("g/CLK"))
            .expect("gater row");
        assert!((g.slack - 5.75).abs() < 1e-12, "{}", g.slack);
    }
}

// STA-REQ-0816: under `last` each transition takes the last listed group that states it, else the
// attribute, and a period reads the rise_constraint only.
// In order G1 G2 P1 P2: high G2 0.3, low G1 0.6 (G2 has no fall), period P1 2.0 (P2 has no rise);
// the fall tables of P1 and P2 are not used (2 per DFL pin, T0142).
// In order G2 G1 P2 P1: high G1 0.5, low G1 0.6, period P1 2.0.
// DFM/CK: no group states a high width, so the attribute 0.7 applies; low the group 0.45.
#[test]
fn last_rule_takes_the_last_listed_group_per_transition() {
    let (l, codes, _) = limits(&[G1, G2, P1, P2], LAST);
    for pin in ["f1/CK", "f3/CK"] {
        close(required(&l, LimitKind::MinPulseWidthHigh, pin), 0.3, pin);
        close(required(&l, LimitKind::MinPulseWidthLow, pin), 0.6, pin);
        close(required(&l, LimitKind::MinPeriod, pin), 2.0, pin);
    }
    close(
        required(&l, LimitKind::MinPulseWidthHigh, "f2/CK"),
        0.7,
        "f2 high",
    );
    close(
        required(&l, LimitKind::MinPulseWidthLow, "f2/CK"),
        0.45,
        "f2 low",
    );
    assert_eq!(l.unused_period_fall_tables, 4);
    assert!(codes.iter().any(|c| c == "T0142"), "{codes:?}");
    assert!(codes.iter().any(|c| c == "T0132"), "{codes:?}");

    let (l, _, _) = limits(&[G2, G1, P2, P1], LAST);
    close(
        required(&l, LimitKind::MinPulseWidthHigh, "f1/CK"),
        0.5,
        "reversed high",
    );
    close(
        required(&l, LimitKind::MinPulseWidthLow, "f1/CK"),
        0.6,
        "reversed low",
    );
    close(
        required(&l, LimitKind::MinPeriod, "f1/CK"),
        2.0,
        "reversed period",
    );

    // Only the attributes: they apply under both rules (period 1.8 from min_period).
    for rules in [WORST, LAST] {
        let (l, _, _) = limits(&[], rules);
        close(
            required(&l, LimitKind::MinPulseWidthHigh, "f1/CK"),
            0.4,
            "attr high",
        );
        close(
            required(&l, LimitKind::MinPulseWidthLow, "f1/CK"),
            0.35,
            "attr low",
        );
        close(
            required(&l, LimitKind::MinPeriod, "f1/CK"),
            1.8,
            "attr period",
        );
    }
}

// STA-REQ-0817, STA-REQ-0813: the scope `clock-ends` leaves out the gater's clock pin, whose clock
// passes on to GCLK, counts it, warns T0141 and makes the pulse checks partial; the flip-flop
// behind the gater is still checked. The rules are recorded in the limits table.
#[test]
fn clock_ends_scope_counts_pass_through_pins() {
    let ends = PulseRules {
        groups: PulseGroupRule::Last,
        pins: PulsePinScope::ClockEnds,
    };
    let (l, codes, table) = limits(&[G1, G2, P1, P2], ends);
    assert_eq!(l.pass_through_pulse_pins, 1);
    assert_eq!(l.rules, ends);
    assert!(
        !l.rows_of(LimitKind::MinPulseWidthLow)
            .any(|r| r.node.0 == pin_node("g/CLK"))
    );
    close(
        required(&l, LimitKind::MinPulseWidthLow, "f3/CK"),
        0.6,
        "f3 low",
    );
    assert!(codes.iter().any(|c| c == "T0141"), "{codes:?}");

    let report = CornerLimitsReport {
        corner: "c".into(),
        kinds: LimitKind::ALL
            .iter()
            .map(|&k| {
                let s = l.kind(k);
                (
                    k.name().to_owned(),
                    ruststa::engine::store::LimitKindReport {
                        checked: s.checked,
                        violating: s.violating,
                        unlimited: s.unlimited,
                        worst_slack_ns: s.worst_slack,
                        unevaluated: s.unevaluated,
                    },
                )
            })
            .collect(),
        pass_through_pulse_pins: l.pass_through_pulse_pins,
        ..CornerLimitsReport::default()
    };
    assert_eq!(
        limit_statuses(Some(&[&report])).pulse_width,
        CheckStatus::Partial
    );

    let t = LimitsTable::from_rbin(Rbin::from_bytes(table, "LIMITS", 1..=1).expect("rbin"))
        .expect("table");
    assert_eq!(t.pulse_rules().expect("rules"), vec![ends]);
    assert_eq!(t.pass_through_pulse_pins().expect("counts"), vec![1]);
    assert_eq!(PulseRules::from_bits(ends.bits()), Some(ends));
    assert_eq!(PulseRules::from_bits(4), None);

    // Under the default scope the gater pin is checked and nothing passes through.
    let (l, codes, _) = limits(&[G1, G2, P1, P2], WORST);
    assert_eq!(l.pass_through_pulse_pins, 0);
    assert!(!codes.iter().any(|c| c == "T0141"));
}
