// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Exception matching, type precedence (`STA-REQ-0256`), specificity (`STA-REQ-0257`),
//! tie resolution (`STA-REQ-0258`), multicycle setup/hold relations (`STA-REQ-0260`,
//! `STA-REQ-0261`), clock groups (`STA-REQ-0266`), and diagnostics/scope accounting
//! (`STA-REQ-0270`, owner: E8).

use std::collections::{BTreeMap, BTreeSet};

use rusteda_core::diag::{Diag, Location};
use rusteda_formats::sdc::{CmdKind, EdgeScope, Sdc, SetupHoldScope, StartEndScope};

use super::state::{
    ExceptionId, ExceptionState, ExceptionStateTable, StartLaunchKey, StartMatchEntry,
};
use crate::engine::input::constr::{
    CompiledClockGroups, CompiledConstraints, CompiledException, ExceptionEndpoints, ExceptionKind,
};
use crate::engine::types::{ClockId, NodeId, Tr};

/// Timing analysis bound for exception evaluation (`STA-REQ-0250`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalysisKind {
    /// Late analysis: setup, recovery, latch setup, clock-gating setup, output `-max`.
    Setup,
    /// Early analysis: hold, removal, latch hold, clock-gating hold, output `-min`.
    Hold,
}

/// Precedence rank of an exception type (`STA-REQ-0256`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ExceptionTypeRank {
    /// `set_multicycle_path` (lowest exception type precedence).
    MulticyclePath = 1,
    /// `set_max_delay` (setup) or `set_min_delay` (hold).
    DelayBound = 2,
    /// `set_false_path` (highest path-exception type precedence).
    FalsePath = 3,
}

/// Specificity components of a matching exception (`STA-REQ-0257`).
pub mod specificity {
    /// Matched via a `-from` port, pin, or cell.
    pub const FROM_OBJECT: u8 = 16;
    /// Matched via a `-to` port, pin, or cell.
    pub const TO_OBJECT: u8 = 8;
    /// Exception specifies `-through` points (Phase 2b).
    pub const THROUGH: u8 = 4;
    /// Matched only via a `-from` clock.
    pub const FROM_CLOCK: u8 = 2;
    /// Matched only via a `-to` clock.
    pub const TO_CLOCK: u8 = 1;
}

/// Winning setup multicycle parameters `(N, T_s)` (`STA-REQ-0260`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupMulticycleWinner {
    /// Winning setup multicycle exception (`None` when default `N = 1` applies).
    pub exc_id: Option<ExceptionId>,
    /// Setup multiplier `N` (default `1`).
    pub multiplier: i64,
    /// True when `-start` was specified (`T_s = T_L`); false for default `-end` (`T_s = T_C`).
    pub use_launch_period: bool,
}

impl Default for SetupMulticycleWinner {
    fn default() -> Self {
        Self {
            exc_id: None,
            multiplier: 1,
            use_launch_period: false,
        }
    }
}

impl SetupMulticycleWinner {
    /// Returns the setup relation shift `(N - 1) * T_s` in ns (`STA-REQ-0260`).
    #[inline]
    #[must_use]
    pub fn shift_ns(self, launch_period_ns: f64, capture_period_ns: f64) -> f64 {
        let t_s = if self.use_launch_period {
            launch_period_ns
        } else {
            capture_period_ns
        };
        (self.multiplier - 1) as f64 * t_s
    }
}

/// Winning hold multicycle parameters `(M, T_h)` (`STA-REQ-0261`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldMulticycleWinner {
    /// Winning hold multicycle exception (`None` when no multicycle matched hold).
    pub exc_id: Option<ExceptionId>,
    /// Hold multiplier `M` (`M` from `-hold`-only, or `0` when both-analyses or no winner).
    pub multiplier: i64,
    /// True for default `-start` (`T_h = T_L`); false when `-end` was specified (`T_h = T_C`).
    pub use_launch_period: bool,
}

impl Default for HoldMulticycleWinner {
    fn default() -> Self {
        Self {
            exc_id: None,
            multiplier: 0,
            use_launch_period: true,
        }
    }
}

impl HoldMulticycleWinner {
    /// Returns the hold backward shift `M * T_h` in ns (`STA-REQ-0261`).
    #[inline]
    #[must_use]
    pub fn shift_ns(self, launch_period_ns: f64, capture_period_ns: f64) -> f64 {
        let t_h = if self.use_launch_period {
            launch_period_ns
        } else {
            capture_period_ns
        };
        self.multiplier as f64 * t_h
    }
}

/// Outcome of resolving all exceptions and clock groups for one `(path, analysis)` pair
/// (`STA-REQ-0256` to `STA-REQ-0266`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathAnalysisResolution {
    /// Path is untimed because `set_clock_groups` separates the launch and capture clocks
    /// (`STA-REQ-0266`).
    UntimedClockGroup,
    /// Path is untimed because a startpoint or endpoint is unclocked and no `set_max_delay` /
    /// `set_min_delay` won the analysis (`STA-REQ-0264`).
    UntimedUnclocked,
    /// Path is untimed in this analysis because `set_false_path` won (`STA-REQ-0259`).
    UntimedFalsePath {
        /// Winning `set_false_path` exception ID.
        exc_id: ExceptionId,
    },
    /// Path is timed in setup under standard or multicycle relation `D_s + (N - 1) * T_s`
    /// (`STA-REQ-0260`).
    TimedSetupMulticycle {
        /// Setup multicycle parameters `(N, T_s)`.
        setup: SetupMulticycleWinner,
    },
    /// Path is timed in hold under standard or multicycle relation
    /// `D_h + (N - 1) * T_s - M * T_h` (`STA-REQ-0261`).
    TimedHoldMulticycle {
        /// Setup multicycle parameters `(N, T_s)`.
        setup: SetupMulticycleWinner,
        /// Hold multicycle parameters `(M, T_h)`.
        hold: HoldMulticycleWinner,
    },
    /// Path is timed in setup under `set_max_delay V` (`STA-REQ-0262`, `STA-REQ-0265`).
    TimedMaxDelay {
        /// Winning `set_max_delay` exception ID.
        exc_id: ExceptionId,
        /// Delay target `V` in ns.
        max_delay_ns: f64,
        /// Whether `-ignore_clock_latency` was specified.
        ignore_clock_latency: bool,
    },
    /// Path is timed in hold under `set_min_delay V` (`STA-REQ-0263`, `STA-REQ-0265`).
    TimedMinDelay {
        /// Winning `set_min_delay` exception ID.
        exc_id: ExceptionId,
        /// Delay target `V` in ns.
        min_delay_ns: f64,
        /// Whether `-ignore_clock_latency` was specified.
        ignore_clock_latency: bool,
    },
}

impl PathAnalysisResolution {
    /// True if the path is timed in this analysis.
    #[inline]
    #[must_use]
    pub const fn is_timed(&self) -> bool {
        matches!(
            self,
            Self::TimedSetupMulticycle { .. }
                | Self::TimedHoldMulticycle { .. }
                | Self::TimedMaxDelay { .. }
                | Self::TimedMinDelay { .. }
        )
    }

    /// True if the path was excluded by `set_false_path` or `set_clock_groups`.
    #[inline]
    #[must_use]
    pub const fn is_excluded_by_exception_or_group(&self) -> bool {
        matches!(
            self,
            Self::UntimedClockGroup | Self::UntimedFalsePath { .. }
        )
    }
}

/// Precomputed clock-group separation lookup (`STA-REQ-0266`).
#[derive(Debug, Clone, Default)]
pub struct ClockGroupsIndex {
    num_clocks: usize,
    commands: Vec<CompiledClockGroups>,
}

impl ClockGroupsIndex {
    /// Builds the clock-groups index from compiled `set_clock_groups` commands and the number of
    /// defined clocks.
    #[must_use]
    pub fn new(commands: Vec<CompiledClockGroups>, num_clocks: usize) -> Self {
        Self {
            num_clocks,
            commands,
        }
    }

    /// Returns the number of `set_clock_groups` commands (`STA-REQ-0270`).
    #[inline]
    #[must_use]
    pub fn num_commands(&self) -> usize {
        self.commands.len()
    }

    /// Returns true if `launch` and `capture` are separated by any `set_clock_groups` command
    /// without `-allow_paths` (`STA-REQ-0266`).
    #[must_use]
    pub fn are_separated(&self, launch: ClockId, capture: ClockId) -> bool {
        if launch == capture {
            return false;
        }
        for cmd in &self.commands {
            if cmd.allow_paths {
                continue;
            }
            if cmd.groups.len() == 1 {
                let g0 = &cmd.groups[0];
                if g0.is_empty() {
                    continue;
                }
                let l_in = g0.contains(&launch) && (capture.0 as usize) < self.num_clocks;
                let c_in = g0.contains(&capture) && (launch.0 as usize) < self.num_clocks;
                if l_in ^ c_in {
                    return true;
                }
            } else if cmd.groups.len() >= 2 {
                let l_any = cmd.groups.iter().any(|g| g.contains(&launch));
                let c_any = cmd.groups.iter().any(|g| g.contains(&capture));
                if l_any && c_any {
                    let same_group = cmd
                        .groups
                        .iter()
                        .any(|g| g.contains(&launch) && g.contains(&capture));
                    if !same_group {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// Internal normalized representation of a single compiled exception rule.
#[derive(Debug, Clone)]
pub struct ExceptionRule {
    pub id: ExceptionId,
    pub order: usize,
    pub kind: ExceptionRuleKind,
    pub rise_fall: EdgeScope,
    pub from: Option<ExceptionEndpoints>,
    pub to: Option<ExceptionEndpoints>,
    pub has_through: bool,
    pub location: Option<Location>,
}

/// Normalized exception kind supporting signed multicycle multipliers (`STA-REQ-0250`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExceptionRuleKind {
    FalsePath {
        setup_hold: SetupHoldScope,
    },
    MulticyclePath {
        multiplier: i64,
        setup_hold: SetupHoldScope,
        start_end: StartEndScope,
    },
    MaxDelay {
        max_delay_ns: f64,
        ignore_clock_latency: bool,
    },
    MinDelay {
        min_delay_ns: f64,
        ignore_clock_latency: bool,
    },
}

impl ExceptionRule {
    /// Returns the [`ExceptionTypeRank`] of this exception (`STA-REQ-0256`).
    #[inline]
    #[must_use]
    pub const fn type_rank(&self) -> ExceptionTypeRank {
        match self.kind {
            ExceptionRuleKind::FalsePath { .. } => ExceptionTypeRank::FalsePath,
            ExceptionRuleKind::MaxDelay { .. } | ExceptionRuleKind::MinDelay { .. } => {
                ExceptionTypeRank::DelayBound
            }
            ExceptionRuleKind::MulticyclePath { .. } => ExceptionTypeRank::MulticyclePath,
        }
    }

    /// True if this exception applies to `analysis` (`STA-REQ-0250`).
    #[inline]
    #[must_use]
    pub const fn applies_to(&self, analysis: AnalysisKind) -> bool {
        match (self.kind, analysis) {
            (ExceptionRuleKind::FalsePath { setup_hold }, AnalysisKind::Setup)
            | (ExceptionRuleKind::MulticyclePath { setup_hold, .. }, AnalysisKind::Setup) => {
                matches!(setup_hold, SetupHoldScope::Setup | SetupHoldScope::Both)
            }
            (ExceptionRuleKind::FalsePath { setup_hold }, AnalysisKind::Hold)
            | (ExceptionRuleKind::MulticyclePath { setup_hold, .. }, AnalysisKind::Hold) => {
                matches!(setup_hold, SetupHoldScope::Hold | SetupHoldScope::Both)
            }
            (ExceptionRuleKind::MaxDelay { .. }, AnalysisKind::Setup) => true,
            (ExceptionRuleKind::MaxDelay { .. }, AnalysisKind::Hold) => false,
            (ExceptionRuleKind::MinDelay { .. }, AnalysisKind::Hold) => true,
            (ExceptionRuleKind::MinDelay { .. }, AnalysisKind::Setup) => false,
        }
    }

    /// Scope rank (`1` if restricted to a single analysis, `0` if applicable to both analyses;
    /// `STA-REQ-0258`).
    #[inline]
    #[must_use]
    pub const fn single_analysis_rank(&self) -> u8 {
        match self.kind {
            ExceptionRuleKind::FalsePath { setup_hold }
            | ExceptionRuleKind::MulticyclePath { setup_hold, .. } => match setup_hold {
                SetupHoldScope::Setup | SetupHoldScope::Hold => 1,
                SetupHoldScope::Both => 0,
            },
            ExceptionRuleKind::MaxDelay { .. } | ExceptionRuleKind::MinDelay { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RankedCandidate {
    exc_id: ExceptionId,
    type_rank: ExceptionTypeRank,
    specificity: u8,
    scope_rank: u8,
    order: usize,
}

impl RankedCandidate {
    #[inline]
    fn rank_key(self) -> (ExceptionTypeRank, u8, u8, usize) {
        (
            self.type_rank,
            self.specificity,
            self.scope_rank,
            self.order,
        )
    }

    #[inline]
    fn tie_group_key(self) -> (ExceptionTypeRank, u8, u8) {
        (self.type_rank, self.specificity, self.scope_rank)
    }
}

/// Endpoint status classification (`STA-REQ-0226`, `STA-REQ-0259`, `STA-REQ-0270`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointStatus {
    /// At least one timed check (setup or hold) applies to the endpoint.
    Constrained,
    /// Sequential check pin whose related clock pin is not in any clock network.
    Unclocked,
    /// Clocked check pin that no launch tag reaches.
    NoDataArrival,
    /// Every path reaching the endpoint is excluded by `set_false_path` or `set_clock_groups`.
    FalsePathOnly,
    /// Top-level output port without `set_output_delay` and without an applicable `set_max_delay`
    /// or `set_min_delay`.
    UnconstrainedOutput,
    /// Endpoint whose check arcs are disabled by `set_disable_timing` or `when`.
    Disabled,
}

impl EndpointStatus {
    /// Canonical report key for this status (`STA-REQ-0226`, `STA-REQ-0270`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constrained => "constrained",
            Self::Unclocked => "unclocked",
            Self::NoDataArrival => "no_data_arrival",
            Self::FalsePathOnly => "false_path_only",
            Self::UnconstrainedOutput => "unconstrained_output",
            Self::Disabled => "disabled",
        }
    }
}

/// Exception and endpoint scope summary for run reports (`STA-REQ-0270`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExceptionScopeSummary {
    /// Count of compiled `set_false_path` exceptions.
    pub false_path: usize,
    /// Count of compiled `set_multicycle_path` exceptions.
    pub multicycle_path: usize,
    /// Count of compiled `set_max_delay` exceptions.
    pub max_delay: usize,
    /// Count of compiled `set_min_delay` exceptions.
    pub min_delay: usize,
    /// Count of compiled `set_clock_groups` commands.
    pub clock_groups: usize,
    /// Count of unused exceptions (`D0112`).
    pub unused: usize,
    /// Count of endpoints with status `false_path_only`.
    pub false_path_only_endpoints: usize,
    /// Breakdown of unconstrained endpoints by status reason (`STA-REQ-0226`, `STA-REQ-0270`).
    pub unconstrained_by_reason: BTreeMap<String, usize>,
}

/// Evaluated timing result for a single endpoint under exceptions (`STA-REQ-0250` to `STA-REQ-0270`).
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointExceptionResult {
    /// Canonical endpoint pin or port name.
    pub pin: String,
    /// Endpoint [`NodeId`].
    pub node: NodeId,
    /// Worst setup slack (ns) across all timed paths to this endpoint, or `None` if untimed.
    pub setup_slack: Option<f64>,
    /// Worst hold slack (ns) across all timed paths to this endpoint, or `None` if untimed.
    pub hold_slack: Option<f64>,
    /// Endpoint status (`constrained`, `false_path_only`, etc.).
    pub status: EndpointStatus,
}

/// Compiled exception table and stateful tracker for used exceptions (`D0112`) and tie conflicts
/// (`D0111`).
#[derive(Debug, Clone)]
pub struct ExceptionTable {
    rules: Vec<ExceptionRule>,
    /// Indices of rules that have `from: None` (unrestricted startpoint).
    unfiltered_from_rules: Vec<ExceptionId>,
    clock_groups: ClockGroupsIndex,
    used: Vec<bool>,
    tie_conflicts: BTreeSet<(ExceptionId, ExceptionId)>,
}

impl ExceptionTable {
    /// Builds an [`ExceptionTable`] from [`CompiledConstraints`].
    #[must_use]
    pub fn from_compiled(compiled: &CompiledConstraints) -> Self {
        Self::from_compiled_with_sdc(compiled, None)
    }

    /// Builds an [`ExceptionTable`] from [`CompiledConstraints`], optionally consulting `sdc` to
    /// preserve signed multicycle multipliers (`STA-REQ-0250`).
    #[must_use]
    pub fn from_compiled_with_sdc(compiled: &CompiledConstraints, sdc: Option<&Sdc>) -> Self {
        let mut sdc_multipliers: BTreeMap<(String, u32, u32), i64> = BTreeMap::new();
        if let Some(sdc_doc) = sdc {
            for cmd in &sdc_doc.cmds {
                if let CmdKind::SetMulticyclePath { multiplier, .. } = cmd.kind
                    && let Some(src) = sdc_doc.sources.get(cmd.source as usize)
                {
                    sdc_multipliers
                        .insert((src.path.clone(), cmd.line, cmd.col), i64::from(multiplier));
                }
            }
        }

        let mut rules = Vec::with_capacity(compiled.exceptions.len());
        let mut unfiltered_from_rules = Vec::new();
        for (i, exc) in compiled.exceptions.iter().enumerate() {
            let id = ExceptionId(i as u32);
            let kind = convert_kind(exc, &sdc_multipliers);
            if exc.from.is_none() {
                unfiltered_from_rules.push(id);
            }
            rules.push(ExceptionRule {
                id,
                order: exc.order,
                kind,
                rise_fall: exc.edge,
                from: exc.from.clone(),
                to: exc.to.clone(),
                has_through: false,
                location: exc.location.clone(),
            });
        }

        let clock_groups =
            ClockGroupsIndex::new(compiled.clock_groups.clone(), compiled.clocks.len());
        let used = vec![false; rules.len()];

        Self {
            rules,
            unfiltered_from_rules,
            clock_groups,
            used,
            tie_conflicts: BTreeSet::new(),
        }
    }

    /// Returns the slice of compiled [`ExceptionRule`]s.
    #[inline]
    #[must_use]
    pub fn rules(&self) -> &[ExceptionRule] {
        &self.rules
    }

    /// Returns a reference to the [`ClockGroupsIndex`].
    #[inline]
    #[must_use]
    pub fn clock_groups(&self) -> &ClockGroupsIndex {
        &self.clock_groups
    }

    /// Returns true if `port_node` can be timed as an unclocked input startpoint by any
    /// `set_max_delay` or `set_min_delay` exception (`STA-REQ-0264`).
    #[must_use]
    pub fn has_unclocked_from_delay_exception(&self, port_node: NodeId) -> bool {
        self.rules.iter().any(|r| {
            matches!(
                r.kind,
                ExceptionRuleKind::MaxDelay { .. } | ExceptionRuleKind::MinDelay { .. }
            ) && match &r.from {
                None => true,
                Some(ep) => ep.has_node(port_node),
            }
        })
    }

    /// Returns true if `port_node` can be timed as an unclocked output endpoint by any
    /// `set_max_delay` or `set_min_delay` exception (`STA-REQ-0264`).
    #[must_use]
    pub fn has_unclocked_to_delay_exception(&self, port_node: NodeId) -> bool {
        self.rules.iter().any(|r| {
            matches!(
                r.kind,
                ExceptionRuleKind::MaxDelay { .. } | ExceptionRuleKind::MinDelay { .. }
            ) && match &r.to {
                None => true,
                Some(ep) => ep.has_node(port_node),
            }
        })
    }

    /// Matches `-from` / `-rise_from` / `-fall_from` rules at `start` and interns the resulting
    /// [`ExceptionState`] in `state_table` (`STA-REQ-0251`, `STA-REQ-0252`, `STA-REQ-0255`).
    pub fn match_start(
        &self,
        state_table: &mut ExceptionStateTable,
        start: StartLaunchKey,
    ) -> ExceptionState {
        let mut entries = Vec::new();
        for rule in &self.rules {
            let Some(ep) = &rule.from else {
                continue;
            };
            if ep.has_node(start.start_node) && edge_matches(ep.edge, start.start_tr) {
                entries.push(StartMatchEntry {
                    exc_id: rule.id,
                    from_spec: specificity::FROM_OBJECT,
                });
            } else if let Some((l_clk, l_edge)) = start.launch_clock
                && ep.clocks.contains(&l_clk)
                && edge_matches(ep.edge, l_edge)
            {
                entries.push(StartMatchEntry {
                    exc_id: rule.id,
                    from_spec: specificity::FROM_CLOCK,
                });
            }
        }
        state_table.intern(entries)
    }

    /// Resolves the winning exception and multicycle/delay parameters for a single path and
    /// analysis (`STA-REQ-0256` to `STA-REQ-0266`).
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_path_analysis(
        &mut self,
        state_table: &ExceptionStateTable,
        exc_state: ExceptionState,
        launch_clock: Option<(ClockId, Tr)>,
        end_node: NodeId,
        end_tr: Tr,
        capture_clock: Option<(ClockId, Tr)>,
        analysis: AnalysisKind,
    ) -> PathAnalysisResolution {
        // 1. Clock groups apply first (`STA-REQ-0256`, `STA-REQ-0266`).
        if let (Some((l_clk, _)), Some((c_clk, _))) = (launch_clock, capture_clock)
            && self.clock_groups.are_separated(l_clk, c_clk)
        {
            return PathAnalysisResolution::UntimedClockGroup;
        }

        let is_unclocked = launch_clock.is_none() || capture_clock.is_none();

        // 2. Collect all matching exceptions for `(exc_state, end_node, end_tr, capture_clock)`.
        let state_data = state_table.get(exc_state);
        let mut all_matching: Vec<(ExceptionId, u8)> = Vec::new();

        for entry in &state_data.entries {
            let rule = &self.rules[entry.exc_id.index()];
            if let Some(to_spec) = match_to_specificity(rule, end_node, end_tr, capture_clock) {
                let through_spec = if rule.has_through {
                    specificity::THROUGH
                } else {
                    0
                };
                all_matching.push((entry.exc_id, entry.from_spec + to_spec + through_spec));
            }
        }
        for &exc_id in &self.unfiltered_from_rules {
            let rule = &self.rules[exc_id.index()];
            if let Some(to_spec) = match_to_specificity(rule, end_node, end_tr, capture_clock) {
                let through_spec = if rule.has_through {
                    specificity::THROUGH
                } else {
                    0
                };
                all_matching.push((exc_id, to_spec + through_spec));
            }
        }

        // 3. Unclocked ends (`STA-REQ-0264`): multicycle paths do not apply, and the path is timed
        // only when `set_max_delay` (setup) or `set_min_delay` (hold) wins.
        if is_unclocked {
            let mut candidates: Vec<RankedCandidate> = all_matching
                .iter()
                .copied()
                .filter_map(|(id, spec)| {
                    let rule = &self.rules[id.index()];
                    if !rule.applies_to(analysis)
                        || matches!(rule.kind, ExceptionRuleKind::MulticyclePath { .. })
                    {
                        return None;
                    }
                    Some(RankedCandidate {
                        exc_id: id,
                        type_rank: rule.type_rank(),
                        specificity: spec,
                        scope_rank: rule.single_analysis_rank(),
                        order: rule.order,
                    })
                })
                .collect();

            let has_delay_candidate = candidates
                .iter()
                .any(|c| c.type_rank == ExceptionTypeRank::DelayBound);
            if !has_delay_candidate {
                return PathAnalysisResolution::UntimedUnclocked;
            }

            let Some(winner) = select_winner_and_record_ties(
                &self.rules,
                &mut candidates,
                analysis,
                &mut self.tie_conflicts,
            ) else {
                return PathAnalysisResolution::UntimedUnclocked;
            };
            self.used[winner.exc_id.index()] = true;
            return match self.rules[winner.exc_id.index()].kind {
                ExceptionRuleKind::FalsePath { .. } => PathAnalysisResolution::UntimedFalsePath {
                    exc_id: winner.exc_id,
                },
                ExceptionRuleKind::MaxDelay {
                    max_delay_ns,
                    ignore_clock_latency,
                } => PathAnalysisResolution::TimedMaxDelay {
                    exc_id: winner.exc_id,
                    max_delay_ns,
                    ignore_clock_latency,
                },
                ExceptionRuleKind::MinDelay {
                    min_delay_ns,
                    ignore_clock_latency,
                } => PathAnalysisResolution::TimedMinDelay {
                    exc_id: winner.exc_id,
                    min_delay_ns,
                    ignore_clock_latency,
                },
                ExceptionRuleKind::MulticyclePath { .. } => {
                    PathAnalysisResolution::UntimedUnclocked
                }
            };
        }

        // 4. Clocked path: rank all matching candidates applicable to `analysis`.
        let mut candidates: Vec<RankedCandidate> = all_matching
            .iter()
            .copied()
            .filter_map(|(id, spec)| {
                let rule = &self.rules[id.index()];
                if !rule.applies_to(analysis) {
                    return None;
                }
                Some(RankedCandidate {
                    exc_id: id,
                    type_rank: rule.type_rank(),
                    specificity: spec,
                    scope_rank: rule.single_analysis_rank(),
                    order: rule.order,
                })
            })
            .collect();

        let winner = select_winner_and_record_ties(
            &self.rules,
            &mut candidates,
            analysis,
            &mut self.tie_conflicts,
        );

        match analysis {
            AnalysisKind::Setup => match winner {
                Some(w) => {
                    self.used[w.exc_id.index()] = true;
                    match self.rules[w.exc_id.index()].kind {
                        ExceptionRuleKind::FalsePath { .. } => {
                            PathAnalysisResolution::UntimedFalsePath { exc_id: w.exc_id }
                        }
                        ExceptionRuleKind::MaxDelay {
                            max_delay_ns,
                            ignore_clock_latency,
                        } => PathAnalysisResolution::TimedMaxDelay {
                            exc_id: w.exc_id,
                            max_delay_ns,
                            ignore_clock_latency,
                        },
                        ExceptionRuleKind::MulticyclePath {
                            multiplier,
                            start_end,
                            ..
                        } => PathAnalysisResolution::TimedSetupMulticycle {
                            setup: SetupMulticycleWinner {
                                exc_id: Some(w.exc_id),
                                multiplier,
                                use_launch_period: matches!(start_end, StartEndScope::Start),
                            },
                        },
                        ExceptionRuleKind::MinDelay { .. } => unreachable!(),
                    }
                }
                None => PathAnalysisResolution::TimedSetupMulticycle {
                    setup: SetupMulticycleWinner::default(),
                },
            },
            AnalysisKind::Hold => {
                if let Some(w) = winner {
                    match self.rules[w.exc_id.index()].kind {
                        ExceptionRuleKind::FalsePath { .. } => {
                            self.used[w.exc_id.index()] = true;
                            return PathAnalysisResolution::UntimedFalsePath { exc_id: w.exc_id };
                        }
                        ExceptionRuleKind::MinDelay {
                            min_delay_ns,
                            ignore_clock_latency,
                        } => {
                            self.used[w.exc_id.index()] = true;
                            return PathAnalysisResolution::TimedMinDelay {
                                exc_id: w.exc_id,
                                min_delay_ns,
                                ignore_clock_latency,
                            };
                        }
                        ExceptionRuleKind::MulticyclePath { .. } => {}
                        ExceptionRuleKind::MaxDelay { .. } => unreachable!(),
                    }
                }

                // Hold uses `D_h + (N - 1) * T_s - M * T_h` (`STA-REQ-0261`).
                // Resolve setup multicycle winner `(N, T_s)` over all setup multicycle candidates on
                // this path, even if `set_false_path -setup` or `set_max_delay` won setup!
                let mut setup_mcp_candidates: Vec<RankedCandidate> = all_matching
                    .iter()
                    .copied()
                    .filter_map(|(id, spec)| {
                        let rule = &self.rules[id.index()];
                        if matches!(rule.kind, ExceptionRuleKind::MulticyclePath { .. })
                            && rule.applies_to(AnalysisKind::Setup)
                        {
                            Some(RankedCandidate {
                                exc_id: id,
                                type_rank: ExceptionTypeRank::MulticyclePath,
                                specificity: spec,
                                scope_rank: rule.single_analysis_rank(),
                                order: rule.order,
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                let setup_winner = select_winner_and_record_ties(
                    &self.rules,
                    &mut setup_mcp_candidates,
                    AnalysisKind::Setup,
                    &mut self.tie_conflicts,
                );
                let setup = match setup_winner {
                    Some(sw) => {
                        self.used[sw.exc_id.index()] = true;
                        let ExceptionRuleKind::MulticyclePath {
                            multiplier,
                            start_end,
                            ..
                        } = self.rules[sw.exc_id.index()].kind
                        else {
                            unreachable!()
                        };
                        SetupMulticycleWinner {
                            exc_id: Some(sw.exc_id),
                            multiplier,
                            use_launch_period: matches!(start_end, StartEndScope::Start),
                        }
                    }
                    None => SetupMulticycleWinner::default(),
                };

                let hold = match winner {
                    Some(hw) => {
                        self.used[hw.exc_id.index()] = true;
                        let ExceptionRuleKind::MulticyclePath {
                            multiplier,
                            setup_hold,
                            start_end,
                        } = self.rules[hw.exc_id.index()].kind
                        else {
                            unreachable!()
                        };
                        let m = match setup_hold {
                            SetupHoldScope::Hold => multiplier,
                            SetupHoldScope::Both | SetupHoldScope::Setup => 0,
                        };
                        HoldMulticycleWinner {
                            exc_id: Some(hw.exc_id),
                            multiplier: m,
                            use_launch_period: !matches!(start_end, StartEndScope::End),
                        }
                    }
                    None => HoldMulticycleWinner::default(),
                };

                PathAnalysisResolution::TimedHoldMulticycle { setup, hold }
            }
        }
    }

    /// Returns whether exception `id` was used on at least one timed path (`STA-REQ-0270`).
    #[inline]
    #[must_use]
    pub fn is_used(&self, id: ExceptionId) -> bool {
        self.used.get(id.index()).copied().unwrap_or(false)
    }

    /// Produces the final list of diagnostics (combining `base_diags` with `D0111` tie warnings and
    /// `D0112` unused-exception notes) and the [`ExceptionScopeSummary`] (`STA-REQ-0270`).
    #[must_use]
    pub fn finalize_diagnostics_and_scope(
        &self,
        base_diags: &[Diag],
        endpoints: &[EndpointExceptionResult],
    ) -> (Vec<Diag>, ExceptionScopeSummary) {
        let mut diags = base_diags.to_vec();

        // 1. Emit `D0111` once per conflicting `(earlier, winner)` pair with both locations.
        for &(earlier_id, winner_id) in &self.tie_conflicts {
            let earlier = &self.rules[earlier_id.index()];
            let winner = &self.rules[winner_id.index()];
            let earlier_loc_str = format_loc(&earlier.location);
            let winner_loc_str = format_loc(&winner.location);
            let mut d = Diag::warning(
                "D0111",
                format!(
                    "conflicting timing exceptions of equal precedence and specificity: earlier command at {earlier_loc_str} is overridden by later command at {winner_loc_str} (STA-REQ-0258)"
                ),
            );
            if let Some(loc) = &winner.location {
                d = d.at(loc.file.clone(), loc.line, loc.column);
            } else if let Some(loc) = &earlier.location {
                d = d.at(loc.file.clone(), loc.line, loc.column);
            }
            diags.push(d);
        }

        // 2. Emit `D0112` for each compiled exception that was never used (`STA-REQ-0270`).
        let mut false_path = 0usize;
        let mut multicycle_path = 0usize;
        let mut max_delay = 0usize;
        let mut min_delay = 0usize;
        let mut unused = 0usize;

        for rule in &self.rules {
            match rule.kind {
                ExceptionRuleKind::FalsePath { .. } => false_path += 1,
                ExceptionRuleKind::MulticyclePath { .. } => multicycle_path += 1,
                ExceptionRuleKind::MaxDelay { .. } => max_delay += 1,
                ExceptionRuleKind::MinDelay { .. } => min_delay += 1,
            }
            if !self.used[rule.id.index()] {
                unused += 1;
                let mut d = Diag::note(
                    "D0112",
                    format!(
                        "timing exception #{} wins no analysis of any timed path and supplies no setup multiplier (STA-REQ-0270)",
                        rule.order + 1
                    ),
                );
                if let Some(loc) = &rule.location {
                    d = d.at(loc.file.clone(), loc.line, loc.column);
                }
                diags.push(d);
            }
        }

        let mut unconstrained_by_reason: BTreeMap<String, usize> = BTreeMap::new();
        let mut false_path_only_endpoints = 0usize;
        for ep in endpoints {
            if ep.status != EndpointStatus::Constrained {
                *unconstrained_by_reason
                    .entry(ep.status.as_str().to_string())
                    .or_insert(0) += 1;
                if ep.status == EndpointStatus::FalsePathOnly {
                    false_path_only_endpoints += 1;
                }
            }
        }

        let scope = ExceptionScopeSummary {
            false_path,
            multicycle_path,
            max_delay,
            min_delay,
            clock_groups: self.clock_groups.num_commands(),
            unused,
            false_path_only_endpoints,
            unconstrained_by_reason,
        };

        (diags, scope)
    }
}

fn convert_kind(
    exc: &CompiledException,
    sdc_multipliers: &BTreeMap<(String, u32, u32), i64>,
) -> ExceptionRuleKind {
    match exc.kind {
        ExceptionKind::FalsePath { setup_hold } => ExceptionRuleKind::FalsePath { setup_hold },
        ExceptionKind::MulticyclePath {
            multiplier,
            setup_hold,
            start_end,
            ..
        } => {
            let signed_mult = exc
                .location
                .as_ref()
                .and_then(|l| {
                    sdc_multipliers
                        .get(&(l.file.clone(), l.line, l.column))
                        .copied()
                })
                .unwrap_or(i64::from(multiplier));
            ExceptionRuleKind::MulticyclePath {
                multiplier: signed_mult,
                setup_hold,
                start_end,
            }
        }
        ExceptionKind::MaxDelay {
            delay_ns,
            ignore_clock_latency,
        } => ExceptionRuleKind::MaxDelay {
            max_delay_ns: delay_ns,
            ignore_clock_latency,
        },
        ExceptionKind::MinDelay {
            delay_ns,
            ignore_clock_latency,
        } => ExceptionRuleKind::MinDelay {
            min_delay_ns: delay_ns,
            ignore_clock_latency,
        },
    }
}

#[inline]
fn edge_matches(scope: EdgeScope, tr: Tr) -> bool {
    match (scope, tr) {
        (EdgeScope::Both, _) => true,
        (EdgeScope::Rise, Tr::Rise) | (EdgeScope::Fall, Tr::Fall) => true,
        (EdgeScope::Rise, Tr::Fall) | (EdgeScope::Fall, Tr::Rise) => false,
    }
}

fn match_to_specificity(
    rule: &ExceptionRule,
    end_node: NodeId,
    end_tr: Tr,
    capture_clock: Option<(ClockId, Tr)>,
) -> Option<u8> {
    if !edge_matches(rule.rise_fall, end_tr) {
        return None;
    }
    let Some(ep) = &rule.to else {
        return Some(0);
    };
    if ep.has_node(end_node) && edge_matches(ep.edge, end_tr) {
        Some(specificity::TO_OBJECT)
    } else if let Some((c_clk, c_edge)) = capture_clock
        && ep.clocks.contains(&c_clk)
        && edge_matches(ep.edge, c_edge)
    {
        Some(specificity::TO_CLOCK)
    } else {
        None
    }
}

fn select_winner_and_record_ties(
    rules: &[ExceptionRule],
    candidates: &mut [RankedCandidate],
    analysis: AnalysisKind,
    tie_conflicts: &mut BTreeSet<(ExceptionId, ExceptionId)>,
) -> Option<RankedCandidate> {
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by_key(|c| c.rank_key());
    let winner = *candidates.last().expect("non-empty candidates");
    let top_group = winner.tie_group_key();
    let winner_rule = &rules[winner.exc_id.index()];

    for cand in candidates.iter().rev().skip(1) {
        if cand.tie_group_key() != top_group {
            break;
        }
        let earlier_rule = &rules[cand.exc_id.index()];
        if effects_differ(earlier_rule, winner_rule, analysis) {
            tie_conflicts.insert((cand.exc_id, winner.exc_id));
        }
    }

    Some(winner)
}

fn effects_differ(a: &ExceptionRule, b: &ExceptionRule, analysis: AnalysisKind) -> bool {
    match (a.kind, b.kind) {
        (ExceptionRuleKind::FalsePath { .. }, ExceptionRuleKind::FalsePath { .. }) => false,
        (
            ExceptionRuleKind::MulticyclePath {
                multiplier: m1,
                setup_hold: sh1,
                start_end: pe1,
            },
            ExceptionRuleKind::MulticyclePath {
                multiplier: m2,
                setup_hold: sh2,
                start_end: pe2,
            },
        ) => {
            let eff_mult = |m: i64, sh: SetupHoldScope| match (analysis, sh) {
                (AnalysisKind::Hold, SetupHoldScope::Both) => 0,
                _ => m,
            };
            let eff_use_launch = |pe: StartEndScope| match analysis {
                AnalysisKind::Setup => matches!(pe, StartEndScope::Start),
                AnalysisKind::Hold => !matches!(pe, StartEndScope::End),
            };
            eff_mult(m1, sh1) != eff_mult(m2, sh2) || eff_use_launch(pe1) != eff_use_launch(pe2)
        }
        (
            ExceptionRuleKind::MaxDelay {
                max_delay_ns: v1,
                ignore_clock_latency: ig1,
            },
            ExceptionRuleKind::MaxDelay {
                max_delay_ns: v2,
                ignore_clock_latency: ig2,
            },
        ) => (v1 - v2).abs() > 1e-12 || ig1 != ig2,
        (
            ExceptionRuleKind::MinDelay {
                min_delay_ns: v1,
                ignore_clock_latency: ig1,
            },
            ExceptionRuleKind::MinDelay {
                min_delay_ns: v2,
                ignore_clock_latency: ig2,
            },
        ) => (v1 - v2).abs() > 1e-12 || ig1 != ig2,
        _ => true,
    }
}

fn format_loc(loc: &Option<Location>) -> String {
    match loc {
        Some(l) if !l.file.is_empty() => format!("{}:{}:{}", l.file, l.line, l.column),
        Some(l) => format!("line {}:{}", l.line, l.column),
        None => "<unknown>".to_string(),
    }
}
