// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Best-first sidetrack `k`-worst path search and sequential launch expansion
//! (owner: E6; `STA-REQ-0215`, `STA-REQ-0228`).

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{NONE, TagMapRec, absent};
use rusteda_core::job::Diag;

use super::{PathArcKind, PathHandle, PathPoint, TimingPath};
use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::DriverLoad;
use crate::engine::graph::{ArcKind, ArcSense, TimingGraph};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::prop::backward::{EndpointRequirement, canon_zero};
use crate::engine::prop::lower::{IdealClockSpec, IdealSchedule, LoweredCorner, LoweredResult};
use crate::engine::types::{An, CornerId, LibArc, NetRow, NodeId, TagId, Tr};

/// Propagates ideal clock sense bits (bit 0 positive, bit 1 negative) across enabled net and
/// combinational/three-state arcs, skipping latch data arcs (`STA-REQ-0200`).
#[must_use]
pub fn propagate_clock_sense(
    graph: &TimingGraph,
    clocks: &[IdealClockSpec],
) -> Vec<Vec<(u32, u8)>> {
    let n_nodes = graph.num_nodes();
    let mut clock_sense = vec![Vec::new(); n_nodes];
    let g_arcs = graph.arcs();

    for (ci, ck) in clocks.iter().enumerate() {
        let mut bits = vec![0u8; n_nodes];
        let mut stack = Vec::new();
        for &src in &ck.sources {
            if src.index() < n_nodes {
                bits[src.index()] |= 1;
                stack.push((src, 1u8));
            }
        }
        while let Some((v, b)) = stack.pop() {
            for &ai in graph.outgoing(v) {
                let arc = &g_arcs[ai as usize];
                if arc.kind.is_latch_data() {
                    continue;
                }
                let nb = match arc.kind {
                    ArcKind::Net { .. } => b,
                    _ => match arc.sense {
                        ArcSense::PositiveUnate => b,
                        ArcSense::NegativeUnate => ((b & 1) << 1) | ((b >> 1) & 1),
                        ArcSense::NonUnate => 3,
                    },
                };
                let w = arc.dst.index();
                if (bits[w] | nb) != bits[w] {
                    bits[w] |= nb;
                    stack.push((arc.dst, bits[w]));
                }
            }
        }
        for (v, &b) in bits.iter().enumerate() {
            if b != 0 {
                clock_sense[v].push((ci as u32, b));
            }
        }
    }
    clock_sense
}

pub(crate) fn source_edges(pin_edge: Tr, bits: u8) -> Vec<Tr> {
    let mut out = Vec::with_capacity(2);
    if bits & 1 != 0 {
        out.push(pin_edge);
    }
    if bits & 2 != 0 {
        out.push(pin_edge.opposite());
    }
    out
}

pub(crate) fn map_cell_arcs(
    netview: &NetlistView<'_>,
    lib: &TimingLib,
) -> Result<Vec<Vec<LibArc>>, Diag> {
    let n_cells = netview.num_cells();
    let mut out = Vec::with_capacity(n_cells);
    for cell_row in 0..n_cells {
        let cell_name = netview.cell_name(cell_row as u32);
        let tc = lib.cell(cell_name).ok_or_else(|| {
            Diag::error(
                "T0004",
                format!(
                    "corner '{}': unknown cell '{cell_name}' (STA-REQ-0003)",
                    lib.corner
                ),
            )
        })?;
        out.push(tc.arcs.clone());
    }
    Ok(out)
}

pub(crate) fn canonical_net_totals<'a>(
    netview: &NetlistView<'_>,
    net_loads: &'a NetLoads,
) -> Cow<'a, [[[f64; 2]; 2]]> {
    if netview.frozen().assigns.is_empty() {
        return Cow::Borrowed(&net_loads.totals);
    }
    let n_nets = netview.num_nets();
    let mut totals = vec![[[0.0f64; 2]; 2]; n_nets];
    for (raw_idx, raw_tot) in net_loads.totals.iter().enumerate() {
        if let NetTarget::Net(canon) = netview.resolve_net(NetRow(raw_idx as u32))
            && let Some(dst) = totals.get_mut(canon.index())
        {
            for tr in 0..2 {
                for an in 0..2 {
                    dst[tr][an] += raw_tot[tr][an];
                }
            }
        }
    }
    Cow::Owned(totals)
}

fn to_fs(ns: f64) -> i64 {
    (ns * 1e6).round() as i64
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.abs().max(1)
}

pub(crate) fn pulse_width_ns(ck: &IdealClockSpec, e_open: Tr, e_close: Tr) -> f64 {
    let p = to_fs(ck.period);
    if p <= 0 {
        return 0.0;
    }
    let w_open = to_fs(ck.waveform[e_open.index()]);
    let w_close = to_fs(ck.waveform[e_close.index()]);
    let mut diff = (w_close - w_open).rem_euclid(p);
    if diff <= 0 {
        diff += p;
    }
    diff as f64 * 1e-6
}

/// Smallest strictly positive `capture - launch` relation in ns over the common period
/// (`STA-REQ-0220`).
#[must_use]
pub fn setup_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    let (tl, tc) = (to_fs(launch.period), to_fs(capture.period));
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let mult = tc / gcd(tl, tc);
    if mult > 1000 {
        return None;
    }
    let (wl, wc) = (
        to_fs(launch.waveform[e_l.index()]),
        to_fs(capture.waveform[e_c.index()]),
    );
    let mut best = i64::MAX;
    for k in 0..mult {
        let l = wl + k * tl;
        let min_c = l + 1;
        let j = (min_c - wc).div_euclid(tc);
        let mut c = wc + j * tc;
        if c < min_c {
            c += tc;
        }
        best = best.min(c - l);
    }
    (best < i64::MAX).then_some(best as f64 * 1e-6)
}

/// Largest non-positive `capture - launch` relation in ns over the common period (`STA-REQ-0220`).
#[must_use]
pub fn hold_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    let (tl, tc) = (to_fs(launch.period), to_fs(capture.period));
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let mult = tc / gcd(tl, tc);
    if mult > 1000 {
        return None;
    }
    let (wl, wc) = (
        to_fs(launch.waveform[e_l.index()]),
        to_fs(capture.waveform[e_c.index()]),
    );
    let mut best = i64::MIN;
    for k in 0..mult {
        let l = wl + k * tl;
        let j = (l - wc).div_euclid(tc);
        let mut c = wc + j * tc;
        if c > l {
            c -= tc;
        }
        best = best.max(c - l);
    }
    (best > i64::MIN).then_some(best as f64 * 1e-6)
}

/// One node step in a raw data-path trace from startpoint to endpoint.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RawStep {
    node: NodeId,
    tag: usize,
    tr: Tr,
    /// `Some(graph_arc_idx)` for an arc step `prev -> node`, `None` at the data startpoint.
    in_graph_arc: Option<u32>,
    /// Tag map record index within `in_arc.map` when the step crossed a `TagMapRec`.
    map_rec_idx: Option<usize>,
}

/// Incoming choice at `(dst_node, dst_tag, dst_tr, an)`.
#[derive(Debug, Clone, Copy)]
struct IncomingChoice {
    /// `Some((src_node, src_tag, src_tr, graph_arc_idx, map_rec_idx))` for an incoming arc, or
    /// `None` for the startpoint row at `dst_node`.
    arc: Option<(NodeId, usize, Tr, u32, Option<usize>)>,
}

pub(crate) struct SearchCtx<'a> {
    netview: &'a NetlistView<'a>,
    graph: &'a TimingGraph,
    lib: &'a TimingLib,
    schedule: &'a IdealSchedule,
    lowered: &'a LoweredCorner<'a>,
    result: &'a LoweredResult,
    cell_arcs: Vec<Vec<LibArc>>,
    canon_totals: Cow<'a, [[[f64; 2]; 2]]>,
    clock_sense: Vec<Vec<(u32, u8)>>,
    start_row_by_pos: Vec<u32>,
}

impl<'a> SearchCtx<'a> {
    pub(crate) fn new(
        netview: &'a NetlistView<'a>,
        graph: &'a TimingGraph,
        lib: &'a TimingLib,
        net_loads: &'a NetLoads,
        schedule: &'a IdealSchedule,
        lowered: &'a LoweredCorner<'a>,
        result: &'a LoweredResult,
    ) -> Self {
        let cell_arcs = map_cell_arcs(netview, lib).unwrap_or_default();
        let canon_totals = canonical_net_totals(netview, net_loads);
        let clock_sense = propagate_clock_sense(graph, &schedule.clocks);
        let mut start_row_by_pos = vec![NONE; graph.num_nodes()];
        for (k, &pos) in lowered.starts.node.iter().enumerate() {
            if (pos as usize) < start_row_by_pos.len() {
                start_row_by_pos[pos as usize] = k as u32;
            }
        }
        Self {
            netview,
            graph,
            lib,
            schedule,
            lowered,
            result,
            cell_arcs,
            canon_totals,
            clock_sense,
            start_row_by_pos,
        }
    }

    /// Enumerates all valid incoming choices into `(dst_node, dst_tag, dst_tr, an)` in the exact
    /// CSR evaluation order of `rusteda_compute`, returning `(tree_choice_idx, choices)`.
    fn incoming_choices(
        &self,
        dst_node: NodeId,
        dst_tag: usize,
        dst_tr: Tr,
        an: An,
    ) -> (Option<usize>, Vec<IncomingChoice>) {
        let dst_pos = self.graph.topo_rank(dst_node) as usize;
        let to = dst_tr.index();
        let an_i = an.index();
        let ntags = self.lowered.ntags as usize;
        let mut choices = Vec::new();
        let mut best_idx = None;
        let mut best_arr = absent(an_i);

        let is_better = |x: f64, y: f64| -> bool {
            match an {
                An::Early => x < y,
                An::Late => x > y,
            }
        };

        // 1. Check if `(dst_node, dst_tag, dst_tr, an)` has a seeded start row arrival.
        let sr = self.start_row_by_pos[dst_pos];
        if sr != NONE {
            let (s_arr, _) = self.lowered.starts.row(sr as usize, self.lowered.ntags);
            let idx = (dst_tag * 2 + to) * 2 + an_i;
            if let Some(&a0) = s_arr.get(idx)
                && a0 != absent(an_i)
                && a0.is_finite()
            {
                let c0 = canon_zero(a0);
                best_arr = c0;
                best_idx = Some(choices.len());
                choices.push(IncomingChoice { arc: None });
            }
        }

        // 2. Scan incoming CSR arcs in exact CSR order (`ti in 0..2`).
        let in_ptr = &self.lowered.csr.in_ptr;
        let in_arcs = &self.lowered.csr.in_arcs;
        let classes = &self.lowered.luts.classes;
        let luts = &self.lowered.luts.luts;
        let data = &self.lowered.luts.data;
        let dst_load = self.lowered.load[dst_pos];
        let arc_lo = in_ptr[dst_pos] as usize;
        let arc_hi = in_ptr[dst_pos + 1] as usize;

        for (csr_off, in_arc) in in_arcs[arc_lo..arc_hi].iter().enumerate() {
            let csr_k = arc_lo + csr_off;
            let graph_arc_idx = self.lowered.csr_arc_to_graph_arc[csr_k];
            let src_pos = in_arc.src as usize;
            let src_node = self.graph.order()[src_pos];
            let cls = &classes[in_arc.class as usize];
            let recs: &[TagMapRec] = if in_arc.map == NONE {
                &[]
            } else {
                self.lowered.csr.map(in_arc.map)
            };

            for ti in 0..2 {
                if !cls.allows(ti, to) {
                    continue;
                }
                let src_tr = if ti == 0 { Tr::Rise } else { Tr::Fall };
                let s = self.result.values.transition(src_pos, ti, an_i);
                if s == absent(an_i) || !s.is_finite() {
                    continue;
                }
                let l = dst_load[to * 2 + an_i];
                let d = if cls.is_wire() {
                    self.lowered
                        .wires
                        .get(csr_k)
                        .map_or(0.0, |w| w.delay[to * 2 + an_i])
                } else {
                    lookup(luts, data, cls.delay[to], [s, l, l])
                };

                if in_arc.map == NONE {
                    if dst_tag >= ntags {
                        continue;
                    }
                    let x = self.result.values.arrival(src_pos, dst_tag, ti, an_i);
                    if x == absent(an_i) || !x.is_finite() {
                        continue;
                    }
                    let cand = canon_zero(x + d);
                    let idx = choices.len();
                    choices.push(IncomingChoice {
                        arc: Some((src_node, dst_tag, src_tr, graph_arc_idx, None)),
                    });
                    if is_better(cand, best_arr) {
                        best_arr = cand;
                        best_idx = Some(idx);
                    }
                } else {
                    for (ri, r) in recs.iter().enumerate() {
                        if r.dst_tag as usize != dst_tag || r.src_tag as usize >= ntags {
                            continue;
                        }
                        let src_t = r.src_tag as usize;
                        let x = self.result.values.arrival(src_pos, src_t, ti, an_i);
                        if x == absent(an_i) || !x.is_finite() {
                            continue;
                        }
                        let Some(m) = self.eval_tag_map(r, x, s, l, cls.setup[ti], an_i) else {
                            continue;
                        };
                        let cand = canon_zero(m + d);
                        let idx = choices.len();
                        choices.push(IncomingChoice {
                            arc: Some((src_node, src_t, src_tr, graph_arc_idx, Some(ri))),
                        });
                        if is_better(cand, best_arr) {
                            best_arr = cand;
                            best_idx = Some(idx);
                        }
                    }
                }
            }
        }

        (best_idx, choices)
    }

    fn eval_tag_map(
        &self,
        r: &TagMapRec,
        x: f64,
        s: f64,
        l: f64,
        setup_lut: u32,
        an: usize,
    ) -> Option<f64> {
        let mut m = x + r.shift;
        if m < r.open[an] {
            return None;
        }
        let close = if setup_lut != NONE {
            let su = lookup(
                &self.lowered.luts.luts,
                &self.lowered.luts.data,
                setup_lut,
                [r.close_slew[an], s, l],
            );
            r.close[an] - su
        } else {
            r.close[an]
        };
        let cap = close.min(r.limit[an]);
        if an == An::Late.index() && m > cap {
            m = cap;
        }
        Some(canon_zero(m))
    }

    /// Traces the primary tree prefix backwards from `(node, tag, tr, an)` to its startpoint,
    /// returning the forward-ordered `Vec<RawStep>`.
    fn trace_tree_prefix(&self, node: NodeId, tag: usize, tr: Tr, an: An) -> Vec<RawStep> {
        let mut rev = Vec::new();
        let mut cur_node = node;
        let mut cur_tag = tag;
        let mut cur_tr = tr;
        let max_steps = self.graph.num_nodes() + 4;

        for _ in 0..max_steps {
            let (tree_idx, choices) = self.incoming_choices(cur_node, cur_tag, cur_tr, an);
            let Some(idx) = tree_idx else {
                rev.push(RawStep {
                    node: cur_node,
                    tag: cur_tag,
                    tr: cur_tr,
                    in_graph_arc: None,
                    map_rec_idx: None,
                });
                break;
            };
            match choices[idx].arc {
                None => {
                    rev.push(RawStep {
                        node: cur_node,
                        tag: cur_tag,
                        tr: cur_tr,
                        in_graph_arc: None,
                        map_rec_idx: None,
                    });
                    break;
                }
                Some((src_node, src_tag, src_tr, g_arc, map_ri)) => {
                    if rev
                        .iter()
                        .any(|s: &RawStep| s.node == src_node && s.tag == src_tag && s.tr == src_tr)
                    {
                        rev.push(RawStep {
                            node: cur_node,
                            tag: cur_tag,
                            tr: cur_tr,
                            in_graph_arc: None,
                            map_rec_idx: None,
                        });
                        break;
                    }
                    rev.push(RawStep {
                        node: cur_node,
                        tag: cur_tag,
                        tr: cur_tr,
                        in_graph_arc: Some(g_arc),
                        map_rec_idx: map_ri,
                    });
                    cur_node = src_node;
                    cur_tag = src_tag;
                    cur_tr = src_tr;
                }
            }
        }

        rev.reverse();
        rev
    }

    /// Evaluates a `RawStep` sequence from startpoint to endpoint against `req` and constructs its
    /// [`TimingPath`] and reverse-topological tie-break signature.
    fn materialize_path(
        &self,
        steps: &[RawStep],
        req: &EndpointRequirement,
        corner: CornerId,
    ) -> Option<(TimingPath, Vec<(u32, u8)>)> {
        let first = *steps.first()?;
        let an = req.an;
        let an_i = an.index();
        let first_pos = self.graph.topo_rank(first.node) as usize;
        let sr = self.start_row_by_pos[first_pos];
        let init_arr = if sr != NONE {
            let (s_arr, _) = self.lowered.starts.row(sr as usize, self.lowered.ntags);
            let idx = (first.tag * 2 + first.tr.index()) * 2 + an_i;
            s_arr
                .get(idx)
                .copied()
                .filter(|&v| v != absent(an_i) && v.is_finite())
                .map(canon_zero)?
        } else {
            return None;
        };
        let first_slew = canon_zero(self.result.values.transition(
            first_pos,
            first.tr.index(),
            an_i,
        ));

        let mut points = Vec::with_capacity(steps.len() + 1);

        // 1. Expand startpoint (`first.node`, `first.tag`, `first.tr`).
        let mut curr_arr = if let Some((ck_pt, launch_pt, launch_arr)) =
            self.expand_sequential_launch(first.node, first.tag, first.tr, an, first_slew)
        {
            points.push(ck_pt);
            points.push(launch_pt);
            launch_arr
        } else {
            let (inst_name, cell_name) = self.node_inst_and_cell(first.node);
            let arc_kind = if self.is_input_delay_port(first.node, first.tag) {
                PathArcKind::Input
            } else if self.is_clock_source(first.node, first.tag) {
                PathArcKind::Clock
            } else {
                PathArcKind::Input
            };
            points.push(PathPoint {
                node: first.node,
                pin_name: self.netview.node_name_str(first.node).into_owned(),
                inst_name,
                cell_name,
                arc_kind,
                transition: first.tr,
                incr_ns: init_arr,
                arrival_ns: init_arr,
                slew_ns: first_slew,
            });
            init_arr
        };

        // 2. Walk `steps[1..]` forward to accumulate exact step delays and arrivals.
        let g_arcs = self.graph.arcs();
        let classes = &self.lowered.luts.classes;
        let luts = &self.lowered.luts.luts;
        let data = &self.lowered.luts.data;

        for idx in 1..steps.len() {
            let prev = steps[idx - 1];
            let step = steps[idx];
            let g_arc_idx = step.in_graph_arc?;
            let csr_k = self.lowered.graph_arc_to_csr_arc[g_arc_idx as usize];
            if csr_k == NONE {
                return None;
            }
            let in_arc = self.lowered.csr.in_arcs[csr_k as usize];
            let cls = &classes[in_arc.class as usize];
            let src_pos = self.graph.topo_rank(prev.node) as usize;
            let dst_pos = self.graph.topo_rank(step.node) as usize;
            let ti = prev.tr.index();
            let to = step.tr.index();
            let s_in = self.result.values.transition(src_pos, ti, an_i);
            let l = self.lowered.load[dst_pos][to * 2 + an_i];
            let d = if cls.is_wire() {
                self.lowered
                    .wires
                    .get(csr_k as usize)
                    .map_or(0.0, |w| w.delay[to * 2 + an_i])
            } else {
                lookup(luts, data, cls.delay[to], [s_in, l, l])
            };

            let entry_arr = if let Some(ri) = step.map_rec_idx
                && in_arc.map != NONE
            {
                let recs = self.lowered.csr.map(in_arc.map);
                let r = recs.get(ri)?;
                self.eval_tag_map(r, curr_arr, s_in, l, cls.setup[ti], an_i)?
            } else {
                curr_arr
            };
            let next_arr = canon_zero(entry_arr + d);
            let incr = canon_zero(d);
            curr_arr = next_arr;

            let g_arc = &g_arcs[g_arc_idx as usize];
            let arc_kind = if g_arc.kind.is_net() {
                PathArcKind::Net
            } else {
                PathArcKind::Cell
            };
            let (inst_name, cell_name) = self.node_inst_and_cell(step.node);
            let dst_slew = canon_zero(self.result.values.transition(dst_pos, to, an_i));
            points.push(PathPoint {
                node: step.node,
                pin_name: self.netview.node_name_str(step.node).into_owned(),
                inst_name,
                cell_name,
                arc_kind,
                transition: step.tr,
                incr_ns: incr,
                arrival_ns: next_arr,
                slew_ns: dst_slew,
            });
        }

        let arrival_ns = canon_zero(curr_arr);
        let required_ns = canon_zero(req.required_ns);
        let slack_ns = match an {
            An::Late => canon_zero(required_ns - arrival_ns),
            An::Early => canon_zero(arrival_ns - required_ns),
        };

        // 3. Compute `logic_levels`, `cell_delay_ns`, `wire_delay_ns`, `cell_delay_pct`,
        // `wire_delay_pct`.
        let mut logic_levels = 0u32;
        let mut cell_delay_ns = 0.0f64;
        let mut wire_delay_ns = 0.0f64;
        for pt in &points {
            match pt.arc_kind {
                PathArcKind::Cell => {
                    logic_levels += 1;
                    cell_delay_ns += pt.incr_ns.max(0.0);
                }
                PathArcKind::Launch => {
                    cell_delay_ns += pt.incr_ns.max(0.0);
                }
                PathArcKind::Net => {
                    wire_delay_ns += pt.incr_ns.max(0.0);
                }
                PathArcKind::Clock | PathArcKind::Input => {}
            }
        }
        cell_delay_ns = canon_zero(cell_delay_ns);
        wire_delay_ns = canon_zero(wire_delay_ns);
        let total_delay = cell_delay_ns + wire_delay_ns;
        let (cell_delay_pct, wire_delay_pct) = if total_delay > 0.0 {
            let c_pct = ((cell_delay_ns / total_delay) * 1000.0).round() / 10.0;
            let w_pct = ((100.0 - c_pct) * 10.0).round() / 10.0;
            (canon_zero(c_pct), canon_zero(w_pct))
        } else {
            (0.0, 0.0)
        };

        let start_pt = points.first()?;
        let end_pt = points.last()?;
        let launch_tag = TagId(first.tag as u32);
        let (launch_clock, launch_edge) = self.tag_clock_and_edge(first.tag);
        let capture_clock = req
            .capture_clock
            .and_then(|c| self.schedule.clocks.get(c.index()))
            .map(|ck| ck.name.clone());

        let topo_sig: Vec<(u32, u8)> = points
            .iter()
            .rev()
            .map(|pt| (self.graph.topo_rank(pt.node), pt.transition.index() as u8))
            .collect();

        let path = TimingPath {
            handle: PathHandle::new(1),
            corner,
            startpoint: start_pt.node,
            startpoint_name: start_pt.pin_name.clone(),
            endpoint: end_pt.node,
            endpoint_name: end_pt.pin_name.clone(),
            role: req.role,
            bound: an,
            path_group: req.path_group.clone(),
            launch_tag,
            launch_clock,
            launch_edge,
            capture_clock,
            capture_edge: req.capture_edge,
            slack_ns,
            arrival_ns,
            required_ns,
            check_ns: canon_zero(req.check_ns),
            logic_levels,
            cell_delay_ns,
            wire_delay_ns,
            cell_delay_pct,
            wire_delay_pct,
            points,
        };

        Some((path, topo_sig))
    }

    /// Finds the `k` worst paths for a single endpoint check group (`reqs` are the
    /// `(tag, end_tr)` requirements of that endpoint check) using best-first sidetrack deviation
    /// search (`STA-REQ-0215`, `STA-REQ-0228`).
    pub(crate) fn search_k_worst_for_check(
        &self,
        reqs: &[&EndpointRequirement],
        k: usize,
        corner: CornerId,
    ) -> Vec<(TimingPath, Vec<(u32, u8)>)> {
        #[derive(Clone)]
        struct HeapEntry {
            slack_ns: f64,
            topo_sig: Vec<(u32, u8)>,
            launch_tag: TagId,
            req_idx: usize,
            steps: Vec<RawStep>,
            branch_limit: usize,
            path: TimingPath,
        }

        impl PartialEq for HeapEntry {
            fn eq(&self, other: &Self) -> bool {
                self.slack_ns.to_bits() == other.slack_ns.to_bits()
                    && self.topo_sig == other.topo_sig
                    && self.launch_tag == other.launch_tag
            }
        }

        impl Eq for HeapEntry {}

        impl PartialOrd for HeapEntry {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for HeapEntry {
            fn cmp(&self, other: &Self) -> Ordering {
                other
                    .slack_ns
                    .total_cmp(&self.slack_ns)
                    .then_with(|| other.topo_sig.cmp(&self.topo_sig))
                    .then_with(|| other.launch_tag.cmp(&self.launch_tag))
            }
        }

        const MAX_KWORST_PER_CHECK: usize = 256;
        const MAX_SIDETRACK_EXPANSIONS: usize = 4096;
        let k = k.min(MAX_KWORST_PER_CHECK);

        let mut heap = BinaryHeap::new();
        let mut expansions = 0usize;

        for (req_idx, &req) in reqs.iter().enumerate() {
            let steps = self.trace_tree_prefix(req.node, req.tag.index(), req.tr, req.an);
            if steps.is_empty() {
                continue;
            }
            let branch_limit = steps.len();
            if let Some((path, topo_sig)) = self.materialize_path(&steps, req, corner) {
                heap.push(HeapEntry {
                    slack_ns: path.slack_ns,
                    topo_sig,
                    launch_tag: path.launch_tag,
                    req_idx,
                    steps,
                    branch_limit,
                    path,
                });
                expansions += 1;
            }
        }

        let mut out = Vec::with_capacity(k);
        while out.len() < k {
            let Some(curr) = heap.pop() else {
                break;
            };
            let req = reqs[curr.req_idx];

            if out.len() + 1 < k && expansions < MAX_SIDETRACK_EXPANSIONS {
                for branch_pos in 0..curr.branch_limit {
                    if expansions >= MAX_SIDETRACK_EXPANSIONS {
                        break;
                    }
                    let dst_step = curr.steps[branch_pos];
                    let (tree_idx, choices) =
                        self.incoming_choices(dst_step.node, dst_step.tag, dst_step.tr, req.an);
                    for (ci, ch) in choices.iter().enumerate() {
                        if Some(ci) == tree_idx || expansions >= MAX_SIDETRACK_EXPANSIONS {
                            continue;
                        }
                        let (mut new_steps, new_branch_limit) = match ch.arc {
                            None => (
                                vec![RawStep {
                                    node: dst_step.node,
                                    tag: dst_step.tag,
                                    tr: dst_step.tr,
                                    in_graph_arc: None,
                                    map_rec_idx: None,
                                }],
                                0,
                            ),
                            Some((src_node, src_tag, src_tr, g_arc, map_ri)) => {
                                let mut prefix =
                                    self.trace_tree_prefix(src_node, src_tag, src_tr, req.an);
                                let limit = prefix.len();
                                prefix.push(RawStep {
                                    node: dst_step.node,
                                    tag: dst_step.tag,
                                    tr: dst_step.tr,
                                    in_graph_arc: Some(g_arc),
                                    map_rec_idx: map_ri,
                                });
                                (prefix, limit)
                            }
                        };
                        let suffix = &curr.steps[branch_pos + 1..];
                        if suffix
                            .iter()
                            .any(|s| new_steps.iter().any(|p| p.node == s.node))
                        {
                            continue;
                        }
                        new_steps.extend_from_slice(suffix);
                        if let Some((path, topo_sig)) =
                            self.materialize_path(&new_steps, req, corner)
                        {
                            heap.push(HeapEntry {
                                slack_ns: path.slack_ns,
                                topo_sig,
                                launch_tag: path.launch_tag,
                                req_idx: curr.req_idx,
                                steps: new_steps,
                                branch_limit: new_branch_limit,
                                path,
                            });
                            expansions += 1;
                        }
                    }
                }
            }

            out.push((curr.path, curr.topo_sig));
        }

        out
    }

    /// Expands a sequential startpoint (`r/Q` or `l/Q`) into `[clock_pin (Clock), out_pin (Launch)]`
    /// (`STA-REQ-0228`).
    fn expand_sequential_launch(
        &self,
        out_node: NodeId,
        tag: usize,
        out_tr: Tr,
        an: An,
        out_slew: f64,
    ) -> Option<(PathPoint, PathPoint, f64)> {
        let c_idx = tag / 2;
        let src_edge = if tag.is_multiple_of(2) {
            Tr::Rise
        } else {
            Tr::Fall
        };
        let ck = self.schedule.clocks.get(c_idx)?;
        let frozen = self.netview.frozen();

        let mut best: Option<(f64, NodeId, Tr, u32)> = None;
        for (li, l) in self.graph.launches().iter().enumerate() {
            if l.out_pin != out_node || self.graph.is_launch_disabled(li) {
                continue;
            }
            let senses = &self.clock_sense[l.clock_pin.index()];
            let Some(&(_, bits)) = senses.iter().find(|&&(c, _)| c as usize == c_idx) else {
                continue;
            };
            if !source_edges(l.edge, bits).contains(&src_edge) {
                continue;
            }
            let cell_row = frozen.instances[l.inst as usize].cell as usize;
            let global_arc = *self.cell_arcs.get(cell_row)?.get(l.lib_arc.index())?;
            let net_cap = match self.netview.node_net(l.out_pin) {
                NetTarget::Net(nr) => self
                    .canon_totals
                    .get(nr.index())
                    .copied()
                    .unwrap_or_default(),
                NetTarget::Const(_) => [[0.0; 2]; 2],
            };
            let c_load = net_cap[out_tr.index()][an.index()];
            let ds = self.lib.arc_delay(
                global_arc,
                out_tr,
                an,
                ck.transition[an.index()],
                DriverLoad::Lumped { c: c_load },
            );
            let cand_arr = canon_zero(ck.latency[an.index()] + ds.delay);
            let replace = match best {
                None => true,
                Some((prev_arr, prev_ck, _, _)) => match an {
                    An::Early => {
                        cand_arr < prev_arr
                            || (cand_arr == prev_arr
                                && self.graph.topo_rank(l.clock_pin)
                                    < self.graph.topo_rank(prev_ck))
                    }
                    An::Late => {
                        cand_arr > prev_arr
                            || (cand_arr == prev_arr
                                && self.graph.topo_rank(l.clock_pin)
                                    < self.graph.topo_rank(prev_ck))
                    }
                },
            };
            if replace {
                best = Some((cand_arr, l.clock_pin, l.edge, l.inst));
            }
        }

        let (launch_arr, clock_pin, pin_edge, _inst) = best?;
        let ck_lat = canon_zero(ck.latency[an.index()]);
        let ck_slew = canon_zero(ck.transition[an.index()]);
        let (ck_inst, ck_cell) = self.node_inst_and_cell(clock_pin);
        let (q_inst, q_cell) = self.node_inst_and_cell(out_node);

        let ck_point = PathPoint {
            node: clock_pin,
            pin_name: self.netview.node_name_str(clock_pin).into_owned(),
            inst_name: ck_inst,
            cell_name: ck_cell,
            arc_kind: PathArcKind::Clock,
            transition: pin_edge,
            incr_ns: ck_lat,
            arrival_ns: ck_lat,
            slew_ns: ck_slew,
        };
        let launch_point = PathPoint {
            node: out_node,
            pin_name: self.netview.node_name_str(out_node).into_owned(),
            inst_name: q_inst,
            cell_name: q_cell,
            arc_kind: PathArcKind::Launch,
            transition: out_tr,
            incr_ns: canon_zero(launch_arr - ck_lat),
            arrival_ns: launch_arr,
            slew_ns: out_slew,
        };
        Some((ck_point, launch_point, launch_arr))
    }

    fn is_input_delay_port(&self, node: NodeId, tag: usize) -> bool {
        self.schedule
            .input_delays
            .iter()
            .any(|d| d.node == node && (d.clock as usize * 2 + d.edge.index()) == tag)
    }

    fn is_clock_source(&self, node: NodeId, tag: usize) -> bool {
        let c_idx = tag / 2;
        self.schedule
            .clocks
            .get(c_idx)
            .is_some_and(|ck| ck.sources.contains(&node))
    }

    fn node_inst_and_cell(&self, node: NodeId) -> (Option<String>, Option<String>) {
        match self.netview.node_kind(node) {
            NodeKind::PortBit { .. } => (None, None),
            NodeKind::InstancePin { inst, cell, .. } => {
                let frozen = self.netview.frozen();
                let inst_name = frozen
                    .string(frozen.instances[inst as usize].name)
                    .ok()
                    .map(str::to_owned);
                let cell_name = Some(self.netview.cell_name(cell).to_owned());
                (inst_name, cell_name)
            }
        }
    }

    fn tag_clock_and_edge(&self, tag: usize) -> (Option<String>, Option<Tr>) {
        let c_idx = tag / 2;
        let edge = if tag.is_multiple_of(2) {
            Tr::Rise
        } else {
            Tr::Fall
        };
        match self.schedule.clocks.get(c_idx) {
            Some(ck) => (Some(ck.name.clone()), Some(edge)),
            None => (None, None),
        }
    }
}
