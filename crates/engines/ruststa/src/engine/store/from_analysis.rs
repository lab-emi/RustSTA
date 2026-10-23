// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing table of a revision (`sta.rbin`, STA v2) built from an [`EngineAnalysis`]: the table
//! that `ruststa run` and `ruststa update` commit (owner: E10; `STA-REQ-0302`, `STA-REQ-0600`).
//!
//! The table depends only on the analysis, never on how its propagation ran: a full analysis and
//! an incremental analysis with bitwise equal values give byte-identical tables (`STA-REQ-0611`).
//! Every lookup of a check or a launch by pin goes through an index built once per table, so the
//! build stays linear in the size of the design.

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use rusteda_core::diag as core_diag;
use std::time::Instant;

use rusteda_core::job::{Ctx, Diag, Event};
use rusteda_core::liberty::{Direction, Library};

use crate::engine::analysis::{CornerAnalysis, EngineAnalysis};
use crate::engine::delay::derate::NoDerate;
use crate::engine::graph::{CheckKind, DisableReason, TimingGraph};
use crate::engine::input::NetlistView;
use crate::engine::input::constr::CompiledConstraints;
use crate::engine::input::netview::NodeKind;
use crate::engine::prop::backward::{BackwardPass, ByNode};
use crate::engine::prop::checks::{EndpointCheckKind, SlackRecord};
use crate::engine::prop::clocks::ClockModel;
use crate::engine::prop::endpoints::EndpointStatus;
use crate::engine::prop::lower::LoweredCorner;
use crate::engine::prop::starts::build_start_rows;
use crate::engine::store::{
    ABSENT_EARLY_LATE, ABSENT_REQUIRED, ABSENT_SLACK, ClockRec, CompletenessReport, CornerRec,
    DisabledArcRec, EndpointKindTag, EndpointRec, EpSlackRec, InstHashBuilder, NO_INDEX, NO_PRED,
    NameHashBuilder, NodeRec, NoteRec, NoteSeverity, PATH_CHAIN_HEADER_WORDS, PathHeaderRec,
    PathPointRec, PathRankRec, ScopeReport, StaMeta, StaStoreData, StoreMode, StorePrecision,
    StringTableBuilder, SummaryRec, TagExtraRec, TagRec, UnconstrainedReasonTag, UnconstrainedRec,
    canon_f32, canon_f64, hash_node_name, pack_chain_step, pack_f64_words,
};
use crate::engine::types::{An, NodeId, Tr};

mod capacity;
mod steps;

pub use capacity::check_store_tables;
use steps::{compute_check_intrinsic, for_each_path_step};

/// The checks and launches of the timing graph indexed by pin, in graph order, so that each
/// lookup of the table build is constant time instead of a scan of every check or launch.
struct PinIndex {
    /// The indices of the checks with each data pin, in graph order.
    checks_by_data_pin: FxHashMap<NodeId, Vec<u32>>,
    /// The first launch in graph order with each output pin.
    first_launch: FxHashMap<NodeId, u32>,
}

impl PinIndex {
    fn new(graph: &TimingGraph) -> Self {
        let mut checks_by_data_pin: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
        for (i, ch) in graph.checks().iter().enumerate() {
            checks_by_data_pin
                .entry(ch.data_pin)
                .or_default()
                .push(i as u32);
        }
        let mut first_launch = FxHashMap::default();
        for (i, l) in graph.launches().iter().enumerate() {
            first_launch.entry(l.out_pin).or_insert(i as u32);
        }
        Self {
            checks_by_data_pin,
            first_launch,
        }
    }

    /// The checks with data pin `node`, in graph order.
    fn checks_of(&self, node: NodeId) -> &[u32] {
        self.checks_by_data_pin
            .get(&node)
            .map_or(&[], Vec::as_slice)
    }
}

/// The parts of an analysis that its timing table is built from: the fields of an
/// [`EngineAnalysis`], which a timing session keeps without a netlist view.
#[derive(Debug, Clone, Copy)]
pub struct StoreInput<'x> {
    /// The netlist view.
    pub netview: &'x NetlistView<'x>,
    /// The timing graph.
    pub graph: &'x TimingGraph,
    /// The compiled constraints.
    pub constraints: &'x CompiledConstraints,
    /// The analysis of every corner, in corner order.
    pub corners: &'x [CornerAnalysis],
    /// The warnings and notes of the analysis.
    pub diags: &'x [Diag],
}

impl<'x> StoreInput<'x> {
    /// The parts of `analysis`.
    #[must_use]
    pub fn of(analysis: &'x EngineAnalysis<'x>) -> Self {
        Self {
            netview: &analysis.netview,
            graph: &analysis.graph,
            constraints: &analysis.constraints,
            corners: &analysis.corners,
            diags: &analysis.diags,
        }
    }
}

/// Builds a deterministic 24-section [`StaStoreData`] directly from [`EngineAnalysis`]
/// (`STA-REQ-0302`).
pub fn build_store_from_analysis(
    analysis: &EngineAnalysis<'_>,
    corner_libs_owned: &[(String, Vec<Library>)],
    backend_name: &str,
    precision: StorePrecision,
    store_mode: StoreMode,
    top_k_paths: usize,
) -> StaStoreData {
    build_store_from_analysis_with_ctx(
        analysis,
        None,
        corner_libs_owned,
        backend_name,
        precision,
        store_mode,
        top_k_paths,
        Ctx::NONE,
    )
}

/// [`build_store_from_analysis`] reporting the wall time of its steps to the observer of `ctx`
/// as `store.*` phases (JOB-REQ-0003), and reusing `lowered`, the lowered problem of every corner
/// that the analysis kept (`EngineAnalysis::analyze_with`), for the backward pass instead of
/// lowering each corner again. The table depends on neither `ctx` nor `lowered`.
pub fn build_store_from_analysis_with_ctx(
    analysis: &EngineAnalysis<'_>,
    lowered: Option<&[LoweredCorner<'_>]>,
    corner_libs_owned: &[(String, Vec<Library>)],
    backend_name: &str,
    precision: StorePrecision,
    store_mode: StoreMode,
    top_k_paths: usize,
    ctx: Ctx<'_>,
) -> StaStoreData {
    build_store_with_ctx(
        &StoreInput::of(analysis),
        lowered,
        corner_libs_owned,
        backend_name,
        precision,
        store_mode,
        top_k_paths,
        ctx,
    )
}

/// Builds the timing table of the analysis whose parts are `analysis` (`STA-REQ-0302`,
/// `STA-REQ-0611`).
pub fn build_store(
    analysis: &StoreInput<'_>,
    corner_libs_owned: &[(String, Vec<Library>)],
    backend_name: &str,
    precision: StorePrecision,
    store_mode: StoreMode,
    top_k_paths: usize,
) -> StaStoreData {
    build_store_with_ctx(
        analysis,
        None,
        corner_libs_owned,
        backend_name,
        precision,
        store_mode,
        top_k_paths,
        Ctx::NONE,
    )
}

/// The wall time of the steps of [`build_store`], as `PhaseFinished` events (JOB-REQ-0003).
struct StepClock<'c> {
    ctx: Ctx<'c>,
    t: Instant,
}

impl StepClock<'_> {
    fn lap(&mut self, phase: &'static str) {
        if self.ctx.observed() {
            let seconds = self.t.elapsed().as_secs_f64();
            self.ctx.emit(|| Event::PhaseFinished { phase, seconds });
            self.t = Instant::now();
        }
    }
}

/// [`build_store`] reporting its steps to the observer of `ctx`, with the kept `lowered` corners
/// of the analysis when it has them.
#[allow(clippy::too_many_arguments)]
pub fn build_store_with_ctx(
    analysis: &StoreInput<'_>,
    lowered: Option<&[LoweredCorner<'_>]>,
    corner_libs_owned: &[(String, Vec<Library>)],
    backend_name: &str,
    precision: StorePrecision,
    store_mode: StoreMode,
    top_k_paths: usize,
    ctx: Ctx<'_>,
) -> StaStoreData {
    let mut clock = StepClock {
        ctx,
        t: Instant::now(),
    };
    let nv = analysis.netview;
    let graph = analysis.graph;
    let pin_index = PinIndex::new(graph);
    let frozen = nv.frozen();
    let n_nodes = graph.num_nodes();
    let n_port_bits = graph.num_port_bits();
    let n_instances = frozen.instances.len();
    let n_cells = frozen.cells.len();
    let n_corners = analysis.corners.len();

    let mut strings = StringTableBuilder::new();
    strings.reserve(
        n_port_bits + n_instances + n_cells + 64,
        (n_port_bits + n_instances) * 10 + 1024,
    );

    let backend_str = strings.intern(backend_name);
    let overall_group_str = strings.intern("**");
    let unclocked_group_str = strings.intern("<unclocked>");

    let mut corner_recs = Vec::with_capacity(n_corners);
    let mut corner_names = Vec::with_capacity(n_corners);
    for (idx, ca) in analysis.corners.iter().enumerate() {
        let name_str = strings.intern(&ca.corner_name);
        corner_names.push(ca.corner_name.clone());
        let lib_count = corner_libs_owned
            .get(idx)
            .map_or(1, |(_, libs)| libs.len() as u32);
        corner_recs.push(CornerRec {
            name_str,
            lib_count,
            flags: 0,
            reserved: 0,
        });
    }

    let mut clock_recs = Vec::with_capacity(analysis.constraints.clocks.len());
    let mut clock_group_strs = Vec::with_capacity(analysis.constraints.clocks.len());
    for ck in &analysis.constraints.clocks {
        let name_str = strings.intern(&ck.name);
        clock_group_strs.push(name_str);
        let source_node = ck.sources.first().map_or(NO_INDEX, |n| n.0);
        let flags = u32::from(ck.is_virtual() || ck.sources.is_empty());
        clock_recs.push(ClockRec {
            name_str,
            source_node,
            master_clock: NO_INDEX,
            flags,
            period_ns: canon_f64(ck.period_ns),
            rise_edge_ns: canon_f64(ck.waveform_ns[0]),
            fall_edge_ns: canon_f64(ck.waveform_ns[1]),
            uncertainty_setup_ns: canon_f32(ck.uncertainty_setup_ns[0] as f32),
            uncertainty_hold_ns: canon_f32(ck.uncertainty_hold_ns[0] as f32),
        });
    }

    let mut path_group_strs = vec![overall_group_str];
    for &gs in &clock_group_strs {
        if !path_group_strs.contains(&gs) {
            path_group_strs.push(gs);
        }
    }
    for ca in analysis.corners {
        for g_name in ca.endpoints.path_groups.keys() {
            let gs = strings.intern(g_name);
            if !path_group_strs.contains(&gs) {
                path_group_strs.push(gs);
            }
        }
    }
    let n_path_groups = path_group_strs.len();

    let first_tags = &analysis.corners[0].tags;
    let n_tags = first_tags.len().max(1);
    let mut tag_recs = Vec::with_capacity(n_tags);
    if first_tags.is_empty() {
        tag_recs.push(TagRec {
            clock_id: NO_INDEX,
            launch_edge: 0,
            flags: 0,
            exc_state: 0,
            launch_source_node: NO_INDEX,
            path_group_str: unclocked_group_str,
        });
    } else {
        for (_, tag) in first_tags.iter() {
            let clk_idx = tag.clock.index();
            let (clock_id, source_node, group_str) = if clk_idx < clock_recs.len() {
                (
                    clk_idx as u32,
                    clock_recs[clk_idx].source_node,
                    clock_group_strs[clk_idx],
                )
            } else {
                (NO_INDEX, NO_INDEX, unclocked_group_str)
            };
            tag_recs.push(TagRec {
                clock_id,
                launch_edge: tag.edge.index() as u8,
                flags: 0,
                exc_state: tag.exc_state.0 as u16,
                launch_source_node: source_node,
                path_group_str: group_str,
            });
        }
    }

    // Build cell & pin string tables + `NAME_HASH` + `INST_HASH`.
    let mut cell_str_ids = Vec::with_capacity(n_cells);
    let mut cell_pin_str_ids: Vec<Vec<u32>> = Vec::with_capacity(n_cells);
    let mut cell_pin_names: Vec<Vec<&str>> = Vec::with_capacity(n_cells);
    for cell_row in 0..n_cells {
        let c_name = nv.cell_name(cell_row as u32);
        cell_str_ids.push(strings.intern(c_name));
        let cp_range = frozen.cell_pins(cell_row as u32);
        let mut p_ids = Vec::with_capacity(cp_range.len());
        let mut p_names = Vec::with_capacity(cp_range.len());
        for cp in 0..cp_range.len() {
            let p_name = nv.cell_pin_name(cell_row as u32, cp as u32);
            p_ids.push(strings.intern(p_name));
            p_names.push(p_name);
        }
        cell_pin_str_ids.push(p_ids);
        cell_pin_names.push(p_names);
    }

    let mut name_hash = NameHashBuilder::with_capacity(n_nodes);
    let mut inst_hash = InstHashBuilder::with_capacity(n_instances);
    let mut node_bucket = vec![NO_INDEX; n_nodes];

    for pb in 0..n_port_bits {
        let node = NodeId(pb as u32);
        let port_name = nv.node_name_str(node);
        let port_str = strings.append_raw(&port_name);
        let h = hash_node_name(None, &port_name);
        node_bucket[pb] = name_hash.insert(h, node, NO_INDEX, port_str);
    }

    // The name hashes of the instance pins do not depend on the tables: compute them in
    // parallel, then insert in instance order (the bucket layout depends on insertion order).
    let pin_hashes: Vec<u64> = (0..frozen.instances.len())
        .into_par_iter()
        .with_min_len(1024)
        .flat_map_iter(|inst_row| {
            let inst_name = frozen.string(frozen.instances[inst_row].name).unwrap_or("");
            let p_names = &cell_pin_names[frozen.instances[inst_row].cell as usize];
            frozen.instance_pins(inst_row as u32).map(move |pin_row| {
                let cp = frozen.pin_cellpin[pin_row] as usize;
                hash_node_name(Some(inst_name), p_names[cp])
            })
        })
        .collect();
    // The instance pin ranges are consecutive, so `pin_hashes` is indexed from the first range.
    let pin_base = frozen.inst_pin_start.first().map_or(0, |&p| p as usize);
    for (inst_row, inst_rec) in frozen.instances.iter().enumerate() {
        let inst_name = frozen.string(inst_rec.name).unwrap_or("");
        let inst_str = strings.append_raw(inst_name);
        let cell_row = inst_rec.cell as usize;
        let cell_str = cell_str_ids[cell_row];
        let pin_range = frozen.instance_pins(inst_row as u32);
        let pin_node_begin = (n_port_bits + pin_range.start) as u32;
        let pin_count = pin_range.len() as u32;
        inst_hash.insert(inst_name, inst_str, cell_str, pin_node_begin, pin_count);

        let p_ids = &cell_pin_str_ids[cell_row];
        for pin_row in pin_range {
            let cp = frozen.pin_cellpin[pin_row] as usize;
            let pin_str = p_ids[cp];
            let node = NodeId((n_port_bits + pin_row) as u32);
            node_bucket[node.index()] =
                name_hash.insert(pin_hashes[pin_row - pin_base], node, inst_str, pin_str);
        }
    }
    drop(pin_hashes);

    clock.lap("store.names");
    // Disabled arcs.
    let mut disabled_arcs = Vec::new();
    let mut disabled_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for (ai, arc) in graph.arcs().iter().enumerate() {
        if let Some(reason) = graph.arc_disable_reason(ai) {
            disabled_arcs.push(DisabledArcRec {
                arc_idx: ai as u32,
                src_node: arc.src.0,
                dst_node: arc.dst.0,
                reason: DisabledArcRec::encode_reason(reason),
            });
            let r_str = match reason {
                DisableReason::LoopBreak => "loop_break",
                DisableReason::Constant => "constant",
                DisableReason::CaseAnalysis => "case_analysis",
                DisableReason::FalseWhen => "false_when",
                DisableReason::UserDisabled => "user_disabled",
            };
            *disabled_by_reason.entry(r_str.to_owned()).or_insert(0) += 1;
        }
    }

    clock.lap("store.disabled_arcs");
    // Collect constrained and unconstrained endpoints across corners.
    let first_ca = &analysis.corners[0];
    let mut constrained_nodes: Vec<NodeId> = Vec::new();
    let mut unconstrained = Vec::new();
    let mut unconstrained_by_reason: BTreeMap<String, usize> = BTreeMap::new();

    for (&node, ep_rec) in &first_ca.endpoints.endpoints {
        match ep_rec.status {
            EndpointStatus::Constrained => {
                constrained_nodes.push(node);
            }
            other => {
                let tag = match other {
                    EndpointStatus::Unclocked => UnconstrainedReasonTag::NoClock,
                    EndpointStatus::NoArrival => UnconstrainedReasonTag::NoArrival,
                    EndpointStatus::FalsePathOnly => UnconstrainedReasonTag::FalsePath,
                    EndpointStatus::Disabled => UnconstrainedReasonTag::ConstantPin,
                    EndpointStatus::UnconstrainedOutput | EndpointStatus::Constrained => {
                        UnconstrainedReasonTag::UncheckedOutput
                    }
                };
                unconstrained.push(UnconstrainedRec {
                    node: node.0,
                    reason: tag as u32,
                    reserved: node_bucket[node.index()],
                });
                *unconstrained_by_reason
                    .entry(other.as_str().to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    constrained_nodes.sort_by_key(|n| n.0);
    let n_endpoints = constrained_nodes.len();
    let mut endpoints = Vec::with_capacity(n_endpoints);
    for &node in &constrained_nodes {
        let ep_rec = &first_ca.endpoints.endpoints[&node];
        let sample = ep_rec.setup.as_ref().or(ep_rec.hold.as_ref());
        let clock_node = sample.and_then(|s| s.clock_pin).map_or(NO_INDEX, |n| n.0);
        let capture_clock = sample.map_or(NO_INDEX, |s| s.capture_clock.0);
        let path_group_str = sample.map_or(overall_group_str, |s| strings.intern(&s.path_group));
        let kind_tag = match sample.map(|s| s.kind) {
            Some(EndpointCheckKind::LatchSetup | EndpointCheckKind::LatchHold) => {
                EndpointKindTag::Latch
            }
            Some(EndpointCheckKind::OutputSetup | EndpointCheckKind::OutputHold) => {
                EndpointKindTag::OutputPort
            }
            Some(EndpointCheckKind::Recovery | EndpointCheckKind::Removal) => {
                EndpointKindTag::AsyncRecoveryRemoval
            }
            Some(EndpointCheckKind::ClockGatingSetup | EndpointCheckKind::ClockGatingHold) => {
                EndpointKindTag::ClockGate
            }
            _ => EndpointKindTag::FlipFlop,
        };
        let edge = sample.map(|s| s.capture_edge);
        let check_arc_idx = pin_index
            .checks_of(node)
            .first()
            .copied()
            .unwrap_or(NO_INDEX);
        endpoints.push(EndpointRec {
            data_node: node.0,
            clock_node,
            capture_clock,
            path_group_str,
            check_arc_idx,
            kind_flags: EndpointRec::pack_kind_flags(kind_tag, edge),
            worst_setup_rank: NO_INDEX,
            worst_hold_rank: NO_INDEX,
            name_bucket_plus1: EndpointRec::pack_name_bucket(Some(node_bucket[node.index()])),
        });
    }

    let mut ep_slack = vec![
        EpSlackRec {
            setup_slack: f64::NAN,
            hold_slack: f64::NAN,
        };
        n_corners * n_endpoints
    ];

    for (c_idx, ca) in analysis.corners.iter().enumerate() {
        for (ep_idx, &node) in constrained_nodes.iter().enumerate() {
            if let Some(ep_rec) = ca.endpoints.endpoints.get(&node) {
                ep_slack[c_idx * n_endpoints + ep_idx] = EpSlackRec {
                    setup_slack: ep_rec
                        .setup
                        .as_ref()
                        .map_or(f64::NAN, |s| canon_f64(s.slack)),
                    hold_slack: ep_rec
                        .hold
                        .as_ref()
                        .map_or(f64::NAN, |s| canon_f64(s.slack)),
                };
            }
        }
    }

    clock.lap("store.endpoints");
    // Run backward pass per corner to populate `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA`.
    let mut node_rec = Vec::new();
    let mut tag_extra_ptr = Vec::new();
    let mut tag_extra = Vec::new();

    if store_mode == StoreMode::Full {
        let total_cn = n_corners * n_nodes;
        for (c_idx, ca) in analysis.corners.iter().enumerate() {
            let bp = compute_corner_backward_pass(analysis, ca, lowered.and_then(|l| l.get(c_idx)));
            clock.lap("store.backward_pass");
            // `NODE_REC` is allocated once the first pass has released its per-tag required times,
            // so that the two never coexist (AGENTS rule 9).
            if c_idx == 0 {
                node_rec.resize(
                    total_cn,
                    NodeRec {
                        slew: [[0.0; 2]; 2],
                        required: ABSENT_REQUIRED,
                        slack: ABSENT_SLACK,
                        tag: NO_INDEX,
                        pad: NO_INDEX,
                        arrival: ABSENT_EARLY_LATE,
                        pred: [[NO_PRED; 2]; 2],
                    },
                );
                tag_extra_ptr.reserve(total_cn + 1);
                tag_extra_ptr.push(0);
            }
            let tv = &ca.node_values;
            let ntags = tv.ntags as usize;
            let base = c_idx * n_nodes;

            for v in 0..n_nodes {
                let node = NodeId(v as u32);
                let slew = [
                    [
                        canon_f64(tv.transition(v, 0, 0)),
                        canon_f64(tv.transition(v, 0, 1)),
                    ],
                    [
                        canon_f64(tv.transition(v, 1, 0)),
                        canon_f64(tv.transition(v, 1, 1)),
                    ],
                ];
                let required = [
                    [
                        canon_f64(bp.pin_required(node, Tr::Rise, An::Early)),
                        canon_f64(bp.pin_required(node, Tr::Rise, An::Late)),
                    ],
                    [
                        canon_f64(bp.pin_required(node, Tr::Fall, An::Early)),
                        canon_f64(bp.pin_required(node, Tr::Fall, An::Late)),
                    ],
                ];
                let slack = [
                    [
                        canon_f64(bp.pin_slack(node, Tr::Rise, An::Early)),
                        canon_f64(bp.pin_slack(node, Tr::Rise, An::Late)),
                    ],
                    [
                        canon_f64(bp.pin_slack(node, Tr::Fall, An::Early)),
                        canon_f64(bp.pin_slack(node, Tr::Fall, An::Late)),
                    ],
                ];

                let mut primary_tag = NO_INDEX;
                let mut primary_arr = ABSENT_EARLY_LATE;
                let mut primary_pred = [[NO_PRED; 2]; 2];

                for t in 0..ntags {
                    let arr = [
                        [
                            canon_f64(tv.arrival(v, t, 0, 0)),
                            canon_f64(tv.arrival(v, t, 0, 1)),
                        ],
                        [
                            canon_f64(tv.arrival(v, t, 1, 0)),
                            canon_f64(tv.arrival(v, t, 1, 1)),
                        ],
                    ];
                    let active = arr[0][0].is_finite()
                        || arr[0][1].is_finite()
                        || arr[1][0].is_finite()
                        || arr[1][1].is_finite();
                    if !active {
                        continue;
                    }
                    let pred = [
                        [tv.predecessor(v, t, 0, 0), tv.predecessor(v, t, 0, 1)],
                        [tv.predecessor(v, t, 1, 0), tv.predecessor(v, t, 1, 1)],
                    ];
                    if primary_tag == NO_INDEX {
                        primary_tag = t as u32;
                        primary_arr = arr;
                        primary_pred = pred;
                    } else {
                        tag_extra.push(TagExtraRec {
                            tag: t as u32,
                            pad: 0,
                            arrival: arr,
                            pred,
                        });
                    }
                }

                node_rec[base + v] = NodeRec {
                    slew,
                    required,
                    slack,
                    tag: primary_tag,
                    pad: node_bucket[v],
                    arrival: primary_arr,
                    pred: primary_pred,
                };
                tag_extra_ptr.push(tag_extra.len() as u32);
            }
        }
    }

    clock.lap("store.node_rec");
    // Build `EP_ORDER` (`[corner][check_kind: 0=setup, 1=hold]`, sorted by ascending slack).
    let mut ep_order = vec![0u32; n_corners * 2 * n_endpoints];
    for c_idx in 0..n_corners {
        let slack_base = c_idx * n_endpoints;
        for kind_i in 0..2 {
            let order_base = (c_idx * 2 + kind_i) * n_endpoints;
            let slice = &mut ep_order[order_base..order_base + n_endpoints];
            for (i, slot) in slice.iter_mut().enumerate() {
                *slot = i as u32;
            }
            slice.sort_by(|&a, &b| {
                let sa = if kind_i == 0 {
                    ep_slack[slack_base + a as usize].setup_slack
                } else {
                    ep_slack[slack_base + a as usize].hold_slack
                };
                let sb = if kind_i == 0 {
                    ep_slack[slack_base + b as usize].setup_slack
                } else {
                    ep_slack[slack_base + b as usize].hold_slack
                };
                cmp_slack_nan_last(sa, sb).then_with(|| a.cmp(&b))
            });
        }
    }

    clock.lap("store.ep_order");
    // Rank every endpoint's worst setup and worst hold record across corners (`STA-REQ-0228`).
    // Ranking needs the slacks only: the records are ranked first, and their paths are traced
    // afterwards in rank order, in parallel chunks, straight into `PATH_CHAIN`, so that no traced
    // path outlives its chunk (AGENTS Rule 9: bounded memory at OpenC910 scale, where every one of
    // millions of endpoints has a ranked path).
    struct CandPath<'r> {
        rec: &'r SlackRecord,
        slack: f64,
        corner_idx: usize,
        endpoint_idx: usize,
        check_kind: CheckKind,
        end_tr: Tr,
    }

    let mut cand_paths: Vec<CandPath<'_>> = Vec::new();
    for (c_idx, ca) in analysis.corners.iter().enumerate() {
        let corner_paths: Vec<CandPath<'_>> = constrained_nodes
            .par_iter()
            .enumerate()
            .flat_map_iter(|(ep_idx, node)| {
                ca.endpoints
                    .endpoints
                    .get(node)
                    .into_iter()
                    .flat_map(|ep_rec| [&ep_rec.setup, &ep_rec.hold].into_iter().flatten())
                    .map(move |rec| CandPath {
                        rec,
                        slack: canon_f64(rec.slack),
                        corner_idx: c_idx,
                        endpoint_idx: ep_idx,
                        check_kind: match rec.kind {
                            EndpointCheckKind::Recovery => CheckKind::Recovery,
                            EndpointCheckKind::Removal => CheckKind::Removal,
                            k if k.is_setup_family() => CheckKind::Setup,
                            _ => CheckKind::Hold,
                        },
                        end_tr: rec.data_tr,
                    })
            })
            .collect();
        cand_paths.extend(corner_paths);
    }

    // Setup-first ranking (`STA-REQ-0228`): all setup-family paths sorted by ascending slack,
    // then all hold-family paths sorted by ascending slack. The key is unique per record, so the
    // order is total.
    cand_paths.par_sort_by(|a, b| {
        a.check_kind
            .is_hold()
            .cmp(&b.check_kind.is_hold())
            .then_with(|| a.slack.total_cmp(&b.slack))
            .then_with(|| a.corner_idx.cmp(&b.corner_idx))
            .then_with(|| a.endpoint_idx.cmp(&b.endpoint_idx))
            .then_with(|| a.end_tr.index().cmp(&b.end_tr.index()))
    });
    clock.lap("store.rank_records");

    let n_ranked = cand_paths.len();
    let n_top_k = n_ranked.min(top_k_paths);
    let mut path_rank = Vec::with_capacity(n_ranked);
    for (r_idx, cp) in cand_paths.iter().enumerate() {
        path_rank.push(PathRankRec::new(
            cp.slack as f32,
            cp.endpoint_idx,
            cp.corner_idx,
            cp.check_kind,
            cp.end_tr,
            r_idx < n_top_k,
        ));
        let ep = &mut endpoints[cp.endpoint_idx];
        if cp.check_kind.is_setup() {
            if ep.worst_setup_rank == NO_INDEX {
                ep.worst_setup_rank = r_idx as u32;
            }
        } else if ep.worst_hold_rank == NO_INDEX {
            ep.worst_hold_rank = r_idx as u32;
        }
    }

    // `PATH_CHAIN`: the header and the steps of every ranked path, in rank order. A chunk of
    // paths is traced in parallel and appended in order, so the words do not depend on the
    // number of threads.
    const TRACE_CHUNK: usize = 1 << 16;
    let mut path_chain_ptr = Vec::with_capacity(n_ranked + 1);
    path_chain_ptr.push(0);
    let mut path_chain: Vec<u32> = Vec::new();
    for chunk in cand_paths.chunks(TRACE_CHUNK) {
        let traced: Vec<Vec<u32>> = chunk
            .par_iter()
            .map(|cp| {
                let ca = &analysis.corners[cp.corner_idx];
                let check_value = compute_check_intrinsic(analysis, ca, &pin_index, cp.rec);
                let mut words = Vec::with_capacity(PATH_CHAIN_HEADER_WORDS + 16 * 3);
                words.extend_from_slice(&pack_f64_words(canon_f64(cp.rec.required)));
                words.extend_from_slice(&pack_f64_words(canon_f64(check_value)));
                for_each_path_step(
                    analysis,
                    ca,
                    &pin_index,
                    &node_bucket,
                    cp.rec,
                    |node, tr, kind, tag_slot, nb, _, _| {
                        words.extend_from_slice(&pack_chain_step(node, tr, kind, tag_slot, nb));
                    },
                );
                words
            })
            .collect();
        for words in traced {
            path_chain.extend_from_slice(&words);
            path_chain_ptr.push(path_chain.len() as u64);
        }
    }
    clock.lap("store.trace_paths");

    // `PATHS` and `PATH_POINTS`: the top-K paths with their arrivals and slews.
    let mut paths = Vec::with_capacity(n_top_k);
    let mut path_points = Vec::new();
    for (r_idx, cp) in cand_paths.iter().take(n_top_k).enumerate() {
        let ca = &analysis.corners[cp.corner_idx];
        let check_value = compute_check_intrinsic(analysis, ca, &pin_index, cp.rec);
        let point_begin = path_points.len() as u32;
        let mut prev_arr = 0.0f64;
        let mut first = true;
        for_each_path_step(
            analysis,
            ca,
            &pin_index,
            &node_bucket,
            cp.rec,
            |node, tr, kind, _, _, arr, slew| {
                let incr = if first { arr } else { arr - prev_arr };
                first = false;
                prev_arr = arr;
                path_points.push(PathPointRec {
                    arrival: canon_f64(arr),
                    incr_delay: canon_f64(incr),
                    slew: canon_f64(slew),
                    node: node.0,
                    tr: tr.index() as u8,
                    kind: kind as u8,
                    pad: 0,
                });
            },
        );
        let point_end = path_points.len() as u32;
        paths.push(PathHeaderRec {
            slack: cp.slack,
            arrival: canon_f64(cp.rec.arrival),
            required: canon_f64(cp.rec.required),
            check_value: canon_f64(check_value),
            rank_idx: r_idx as u32,
            endpoint_idx: cp.endpoint_idx as u32,
            launch_clock: cp.rec.launch_clock.0,
            capture_clock: cp.rec.capture_clock.0,
            point_begin,
            point_end,
        });
    }

    clock.lap("store.rank_paths");
    // Build `SUMMARY` (`num_corners * n_path_groups`).
    let mut summary = Vec::with_capacity(n_corners * n_path_groups);
    for c_idx in 0..n_corners {
        let slack_base = c_idx * n_endpoints;
        for &g_str in &path_group_strs {
            let is_overall = g_str == overall_group_str;
            let mut setup_wns = 0.0f64;
            let mut setup_tns = 0.0f64;
            let mut setup_violating = 0u32;
            let mut setup_endpoints = 0u32;
            let mut hold_wns = 0.0f64;
            let mut hold_tns = 0.0f64;
            let mut hold_violating = 0u32;
            let mut hold_endpoints = 0u32;

            for (ep_idx, ep) in endpoints.iter().enumerate() {
                if !is_overall && ep.path_group_str != g_str {
                    continue;
                }
                let sl = &ep_slack[slack_base + ep_idx];
                if sl.setup_slack.is_finite() {
                    setup_endpoints += 1;
                    if sl.setup_slack < 0.0 {
                        setup_wns = setup_wns.min(sl.setup_slack);
                        setup_tns += sl.setup_slack;
                        setup_violating += 1;
                    }
                }
                if sl.hold_slack.is_finite() {
                    hold_endpoints += 1;
                    if sl.hold_slack < 0.0 {
                        hold_wns = hold_wns.min(sl.hold_slack);
                        hold_tns += sl.hold_slack;
                        hold_violating += 1;
                    }
                }
            }

            summary.push(SummaryRec {
                setup_wns: canon_f64(setup_wns),
                setup_tns: canon_f64(setup_tns),
                hold_wns: canon_f64(hold_wns),
                hold_tns: canon_f64(hold_tns),
                corner_idx: c_idx as u32,
                group_str: g_str,
                setup_violating,
                setup_endpoints,
                hold_violating,
                hold_endpoints,
                max_trans_violating: 0,
                max_cap_violating: 0,
                total_endpoints: n_endpoints as u32,
                unconstrained_endpoints: unconstrained.len() as u32,
                reserved0: 0,
                reserved1: 0,
            });
        }
    }

    // Build `NOTES` from `analysis.diags`.
    let mut notes = Vec::with_capacity(analysis.diags.len());
    for d in analysis.diags {
        let code_str = strings.intern(&d.code);
        let message_str = strings.intern(&d.message);
        let sev = match d.severity {
            core_diag::Severity::Note => NoteSeverity::Note as u32,
            _ => NoteSeverity::Warning as u32,
        };
        notes.push(NoteRec {
            code_str,
            message_str,
            severity: sev,
            count: 1,
        });
    }

    let unconstrained_inputs = count_unconstrained_inputs(analysis);
    // Unanalyzed library arcs by timing type, the worst corner of each type (`STA-REQ-0015`).
    let mut unanalyzed_by_type: BTreeMap<String, usize> = BTreeMap::new();
    let frozen = analysis.netview.frozen();
    for ca in analysis.corners {
        let mut corner_counts: BTreeMap<String, usize> = BTreeMap::new();
        for inst in frozen.instances.iter() {
            let Ok(cell_name) = frozen.string(frozen.cells[inst.cell as usize]) else {
                continue;
            };
            if let Some(tc) = ca.lib.cell(cell_name) {
                for (timing_type, &n) in &tc.unanalyzed_by_type {
                    *corner_counts.entry(timing_type.clone()).or_insert(0) += n;
                }
            }
        }
        for (timing_type, n) in corner_counts {
            let worst = unanalyzed_by_type.entry(timing_type).or_insert(0);
            *worst = (*worst).max(n);
        }
    }

    let wire_scope = analysis.corners[0].wire_model.scope();
    let mut exceptions_applied = Vec::new();
    if let Some(wl_name) = &wire_scope.wire_load_model {
        exceptions_applied.push(format!("wire_load:{wl_name}"));
    }
    if unconstrained_inputs > 0 {
        exceptions_applied.push(format!("unconstrained_inputs:{unconstrained_inputs}"));
    }

    let scope_report = ScopeReport {
        top: nv.module_name().to_owned(),
        corners: corner_names,
        wire_model: wire_scope.wire_model.to_owned(),
        clock_model: analysis.corners[0].clock_model.scope_name().to_owned(),
        cppr: String::from("off"),
        backend: backend_name.to_owned(),
        precision,
        store_mode,
        exceptions_applied,
        completeness: CompletenessReport {
            analyzed_endpoints: n_endpoints,
            unconstrained_endpoints: unconstrained.len(),
            unconstrained_by_reason,
            disabled_arcs_by_reason: disabled_by_reason,
            unanalyzed_arcs_by_type: unanalyzed_by_type,
        },
    };
    let scope_bytes = serde_json::to_vec(&scope_report).unwrap_or_else(|_| b"{}".to_vec());

    let (str_bytes, str_offs) = strings.finish();
    let meta = StaMeta {
        num_nodes: n_nodes as u32,
        num_port_bits: n_port_bits as u32,
        num_arcs: graph.arcs().len() as u32,
        num_cells: n_instances as u32,
        num_startpoints: n_port_bits as u32,
        num_disabled_arcs: disabled_arcs.len() as u32,
        num_corners: n_corners as u32,
        num_clocks: clock_recs.len() as u32,
        num_tags: tag_recs.len() as u32,
        num_endpoints: n_endpoints as u32,
        num_ranked_paths: n_ranked as u32,
        num_top_k_paths: n_top_k as u32,
        num_path_groups: n_path_groups as u32,
        store_mode: store_mode.as_u32(),
        backend_str,
        precision: precision as u32,
    };

    clock.lap("store.summary_scope");
    let mut data = StaStoreData {
        str_bytes,
        str_offs,
        meta: [meta],
        corners: corner_recs,
        clocks: clock_recs,
        tags: tag_recs,
        disabled_arcs,
        node_rec,
        tag_extra_ptr,
        tag_extra,
        endpoints,
        ep_slack,
        ep_order,
        path_rank,
        path_chain_ptr,
        path_chain,
        paths,
        path_points,
        name_hash: name_hash.finish(),
        inst_hash: inst_hash.finish(),
        summary,
        unconstrained,
        notes,
        scope: scope_bytes,
    };
    data.canonicalize();
    clock.lap("store.canonicalize");
    data
}

fn count_unconstrained_inputs(analysis: &StoreInput<'_>) -> u32 {
    let mut clock_sources = BTreeSet::new();
    for ck in &analysis.constraints.clocks {
        for &src in &ck.sources {
            clock_sources.insert(src);
        }
    }
    let mut input_delayed = BTreeSet::new();
    for id in &analysis.constraints.input_delays {
        input_delayed.insert(id.node);
    }
    let mut count = 0u32;
    for pb in 0..analysis.graph.num_port_bits() {
        let nid = NodeId(pb as u32);
        if let NodeKind::PortBit { dir, .. } = analysis.netview.node_kind(nid)
            && matches!(dir, Direction::Input | Direction::Inout)
            && !clock_sources.contains(&nid)
            && !input_delayed.contains(&nid)
        {
            count += 1;
        }
    }
    count
}

/// The required times of corner `ca`: seeded at its checks and propagated backward over the
/// lowered problem of the corner, `kept` when the analysis kept it, else lowered again.
fn compute_corner_backward_pass(
    analysis: &StoreInput<'_>,
    ca: &CornerAnalysis,
    kept: Option<&LoweredCorner<'_>>,
) -> BackwardPass {
    let n_nodes = analysis.graph.num_nodes();
    let ntags = ca.tags.len().max(1);
    let mut bp = BackwardPass::required_and_slack_only(n_nodes, ntags);

    for rec in &ca.endpoints.records {
        let an = rec.kind.data_bound();
        bp.seed_required(rec.endpoint, rec.tag.index(), rec.data_tr, an, rec.required);
    }
    // The node-ordered values of the corner hold the propagated arrivals and transitions, so the
    // pass does not need the level-ordered result, which a caller may already have released.
    let values = ByNode {
        values: &ca.node_values,
        order: analysis.graph.order(),
    };
    if let Some(lowered) = kept {
        bp.propagate(analysis.graph, lowered, &values);
        return bp;
    }

    if let Ok(start_build) = build_start_rows(
        analysis.netview,
        analysis.graph,
        &ca.lib,
        analysis.constraints,
        &ca.clock_model,
        &ca.tags,
        &ca.exceptions,
        &ca.net_loads,
        &NoDerate,
    ) && let Ok(mut lowered) = LoweredCorner::lower(
        analysis.netview,
        analysis.graph,
        &ca.lib,
        &ca.net_loads,
        ntags as u32,
        start_build.rows,
        &ca.latch_maps,
    ) {
        lowered.set_wire_model(analysis.graph, &ca.wire_model);
        bp.propagate(analysis.graph, &lowered, &values);
    }
    bp
}

fn cmp_slack_nan_last(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.total_cmp(&b),
    }
}
