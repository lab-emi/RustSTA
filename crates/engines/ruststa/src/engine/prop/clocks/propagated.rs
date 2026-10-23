// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Bounded per-clock CPU reference propagation (STA-REQ-0710 to STA-REQ-0713).
//! Reuses the canonical timing CSR/NLDM/RC records; no clock paths are enumerated.
use super::ideal::IdealClockModel;
use super::{ClockArrival, SENSE_BOTH};
use crate::engine::{
    analysis::AnalysisOptions,
    delay::{load::NetLoads, wire::WireModel},
    graph::{DisableReason, TimingGraph},
    input::{NetlistView, NodeKind, constr::CompiledConstraints, libview::TimingLib},
    prop::lower::{
        ArcTagMaps, BackendKind, GraphStartBuilder, LoweredCorner, Precision, TimingValues,
    },
    types::{An, ClockId, NodeId, Tr},
};
use rusteda_core::job::{Ctx, Diag};
use std::sync::Arc;

/// Immutable arrival/slew results; session snapshots share them through Arc.
#[derive(Debug, Clone)]
pub struct PropagatedClocks {
    values: Vec<Option<TimingValues>>,
    rank: Vec<u32>,
    all_physical: bool,
}
impl PropagatedClocks {
    pub fn contains(&self, clock: ClockId) -> bool {
        self.values.get(clock.index()).is_some_and(Option::is_some)
    }
    pub fn scope_name(&self) -> &'static str {
        if self.all_physical {
            "propagated"
        } else {
            "mixed"
        }
    }
    pub fn arrival(
        &self,
        clock: ClockId,
        edge: Tr,
        pin: NodeId,
        tr: Tr,
        an: An,
    ) -> Option<ClockArrival> {
        let values = self.values.get(clock.index())?.as_ref()?;
        let pos = *self.rank.get(pin.index())? as usize;
        let latency = values.arrival(pos, edge.index(), tr.index(), an.index());
        if !latency.is_finite() {
            return None;
        }
        Some(ClockArrival {
            latency,
            slew: values.transition(pos, tr.index(), an.index()),
        })
    }
}

/// Memory/work preflight before sense tracing, CSR allocation or clock propagation.
#[allow(clippy::too_many_arguments)]
pub fn build_model(
    view: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    loads: &NetLoads,
    wire: &dyn WireModel,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    diags: &mut Vec<Diag>,
) -> Result<IdealClockModel, Diag> {
    let n = graph.num_nodes() as u64;
    let e = graph.arcs().len() as u64;
    let selected = constraints
        .clocks
        .iter()
        .filter(|c| c.propagated && !c.is_virtual())
        .count() as u64;
    let clocks = constraints.clocks.len() as u64;
    // The largest extracted corner bounds the RC state of any one corner.
    let rc = options
        .parasitics
        .iter()
        .map(|p| p.data.estimated_state_bytes())
        .max()
        .unwrap_or(0);
    let state = n
        .saturating_mul(clocks.max(1))
        .saturating_mul(320)
        .saturating_add(n.saturating_mul(selected).saturating_mul(128))
        .saturating_add(if selected > 0 {
            n.saturating_mul(320) + e.saturating_mul(192)
        } else {
            0
        })
        .saturating_add(rc);
    let work = clocks
        .saturating_add(selected)
        .saturating_mul(n.saturating_add(e));
    if clocks.saturating_mul(2) > options.tag_budget as u64 {
        crate::engine::prop::tags::TagTable::for_clocks_with_budget(
            clocks as usize,
            options.tag_budget,
        )?;
    }
    if state > options.max_state_bytes || work > options.max_state_bytes / 8 {
        return Err(Diag::error(
            "T0023",
            format!(
                "propagation state/work budget exceeded including clocks: {state} bytes, {work} visits; --max-state-gib and tag budget bound clock propagation"
            ),
        ));
    }
    let mut model = IdealClockModel::build(graph, constraints);
    if selected == 0 {
        return Ok(model);
    }
    let mut selected_node = vec![false; view.num_nodes()];
    let mut ambiguous = vec![false; constraints.clocks.len()];
    for (i, selected_here) in selected_node.iter_mut().enumerate() {
        for &(cid, bits) in model.node_clocks(NodeId(i as u32)) {
            if constraints.clocks[cid.index()].propagated {
                *selected_here = true;
                ambiguous[cid.index()] |= bits == SENSE_BOTH;
            }
        }
    }
    for (i, arc) in graph.arcs().iter().enumerate() {
        if (arc.kind.is_latch_data() && !graph.is_arc_disabled(i)
            || graph.arc_disable_reason(i) == Some(DisableReason::LoopBreak))
            && selected_node[arc.src.index()]
        {
            return Err(Diag::error(
                "T0075",
                "clock propagation through a latch data arc or clock loop is unsupported",
            ));
        }
    }
    let mut values = Vec::with_capacity(constraints.clocks.len());
    let mut lowered = LoweredCorner::lower(
        view,
        graph,
        lib,
        loads,
        2,
        GraphStartBuilder::new(2).build(),
        &ArcTagMaps::new(),
    )?;
    for (ci, clock) in constraints.clocks.iter().enumerate() {
        if !clock.propagated || clock.is_virtual() {
            values.push(None);
            continue;
        }
        let cid = ClockId(ci as u32);
        if clock.network_latency_ns.iter().flatten().any(|&v| v != 0.0)
            || clock.transition_ns.iter().flatten().any(|&v| v != 0.0)
            || constraints
                .clock_latencies
                .iter()
                .any(|l| !l.source && l.clock.is_none_or(|c| c == cid))
        {
            diags.push(Diag::warning("T0076",format!("clock {}: propagated network replaces ideal network latency/transition annotations; source set_input_transition is used",clock.name)));
        }
        if ambiguous[ci] {
            return Err(Diag::error(
                "T0075",
                "non-unate/reconvergent inversion clock network is unsupported",
            ));
        }
        let mut starts = GraphStartBuilder::new(2);
        for &src in &clock.sources {
            if !matches!(view.node_kind(src), NodeKind::PortBit { .. })
                || constraints.driving_cells.contains_key(&src)
            {
                return Err(Diag::error(
                    "T0075",
                    "propagated clock requires a primary-port source with set_input_transition; internal/driving-cell clock sources are unsupported",
                ));
            }
            for edge in Tr::ALL {
                let mut arr = [0.0; 2];
                let mut slew = [0.0; 2];
                for an in An::ALL {
                    arr[an.index()] = model.source_latency(cid, edge, an);
                    slew[an.index()] = constraints
                        .input_transitions
                        .get(&src)
                        .and_then(|t| t.slew_ns[an.index()][edge.index()])
                        .unwrap_or(0.0);
                }
                starts.set_tagged(graph, src, edge.index() as u32, edge, arr, slew);
            }
        }
        lowered.starts = starts.build();
        lowered.set_wire_model(graph, view, wire, lib, options.max_state_bytes)?;
        let mut result = lowered
            .propagate(BackendKind::Cpu, Precision::F64, Ctx::NONE)
            .map_err(|e| crate::engine::prop::lower::compute_error_diag(&e))?
            .values;
        result.pred = Vec::new(); // Predecessors are not clock paths; release their allocation.
        values.push(Some(result));
    }
    let rank = (0..view.num_nodes())
        .map(|i| graph.topo_rank(NodeId(i as u32)))
        .collect();
    let all_physical = constraints
        .clocks
        .iter()
        .filter(|c| !c.is_virtual())
        .all(|c| c.propagated);
    model.set_propagated(Arc::new(PropagatedClocks {
        values,
        rank,
        all_physical,
    }));
    Ok(model)
}
