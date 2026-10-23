// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Backward propagation of the seeded required times and the per-pin summary of
//! [`BackwardPass`] (owner: E6; `STA-REQ-0227`).

use rayon::prelude::*;
use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{NONE, TagMapRec, absent};

use super::{BackwardPass, absent_req, canon_zero};
use crate::engine::graph::TimingGraph;
use crate::engine::prop::lower::{LoweredCorner, LoweredResult, TimingValues};
use crate::engine::types::{An, NodeId, TagId, Tr};

/// The propagated arrivals and transitions that the backward pass reads, by level-ordered node
/// position: a [`LoweredResult`] itself, or the node-ordered copy of the analysis through the
/// level order ([`ByNode`]), which holds the same values.
pub trait PositionValues: Sync {
    /// The number of tags of the arrivals.
    fn ntags(&self) -> usize;
    /// The transition time at position `pos`.
    fn transition(&self, pos: usize, tr: usize, an: usize) -> f64;
    /// The arrival of tag `tag` at position `pos`.
    fn arrival(&self, pos: usize, tag: usize, tr: usize, an: usize) -> f64;
}

impl PositionValues for LoweredResult {
    fn ntags(&self) -> usize {
        self.values.ntags as usize
    }

    fn transition(&self, pos: usize, tr: usize, an: usize) -> f64 {
        self.values.transition(pos, tr, an)
    }

    fn arrival(&self, pos: usize, tag: usize, tr: usize, an: usize) -> f64 {
        self.values.arrival(pos, tag, tr, an)
    }
}

/// Node-ordered propagation values (`LoweredResult::to_node_order`) read by level-ordered
/// position through `order` (`TimingGraph::order`).
#[derive(Debug)]
pub struct ByNode<'a> {
    /// The node-ordered values.
    pub values: &'a TimingValues,
    /// The node at each level-ordered position.
    pub order: &'a [NodeId],
}

impl PositionValues for ByNode<'_> {
    fn ntags(&self) -> usize {
        self.values.ntags as usize
    }

    fn transition(&self, pos: usize, tr: usize, an: usize) -> f64 {
        self.values.transition(self.order[pos].index(), tr, an)
    }

    fn arrival(&self, pos: usize, tag: usize, tr: usize, an: usize) -> f64 {
        self.values.arrival(self.order[pos].index(), tag, tr, an)
    }
}

impl BackwardPass {
    /// Propagates seeded required times backwards in descending level order (`Levels`) across all
    /// enabled arcs (including latch `D -> Q` [`TagMapRec`] windows) and computes per-pin
    /// `pin_required`, `pin_arrival`, `pin_slack`, and `worst_tag` (`STA-REQ-0227`).
    ///
    /// Each source node takes the `min` (late) or `max` (early) of its seed and of the candidates
    /// of its outgoing arcs once every fanout node, on a higher level, is final; the nodes of one
    /// level are therefore independent and are evaluated in parallel. The `min`/`max` of a set does
    /// not depend on the order of its candidates (`canon_zero` folds `-0.0`), so the result is the
    /// one of the sequential push form.
    pub fn propagate<V: PositionValues>(
        &mut self,
        graph: &TimingGraph,
        lowered: &LoweredCorner<'_>,
        result: &V,
    ) {
        self.propagate_required(graph, lowered, result);
        self.summarize_pins(graph, result);
        if self.lean {
            self.tagged_req = Vec::new();
        }
    }

    /// The required-time propagation of [`Self::propagate`], without the per-pin summary.
    fn propagate_required<V: PositionValues>(
        &mut self,
        graph: &TimingGraph,
        lowered: &LoweredCorner<'_>,
        result: &V,
    ) {
        let order = graph.order();
        let n_pos = order.len();
        let in_ptr = &lowered.csr.in_ptr;
        let in_arcs = &lowered.csr.in_arcs;
        if in_ptr.len() != n_pos + 1 {
            self.propagate_push(graph, lowered, result);
            return;
        }

        // Fanout CSR over level-ordered positions: `(incoming-arc index, dst position)` per source.
        let mut out_ptr = vec![0u32; n_pos + 1];
        let mut rising = true;
        for dst_pos in 0..n_pos {
            let dst_level = graph.level(order[dst_pos]);
            for arc in &in_arcs[in_ptr[dst_pos] as usize..in_ptr[dst_pos + 1] as usize] {
                let src_pos = arc.src as usize;
                rising &= src_pos < n_pos && graph.level(order[src_pos]) < dst_level;
                if src_pos < n_pos {
                    out_ptr[src_pos + 1] += 1;
                }
            }
        }
        if !rising {
            self.propagate_push(graph, lowered, result);
            return;
        }
        for i in 0..n_pos {
            out_ptr[i + 1] += out_ptr[i];
        }
        let mut fill = out_ptr.clone();
        let mut out_arcs = vec![(0u32, 0u32); out_ptr[n_pos] as usize];
        for dst_pos in 0..n_pos {
            for k in in_ptr[dst_pos] as usize..in_ptr[dst_pos + 1] as usize {
                let src_pos = in_arcs[k].src as usize;
                out_arcs[fill[src_pos] as usize] = (k as u32, dst_pos as u32);
                fill[src_pos] += 1;
            }
        }
        drop(fill);

        const CHUNK: usize = 1 << 15;
        let w = self.ntags * 4;
        let level_start = graph.level_start();
        let mut vals: Vec<f64> = Vec::new();
        for l in (0..level_start.len().saturating_sub(1)).rev() {
            let (lo, hi) = (level_start[l] as usize, level_start[l + 1] as usize);
            for c_lo in (lo..hi).step_by(CHUNK) {
                let c_hi = (c_lo + CHUNK).min(hi);
                vals.clear();
                vals.resize((c_hi - c_lo) * w, 0.0);
                let this = &*self;
                vals.par_chunks_mut(w).enumerate().for_each(|(i, acc)| {
                    let src_pos = c_lo + i;
                    let v = order[src_pos].index();
                    acc.copy_from_slice(&this.tagged_req[v * w..(v + 1) * w]);
                    let fanout =
                        &out_arcs[out_ptr[src_pos] as usize..out_ptr[src_pos + 1] as usize];
                    this.pull_required(order, lowered, result, src_pos, fanout, acc);
                });
                for (i, acc) in vals.chunks_exact(w).enumerate() {
                    let v = order[c_lo + i].index();
                    self.tagged_req[v * w..(v + 1) * w].copy_from_slice(acc);
                }
            }
        }
        drop(vals);
        drop(out_arcs);
    }

    /// Folds into `acc` (the required times of source position `src_pos`, laid out like one node
    /// of `tagged_req`) the candidates of its outgoing arcs `fanout` (`(incoming-arc index, dst
    /// position)`), exactly as [`Self::propagate_push`] computes them.
    fn pull_required<V: PositionValues>(
        &self,
        order: &[NodeId],
        lowered: &LoweredCorner<'_>,
        result: &V,
        src_pos: usize,
        fanout: &[(u32, u32)],
        acc: &mut [f64],
    ) {
        let classes = &lowered.luts.classes;
        let luts = &lowered.luts.luts;
        let data = &lowered.luts.data;
        let ntags = self.ntags.min(result.ntags());
        let slot = |t: usize, tr: usize, an: usize| (t * 2 + tr) * 2 + an;
        let fold = |acc: &mut [f64], i: usize, an_i: usize, cand: f64| {
            acc[i] = if an_i == An::Late.index() {
                canon_zero(acc[i].min(cand))
            } else {
                canon_zero(acc[i].max(cand))
            };
        };
        for &(k, dst_pos) in fanout {
            let (k, dst_pos) = (k as usize, dst_pos as usize);
            let in_arc = &lowered.csr.in_arcs[k];
            let dst_v = order[dst_pos].index();
            let dst_load = lowered.load[dst_pos];
            let cls = &classes[in_arc.class as usize];
            let recs: &[TagMapRec] = if in_arc.map == NONE {
                &[]
            } else {
                lowered.csr.map(in_arc.map)
            };
            for ti in 0..2 {
                for to in 0..2 {
                    if !cls.allows(ti, to) {
                        continue;
                    }
                    for an_i in 0..2 {
                        let s = result.transition(src_pos, ti, an_i);
                        if s == absent(an_i) || !s.is_finite() {
                            continue;
                        }
                        let d = if cls.is_wire() {
                            lowered.wires.get(k).map_or(0.0, |w| w.delay[to * 2 + an_i])
                        } else {
                            let l = dst_load[to * 2 + an_i];
                            lookup(luts, data, cls.delay[to], [s, l, l])
                        };
                        if in_arc.map == NONE {
                            for t in 0..ntags {
                                let r_dst = self.tagged_req[self.req_idx(dst_v, t, to, an_i)];
                                if !r_dst.is_finite() {
                                    continue;
                                }
                                fold(acc, slot(t, ti, an_i), an_i, canon_zero(r_dst - d));
                            }
                        } else {
                            for r in recs {
                                let src_t = r.src_tag as usize;
                                let dst_t = r.dst_tag as usize;
                                if src_t >= ntags || dst_t >= ntags {
                                    continue;
                                }
                                let r_dst = self.tagged_req[self.req_idx(dst_v, dst_t, to, an_i)];
                                if !r_dst.is_finite() {
                                    continue;
                                }
                                let x = result.arrival(src_pos, src_t, ti, an_i);
                                if x == absent(an_i) || !x.is_finite() {
                                    continue;
                                }
                                if x + r.shift < r.open[an_i] {
                                    continue;
                                }
                                let cand = canon_zero(r_dst - d - r.shift);
                                fold(acc, slot(src_t, ti, an_i), an_i, cand);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Computes the per-pin worst-slack tag, required time, arrival and slack from the
    /// propagated `tagged_req` (STA-REQ-0227), every node independently and in place.
    fn summarize_pins<V: PositionValues>(&mut self, graph: &TimingGraph, result: &V) {
        let order = graph.order();
        let ntags = self.ntags.min(result.ntags());
        let width = self.ntags;
        let lean = self.lean;
        let Self {
            tagged_req,
            pin_required,
            pin_arrival,
            pin_slack,
            worst_tag,
            ..
        } = self;
        let tagged_req = &*tagged_req;
        let summarize = |pos: usize,
                         v: usize,
                         req_out: &mut [[f64; 2]; 2],
                         arr_out: &mut [[f64; 2]; 2],
                         slack_out: &mut [[f64; 2]; 2],
                         tag_out: &mut [[Option<TagId>; 2]; 2]| {
            for tr in Tr::ALL {
                let ti = tr.index();
                for an in An::ALL {
                    let ai = an.index();
                    let mut best_slack = f64::INFINITY;
                    let mut best_req = absent_req(an);
                    let mut best_arr = absent(ai);
                    let mut best_tag = None;
                    let mut fallback_req = absent_req(an);
                    let mut fallback_arr = absent(ai);

                    for t in 0..ntags {
                        let a = canon_zero(result.arrival(pos, t, ti, ai));
                        let r = canon_zero(tagged_req[((v * width + t) * 2 + ti) * 2 + ai]);
                        if a.is_finite() {
                            fallback_arr = match an {
                                An::Early => fallback_arr.min(a),
                                An::Late => fallback_arr.max(a),
                            };
                        }
                        if r.is_finite() {
                            fallback_req = match an {
                                An::Early => fallback_req.max(r),
                                An::Late => fallback_req.min(r),
                            };
                        }
                        if a.is_finite() && r.is_finite() {
                            let sl = match an {
                                An::Late => canon_zero(r - a),
                                An::Early => canon_zero(a - r),
                            };
                            if sl < best_slack {
                                best_slack = sl;
                                best_req = r;
                                best_arr = a;
                                best_tag = Some(TagId(t as u32));
                            }
                        }
                    }

                    if best_tag.is_some() {
                        req_out[ti][ai] = canon_zero(best_req);
                        arr_out[ti][ai] = canon_zero(best_arr);
                        slack_out[ti][ai] = canon_zero(best_slack);
                        tag_out[ti][ai] = best_tag;
                    } else {
                        req_out[ti][ai] = canon_zero(fallback_req);
                        arr_out[ti][ai] = canon_zero(fallback_arr);
                        slack_out[ti][ai] = f64::INFINITY;
                        tag_out[ti][ai] = None;
                    }
                }
            }
        };
        // Every node has one position in `order`; otherwise visit the positions in order. A lean
        // pass computes the arrival and worst tag of a pin and drops them.
        if lean && order.len() == pin_required.len() {
            pin_required
                .par_iter_mut()
                .zip(pin_slack.par_iter_mut())
                .enumerate()
                .with_min_len(1024)
                .for_each(|(v, (req, slack))| {
                    let pos = graph.topo_rank(NodeId(v as u32)) as usize;
                    let (mut arr, mut tag) = ([[0.0; 2]; 2], [[None; 2]; 2]);
                    summarize(pos, v, req, &mut arr, slack, &mut tag);
                });
        } else if lean {
            for (pos, &node) in order.iter().enumerate() {
                let v = node.index();
                let (mut arr, mut tag) = ([[0.0; 2]; 2], [[None; 2]; 2]);
                summarize(
                    pos,
                    v,
                    &mut pin_required[v],
                    &mut arr,
                    &mut pin_slack[v],
                    &mut tag,
                );
            }
        } else if order.len() == pin_required.len() {
            pin_required
                .par_iter_mut()
                .zip(pin_arrival.par_iter_mut())
                .zip(pin_slack.par_iter_mut())
                .zip(worst_tag.par_iter_mut())
                .enumerate()
                .with_min_len(1024)
                .for_each(|(v, (((req, arr), slack), tag))| {
                    let pos = graph.topo_rank(NodeId(v as u32)) as usize;
                    summarize(pos, v, req, arr, slack, tag);
                });
        } else {
            for (pos, &node) in order.iter().enumerate() {
                let v = node.index();
                summarize(
                    pos,
                    v,
                    &mut pin_required[v],
                    &mut pin_arrival[v],
                    &mut pin_slack[v],
                    &mut worst_tag[v],
                );
            }
        }
    }

    /// The sequential form of the required-time propagation of [`Self::propagate`]: each node in
    /// descending level order pushes its required times to the sources of its incoming arcs. It
    /// serves a CSR whose arcs do not all rise in level.
    fn propagate_push<V: PositionValues>(
        &mut self,
        graph: &TimingGraph,
        lowered: &LoweredCorner<'_>,
        result: &V,
    ) {
        let order = graph.order();
        let in_ptr = &lowered.csr.in_ptr;
        let in_arcs = &lowered.csr.in_arcs;
        let classes = &lowered.luts.classes;
        let luts = &lowered.luts.luts;
        let data = &lowered.luts.data;
        let ntags = self.ntags.min(result.ntags());

        for (dst_pos, &dst_node) in order.iter().enumerate().rev() {
            let dst_v = dst_node.index();
            let dst_load = lowered.load[dst_pos];
            let arc_lo = in_ptr[dst_pos] as usize;
            let arc_hi = in_ptr[dst_pos + 1] as usize;

            for (csr_k, in_arc) in in_arcs[arc_lo..arc_hi].iter().enumerate() {
                let k = arc_lo + csr_k;
                let src_pos = in_arc.src as usize;
                let src_v = order[src_pos].index();
                let cls = &classes[in_arc.class as usize];
                let recs: &[TagMapRec] = if in_arc.map == NONE {
                    &[]
                } else {
                    lowered.csr.map(in_arc.map)
                };

                for ti in 0..2 {
                    for to in 0..2 {
                        if !cls.allows(ti, to) {
                            continue;
                        }
                        for an_i in 0..2 {
                            let s = result.transition(src_pos, ti, an_i);
                            if s == absent(an_i) || !s.is_finite() {
                                continue;
                            }
                            let d = if cls.is_wire() {
                                lowered.wires.get(k).map_or(0.0, |w| w.delay[to * 2 + an_i])
                            } else {
                                let l = dst_load[to * 2 + an_i];
                                lookup(luts, data, cls.delay[to], [s, l, l])
                            };

                            if in_arc.map == NONE {
                                for t in 0..ntags {
                                    let r_dst = self.tagged_req[self.req_idx(dst_v, t, to, an_i)];
                                    if !r_dst.is_finite() {
                                        continue;
                                    }
                                    let cand = canon_zero(r_dst - d);
                                    let src_idx = self.req_idx(src_v, t, ti, an_i);
                                    if an_i == An::Late.index() {
                                        self.tagged_req[src_idx] =
                                            canon_zero(self.tagged_req[src_idx].min(cand));
                                    } else {
                                        self.tagged_req[src_idx] =
                                            canon_zero(self.tagged_req[src_idx].max(cand));
                                    }
                                }
                            } else {
                                for r in recs {
                                    let src_t = r.src_tag as usize;
                                    let dst_t = r.dst_tag as usize;
                                    if src_t >= ntags || dst_t >= ntags {
                                        continue;
                                    }
                                    let r_dst =
                                        self.tagged_req[self.req_idx(dst_v, dst_t, to, an_i)];
                                    if !r_dst.is_finite() {
                                        continue;
                                    }
                                    let x = result.arrival(src_pos, src_t, ti, an_i);
                                    if x == absent(an_i) || !x.is_finite() {
                                        continue;
                                    }
                                    if x + r.shift < r.open[an_i] {
                                        continue;
                                    }
                                    let cand = canon_zero(r_dst - d - r.shift);
                                    let src_idx = self.req_idx(src_v, src_t, ti, an_i);
                                    if an_i == An::Late.index() {
                                        self.tagged_req[src_idx] =
                                            canon_zero(self.tagged_req[src_idx].min(cand));
                                    } else {
                                        self.tagged_req[src_idx] =
                                            canon_zero(self.tagged_req[src_idx].max(cand));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
