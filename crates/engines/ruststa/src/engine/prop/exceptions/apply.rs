// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions in the analysis of a corner (owner: E8; `STA-REQ-0250` to `STA-REQ-0271`).
//!
//! The `-from` state of each startpoint selects the propagation tag that carries it
//! ([`ExceptionContext::start_tag`]); every evaluated check then resolves the exceptions and clock
//! groups that match its path ([`ExceptionContext::apply_to_records`]): a false path or a clock
//! group removes the check, a multicycle path shifts its relation, and a maximum or minimum delay
//! replaces it. The checks of transparent latches resolve where their window is known
//! ([`ExceptionContext::resolve`]). Without exceptions and clock groups the context is inactive and
//! the analysis is unchanged.

use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_core::job::Diag;
use rusteda_core::liberty::Direction;

use super::resolve::{AnalysisKind, ExceptionRuleKind, ExceptionTable, PathAnalysisResolution};
use super::state::{ExceptionState, ExceptionStateTable, StartLaunchKey};
use crate::engine::graph::TimingGraph;
use crate::engine::input::constr::CompiledConstraints;
use crate::engine::input::{NetlistView, NodeKind};
use crate::engine::prop::checks::{EndpointCheckKind, SlackRecord};
use crate::engine::prop::clocks::ClockModel;
use crate::engine::prop::clocks::ideal::IdealClockModel;
use crate::engine::prop::tags::{Tag, TagTable};
use crate::engine::types::{ClockId, ExcStateId, NodeId, TagId, Tr};

/// The timing exceptions of one corner analysis (`STA-REQ-0271`).
#[derive(Debug, Clone)]
pub struct ExceptionContext {
    table: Option<ExceptionTable>,
    states: ExceptionStateTable,
    /// Tag of each startpoint whose `-from` state is not empty, keyed by
    /// `(start node, start transition, launch clock, launch source edge)`.
    starts: FxHashMap<(NodeId, Tr, ClockId, Tr), TagId>,
}

impl ExceptionContext {
    /// A context without exceptions: every startpoint launches its base tag and every check stays.
    #[must_use]
    pub fn inactive() -> Self {
        Self {
            table: None,
            states: ExceptionStateTable::new(),
            starts: FxHashMap::default(),
        }
    }

    /// True when the constraints have timing exceptions or clock groups.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.table.is_some()
    }

    /// Builds the context of `constraints` and interns into `tags` the tag of every startpoint
    /// whose `-from` state is not empty: input ports with input delays, clock sources, and the
    /// clock pins of flip-flops and the enable pins of latches (`STA-REQ-0251`, `STA-REQ-0252`).
    ///
    /// Constructs this version does not analyse are never dropped silently (AGENTS rule 7.1):
    /// `-ignore_clock_latency` is error `T0071`, and a maximum or minimum delay that could time a
    /// path from or to a port without input or output delay (STA-REQ-0264) gets warning `T0072`,
    /// returned with the context.
    pub fn build(
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        constraints: &CompiledConstraints,
        clock_model: &IdealClockModel,
        tags: &mut TagTable,
    ) -> Result<(Self, Vec<Diag>), Diag> {
        if constraints.exceptions.is_empty() && constraints.clock_groups.is_empty() {
            return Ok((Self::inactive(), Vec::new()));
        }
        let table = ExceptionTable::from_compiled(constraints);
        let diags = unsupported_diagnostics(netview, constraints, &table)?;

        let mut ctx = Self {
            table: Some(table),
            states: ExceptionStateTable::new(),
            starts: FxHashMap::default(),
        };
        for d in &constraints.input_delays {
            let Some(clock) = d.clock else {
                continue;
            };
            for tr in Tr::ALL {
                ctx.add_start(tags, d.node, tr, clock, d.clock_edge)?;
            }
        }
        for (ci, ck) in constraints.clocks.iter().enumerate() {
            for &src in &ck.sources {
                for edge in Tr::ALL {
                    ctx.add_start(tags, src, edge, ClockId(ci as u32), edge)?;
                }
            }
        }
        for l in graph.launches() {
            for &(clock, _) in clock_model.pin_clocks(l.clock_pin) {
                for edge in Tr::ALL {
                    ctx.add_start(tags, l.clock_pin, l.edge, clock, edge)?;
                }
            }
        }
        Ok((ctx, diags))
    }

    fn add_start(
        &mut self,
        tags: &mut TagTable,
        node: NodeId,
        start_tr: Tr,
        clock: ClockId,
        edge: Tr,
    ) -> Result<(), Diag> {
        let Some(table) = &self.table else {
            return Ok(());
        };
        let key = StartLaunchKey {
            start_node: node,
            start_tr,
            launch_clock: Some((clock, edge)),
        };
        let state = table.match_start(&mut self.states, key);
        if state != ExceptionState::EMPTY {
            let tid = tags.intern(Tag::with_exc(clock, edge, ExcStateId(state.0)))?;
            self.starts.insert((node, start_tr, clock, edge), tid);
        }
        Ok(())
    }

    /// The tag launched at startpoint `node` with start transition `start_tr` by clock `clock`
    /// at source edge `edge`: the tag of its `-from` state, else the base tag of the clock edge.
    #[must_use]
    pub fn start_tag(
        &self,
        tags: &TagTable,
        node: NodeId,
        start_tr: Tr,
        clock: ClockId,
        edge: Tr,
    ) -> Option<TagId> {
        self.starts
            .get(&(node, start_tr, clock, edge))
            .copied()
            .or_else(|| tags.base_tag(clock, edge))
    }

    /// Resolves the exceptions and clock groups of the path of `tag` that ends at `end` with
    /// transition `end_tr`, captured by `capture` (`STA-REQ-0256` to `STA-REQ-0266`). An inactive
    /// context resolves every path to the standard relation.
    pub fn resolve(
        &mut self,
        tag: Tag,
        end: NodeId,
        end_tr: Tr,
        capture: (ClockId, Tr),
        analysis: AnalysisKind,
    ) -> PathAnalysisResolution {
        match &mut self.table {
            Some(table) => table.resolve_path_analysis(
                &self.states,
                ExceptionState(tag.exc_state.0),
                Some((tag.clock, tag.edge)),
                end,
                end_tr,
                Some(capture),
                analysis,
            ),
            None => match analysis {
                AnalysisKind::Setup => PathAnalysisResolution::TimedSetupMulticycle {
                    setup: Default::default(),
                },
                AnalysisKind::Hold => PathAnalysisResolution::TimedHoldMulticycle {
                    setup: Default::default(),
                    hold: Default::default(),
                },
            },
        }
    }

    /// The change of the relation of a check of the analysis `analysis` that `res` gives, for a
    /// check whose standard relation is `relation_ns`, or `None` when the check is not timed
    /// (`STA-REQ-0259` to `STA-REQ-0263`).
    #[must_use]
    pub fn relation_shift(
        res: PathAnalysisResolution,
        relation_ns: f64,
        launch_period_ns: f64,
        capture_period_ns: f64,
    ) -> Option<f64> {
        match res {
            PathAnalysisResolution::TimedSetupMulticycle { setup } => {
                Some(setup.shift_ns(launch_period_ns, capture_period_ns))
            }
            PathAnalysisResolution::TimedHoldMulticycle { setup, hold } => Some(
                setup.shift_ns(launch_period_ns, capture_period_ns)
                    - hold.shift_ns(launch_period_ns, capture_period_ns),
            ),
            PathAnalysisResolution::TimedMaxDelay { max_delay_ns, .. } => {
                Some(max_delay_ns - relation_ns)
            }
            PathAnalysisResolution::TimedMinDelay { min_delay_ns, .. } => {
                Some(min_delay_ns - relation_ns)
            }
            PathAnalysisResolution::UntimedClockGroup
            | PathAnalysisResolution::UntimedUnclocked
            | PathAnalysisResolution::UntimedFalsePath { .. } => None,
        }
    }

    /// Applies the exceptions to the evaluated checks `records` other than latch setup checks and
    /// output delays:
    /// removes the checks that a false path or a clock group excludes, and moves the required time
    /// and slack of the others by the change of their relation (`STA-REQ-0259` to `STA-REQ-0263`,
    /// `STA-REQ-0268`).
    pub fn apply_to_records(
        &mut self,
        records: &mut Vec<SlackRecord>,
        tags: &TagTable,
        constraints: &CompiledConstraints,
    ) {
        if !self.is_active() {
            return;
        }
        records.retain_mut(|rec| {
            // Latch setup checks and output delays resolve where their window or their latency
            // is known.
            if matches!(
                rec.kind,
                EndpointCheckKind::LatchSetup
                    | EndpointCheckKind::OutputSetup
                    | EndpointCheckKind::OutputHold
            ) {
                return true;
            }
            let tag = tags.tag(rec.tag);
            let setup = rec.kind.is_setup_family();
            let analysis = if setup {
                AnalysisKind::Setup
            } else {
                AnalysisKind::Hold
            };
            let res = self.resolve(
                tag,
                rec.endpoint,
                rec.data_tr,
                (rec.capture_clock, rec.capture_edge),
                analysis,
            );
            let Some(shift) = Self::relation_shift(
                res,
                rec.relation,
                constraints.clocks[tag.clock.index()].period_ns,
                constraints.clocks[rec.capture_clock.index()].period_ns,
            ) else {
                return false;
            };
            if shift != 0.0 {
                rec.relation += shift;
                rec.required = canon_zero(rec.required + shift);
                rec.slack = canon_zero(if setup {
                    rec.slack + shift
                } else {
                    rec.slack - shift
                });
            }
            true
        });
    }

    /// Notes `D0111` (ties) and `D0112` (exceptions that win no analysis) of the exceptions
    /// applied so far (`STA-REQ-0258`, `STA-REQ-0270`).
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diag> {
        self.table.as_ref().map_or_else(Vec::new, |table| {
            table.finalize_diagnostics_and_scope(&[], &[]).0
        })
    }

    /// Number of exceptions that won no analysis of any timed path so far (`STA-REQ-0270`).
    #[must_use]
    pub fn unused(&self) -> usize {
        self.table.as_ref().map_or(0, |table| {
            table
                .rules()
                .iter()
                .filter(|r| !table.is_used(r.id))
                .count()
        })
    }
}

fn unsupported_diagnostics(
    netview: &NetlistView<'_>,
    constraints: &CompiledConstraints,
    table: &ExceptionTable,
) -> Result<Vec<Diag>, Diag> {
    for rule in table.rules() {
        let ignores_latency = matches!(
            rule.kind,
            ExceptionRuleKind::MaxDelay {
                ignore_clock_latency: true,
                ..
            } | ExceptionRuleKind::MinDelay {
                ignore_clock_latency: true,
                ..
            }
        );
        if ignores_latency {
            let mut d = Diag::error(
                "T0071",
                "set_max_delay or set_min_delay with -ignore_clock_latency is not analysed by \
                 this version (STA-REQ-0265); remove the option or the exception",
            );
            if let Some(loc) = &rule.location {
                d = d.at(loc.file.clone(), loc.line, loc.column);
            }
            return Err(d);
        }
    }

    let mut diags = Vec::new();
    let clock_sources: FxHashSet<NodeId> = constraints
        .clocks
        .iter()
        .flat_map(|c| c.sources.iter().copied())
        .collect();
    let clocked_inputs: FxHashSet<NodeId> = constraints
        .input_delays
        .iter()
        .filter(|d| d.clock.is_some())
        .map(|d| d.node)
        .collect();
    let clocked_outputs: FxHashSet<NodeId> = constraints
        .output_delays
        .iter()
        .filter(|d| d.clock.is_some())
        .map(|d| d.node)
        .collect();
    for pb in 0..netview.num_port_bits() {
        let node = NodeId(pb as u32);
        let NodeKind::PortBit { dir, .. } = netview.node_kind(node) else {
            continue;
        };
        let unclocked_start = matches!(dir, Direction::Input | Direction::Inout)
            && !clock_sources.contains(&node)
            && !clocked_inputs.contains(&node)
            && table.has_unclocked_from_delay_exception(node);
        let unclocked_end = matches!(dir, Direction::Output | Direction::Inout)
            && !clocked_outputs.contains(&node)
            && table.has_unclocked_to_delay_exception(node);
        if unclocked_start || unclocked_end {
            let (what, delay) = if unclocked_start {
                ("input port", "input delay")
            } else {
                ("output port", "output delay")
            };
            diags.push(Diag::warning(
                "T0072",
                format!(
                    "a set_max_delay or set_min_delay may apply to paths at {what} '{}', which \
                     has no {delay}; this version does not time unclocked ends (STA-REQ-0264), \
                     so such paths are not timed: give the port a {delay} relative to a clock",
                    netview.node_name_str(node)
                ),
            ));
        }
    }
    Ok(diags)
}

#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}
