// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Lowering onto the compute timing task (owner: C2; `STA-REQ-0400` to `STA-REQ-0408`,
//! `COMPUTE-REQ-0010` to `COMPUTE-REQ-0029`).
//!
//! Every propagation of the engine runs through the timing task of `rusteda-compute`, on its CPU
//! backend in `f64` or on a GPU backend (`cuda` in `f64` or `f32`, or `gpu`). The lowering turns
//! the [`TimingGraph`], the corner's [`TimingLib`], the per-net [`NetLoads`], the start rows and
//! the tag maps into the task's [`LevelizedTimingCsr`], [`CompiledLutBank`], [`TimingProblem`],
//! [`TimingStateSoA`] and [`TagMapRec`] records, and reads back arrivals, transitions and
//! predecessors.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

mod rc;
mod slew_only;
mod start_builder;

pub use start_builder::GraphStartBuilder;

pub(crate) use rc::arc_value;

pub use rusteda_compute::timing::{
    ArcValues, CompiledLutBank, InArc, LevelizedTimingCsr, NO_PRED, NONE, PropagationStats,
    Readback, Request, Scope, StartRows, TagMapRec, TimingProblem, TimingPropagationBackend,
    TimingSession, TimingStateSoA, TimingValues, WireRec, absent, arr_index, out_adjacency,
    pred_source, prewarm, slew_index, timing_backend,
};
pub use rusteda_compute::{BackendKind, ComputeError, Precision};
use rusteda_core::job::{Ctx, Diag};
use rusteda_core::liberty::Direction;

use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::{DriverLoad, WireModel};
use crate::engine::graph::{ArcKind, ArcSense, GraphArc, TimingGraph};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::types::{An, LibArc, NetRow, NodeId, Tr};

/// Canonicalizes `-0.0` to `+0.0` (`COMPUTE-REQ-0016`).
#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}

/// Maps a [`ComputeError`] from `rusteda-compute` to its contract diagnostic (`A0501`–`A0506`,
/// `STA-REQ-0404`, `COMPUTE-REQ-0025`).
#[must_use]
pub fn compute_error_diag(err: &ComputeError) -> Diag {
    Diag::error(compute_error_code(err), err.to_string())
}

/// Returns the `A05xx` diagnostic code for `err` (`COMPUTE-REQ-0025`).
#[must_use]
pub const fn compute_error_code(err: &ComputeError) -> &'static str {
    match err {
        ComputeError::NotCompiled(_) | ComputeError::Unavailable(_) => "A0501",
        ComputeError::TooLarge(_) => "A0502",
        ComputeError::Compile(_) => "A0503",
        ComputeError::Device(_) => "A0504",
        ComputeError::Invalid(_) => "A0505",
        ComputeError::Cancelled => "A0506",
        _ => "A0504",
    }
}

/// True when `err` represents an unavailable or uncompiled backend (`BACKEND_UNAVAILABLE`, exit 1;
/// `STA-REQ-0404`, `COMPUTE-REQ-0025`).
#[must_use]
pub const fn is_backend_unavailable(err: &ComputeError) -> bool {
    matches!(
        err,
        ComputeError::NotCompiled(_) | ComputeError::Unavailable(_)
    )
}

/// Selects and prepares the requested backend and precision (`STA-REQ-0408`, `COMPUTE-REQ-0029`).
pub fn prewarm_backend(kind: BackendKind, precision: Precision) -> Result<(), ComputeError> {
    prewarm(kind, precision)
}

/// Runs `work` on the calling thread while overlapping GPU runtime preparation and NVRTC kernel
/// compilation for `(kind, precision)` on a background thread when `kind != BackendKind::Cpu`
/// (`STA-REQ-0408`, `COMPUTE-REQ-0003`, `COMPUTE-REQ-0029`).
pub fn with_prewarm<T>(
    kind: BackendKind,
    precision: Precision,
    work: impl FnOnce() -> T,
) -> (T, Result<(), ComputeError>) {
    if kind == BackendKind::Cpu {
        let out = work();
        let prep = prewarm(kind, precision);
        return (out, prep);
    }
    std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .name("ruststa-compute-prewarm".into())
            .spawn_scoped(s, move || prewarm(kind, precision))
            .ok();
        let out = work();
        let prep = match handle {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err(ComputeError::Device("prewarm thread panicked".into()))),
            None => prewarm(kind, precision),
        };
        (out, prep)
    })
}

/// Collection of tag-map record slices keyed by [`TimingGraph`] arc index (`0..graph.num_arcs()`,
/// `COMPUTE-REQ-0012`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArcTagMaps {
    by_arc: BTreeMap<u32, Vec<TagMapRec>>,
}

impl ArcTagMaps {
    /// Creates an empty tag-map collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Associates `records` with `TimingGraph` arc index `arc_idx`.
    pub fn insert(&mut self, arc_idx: u32, records: Vec<TagMapRec>) {
        if !records.is_empty() {
            self.by_arc.insert(arc_idx, records);
        }
    }

    /// Appends a single [`TagMapRec`] to `TimingGraph` arc index `arc_idx`.
    pub fn push(&mut self, arc_idx: u32, rec: TagMapRec) {
        self.by_arc.entry(arc_idx).or_default().push(rec);
    }

    /// Returns the tag-map records for `TimingGraph` arc index `arc_idx`, if any.
    #[must_use]
    pub fn get(&self, arc_idx: u32) -> Option<&[TagMapRec]> {
        self.by_arc.get(&arc_idx).map(Vec::as_slice)
    }

    /// Number of graph arcs that carry a tag map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_arc.len()
    }

    /// True when no graph arc carries a tag map.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_arc.is_empty()
    }
}

/// Lowered timing problem of one corner, ready for `rusteda_compute::timing` (`STA-REQ-0401`,
/// `COMPUTE-REQ-0011` to `COMPUTE-REQ-0015`).
#[derive(Debug, Clone)]
pub struct LoweredCorner<'a> {
    /// Level-ordered CSR graph and tag-map tables.
    pub csr: LevelizedTimingCsr<'static>,
    /// Maps each CSR incoming arc index (`0..csr.arcs()`) back to its [`TimingGraph`] arc index;
    /// an arc that carries only the transition of a launch arc `li` disabled by logic values maps
    /// to `graph.arcs().len() + li` ([`Self::csr_raw_arc`], `STA-REQ-0116`).
    pub csr_arc_to_graph_arc: Vec<u32>,
    /// Maps each [`TimingGraph`] arc index (`0..graph.num_arcs()`) to its CSR incoming arc index,
    /// or [`NONE`] when the arc is disabled.
    pub graph_arc_to_csr_arc: Vec<u32>,
    /// Corner lookup-table bank and [`ArcClass`](rusteda_compute::timing::ArcClass) descriptors.
    pub luts: CompiledLutBank<'a>,
    /// Number of propagation tags `T >= 1`.
    pub ntags: u32,
    /// Per level-ordered node capacitive loads `[Rise/Early, Rise/Late, Fall/Early, Fall/Late]` in pF.
    pub load: Vec<[f64; 4]>,
    /// Startpoint rows in level-ordered node coordinates.
    pub starts: StartRows,
    /// Optional RC wire records per CSR incoming arc (empty for ideal/lumped wire models).
    pub wires: Vec<WireRec>,
    /// CPU-prepared f64 RC arc values, shared by all propagation backends.
    pub arc_values: Vec<ArcValues>,
    /// The tag map of the incoming arcs that carry only their output transition (an empty record
    /// range, so that no arrival passes them), or [`NONE`] without any such arc: the arcs and
    /// launches that logic values disable, and the library transitions that a sense refinement
    /// removed from an enabled arc (`STA-REQ-0116`).
    pub slew_only_map: u32,
    /// Wall-clock seconds spent lowering into the compute ABI.
    pub lower_s: f64,
}

impl<'a> LoweredCorner<'a> {
    /// Lowers `netview`, `graph`, `lib`, `net_loads`, `ntags`, `starts` and `tag_maps` into the
    /// `rusteda_compute::timing` ABI (`STA-REQ-0401`).
    pub fn lower(
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &'a TimingLib,
        net_loads: &NetLoads,
        ntags: u32,
        starts: StartRows,
        tag_maps: &ArcTagMaps,
    ) -> Result<Self, Diag> {
        let t0 = Instant::now();
        let n_nodes = graph.num_nodes();
        let ntags = ntags.max(1);

        // Map each netlist cell type `0..netview.num_cells()` to its `TimingCell::arcs` slice.
        let cell_arcs = map_cell_arcs(netview, lib)?;

        // Compute canonical net loads (accounting for any continuous-assign net aliases).
        let canon_totals = canonical_net_totals(netview, net_loads);

        // Build per-level-ordered-node loads `load[pos]` and incoming CSR `in_ptr`/`in_arcs`.
        let g_arcs = graph.arcs();
        let mut load = Vec::with_capacity(n_nodes);
        let mut in_ptr = Vec::with_capacity(n_nodes + 1);
        let mut in_arcs = Vec::with_capacity(g_arcs.len());
        let mut csr_arc_to_graph_arc = Vec::with_capacity(g_arcs.len());
        let mut graph_arc_to_csr_arc = vec![NONE; g_arcs.len()];
        let mut map_ptr = vec![0u32];
        let mut maps = Vec::new();

        let mut luts = lib.lut_bank.as_compiled();
        let order = graph.order();

        let class_for = |luts: &mut CompiledLutBank<'_>,
                         inst: u32,
                         local_arc: LibArc,
                         sense: Option<ArcSense>| {
            slew_only::class_for(netview, lib, &cell_arcs, luts, inst, local_arc, sense)
        };
        let mut slew_only = slew_only::slew_only_arcs(netview, graph, lib, &cell_arcs, &mut luts);
        slew_only.sort_unstable();
        let slew_only_map = if slew_only.is_empty() {
            NONE
        } else {
            map_ptr.push(maps.len() as u32);
            0
        };
        let mut next_slew_only = 0usize;

        for (pos, &raw_node) in order.iter().enumerate() {
            let node_load = match netview.node_net(raw_node) {
                NetTarget::Net(net_row) => canon_totals
                    .get(net_row.index())
                    .map_or([0.0; 4], |t| [t[0][0], t[0][1], t[1][0], t[1][1]]),
                NetTarget::Const(_) => [0.0; 4],
            };
            load.push(node_load);

            in_ptr.push(in_arcs.len() as u32);
            // The arcs that carry only a transition into this node, merged by source rank after
            // the enabled arcs of the same source (the CSR lists sources in rank order).
            let extra_end = next_slew_only
                + slew_only[next_slew_only..]
                    .iter()
                    .take_while(|e| e.0 == pos as u32)
                    .count();
            let mut extras = slew_only[next_slew_only..extra_end].iter().peekable();
            next_slew_only = extra_end;
            for &ai in graph.incoming(raw_node) {
                let g_arc = &g_arcs[ai as usize];
                let src = graph.topo_rank(g_arc.src);
                while let Some(&&(_, e_src, e_ref, e_class)) = extras.peek()
                    && e_src < src
                {
                    in_arcs.push(InArc {
                        src: e_src,
                        class: e_class,
                        map: slew_only_map,
                    });
                    csr_arc_to_graph_arc.push(e_ref);
                    extras.next();
                }
                let class = match g_arc.kind {
                    ArcKind::Net { .. } => lib.wire_class,
                    _ => {
                        let (inst, local_arc) = g_arc
                            .kind
                            .inst_and_lib_arc()
                            .expect("non-net arc has inst and lib_arc");
                        class_for(&mut luts, inst, local_arc, Some(g_arc.sense))
                    }
                };
                let map = if let Some(recs) = tag_maps.get(ai)
                    && !recs.is_empty()
                {
                    let m = (map_ptr.len() - 1) as u32;
                    maps.extend_from_slice(recs);
                    map_ptr.push(maps.len() as u32);
                    m
                } else {
                    NONE
                };
                let csr_idx = in_arcs.len() as u32;
                in_arcs.push(InArc { src, class, map });
                csr_arc_to_graph_arc.push(ai);
                graph_arc_to_csr_arc[ai as usize] = csr_idx;
            }
            for &(_, e_src, e_ref, e_class) in extras {
                in_arcs.push(InArc {
                    src: e_src,
                    class: e_class,
                    map: slew_only_map,
                });
                csr_arc_to_graph_arc.push(e_ref);
            }
        }
        in_ptr.push(in_arcs.len() as u32);

        let (out_ptr, out_dst) = out_adjacency(n_nodes, &in_ptr, &in_arcs);
        let level_ptr = if n_nodes == 0 {
            vec![0]
        } else {
            graph.level_start().to_vec()
        };

        let csr = LevelizedTimingCsr {
            level_ptr: Cow::Owned(level_ptr),
            in_ptr: Cow::Owned(in_ptr),
            in_arcs: Cow::Owned(in_arcs),
            out_ptr: Cow::Owned(out_ptr),
            out_dst: Cow::Owned(out_dst),
            map_ptr: Cow::Owned(map_ptr),
            maps: Cow::Owned(maps),
        };
        let lower_s = t0.elapsed().as_secs_f64();

        Ok(Self {
            csr,
            csr_arc_to_graph_arc,
            graph_arc_to_csr_arc,
            luts,
            ntags,
            load,
            starts,
            wires: Vec::new(),
            arc_values: Vec::new(),
            slew_only_map,
            lower_s,
        })
    }

    /// True when CSR incoming arc `k` carries only its output transition (`STA-REQ-0116`).
    #[inline]
    #[must_use]
    pub fn is_slew_only(&self, k: usize) -> bool {
        self.slew_only_map != NONE && self.csr.in_arcs[k].map == self.slew_only_map
    }

    /// The timing graph arc of CSR incoming arc `k`; for an arc carrying the transition of a
    /// launch arc disabled by logic values, the clock-to-output arc as a cell arc
    /// (`STA-REQ-0116`).
    #[must_use]
    pub fn csr_raw_arc(&self, graph: &TimingGraph, k: usize) -> GraphArc {
        let r = self.csr_arc_to_graph_arc[k] as usize;
        match graph.arcs().get(r) {
            Some(a) => *a,
            None => {
                let l = graph.launches()[r - graph.arcs().len()];
                GraphArc {
                    src: l.clock_pin,
                    dst: l.out_pin,
                    sense: l.sense,
                    kind: ArcKind::Comb {
                        inst: l.inst,
                        lib_arc: l.lib_arc,
                        outs: [true, true],
                    },
                }
            }
        }
    }

    /// Prepare bounded RC waveform values after the start transitions are final.
    pub fn set_wire_model(
        &mut self,
        graph: &TimingGraph,
        view: &NetlistView<'_>,
        wire: &dyn WireModel,
        lib: &TimingLib,
        max_bytes: u64,
    ) -> Result<(), Diag> {
        rc::prepare(self, graph, view, wire, lib, max_bytes)
    }

    /// Borrows this lowered corner as a [`TimingProblem`] for `rusteda_compute::timing`.
    #[inline]
    #[must_use]
    pub fn problem(&self) -> TimingProblem<'_> {
        TimingProblem {
            graph: &self.csr,
            luts: &self.luts,
            ntags: self.ntags,
            load: &self.load,
            starts: &self.starts,
            wires: &self.wires,
            arc_values: &self.arc_values,
        }
    }

    /// Builds a [`TimingStateSoA`] initialized with this corner's `ntags`, `load` and `starts`
    /// (`COMPUTE-REQ-0015`, `COMPUTE-REQ-0023`).
    #[must_use]
    pub fn state_soa(&self) -> TimingStateSoA {
        TimingStateSoA::new(self.ntags, self.load.clone(), self.starts.clone())
    }

    /// Opens a resident [`TimingSession`] on `(kind, precision)` (`COMPUTE-REQ-0021`).
    pub fn open_session<'p>(
        &'p self,
        kind: BackendKind,
        precision: Precision,
    ) -> Result<Box<dyn TimingSession + 'p>, ComputeError> {
        let backend = timing_backend(kind, precision)?;
        let problem = self.problem();
        backend.open(&problem)
    }

    /// Runs a full deterministic propagation (`Scope::All`, `Readback::All`) on `(kind, precision)`
    /// with job context `ctx` (`STA-REQ-0400` to `STA-REQ-0407`, `COMPUTE-REQ-0017` to
    /// `COMPUTE-REQ-0028`).
    pub fn propagate(
        &self,
        kind: BackendKind,
        precision: Precision,
        ctx: Ctx<'_>,
    ) -> Result<LoweredResult, ComputeError> {
        let backend = timing_backend(kind, precision)?;
        self.propagate_with_backend(backend, ctx)
    }

    /// Runs a full deterministic propagation (`Scope::All`, `Readback::All`) on `backend` with
    /// job context `ctx`.
    pub fn propagate_with_backend(
        &self,
        backend: &dyn TimingPropagationBackend,
        ctx: Ctx<'_>,
    ) -> Result<LoweredResult, ComputeError> {
        let problem = self.problem();
        let mut session = backend.open(&problem)?;
        let req = Request::all().with_ctx(ctx);
        let stats = session.propagate(&req)?;
        let values = session.into_values();
        Ok(LoweredResult {
            values,
            stats,
            lower_s: self.lower_s,
        })
    }

    /// Runs the single-call [`TimingPropagationBackend::propagate_levels`] method (`COMPUTE-REQ-0023`).
    pub fn propagate_levels(
        &self,
        backend: &dyn TimingPropagationBackend,
        deterministic: bool,
    ) -> Result<LoweredResult, ComputeError> {
        if !self.arc_values.is_empty() {
            return Err(ComputeError::Invalid(
                "RC arc values require the session API".into(),
            ));
        }
        let mut state = self.state_soa();
        let stats = backend.propagate_levels(&self.csr, &self.luts, &mut state, deterministic)?;
        Ok(LoweredResult {
            values: state.values(),
            stats,
            lower_s: self.lower_s,
        })
    }
}

/// Output of a lowered propagation: level-ordered [`TimingValues`], [`PropagationStats`], and
/// lowering time (`STA-REQ-0401`, `COMPUTE-REQ-0016`, `COMPUTE-REQ-0024`).
#[derive(Debug, Clone)]
pub struct LoweredResult {
    /// Propagated arrivals, per-pin transitions and predecessors indexed by level-ordered node
    /// position `pos = graph.topo_rank(node)`.
    pub values: TimingValues,
    /// Execution and timing statistics (`upload_s`, `kernel_s`, `readback_s`, `total_s`, etc.).
    pub stats: PropagationStats,
    /// Wall-clock seconds spent lowering the graph and inputs into the compute ABI.
    pub lower_s: f64,
}

impl LoweredResult {
    /// Returns the arrival time (ns) at `node` for `tag`, transition `tr` and bound `an`.
    #[inline]
    #[must_use]
    pub fn arrival(&self, graph: &TimingGraph, node: NodeId, tag: usize, tr: Tr, an: An) -> f64 {
        let pos = graph.topo_rank(node) as usize;
        self.values.arrival(pos, tag, tr.index(), an.index())
    }

    /// Returns the per-pin transition time (ns) at `node` for transition `tr` and bound `an`.
    #[inline]
    #[must_use]
    pub fn slew(&self, graph: &TimingGraph, node: NodeId, tr: Tr, an: An) -> f64 {
        let pos = graph.topo_rank(node) as usize;
        self.values.transition(pos, tr.index(), an.index())
    }

    /// Returns the predecessor `(src_node, src_tr)` that set the arrival at `node` for
    /// `(tag, tr, an)`, or `None` if the arrival is a start value or absent (`COMPUTE-REQ-0016`).
    #[inline]
    #[must_use]
    pub fn predecessor(
        &self,
        graph: &TimingGraph,
        node: NodeId,
        tag: usize,
        tr: Tr,
        an: An,
    ) -> Option<(NodeId, Tr)> {
        let pos = graph.topo_rank(node) as usize;
        let raw_pred = self.values.predecessor(pos, tag, tr.index(), an.index());
        let (src_pos, src_ti) = pred_source(raw_pred)?;
        let src_node = graph.order()[src_pos as usize];
        let src_tr = if src_ti == 0 { Tr::Rise } else { Tr::Fall };
        Some((src_node, src_tr))
    }

    /// Remaps `self.values` from level-ordered node indices (`pos = graph.topo_rank(node)`) into
    /// [`NodeId`]-ordered arrays (`0..graph.num_nodes()`), translating predecessor node indices
    /// to `src_node.0 | (ti << 31)`.
    #[must_use]
    pub fn to_node_order(&self, graph: &TimingGraph) -> TimingValues {
        let n_nodes = graph.num_nodes();
        let ntags = self.values.ntags as usize;
        let w = ntags * 4;
        let mut arr = vec![0.0; n_nodes * w];
        let mut slew = vec![0.0; n_nodes * 4];
        let mut pred = vec![NO_PRED; n_nodes * w];
        let order = graph.order();

        for (pos, &node) in order.iter().enumerate() {
            let v = node.index();
            let dst_arr = &mut arr[v * w..(v + 1) * w];
            let src_arr = &self.values.arr[pos * w..(pos + 1) * w];
            dst_arr.copy_from_slice(src_arr);

            let dst_slew = &mut slew[v * 4..(v + 1) * 4];
            let src_slew = &self.values.slew[pos * 4..(pos + 1) * 4];
            dst_slew.copy_from_slice(src_slew);

            let dst_pred = &mut pred[v * w..(v + 1) * w];
            let src_pred = &self.values.pred[pos * w..(pos + 1) * w];
            for (dp, &sp) in dst_pred.iter_mut().zip(src_pred.iter()) {
                if let Some((src_pos, ti)) = pred_source(sp) {
                    let src_node = order[src_pos as usize];
                    *dp = src_node.0 | ((ti as u32) << 31);
                } else {
                    *dp = NO_PRED;
                }
            }
        }

        TimingValues {
            ntags: self.values.ntags,
            arr,
            slew,
            pred,
        }
    }
}

/// Ideal clock specification for seeding start rows and latch tag maps (`STA-REQ-0200`,
/// `STA-REQ-0201`, `STA-REQ-0213`).
#[derive(Debug, Clone)]
pub struct IdealClockSpec {
    /// Clock name.
    pub name: String,
    /// Clock period in ns (`> 0.0`).
    pub period: f64,
    /// Waveform `[rise_edge_ns, fall_edge_ns]` within `[0, period)`.
    pub waveform: [f64; 2],
    /// Source port/pin [`NodeId`]s of the clock.
    pub sources: Vec<NodeId>,
    /// Clock latency `[early_ns, late_ns]`.
    pub latency: [f64; 2],
    /// Clock transition `[early_ns, late_ns]`.
    pub transition: [f64; 2],
    /// Clock uncertainty `[setup_ns, hold_ns]`.
    pub uncertainty: [f64; 2],
}

impl IdealClockSpec {
    /// Creates an ideal clock with default 50% duty cycle `[0.0, period / 2.0]` and zero
    /// latency, transition and uncertainty.
    #[must_use]
    pub fn new(name: impl Into<String>, period: f64, sources: Vec<NodeId>) -> Self {
        Self {
            name: name.into(),
            period,
            waveform: [0.0, period * 0.5],
            sources,
            latency: [0.0, 0.0],
            transition: [0.0, 0.0],
            uncertainty: [0.0, 0.0],
        }
    }
}

/// Input delay specification on a primary input port node (`STA-REQ-0201`, `STA-REQ-0217`).
#[derive(Debug, Clone, Copy)]
pub struct InputDelaySpec {
    /// Target input port [`NodeId`].
    pub node: NodeId,
    /// Index of the launch clock in [`IdealSchedule::clocks`].
    pub clock: u32,
    /// Active launch clock source edge (`Rise` or `Fall`).
    pub edge: Tr,
    /// Input arrival `[early_ns, late_ns]` relative to the clock edge (including clock latency).
    pub delay: [f64; 2],
    /// Input pin transition `[early_ns, late_ns]`.
    pub transition: [f64; 2],
}

/// Ideal clock and I/O schedule used to seed [`StartRows`] and latch [`ArcTagMaps`] on a
/// [`TimingGraph`] (`STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0213`).
#[derive(Debug, Clone, Default)]
pub struct IdealSchedule {
    /// Ideal clocks in definition order. Tag `(clock_idx, edge)` is `clock_idx * 2 + edge.index()`.
    pub clocks: Vec<IdealClockSpec>,
    /// Input delays on primary input port nodes.
    pub input_delays: Vec<InputDelaySpec>,
    /// Untagged input transitions `(node, [early_ns, late_ns])` on unconstrained input ports
    /// (`STA-REQ-0115`).
    pub untagged_transitions: Vec<(NodeId, [f64; 2])>,
    /// Optional per-latch `set_max_time_borrow` override in ns keyed by latch `D -> Q` graph arc
    /// index (`STA-REQ-0221`).
    pub max_time_borrow: HashMap<u32, f64>,
}

impl IdealSchedule {
    /// Builds a default corpus schedule for `netview` and `graph`:
    /// - Any input port named `clk`, `clk_i`, `clock`, `ck`, `en`, `en_n`, `g`, `gate` becomes a
    ///   clock (or a virtual clock `vclk` of period `1.5 ns` is created on the first input port if
    ///   no clock-named port exists);
    /// - Every non-clock input port gets `set_input_delay 0.2` and `set_input_transition 0.03` on
    ///   the first clock.
    #[must_use]
    pub fn default_for_netlist(netview: &NetlistView<'_>) -> Self {
        let mut port_by_name: HashMap<String, NodeId> = HashMap::new();
        let mut input_nodes = Vec::new();
        for i in 0..netview.num_port_bits() {
            let node = NodeId(i as u32);
            if let NodeKind::PortBit { dir, .. } = netview.node_kind(node)
                && matches!(dir, Direction::Input | Direction::Inout)
            {
                let name = netview.node_name_str(node).into_owned();
                port_by_name.insert(name, node);
                input_nodes.push(node);
            }
        }

        let candidates = [
            ("clk", 1.2, [0.0, 0.6]),
            ("clk_i", 1.2, [0.0, 0.6]),
            ("clock", 1.2, [0.0, 0.6]),
            ("ck", 1.2, [0.0, 0.6]),
            ("en", 2.0, [0.0, 0.7]),
            ("en_n", 2.0, [1.0, 1.8]),
            ("g", 2.0, [0.0, 1.0]),
            ("gate", 2.0, [0.0, 1.0]),
        ];

        let mut clocks = Vec::new();
        let mut clock_port_set = std::collections::HashSet::new();
        for (name, period, waveform) in candidates {
            if let Some(&node) = port_by_name.get(name) {
                let mut ck = IdealClockSpec::new(name, period, vec![node]);
                ck.waveform = waveform;
                ck.transition = [0.03, 0.03];
                clocks.push(ck);
                clock_port_set.insert(node);
            }
        }

        if clocks.is_empty() {
            let sources = input_nodes.first().copied().into_iter().collect();
            let mut vclk = IdealClockSpec::new("vclk", 1.5, sources);
            vclk.transition = [0.03, 0.03];
            clocks.push(vclk);
        }

        let mut input_delays = Vec::new();
        for &node in &input_nodes {
            input_delays.push(InputDelaySpec {
                node,
                clock: 0,
                edge: Tr::Rise,
                delay: [0.2, 0.2],
                transition: [0.03, 0.03],
            });
            let _ = clock_port_set.contains(&node);
        }

        Self {
            clocks,
            input_delays,
            untagged_transitions: Vec::new(),
            max_time_borrow: HashMap::new(),
        }
    }

    /// Lowers `netview`, `graph`, `lib` and `net_loads` with the start rows and latch tag maps
    /// produced by this schedule (`STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0213`, `STA-REQ-0401`).
    pub fn lower<'a>(
        &self,
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &'a TimingLib,
        net_loads: &NetLoads,
    ) -> Result<LoweredCorner<'a>, Diag> {
        let (ntags, starts, tag_maps) =
            self.build_starts_and_maps(netview, graph, lib, net_loads)?;
        LoweredCorner::lower(netview, graph, lib, net_loads, ntags, starts, &tag_maps)
    }

    /// Computes `(ntags, StartRows, ArcTagMaps)` for this schedule.
    pub fn build_starts_and_maps(
        &self,
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &TimingLib,
        net_loads: &NetLoads,
    ) -> Result<(u32, StartRows, ArcTagMaps), Diag> {
        let n_nodes = graph.num_nodes();
        let ntags = (2 * self.clocks.len().max(1)) as u32;
        let cell_arcs = map_cell_arcs(netview, lib)?;
        let canon_totals = canonical_net_totals(netview, net_loads);

        // 1. Propagate ideal clock sense bits (bit 0 positive, bit 1 negative) across enabled
        // net and combinational/tristate arcs, skipping latch data arcs (STA-REQ-0200).
        let mut clock_sense: Vec<Vec<(u32, u8)>> = vec![Vec::new(); n_nodes];
        let g_arcs = graph.arcs();
        for (ci, ck) in self.clocks.iter().enumerate() {
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

        // 2. Seed startpoint rows (clock-as-data, input delays, untagged input slews, and
        // sequential CK -> Q launches).
        let mut starts = GraphStartBuilder::new(ntags);

        for (ci, ck) in self.clocks.iter().enumerate() {
            for &src in &ck.sources {
                if src.index() >= n_nodes {
                    continue;
                }
                for edge in Tr::ALL {
                    let t = ci as u32 * 2 + edge.index() as u32;
                    for an in An::ALL {
                        starts.merge_bound(
                            graph,
                            src,
                            t,
                            edge,
                            an,
                            ck.latency[an.index()],
                            ck.transition[an.index()],
                        );
                    }
                }
            }
        }

        for d in &self.input_delays {
            if d.node.index() >= n_nodes {
                continue;
            }
            let t = d.clock * 2 + d.edge.index() as u32;
            if t >= ntags {
                continue;
            }
            for tr in Tr::ALL {
                for an in An::ALL {
                    starts.merge_bound(
                        graph,
                        d.node,
                        t,
                        tr,
                        an,
                        d.delay[an.index()],
                        d.transition[an.index()],
                    );
                }
            }
        }

        for &(node, s) in &self.untagged_transitions {
            if node.index() < n_nodes {
                for tr in Tr::ALL {
                    for an in An::ALL {
                        starts.merge_untagged_slew(graph, node, tr, an, s[an.index()]);
                    }
                }
            }
        }

        for (li, l) in graph.launches().iter().enumerate() {
            if graph.is_launch_disabled(li) {
                continue;
            }
            let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
            let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
            let net_cap = match netview.node_net(l.out_pin) {
                NetTarget::Net(nr) => canon_totals.get(nr.index()).copied().unwrap_or_default(),
                NetTarget::Const(_) => [[0.0; 2]; 2],
            };
            for &(c, bits) in &clock_sense[l.clock_pin.index()] {
                let ck = &self.clocks[c as usize];
                for src_edge in source_edges(l.edge, bits) {
                    let t = c * 2 + src_edge.index() as u32;
                    for to in Tr::ALL {
                        for an in An::ALL {
                            let c_load = net_cap[to.index()][an.index()];
                            let ds = lib.arc_delay(
                                global_arc,
                                to,
                                an,
                                ck.transition[an.index()],
                                DriverLoad::Lumped { c: c_load },
                            );
                            starts.merge_bound(
                                graph,
                                l.out_pin,
                                t,
                                to,
                                an,
                                ck.latency[an.index()] + ds.delay,
                                ds.slew,
                            );
                        }
                    }
                }
            }
        }

        // 3. Build TagMapRec windows for transparent latch D -> Q arcs (STA-REQ-0213,
        // COMPUTE-REQ-0012, COMPUTE-REQ-0018).
        let mut tag_maps = ArcTagMaps::new();
        let g_checks = graph.checks();
        let g_launches = graph.launches();

        for (ai, arc) in g_arcs.iter().enumerate() {
            if graph.is_arc_disabled(ai) {
                continue;
            }
            let ArcKind::LatchData {
                inst, setup_check, ..
            } = arc.kind
            else {
                continue;
            };
            let Some(ch) = g_checks.get(setup_check as usize) else {
                continue;
            };
            let open_pin_edge = g_launches
                .iter()
                .find(|l| l.inst == inst && l.clock_pin == ch.clock_pin && l.out_pin == arc.dst)
                .map_or_else(|| ch.edge.opposite(), |l| l.edge);

            let close_slew = clock_sense[ch.clock_pin.index()]
                .first()
                .map_or([0.0, 0.0], |&(c, _)| self.clocks[c as usize].transition);
            let max_borrow = self.max_time_borrow.get(&(ai as u32)).copied();

            let mut recs = Vec::new();
            for src_tag in 0..ntags {
                let (l_idx, el_idx) = ((src_tag / 2) as usize, (src_tag % 2) as usize);
                if l_idx >= self.clocks.len() {
                    continue;
                }
                let ck_l = &self.clocks[l_idx];
                let e_l = if el_idx == 0 { Tr::Rise } else { Tr::Fall };

                let mut best_close = f64::NEG_INFINITY;
                let mut best_open = f64::INFINITY;
                for &(c, bits) in &clock_sense[ch.clock_pin.index()] {
                    let ck_c = &self.clocks[c as usize];
                    for ec in source_edges(ch.edge, bits) {
                        if let Some(ds) = setup_relation_ns(ck_l, e_l, ck_c, ec) {
                            best_close = best_close
                                .max(ds + ck_c.latency[An::Late.index()] - ck_c.uncertainty[0]);
                        }
                    }
                    for eo in source_edges(open_pin_edge, bits) {
                        if let Some(do_rel) = open_relation_ns(ck_l, e_l, ck_c, eo) {
                            best_open = best_open.min(
                                do_rel + ck_c.latency[An::Early.index()] - ck_c.uncertainty[0],
                            );
                        }
                    }
                }

                let open_arr = if best_open < f64::INFINITY {
                    best_open
                } else {
                    f64::NEG_INFINITY
                };
                let close_arr = if best_close > f64::NEG_INFINITY {
                    best_close
                } else {
                    f64::INFINITY
                };
                let limit_arr = match (max_borrow, best_open < f64::INFINITY) {
                    (Some(b), true) => best_open + b,
                    _ => f64::INFINITY,
                };

                if open_arr > f64::NEG_INFINITY || close_arr < f64::INFINITY {
                    recs.push(TagMapRec {
                        src_tag,
                        dst_tag: src_tag,
                        shift: 0.0,
                        open: [open_arr, f64::NEG_INFINITY],
                        close: [f64::INFINITY, close_arr],
                        close_slew,
                        limit: [f64::INFINITY, limit_arr],
                    });
                } else {
                    recs.push(TagMapRec::through(src_tag, src_tag));
                }
            }
            if !recs.is_empty() {
                tag_maps.insert(ai as u32, recs);
            }
        }

        Ok((ntags, starts.build(), tag_maps))
    }
}

fn source_edges(pin_edge: Tr, bits: u8) -> Vec<Tr> {
    let mut out = Vec::with_capacity(2);
    if bits & 1 != 0 {
        out.push(pin_edge);
    }
    if bits & 2 != 0 {
        out.push(pin_edge.opposite());
    }
    out
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

/// Smallest strictly positive `capture - launch` relation in ns over the common period (`STA-REQ-0220`).
fn setup_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(launch, e_l, capture, e_c, true)
}

/// Smallest non-negative `capture - launch` relation in ns over the common period.
fn open_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(launch, e_l, capture, e_c, false)
}

fn edge_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
    strict: bool,
) -> Option<f64> {
    let tl = to_fs(launch.period);
    let tc = to_fs(capture.period);
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let g = gcd(tl, tc);
    let mult = tc / g;
    if mult > 1000 {
        return None;
    }
    let wl = to_fs(launch.waveform[e_l.index()]);
    let wc = to_fs(capture.waveform[e_c.index()]);
    let mut best = i64::MAX;
    for k in 0..mult {
        let l = wl + k * tl;
        let min_c = if strict { l + 1 } else { l };
        let j = (min_c - wc).div_euclid(tc);
        let mut c = wc + j * tc;
        if c < min_c {
            c += tc;
        }
        best = best.min(c - l);
    }
    (best < i64::MAX).then_some(best as f64 * 1e-6)
}

fn map_cell_arcs(netview: &NetlistView<'_>, lib: &TimingLib) -> Result<Vec<Vec<LibArc>>, Diag> {
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

fn canonical_net_totals<'a>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusteda_compute::timing::{EARLY, LATE, RISE};

    #[test]
    fn compute_error_codes_match_contract() {
        assert_eq!(
            compute_error_code(&ComputeError::NotCompiled("cuda")),
            "A0501"
        );
        assert_eq!(
            compute_error_code(&ComputeError::Unavailable("no gpu".into())),
            "A0501"
        );
        assert!(is_backend_unavailable(&ComputeError::NotCompiled("cuda")));
        assert_eq!(
            compute_error_code(&ComputeError::TooLarge("big".into())),
            "A0502"
        );
        assert_eq!(
            compute_error_code(&ComputeError::Compile("nvrtc".into())),
            "A0503"
        );
        assert_eq!(
            compute_error_code(&ComputeError::Device("drv".into())),
            "A0504"
        );
        assert_eq!(
            compute_error_code(&ComputeError::Invalid("csr".into())),
            "A0505"
        );
        assert_eq!(compute_error_code(&ComputeError::Cancelled), "A0506");
        let diag = compute_error_diag(&ComputeError::NotCompiled("cuda"));
        assert_eq!(diag.code, "A0501");
    }

    #[test]
    fn canonical_zero_and_indices() {
        assert_eq!(canon_zero(-0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(arr_index(0, 0, RISE, EARLY, 1), 0);
        assert_eq!(slew_index(0, RISE, LATE), 1);
        let _ = (Scope::All, Readback::All);
    }
}
