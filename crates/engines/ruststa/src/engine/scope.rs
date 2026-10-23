// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The analysis scope and the timing summary and status of a revision (owner: E5, from E0).
//!
//! The scope records what an analysis modelled: the corners, the wire model and the clock model
//! of each seam, CPPR, the exceptions applied and the completeness of the analysis (unanalyzed
//! arcs, unconstrained endpoints by reason). It is stored in the `SCOPE` section of `sta.rbin` as
//! canonical JSON and reported in the command envelope. The revision's meta data gets the timing
//! summary (`summary.sta`: worst and total negative slacks and endpoint counts, each the worst
//! over the corners) and status (`status.sta`).

use serde::{Deserialize, Serialize};

/// Canonicalize `-0.0` to `+0.0`.
#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}

/// Counts of timing exceptions applied during the analysis (`SCOPE.exceptions_applied`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeExceptions {
    pub false_path: u32,
    pub multicycle_path: u32,
    pub max_delay: u32,
    pub min_delay: u32,
    pub clock_groups: u32,
}

/// Counts of unconstrained endpoints by reason (`STA-REQ-0225`, `SCOPE.completeness.unconstrained_endpoints`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnconstrainedCounts {
    /// Sequential check pins whose clock pin receives no clock edge.
    pub unclocked: u32,
    /// Endpoints that are clocked (or have `set_output_delay`) and not disabled, but no data tag
    /// reaches the data pin.
    pub no_arrival: u32,
    /// Endpoints where data tags reach the pin and clocks reach the clock pin, but every
    /// `(tag, capture)` pair is excluded by `set_false_path` or `set_clock_groups`.
    pub false_path_only: u32,
    /// Output or inout ports with no `set_output_delay`.
    pub unconstrained_output: u32,
    /// Endpoints disabled by `set_disable_timing` or constant propagation (`set_case_analysis`).
    pub disabled: u32,
}

impl UnconstrainedCounts {
    /// Total number of unconstrained endpoints across all reasons (`STA-REQ-0225`).
    #[inline]
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.unclocked
            .saturating_add(self.no_arrival)
            .saturating_add(self.false_path_only)
            .saturating_add(self.unconstrained_output)
            .saturating_add(self.disabled)
    }

    /// True when no endpoint is unconstrained (`unclocked`, `no_arrival`, `unconstrained_output`
    /// are all zero; `false_path_only` and `disabled` are intentional user exclusions).
    #[inline]
    #[must_use]
    pub const fn has_unconstrained_gaps(&self) -> bool {
        self.unclocked > 0 || self.no_arrival > 0 || self.unconstrained_output > 0
    }

    /// Merges two per-corner [`UnconstrainedCounts`] by taking the component-wise maximum across
    /// corners.
    #[must_use]
    pub fn max_with(self, other: Self) -> Self {
        Self {
            unclocked: self.unclocked.max(other.unclocked),
            no_arrival: self.no_arrival.max(other.no_arrival),
            false_path_only: self.false_path_only.max(other.false_path_only),
            unconstrained_output: self.unconstrained_output.max(other.unconstrained_output),
            disabled: self.disabled.max(other.disabled),
        }
    }
}

/// Completeness accounting of an analysis (`STA-REQ-0015`, `STA-REQ-0225`, `SCOPE.completeness`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeCompleteness {
    /// Total unanalyzed Liberty timing arcs across all instantiated cells (summed/maxed across
    /// corners).
    pub unanalyzed_arcs: u32,
    /// Unconstrained endpoints broken down by reason (`STA-REQ-0225`).
    pub unconstrained_endpoints: UnconstrainedCounts,
}

impl ScopeCompleteness {
    /// True when the analysis is complete for `status.sta == "clean"`: no unanalyzed arcs and no
    /// unintentional unconstrained endpoints (`unclocked`, `no_arrival`, `unconstrained_output`).
    #[inline]
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.unanalyzed_arcs == 0 && !self.unconstrained_endpoints.has_unconstrained_gaps()
    }
}

/// The analysis scope stored in the `SCOPE` section of `sta.rbin` and in the command envelope
/// (`STA-REQ-0225`, `STA-REQ-0226`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisScope {
    /// Analysed corners in revision corner-list order.
    pub corners: Vec<String>,
    /// Wire model: `"zero"` or `"rc"`.
    pub wire_model: String,
    /// Clock model: `"ideal"`, `"propagated"`, or `"mixed"`.
    pub clock_model: String,
    /// CPPR mode: `"none"` or `"exact"`.
    pub cppr: String,
    /// POCV mode: `"none"`.
    pub pocv: String,
    /// Crosstalk mode: `"none"`.
    pub crosstalk: String,
    /// Counts of exceptions applied.
    pub exceptions_applied: ScopeExceptions,
    /// Completeness of the analysis.
    pub completeness: ScopeCompleteness,
}

impl Default for AnalysisScope {
    fn default() -> Self {
        Self {
            corners: Vec::new(),
            wire_model: "zero".into(),
            clock_model: "ideal".into(),
            cppr: "none".into(),
            pocv: "none".into(),
            crosstalk: "none".into(),
            exceptions_applied: ScopeExceptions::default(),
            completeness: ScopeCompleteness::default(),
        }
    }
}

impl AnalysisScope {
    /// Serializes this scope as canonical compact JSON for the `SCOPE` section of `sta.rbin`.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).expect("AnalysisScope serializes to JSON")
    }
}

/// Per-corner timing summary before aggregating across corners (`STA-REQ-0226`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CornerSummary {
    /// Corner name.
    pub corner: String,
    /// Worst setup slack (ns), or `None` when no setup check is evaluated in this corner.
    pub wns_setup_ns: Option<f64>,
    /// Total negative setup slack (ns, `<= 0.0`).
    pub tns_setup_ns: f64,
    /// Worst hold slack (ns), or `None` when no hold check is evaluated in this corner.
    pub wns_hold_ns: Option<f64>,
    /// Total negative hold slack (ns, `<= 0.0`).
    pub tns_hold_ns: f64,
    /// Number of endpoints with negative setup slack (`< 0.0`).
    pub violating_setup: u32,
    /// Number of endpoints with negative hold slack (`< 0.0`).
    pub violating_hold: u32,
    /// Number of constrained endpoints in this corner.
    pub endpoints: u32,
    /// Number of unconstrained endpoints in this corner (`UnconstrainedCounts::total()`).
    pub unconstrained_endpoints: u32,
}

/// Revision timing summary (`summary.sta` in `meta.json`; `STA-REQ-0226`), with each metric taken
/// as the worst over the revision's corners.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StaSummary {
    /// Worst setup slack across all corners (ns), or `None` when no setup check exists.
    pub wns_setup_ns: Option<f64>,
    /// Worst (most negative) total negative setup slack across all corners (ns, `<= 0.0`).
    pub tns_setup_ns: f64,
    /// Worst hold slack across all corners (ns), or `None` when no hold check exists.
    pub wns_hold_ns: Option<f64>,
    /// Worst (most negative) total negative hold slack across all corners (ns, `<= 0.0`).
    pub tns_hold_ns: f64,
    /// Maximum number of setup-violating endpoints across corners.
    pub violating_setup: u32,
    /// Maximum number of hold-violating endpoints across corners.
    pub violating_hold: u32,
    /// Maximum number of constrained endpoints across corners.
    pub endpoints: u32,
    /// Maximum number of unconstrained endpoints across corners.
    pub unconstrained_endpoints: u32,
}

impl StaSummary {
    /// Aggregates per-corner summaries into the worst-over-corners revision summary (`STA-REQ-0226`).
    #[must_use]
    pub fn from_corners(corners: &[CornerSummary]) -> Self {
        let mut wns_setup_ns: Option<f64> = None;
        let mut tns_setup_ns: f64 = 0.0;
        let mut wns_hold_ns: Option<f64> = None;
        let mut tns_hold_ns: f64 = 0.0;
        let mut violating_setup = 0u32;
        let mut violating_hold = 0u32;
        let mut endpoints = 0u32;
        let mut unconstrained_endpoints = 0u32;

        for c in corners {
            if let Some(w) = c.wns_setup_ns {
                let w = canon_zero(w);
                wns_setup_ns = Some(match wns_setup_ns {
                    Some(prev) => canon_zero(prev.min(w)),
                    None => w,
                });
            }
            tns_setup_ns = canon_zero(tns_setup_ns.min(canon_zero(c.tns_setup_ns)));
            if let Some(w) = c.wns_hold_ns {
                let w = canon_zero(w);
                wns_hold_ns = Some(match wns_hold_ns {
                    Some(prev) => canon_zero(prev.min(w)),
                    None => w,
                });
            }
            tns_hold_ns = canon_zero(tns_hold_ns.min(canon_zero(c.tns_hold_ns)));
            violating_setup = violating_setup.max(c.violating_setup);
            violating_hold = violating_hold.max(c.violating_hold);
            endpoints = endpoints.max(c.endpoints);
            unconstrained_endpoints = unconstrained_endpoints.max(c.unconstrained_endpoints);
        }

        Self {
            wns_setup_ns,
            tns_setup_ns,
            wns_hold_ns,
            tns_hold_ns,
            violating_setup,
            violating_hold,
            endpoints,
            unconstrained_endpoints,
        }
    }
}

/// Revision timing status (`status.sta` in `meta.json`; contract `CA-STATUS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaStatusValue {
    /// `violating_setup == 0 && violating_hold == 0` and the analysis is complete with at least
    /// one constrained endpoint.
    Clean,
    /// `violating_setup > 0 || violating_hold > 0` (even if the analysis is also incomplete).
    Violating,
    /// No setup or hold violations, but `completeness` has unanalyzed arcs or unconstrained
    /// endpoints (or `endpoints == 0`).
    Incomplete,
    /// Upstream inputs changed since the timing table was written.
    Stale,
    /// No timing analysis has been run on the revision.
    Missing,
}

impl StaStatusValue {
    /// Computes the `status.sta` value for a completed analysis from its summary and completeness.
    #[must_use]
    pub const fn compute(summary: &StaSummary, completeness: &ScopeCompleteness) -> Self {
        if summary.violating_setup > 0 || summary.violating_hold > 0 {
            Self::Violating
        } else if completeness.is_complete() && summary.endpoints > 0 {
            Self::Clean
        } else {
            Self::Incomplete
        }
    }

    /// Canonical string representation stored in `meta.json`.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Violating => "violating",
            Self::Incomplete => "incomplete",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_and_status_over_corners() {
        let c1 = CornerSummary {
            corner: "fast".into(),
            wns_setup_ns: Some(0.15),
            tns_setup_ns: 0.0,
            wns_hold_ns: Some(-0.04),
            tns_hold_ns: -0.07,
            violating_setup: 0,
            violating_hold: 2,
            endpoints: 4,
            unconstrained_endpoints: 0,
        };
        let c2 = CornerSummary {
            corner: "slow".into(),
            wns_setup_ns: Some(-0.12),
            tns_setup_ns: -0.25,
            wns_hold_ns: Some(0.08),
            tns_hold_ns: 0.0,
            violating_setup: 2,
            violating_hold: 0,
            endpoints: 4,
            unconstrained_endpoints: 0,
        };
        let sum = StaSummary::from_corners(&[c1, c2]);
        assert_eq!(sum.wns_setup_ns, Some(-0.12));
        assert_eq!(sum.tns_setup_ns, -0.25);
        assert_eq!(sum.wns_hold_ns, Some(-0.04));
        assert_eq!(sum.tns_hold_ns, -0.07);
        assert_eq!(sum.violating_setup, 2);
        assert_eq!(sum.violating_hold, 2);
        assert_eq!(sum.endpoints, 4);

        let comp = ScopeCompleteness::default();
        assert_eq!(
            StaStatusValue::compute(&sum, &comp),
            StaStatusValue::Violating
        );

        let clean_sum = StaSummary {
            wns_setup_ns: Some(0.1),
            tns_setup_ns: 0.0,
            wns_hold_ns: Some(0.05),
            tns_hold_ns: 0.0,
            violating_setup: 0,
            violating_hold: 0,
            endpoints: 4,
            unconstrained_endpoints: 0,
        };
        assert_eq!(
            StaStatusValue::compute(&clean_sum, &comp),
            StaStatusValue::Clean
        );

        let inc_comp = ScopeCompleteness {
            unanalyzed_arcs: 0,
            unconstrained_endpoints: UnconstrainedCounts {
                unclocked: 1,
                ..UnconstrainedCounts::default()
            },
        };
        assert_eq!(
            StaStatusValue::compute(&clean_sum, &inc_comp),
            StaStatusValue::Incomplete
        );
    }
}
