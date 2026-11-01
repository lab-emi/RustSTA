// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing checks (owner: E5).
//!
//! Engine section C. Setup and hold checks of sequential cells, recovery and removal checks of
//! asynchronous controls (path group `async_default`), output delays, latch data checks and
//! three-state enable checks; the clock-gating checks are in [`gating`](super::gating).

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;

use super::clocks::ideal::IdealClockModel;
use super::clocks::{ClockModel, SENSE_NEG, SENSE_POS, source_edges_for_pin_tr};
use super::cppr::Cppr;
use super::exceptions::{AnalysisKind, ExceptionContext, PathAnalysisResolution};
use super::gating::{ClockGatingSite, evaluate_gating_sites};
use super::latch::{compute_latch_window, latch_open_pin_edge};
use super::lower::LoweredResult;
use super::relation::RelationTable;
use super::tags::TagTable;
use crate::engine::delay::lut::NO_TABLE;
use crate::engine::graph::{ArcKind, CheckArc, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, DisabledTiming};
use crate::engine::input::libview::{CheckArcKind, TimingCell, TimingLib};
use crate::engine::input::{NetlistView, NodeKind};
use crate::engine::types::{An, ClockId, NodeId, TagId, Tr};

/// Default path group name for asynchronous `recovery` and `removal` checks (`STA-REQ-0203`).
pub const ASYNC_DEFAULT_GROUP: &str = "async_default";

/// Specific kind of timing check evaluated at an endpoint (`STA-REQ-0202`, `STA-REQ-0203`,
/// `STA-REQ-0204`, `STA-REQ-0218`–`STA-REQ-0224`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointCheckKind {
    /// Edge-triggered sequential setup check (`setup_rising`, `setup_falling`).
    Setup,
    /// Edge-triggered sequential hold check (`hold_rising`, `hold_falling`).
    Hold,
    /// Asynchronous control recovery check (`recovery_rising`, `recovery_falling`).
    Recovery,
    /// Asynchronous control removal check (`removal_rising`, `removal_falling`).
    Removal,
    /// Level-sensitive latch data setup check with time borrowing (`STA-REQ-0221`).
    LatchSetup,
    /// Level-sensitive latch data hold check against the closing edge (`STA-REQ-0221`).
    LatchHold,
    /// Output port `set_output_delay -max` setup check (`STA-REQ-0218`, `STA-REQ-0219`).
    OutputSetup,
    /// Output port `set_output_delay -min` hold check (`STA-REQ-0218`, `STA-REQ-0219`).
    OutputHold,
    /// Combinational clock-gating setup check (`STA-REQ-0224`).
    ClockGatingSetup,
    /// Combinational clock-gating hold check (`STA-REQ-0224`).
    ClockGatingHold,
}

impl EndpointCheckKind {
    /// True for late-path checks (`Setup`, `Recovery`, `LatchSetup`, `OutputSetup`,
    /// `ClockGatingSetup`).
    #[inline]
    #[must_use]
    pub const fn is_setup_family(self) -> bool {
        matches!(
            self,
            Self::Setup
                | Self::Recovery
                | Self::LatchSetup
                | Self::OutputSetup
                | Self::ClockGatingSetup
        )
    }

    /// True for early-path checks (`Hold`, `Removal`, `LatchHold`, `OutputHold`,
    /// `ClockGatingHold`).
    #[inline]
    #[must_use]
    pub const fn is_hold_family(self) -> bool {
        !self.is_setup_family()
    }

    /// True for asynchronous control checks (`Recovery` and `Removal`; `STA-REQ-0203`).
    #[inline]
    #[must_use]
    pub const fn is_async_control(self) -> bool {
        matches!(self, Self::Recovery | Self::Removal)
    }

    /// The data arrival bound (`An::Late` for setup-family checks, `An::Early` for hold-family
    /// checks).
    #[inline]
    #[must_use]
    pub const fn data_bound(self) -> An {
        if self.is_setup_family() {
            An::Late
        } else {
            An::Early
        }
    }
}

/// One evaluated timing check slack at an endpoint (`STA-REQ-0218`–`STA-REQ-0224`).
#[derive(Debug, Clone, PartialEq)]
pub struct SlackRecord {
    /// Constrained endpoint node (`D`, async control pin, clock-gating enable pin, or output port).
    pub endpoint: NodeId,
    /// Related clock pin node (`None` for output port delays).
    pub clock_pin: Option<NodeId>,
    /// Specific check kind.
    pub kind: EndpointCheckKind,
    /// Slack in ns (`required - arrival` for setup family, `arrival - required` for hold family).
    pub slack: f64,
    /// Data arrival time in ns relative to the launch edge's nominal waveform time.
    pub arrival: f64,
    /// Required time in ns relative to the launch edge's nominal waveform time.
    pub required: f64,
    /// Relation of the check in ns: the capture edge time relative to the launch edge, as moved by
    /// a multicycle path or replaced by a maximum or minimum delay (`STA-REQ-0271`); the closing
    /// relation of a latch setup check.
    pub relation: f64,
    /// Time borrowed in ns (`> 0.0` when a transparent latch setup check borrows time;
    /// `STA-REQ-0221`).
    pub borrowed: f64,
    /// Propagation tag that produced `arrival`.
    pub tag: TagId,
    /// Launching clock of `tag`.
    pub launch_clock: ClockId,
    /// Launching waveform edge of `tag`.
    pub launch_edge: Tr,
    /// Transition of the data signal at `endpoint`.
    pub data_tr: Tr,
    /// Capturing clock.
    pub capture_clock: ClockId,
    /// Capturing waveform edge of `capture_clock`.
    pub capture_edge: Tr,
    /// Path group name (`capture_clock` name by default, `"async_default"` for `Recovery` and
    /// `Removal`, or overridden by `group_path`).
    pub path_group: String,
}

/// Evaluates all sequential checks (`Setup`, `Hold`, `Recovery`, `Removal`, `LatchSetup`,
/// `LatchHold`), output delays (`OutputSetup`, `OutputHold`), and combinational clock-gating
/// checks (`ClockGatingSetup`, `ClockGatingHold`) for a corner (`STA-REQ-0218`–`STA-REQ-0224`).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_all_checks<C: Cppr>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    constraints: &CompiledConstraints,
    clock_model: &IdealClockModel,
    cppr_model: &C,
    tags: &TagTable,
    exceptions: &mut ExceptionContext,
    gating_sites: &[ClockGatingSite],
    prop_res: &LoweredResult,
    range_tracker: &mut crate::engine::delay::lut::RangeTracker,
) -> Result<Vec<SlackRecord>, Diag> {
    let mut records = Vec::new();
    if tags.is_empty() {
        return Ok(records);
    }

    let cell_map = map_cells(netview, lib)?;
    let mut rel_table = RelationTable::new();
    let g_checks = graph.checks();
    let g_arcs = graph.arcs();
    // The latch data arcs of each instance, `(setup check, source, destination)` in arc order,
    // so that a check finds its latch arcs without a scan of every arc.
    let mut latch_arcs: FxHashMap<u32, Vec<(u32, NodeId, NodeId)>> = FxHashMap::default();
    for a in g_arcs {
        if let ArcKind::LatchData {
            inst, setup_check, ..
        } = a.kind
        {
            latch_arcs
                .entry(inst)
                .or_default()
                .push((setup_check, a.src, a.dst));
        }
    }

    // 1. Sequential setup, hold, recovery, removal, and latch data checks (`graph.checks()`).
    for (check_idx, ch) in g_checks.iter().enumerate() {
        // A check that a false `when`, a constant or case analysis disables is not timed even
        // where arrivals reach its pins (`STA-REQ-0155`, `STA-REQ-0163`).
        if graph.is_check_disabled(check_idx)
            || is_check_disabled(netview, ch, &constraints.disabled_timing)
        {
            continue;
        }
        let cell_row = netview.frozen().instances[ch.inst as usize].cell as usize;
        let tc = cell_map[cell_row];
        let Some(cell_check) = tc.checks.get(ch.lib_arc.index()) else {
            continue;
        };
        let inst_latch_arcs = latch_arcs.get(&ch.inst).map_or(&[][..], Vec::as_slice);
        let open_pin_edge = if inst_latch_arcs
            .iter()
            .any(|&(sc, _, _)| sc as usize == check_idx)
        {
            latch_open_pin_edge(graph, check_idx, ch)
        } else {
            None
        };
        let is_latch_inst = open_pin_edge.is_some()
            || inst_latch_arcs
                .iter()
                .any(|&(_, src, _)| src == ch.data_pin);
        let latch_out_pin = inst_latch_arcs
            .iter()
            .find(|&&(sc, _, _)| sc as usize == check_idx)
            .map(|&(_, _, dst)| dst);

        let check_kind = match cell_check.kind {
            CheckArcKind::Setup => {
                if open_pin_edge.is_some() {
                    EndpointCheckKind::LatchSetup
                } else {
                    EndpointCheckKind::Setup
                }
            }
            CheckArcKind::Hold => {
                if is_latch_inst {
                    EndpointCheckKind::LatchHold
                } else {
                    EndpointCheckKind::Hold
                }
            }
            CheckArcKind::Recovery => EndpointCheckKind::Recovery,
            CheckArcKind::Removal => EndpointCheckKind::Removal,
        };

        let senses = clock_model.pin_clocks(ch.clock_pin);
        if senses.is_empty() {
            continue;
        }

        if let Some(open_edge) = open_pin_edge {
            // Transparent latch data setup check (`STA-REQ-0221`).
            for &(capture_clock, bits) in senses {
                for (sense_bit, neg_sense) in [(SENSE_POS, false), (SENSE_NEG, true)] {
                    if bits & sense_bit == 0 {
                        continue;
                    }
                    for (tid, tag) in tags.iter() {
                        let Some(win) = compute_latch_window(
                            constraints,
                            clock_model,
                            cppr_model,
                            ch,
                            open_edge,
                            latch_out_pin,
                            tag,
                            capture_clock,
                            neg_sense,
                        )?
                        else {
                            continue;
                        };
                        // The path group of a record, resolved once and only for a tag that has records:
                        // most tags have no arrival at a given pin.
                        let mut group: Option<String> = None;
                        for data_tr in Tr::ALL {
                            if cell_check.table[data_tr.index()] == NO_TABLE {
                                continue;
                            }
                            let arr_l = prop_res.arrival(
                                graph,
                                ch.data_pin,
                                tid.index(),
                                data_tr,
                                An::Late,
                            );
                            if !An::Late.is_present(arr_l) {
                                continue;
                            }
                            let slew_l = prop_res.slew(graph, ch.data_pin, data_tr, An::Late);
                            let s_setup = lib.eval_check_tracked(
                                cell_check,
                                data_tr,
                                win.close_slew_early,
                                slew_l,
                                0.0,
                                range_tracker,
                            );
                            // Exceptions of the latch data check (`STA-REQ-0267`): a maximum
                            // delay checks the pin like a flip-flop pin against `V`.
                            let res = exceptions.resolve(
                                tag,
                                ch.data_pin,
                                data_tr,
                                (capture_clock, win.e_close),
                                AnalysisKind::Setup,
                            );
                            let (req, slack, borrowed, relation) = match res {
                                PathAnalysisResolution::TimedMaxDelay { max_delay_ns, .. } => {
                                    let req =
                                        win.r_close_unsetup - win.d_close + max_delay_ns - s_setup;
                                    (req, req - arr_l, 0.0, max_delay_ns)
                                }
                                other => {
                                    let Some(shift) = ExceptionContext::relation_shift(
                                        other,
                                        win.d_close,
                                        constraints.clocks[tag.clock.index()].period_ns,
                                        constraints.clocks[capture_clock.index()].period_ns,
                                    ) else {
                                        continue;
                                    };
                                    let w = if shift != 0.0 {
                                        win.shifted(shift)
                                    } else {
                                        win
                                    };
                                    let (req, slack, borrowed) = w.eval_setup(arr_l, s_setup);
                                    (req, slack, borrowed, w.d_close)
                                }
                            };
                            records.push(SlackRecord {
                                endpoint: ch.data_pin,
                                clock_pin: Some(ch.clock_pin),
                                kind: check_kind,
                                slack: canon_zero(slack),
                                arrival: canon_zero(arr_l),
                                required: canon_zero(req),
                                relation,
                                borrowed: canon_zero(borrowed),
                                tag: tid,
                                launch_clock: tag.clock,
                                launch_edge: tag.edge,
                                data_tr,
                                capture_clock,
                                capture_edge: win.e_close,
                                path_group: group
                                    .get_or_insert_with(|| {
                                        resolve_path_group(
                                            constraints,
                                            check_kind,
                                            tag.clock,
                                            capture_clock,
                                            ch.data_pin,
                                        )
                                    })
                                    .clone(),
                            });
                        }
                    }
                }
            }
            continue;
        }

        // Non-latch-setup sequential check (`Setup`, `Hold`, `Recovery`, `Removal`, `LatchHold`).
        for &(capture_clock, bits) in senses {
            for e_c in source_edges_for_pin_tr(ch.edge, bits) {
                if check_kind.is_setup_family() {
                    let Some(ck_arr) =
                        clock_model.arrival(capture_clock, e_c, ch.clock_pin, ch.edge, An::Early)
                    else {
                        continue;
                    };
                    // The related pin transition is the one of the capture clock path, the
                    // early analysis of a setup-family check (`STA-REQ-0229`).
                    let ck_slew = ck_arr.slew;
                    for (tid, tag) in tags.iter() {
                        let (d_s, _) = rel_table.get_or_compute(
                            constraints,
                            tag.clock,
                            tag.edge,
                            capture_clock,
                            e_c,
                        )?;
                        let u_s = clock_model.uncertainty(
                            constraints,
                            tag.clock,
                            tag.edge,
                            capture_clock,
                            e_c,
                            Some(ch.data_pin),
                            true,
                        );
                        let cppr = if tag.clock == capture_clock {
                            cppr_model.credit(capture_clock, ch.clock_pin, ch.clock_pin)
                        } else {
                            0.0
                        };
                        // The path group of a record, resolved once and only for a tag that has records:
                        // most tags have no arrival at a given pin.
                        let mut group: Option<String> = None;
                        for data_tr in Tr::ALL {
                            if cell_check.table[data_tr.index()] == NO_TABLE {
                                continue;
                            }
                            let arr_l = prop_res.arrival(
                                graph,
                                ch.data_pin,
                                tid.index(),
                                data_tr,
                                An::Late,
                            );
                            if !An::Late.is_present(arr_l) {
                                continue;
                            }
                            let slew_l = prop_res.slew(graph, ch.data_pin, data_tr, An::Late);
                            let s_setup = lib.eval_check_tracked(
                                cell_check,
                                data_tr,
                                ck_slew,
                                slew_l,
                                0.0,
                                range_tracker,
                            );
                            let req_s = d_s + ck_arr.latency - s_setup - u_s + cppr;
                            let slack_s = canon_zero(req_s - arr_l);
                            records.push(SlackRecord {
                                endpoint: ch.data_pin,
                                clock_pin: Some(ch.clock_pin),
                                kind: check_kind,
                                slack: slack_s,
                                arrival: canon_zero(arr_l),
                                required: canon_zero(req_s),
                                relation: d_s,
                                borrowed: 0.0,
                                tag: tid,
                                launch_clock: tag.clock,
                                launch_edge: tag.edge,
                                data_tr,
                                capture_clock,
                                capture_edge: e_c,
                                path_group: group
                                    .get_or_insert_with(|| {
                                        resolve_path_group(
                                            constraints,
                                            check_kind,
                                            tag.clock,
                                            capture_clock,
                                            ch.data_pin,
                                        )
                                    })
                                    .clone(),
                            });
                        }
                    }
                } else {
                    let Some(ck_arr) =
                        clock_model.arrival(capture_clock, e_c, ch.clock_pin, ch.edge, An::Late)
                    else {
                        continue;
                    };
                    // The related pin transition is the one of the capture clock path, the
                    // late analysis of a hold-family check (`STA-REQ-0229`).
                    let ck_slew = ck_arr.slew;
                    for (tid, tag) in tags.iter() {
                        let (_, d_h) = rel_table.get_or_compute(
                            constraints,
                            tag.clock,
                            tag.edge,
                            capture_clock,
                            e_c,
                        )?;
                        let u_h = clock_model.uncertainty(
                            constraints,
                            tag.clock,
                            tag.edge,
                            capture_clock,
                            e_c,
                            Some(ch.data_pin),
                            false,
                        );
                        let cppr = if tag.clock == capture_clock {
                            cppr_model.credit(capture_clock, ch.clock_pin, ch.clock_pin)
                        } else {
                            0.0
                        };
                        // The path group of a record, resolved once and only for a tag that has records:
                        // most tags have no arrival at a given pin.
                        let mut group: Option<String> = None;
                        for data_tr in Tr::ALL {
                            if cell_check.table[data_tr.index()] == NO_TABLE {
                                continue;
                            }
                            let arr_e = prop_res.arrival(
                                graph,
                                ch.data_pin,
                                tid.index(),
                                data_tr,
                                An::Early,
                            );
                            if !An::Early.is_present(arr_e) {
                                continue;
                            }
                            let slew_e = prop_res.slew(graph, ch.data_pin, data_tr, An::Early);
                            let s_hold = lib.eval_check_tracked(
                                cell_check,
                                data_tr,
                                ck_slew,
                                slew_e,
                                0.0,
                                range_tracker,
                            );
                            let req_h = d_h + ck_arr.latency + s_hold + u_h - cppr;
                            let slack_h = canon_zero(arr_e - req_h);
                            records.push(SlackRecord {
                                endpoint: ch.data_pin,
                                clock_pin: Some(ch.clock_pin),
                                kind: check_kind,
                                slack: slack_h,
                                arrival: canon_zero(arr_e),
                                required: canon_zero(req_h),
                                relation: d_h,
                                borrowed: 0.0,
                                tag: tid,
                                launch_clock: tag.clock,
                                launch_edge: tag.edge,
                                data_tr,
                                capture_clock,
                                capture_edge: e_c,
                                path_group: group
                                    .get_or_insert_with(|| {
                                        resolve_path_group(
                                            constraints,
                                            check_kind,
                                            tag.clock,
                                            capture_clock,
                                            ch.data_pin,
                                        )
                                    })
                                    .clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    // 2. Output port delays (`set_output_delay`; `STA-REQ-0218`, `STA-REQ-0219`).
    // A one-sided `-max` or `-min` output delay evaluates only the specified check side (`Setup`
    // for `-max`, `Hold` for `-min`).
    for d in &constraints.output_delays {
        if d.node.index() >= graph.num_nodes() {
            continue;
        }
        let Some(capture_clock) = d.clock else {
            continue;
        };
        let e_c = d.clock_edge;
        let lat_early = clock_model.io_latency(
            capture_clock,
            e_c,
            An::Early,
            d.source_latency_included,
            d.network_latency_included,
        );
        let lat_late = clock_model.io_latency(
            capture_clock,
            e_c,
            An::Late,
            d.source_latency_included,
            d.network_latency_included,
        );

        for (tid, tag) in tags.iter() {
            let (d_s, d_h) =
                rel_table.get_or_compute(constraints, tag.clock, tag.edge, capture_clock, e_c)?;
            let u_s = clock_model.uncertainty(
                constraints,
                tag.clock,
                tag.edge,
                capture_clock,
                e_c,
                Some(d.node),
                true,
            );
            let u_h = clock_model.uncertainty(
                constraints,
                tag.clock,
                tag.edge,
                capture_clock,
                e_c,
                Some(d.node),
                false,
            );

            for data_tr in Tr::ALL {
                if let Some(d_out_max) = d.delay_ns[An::Late.index()][data_tr.index()] {
                    let arr_l = prop_res.arrival(graph, d.node, tid.index(), data_tr, An::Late);
                    // Exceptions (`STA-REQ-0262`): a maximum delay that wins at an output port
                    // adds no capture clock latency.
                    let resolved = An::Late.is_present(arr_l).then(|| {
                        let res = exceptions.resolve(
                            tag,
                            d.node,
                            data_tr,
                            (capture_clock, e_c),
                            AnalysisKind::Setup,
                        );
                        match res {
                            PathAnalysisResolution::TimedMaxDelay { max_delay_ns, .. } => {
                                Some((max_delay_ns - d_out_max - u_s, max_delay_ns))
                            }
                            other => ExceptionContext::relation_shift(
                                other,
                                d_s,
                                constraints.clocks[tag.clock.index()].period_ns,
                                constraints.clocks[capture_clock.index()].period_ns,
                            )
                            .map(|shift| {
                                let rel = d_s + shift;
                                (rel + lat_early - d_out_max - u_s, rel)
                            }),
                        }
                    });
                    if let Some(Some((req_s, relation))) = resolved {
                        let slack_s = canon_zero(req_s - arr_l);
                        let group = resolve_path_group(
                            constraints,
                            EndpointCheckKind::OutputSetup,
                            tag.clock,
                            capture_clock,
                            d.node,
                        );
                        records.push(SlackRecord {
                            endpoint: d.node,
                            clock_pin: None,
                            kind: EndpointCheckKind::OutputSetup,
                            slack: slack_s,
                            arrival: canon_zero(arr_l),
                            required: canon_zero(req_s),
                            relation,
                            borrowed: 0.0,
                            tag: tid,
                            launch_clock: tag.clock,
                            launch_edge: tag.edge,
                            data_tr,
                            capture_clock,
                            capture_edge: e_c,
                            path_group: group,
                        });
                    }
                }
                if let Some(d_out_min) = d.delay_ns[An::Early.index()][data_tr.index()] {
                    let arr_e = prop_res.arrival(graph, d.node, tid.index(), data_tr, An::Early);
                    // Exceptions (`STA-REQ-0263`): a minimum delay that wins at an output port
                    // adds no capture clock latency.
                    let resolved = An::Early.is_present(arr_e).then(|| {
                        let res = exceptions.resolve(
                            tag,
                            d.node,
                            data_tr,
                            (capture_clock, e_c),
                            AnalysisKind::Hold,
                        );
                        match res {
                            PathAnalysisResolution::TimedMinDelay { min_delay_ns, .. } => {
                                Some((min_delay_ns - d_out_min + u_h, min_delay_ns))
                            }
                            other => ExceptionContext::relation_shift(
                                other,
                                d_h,
                                constraints.clocks[tag.clock.index()].period_ns,
                                constraints.clocks[capture_clock.index()].period_ns,
                            )
                            .map(|shift| {
                                let rel = d_h + shift;
                                (rel + lat_late - d_out_min + u_h, rel)
                            }),
                        }
                    });
                    if let Some(Some((req_h, relation))) = resolved {
                        let slack_h = canon_zero(arr_e - req_h);
                        let group = resolve_path_group(
                            constraints,
                            EndpointCheckKind::OutputHold,
                            tag.clock,
                            capture_clock,
                            d.node,
                        );
                        records.push(SlackRecord {
                            endpoint: d.node,
                            clock_pin: None,
                            kind: EndpointCheckKind::OutputHold,
                            slack: slack_h,
                            arrival: canon_zero(arr_e),
                            required: canon_zero(req_h),
                            relation,
                            borrowed: 0.0,
                            tag: tid,
                            launch_clock: tag.clock,
                            launch_edge: tag.edge,
                            data_tr,
                            capture_clock,
                            capture_edge: e_c,
                            path_group: group,
                        });
                    }
                }
            }
        }
    }

    // 3. Combinational clock-gating checks (`STA-REQ-0224`).
    evaluate_gating_sites(
        gating_sites,
        graph,
        constraints,
        clock_model,
        cppr_model,
        tags,
        prop_res,
        |rec| records.push(rec),
    )?;

    // 4. Timing exceptions and clock groups of every other check (`STA-REQ-0271`).
    exceptions.apply_to_records(&mut records, tags, constraints);

    Ok(records)
}

/// Resolves the path group name for an endpoint check (`STA-REQ-0203`, `group_path`).
///
/// Asynchronous `Recovery` and `Removal` checks default to `"async_default"`; all other checks
/// default to the capture clock's name unless overridden by an explicit `group_path -name`.
#[must_use]
pub fn resolve_path_group(
    constraints: &CompiledConstraints,
    kind: EndpointCheckKind,
    launch_clock: ClockId,
    capture_clock: ClockId,
    endpoint: NodeId,
) -> String {
    for pg in constraints.path_groups.iter().rev() {
        if pg.default {
            continue;
        }
        let Some(name) = &pg.name else {
            continue;
        };
        let from_ok = match &pg.from {
            None => true,
            Some(ep) => ep.clocks.contains(&launch_clock),
        };
        let to_ok = match &pg.to {
            None => true,
            Some(ep) => ep.clocks.contains(&capture_clock) || ep.has_node(endpoint),
        };
        if from_ok && to_ok && (pg.from.is_some() || pg.to.is_some()) {
            return name.clone();
        }
    }
    if kind.is_async_control() {
        ASYNC_DEFAULT_GROUP.into()
    } else {
        constraints
            .clocks
            .get(capture_clock.index())
            .map_or_else(|| "default".into(), |c| c.name.clone())
    }
}

fn map_cells<'a>(
    netview: &NetlistView<'_>,
    lib: &'a TimingLib,
) -> Result<Vec<&'a TimingCell>, Diag> {
    let n_cells = netview.num_cells();
    let mut out = Vec::with_capacity(n_cells);
    for c in 0..n_cells {
        let name = netview.cell_name(c as u32);
        let tc = lib.cell(name).ok_or_else(|| {
            Diag::error(
                "T0013",
                format!(
                    "corner '{}': netlist cell '{name}' is not present in the corner library",
                    lib.corner
                ),
            )
        })?;
        out.push(tc);
    }
    Ok(out)
}

/// Returns `true` when sequential check arc `ch` is disabled by `set_disable_timing`
/// (`STA-REQ-0157`, `STA-REQ-0226`).
pub(crate) fn is_check_disabled(
    netview: &NetlistView<'_>,
    ch: &CheckArc,
    disabled: &[DisabledTiming],
) -> bool {
    if disabled.is_empty() {
        return false;
    }
    let cell_row = netview.frozen().instances[ch.inst as usize].cell;
    let cell_name = netview.cell_name(cell_row);
    let pin_short_name = |node: NodeId| -> Option<&str> {
        match netview.node_kind(node) {
            NodeKind::InstancePin { cell, cell_pin, .. } => {
                Some(netview.cell_pin_name(cell, cell_pin))
            }
            NodeKind::PortBit { .. } => None,
        }
    };
    let ck_name = pin_short_name(ch.clock_pin);
    let d_name = pin_short_name(ch.data_pin);

    for dt in disabled {
        match dt {
            DisabledTiming::Cell { inst } => {
                if *inst == ch.inst {
                    return true;
                }
            }
            DisabledTiming::CellArc {
                inst,
                from_pin,
                to_pin,
            } => {
                if *inst == ch.inst
                    && ((ck_name == Some(from_pin.as_str()) && d_name == Some(to_pin.as_str()))
                        || (d_name == Some(from_pin.as_str()) && ck_name == Some(to_pin.as_str())))
                {
                    return true;
                }
            }
            DisabledTiming::Pin { pin } => {
                if let Some(nid) = pin.node
                    && (nid == ch.data_pin || nid == ch.clock_pin)
                {
                    return true;
                }
            }
            DisabledTiming::Port { port } => {
                if *port == ch.data_pin || *port == ch.clock_pin {
                    return true;
                }
            }
            DisabledTiming::LibCell {
                lib_cell,
                from_pin,
                to_pin,
            } => {
                if cell_name == lib_cell.cell.as_str() {
                    let match_dir = |src_p: Option<&str>, dst_p: Option<&str>| {
                        from_pin.as_deref().is_none_or(|f| src_p == Some(f))
                            && to_pin.as_deref().is_none_or(|t| dst_p == Some(t))
                    };
                    if match_dir(ck_name, d_name) || match_dir(d_name, ck_name) {
                        return true;
                    }
                }
            }
            DisabledTiming::LibPin { lib_pin } => {
                if cell_name == lib_pin.cell.as_str()
                    && (ck_name == Some(lib_pin.pin.as_str())
                        || d_name == Some(lib_pin.pin.as_str()))
                {
                    return true;
                }
            }
        }
    }
    false
}

#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}
