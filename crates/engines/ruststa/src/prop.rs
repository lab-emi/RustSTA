// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Clock networks, launch tags and propagation of arrival and transition times (STA-REQ-0101,
//! STA-REQ-0110 to STA-REQ-0115, STA-REQ-0200, STA-REQ-0201).

use crate::constraints::Constraints;
use crate::graph::{ArcRef, Graph, NodeKind};
use crate::model::{ArcKind, Model, Sense, Tr};
use rusteda_core::job::{Cancelled, Ctx};

/// Analysis index: early (minimum) and late (maximum).
pub const EARLY: usize = 0;
pub const LATE: usize = 1;

/// Clock senses reaching a node: bit 0 positive, bit 1 negative.
pub type SenseBits = u8;

/// Per-node values of every tag, transition and analysis.
#[derive(Debug, Clone)]
pub struct Values {
    pub ntags: usize,
    pub arr: Vec<f64>,
    pub slew: Vec<f64>,
}

impl Values {
    pub fn new(nodes: usize, ntags: usize) -> Values {
        let n = nodes * ntags * 4;
        let mut arr = vec![0.0; n];
        let mut slew = vec![0.0; n];
        for i in 0..n {
            let (a, s) = if i % 2 == EARLY {
                (f64::INFINITY, f64::INFINITY)
            } else {
                (f64::NEG_INFINITY, f64::NEG_INFINITY)
            };
            arr[i] = a;
            slew[i] = s;
        }
        Values { ntags, arr, slew }
    }

    #[inline]
    pub fn idx(&self, v: u32, tag: usize, tr: Tr, an: usize) -> usize {
        ((v as usize * self.ntags + tag) * 2 + tr) * 2 + an
    }

    /// True when (tag, transition) reaches the node.
    pub fn present(&self, v: u32, tag: usize, tr: Tr) -> bool {
        self.arr[self.idx(v, tag, tr, LATE)] > f64::NEG_INFINITY
    }

    /// Merges an arrival and transition into the node (STA-REQ-0115).
    #[inline]
    pub fn merge(&mut self, v: u32, tag: usize, tr: Tr, an: usize, arr: f64, slew: f64) {
        let i = self.idx(v, tag, tr, an);
        if an == LATE {
            self.arr[i] = self.arr[i].max(arr);
            self.slew[i] = self.slew[i].max(slew);
        } else {
            self.arr[i] = self.arr[i].min(arr);
            self.slew[i] = self.slew[i].min(slew);
        }
    }
}

/// Everything propagation needs besides the graph and model.
#[derive(Debug, Clone)]
pub struct Setup {
    pub ntags: usize,
    /// Load of each net per driver transition and analysis: `[tr][an]`.
    pub net_load: Vec<[[f64; 2]; 2]>,
    /// Clock senses per node: (clock, bits).
    pub clock_sense: Vec<Vec<(u32, SenseBits)>>,
    /// Startpoint values before propagation.
    pub init: Values,
    /// Nodes that received startpoint values.
    pub is_start: Vec<bool>,
    /// Latest usable late arrival at the source of each arc per tag (latch closing edges,
    /// STA-REQ-0213); infinite for other arcs. Indexed `arc * ntags + tag`.
    pub clip: Vec<f64>,
    /// Earliest usable early arrival at the source of each arc per tag (latch opening edges);
    /// minus infinity for other arcs.
    pub clip_early: Vec<f64>,
    /// For latch data arcs: the setup tables (data rise, fall) and the clock transition that
    /// reduce `clip` by the setup time.
    pub clip_setup: Vec<([crate::model::TableId; 2], f64)>,
}

impl Setup {
    /// Latest usable late arrival entering arc `ai` for a tag and data transition, given the
    /// source's late transition time (STA-REQ-0213).
    #[inline]
    pub fn late_clip(&self, m: &Model, ai: u32, t: usize, ti: Tr, s_in: f64) -> f64 {
        let c = self.clip[ai as usize * self.ntags + t];
        if c == f64::INFINITY {
            return c;
        }
        let (tabs, ctr) = self.clip_setup[ai as usize];
        c - m.eval(tabs[ti], ctr, s_in)
    }
}

/// Tag of a clock and source edge.
pub fn tag(clock: u32, edge: usize) -> usize {
    clock as usize * 2 + edge
}

/// Source edge of a clock that produces transition `pin_edge` at a node reached with `sense`
/// bits; both source edges for a node reached with both senses.
pub fn source_edges(pin_edge: Tr, bits: SenseBits) -> Vec<usize> {
    let mut out = Vec::new();
    if bits & 1 != 0 {
        out.push(pin_edge);
    }
    if bits & 2 != 0 {
        out.push(1 - pin_edge);
    }
    out
}

/// Output transitions of a cell arc for an input transition (STA-REQ-0112).
pub fn out_transitions(kind: ArcKind, sense: Sense, input: Tr) -> [bool; 2] {
    let ArcKind::Comb { outs } = kind else {
        return [false, false];
    };
    let mut o = match sense {
        Sense::Positive => [input == 0, input == 1],
        Sense::Negative => [input == 1, input == 0],
        Sense::Non => [true, true],
    };
    o[0] &= outs[0];
    o[1] &= outs[1];
    o
}

impl Setup {
    pub fn build(g: &Graph, m: &Model, k: &Constraints) -> Result<Setup, String> {
        let n = g.nodes.len();
        let ntags = 2 * k.clocks.len().max(1);
        // Net loads (STA-REQ-0110).
        let mut net_load = vec![[[0.0f64; 2]; 2]; g.net_loads.len()];
        for (net, loads) in g.net_loads.iter().enumerate() {
            let mut l = [[0.0; 2]; 2];
            let add_port = |p: [f64; 2], l: &mut [[f64; 2]; 2]| {
                for row in l.iter_mut() {
                    row[EARLY] += p[EARLY];
                    row[LATE] += p[LATE];
                }
            };
            for &v in loads {
                let c = g.node_cap[v as usize];
                for (tr, row) in l.iter_mut().enumerate() {
                    row[EARLY] += c[tr][EARLY];
                    row[LATE] += c[tr][LATE];
                }
                if let Some(p) = k.port_load.get(&v) {
                    add_port(*p, &mut l);
                }
            }
            if let Some(d) = g.net_driver[net] {
                // A load set on an input port also loads its net.
                if let Some(p) = k.port_load.get(&d) {
                    add_port(*p, &mut l);
                }
            }
            net_load[net] = l;
        }
        // Clock networks (STA-REQ-0200).
        let mut out: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (ai, a) in g.arcs.iter().enumerate() {
            out[a.from as usize].push(ai as u32);
        }
        let mut clock_sense: Vec<Vec<(u32, SenseBits)>> = vec![Vec::new(); n];
        for (ci, ck) in k.clocks.iter().enumerate() {
            let mut bits = vec![0u8; n];
            let mut stack: Vec<(u32, u8)> = Vec::new();
            for &s in &ck.sources {
                bits[s as usize] |= 1;
                stack.push((s, 1));
            }
            while let Some((v, b)) = stack.pop() {
                for &ai in &out[v as usize] {
                    let a = g.arcs[ai as usize];
                    let nb = match a.kind {
                        ArcRef::Net => b,
                        ArcRef::Cell { inst, arc } => {
                            let ca =
                                &m.cells[g.inst_cell[inst as usize] as usize].arcs[arc as usize];
                            if ca.latch_check.is_some() {
                                continue;
                            }
                            match ca.sense {
                                Sense::Positive => b,
                                Sense::Negative => ((b & 1) << 1) | ((b >> 1) & 1),
                                Sense::Non => 3,
                            }
                        }
                    };
                    let w = a.to as usize;
                    if bits[w] | nb != bits[w] {
                        bits[w] |= nb;
                        stack.push((a.to, bits[w]));
                    }
                }
            }
            for (v, b) in bits.iter().enumerate() {
                if *b != 0 {
                    clock_sense[v].push((ci as u32, *b));
                }
            }
        }
        // Startpoints (STA-REQ-0201).
        let mut init = Values::new(n, ntags);
        let mut is_start = vec![false; n];
        // Clock used as data: each source edge starts the matching transition.
        for (ci, ck) in k.clocks.iter().enumerate() {
            for &src in &ck.sources {
                for e in 0..2 {
                    for an in [EARLY, LATE] {
                        init.merge(src, tag(ci as u32, e), e, an, ck.latency[an], ck.transition);
                    }
                }
                is_start[src as usize] = true;
            }
        }
        for d in &k.input_delays {
            let t = tag(d.clock, d.edge);
            let tr = k
                .input_transition
                .get(&d.node)
                .copied()
                .unwrap_or([0.0, 0.0]);
            for x in 0..2 {
                init.merge(d.node, t, x, EARLY, d.value[EARLY], tr[EARLY]);
                init.merge(d.node, t, x, LATE, d.value[LATE], tr[LATE]);
            }
            is_start[d.node as usize] = true;
        }
        for l in &g.launches {
            let cell = &m.cells[g.inst_cell[l.inst as usize] as usize];
            let arc = &cell.arcs[l.arc as usize];
            let ArcKind::Edge(pin_edge) = arc.kind else {
                continue;
            };
            let net = g.nodes[l.out as usize].net as usize;
            for &(c, b) in &clock_sense[l.clock_pin as usize] {
                let ck = &k.clocks[c as usize];
                for e in source_edges(pin_edge, b) {
                    let t = tag(c, e);
                    for an in [EARLY, LATE] {
                        for o in 0..2 {
                            let load = net_load.get(net).map_or(0.0, |x| x[o][an]);
                            let d = m.eval(arc.delay[o], ck.transition, load);
                            let s = m.eval(arc.slew[o], ck.transition, load);
                            init.merge(l.out, t, o, an, ck.latency[an] + d, s);
                        }
                    }
                    is_start[l.out as usize] = true;
                }
            }
        }
        // Latch closing edges (STA-REQ-0213).
        let check_of: rustc_hash::FxHashMap<(u32, u32), &crate::graph::Check> =
            g.checks.iter().map(|c| ((c.inst, c.check), c)).collect();
        let mut clip = vec![f64::INFINITY; g.arcs.len() * ntags];
        let mut clip_early = vec![f64::NEG_INFINITY; g.arcs.len() * ntags];
        let mut clip_setup = vec![([crate::model::NO_TABLE; 2], 0.0); g.arcs.len()];
        for (ai, a) in g.arcs.iter().enumerate() {
            let ArcRef::Cell { inst, arc } = a.kind else {
                continue;
            };
            let cell = &m.cells[g.inst_cell[inst as usize] as usize];
            let Some(ci) = cell.arcs[arc as usize].latch_check else {
                continue;
            };
            let check = &cell.checks[ci];
            // The instance's node of the check's clock pin.
            let gc = check_of.get(&(inst, ci as u32)).copied();
            clip_setup[ai] = (
                check.table,
                gc.and_then(|gc| clock_sense[gc.clock as usize].first())
                    .map_or(0.0, |(c, _)| k.clocks[*c as usize].transition),
            );
            // Opening transition at the enable pin: the edge arc from the check's clock pin.
            let open_pin = cell.arcs.iter().find_map(|e| match e.kind {
                ArcKind::Edge(tr) if e.from == check.clock => Some(tr),
                _ => None,
            });
            let Some(gc) = gc else {
                continue;
            };
            for t in 0..ntags {
                let (l, el) = ((t / 2) as u32, t % 2);
                if l as usize >= k.clocks.len() {
                    continue;
                }
                let mut best = f64::NEG_INFINITY;
                let mut open = f64::INFINITY;
                for &(c, bits) in &clock_sense[gc.clock as usize] {
                    for ec in source_edges(check.edge, bits) {
                        let ds = crate::checks::setup_relation(k, l, el, c, ec)?;
                        best = best.max(ds + k.clocks[c as usize].latency[LATE]);
                    }
                    if let Some(op) = open_pin {
                        for eo in source_edges(op, bits) {
                            let o = crate::checks::edge_relation(k, l, el, c, eo, false)?;
                            open = open.min(o + k.clocks[c as usize].latency[EARLY]);
                        }
                    }
                }
                if best > f64::NEG_INFINITY {
                    clip[ai * ntags + t] = best;
                }
                if open < f64::INFINITY {
                    clip_early[ai * ntags + t] = open;
                }
            }
        }
        Ok(Setup {
            ntags,
            net_load,
            clock_sense,
            init,
            is_start,
            clip,
            clip_early,
            clip_setup,
        })
    }
}

/// Delay and output transition of propagation arc `ai` into node `v` for input transition `ti`
/// and output transition `to`, from the source transition `s_in` (STA-REQ-0113).
#[inline]
pub fn arc_eval(
    g: &Graph,
    m: &Model,
    s: &Setup,
    ai: u32,
    v: u32,
    to: Tr,
    an: usize,
    s_in: f64,
) -> (f64, f64) {
    match g.arcs[ai as usize].kind {
        ArcRef::Net => (0.0, s_in),
        ArcRef::Cell { inst, arc } => {
            let ca = &m.cells[g.inst_cell[inst as usize] as usize].arcs[arc as usize];
            let net = g.nodes[v as usize].net as usize;
            let load = s.net_load.get(net).map_or(0.0, |x| x[to][an]);
            (
                m.eval(ca.delay[to], s_in, load),
                m.eval(ca.slew[to], s_in, load),
            )
        }
    }
}

/// Output transitions reached from input transition `ti` over arc `ai`.
#[inline]
pub fn arc_outs(g: &Graph, m: &Model, ai: u32, ti: Tr) -> [bool; 2] {
    match g.arcs[ai as usize].kind {
        ArcRef::Net => [ti == 0, ti == 1],
        ArcRef::Cell { inst, arc } => {
            let ca = &m.cells[g.inst_cell[inst as usize] as usize].arcs[arc as usize];
            out_transitions(ca.kind, ca.sense, ti)
        }
    }
}

/// Values of the levels below the one being propagated, in level order (`pos` maps a node to its
/// level-order position; `w` values per node).
struct Lower<'a> {
    arr: &'a [f64],
    slew: &'a [f64],
    pos: &'a [u32],
    w: usize,
}

/// Reduces the incoming arcs of node `v` in CSR order into `arr` and `slew`, which hold the
/// node's `ntags * 4` values (initially its startpoint values). Reads only values of the arcs'
/// source nodes, which lie on lower levels.
#[inline]
fn reduce_node(
    g: &Graph,
    m: &Model,
    s: &Setup,
    low: &Lower,
    v: u32,
    arr: &mut [f64],
    slew: &mut [f64],
) {
    if matches!(g.nodes[v as usize].kind, NodeKind::PortIn(_)) {
        return;
    }
    for &ai in g.incoming(v) {
        let u = g.arcs[ai as usize].from;
        let ub = low.pos[u as usize] as usize * low.w;
        for t in 0..s.ntags {
            for ti in 0..2 {
                if low.arr[ub + (t * 2 + ti) * 2 + LATE] == f64::NEG_INFINITY {
                    continue;
                }
                let outs = arc_outs(g, m, ai, ti);
                for to in 0..2 {
                    if !outs[to] {
                        continue;
                    }
                    for an in [EARLY, LATE] {
                        let iu = ub + (t * 2 + ti) * 2 + an;
                        let (mut a_in, s_in) = (low.arr[iu], low.slew[iu]);
                        if an == LATE {
                            a_in = a_in.min(s.late_clip(m, ai, t, ti, s_in));
                        } else if a_in == f64::INFINITY
                            || a_in < s.clip_early[ai as usize * s.ntags + t]
                        {
                            // Absent early value (the late one passed a latch the early one
                            // did not), or early data before the latch opens.
                            continue;
                        }
                        let (d, so) = arc_eval(g, m, s, ai, v, to, an, s_in);
                        let i = (t * 2 + to) * 2 + an;
                        if an == LATE {
                            arr[i] = arr[i].max(a_in + d);
                            slew[i] = slew[i].max(so);
                        } else {
                            arr[i] = arr[i].min(a_in + d);
                            slew[i] = slew[i].min(so);
                        }
                    }
                }
            }
        }
    }
}

/// Propagation in level order: values are kept in level-order position, so the nodes of a level
/// are contiguous and only read the positions before them. With `parallel` the nodes of a level
/// run on the threads of the current rayon pool; every node is reduced by the same code in the
/// same arc order, so the result does not depend on the number of threads.
fn propagate_levels(
    g: &Graph,
    m: &Model,
    s: &Setup,
    parallel: bool,
    ctx: Ctx<'_>,
) -> Result<Values, Cancelled> {
    use rayon::prelude::*;
    let n = g.nodes.len();
    let w = s.ntags * 4;
    let mut pos = vec![0u32; n];
    for (p, &v) in g.order.iter().enumerate() {
        pos[v as usize] = p as u32;
    }
    let mut arr = vec![0.0f64; n * w];
    let mut slew = vec![0.0f64; n * w];
    let gather = |(a, sl): (&mut [f64], &mut [f64]), v: u32| {
        let b = v as usize * w;
        a.copy_from_slice(&s.init.arr[b..b + w]);
        sl.copy_from_slice(&s.init.slew[b..b + w]);
    };
    if parallel {
        arr.par_chunks_mut(w)
            .zip(slew.par_chunks_mut(w))
            .zip(g.order.par_iter())
            .for_each(|(x, &v)| gather(x, v));
    } else {
        for (x, &v) in arr
            .chunks_mut(w)
            .zip(slew.chunks_mut(w))
            .zip(g.order.iter())
        {
            gather(x, v);
        }
    }
    let levels = g.level_start.len().saturating_sub(1) as u64;
    for (li, lv) in g.level_start.windows(2).enumerate() {
        ctx.checkpoint()?;
        let (b, e) = (lv[0] as usize * w, lv[1] as usize * w);
        let (alo, ahi) = arr.split_at_mut(b);
        let (slo, shi) = slew.split_at_mut(b);
        let low = Lower {
            arr: alo,
            slew: slo,
            pos: &pos,
            w,
        };
        let nodes = &g.order[lv[0] as usize..lv[1] as usize];
        let (ca, cs) = (&mut ahi[..e - b], &mut shi[..e - b]);
        if parallel && nodes.len() >= 64 {
            ca.par_chunks_mut(w)
                .zip(cs.par_chunks_mut(w))
                .zip(nodes.par_iter())
                .with_min_len(32)
                .for_each(|((a, sl), &v)| reduce_node(g, m, s, &low, v, a, sl));
        } else {
            for ((a, sl), &v) in ca.chunks_mut(w).zip(cs.chunks_mut(w)).zip(nodes.iter()) {
                reduce_node(g, m, s, &low, v, a, sl);
            }
        }
        ctx.progress("propagate", li as u64 + 1, levels);
    }
    let mut val = Values {
        ntags: s.ntags,
        arr: vec![0.0; n * w],
        slew: vec![0.0; n * w],
    };
    let scatter = |(a, sl): (&mut [f64], &mut [f64]), v: usize| {
        let b = pos[v] as usize * w;
        a.copy_from_slice(&arr[b..b + w]);
        sl.copy_from_slice(&slew[b..b + w]);
    };
    if parallel {
        val.arr
            .par_chunks_mut(w)
            .zip(val.slew.par_chunks_mut(w))
            .enumerate()
            .for_each(|(v, x)| scatter(x, v));
    } else {
        for (v, x) in val
            .arr
            .chunks_mut(w)
            .zip(val.slew.chunks_mut(w))
            .enumerate()
        {
            scatter(x, v);
        }
    }
    Ok(val)
}

/// Scalar reference propagation in `f64` (STA-REQ-0400) on one thread.
pub fn propagate_cpu(g: &Graph, m: &Model, s: &Setup) -> Values {
    propagate_levels(g, m, s, false, Ctx::NONE).unwrap_or_else(|_| unreachable!("no cancel token"))
}

/// Level-parallel propagation on the threads of the current rayon pool (STA-REQ-0405),
/// bit-identical to [`propagate_cpu`] for any number of threads.
pub fn propagate_cpu_parallel(g: &Graph, m: &Model, s: &Setup) -> Values {
    propagate_levels(g, m, s, true, Ctx::NONE).unwrap_or_else(|_| unreachable!("no cancel token"))
}

/// [`propagate_cpu_parallel`] with a run context: a checkpoint and level progress per level
/// (STA-REQ-0310).
pub fn propagate_cpu_parallel_with(
    g: &Graph,
    m: &Model,
    s: &Setup,
    ctx: Ctx<'_>,
) -> Result<Values, Cancelled> {
    propagate_levels(g, m, s, true, ctx)
}
