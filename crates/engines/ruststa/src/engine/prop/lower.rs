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
use std::collections::BTreeMap;
use std::time::Instant;

use rustc_hash::FxHashMap;

mod rc;
mod schedule;
mod slew_only;
mod start_builder;

pub use schedule::{IdealClockSpec, IdealSchedule, InputDelaySpec};
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

use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::{DriverLoad, WireModel};
use crate::engine::graph::{ArcKind, ArcSense, GraphArc, TimingGraph};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView};
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
    /// to `graph.arcs().len() + li` ([`Self::csr_raw_arc`], `STA-REQ-0117`).
    pub csr_arc_to_graph_arc: Vec<u32>,
    /// Maps each [`TimingGraph`] arc index (`0..graph.num_arcs()`) to its CSR incoming arc index,
    /// or [`NONE`] when the arc is disabled.
    pub graph_arc_to_csr_arc: Vec<u32>,
    /// Corner lookup-table bank and [`ArcClass`](rusteda_compute::timing::ArcClass) descriptors.
    pub luts: CompiledLutBank<'a>,
    /// Number of propagation tags `T >= 1`.
    pub ntags: u32,
    /// Global early/late delay factors, shared by all propagation backends.
    pub derates: crate::engine::delay::derate::TimingDerate,
    /// Per level-ordered node capacitive loads `[Rise/Early, Rise/Late, Fall/Early, Fall/Late]` in pF.
    pub load: Vec<[f64; 4]>,
    /// Startpoint rows in level-ordered node coordinates.
    pub starts: StartRows,
    /// Optional RC wire records per CSR incoming arc (empty for ideal/lumped wire models).
    pub wires: Vec<WireRec>,
    /// CPU-prepared f64 RC arc values, shared by all propagation backends.
    pub arc_values: Vec<ArcValues>,
    /// The delays of `arc_values` alone (`ArcValues::delay`), once only the required-time
    /// propagation reads them ([`Self::retain_for_required_times`]); empty otherwise.
    pub arc_delays: Vec<[f64; 8]>,
    /// Per level-ordered node `[Rise/Early, Rise/Late, Fall/Early, Fall/Late]`: the driver
    /// waveform (Thevenin resistance in kOhm, source ramp duration in ns) that set the node's
    /// prepared transition, when a cell arc into a pi load outside the lumped regime set it
    /// (empty without RC preparation; `STA-REQ-0729`).
    pub drives: Vec<[Option<(f64, f64)>; 4]>,
    /// The driver waveforms of the start transitions at sequential outputs (`STA-REQ-0729`).
    pub start_drives: FxHashMap<NodeId, [Option<(f64, f64)>; 4]>,
    /// The tag map of the incoming arcs that carry only their output transition (an empty record
    /// range, so that no arrival passes them), or [`NONE`] without any such arc: the arcs and
    /// launches that logic values disable, and the library transitions that a sense refinement
    /// removed from an enabled arc (`STA-REQ-0117`).
    pub slew_only_map: u32,
    /// The incoming arcs that a cone lowering ([`LoweredCorner::lower_cone`]) left out; empty for
    /// a full lowering.
    pub pruned: PrunedArcs,
    /// Wall-clock seconds spent lowering into the compute ABI.
    pub lower_s: f64,
}

/// The incoming arcs that [`LoweredCorner::lower_cone`] leaves out, counted by their
/// [`ArcClass::flags`](rusteda_compute::timing::ArcClass::flags), the only part of a class that
/// the state and work budgets of the RC waveform preparation read. The preparation charges them
/// as if it prepared them, so that `--max-state-gib` decides, and reports, for a cone exactly as
/// for the whole graph (`STA-REQ-0713`, `STA-REQ-0723`).
#[derive(Debug, Clone, Default)]
pub struct PrunedArcs {
    /// `(flags, arcs)`, one entry per distinct flags value, in the order first seen.
    by_flags: Vec<(u32, u64)>,
}

impl PrunedArcs {
    /// Counts one more left-out arc of a class with `flags`.
    fn add(&mut self, flags: u32) {
        match self.by_flags.iter_mut().find(|(f, _)| *f == flags) {
            Some((_, arcs)) => *arcs += 1,
            None => self.by_flags.push((flags, 1)),
        }
    }

    /// The number of left-out arcs.
    #[must_use]
    pub fn arcs(&self) -> u64 {
        self.by_flags.iter().map(|&(_, arcs)| arcs).sum()
    }

    /// The left-out arcs as `(flags, arcs)`, one entry per distinct flags value.
    #[must_use]
    pub fn by_flags(&self) -> &[(u32, u64)] {
        &self.by_flags
    }
}

/// The level-ordered positions that [`LoweredCorner::lower_cone`] keeps the incoming arcs of:
/// the nodes from which a node that an arrival from `roots` reaches (over the enabled arcs, the
/// only ones without a tag map in such a propagation) can be reached over the lowered arcs, the
/// enabled ones and the transition-only ones of `slew_only` (`(dst, src, arc, class)` by
/// position, sorted). `O(V + E)`.
fn cone_positions(
    graph: &TimingGraph,
    roots: &[NodeId],
    slew_only: &[(u32, u32, u32, u32)],
) -> Vec<bool> {
    let n = graph.num_nodes();
    let g_arcs = graph.arcs();
    let order = graph.order();
    // An arrival reaches a node through an enabled incoming arc from a node it reaches; the
    // sources of a node's incoming arcs come before it in level order.
    let mut reached = vec![false; n];
    for &root in roots {
        if root.index() < n {
            reached[graph.topo_rank(root) as usize] = true;
        }
    }
    for pos in 0..n {
        if !reached[pos] {
            reached[pos] = graph
                .incoming(order[pos])
                .iter()
                .any(|&ai| reached[graph.topo_rank(g_arcs[ai as usize].src) as usize]);
        }
    }
    let mut positions: Vec<u32> = (0..n as u32).filter(|&p| reached[p as usize]).collect();
    let mut keep = reached;
    while let Some(pos) = positions.pop() {
        let sources = graph
            .incoming(order[pos as usize])
            .iter()
            .map(|&ai| graph.topo_rank(g_arcs[ai as usize].src));
        let first = slew_only.partition_point(|e| e.0 < pos);
        let extra = slew_only[first..]
            .iter()
            .take_while(|e| e.0 == pos)
            .map(|e| e.1);
        for src in sources.chain(extra) {
            if !keep[src as usize] {
                keep[src as usize] = true;
                positions.push(src);
            }
        }
    }
    keep
}

impl<'a> LoweredCorner<'a> {
    /// The prepared delay `j` (`(input transition * 2 + output transition) * 2 + bound`) of CSR
    /// arc `k`: of its arc values, or of their delays once only those are kept.
    #[inline]
    #[must_use]
    pub fn arc_delay(&self, k: usize, j: usize) -> Option<f64> {
        match self.arc_values.get(k) {
            Some(values) => Some(values.delay[j]),
            None => self.arc_delays.get(k).map(|delays| delays[j]),
        }
    }

    /// Keeps only what the required-time propagation of the timing table reads
    /// (`BackwardPass::propagate`): the CSR, the tables, the loads and the delays of the arc
    /// values ([`Self::arc_delays`], in the allocation of the arc values, half of it released),
    /// and the wire records only when an arc has no value. The transitions of the arc values,
    /// the driver waveforms, the start rows and the arc maps are released; the problem can no
    /// longer be propagated forward.
    pub fn retain_for_required_times(&mut self) {
        if self.arc_values.len() >= self.csr.in_arcs.len() {
            self.wires = Vec::new();
        }
        if !self.arc_values.is_empty() {
            let mut delays: Vec<[f64; 8]> = std::mem::take(&mut self.arc_values)
                .into_iter()
                .map(|v| v.delay)
                .collect();
            delays.shrink_to_fit();
            self.arc_delays = delays;
        }
        self.drives = Vec::new();
        self.start_drives = FxHashMap::default();
        self.starts = StartRows::default();
        self.csr_arc_to_graph_arc = Vec::new();
        self.graph_arc_to_csr_arc = Vec::new();
    }

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
        Self::lower_into(
            netview, graph, lib, net_loads, ntags, starts, tag_maps, None,
        )
    }

    /// [`Self::lower`] with only the arcs that a propagation of arrivals from `roots` reads, for a
    /// propagation whose only arrivals start at `roots` and whose values are read only where an
    /// arrival is present, such as the clock network (`STA-REQ-0713`): the arcs into the nodes
    /// from which a node that an arrival from `roots` reaches can be reached. Every other node
    /// keeps its row, without incoming arcs. Arrivals pass only the enabled arcs without tag
    /// maps, so every node with an arrival, and every node whose value it reads, keeps all its
    /// incoming arcs: their values are those of [`Self::lower`] bit for bit, and the other
    /// nodes have no arrival in either. The arc tables of the waveform preparation are sized to
    /// the kept arcs; its state and work budgets are charged for the left-out arcs as well
    /// ([`PrunedArcs`]), so that a budget rejects, with the same diagnostic, exactly what it
    /// rejects for [`Self::lower`].
    #[allow(clippy::too_many_arguments)]
    pub fn lower_cone(
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &'a TimingLib,
        net_loads: &NetLoads,
        ntags: u32,
        starts: StartRows,
        tag_maps: &ArcTagMaps,
        roots: &[NodeId],
    ) -> Result<Self, Diag> {
        Self::lower_into(
            netview,
            graph,
            lib,
            net_loads,
            ntags,
            starts,
            tag_maps,
            Some(roots),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_into(
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &'a TimingLib,
        net_loads: &NetLoads,
        ntags: u32,
        starts: StartRows,
        tag_maps: &ArcTagMaps,
        roots: Option<&[NodeId]>,
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
        let keep = roots.map(|roots| cone_positions(graph, roots, &slew_only));
        let mut pruned = PrunedArcs::default();

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
            if keep.as_ref().is_some_and(|keep| !keep[pos]) {
                // Left out: counted by the classes the full lowering gives these arcs.
                for &ai in graph.incoming(raw_node) {
                    let g_arc = &g_arcs[ai as usize];
                    pruned.add(match g_arc.kind {
                        ArcKind::Net { .. } => luts.classes[lib.wire_class as usize].flags,
                        _ => {
                            let (inst, local_arc) = g_arc
                                .kind
                                .inst_and_lib_arc()
                                .expect("non-net arc has inst and lib_arc");
                            slew_only::class_flags(
                                netview,
                                lib,
                                &cell_arcs,
                                inst,
                                local_arc,
                                Some(g_arc.sense),
                            )
                        }
                    });
                }
                for &(_, _, _, e_class) in extras {
                    pruned.add(luts.classes[e_class as usize].flags);
                }
                continue;
            }
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
            derates: Default::default(),
            load,
            starts,
            wires: Vec::new(),
            arc_values: Vec::new(),
            arc_delays: Vec::new(),
            drives: Vec::new(),
            start_drives: FxHashMap::default(),
            slew_only_map,
            pruned,
            lower_s,
        })
    }

    /// True when CSR incoming arc `k` carries only its output transition (`STA-REQ-0117`).
    #[inline]
    #[must_use]
    pub fn is_slew_only(&self, k: usize) -> bool {
        self.slew_only_map != NONE && self.csr.in_arcs[k].map == self.slew_only_map
    }

    /// The timing graph arc of CSR incoming arc `k`; for an arc carrying the transition of a
    /// launch arc disabled by logic values, the clock-to-output arc as a cell arc
    /// (`STA-REQ-0117`).
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
        rc::prepare(self, graph, view, wire, lib, max_bytes, true)
    }

    /// [`Self::set_wire_model`] without the wire records of the extracted net arcs (their first
    /// moments), for a problem whose propagations read only the prepared arc values: the timing
    /// table's analysis and the clock network propagation (AGENTS rule 9). The values are the
    /// same.
    pub fn set_wire_values(
        &mut self,
        graph: &TimingGraph,
        view: &NetlistView<'_>,
        wire: &dyn WireModel,
        lib: &TimingLib,
        max_bytes: u64,
    ) -> Result<(), Diag> {
        rc::prepare(self, graph, view, wire, lib, max_bytes, false)
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

/// Propagated arrivals and transitions looked up by node: a [`LoweredResult`], whose values are
/// level-ordered, or [`NodeOrdered`], its node-ordered copy ([`LoweredResult::to_node_order`]),
/// which holds the same values bit for bit.
pub trait NodeValues: Sync {
    /// The arrival time (ns) at `node` for `tag`, transition `tr` and bound `an`.
    fn arrival(&self, graph: &TimingGraph, node: NodeId, tag: usize, tr: Tr, an: An) -> f64;
    /// The transition time (ns) at `node` for transition `tr` and bound `an`.
    fn slew(&self, graph: &TimingGraph, node: NodeId, tr: Tr, an: An) -> f64;
}

impl NodeValues for LoweredResult {
    fn arrival(&self, graph: &TimingGraph, node: NodeId, tag: usize, tr: Tr, an: An) -> f64 {
        LoweredResult::arrival(self, graph, node, tag, tr, an)
    }

    fn slew(&self, graph: &TimingGraph, node: NodeId, tr: Tr, an: An) -> f64 {
        LoweredResult::slew(self, graph, node, tr, an)
    }
}

/// Node-ordered propagation values ([`LoweredResult::to_node_order`]) looked up by node, so that
/// a corner that keeps only them reads the values of its propagation.
#[derive(Debug, Clone, Copy)]
pub struct NodeOrdered<'v>(pub &'v TimingValues);

impl NodeValues for NodeOrdered<'_> {
    fn arrival(&self, _graph: &TimingGraph, node: NodeId, tag: usize, tr: Tr, an: An) -> f64 {
        self.0.arrival(node.index(), tag, tr.index(), an.index())
    }

    fn slew(&self, _graph: &TimingGraph, node: NodeId, tr: Tr, an: An) -> f64 {
        self.0.transition(node.index(), tr.index(), an.index())
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
