// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Forward and backward cone propagation with bitwise change detection (`owner: E11`;
//! `STA-REQ-0452` to `STA-REQ-0455`).
//!
//! Maps timing state across [`GateNetlist`] or [`FrozenNetlist`] edits in canonical `freeze()`
//! node order, seeds the level-ordered forward cone (`Scope::Cone` in `rusteda_compute::timing`),
//! and propagates per-tag required times backward in reverse level order with bitwise
//! (`to_bits() == to_bits()`) change cutoffs so that every value is bitwise identical to a full
//! analysis.

use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{
    ArcClass, InArc, LevelizedTimingCsr, LutDesc, NO_PRED, NONE, Request, StartRows, TagMapRec,
    TimingValues, absent, pred_source,
};
use rusteda_compute::{BackendKind, ComputeError, Precision};
use rusteda_core::job::Ctx;
use rusteda_core::netlist::{GateNetlist, PinId};

use crate::engine::graph::TimingGraph;
use crate::engine::prop::checks::SlackRecord;
use crate::engine::prop::lower::{LoweredCorner, LoweredResult};
use crate::engine::store::layout::{ABSENT_REQUIRED, ABSENT_SLACK, canon_f64};
use crate::engine::types::{An, NodeId};

/// Stable identity of a timing graph node across [`GateNetlist`] edits (`STA-REQ-0452`).
///
/// Follows the exact canonical ordering of [`GateNetlist::freeze`]: port bits `0..num_port_bits`
/// in declaration order, followed by connected instance pins in `(inst_id_index_order,
/// inst.connected_order)`. Because [`PinId`] carries a generation counter, deleted and re-created
/// pins in reused arena slots never collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionNodeKey {
    /// Top-level port bit index (`0..num_port_bits`).
    PortBit(u32),
    /// Connected instance pin identified by its generational [`PinId`].
    Pin(PinId),
}

/// Computes the [`SessionNodeKey`] for each [`NodeId`] (`0..num_nodes`) in `netlist.freeze()`
/// order (`STA-REQ-0452`).
#[must_use]
pub fn session_node_keys(netlist: &GateNetlist) -> Vec<SessionNodeKey> {
    let n_port_bits: u32 = netlist.ports().iter().map(|p| p.bits.len() as u32).sum();
    let mut keys = Vec::with_capacity(n_port_bits as usize + netlist.pin_count());
    for pb in 0..n_port_bits {
        keys.push(SessionNodeKey::PortBit(pb));
    }
    for (_, inst) in netlist.instances() {
        for &pin_id in &inst.connected {
            if let Ok(pin) = netlist.pin(pin_id)
                && pin.conn.is_some()
            {
                keys.push(SessionNodeKey::Pin(pin_id));
            }
        }
    }
    keys
}

/// Bijective node correspondence between an old timing graph and a new timing graph after one or
/// more netlist edits (`STA-REQ-0452`, `STA-REQ-0457`).
#[derive(Debug, Clone)]
pub struct NodeMapping {
    /// For each `new_node` (`0..new_num_nodes`), the corresponding `old_node`, if any.
    pub new_to_old: Vec<Option<NodeId>>,
    /// For each `old_node` (`0..old_num_nodes`), the corresponding `new_node`, if any.
    pub old_to_new: Vec<Option<NodeId>>,
    /// True for each `new_node` directly touched by an edit (pin of a modified instance or
    /// driver/load of a modified net).
    pub directly_dirty: Vec<bool>,
}

impl NodeMapping {
    /// Builds a [`NodeMapping`] from the index `old_index` (key to old node) of the `old_len` old
    /// node keys, the new node keys `new_keys` and the directly dirty keys `dirty_keys`, as a
    /// session keeps them between edits (`STA-REQ-0452`). A new node without an old key is
    /// directly dirty.
    #[must_use]
    pub fn from_key_index(
        old_index: &FxHashMap<SessionNodeKey, NodeId>,
        old_len: usize,
        new_keys: &[SessionNodeKey],
        dirty_keys: &FxHashSet<SessionNodeKey>,
    ) -> Self {
        let mut new_to_old = vec![None; new_keys.len()];
        let mut old_to_new = vec![None; old_len];
        let mut directly_dirty = vec![false; new_keys.len()];
        for (new_idx, key) in new_keys.iter().enumerate() {
            let new_nid = NodeId(new_idx as u32);
            if dirty_keys.contains(key) {
                directly_dirty[new_idx] = true;
            }
            match old_index.get(key) {
                Some(&old_nid) if old_nid.index() < old_len => {
                    new_to_old[new_idx] = Some(old_nid);
                    old_to_new[old_nid.index()] = Some(new_nid);
                }
                _ => directly_dirty[new_idx] = true,
            }
        }
        Self {
            new_to_old,
            old_to_new,
            directly_dirty,
        }
    }
}

/// Node and cone evaluation statistics for an incremental timing update (`STA-REQ-0453`,
/// `STA-REQ-0454`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// Total number of nodes in the updated timing graph.
    pub total_nodes: usize,
    /// Number of instances directly modified by the edit(s).
    pub dirty_instances: usize,
    /// Number of nets directly modified by the edit(s).
    pub dirty_nets: usize,
    /// Number of seed nodes enqueued into the forward cone (`Scope::Cone`).
    pub forward_seeds: usize,
    /// Number of node evaluations across all corners in the forward cone.
    pub forward_nodes_evaluated: usize,
    /// Number of nodes whose forward `arr`, `slew`, or `pred` changed bitwise.
    pub forward_nodes_changed: usize,
    /// Number of seed nodes enqueued into the backward cone.
    pub backward_seeds: usize,
    /// Number of node evaluations across all corners in the backward cone.
    pub backward_nodes_evaluated: usize,
    /// Number of nodes whose backward `req` changed bitwise.
    pub backward_nodes_changed: usize,
}

/// Maps `old_node_values` (in `old` [`NodeId`] order) onto `new_lowered` (in `new_graph`
/// level-ordered coordinates), identifies the exact forward seed set, and runs `Scope::Cone`
/// propagation (`STA-REQ-0453`).
///
/// Returns `(propagated_result, changed_level_positions, forward_seed_count)`.
#[allow(clippy::too_many_arguments)]
pub fn propagate_forward_cone(
    old_graph: Option<&TimingGraph>,
    old_lowered: Option<&LoweredCorner<'_>>,
    old_node_values: &TimingValues,
    new_graph: &TimingGraph,
    new_lowered: &LoweredCorner<'_>,
    mapping: &NodeMapping,
    backend: BackendKind,
    precision: Precision,
    ctx: Ctx<'_>,
) -> Result<(LoweredResult, Vec<u32>, usize), ComputeError> {
    let n_new = new_graph.num_nodes();
    let ntags = new_lowered.ntags as usize;
    let w = ntags * 4;

    if old_node_values.ntags != new_lowered.ntags {
        let res = new_lowered.propagate(backend, precision, ctx)?;
        let all_pos: Vec<u32> = (0..n_new as u32).collect();
        return Ok((res, all_pos, n_new));
    }

    let mut carried_arr = vec![0.0; n_new * w];
    let mut carried_slew = vec![0.0; n_new * 4];
    let mut carried_pred = vec![NO_PRED; n_new * w];

    let new_start_index = build_start_row_index(n_new, &new_lowered.starts);
    let old_start_index = old_lowered.map(|ol| build_start_row_index(ol.csr.nodes(), &ol.starts));

    let new_order = new_graph.order();
    let mut seeds = Vec::new();

    for (new_pos, &new_node) in new_order.iter().enumerate() {
        let new_v = new_node.index();
        let dst_arr = &mut carried_arr[new_pos * w..(new_pos + 1) * w];
        let dst_slew = &mut carried_slew[new_pos * 4..(new_pos + 1) * 4];
        let dst_pred = &mut carried_pred[new_pos * w..(new_pos + 1) * w];

        let Some(old_node) = mapping.new_to_old.get(new_v).copied().flatten() else {
            init_absent_row(dst_arr, dst_slew, dst_pred);
            seeds.push(new_pos as u32);
            continue;
        };
        let old_v = old_node.index();
        if old_v * w + w > old_node_values.arr.len() {
            init_absent_row(dst_arr, dst_slew, dst_pred);
            seeds.push(new_pos as u32);
            continue;
        }

        dst_arr.copy_from_slice(&old_node_values.arr[old_v * w..(old_v + 1) * w]);
        dst_slew.copy_from_slice(&old_node_values.slew[old_v * 4..(old_v + 1) * 4]);

        let mut pred_invalidated = false;
        let old_preds = &old_node_values.pred[old_v * w..(old_v + 1) * w];
        for (dp, &op) in dst_pred.iter_mut().zip(old_preds.iter()) {
            if op == NO_PRED {
                *dp = NO_PRED;
            } else {
                let old_src = NodeId(op & 0x7FFF_FFFF);
                let ti = (op >> 31) & 1;
                if let Some(new_src) = mapping.old_to_new.get(old_src.index()).copied().flatten() {
                    let new_src_pos = new_graph.topo_rank(new_src);
                    *dp = (new_src_pos & 0x7FFF_FFFF) | (ti << 31);
                } else {
                    *dp = NO_PRED;
                    pred_invalidated = true;
                }
            }
        }

        let mut is_seed = mapping.directly_dirty[new_v] || pred_invalidated;

        if !is_seed {
            if let (Some(og), Some(ol), Some(old_si)) =
                (old_graph, old_lowered, old_start_index.as_deref())
            {
                let old_pos = og.topo_rank(old_node) as usize;
                is_seed = !loads_bitwise_eq(&ol.load[old_pos], &new_lowered.load[new_pos])
                    || !start_row_bitwise_eq(
                        &ol.starts,
                        old_si[old_pos],
                        &new_lowered.starts,
                        new_start_index[new_pos],
                        new_lowered.ntags,
                    )
                    || !in_arcs_equivalent(
                        og,
                        ol,
                        old_pos,
                        new_graph,
                        new_lowered,
                        new_pos,
                        mapping,
                    );
            } else {
                is_seed = true;
            }
        }

        if is_seed {
            seeds.push(new_pos as u32);
        }
    }

    let carried_values = TimingValues {
        ntags: new_lowered.ntags,
        arr: carried_arr,
        slew: carried_slew,
        pred: carried_pred,
    };

    let mut session = new_lowered.open_session(backend, precision)?;
    session.write_values(&carried_values)?;
    let seed_count = seeds.len();
    let req = Request::cone(&seeds).with_ctx(ctx);
    let stats = session.propagate(&req)?;
    let changed = session.changed().unwrap_or(&[]).to_vec();
    let values = session.into_values();

    Ok((
        LoweredResult {
            values,
            stats,
            lower_s: new_lowered.lower_s,
        },
        changed,
        seed_count,
    ))
}

fn init_absent_row(arr: &mut [f64], slew: &mut [f64], pred: &mut [u32]) {
    for (i, x) in arr.iter_mut().enumerate() {
        *x = absent(i % 2);
    }
    for (i, x) in slew.iter_mut().enumerate() {
        *x = absent(i % 2);
    }
    pred.fill(NO_PRED);
}

fn build_start_row_index(n: usize, starts: &StartRows) -> Vec<u32> {
    let mut out = vec![NONE; n];
    for (k, &v) in starts.node.iter().enumerate() {
        if (v as usize) < n {
            out[v as usize] = k as u32;
        }
    }
    out
}

#[inline]
fn loads_bitwise_eq(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0].to_bits() == b[0].to_bits()
        && a[1].to_bits() == b[1].to_bits()
        && a[2].to_bits() == b[2].to_bits()
        && a[3].to_bits() == b[3].to_bits()
}

fn start_row_bitwise_eq(
    old_starts: &StartRows,
    old_row: u32,
    new_starts: &StartRows,
    new_row: u32,
    ntags: u32,
) -> bool {
    match (old_row, new_row) {
        (NONE, NONE) => true,
        (NONE, _) | (_, NONE) => false,
        (or, nr) => {
            let (oa, os) = old_starts.row(or as usize, ntags);
            let (na, ns) = new_starts.row(nr as usize, ntags);
            slice_bitwise_eq(oa, na) && slice_bitwise_eq(os, ns)
        }
    }
}

#[inline]
fn slice_bitwise_eq(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.to_bits() == y.to_bits())
}

fn in_arcs_equivalent(
    old_graph: &TimingGraph,
    old_lowered: &LoweredCorner<'_>,
    old_pos: usize,
    new_graph: &TimingGraph,
    new_lowered: &LoweredCorner<'_>,
    new_pos: usize,
    mapping: &NodeMapping,
) -> bool {
    let old_in = &old_lowered.csr.in_arcs
        [old_lowered.csr.in_ptr[old_pos] as usize..old_lowered.csr.in_ptr[old_pos + 1] as usize];
    let new_in = &new_lowered.csr.in_arcs
        [new_lowered.csr.in_ptr[new_pos] as usize..new_lowered.csr.in_ptr[new_pos + 1] as usize];
    if old_in.len() != new_in.len() {
        return false;
    }
    let old_order = old_graph.order();
    let new_order = new_graph.order();
    for (oa, na) in old_in.iter().zip(new_in.iter()) {
        let old_src_node = old_order[oa.src as usize];
        let new_src_node = new_order[na.src as usize];
        if mapping.new_to_old[new_src_node.index()] != Some(old_src_node) {
            return false;
        }
        let old_cls = &old_lowered.luts.classes[oa.class as usize];
        let new_cls = &new_lowered.luts.classes[na.class as usize];
        if old_cls != new_cls {
            return false;
        }
        if !tag_maps_eq(&old_lowered.csr, oa.map, &new_lowered.csr, na.map) {
            return false;
        }
    }
    true
}

fn tag_maps_eq(
    old_csr: &LevelizedTimingCsr<'_>,
    old_map: u32,
    new_csr: &LevelizedTimingCsr<'_>,
    new_map: u32,
) -> bool {
    match (old_map, new_map) {
        (NONE, NONE) => true,
        (NONE, _) | (_, NONE) => false,
        (om, nm) => {
            let o_slice = &old_csr.maps
                [old_csr.map_ptr[om as usize] as usize..old_csr.map_ptr[om as usize + 1] as usize];
            let n_slice = &new_csr.maps
                [new_csr.map_ptr[nm as usize] as usize..new_csr.map_ptr[nm as usize + 1] as usize];
            o_slice == n_slice
        }
    }
}

/// Per-corner backward required times and per-pin worst slacks (`STA-REQ-0300`, `STA-REQ-0301`,
/// `STA-REQ-0454`).
#[derive(Debug, Clone, PartialEq)]
pub struct CornerBackwardState {
    /// Number of propagation tags `T >= 1`.
    pub ntags: u32,
    /// Endpoint required times in [`NodeId`] order (`node * T * 4 + (tag * 2 + tr) * 2 + an`).
    pub endpoint_req: Vec<f64>,
    /// Propagated per-tag required times in [`NodeId`] order (`node * T * 4 + (tag * 2 + tr) * 2 + an`).
    pub node_req: Vec<f64>,
    /// Per-pin worst required time `pin_required[node][tr][an]` in [`NodeId`] order.
    pub pin_required: Vec<[[f64; 2]; 2]>,
    /// Per-pin worst slack `pin_slack[node][tr][an]` in [`NodeId`] order (`[Rise/Fall][Early/Late]`).
    pub pin_slack: Vec<[[f64; 2]; 2]>,
}

impl CornerBackwardState {
    /// Returns the propagated required time `req[node][tag][tr][an]` in `ns`.
    #[inline]
    #[must_use]
    pub fn required(&self, node: usize, tag: usize, tr: usize, an: usize) -> f64 {
        let w = self.ntags as usize * 4;
        self.node_req[node * w + (tag * 2 + tr) * 2 + an]
    }
}

/// Outgoing CSR arc table indexed by level-ordered source node position `src_pos`.
pub(crate) struct OutgoingCsr {
    pub ptr: Vec<u32>,
    /// Pairs `(dst_pos, csr_in_arc_idx)` sorted by `(dst_pos, csr_in_arc_idx)`.
    pub arcs: Vec<(u32, u32)>,
}

impl OutgoingCsr {
    pub fn build(csr: &LevelizedTimingCsr<'_>) -> Self {
        let n = csr.nodes();
        let mut counts = vec![0u32; n + 1];
        for a in csr.in_arcs.iter() {
            counts[a.src as usize + 1] += 1;
        }
        for i in 0..n {
            counts[i + 1] += counts[i];
        }
        let mut fill = counts.clone();
        let mut arcs = vec![(0u32, 0u32); csr.in_arcs.len()];
        for dst_pos in 0..n {
            for k in csr.in_ptr[dst_pos]..csr.in_ptr[dst_pos + 1] {
                let src_pos = csr.in_arcs[k as usize].src as usize;
                let slot = fill[src_pos] as usize;
                arcs[slot] = (dst_pos as u32, k);
                fill[src_pos] += 1;
            }
        }
        Self { ptr: counts, arcs }
    }
}

#[inline]
fn absent_req(an: usize) -> f64 {
    if an == An::Early.index() {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    }
}

fn build_endpoint_req(n_nodes: usize, ntags: u32, records: &[SlackRecord]) -> Vec<f64> {
    let w = ntags as usize * 4;
    let mut ep_req: Vec<f64> = (0..n_nodes * w).map(|i| absent_req(i % 2)).collect();
    for rec in records {
        let v = rec.endpoint.index();
        let t = rec.tag.index();
        if v >= n_nodes || t >= ntags as usize {
            continue;
        }
        let tr_i = rec.data_tr.index();
        let an_i = rec.kind.data_bound().index();
        let idx = v * w + (t * 2 + tr_i) * 2 + an_i;
        let r = canon_f64(rec.required);
        if an_i == An::Late.index() {
            ep_req[idx] = canon_f64(ep_req[idx].min(r));
        } else {
            ep_req[idx] = canon_f64(ep_req[idx].max(r));
        }
    }
    ep_req
}

/// Runs a full reverse-level backward required-time pass over `graph` and `lowered`.
#[must_use]
pub fn propagate_backward_all(
    graph: &TimingGraph,
    lowered: &LoweredCorner<'_>,
    propagated: &LoweredResult,
    node_values: &TimingValues,
    records: &[SlackRecord],
) -> CornerBackwardState {
    let n_nodes = graph.num_nodes();
    let ntags = lowered.ntags;
    let w = ntags as usize * 4;
    let endpoint_req = build_endpoint_req(n_nodes, ntags, records);
    let out_csr = OutgoingCsr::build(&lowered.csr);
    let order = graph.order();

    let mut pos_req: Vec<f64> = (0..n_nodes * w).map(|i| absent_req(i % 2)).collect();
    let mut row_buf = vec![0.0; w];

    for src_pos in (0..n_nodes).rev() {
        let src_node = order[src_pos].index();
        eval_backward_pos(
            src_pos,
            &endpoint_req[src_node * w..(src_node + 1) * w],
            &out_csr,
            lowered,
            &propagated.values,
            &pos_req,
            &mut row_buf,
        );
        pos_req[src_pos * w..(src_pos + 1) * w].copy_from_slice(&row_buf);
    }

    let mut node_req = vec![0.0; n_nodes * w];
    for (pos, &node) in order.iter().enumerate() {
        let v = node.index();
        node_req[v * w..(v + 1) * w].copy_from_slice(&pos_req[pos * w..(pos + 1) * w]);
    }

    let mut pin_required = vec![ABSENT_REQUIRED; n_nodes];
    let mut pin_slack = vec![ABSENT_SLACK; n_nodes];
    for v in 0..n_nodes {
        let (pr, ps) = compute_pin_req_and_slack(v, ntags as usize, node_values, &node_req);
        pin_required[v] = pr;
        pin_slack[v] = ps;
    }

    CornerBackwardState {
        ntags,
        endpoint_req,
        node_req,
        pin_required,
        pin_slack,
    }
}

/// Runs an incremental reverse-level backward cone required-time pass with bitwise
/// (`to_bits() == to_bits()`) change cutoff (`STA-REQ-0454`).
///
/// Returns `(backward_state, backward_seeds_count, backward_evaluated_count, backward_changed_count)`.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn propagate_backward_cone(
    old_graph: Option<&TimingGraph>,
    old_lowered: Option<&LoweredCorner<'_>>,
    old_node_values: &TimingValues,
    old_backward: &CornerBackwardState,
    new_graph: &TimingGraph,
    new_lowered: &LoweredCorner<'_>,
    new_propagated: &LoweredResult,
    new_node_values: &TimingValues,
    forward_changed_pos: &[u32],
    records: &[SlackRecord],
    mapping: &NodeMapping,
) -> (CornerBackwardState, usize, usize, usize) {
    let n_new = new_graph.num_nodes();
    let ntags = new_lowered.ntags;
    if old_backward.ntags != ntags {
        let state = propagate_backward_all(
            new_graph,
            new_lowered,
            new_propagated,
            new_node_values,
            records,
        );
        return (state, n_new, n_new, n_new);
    }

    let w = ntags as usize * 4;
    let endpoint_req = build_endpoint_req(n_new, ntags, records);
    let new_out_csr = OutgoingCsr::build(&new_lowered.csr);
    let old_out_csr = old_lowered.map(|ol| OutgoingCsr::build(&ol.csr));

    let new_order = new_graph.order();
    let mut pos_req: Vec<f64> = (0..n_new * w).map(|i| absent_req(i % 2)).collect();
    let mut seeds = Vec::new();

    for (new_pos, &new_node) in new_order.iter().enumerate() {
        let new_v = new_node.index();
        let Some(old_node) = mapping.new_to_old.get(new_v).copied().flatten() else {
            seeds.push(new_pos as u32);
            continue;
        };
        let old_v = old_node.index();
        if old_v * w + w > old_backward.node_req.len() {
            seeds.push(new_pos as u32);
            continue;
        }
        pos_req[new_pos * w..(new_pos + 1) * w]
            .copy_from_slice(&old_backward.node_req[old_v * w..(old_v + 1) * w]);

        let mut is_seed = mapping.directly_dirty[new_v];
        if !is_seed {
            let ep_changed = !slice_bitwise_eq(
                &old_backward.endpoint_req[old_v * w..(old_v + 1) * w],
                &endpoint_req[new_v * w..(new_v + 1) * w],
            );
            let slew_changed = !slice_bitwise_eq(
                &old_node_values.slew[old_v * 4..(old_v + 1) * 4],
                &new_node_values.slew[new_v * 4..(new_v + 1) * 4],
            );
            let arr_changed = !slice_bitwise_eq(
                &old_node_values.arr[old_v * w..(old_v + 1) * w],
                &new_node_values.arr[new_v * w..(new_v + 1) * w],
            );
            if ep_changed || slew_changed || arr_changed {
                is_seed = true;
            } else if let (Some(og), Some(ol), Some(o_out)) =
                (old_graph, old_lowered, old_out_csr.as_ref())
            {
                let old_pos = og.topo_rank(old_node) as usize;
                is_seed = !out_arcs_equivalent(
                    og,
                    ol,
                    o_out,
                    old_pos,
                    new_graph,
                    new_lowered,
                    &new_out_csr,
                    new_pos,
                    mapping,
                );
            } else {
                is_seed = true;
            }
        }
        if is_seed {
            seeds.push(new_pos as u32);
        }
    }

    let n_levels = new_lowered.csr.levels();
    let level_of = new_lowered.csr.level_of();
    let mut dirty = vec![false; n_new];
    let mut bucket: Vec<Vec<u32>> = vec![Vec::new(); n_levels];
    for &p in &seeds {
        if !dirty[p as usize] {
            dirty[p as usize] = true;
            bucket[level_of[p as usize] as usize].push(p);
        }
    }

    let mut evaluated = 0usize;
    let mut backward_changed_pos = Vec::new();
    let mut row_buf = vec![0.0; w];

    for l in (0..n_levels).rev() {
        if bucket[l].is_empty() {
            continue;
        }
        let mut list = std::mem::take(&mut bucket[l]);
        list.sort_unstable();
        evaluated += list.len();
        for &src_pos_u32 in &list {
            let src_pos = src_pos_u32 as usize;
            let src_node = new_order[src_pos].index();
            eval_backward_pos(
                src_pos,
                &endpoint_req[src_node * w..(src_node + 1) * w],
                &new_out_csr,
                new_lowered,
                &new_propagated.values,
                &pos_req,
                &mut row_buf,
            );
            let cur = &mut pos_req[src_pos * w..(src_pos + 1) * w];
            if slice_bitwise_eq(cur, &row_buf) {
                continue;
            }
            cur.copy_from_slice(&row_buf);
            backward_changed_pos.push(src_pos_u32);
            let in_begin = new_lowered.csr.in_ptr[src_pos] as usize;
            let in_end = new_lowered.csr.in_ptr[src_pos + 1] as usize;
            for in_arc in &new_lowered.csr.in_arcs[in_begin..in_end] {
                let u = in_arc.src as usize;
                if !dirty[u] {
                    dirty[u] = true;
                    bucket[level_of[u] as usize].push(u as u32);
                }
            }
        }
    }

    let mut node_req = vec![0.0; n_new * w];
    for (pos, &node) in new_order.iter().enumerate() {
        let v = node.index();
        node_req[v * w..(v + 1) * w].copy_from_slice(&pos_req[pos * w..(pos + 1) * w]);
    }

    // Update pin_required and pin_slack for all nodes touched by the forward or backward cone.
    let mut pin_dirty = vec![false; n_new];
    for &p in forward_changed_pos
        .iter()
        .chain(backward_changed_pos.iter())
        .chain(seeds.iter())
    {
        pin_dirty[new_order[p as usize].index()] = true;
    }

    let mut pin_required = vec![ABSENT_REQUIRED; n_new];
    let mut pin_slack = vec![ABSENT_SLACK; n_new];
    for v in 0..n_new {
        if !pin_dirty[v]
            && !mapping.directly_dirty[v]
            && let Some(old_node) = mapping.new_to_old[v]
            && old_node.index() < old_backward.pin_required.len()
        {
            pin_required[v] = old_backward.pin_required[old_node.index()];
            pin_slack[v] = old_backward.pin_slack[old_node.index()];
        } else {
            let (pr, ps) = compute_pin_req_and_slack(v, ntags as usize, new_node_values, &node_req);
            pin_required[v] = pr;
            pin_slack[v] = ps;
        }
    }

    let state = CornerBackwardState {
        ntags,
        endpoint_req,
        node_req,
        pin_required,
        pin_slack,
    };
    (state, seeds.len(), evaluated, backward_changed_pos.len())
}

#[allow(clippy::too_many_arguments)]
fn out_arcs_equivalent(
    old_graph: &TimingGraph,
    old_lowered: &LoweredCorner<'_>,
    old_out: &OutgoingCsr,
    old_pos: usize,
    new_graph: &TimingGraph,
    new_lowered: &LoweredCorner<'_>,
    new_out: &OutgoingCsr,
    new_pos: usize,
    mapping: &NodeMapping,
) -> bool {
    let o_slice = &old_out.arcs[old_out.ptr[old_pos] as usize..old_out.ptr[old_pos + 1] as usize];
    let n_slice = &new_out.arcs[new_out.ptr[new_pos] as usize..new_out.ptr[new_pos + 1] as usize];
    if o_slice.len() != n_slice.len() {
        return false;
    }
    let old_order = old_graph.order();
    let new_order = new_graph.order();
    for (&(o_dst_pos, o_arc_k), &(n_dst_pos, n_arc_k)) in o_slice.iter().zip(n_slice.iter()) {
        let old_dst_node = old_order[o_dst_pos as usize];
        let new_dst_node = new_order[n_dst_pos as usize];
        if mapping.new_to_old[new_dst_node.index()] != Some(old_dst_node) {
            return false;
        }
        if !loads_bitwise_eq(
            &old_lowered.load[o_dst_pos as usize],
            &new_lowered.load[n_dst_pos as usize],
        ) {
            return false;
        }
        let oa: InArc = old_lowered.csr.in_arcs[o_arc_k as usize];
        let na: InArc = new_lowered.csr.in_arcs[n_arc_k as usize];
        let o_cls: &ArcClass = &old_lowered.luts.classes[oa.class as usize];
        let n_cls: &ArcClass = &new_lowered.luts.classes[na.class as usize];
        if o_cls != n_cls {
            return false;
        }
        if !tag_maps_eq(&old_lowered.csr, oa.map, &new_lowered.csr, na.map) {
            return false;
        }
    }
    true
}

fn eval_backward_pos(
    src_pos: usize,
    ep_req_row: &[f64],
    out_csr: &OutgoingCsr,
    lowered: &LoweredCorner<'_>,
    pos_values: &TimingValues,
    pos_req: &[f64],
    out_row: &mut [f64],
) {
    let ntags = lowered.ntags as usize;
    let w = ntags * 4;
    out_row.copy_from_slice(ep_req_row);

    let out_begin = out_csr.ptr[src_pos] as usize;
    let out_end = out_csr.ptr[src_pos + 1] as usize;
    let classes = &lowered.luts.classes;
    let luts: &[LutDesc] = &lowered.luts.luts;
    let lut_data: &[f64] = &lowered.luts.data;

    for &(dst_pos_u32, arc_k_u32) in &out_csr.arcs[out_begin..out_end] {
        let dst_pos = dst_pos_u32 as usize;
        let arc_k = arc_k_u32 as usize;
        let in_arc = lowered.csr.in_arcs[arc_k];
        let cls = &classes[in_arc.class as usize];
        let dst_load = lowered.load[dst_pos];
        let recs: &[TagMapRec] = if in_arc.map == NONE {
            &[]
        } else {
            let m = in_arc.map as usize;
            &lowered.csr.maps[lowered.csr.map_ptr[m] as usize..lowered.csr.map_ptr[m + 1] as usize]
        };

        for ti in 0..2 {
            for to in 0..2 {
                if !cls.allows(ti, to) {
                    continue;
                }
                for an_i in 0..2 {
                    let s = pos_values.transition(src_pos, ti, an_i);
                    if s == absent(an_i) || !s.is_finite() {
                        continue;
                    }
                    let delay = if cls.is_wire() {
                        match lowered.wires.get(arc_k) {
                            Some(wr) => wr.delay[to * 2 + an_i],
                            None => 0.0,
                        }
                    } else {
                        let l = dst_load[to * 2 + an_i];
                        lookup(luts, lut_data, cls.delay[to], [s, l, l])
                    };

                    if in_arc.map == NONE {
                        for t in 0..ntags {
                            let r_dst = pos_req[dst_pos * w + (t * 2 + to) * 2 + an_i];
                            if !r_dst.is_finite() {
                                continue;
                            }
                            let cand = canon_f64(r_dst - delay);
                            let slot = &mut out_row[(t * 2 + ti) * 2 + an_i];
                            if an_i == An::Late.index() {
                                *slot = canon_f64((*slot).min(cand));
                            } else {
                                *slot = canon_f64((*slot).max(cand));
                            }
                        }
                    } else {
                        for r in recs {
                            let dst_t = r.dst_tag as usize;
                            let src_t = r.src_tag as usize;
                            if dst_t >= ntags || src_t >= ntags {
                                continue;
                            }
                            let r_dst = pos_req[dst_pos * w + (dst_t * 2 + to) * 2 + an_i];
                            if !r_dst.is_finite() {
                                continue;
                            }
                            let x = pos_values.arrival(src_pos, src_t, ti, an_i);
                            if x == absent(an_i) || !x.is_finite() {
                                continue;
                            }
                            if x + r.shift < r.open[an_i] {
                                continue;
                            }
                            let cand = canon_f64(r_dst - delay - r.shift);
                            let slot = &mut out_row[(src_t * 2 + ti) * 2 + an_i];
                            if an_i == An::Late.index() {
                                *slot = canon_f64((*slot).min(cand));
                            } else {
                                *slot = canon_f64((*slot).max(cand));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn compute_pin_req_and_slack(
    v: usize,
    ntags: usize,
    node_values: &TimingValues,
    node_req: &[f64],
) -> ([[f64; 2]; 2], [[f64; 2]; 2]) {
    let w = ntags * 4;
    let mut req_out = ABSENT_REQUIRED;
    let mut slack_out = ABSENT_SLACK;

    for tr_i in 0..2 {
        // Early (Hold): slack = arr - req
        let mut best_hold_slack = f64::INFINITY;
        let mut best_hold_req = f64::NEG_INFINITY;
        let mut max_early_req = f64::NEG_INFINITY;
        for t in 0..ntags {
            let r = node_req[v * w + (t * 2 + tr_i) * 2];
            if r.is_finite() {
                max_early_req = max_early_req.max(r);
                let a = node_values.arrival(v, t, tr_i, 0);
                if a.is_finite() {
                    let sl = canon_f64(a - r);
                    if sl < best_hold_slack {
                        best_hold_slack = sl;
                        best_hold_req = r;
                    }
                }
            }
        }
        req_out[tr_i][0] = if best_hold_slack.is_finite() {
            canon_f64(best_hold_req)
        } else {
            canon_f64(max_early_req)
        };
        slack_out[tr_i][0] = canon_f64(best_hold_slack);

        // Late (Setup): slack = req - arr
        let mut best_setup_slack = f64::INFINITY;
        let mut best_setup_req = f64::INFINITY;
        let mut min_late_req = f64::INFINITY;
        for t in 0..ntags {
            let r = node_req[v * w + (t * 2 + tr_i) * 2 + 1];
            if r.is_finite() {
                min_late_req = min_late_req.min(r);
                let a = node_values.arrival(v, t, tr_i, 1);
                if a.is_finite() {
                    let sl = canon_f64(r - a);
                    if sl < best_setup_slack {
                        best_setup_slack = sl;
                        best_setup_req = r;
                    }
                }
            }
        }
        req_out[tr_i][1] = if best_setup_slack.is_finite() {
            canon_f64(best_setup_req)
        } else {
            canon_f64(min_late_req)
        };
        slack_out[tr_i][1] = canon_f64(best_setup_slack);
    }

    let _ = pred_source(NO_PRED);
    (req_out, slack_out)
}
