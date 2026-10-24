// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Levels and the canonical topological id (owner: E1).
//!
//! Engine section A. Exact longest-path levels. The topological id of a node is (level,
//! canonical key), the canonical key being the order in which the netlist's freeze lists the
//! node: port bits, then instance pins by instance and connection order. Timing sessions use the
//! same key, so that ties break the same way as in a full analysis after any sequence of edits.
//! The topological id is the only tie-break of the engine: arc order, path heaps, endpoint order
//! and CPPR parents.

use crate::engine::graph::build::{DisableReason, GraphArc, LaunchArc, carries_transition};
use crate::engine::types::NodeId;

/// Exact longest-path levels, canonical `(level, NodeId)` topological ordering, and CSR
/// incoming/outgoing enabled arc tables.
#[derive(Debug, Clone)]
pub struct Levels {
    /// Longest-path level of each node (`0..num_nodes()`).
    level: Vec<u32>,
    /// Nodes sorted by canonical topological id `(level(v), v.0)`.
    order: Vec<NodeId>,
    /// Position `0..num_nodes()` of each `NodeId` in `order`.
    topo_rank: Vec<u32>,
    /// CSR offsets into `order` for each level `0..num_levels()` (`len == num_levels() + 1`).
    level_start: Vec<u32>,
    /// CSR offsets (`num_nodes() + 1`) and arc indices of enabled incoming arcs per `NodeId`,
    /// ordered by `(topo_id(src), arc_idx)`.
    in_off: Vec<u32>,
    in_arcs: Vec<u32>,
    /// CSR offsets (`num_nodes() + 1`) and arc indices of enabled outgoing arcs per `NodeId`,
    /// ordered by `(topo_id(dst), arc_idx)`.
    out_off: Vec<u32>,
    out_arcs: Vec<u32>,
}

impl Levels {
    /// Computes longest-path levels, canonical topological ordering, and CSR arc tables from
    /// scratch over the enabled arcs of `arcs`.
    ///
    /// Assumes enabled arcs form a DAG (all combinational cycles have already been broken).
    pub fn compute(
        n_nodes: usize,
        arcs: &[GraphArc],
        disabled: &[Option<DisableReason>],
    ) -> Levels {
        let (out_off, out_arcs, mut indeg) = build_out_csr_and_indeg(n_nodes, arcs, disabled);
        let (level, visited) = kahn_longest_paths(n_nodes, arcs, &out_off, &out_arcs, &mut indeg);
        debug_assert_eq!(
            visited, n_nodes,
            "Levels::compute requires an acyclic graph"
        );
        Self::from_kahn(n_nodes, arcs, disabled, level, out_off, out_arcs)
    }

    /// Computes levels over every arc and launch that carries a transition (`STA-REQ-0116`): the
    /// enabled arcs, the arcs that logic values disable and the launches flagged in
    /// `carrying_launches` (disabled by logic values, or unclocked). The incoming and
    /// outgoing tables still list the enabled arcs only. If the arcs and launches disabled by
    /// logic values close a loop, the launches and then those arcs are left out of the levels
    /// (the lowering merges only the transitions of arcs that rise in level); the second value
    /// is the number of arcs and launches left out (0 on an acyclic graph).
    pub fn compute_carrying(
        n_nodes: usize,
        arcs: &[GraphArc],
        disabled: &[Option<DisableReason>],
        launches: &[LaunchArc],
        carrying_launches: &[bool],
    ) -> (Levels, usize) {
        let carried_arcs = || {
            arcs.iter()
                .zip(disabled)
                .filter(|(_, d)| d.is_some() && carries_transition(**d))
                .map(|(a, _)| (a.src.0, a.dst.0))
        };
        let carried_launches = || {
            launches
                .iter()
                .zip(carrying_launches)
                .filter(|(_, c)| **c)
                .map(|(l, _)| (l.clock_pin.0, l.out_pin.0))
        };
        let n_extra_arcs = carried_arcs().count();
        let n_extra_launches = carried_launches().count();
        let (out_off, out_arcs, _) = build_out_csr_and_indeg(n_nodes, arcs, disabled);
        if n_extra_arcs + n_extra_launches > 0 {
            let enabled = arcs
                .iter()
                .zip(disabled)
                .filter(|(_, d)| d.is_none())
                .map(|(a, _)| (a.src.0, a.dst.0));
            let attempts: [(bool, bool, usize); 2] =
                [(true, true, 0), (true, false, n_extra_launches)];
            for (with_arcs, with_launches, dropped) in attempts {
                let edges: Vec<(u32, u32)> = enabled
                    .clone()
                    .chain(carried_arcs().filter(|_| with_arcs))
                    .chain(carried_launches().filter(|_| with_launches))
                    .collect();
                if let Some(level) = edge_levels(n_nodes, &edges) {
                    let levels = Self::from_kahn(n_nodes, arcs, disabled, level, out_off, out_arcs);
                    return (levels, dropped);
                }
            }
        }
        let mut indeg = vec![0u32; n_nodes];
        for (ai, arc) in arcs.iter().enumerate() {
            if disabled[ai].is_none() {
                indeg[arc.dst.index()] += 1;
            }
        }
        let (level, visited) = kahn_longest_paths(n_nodes, arcs, &out_off, &out_arcs, &mut indeg);
        debug_assert_eq!(
            visited, n_nodes,
            "Levels::compute_carrying requires an acyclic enabled graph"
        );
        let levels = Self::from_kahn(n_nodes, arcs, disabled, level, out_off, out_arcs);
        (levels, n_extra_arcs + n_extra_launches)
    }

    /// Completes [`Levels`] construction from precomputed `level`, `out_off`, and `out_arcs`.
    pub(crate) fn from_kahn(
        n_nodes: usize,
        arcs: &[GraphArc],
        disabled: &[Option<DisableReason>],
        level: Vec<u32>,
        out_off: Vec<u32>,
        mut out_arcs: Vec<u32>,
    ) -> Levels {
        if n_nodes == 0 {
            return Levels {
                level,
                order: Vec::new(),
                topo_rank: Vec::new(),
                level_start: vec![0],
                in_off: vec![0],
                in_arcs: Vec::new(),
                out_off: vec![0],
                out_arcs: Vec::new(),
            };
        }

        // Stable counting sort of 0..n_nodes by level[v]. Since we iterate v = 0..n_nodes in
        // increasing NodeId order, each level bucket is automatically ordered by (level[v], v).
        let max_level = level.iter().copied().max().unwrap_or(0) as usize;
        let num_levels = max_level + 1;
        let mut level_start = vec![0u32; num_levels + 1];
        for &lv in &level {
            level_start[lv as usize + 1] += 1;
        }
        for l in 0..num_levels {
            level_start[l + 1] += level_start[l];
        }
        let mut cursor = level_start.clone();
        let mut order = vec![NodeId(0); n_nodes];
        let mut topo_rank = vec![0u32; n_nodes];
        for (v, &lv) in level.iter().enumerate() {
            let pos = cursor[lv as usize] as usize;
            cursor[lv as usize] += 1;
            order[pos] = NodeId(v as u32);
            topo_rank[v] = pos as u32;
        }

        // Build incoming CSR over enabled arcs.
        let mut in_off = vec![0u32; n_nodes + 1];
        for (ai, arc) in arcs.iter().enumerate() {
            if disabled[ai].is_none() {
                in_off[arc.dst.index() + 1] += 1;
            }
        }
        for v in 0..n_nodes {
            in_off[v + 1] += in_off[v];
        }
        let mut fill_in = in_off.clone();
        let mut in_arcs = vec![0u32; in_off[n_nodes] as usize];
        for (ai, arc) in arcs.iter().enumerate() {
            if disabled[ai].is_none() {
                let dst = arc.dst.index();
                let pos = fill_in[dst] as usize;
                in_arcs[pos] = ai as u32;
                fill_in[dst] += 1;
            }
        }

        // Order incoming arcs per node by (topo_id(src), arc_idx), and outgoing arcs per node
        // by (topo_id(dst), arc_idx).
        for v in 0..n_nodes {
            let (in_lo, in_hi) = (in_off[v] as usize, in_off[v + 1] as usize);
            if in_hi - in_lo > 1 {
                in_arcs[in_lo..in_hi].sort_unstable_by_key(|&ai| {
                    let src = arcs[ai as usize].src;
                    (topo_rank[src.index()], ai)
                });
            }
            let (out_lo, out_hi) = (out_off[v] as usize, out_off[v + 1] as usize);
            if out_hi - out_lo > 1 {
                out_arcs[out_lo..out_hi].sort_unstable_by_key(|&ai| {
                    let dst = arcs[ai as usize].dst;
                    (topo_rank[dst.index()], ai)
                });
            }
        }

        Levels {
            level,
            order,
            topo_rank,
            level_start,
            in_off,
            in_arcs,
            out_off,
            out_arcs,
        }
    }

    /// Number of nodes in the graph.
    #[inline]
    pub fn num_nodes(&self) -> usize {
        self.level.len()
    }

    /// Number of topological levels (`0` for an empty graph, else `max_level() + 1`).
    #[inline]
    pub fn num_levels(&self) -> usize {
        if self.order.is_empty() {
            0
        } else {
            self.level_start.len() - 1
        }
    }

    /// Maximum level index (`0` if empty or single-level).
    #[inline]
    pub fn max_level(&self) -> u32 {
        self.num_levels().saturating_sub(1) as u32
    }

    /// Exact longest-path level of `node`.
    #[inline]
    pub fn level(&self, node: NodeId) -> u32 {
        self.level[node.index()]
    }

    /// Slice of longest-path levels indexed by `NodeId::index()`.
    #[inline]
    pub fn levels(&self) -> &[u32] {
        &self.level
    }

    /// Canonical topological identifier `(level(node), node.0)`.
    ///
    /// Because `NodeId` already encodes the frozen netlist's canonical node order (port bits first,
    /// then connected instance pins in `(inst_row, connection_order)`), `(level(node), node.0)` is
    /// the unique total topological tie-break key across the engine.
    #[inline]
    pub fn topo_id(&self, node: NodeId) -> (u32, u32) {
        (self.level[node.index()], node.0)
    }

    /// Dense 0-based position (`0..num_nodes()`) of `node` in [`Self::order`].
    #[inline]
    pub fn topo_rank(&self, node: NodeId) -> u32 {
        self.topo_rank[node.index()]
    }

    /// All nodes in canonical topological order `(level(v), v.0)`.
    #[inline]
    pub fn order(&self) -> &[NodeId] {
        &self.order
    }

    /// CSR level boundary offsets into [`Self::order`].
    #[inline]
    pub fn level_start(&self) -> &[u32] {
        &self.level_start
    }

    /// Slice of nodes belonging to topological `level` (`0..num_levels()`).
    #[inline]
    pub fn level_nodes(&self, level: usize) -> &[NodeId] {
        let start = self.level_start[level] as usize;
        let end = self.level_start[level + 1] as usize;
        &self.order[start..end]
    }

    /// Enabled incoming arc indices of `node`, ordered by `(topo_id(src), arc_idx)`.
    #[inline]
    pub fn incoming(&self, node: NodeId) -> &[u32] {
        let i = node.index();
        &self.in_arcs[self.in_off[i] as usize..self.in_off[i + 1] as usize]
    }

    /// Enabled outgoing arc indices of `node`, ordered by `(topo_id(dst), arc_idx)`.
    #[inline]
    pub fn outgoing(&self, node: NodeId) -> &[u32] {
        let i = node.index();
        &self.out_arcs[self.out_off[i] as usize..self.out_off[i + 1] as usize]
    }

    /// CSR offset array (`len == num_nodes() + 1`) for enabled incoming arcs.
    #[inline]
    pub fn in_off(&self) -> &[u32] {
        &self.in_off
    }

    /// CSR arc index array for enabled incoming arcs.
    #[inline]
    pub fn in_arcs(&self) -> &[u32] {
        &self.in_arcs
    }

    /// CSR offset array (`len == num_nodes() + 1`) for enabled outgoing arcs.
    #[inline]
    pub fn out_off(&self) -> &[u32] {
        &self.out_off
    }

    /// CSR arc index array for enabled outgoing arcs.
    #[inline]
    pub fn out_arcs(&self) -> &[u32] {
        &self.out_arcs
    }
}

/// Builds the CSR outgoing arc table over enabled arcs and the per-node in-degree array.
pub(crate) fn build_out_csr_and_indeg(
    n_nodes: usize,
    arcs: &[GraphArc],
    disabled: &[Option<DisableReason>],
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let mut out_off = vec![0u32; n_nodes + 1];
    let mut indeg = vec![0u32; n_nodes];
    for (ai, arc) in arcs.iter().enumerate() {
        if disabled[ai].is_none() {
            out_off[arc.src.index() + 1] += 1;
            indeg[arc.dst.index()] += 1;
        }
    }
    for v in 0..n_nodes {
        out_off[v + 1] += out_off[v];
    }
    let mut fill = out_off.clone();
    let mut out_arcs = vec![0u32; out_off[n_nodes] as usize];
    for (ai, arc) in arcs.iter().enumerate() {
        if disabled[ai].is_none() {
            let src = arc.src.index();
            let pos = fill[src] as usize;
            out_arcs[pos] = ai as u32;
            fill[src] += 1;
        }
    }
    (out_off, out_arcs, indeg)
}

/// Runs Kahn's topological pass, computing longest-path `level[v]` and returning `(level, visited)`.
///
/// If `visited == n_nodes`, the enabled graph is acyclic and `level` holds the exact longest-path
/// levels. If `visited < n_nodes`, at least one combinational cycle exists among the remaining
/// nodes with `indeg[v] > 0`.
pub(crate) fn kahn_longest_paths(
    n_nodes: usize,
    arcs: &[GraphArc],
    out_off: &[u32],
    out_arcs: &[u32],
    indeg: &mut [u32],
) -> (Vec<u32>, usize) {
    let mut level = vec![0u32; n_nodes];
    let mut queue = Vec::with_capacity(n_nodes);
    for (v, &deg) in indeg.iter().enumerate() {
        if deg == 0 {
            queue.push(v as u32);
        }
    }
    let mut head = 0usize;
    while head < queue.len() {
        let v = queue[head] as usize;
        head += 1;
        let next_lv = level[v] + 1;
        let start = out_off[v] as usize;
        let end = out_off[v + 1] as usize;
        for &ai in &out_arcs[start..end] {
            let w = arcs[ai as usize].dst.index();
            if next_lv > level[w] {
                level[w] = next_lv;
            }
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w as u32);
            }
        }
    }
    (level, queue.len())
}

/// Exact longest-path levels of the graph with the directed `edges` `(src, dst)`, or `None` when
/// the edges close a loop (Kahn's algorithm, `O(V + E)`).
fn edge_levels(n_nodes: usize, edges: &[(u32, u32)]) -> Option<Vec<u32>> {
    let mut off = vec![0u32; n_nodes + 1];
    let mut indeg = vec![0u32; n_nodes];
    for &(s, d) in edges {
        off[s as usize + 1] += 1;
        indeg[d as usize] += 1;
    }
    for v in 0..n_nodes {
        off[v + 1] += off[v];
    }
    let mut fill = off.clone();
    let mut dst = vec![0u32; edges.len()];
    for &(s, d) in edges {
        dst[fill[s as usize] as usize] = d;
        fill[s as usize] += 1;
    }
    let mut level = vec![0u32; n_nodes];
    let mut queue: Vec<u32> = (0..n_nodes as u32)
        .filter(|&v| indeg[v as usize] == 0)
        .collect();
    let mut head = 0usize;
    while head < queue.len() {
        let v = queue[head] as usize;
        head += 1;
        let next = level[v] + 1;
        for &w in &dst[off[v] as usize..off[v + 1] as usize] {
            let w = w as usize;
            if next > level[w] {
                level[w] = next;
            }
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w as u32);
            }
        }
    }
    (queue.len() == n_nodes).then_some(level)
}
