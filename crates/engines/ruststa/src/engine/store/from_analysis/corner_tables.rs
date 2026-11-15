// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Counts and required-time reconstruction used by the store tables.

use std::collections::BTreeSet;

use rusteda_core::liberty::Direction;

use super::StoreInput;
use crate::engine::analysis::CornerAnalysis;
use crate::engine::input::netview::NodeKind;
use crate::engine::limits::LimitKind;
use crate::engine::prop::backward::{BackwardPass, ByNode};
use crate::engine::prop::latch::lower_corner_with_derates;
use crate::engine::prop::lower::LoweredCorner;
use crate::engine::prop::starts::build_start_rows;
use crate::engine::store::{CornerLimitsReport, LimitKindReport};
use crate::engine::types::NodeId;

pub(super) fn count_unconstrained_inputs(analysis: &StoreInput<'_>) -> u32 {
    let mut clock_sources = BTreeSet::new();
    for ck in &analysis.constraints.clocks {
        for &src in &ck.sources {
            clock_sources.insert(src);
        }
    }
    let mut input_delayed = BTreeSet::new();
    for id in &analysis.constraints.input_delays {
        input_delayed.insert(id.node);
    }
    let mut count = 0u32;
    for pb in 0..analysis.graph.num_port_bits() {
        let nid = NodeId(pb as u32);
        if let NodeKind::PortBit { dir, .. } = analysis.netview.node_kind(nid)
            && matches!(dir, Direction::Input | Direction::Inout)
            && !clock_sources.contains(&nid)
            && !input_delayed.contains(&nid)
        {
            count += 1;
        }
    }
    count
}

/// The required times of corner `ca`: seeded at its checks and propagated backward over the
/// lowered problem of the corner, `kept` when the analysis kept it, else lowered again.
pub(super) fn compute_corner_backward_pass(
    analysis: &StoreInput<'_>,
    ca: &CornerAnalysis,
    kept: Option<&LoweredCorner<'_>>,
) -> BackwardPass {
    let ntags = ca.tags.len().max(1);
    // The node-ordered values of the corner hold the propagated arrivals and transitions, so the
    // pass does not need the level-ordered result, which a caller may already have released.
    if let Some(lowered) = kept {
        return BackwardPass::for_table(
            analysis.graph,
            lowered,
            ntags,
            &ca.endpoints.records,
            &ca.node_values,
        );
    }
    let mut bp =
        BackwardPass::seeded_for_table(analysis.graph.num_nodes(), ntags, &ca.endpoints.records);
    let values = ByNode {
        values: &ca.node_values,
        order: analysis.graph.order(),
    };

    if let Ok(start_build) = build_start_rows(
        analysis.netview,
        analysis.graph,
        &ca.lib,
        analysis.constraints,
        &ca.clock_model,
        &ca.tags,
        &ca.exceptions,
        &ca.net_loads,
        &analysis.constraints.timing_derate,
    ) {
        // The same graph, starts and RC data passed analysis preflight already; the lowering is
        // the analysis's own (`lower_corner`), so the arc values equal those of a kept problem.
        let lowered = lower_corner_with_derates(
            analysis.netview,
            analysis.graph,
            &ca.lib,
            &ca.net_loads,
            ntags as u32,
            start_build.rows,
            &start_build.launch_slews,
            &start_build.launch_drives,
            &ca.latch_maps,
            &ca.wire_model,
            u64::MAX,
            analysis.constraints.timing_derate,
            None,
            false,
        )
        .expect("the lowering of a validated analysis is deterministic");
        bp.propagate(analysis.graph, &lowered, &values);
    }
    bp
}

pub(super) fn cmp_slack_nan_last(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.total_cmp(&b),
    }
}

/// `v` as `u32`, saturated.
pub(super) fn sat_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// The counts of the pin limit checks of `ca` as the `SCOPE` of `sta.rbin` records them
/// (`STA-REQ-0807`).
#[must_use]
pub fn corner_limits_report(ca: &CornerAnalysis) -> CornerLimitsReport {
    CornerLimitsReport {
        corner: ca.corner_name.clone(),
        kinds: LimitKind::ALL
            .iter()
            .map(|&k| {
                let s = ca.limits.kind(k);
                (
                    k.name().to_owned(),
                    LimitKindReport {
                        checked: s.checked,
                        violating: s.violating,
                        unlimited: s.unlimited,
                        worst_slack_ns: s.worst_slack,
                        unevaluated: s.unevaluated,
                    },
                )
            })
            .collect(),
        unclocked_pulse_pins: ca.limits.unclocked_pulse_pins,
        pass_through_pulse_pins: ca.limits.pass_through_pulse_pins,
        pulse_groups: ca.limits.rules.groups.name().to_owned(),
        pulse_pins: ca.limits.rules.pins.name().to_owned(),
        clock_limits: ca.limits.clock_limits.name().to_owned(),
        constant_drivers: ca.limits.constant_drivers,
        static_transition_pins: ca.limits.static_transition_pins,
        ideal_clock_pins: ca.limits.ideal_clock_pins,
        untimed_pins: ca.limits.untimed_pins,
        clock_scope_pins: ca.limits.clock_scope_pins,
        unconnected_output_pins: ca.limits.unconnected_output_pins,
    }
}
