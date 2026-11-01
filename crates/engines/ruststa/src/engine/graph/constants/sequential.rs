// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Sequential cells next to logic values: unclocked launch arcs (`STA-REQ-0117`), latch enables
//! held by constants (`STA-REQ-0163`) and latch groups that store the inverted data pin
//! (`STA-REQ-0230`).

use rusteda_core::liberty::{Cell, Func, SeqKind};

use super::{
    ArcKind, CompiledConstraints, LogicVal, NodeId, PinConst, PropEngine, TimingGraph,
    collect_vars, eval_func, inst_ctx, pin_pair,
};

/// Per launch arc of `graph`, true when it is enabled and no clock of `constraints` reaches its
/// clock pin through enabled arcs other than latch data arcs (the reach of the clock model,
/// `STA-REQ-0200`); without constraints no clock reaches any pin (`STA-REQ-0117`).
pub(super) fn unclocked_launches(
    graph: &TimingGraph,
    constraints: Option<&CompiledConstraints>,
) -> Vec<bool> {
    let n = graph.num_nodes();
    let mut reached = vec![false; n];
    if let Some(c) = constraints {
        let mut stack: Vec<NodeId> = Vec::new();
        for src in c.clocks.iter().flat_map(|ck| ck.sources.iter().copied()) {
            if src.index() < n && !reached[src.index()] {
                reached[src.index()] = true;
                stack.push(src);
            }
        }
        let arcs = graph.arcs();
        while let Some(v) = stack.pop() {
            for &ai in graph.outgoing(v) {
                let arc = &arcs[ai as usize];
                if arc.kind.is_latch_data() || reached[arc.dst.index()] {
                    continue;
                }
                reached[arc.dst.index()] = true;
                stack.push(arc.dst);
            }
        }
    }
    graph
        .launches()
        .iter()
        .enumerate()
        .map(|(li, l)| !graph.is_launch_disabled(li) && !reached[l.clock_pin.index()])
        .collect()
}

/// Per launch arc of `graph` that launches no arrival (unclocked, or disabled by logic values),
/// true when an ideal clock of `constraints` reaches its clock pin through the enabled arcs and
/// the arcs that logic values disable, other than latch data arcs (`STA-REQ-0822`).
pub(super) fn ideal_launches(
    graph: &TimingGraph,
    constraints: Option<&CompiledConstraints>,
    unclocked: &[bool],
) -> Vec<bool> {
    let n = graph.num_nodes();
    let Some(c) = constraints else {
        return Vec::new();
    };
    let sources: Vec<NodeId> = c
        .clocks
        .iter()
        .filter(|ck| !ck.propagated && !ck.is_virtual())
        .flat_map(|ck| ck.sources.iter().copied())
        .filter(|s| s.index() < n)
        .collect();
    if sources.is_empty() {
        return Vec::new();
    }
    let arcs = graph.arcs();
    let mut logic_out: rustc_hash::FxHashMap<NodeId, Vec<u32>> = rustc_hash::FxHashMap::default();
    for (ai, arc) in arcs.iter().enumerate() {
        if graph
            .arc_disable_reason(ai)
            .is_some_and(|r| r.carries_transition())
        {
            logic_out.entry(arc.src).or_default().push(ai as u32);
        }
    }
    let mut reached = vec![false; n];
    let mut stack: Vec<NodeId> = Vec::new();
    for src in sources {
        if !reached[src.index()] {
            reached[src.index()] = true;
            stack.push(src);
        }
    }
    while let Some(v) = stack.pop() {
        let extra = logic_out.get(&v).map_or(&[][..], Vec::as_slice);
        for &ai in graph.outgoing(v).iter().chain(extra) {
            let arc = &arcs[ai as usize];
            if arc.kind.is_latch_data() || reached[arc.dst.index()] {
                continue;
            }
            reached[arc.dst.index()] = true;
            stack.push(arc.dst);
        }
    }
    graph
        .launches()
        .iter()
        .enumerate()
        .map(|(li, l)| {
            let silent = unclocked.get(li).copied().unwrap_or(false)
                || graph
                    .disabled_launches()
                    .get(li)
                    .copied()
                    .flatten()
                    .is_some_and(|r| r.carries_transition());
            silent && reached[l.clock_pin.index()]
        })
        .collect()
}

/// The value of the enable of the latch of `cell` whose data input is `data_pin` (any latch of
/// the cell when the name is unknown) under the pin values `env`, `X` when the cell has no
/// latch (`STA-REQ-0163`).
pub(super) fn latch_enable(
    cell: &Cell,
    data_pin: Option<&str>,
    env: &impl Fn(&str) -> PinConst,
) -> LogicVal {
    let latches = || cell.sequentials.iter().filter(|q| q.kind == SeqKind::Latch);
    let uses_pin = |f: &Func| {
        let mut vars = Vec::new();
        collect_vars(f, &mut vars);
        data_pin.is_some_and(|p| vars.contains(&p))
    };
    latches()
        .find(|q| uses_pin(&q.data))
        .or_else(|| latches().next())
        .map_or(LogicVal::X, |q| {
            eval_func(&q.clock, &|name| env(name).value)
        })
}

impl PropEngine<'_> {
    /// The instances with a latch data arc whose latch group stores the inverted data pin
    /// (`data_in : "!D"`, `STA-REQ-0230`).
    pub(super) fn inverted_data_latches(&self, graph: &TimingGraph) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for arc in graph.arcs() {
            let ArcKind::LatchData { inst, .. } = arc.kind else {
                continue;
            };
            if out.last() == Some(&inst) {
                continue;
            }
            let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, inst) else {
                continue;
            };
            let Some((_, cell, from_cp, _)) = pin_pair(self.netview, arc.src, arc.dst) else {
                continue;
            };
            let d = self.netview.cell_pin_name(cell, from_cp);
            let inverted = ictx.meta.cell.sequentials.iter().any(|q| {
                q.kind == SeqKind::Latch
                    && matches!(&q.data, Func::Not(x) if matches!(x.as_ref(), Func::Var(v) if v == d))
            });
            if inverted {
                out.push(inst);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}
