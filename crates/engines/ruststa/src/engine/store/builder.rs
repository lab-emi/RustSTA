// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Deterministic builder for `STA v2` (`sta.rbin`) from `NetlistView`, `TimingGraph`,
//! `TimingLib`, `IdealSchedule`, and `LoweredCorner` / `LoweredResult` (`owner: E10`;
//! `STA-REQ-0302`, `STA-REQ-0312` to `STA-REQ-0315`, `STA-REQ-0401`, `STA-REQ-0405`,
//! `WS-REQ-0030` to `WS-REQ-0038`).

use std::collections::BTreeMap;

use rayon::prelude::*;
use rusteda_compute::timing::{TimingValues, absent};
use rusteda_core::job::Ctx;
use rusteda_core::liberty::Direction;

use crate::engine::delay::load::{NetLoadOverrides, NetLoads};
use crate::engine::delay::wire::Ideal;
use crate::engine::graph::{CheckKind, DisableReason, TimingGraph};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::netview::{NetlistView, NodeKind};
use crate::engine::prop::lower::{
    BackendKind, IdealSchedule, LoweredCorner, LoweredResult, Precision,
};
use crate::engine::store::hash::{
    InstHashBuilder, NameHashBuilder, StringTableBuilder, hash_node_name,
};
use crate::engine::store::layout::{
    ABSENT_EARLY_LATE, ABSENT_REQUIRED, ABSENT_SLACK, ClockRec, CompletenessReport, CornerRec,
    DisabledArcRec, EndpointKindTag, EndpointRec, EpSlackRec, NO_INDEX, NO_PRED, NodeRec, NoteRec,
    NoteSeverity, PathHeaderRec, PathPointKind, PathPointRec, PathRankRec, ScopeReport, StaMeta,
    StoreMode, StorePrecision, SummaryRec, TagExtraRec, TagRec, UnconstrainedReasonTag,
    UnconstrainedRec, canon_f32, canon_f64, canon_grid, pack_chain_step, pack_f64_words,
};
use crate::engine::store::writer::StaStoreData;
use crate::engine::types::{An, NodeId, Tr};

/// Options controlling `sta.rbin` construction.
#[derive(Clone, Debug)]
pub struct StaStoreBuildOptions {
    /// Storage mode (`Full` or `Summary`).
    pub store_mode: StoreMode,
    /// Number of worst paths materialized in `PATHS` and `PATH_POINTS` (default `100`).
    pub top_k_paths: usize,
    /// Maximum number of ranked paths recorded in `PATH_RANK` and `PATH_CHAIN` (default `10_000`).
    pub path_limit: usize,
    /// Backend name recorded in `META` and `SCOPE` (default `"cpu"`).
    pub backend_name: String,
    /// Propagation precision recorded in `META` and `SCOPE` (default `StorePrecision::F64`).
    pub precision: StorePrecision,
    /// Wire model name recorded in `SCOPE` (default `"ideal"`).
    pub wire_model: String,
    /// Clock model name recorded in `SCOPE` (default `"ideal"`).
    pub clock_model: String,
    /// CPPR mode recorded in `SCOPE` (default `"off"`).
    pub cppr: String,
}

impl Default for StaStoreBuildOptions {
    fn default() -> Self {
        Self {
            store_mode: StoreMode::Full,
            top_k_paths: 100,
            path_limit: 10_000,
            backend_name: String::from("cpu"),
            precision: StorePrecision::F64,
            wire_model: String::from("ideal"),
            clock_model: String::from("ideal"),
            cppr: String::from("off"),
        }
    }
}

/// Per-corner input to [`build_sta_store`].
#[derive(Clone, Copy, Debug)]
pub struct CornerBuildInput<'a> {
    /// Operating corner name (`"default"`, `"slow"`, `"fast"`, ...).
    pub name: &'a str,
    /// Number of Liberty libraries in this corner.
    pub lib_count: u32,
    /// Compiled library view for this corner.
    pub tlib: &'a TimingLib,
    /// Lowered corner problem.
    pub lowered: &'a LoweredCorner<'a>,
    /// Propagated values for this corner.
    pub result: &'a LoweredResult,
}

#[derive(Clone, Debug)]
struct CandidatePath {
    slack: f64,
    arrival: f64,
    required: f64,
    check_value: f64,
    corner_idx: usize,
    endpoint_idx: usize,
    check_kind: CheckKind,
    end_tr: Tr,
    launch_clock: u32,
    capture_clock: u32,
    steps: Vec<(NodeId, Tr, PathPointKind, usize, u32, f64, f64)>,
}

/// Runs forward CPU propagation and builds a complete [`StaStoreData`] for a single corner using
/// `IdealSchedule::default_for_netlist` when `schedule` is `None`.
pub fn build_single_corner_store(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    tlib: &TimingLib,
    schedule: Option<&IdealSchedule>,
    options: &StaStoreBuildOptions,
) -> Result<StaStoreData, String> {
    let owned_sched;
    let sched = match schedule {
        Some(s) => s,
        None => {
            owned_sched = IdealSchedule::default_for_netlist(nv);
            &owned_sched
        }
    };
    let loads = NetLoads::compute(nv.frozen(), tlib, &Ideal, &NetLoadOverrides::default());
    let lowered = sched
        .lower(nv, graph, tlib, &loads)
        .map_err(|d| d.to_string())?;
    let ctx = Ctx::new(None, None);
    let result = lowered
        .propagate(BackendKind::Cpu, Precision::F64, ctx)
        .map_err(|e| e.to_string())?;
    let corner_input = CornerBuildInput {
        name: &tlib.corner,
        lib_count: 1,
        tlib,
        lowered: &lowered,
        result: &result,
    };
    Ok(build_sta_store(nv, graph, sched, &[corner_input], options))
}

/// Builds a deterministic [`StaStoreData`] across all `corners` (`STA-REQ-0302`, `STA-REQ-0405`).
#[must_use]
pub fn build_sta_store(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    schedule: &IdealSchedule,
    corners: &[CornerBuildInput<'_>],
    options: &StaStoreBuildOptions,
) -> StaStoreData {
    let frozen = nv.frozen();
    let n_nodes = graph.num_nodes();
    let n_port_bits = graph.num_port_bits();
    let n_instances = frozen.instances.len();
    let n_cells = frozen.cells.len();
    let n_corners = corners.len();

    // 1. Build string table, NAME_HASH, and INST_HASH without allocating per-pin `"inst/pin"`
    // strings on the heap.
    let mut strings = StringTableBuilder::new();
    strings.reserve(
        n_port_bits + n_instances + n_cells + 64,
        (n_port_bits + n_instances) * 10 + 1024,
    );

    let backend_str = strings.intern(&options.backend_name);
    let overall_group_str = strings.intern("**");
    let unclocked_group_str = strings.intern("<unclocked>");

    let mut corner_recs = Vec::with_capacity(n_corners);
    let mut corner_names = Vec::with_capacity(n_corners);
    for c in corners {
        let name_str = strings.intern(c.name);
        corner_names.push(c.name.to_owned());
        corner_recs.push(CornerRec {
            name_str,
            lib_count: c.lib_count,
            flags: 0,
            reserved: 0,
        });
    }

    let mut clock_recs = Vec::with_capacity(schedule.clocks.len());
    let mut clock_group_strs = Vec::with_capacity(schedule.clocks.len());
    for ck in &schedule.clocks {
        let name_str = strings.intern(&ck.name);
        clock_group_strs.push(name_str);
        let source_node = ck.sources.first().map_or(NO_INDEX, |n| n.0);
        let flags = u32::from(ck.sources.is_empty());
        clock_recs.push(ClockRec {
            name_str,
            source_node,
            master_clock: NO_INDEX,
            flags,
            period_ns: canon_f64(ck.period),
            rise_edge_ns: canon_f64(ck.waveform[0]),
            fall_edge_ns: canon_f64(ck.waveform[1]),
            uncertainty_setup_ns: canon_f32(ck.uncertainty[0] as f32),
            uncertainty_hold_ns: canon_f32(ck.uncertainty[1] as f32),
        });
    }

    // Path groups: group 0 is "**" (overall), followed by each clock's path group.
    let mut path_group_strs = vec![overall_group_str];
    for &gs in &clock_group_strs {
        if !path_group_strs.contains(&gs) {
            path_group_strs.push(gs);
        }
    }
    let n_path_groups = path_group_strs.len();

    // Build TagRec table (`2 * clocks.len().max(1)`).
    let n_tags = corners.first().map_or_else(
        || 2 * schedule.clocks.len().max(1),
        |c| c.lowered.ntags as usize,
    );
    let mut tag_recs = Vec::with_capacity(n_tags);
    for t in 0..n_tags {
        let clk_idx = t / 2;
        let edge = (t % 2) as u8;
        let (clock_id, source_node, group_str) = if clk_idx < schedule.clocks.len() {
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
            launch_edge: edge,
            flags: 0,
            exc_state: 0,
            launch_source_node: source_node,
            path_group_str: group_str,
        });
    }

    // Intern cell names and cell pin names once per cell type.
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
        let p_names = &cell_pin_names[cell_row];
        for pin_row in pin_range {
            let cp = frozen.pin_cellpin[pin_row] as usize;
            let pin_name = p_names[cp];
            let pin_str = p_ids[cp];
            let node = NodeId((n_port_bits + pin_row) as u32);
            let h = hash_node_name(Some(inst_name), pin_name);
            node_bucket[node.index()] = name_hash.insert(h, node, inst_str, pin_str);
        }
    }

    // 2. Disabled arcs (`DISABLED_ARCS`).
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

    // 3. Remap each corner's `LoweredResult` into `NodeId` order (`0..n_nodes`) and compute
    // backward required times.
    let mut corner_node_vals: Vec<TimingValues> = Vec::with_capacity(n_corners);
    for c in corners {
        corner_node_vals.push(c.result.to_node_order(graph));
    }

    // 4. Identify endpoints (`ENDPOINTS`) and unconstrained endpoints (`UNCONSTRAINED`).
    // Group CheckArcs by `(data_pin, clock_pin, inst)` so a flip-flop D pin with setup+hold has
    // one EndpointRec entry sorted by `(data_node, check_arc_idx, capture_clock)`.
    let default_clk_id = if clock_recs.is_empty() { NO_INDEX } else { 0 };
    let default_group_str = clock_group_strs
        .first()
        .copied()
        .unwrap_or(overall_group_str);
    let default_period = clock_recs.first().map_or(1.5, |c| c.period_ns);
    let default_setup_unc = clock_recs
        .first()
        .map_or(0.0, |c| f64::from(c.uncertainty_setup_ns));
    let default_hold_unc = clock_recs
        .first()
        .map_or(0.0, |c| f64::from(c.uncertainty_hold_ns));

    #[derive(Clone, Debug)]
    struct RawEndpoint {
        data_node: NodeId,
        clock_node: NodeId,
        capture_clock: u32,
        path_group_str: u32,
        setup_check_idx: Option<usize>,
        hold_check_idx: Option<usize>,
        kind: EndpointKindTag,
        edge: Option<Tr>,
    }

    let mut raw_endpoints: Vec<RawEndpoint> = Vec::new();
    let mut ep_map: BTreeMap<(u32, u32, u8), usize> = BTreeMap::new();

    for (ci, ch) in graph.checks().iter().enumerate() {
        let kind_tag = match ch.kind {
            CheckKind::Setup | CheckKind::Hold => {
                // Check if this instance has a LatchData arc from data_pin.
                EndpointKindTag::FlipFlop
            }
            CheckKind::Recovery | CheckKind::Removal => EndpointKindTag::AsyncRecoveryRemoval,
        };
        let key = (ch.data_pin.0, ch.clock_pin.0, kind_tag as u8);
        let idx = *ep_map.entry(key).or_insert_with(|| {
            let i = raw_endpoints.len();
            raw_endpoints.push(RawEndpoint {
                data_node: ch.data_pin,
                clock_node: ch.clock_pin,
                capture_clock: default_clk_id,
                path_group_str: default_group_str,
                setup_check_idx: None,
                hold_check_idx: None,
                kind: kind_tag,
                edge: Some(ch.edge),
            });
            i
        });
        if ch.kind.is_setup() {
            raw_endpoints[idx].setup_check_idx = Some(ci);
        } else {
            raw_endpoints[idx].hold_check_idx = Some(ci);
        }
    }

    // For purely combinational netlists (or output ports with arrivals when no sequential checks
    // exist on the design), include output ports as endpoints so every corpus netlist has checked
    // endpoints, slacks, and paths.
    if raw_endpoints.is_empty() {
        for pb in 0..n_port_bits {
            let node = NodeId(pb as u32);
            if let NodeKind::PortBit { dir, .. } = nv.node_kind(node)
                && matches!(dir, Direction::Output | Direction::Inout)
            {
                raw_endpoints.push(RawEndpoint {
                    data_node: node,
                    clock_node: NodeId(NO_INDEX),
                    capture_clock: default_clk_id,
                    path_group_str: default_group_str,
                    setup_check_idx: None,
                    hold_check_idx: None,
                    kind: EndpointKindTag::OutputPort,
                    edge: Some(Tr::Rise),
                });
            }
        }
    }

    // Separate constrained vs unconstrained endpoints (`NoArrival`).
    let mut constrained_raw: Vec<RawEndpoint> = Vec::with_capacity(raw_endpoints.len());
    let mut unconstrained = Vec::new();
    let mut unconstrained_by_reason: BTreeMap<String, usize> = BTreeMap::new();

    for rep in raw_endpoints {
        let v = rep.data_node.index();
        let has_arrival = corner_node_vals.iter().any(|tv| {
            let ntags = tv.ntags as usize;
            (0..ntags)
                .any(|t| (0..2).any(|tr| (0..2).any(|an| tv.arrival(v, t, tr, an).is_finite())))
        });
        if has_arrival {
            constrained_raw.push(rep);
        } else {
            unconstrained.push(UnconstrainedRec {
                node: rep.data_node.0,
                reason: UnconstrainedReasonTag::NoArrival as u32,
                reserved: node_bucket[v],
            });
            *unconstrained_by_reason
                .entry(String::from("no_arrival"))
                .or_insert(0) += 1;
        }
    }

    constrained_raw.sort_by_key(|r| {
        (
            r.data_node.0,
            r.setup_check_idx
                .or(r.hold_check_idx)
                .map_or(NO_INDEX, |i| i as u32),
            r.capture_clock,
        )
    });

    let n_endpoints = constrained_raw.len();
    let mut endpoints: Vec<EndpointRec> = constrained_raw
        .iter()
        .map(|r| {
            let check_arc_idx = r
                .setup_check_idx
                .or(r.hold_check_idx)
                .map_or(NO_INDEX, |i| i as u32);
            EndpointRec {
                data_node: r.data_node.0,
                clock_node: r.clock_node.0,
                capture_clock: r.capture_clock,
                path_group_str: r.path_group_str,
                check_arc_idx,
                kind_flags: EndpointRec::pack_kind_flags(r.kind, r.edge),
                worst_setup_rank: NO_INDEX,
                worst_hold_rank: NO_INDEX,
                name_bucket_plus1: EndpointRec::pack_name_bucket(Some(
                    node_bucket[r.data_node.index()],
                )),
            }
        })
        .collect();

    // 5. Build per-(corner, node) `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA`, plus backward
    // required times!
    // Also build a lookup table of which tag_slot `(0 = primary, 1.. = extra)` each `(corner,
    // node, tag)` maps to so `PATH_CHAIN` can encode `tag_slot` in O(1).
    let mut node_rec = Vec::new();
    let mut tag_extra_ptr = Vec::new();
    let mut tag_extra = Vec::new();

    // Per corner: `node_required[corner][node][tr][an]`
    let mut corner_required: Vec<Vec<[[f64; 2]; 2]>> =
        vec![vec![ABSENT_REQUIRED; n_nodes]; n_corners];

    // Evaluate endpoints per corner in parallel over `0..n_endpoints`.
    let mut ep_slack = vec![
        EpSlackRec {
            setup_slack: f64::NAN,
            hold_slack: f64::NAN,
        };
        n_corners * n_endpoints
    ];
    let mut candidate_paths: Vec<CandidatePath> = Vec::new();

    for (c_idx, c_in) in corners.iter().enumerate() {
        let tv = &corner_node_vals[c_idx];
        let ntags = tv.ntags as usize;

        let eval_one_ep = |(ep_idx, rep): (usize, &RawEndpoint)| {
            let v = rep.data_node.index();
            let clk_slew_rise = if rep.clock_node.0 != NO_INDEX {
                let cv = rep.clock_node.index();
                let s = tv.transition(cv, Tr::Rise.index(), An::Late.index());
                if s.is_finite() { s } else { 0.03 }
            } else {
                0.03
            };

            let mut best_setup: Option<(f64, f64, f64, f64, usize, Tr)> = None;
            let mut best_hold: Option<(f64, f64, f64, f64, usize, Tr)> = None;
            let mut node_req = ABSENT_REQUIRED;

            for tr in Tr::ALL {
                let tr_i = tr.index();
                // Setup (Late)
                let data_slew_late = tv.transition(v, tr_i, An::Late.index());
                let setup_check_val = if let Some(chk_i) = rep.setup_check_idx {
                    let ch = &graph.checks()[chk_i];
                    let cell_row = frozen.instances[ch.inst as usize].cell;
                    let cell_name = nv.cell_name(cell_row);
                    if let Some(tc) = c_in.tlib.cell(cell_name)
                        && let Some(cc) = tc.checks.get(ch.lib_arc.index())
                    {
                        c_in.tlib.eval_check(
                            cc,
                            tr,
                            clk_slew_rise,
                            if data_slew_late.is_finite() {
                                data_slew_late
                            } else {
                                0.0
                            },
                            0.0,
                        )
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let req_late = canon_f64(default_period - default_setup_unc - setup_check_val);
                node_req[tr_i][An::Late.index()] = req_late;

                for t in 0..ntags {
                    let arr = tv.arrival(v, t, tr_i, An::Late.index());
                    if arr.is_finite() {
                        let sl = canon_f64(req_late - arr);
                        if best_setup.is_none_or(|(b_sl, _, _, _, _, _)| sl < b_sl) {
                            best_setup = Some((sl, arr, req_late, setup_check_val, t, tr));
                        }
                    }
                }

                // Hold (Early)
                let data_slew_early = tv.transition(v, tr_i, An::Early.index());
                let hold_check_val = if let Some(chk_i) = rep.hold_check_idx {
                    let ch = &graph.checks()[chk_i];
                    let cell_row = frozen.instances[ch.inst as usize].cell;
                    let cell_name = nv.cell_name(cell_row);
                    if let Some(tc) = c_in.tlib.cell(cell_name)
                        && let Some(cc) = tc.checks.get(ch.lib_arc.index())
                    {
                        c_in.tlib.eval_check(
                            cc,
                            tr,
                            clk_slew_rise,
                            if data_slew_early.is_finite() {
                                data_slew_early
                            } else {
                                0.0
                            },
                            0.0,
                        )
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let req_early = canon_f64(default_hold_unc + hold_check_val);
                node_req[tr_i][An::Early.index()] = req_early;

                for t in 0..ntags {
                    let arr = tv.arrival(v, t, tr_i, An::Early.index());
                    if arr.is_finite() {
                        let sl = canon_f64(arr - req_early);
                        if best_hold.is_none_or(|(b_sl, _, _, _, _, _)| sl < b_sl) {
                            best_hold = Some((sl, arr, req_early, hold_check_val, t, tr));
                        }
                    }
                }
            }

            (ep_idx, rep.data_node, node_req, best_setup, best_hold)
        };

        let ep_results: Vec<_> = if n_endpoints >= 512 {
            constrained_raw
                .par_iter()
                .enumerate()
                .map(eval_one_ep)
                .collect()
        } else {
            constrained_raw
                .iter()
                .enumerate()
                .map(eval_one_ep)
                .collect()
        };

        let req_vec = &mut corner_required[c_idx];
        for &(ep_idx, data_node, node_req, best_setup, best_hold) in &ep_results {
            let v = data_node.index();
            for tr_i in 0..2 {
                req_vec[v][tr_i][An::Late.index()] =
                    req_vec[v][tr_i][An::Late.index()].min(node_req[tr_i][An::Late.index()]);
                req_vec[v][tr_i][An::Early.index()] =
                    req_vec[v][tr_i][An::Early.index()].max(node_req[tr_i][An::Early.index()]);
            }
            ep_slack[c_idx * n_endpoints + ep_idx] = EpSlackRec {
                setup_slack: best_setup.map_or(f64::NAN, |x| canon_f64(x.0)),
                hold_slack: best_hold.map_or(f64::NAN, |x| canon_f64(x.0)),
            };
        }

        // Propagate required times backward in reverse level order across enabled CSR arcs.
        propagate_backward_required(graph, c_in.tlib, c_in.lowered, tv, req_vec);

        // Trace worst setup and hold paths for each endpoint.
        for (ep_idx, _, _, best_setup, best_hold) in ep_results {
            let rep = &constrained_raw[ep_idx];
            let setup_kind = if rep.kind == EndpointKindTag::AsyncRecoveryRemoval {
                CheckKind::Recovery
            } else {
                CheckKind::Setup
            };
            let hold_kind = if rep.kind == EndpointKindTag::AsyncRecoveryRemoval {
                CheckKind::Removal
            } else {
                CheckKind::Hold
            };
            if let Some((sl, arr, req, chk_val, tag, end_tr)) = best_setup {
                let steps =
                    trace_path_steps(nv, tv, &node_bucket, rep.data_node, tag, end_tr, An::Late);
                let launch_clock = tag_recs
                    .get(tag)
                    .map_or(rep.capture_clock, |tr_rec| tr_rec.clock_id);
                candidate_paths.push(CandidatePath {
                    slack: sl,
                    arrival: canon_f64(arr),
                    required: canon_f64(req),
                    check_value: canon_f64(chk_val),
                    corner_idx: c_idx,
                    endpoint_idx: ep_idx,
                    check_kind: setup_kind,
                    end_tr,
                    launch_clock,
                    capture_clock: rep.capture_clock,
                    steps,
                });
            }
            if let Some((sl, arr, req, chk_val, tag, end_tr)) = best_hold {
                let steps =
                    trace_path_steps(nv, tv, &node_bucket, rep.data_node, tag, end_tr, An::Early);
                let launch_clock = tag_recs
                    .get(tag)
                    .map_or(rep.capture_clock, |tr_rec| tr_rec.clock_id);
                candidate_paths.push(CandidatePath {
                    slack: sl,
                    arrival: canon_f64(arr),
                    required: canon_f64(req),
                    check_value: canon_f64(chk_val),
                    corner_idx: c_idx,
                    endpoint_idx: ep_idx,
                    check_kind: hold_kind,
                    end_tr,
                    launch_clock,
                    capture_clock: rep.capture_clock,
                    steps,
                });
            }
        }
    }

    // Build `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA` when `store_mode == Full`.
    if options.store_mode == StoreMode::Full {
        let total_cn = n_corners * n_nodes;
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

        for c_idx in 0..n_corners {
            let tv = &corner_node_vals[c_idx];
            let req_vec = &corner_required[c_idx];
            let ntags = tv.ntags as usize;
            let base = c_idx * n_nodes;

            for v in 0..n_nodes {
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
                let required = canon_grid(req_vec[v]);

                let mut primary_tag = NO_INDEX;
                let mut primary_arr = ABSENT_EARLY_LATE;
                let mut primary_pred = [[NO_PRED; 2]; 2];
                let mut worst_arr_early = [f64::INFINITY; 2];
                let mut worst_arr_late = [f64::NEG_INFINITY; 2];

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
                    for tr_i in 0..2 {
                        if arr[tr_i][0].is_finite() {
                            worst_arr_early[tr_i] = worst_arr_early[tr_i].min(arr[tr_i][0]);
                        }
                        if arr[tr_i][1].is_finite() {
                            worst_arr_late[tr_i] = worst_arr_late[tr_i].max(arr[tr_i][1]);
                        }
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

                let mut slack = ABSENT_SLACK;
                for tr_i in 0..2 {
                    if worst_arr_early[tr_i].is_finite() && required[tr_i][0].is_finite() {
                        slack[tr_i][0] = canon_f64(worst_arr_early[tr_i] - required[tr_i][0]);
                    }
                    if worst_arr_late[tr_i].is_finite() && required[tr_i][1].is_finite() {
                        slack[tr_i][1] = canon_f64(required[tr_i][1] - worst_arr_late[tr_i]);
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

    // 6. Build `EP_ORDER` (`[corner][check_kind: 0=setup, 1=hold]`, sorted by ascending slack).
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

    // 7. Sort `candidate_paths` deterministically by `(slack, corner_idx, endpoint_idx, is_hold,
    // end_tr)` and build `PATH_RANK`, `PATH_CHAIN_PTR`, `PATH_CHAIN`, `PATHS`, `PATH_POINTS`.
    candidate_paths.sort_by(|a, b| {
        a.slack
            .total_cmp(&b.slack)
            .then_with(|| a.corner_idx.cmp(&b.corner_idx))
            .then_with(|| a.endpoint_idx.cmp(&b.endpoint_idx))
            .then_with(|| a.check_kind.is_hold().cmp(&b.check_kind.is_hold()))
            .then_with(|| a.end_tr.index().cmp(&b.end_tr.index()))
    });
    if candidate_paths.len() > options.path_limit {
        candidate_paths.truncate(options.path_limit);
    }

    let n_ranked = candidate_paths.len();
    let n_top_k = n_ranked.min(options.top_k_paths);
    let mut path_rank = Vec::with_capacity(n_ranked);
    let mut path_chain_ptr = Vec::with_capacity(n_ranked + 1);
    path_chain_ptr.push(0);
    let mut path_chain = Vec::with_capacity(n_ranked * 16);
    let mut paths = Vec::with_capacity(n_top_k);
    let mut path_points = Vec::new();

    for (r_idx, cp) in candidate_paths.iter().enumerate() {
        let in_top_k = r_idx < n_top_k;
        path_rank.push(PathRankRec::new(
            cp.slack as f32,
            cp.endpoint_idx,
            cp.corner_idx,
            cp.check_kind,
            cp.end_tr,
            in_top_k,
        ));

        let ep = &mut endpoints[cp.endpoint_idx];
        if cp.check_kind.is_setup() {
            if ep.worst_setup_rank == NO_INDEX {
                ep.worst_setup_rank = r_idx as u32;
            }
        } else if ep.worst_hold_rank == NO_INDEX {
            ep.worst_hold_rank = r_idx as u32;
        }

        path_chain.extend_from_slice(&pack_f64_words(cp.required));
        path_chain.extend_from_slice(&pack_f64_words(cp.check_value));
        for &(node, tr, kind, tag_slot, nb, _, _) in &cp.steps {
            path_chain.extend_from_slice(&pack_chain_step(node, tr, kind, tag_slot, nb));
        }
        path_chain_ptr.push(path_chain.len() as u64);

        if in_top_k {
            let point_begin = path_points.len() as u32;
            let mut prev_arr = 0.0f64;
            for (k, &(node, tr, kind, _, _, arr, slew)) in cp.steps.iter().enumerate() {
                let incr = if k == 0 { arr } else { arr - prev_arr };
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
            }
            let point_end = path_points.len() as u32;
            paths.push(PathHeaderRec {
                slack: canon_f64(cp.slack),
                arrival: canon_f64(cp.arrival),
                required: canon_f64(cp.required),
                check_value: canon_f64(cp.check_value),
                rank_idx: r_idx as u32,
                endpoint_idx: cp.endpoint_idx as u32,
                launch_clock: cp.launch_clock,
                capture_clock: cp.capture_clock,
                point_begin,
                point_end,
            });
        }
    }

    // 8. Build `SUMMARY` (`num_corners * n_path_groups`).
    let mut summary = Vec::with_capacity(n_corners * n_path_groups);
    for c_idx in 0..n_corners {
        let slack_base = c_idx * n_endpoints;
        for &g_str in &path_group_strs {
            let is_overall = g_str == overall_group_str;
            let mut setup_wns = f64::INFINITY;
            let mut setup_tns = 0.0f64;
            let mut setup_violating = 0u32;
            let mut setup_endpoints = 0u32;
            let mut hold_wns = f64::INFINITY;
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
                    setup_wns = setup_wns.min(sl.setup_slack);
                    if sl.setup_slack < 0.0 {
                        setup_tns += sl.setup_slack;
                        setup_violating += 1;
                    }
                }
                if sl.hold_slack.is_finite() {
                    hold_endpoints += 1;
                    hold_wns = hold_wns.min(sl.hold_slack);
                    if sl.hold_slack < 0.0 {
                        hold_tns += sl.hold_slack;
                        hold_violating += 1;
                    }
                }
            }

            summary.push(SummaryRec {
                setup_wns: if setup_wns.is_finite() {
                    canon_f64(setup_wns)
                } else {
                    0.0
                },
                setup_tns: canon_f64(setup_tns),
                hold_wns: if hold_wns.is_finite() {
                    canon_f64(hold_wns)
                } else {
                    0.0
                },
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

    // 9. Build `NOTES` from graph diagnostics.
    let mut notes = Vec::with_capacity(graph.diagnostics().len());
    for diag in graph.diagnostics() {
        let code_str = strings.intern(diag.code);
        let message_str = strings.intern(&diag.message);
        notes.push(NoteRec {
            code_str,
            message_str,
            severity: NoteSeverity::Warning as u32,
            count: 1,
        });
    }

    // 10. Build `SCOPE` JSON and `META`.
    let scope_report = ScopeReport {
        top: nv.module_name().to_owned(),
        corners: corner_names,
        wire_model: options.wire_model.clone(),
        clock_model: options.clock_model.clone(),
        cppr: options.cppr.clone(),
        backend: options.backend_name.clone(),
        precision: options.precision,
        store_mode: options.store_mode,
        exceptions_applied: Vec::new(),
        completeness: CompletenessReport {
            analyzed_endpoints: n_endpoints,
            unconstrained_endpoints: unconstrained.len(),
            unconstrained_by_reason,
            disabled_arcs_by_reason: disabled_by_reason,
            unanalyzed_arcs_by_type: BTreeMap::new(),
        },
        limits: None,
    };
    let scope_bytes = serde_json::to_vec(&scope_report).unwrap_or_else(|_| b"{}".to_vec());

    let num_startpoints = corners
        .first()
        .map_or(0, |c| c.lowered.starts.node.len() as u32);
    let (str_bytes, str_offs) = strings.finish();

    let meta = StaMeta {
        num_nodes: n_nodes as u32,
        num_port_bits: n_port_bits as u32,
        num_arcs: graph.arcs().len() as u32,
        num_cells: n_instances as u32,
        num_startpoints,
        num_disabled_arcs: disabled_arcs.len() as u32,
        num_corners: n_corners as u32,
        num_clocks: clock_recs.len() as u32,
        num_tags: tag_recs.len() as u32,
        num_endpoints: n_endpoints as u32,
        num_ranked_paths: n_ranked as u32,
        num_top_k_paths: n_top_k as u32,
        num_path_groups: n_path_groups as u32,
        store_mode: options.store_mode.as_u32(),
        backend_str,
        precision: options.precision as u32,
    };

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
    data
}

fn cmp_slack_nan_last(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => a.total_cmp(&b),
    }
}

fn propagate_backward_required(
    graph: &TimingGraph,
    tlib: &TimingLib,
    lowered: &LoweredCorner<'_>,
    tv: &TimingValues,
    req_vec: &mut [[[f64; 2]; 2]],
) {
    // The arc classes of the lowered problem: lowering appends the classes of arcs whose sense
    // constant propagation refined, and of latch data arcs, to those of the library.
    let (order, in_ptr, in_arcs, classes) = (
        graph.order(),
        &lowered.csr.in_ptr,
        &lowered.csr.in_arcs,
        &lowered.luts.classes,
    );
    for dst_pos in (0..order.len()).rev() {
        let dst_node = order[dst_pos].index();
        let dst_req = req_vec[dst_node];
        if dst_req == ABSENT_REQUIRED {
            continue;
        }
        let dst_load = lowered.load[dst_pos];
        for in_arc in &in_arcs[in_ptr[dst_pos] as usize..in_ptr[dst_pos + 1] as usize] {
            let (src_pos, cls) = (in_arc.src as usize, &classes[in_arc.class as usize]);
            let src_node = order[src_pos].index();
            let is_wire = in_arc.class == tlib.wire_class;
            for ti in 0..2 {
                for to in 0..2 {
                    let bit = 1u32 << (ti * 2 + to);
                    if (!is_wire && (cls.flags & bit) == 0) || (is_wire && ti != to) {
                        continue;
                    }
                    for an_i in 0..2 {
                        let r_dst = dst_req[to][an_i];
                        if !r_dst.is_finite() {
                            continue;
                        }
                        let delay = if is_wire {
                            0.0
                        } else {
                            let s = tv.transition(src_pos, ti, an_i);
                            let s_in = if s.is_finite() { s } else { 0.0 };
                            tlib.lut_bank
                                .eval(cls.delay[to], [s_in, dst_load[to * 2 + an_i], 0.0])
                        };
                        let cand = canon_f64(r_dst - delay);
                        let slot = &mut req_vec[src_node][ti][an_i];
                        *slot = if an_i == An::Late.index() {
                            slot.min(cand)
                        } else {
                            slot.max(cand)
                        };
                    }
                }
            }
        }
    }
}

fn trace_path_steps(
    nv: &NetlistView<'_>,
    tv: &TimingValues,
    node_bucket: &[u32],
    end_node: NodeId,
    tag: usize,
    end_tr: Tr,
    an: An,
) -> Vec<(NodeId, Tr, PathPointKind, usize, u32, f64, f64)> {
    let (ntags, an_i) = (tv.ntags as usize, an.index());
    let mut rev_nodes: Vec<(NodeId, Tr)> = Vec::new();
    let (mut curr_node, mut curr_tr) = (end_node, end_tr);
    for _ in 0..nv.num_nodes().min(4096) {
        rev_nodes.push((curr_node, curr_tr));
        let raw_pred = tv.predecessor(curr_node.index(), tag, curr_tr.index(), an_i);
        if raw_pred == NO_PRED {
            break;
        }
        let src_node = NodeId(raw_pred & 0x7FFF_FFFF);
        let src_tr = if (raw_pred >> 31) == 0 {
            Tr::Rise
        } else {
            Tr::Fall
        };
        if src_node.index() >= nv.num_nodes() || src_node == curr_node {
            break;
        }
        let src_arr = tv.arrival(src_node.index(), tag, src_tr.index(), an_i);
        if src_arr == absent(an_i) || !src_arr.is_finite() {
            break;
        }
        (curr_node, curr_tr) = (src_node, src_tr);
    }
    rev_nodes.reverse();
    let mut out = Vec::with_capacity(rev_nodes.len());
    for (k, &(node, tr)) in rev_nodes.iter().enumerate() {
        let v = node.index();
        let kind = if k == 0 {
            if node.0 < nv.num_port_bits() as u32 {
                PathPointKind::Port
            } else {
                PathPointKind::ClockLaunch
            }
        } else {
            match (nv.node_kind(rev_nodes[k - 1].0), nv.node_kind(node)) {
                (
                    NodeKind::InstancePin { inst: i1, .. },
                    NodeKind::InstancePin { inst: i2, .. },
                ) if i1 == i2 => PathPointKind::Cell,
                (_, NodeKind::PortBit { .. }) => PathPointKind::Port,
                _ => PathPointKind::Net,
            }
        };
        let active_rank = (0..tag.min(ntags))
            .filter(|&t| {
                (0..2).any(|tr_i| (0..2).any(|a_i| tv.arrival(v, t, tr_i, a_i).is_finite()))
            })
            .count();
        let arr = canon_f64(tv.arrival(v, tag, tr.index(), an_i));
        let slew = canon_f64(tv.transition(v, tr.index(), an_i));
        out.push((node, tr, kind, active_rank, node_bucket[v], arr, slew));
    }
    out
}
