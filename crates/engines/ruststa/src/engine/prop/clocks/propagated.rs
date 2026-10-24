// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Bounded per-clock CPU reference propagation (STA-REQ-0710 to STA-REQ-0713).
//! Reuses the canonical timing CSR/NLDM/RC records; no clock paths are enumerated.
use super::ideal::IdealClockModel;
use super::{ClockArrival, SENSE_BOTH};
use crate::engine::{
    analysis::AnalysisOptions,
    delay::{load::NetLoads, wire::WireModel},
    graph::{DisableReason, TimingGraph, carries_transition},
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
    for (ci, clock) in constraints.clocks.iter().enumerate() {
        if !clock.propagated || clock.is_virtual() {
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
        for &src in &clock.sources {
            if !matches!(view.node_kind(src), NodeKind::PortBit { .. })
                || constraints.driving_cells.contains_key(&src)
            {
                return Err(Diag::error(
                    "T0075",
                    "propagated clock requires a primary-port source with set_input_transition; internal/driving-cell clock sources are unsupported",
                ));
            }
        }
    }
    // Transitions are a property of the pin, not of the clock: every clock's propagation also
    // starts the transitions of the other input ports and of the tie cells, without arrivals, so
    // that a clock pin's transition merges every arc into it as in the data propagation
    // (`STA-REQ-0117`).
    let driving_cells = crate::engine::prop::starts::driving_cell_effects(
        view,
        graph,
        lib,
        constraints,
        &crate::engine::prop::starts::canonical_net_totals(view, loads),
        &crate::engine::delay::derate::NoDerate,
    );
    let mut lowered = LoweredCorner::lower(
        view,
        graph,
        lib,
        loads,
        2,
        GraphStartBuilder::new(2).build(),
        &ArcTagMaps::new(),
    )?;
    let mut propagate_all = |launched: &[(NodeId, [[f64; 2]; 2])],
                             model: &IdealClockModel|
     -> Result<Vec<Option<TimingValues>>, Diag> {
        let mut values = Vec::with_capacity(constraints.clocks.len());
        for (ci, clock) in constraints.clocks.iter().enumerate() {
            if !clock.propagated || clock.is_virtual() {
                values.push(None);
                continue;
            }
            let cid = ClockId(ci as u32);
            let mut starts = GraphStartBuilder::new(2);
            let own: rustc_hash::FxHashSet<NodeId> = clock.sources.iter().copied().collect();
            let seeds = crate::engine::prop::starts::untagged_seeds(
                view,
                graph,
                constraints,
                &driving_cells,
                &own,
            );
            for (node, slew) in seeds.iter().chain(launched) {
                for tr in Tr::ALL {
                    for an in An::ALL {
                        starts.merge_untagged_slew(
                            graph,
                            *node,
                            tr,
                            an,
                            slew[tr.index()][an.index()],
                        );
                    }
                }
            }
            for &src in &clock.sources {
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
        Ok(values)
    };
    let rank: Vec<u32> = (0..view.num_nodes())
        .map(|i| graph.topo_rank(NodeId(i as u32)))
        .collect();
    let all_physical = constraints
        .clocks
        .iter()
        .filter(|c| !c.is_virtual())
        .all(|c| c.propagated);
    let mut values = propagate_all(&[], &model)?;
    // A launched signal that enters the clock network (a flop output into a clock gate or a clock
    // multiplexer select) merges its transition there as in the data propagation: the outputs
    // of the launches whose transitions reach a clock pin seed the next propagation with the
    // transitions that the previous one gives their clock pins, until these no longer change
    // (one more propagation per launch stage in series, `STA-REQ-0714`).
    let feeds = launches_into_network(graph, &selected_node);
    let stages = feeds.iter().filter(|&&f| f).count();
    let mut launched: Vec<(NodeId, [[f64; 2]; 2])> = Vec::new();
    for pass in 1..=stages {
        model.set_propagated(Arc::new(PropagatedClocks {
            values,
            rank: rank.clone(),
            all_physical,
        }));
        let next = crate::engine::prop::starts::launch_transitions(
            view, graph, lib, &model, loads, &feeds,
        )?;
        values = model
            .take_propagated()
            .and_then(|p| Arc::try_unwrap(p).ok())
            .map(|p| p.values)
            .ok_or_else(|| Diag::error("T0023", "propagated clock values are shared"))?;
        if next == launched {
            break;
        }
        if work.saturating_mul(pass as u64 + 1) > options.max_state_bytes / 8 {
            return Err(Diag::error(
                "T0023",
                format!(
                    "propagation work budget exceeded by {} clock propagations for launches into the clock network; --max-state-gib bounds clock propagation",
                    pass + 1
                ),
            ));
        }
        launched = next;
        // The previous clock values are not needed any more: release them before the next
        // propagation, so that at most one set is retained (STA-REQ-0713).
        drop(std::mem::take(&mut values));
        values = propagate_all(&launched, &model)?;
    }
    model.set_propagated(Arc::new(PropagatedClocks {
        values,
        rank,
        all_physical,
    }));
    Ok(model)
}

/// Per launch arc of `graph`, true when it launches (enabled, and a clock reaches its clock pin)
/// and its output's transition reaches a pin of `network` through the arcs that carry
/// transitions (`STA-REQ-0117`): the enabled arcs, the arcs that logic values disable and the
/// launch arcs that carry only their transition. One backward pass over those arcs (`O(V + E)`).
fn launches_into_network(graph: &TimingGraph, network: &[bool]) -> Vec<bool> {
    let n = graph.num_nodes();
    let mut reaches: Vec<bool> = (0..n)
        .map(|i| network.get(i).copied().unwrap_or(false))
        .collect();
    if !reaches.iter().any(|&r| r) {
        return vec![false; graph.launches().len()];
    }
    // Incoming adjacency (CSR) of the arcs that carry transitions.
    let edges: Vec<(u32, u32)> = graph
        .arcs()
        .iter()
        .enumerate()
        .filter(|(ai, _)| carries_transition(graph.arc_disable_reason(*ai)))
        .map(|(_, a)| (a.dst.0, a.src.0))
        .chain(
            graph
                .launches()
                .iter()
                .enumerate()
                .filter(|(li, _)| graph.launch_carries_transition(*li))
                .map(|(_, l)| (l.out_pin.0, l.clock_pin.0)),
        )
        .collect();
    let mut off = vec![0u32; n + 1];
    for &(d, _) in &edges {
        off[d as usize + 1] += 1;
    }
    for i in 0..n {
        off[i + 1] += off[i];
    }
    let mut fill = off.clone();
    let mut src = vec![0u32; edges.len()];
    for &(d, s) in &edges {
        src[fill[d as usize] as usize] = s;
        fill[d as usize] += 1;
    }
    let mut stack: Vec<u32> = (0..n as u32).filter(|&i| reaches[i as usize]).collect();
    while let Some(v) = stack.pop() {
        for &s in &src[off[v as usize] as usize..off[v as usize + 1] as usize] {
            if !reaches[s as usize] {
                reaches[s as usize] = true;
                stack.push(s);
            }
        }
    }
    graph
        .launches()
        .iter()
        .enumerate()
        .map(|(li, l)| {
            (graph.is_ideal_launch(li)
                || !graph.is_launch_disabled(li) && !graph.launch_carries_transition(li))
                && reaches[l.out_pin.index()]
        })
        .collect()
}
