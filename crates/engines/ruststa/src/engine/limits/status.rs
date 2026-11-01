// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The status of the pin limit checks over corners (`STA-REQ-0807`, `STA-REQ-0813`).
//!
//! One function turns the per-corner counts of the `SCOPE` of a timing table into the counts and
//! statuses that the revision summary and every command envelope report, so that both use the
//! same rules (CONTRIBUTING Rule 4.2):
//!
//! - `unchecked`: the limits were not analyzed, or nothing was evaluated although checks exist
//!   (every pulse pin unclocked, or every value not finite);
//! - `unconstrained`: there is nothing to check (no pin with a pulse constraint, or no limit of
//!   the kind anywhere);
//! - `violated`: a checked row has a negative slack;
//! - `partial`: no violation, but some checks were not evaluated (unclocked pulse pins, pulse pins
//!   left out by the `clock-ends` scope, values that are not finite, untimed pins, pins of an
//!   ideal clock network, pins or drivers of a design rule without a limit while others have
//!   one) or, for the design rules as a whole, a kind is unconstrained;
//! - `met`: every check was evaluated and none is violated.
//!
//! A violating count is `null` whenever its kinds have no checked row.

use crate::engine::store::{CornerLimitsReport, LimitKindReport};

use super::LimitKind;

/// The status of a group of limit checks (`STA-REQ-0813`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Every check was evaluated and none is violated.
    Met,
    /// A checked row has a negative slack.
    Violated,
    /// No violation, but not every check was evaluated, or a design rule is unconstrained.
    Partial,
    /// There is nothing to check.
    Unconstrained,
    /// Not analyzed, or nothing evaluated although checks exist.
    Unchecked,
}

impl CheckStatus {
    /// The name of the status in reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Violated => "violated",
            Self::Partial => "partial",
            Self::Unconstrained => "unconstrained",
            Self::Unchecked => "unchecked",
        }
    }

    /// The status of one group of kinds from its counts: checked rows, the worst corner's
    /// violating rows, and whether some checks of the group were not evaluated.
    fn of(checked: u64, violating: u64, incomplete: bool) -> Self {
        if violating > 0 {
            Self::Violated
        } else if checked == 0 {
            if incomplete {
                Self::Unchecked
            } else {
                Self::Unconstrained
            }
        } else if incomplete {
            Self::Partial
        } else {
            Self::Met
        }
    }
}

/// The counts and statuses of the limit checks of a set of corners (`STA-REQ-0807`,
/// `STA-REQ-0813`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitStatuses {
    /// Violating `min_pulse_width` rows (high and low) of the worst corner; `None` without a
    /// checked pulse width row.
    pub min_pulse_width_violating: Option<u64>,
    /// Violating `min_period` rows of the worst corner; `None` without a checked period row.
    pub min_period_violating: Option<u64>,
    /// The status of the pulse width and period checks.
    pub pulse_width: CheckStatus,
    /// Per design rule (`max_transition`, `max_capacitance`, `max_fanout`): the violating rows of
    /// the worst corner (`None` without a checked row) and the status of the kind.
    pub drv: [(Option<u64>, CheckStatus); 3],
    /// The status of the three design rules together.
    pub drv_status: CheckStatus,
}

/// The design rules in the order of [`LimitStatuses::drv`].
pub const DRV_KINDS: [LimitKind; 3] = [
    LimitKind::MaxTransition,
    LimitKind::MaxCapacitance,
    LimitKind::MaxFanout,
];

/// The pulse width kinds.
const MPW_KINDS: [LimitKind; 2] = [LimitKind::MinPulseWidthHigh, LimitKind::MinPulseWidthLow];

fn counts<'a>(
    corner: &'a CornerLimitsReport,
    kinds: &'a [LimitKind],
) -> impl Iterator<Item = &'a LimitKindReport> + 'a {
    kinds.iter().filter_map(|k| corner.kinds.get(k.name()))
}

/// The counts of a group of kinds over a set of corners.
#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    /// Checked rows over the corners.
    checked: u64,
    /// Violating rows of the worst corner.
    violating: u64,
    /// Checks not evaluated (values that are not finite) over the corners.
    unevaluated: u64,
    /// Pins or drivers with a value but no limit over the corners (design rules).
    unlimited: u64,
}

/// The [`Totals`] of `kinds` over `corners`.
fn totals(corners: &[&CornerLimitsReport], kinds: &[LimitKind]) -> Totals {
    let mut t = Totals::default();
    for c in corners {
        let mut v = 0u64;
        for k in counts(c, kinds) {
            t.checked = t.checked.saturating_add(k.checked);
            t.unevaluated = t.unevaluated.saturating_add(k.unevaluated);
            t.unlimited = t.unlimited.saturating_add(k.unlimited);
            v = v.saturating_add(k.violating);
        }
        t.violating = t.violating.max(v);
    }
    t
}

/// The counts and statuses of `corners`, or of an analysis without limit checks when `corners`
/// is `None` or empty (`STA-REQ-0807`, `STA-REQ-0813`).
#[must_use]
pub fn limit_statuses(corners: Option<&[&CornerLimitsReport]>) -> LimitStatuses {
    let Some(corners) = corners.filter(|c| !c.is_empty()) else {
        return LimitStatuses {
            min_pulse_width_violating: None,
            min_period_violating: None,
            pulse_width: CheckStatus::Unchecked,
            drv: [(None, CheckStatus::Unchecked); 3],
            drv_status: CheckStatus::Unchecked,
        };
    };
    let count = |checked: u64, violating: u64| (checked > 0).then_some(violating);

    let mpw = totals(corners, &MPW_KINDS);
    let per = totals(corners, &[LimitKind::MinPeriod]);
    // Pins reached by no clock, and pins left out by the `clock-ends` scope, are not checked
    // (`STA-REQ-0802`, `STA-REQ-0817`).
    let unchecked = corners
        .iter()
        .map(|c| {
            c.unclocked_pulse_pins
                .saturating_add(c.pass_through_pulse_pins)
        })
        .max();
    let pulse_incomplete =
        unchecked.is_some_and(|u| u > 0) || mpw.unevaluated + per.unevaluated > 0;
    let pulse_width = CheckStatus::of(
        mpw.checked + per.checked,
        mpw.violating + per.violating,
        pulse_incomplete,
    );

    // A design rule checked on some pins but without a limit on others is `partial`, never
    // `met`; without a limit anywhere it is `unconstrained`. The limited checks of untimed pins
    // and of the pins of an ideal clock network are unevaluated (`STA-REQ-0809`,
    // `STA-REQ-0822`).
    let drv = DRV_KINDS.map(|k| {
        let t = totals(corners, &[k]);
        let unlimited = t.checked > 0 && t.unlimited > 0;
        (
            count(t.checked, t.violating),
            CheckStatus::of(t.checked, t.violating, t.unevaluated > 0 || unlimited),
        )
    });
    let statuses = drv.map(|(_, s)| s);
    let all = |s: CheckStatus| statuses.iter().all(|&x| x == s);
    let drv_status = if statuses.contains(&CheckStatus::Violated) {
        CheckStatus::Violated
    } else if all(CheckStatus::Met) {
        CheckStatus::Met
    } else if all(CheckStatus::Unconstrained) {
        CheckStatus::Unconstrained
    } else if statuses
        .iter()
        .all(|s| matches!(s, CheckStatus::Unconstrained | CheckStatus::Unchecked))
    {
        CheckStatus::Unchecked
    } else {
        CheckStatus::Partial
    };

    LimitStatuses {
        min_pulse_width_violating: count(mpw.checked, mpw.violating),
        min_period_violating: count(per.checked, per.violating),
        pulse_width,
        drv,
        drv_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corner(kinds: &[(LimitKind, u64, u64, u64)], unclocked: u64) -> CornerLimitsReport {
        CornerLimitsReport {
            corner: "c".into(),
            kinds: LimitKind::ALL
                .iter()
                .map(|&k| {
                    let (checked, violating, unevaluated) = kinds
                        .iter()
                        .find(|x| x.0 == k)
                        .map_or((0, 0, 0), |x| (x.1, x.2, x.3));
                    (
                        k.name().to_owned(),
                        LimitKindReport {
                            checked,
                            violating,
                            unlimited: 0,
                            worst_slack_ns: None,
                            unevaluated,
                        },
                    )
                })
                .collect(),
            unclocked_pulse_pins: unclocked,
            ..CornerLimitsReport::default()
        }
    }

    use LimitKind::{
        MaxCapacitance as Cap, MaxFanout as Fan, MaxTransition as Tran, MinPeriod as Per,
        MinPulseWidthHigh as High, MinPulseWidthLow as Low,
    };

    // STA-REQ-0813: without an analysis every count is null and every status unchecked.
    #[test]
    fn not_analyzed_is_unchecked() {
        let s = limit_statuses(None);
        assert_eq!(s.pulse_width, CheckStatus::Unchecked);
        assert_eq!(s.drv_status, CheckStatus::Unchecked);
        assert_eq!(s.min_pulse_width_violating, None);
        assert!(
            s.drv
                .iter()
                .all(|&(v, st)| v.is_none() && st == CheckStatus::Unchecked)
        );
        assert_eq!(limit_statuses(Some(&[])), s);
    }

    // STA-REQ-0813: unclocked pulse pins make the pulse checks unchecked (nothing evaluated) or
    // partial (some evaluated), never met; a violation still wins.
    #[test]
    fn unclocked_pulse_pins_are_never_met() {
        let none = corner(&[(Tran, 3, 0, 0), (Cap, 2, 0, 0), (Fan, 2, 0, 0)], 4);
        let s = limit_statuses(Some(&[&none]));
        assert_eq!(s.pulse_width, CheckStatus::Unchecked);
        assert_eq!(
            (s.min_pulse_width_violating, s.min_period_violating),
            (None, None)
        );
        let some = corner(&[(High, 2, 0, 0), (Low, 2, 0, 0)], 1);
        let s = limit_statuses(Some(&[&some]));
        assert_eq!(s.pulse_width, CheckStatus::Partial);
        assert_eq!(
            (s.min_pulse_width_violating, s.min_period_violating),
            (Some(0), None)
        );
        let bad = corner(&[(High, 2, 1, 0), (Per, 1, 0, 0)], 1);
        let s = limit_statuses(Some(&[&bad]));
        assert_eq!(s.pulse_width, CheckStatus::Violated);
        assert_eq!(
            (s.min_pulse_width_violating, s.min_period_violating),
            (Some(1), Some(0))
        );
        let met = corner(&[(High, 2, 0, 0), (Low, 2, 0, 0), (Per, 2, 0, 0)], 0);
        assert_eq!(limit_statuses(Some(&[&met])).pulse_width, CheckStatus::Met);
        let empty = corner(&[], 0);
        assert_eq!(
            limit_statuses(Some(&[&empty])).pulse_width,
            CheckStatus::Unconstrained
        );
    }

    // STA-REQ-0813: a design rule without any limit is unconstrained with a null count, and the
    // design rules together are then partial, never met.
    #[test]
    fn unconstrained_design_rules_are_not_met() {
        let c = corner(&[(Tran, 19, 0, 0), (Cap, 10, 0, 0)], 0);
        let s = limit_statuses(Some(&[&c]));
        assert_eq!(s.drv[0], (Some(0), CheckStatus::Met));
        assert_eq!(s.drv[1], (Some(0), CheckStatus::Met));
        assert_eq!(s.drv[2], (None, CheckStatus::Unconstrained));
        assert_eq!(s.drv_status, CheckStatus::Partial);
        let none = corner(&[], 0);
        assert_eq!(
            limit_statuses(Some(&[&none])).drv_status,
            CheckStatus::Unconstrained
        );
        let all = corner(&[(Tran, 1, 0, 0), (Cap, 1, 0, 0), (Fan, 1, 0, 0)], 0);
        assert_eq!(limit_statuses(Some(&[&all])).drv_status, CheckStatus::Met);
        let viol = corner(&[(Tran, 1, 1, 0)], 0);
        assert_eq!(
            limit_statuses(Some(&[&viol])).drv_status,
            CheckStatus::Violated
        );
    }

    // STA-REQ-0813: a design rule with a limit on some pins and none on others is partial, never
    // met; a violation still wins, and without any limit the rule stays unconstrained.
    #[test]
    fn partly_limited_design_rules_are_partial() {
        let with_unlimited = |kind: LimitKind, checked, violating, unlimited| {
            let mut c = corner(&[(Tran, 2, 0, 0), (Cap, 2, 0, 0), (Fan, 2, 0, 0)], 0);
            let k = c.kinds.get_mut(kind.name()).expect("kind");
            (k.checked, k.violating, k.unlimited) = (checked, violating, unlimited);
            c
        };
        let c = with_unlimited(Cap, 1, 0, 7);
        let s = limit_statuses(Some(&[&c]));
        assert_eq!(s.drv[1], (Some(0), CheckStatus::Partial));
        assert_eq!(s.drv_status, CheckStatus::Partial);
        let c = with_unlimited(Tran, 3, 1, 29);
        let s = limit_statuses(Some(&[&c]));
        assert_eq!(s.drv[0], (Some(1), CheckStatus::Violated));
        assert_eq!(s.drv_status, CheckStatus::Violated);
        let c = with_unlimited(Fan, 0, 0, 19);
        let s = limit_statuses(Some(&[&c]));
        assert_eq!(s.drv[2], (None, CheckStatus::Unconstrained));
        assert_eq!(s.drv_status, CheckStatus::Partial);
    }

    // STA-REQ-0813: checks that are not finite make their kind partial or unchecked; the worst
    // corner gives the violating count.
    #[test]
    fn unevaluated_checks_and_worst_corner() {
        let a = corner(&[(Tran, 4, 1, 1), (Cap, 0, 0, 2), (Fan, 3, 0, 0)], 0);
        let b = corner(&[(Tran, 4, 3, 0), (Cap, 0, 0, 0), (Fan, 3, 0, 0)], 0);
        let s = limit_statuses(Some(&[&a, &b]));
        assert_eq!(s.drv[0], (Some(3), CheckStatus::Violated));
        assert_eq!(s.drv[1], (None, CheckStatus::Unchecked));
        assert_eq!(s.drv_status, CheckStatus::Violated);
        let c = corner(&[(Tran, 4, 0, 1), (Cap, 1, 0, 0), (Fan, 1, 0, 0)], 0);
        let s = limit_statuses(Some(&[&c]));
        assert_eq!(s.drv[0], (Some(0), CheckStatus::Partial));
        assert_eq!(s.drv_status, CheckStatus::Partial);
        let d = corner(&[(High, 2, 0, 1)], 0);
        assert_eq!(
            limit_statuses(Some(&[&d])).pulse_width,
            CheckStatus::Partial
        );
    }
}
