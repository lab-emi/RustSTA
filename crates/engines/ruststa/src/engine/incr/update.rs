// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing of a revision from the timing table of an ancestor revision (`owner: E11`;
//! `STA-REQ-0456` to `STA-REQ-0458`, `STA-REQ-0605`, `STA-REQ-0610`).
//!
//! [`select_ancestor`] finds the nearest ancestor revision with a timing table and decides
//! whether the revision can be analyzed from it: same libraries, constraints and parasitics,
//! a table of every node (`full` store mode) computed in `f64` for the same corners, the same
//! ports, and a dirty fraction within the threshold. [`analyze_from_ancestor`] then lowers the
//! ancestor's netlist as the full analysis does, takes the propagated values of every node and tag
//! from the ancestor's table, and runs the analysis of the revision with a cone propagation from
//! them. The result is an [`EngineAnalysis`] bitwise equal to a full analysis, so the timing table
//! built from it is byte-identical to the table of `ruststa run` (`STA-REQ-0611`).
//! [`update_revision`] commits it as a new revision.

use std::fmt;

use rustc_hash::FxHashSet;
use rusteda_compute::timing::{NO_PRED, TimingValues, absent};
use rusteda_core::job::{Ctx, Diag};
use rusteda_core::liberty::Library;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::{ConstrSection, Sdc, section_id as sdc_sec};
use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::schema::JsonMap;
use rusteda_workspace::{Error as WsError, Invocation, Rev, Revision, Workspace};

use super::cone::IncrementalStats;
use super::diff::{NetlistDiff, NetlistDiffSummary, diff_frozen_netlists};
use super::edit::SessionError;
use super::verify::BitwiseMismatch;
use super::{PriorAnalysis, PriorCorner};
use crate::engine::analysis::{
    AnalysisOptions, EngineAnalysis, Propagation, STATE_BYTES_PER_NODE_TAG, compile_corner_libs,
    lower_corners,
};
use crate::engine::input::constr::{CompiledConstraints, CornerLibs};
use crate::engine::prop::lower::Precision;
use crate::engine::scope::StaSummary;
use crate::engine::store::from_analysis::build_store_from_analysis;
use crate::engine::store::layout::{NO_INDEX, StoreMode, StorePrecision, TABLE_NAME};
use crate::engine::store::reader::{StaStore, StaStoreError};
use crate::engine::store::writer::StaStoreData;

/// Default maximum dirty instance/net fraction (`25%`) before an update falls back to a full
/// computation (`STA-REQ-0456`).
pub const DEFAULT_MAX_DIRTY_FRACTION: f64 = 0.25;

/// Why an update computes the whole graph instead of the cone of the changes (`STA-REQ-0456`,
/// `STA-REQ-0610`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullUpdateReason {
    /// A full computation was requested explicitly.
    Forced,
    /// No ancestor revision in the parent chain has a timing table.
    NoAncestorSta,
    /// The ancestor's table holds no values per node (`summary` store mode).
    AncestorSummaryOnly,
    /// The `constraints` table changed between the ancestor and the revision.
    ConstraintsChanged,
    /// The libraries or corners changed between the ancestor and the revision.
    LibrariesChanged,
    /// The `parasitics` table changed between the ancestor and the revision.
    ParasiticsChanged,
    /// Top-level ports (count, names, ranges or directions) changed.
    PortsChanged,
    /// The fraction of changed instances or nets is above the threshold.
    DirtyFractionExceeded,
    /// The ancestor's table was computed for other corners or in `f32`, or `f32` is asked for.
    OptionsChanged,
    /// The ancestor's table does not fit the analysis of its own netlist (node or tag counts).
    AncestorInconsistent,
    /// The state of the ancestor and of the revision together would exceed the state budget
    /// (`STA-REQ-0024`).
    StateBudget,
}

impl FullUpdateReason {
    /// The reason as a stable token of the `incremental.reason` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forced => "forced",
            Self::NoAncestorSta => "no_ancestor_table",
            Self::AncestorSummaryOnly => "ancestor_summary_only",
            Self::ConstraintsChanged => "constraints_changed",
            Self::LibrariesChanged => "libraries_changed",
            Self::ParasiticsChanged => "parasitics_changed",
            Self::PortsChanged => "ports_changed",
            Self::DirtyFractionExceeded => "dirty_fraction_exceeded",
            Self::OptionsChanged => "options_changed",
            Self::AncestorInconsistent => "ancestor_inconsistent",
            Self::StateBudget => "state_budget",
        }
    }
}

impl fmt::Display for FullUpdateReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Execution mode selected by [`update_revision`] (`STA-REQ-0456`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    /// The revision already has a current timing table.
    UpToDate,
    /// The revision was analyzed from an ancestor's timing table.
    Incremental,
    /// The revision was analyzed from scratch.
    Full(FullUpdateReason),
}

/// Options of [`update_revision`] (`STA-REQ-0456` to `STA-REQ-0458`).
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Options of the analysis.
    pub analysis: AnalysisOptions,
    /// The corners to analyze, in order; empty for every corner of the revision.
    pub corners: Vec<String>,
    /// The store mode of the committed table.
    pub store_mode: StoreMode,
    /// The ranked paths materialized in the committed table (`ruststa run` uses at least 100).
    pub top_k_paths: usize,
    /// Maximum fraction of changed instances or nets for an incremental update.
    pub max_dirty_fraction: f64,
    /// Forces a full computation.
    pub force_full: bool,
    /// Also runs a full analysis and requires a byte-identical table (`STA-REQ-0458`).
    pub verify_full: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            analysis: AnalysisOptions::default(),
            corners: Vec::new(),
            store_mode: StoreMode::Full,
            top_k_paths: 100,
            max_dirty_fraction: DEFAULT_MAX_DIRTY_FRACTION,
            force_full: false,
            verify_full: false,
        }
    }
}

/// Result of [`update_revision`] (`STA-REQ-0456`).
#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub mode: UpdateMode,
    pub base_rev: Rev,
    pub committed_rev: Rev,
    pub ancestor_sta_rev: Option<Rev>,
    pub diff: Option<NetlistDiffSummary>,
    pub stats: IncrementalStats,
    pub sta_summary: StaSummary,
    pub sta_store: StaStoreData,
}

/// Error returned by [`update_revision`] and its helpers.
#[derive(Debug)]
pub enum UpdateError {
    Workspace(WsError),
    StaStore(StaStoreError),
    Session(SessionError),
    BitwiseMismatch(BitwiseMismatch),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(e) => write!(f, "workspace error: {e}"),
            Self::StaStore(e) => write!(f, "sta store error: {e}"),
            Self::Session(e) => write!(f, "{e}"),
            Self::BitwiseMismatch(m) => write!(f, "verify-full bitwise mismatch: {m}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<WsError> for UpdateError {
    fn from(value: WsError) -> Self {
        Self::Workspace(value)
    }
}

impl From<StaStoreError> for UpdateError {
    fn from(value: StaStoreError) -> Self {
        Self::StaStore(value)
    }
}

impl From<SessionError> for UpdateError {
    fn from(value: SessionError) -> Self {
        Self::Session(value)
    }
}

impl From<BitwiseMismatch> for UpdateError {
    fn from(value: BitwiseMismatch) -> Self {
        Self::BitwiseMismatch(value)
    }
}

/// The ancestor revision an incremental update starts from (`STA-REQ-0610`): its timing table,
/// its netlist table and the diff of its netlist to the updated one.
#[derive(Debug)]
pub struct AncestorTable {
    /// The ancestor revision.
    pub rev: Revision,
    /// Its timing table.
    pub store: StaStore,
    /// Its netlist table.
    pub netlist: Rbin,
    /// The diff of its netlist to the netlist of the updated revision.
    pub diff: NetlistDiff,
}

/// The outcome of [`select_ancestor`]: the nearest ancestor with a timing table, if any, and
/// either that table or the reason why the update computes the whole graph.
#[derive(Debug)]
pub struct AncestorChoice {
    /// The nearest ancestor revision with a timing table.
    pub ancestor_rev: Option<Rev>,
    /// The ancestor table to start from, or the reason for a full computation.
    pub table: Result<AncestorTable, FullUpdateReason>,
    /// The diff summary when the netlists were compared.
    pub diff: Option<NetlistDiffSummary>,
}

/// Finds the nearest revision with a timing table among `target`'s parents (`target` itself
/// included) and decides whether `target` can be analyzed from it (`STA-REQ-0456`,
/// `STA-REQ-0610`). `corners` are the corners the update analyzes, in order, and `precision`
/// the precision of its propagation.
pub fn select_ancestor(
    ws: &Workspace,
    target: &Revision,
    new_frozen: &FrozenNetlist<'_>,
    corners: &[String],
    precision: Precision,
    max_dirty_fraction: f64,
    force_full: bool,
) -> Result<AncestorChoice, UpdateError> {
    let mut cur = Some(target.rev());
    let mut visited = FxHashSet::default();
    let anc = loop {
        let Some(id) = cur else {
            return Ok(AncestorChoice {
                ancestor_rev: None,
                table: Err(FullUpdateReason::NoAncestorSta),
                diff: None,
            });
        };
        if !visited.insert(id) {
            return Ok(AncestorChoice {
                ancestor_rev: None,
                table: Err(FullUpdateReason::NoAncestorSta),
                diff: None,
            });
        }
        let rev = ws.revision(id)?;
        if rev.has_table(TABLE_NAME) && rev.has_table("netlist") {
            break rev;
        }
        cur = rev.meta().parent;
    };
    let ancestor_rev = Some(anc.rev());
    let fail = |reason: FullUpdateReason| {
        Ok(AncestorChoice {
            ancestor_rev,
            table: Err(reason),
            diff: None,
        })
    };
    if force_full {
        return fail(FullUpdateReason::Forced);
    }
    let Ok(store) = StaStore::open_for_mutation_input(&anc) else {
        return fail(FullUpdateReason::AncestorInconsistent);
    };
    if store.store_mode()? == StoreMode::Summary {
        return fail(FullUpdateReason::AncestorSummaryOnly);
    }
    if anc.libraries_hash() != target.libraries_hash() || anc.corners() != target.corners() {
        return fail(FullUpdateReason::LibrariesChanged);
    }
    if anc.table_hash("constraints") != target.table_hash("constraints") {
        return fail(FullUpdateReason::ConstraintsChanged);
    }
    if anc.table_hash("parasitics") != target.table_hash("parasitics") {
        return fail(FullUpdateReason::ParasiticsChanged);
    }
    let scope = store.query_scope()?;
    if scope.corners != corners
        || scope.precision != StorePrecision::F64
        || precision != Precision::F64
    {
        return fail(FullUpdateReason::OptionsChanged);
    }
    let netlist = rusteda_workspace::netlist::open_rev(&anc)?;
    let diff = {
        let old_frozen = rusteda_workspace::netlist::decode(&netlist)?;
        diff_frozen_netlists(&old_frozen, new_frozen)
    };
    let summary = diff.summary.clone();
    let reason = if diff.ports_changed {
        Some(FullUpdateReason::PortsChanged)
    } else if diff.summary.instance_dirty_fraction > max_dirty_fraction
        || diff.summary.net_dirty_fraction > max_dirty_fraction
    {
        Some(FullUpdateReason::DirtyFractionExceeded)
    } else {
        None
    };
    Ok(AncestorChoice {
        ancestor_rev,
        table: match reason {
            Some(r) => Err(r),
            None => Ok(AncestorTable {
                rev: anc,
                store,
                netlist,
                diff,
            }),
        },
        diff: Some(summary),
    })
}

/// Analyzes `netlist` from the timing table of `ancestor` (`STA-REQ-0605`, `STA-REQ-0610`): the
/// ancestor's netlist is lowered with the same libraries, constraints and options, the values of
/// every node and tag are read from its table, and the analysis of `netlist` propagates only the
/// cone of the nodes whose inputs changed. `Ok(Err(reason))` when the table does not fit the
/// lowered ancestor, so that the caller computes the whole graph.
#[allow(clippy::type_complexity)]
pub fn analyze_from_ancestor<'a>(
    netlist: &'a FrozenNetlist<'a>,
    corners: &[CornerLibs<'_>],
    sdc: &Sdc,
    constraints: CompiledConstraints,
    options: &AnalysisOptions,
    ctx: Ctx<'_>,
    ancestor: &AncestorTable,
) -> Result<Result<(EngineAnalysis<'a>, IncrementalStats), FullUpdateReason>, UpdateError> {
    // The values of the ancestor live beside those of the revision: both within the budget of one
    // corner's state (`STA-REQ-0024`).
    let meta = ancestor.store.view()?.meta;
    let state =
        u64::from(meta.num_nodes) * u64::from(meta.num_tags.max(1)) * STATE_BYTES_PER_NODE_TAG;
    if state.saturating_mul(2) > options.max_state_bytes {
        return Ok(Err(FullUpdateReason::StateBudget));
    }
    let timing_libs = compile_corner_libs(corners).map_err(|d| SessionError::Diag(vec![d]))?;
    let old_frozen = rusteda_workspace::netlist::decode(&ancestor.netlist)?;
    let Ok(old_constraints) = CompiledConstraints::compile(sdc, &old_frozen, corners) else {
        return Ok(Err(FullUpdateReason::AncestorInconsistent));
    };
    let Ok((old_graph, old_lowered)) = lower_corners(
        &old_frozen,
        corners,
        &timing_libs,
        &old_constraints,
        options,
    ) else {
        return Ok(Err(FullUpdateReason::AncestorInconsistent));
    };
    if meta.num_nodes as usize != old_graph.num_nodes()
        || meta.num_corners as usize != old_lowered.len()
        || old_lowered.iter().any(|l| l.ntags != meta.num_tags.max(1))
    {
        return Ok(Err(FullUpdateReason::AncestorInconsistent));
    }
    let mut old_values = Vec::with_capacity(old_lowered.len());
    for (c, l) in old_lowered.iter().enumerate() {
        old_values.push(reconstruct_timing_values_from_store(
            &ancestor.store,
            c,
            old_graph.num_nodes(),
            l.ntags,
        )?);
    }
    let prior = PriorAnalysis {
        graph: &old_graph,
        corners: old_lowered
            .iter()
            .zip(&old_values)
            .map(|(lowered, node_values)| PriorCorner {
                lowered,
                node_values,
            })
            .collect(),
    };
    let (analysis, record) = EngineAnalysis::analyze_with(
        netlist,
        corners,
        Some(timing_libs),
        constraints,
        options,
        ctx,
        Propagation::Cone {
            prior: &prior,
            mapping: &ancestor.diff.mapping,
        },
        false,
    )
    .map_err(SessionError::Diag)?;
    let n_nodes = analysis.graph.num_nodes();
    let mut stats = IncrementalStats {
        total_nodes: n_nodes,
        dirty_instances: ancestor.diff.summary.dirty_instances(),
        dirty_nets: ancestor.diff.summary.dirty_nets(),
        backward_seeds: n_nodes,
        backward_nodes_evaluated: n_nodes * analysis.corners.len(),
        backward_nodes_changed: n_nodes,
        ..IncrementalStats::default()
    };
    for (i, ch) in record.changed.iter().enumerate() {
        stats.forward_seeds = stats.forward_seeds.max(record.seeds[i]);
        stats.forward_nodes_evaluated += record.evaluated[i] as usize;
        stats.forward_nodes_changed = stats
            .forward_nodes_changed
            .max(ch.as_ref().map_or(n_nodes, Vec::len));
    }
    Ok(Ok((analysis, stats)))
}

/// Reconstructs [`TimingValues`] (in [`NodeId`](crate::engine::types::NodeId) order
/// `0..n_nodes`) for `corner` from an opened [`StaStore`] (`NODE_REC` + `TAG_EXTRA`;
/// `STA-REQ-0457`).
pub fn reconstruct_timing_values_from_store(
    store: &StaStore,
    corner: usize,
    n_nodes: usize,
    ntags: u32,
) -> Result<TimingValues, StaStoreError> {
    store.require_full_mode_for_incremental()?;
    let v = store.view()?;
    let n_corners = v.meta.num_corners as usize;
    if corner >= n_corners {
        return Err(StaStoreError::CornerOutOfRange {
            corner,
            total: n_corners,
        });
    }
    let ntags_usize = ntags.max(1) as usize;
    let w = ntags_usize * 4;
    let mut arr: Vec<f64> = (0..n_nodes * w).map(|i| absent(i % 2)).collect();
    let mut slew: Vec<f64> = (0..n_nodes * 4).map(|i| absent(i % 2)).collect();
    let mut pred: Vec<u32> = vec![NO_PRED; n_nodes * w];

    let store_nodes = (v.meta.num_nodes as usize).min(n_nodes);
    let base = corner * (v.meta.num_nodes as usize);

    for node_idx in 0..store_nodes {
        let nr = &v.node_rec[base + node_idx];
        slew[node_idx * 4] = nr.slew[0][0];
        slew[node_idx * 4 + 1] = nr.slew[0][1];
        slew[node_idx * 4 + 2] = nr.slew[1][0];
        slew[node_idx * 4 + 3] = nr.slew[1][1];

        if nr.tag != NO_INDEX && (nr.tag as usize) < ntags_usize {
            let t = nr.tag as usize;
            write_tag_slot(
                &mut arr[node_idx * w + t * 4..node_idx * w + (t + 1) * 4],
                &mut pred[node_idx * w + t * 4..node_idx * w + (t + 1) * 4],
                &nr.arrival,
                &nr.pred,
            );
        }

        let extra_begin = v.tag_extra_ptr[base + node_idx] as usize;
        let extra_end = v.tag_extra_ptr[base + node_idx + 1] as usize;
        for extra in &v.tag_extra[extra_begin..extra_end] {
            let t = extra.tag as usize;
            if t < ntags_usize {
                write_tag_slot(
                    &mut arr[node_idx * w + t * 4..node_idx * w + (t + 1) * 4],
                    &mut pred[node_idx * w + t * 4..node_idx * w + (t + 1) * 4],
                    &extra.arrival,
                    &extra.pred,
                );
            }
        }
    }

    Ok(TimingValues {
        ntags: ntags.max(1),
        arr,
        slew,
        pred,
    })
}

fn write_tag_slot(
    arr_slot: &mut [f64],
    pred_slot: &mut [u32],
    arrival: &[[f64; 2]; 2],
    pred: &[[u32; 2]; 2],
) {
    arr_slot[0] = arrival[0][0];
    arr_slot[1] = arrival[0][1];
    arr_slot[2] = arrival[1][0];
    arr_slot[3] = arrival[1][1];
    pred_slot[0] = pred[0][0];
    pred_slot[1] = pred[0][1];
    pred_slot[2] = pred[1][0];
    pred_slot[3] = pred[1][1];
}

/// Updates the timing table of `target_rev` (`HEAD` when `None`) and commits it as a new revision
/// (`STA-REQ-0456` to `STA-REQ-0458`, `STA-REQ-0605`): incrementally from the nearest ancestor's
/// table when [`select_ancestor`] allows it, else by a full analysis. The table is the one
/// `ruststa run` commits for the revision, byte for byte.
pub fn update_revision(
    ws: &Workspace,
    target_rev: Option<Rev>,
    options: &UpdateOptions,
) -> Result<UpdateResult, UpdateError> {
    let rev_id = match target_rev {
        Some(r) => r,
        None => ws.head()?,
    };
    let rev = ws.revision(rev_id)?;
    let corners = load_revision_corners(&rev, &options.corners)?;
    let corner_names: Vec<String> = corners.iter().map(|(n, _)| n.clone()).collect();
    let upstream_names: Vec<String> = if options.corners.is_empty() {
        vec!["netlist".into(), "constraints".into(), "libraries".into()]
    } else {
        let mut v = vec!["netlist".to_string(), "constraints".to_string()];
        v.extend(options.corners.iter().map(|c| format!("libraries.{c}")));
        v
    };
    let precision = match options.analysis.precision {
        Precision::F64 => StorePrecision::F64,
        Precision::F32 => StorePrecision::F32,
    };

    // 1. A current table needs no update (`STA-REQ-0456`).
    if !options.force_full
        && rev.has_table(TABLE_NAME)
        && let Ok(entry) = rev.table(TABLE_NAME)
        && let Ok(existing) = StaStore::open_for_mutation_input(&rev)
        && let Ok(scope) = existing.query_scope()
        && scope.store_mode == options.store_mode
        && scope.precision == precision
        && scope.corners == corner_names
        && entry
            .upstream
            .keys()
            .map(String::as_str)
            .eq(sorted(&upstream_names).iter().map(String::as_str))
        && entry
            .upstream
            .iter()
            .all(|(k, h)| rev.table_hash(k) == Some(*h))
    {
        let sta_store = existing.to_store_data()?;
        let n_nodes = sta_store.meta[0].num_nodes as usize;
        return Ok(UpdateResult {
            mode: UpdateMode::UpToDate,
            base_rev: rev_id,
            committed_rev: rev_id,
            ancestor_sta_rev: Some(rev_id),
            diff: None,
            stats: IncrementalStats {
                total_nodes: n_nodes,
                ..IncrementalStats::default()
            },
            sta_summary: StaSummary::default(),
            sta_store,
        });
    }

    // 2. The inputs of the revision.
    let netlist_rbin = rusteda_workspace::netlist::open_rev(&rev)?;
    let frozen = rusteda_workspace::netlist::decode(&netlist_rbin)?;
    let sdc = load_revision_sdc(&rev)?;
    let corner_refs: Vec<CornerLibs<'_>> = corners
        .iter()
        .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
        .collect();
    let compile = || {
        CompiledConstraints::compile(&sdc, &frozen, &corner_refs)
            .map_err(|e| UpdateError::Session(SessionError::Diag(e.diags)))
    };

    // 3. Incremental from the nearest ancestor table, else full (`STA-REQ-0456`).
    let choice = select_ancestor(
        ws,
        &rev,
        &frozen,
        &corner_names,
        options.analysis.precision,
        options.max_dirty_fraction,
        options.force_full,
    )?;
    let mut incremental = None;
    let mut reason = FullUpdateReason::NoAncestorSta;
    match &choice.table {
        Ok(anc) => {
            match analyze_from_ancestor(
                &frozen,
                &corner_refs,
                &sdc,
                compile()?,
                &options.analysis,
                Ctx::NONE,
                anc,
            )? {
                Ok(done) => incremental = Some(done),
                Err(r) => reason = r,
            }
        }
        Err(r) => reason = *r,
    }
    let (mode, analysis, stats) = match incremental {
        Some((analysis, stats)) => (UpdateMode::Incremental, analysis, stats),
        None => {
            let analysis = EngineAnalysis::analyze(
                &frozen,
                &corner_refs,
                compile()?,
                &options.analysis,
                Ctx::NONE,
            )
            .map_err(SessionError::Diag)?;
            let n = analysis.graph.num_nodes();
            let k = analysis.corners.len();
            let stats = IncrementalStats {
                total_nodes: n,
                dirty_instances: frozen.instances.len(),
                dirty_nets: frozen.nets.len(),
                forward_seeds: n,
                forward_nodes_evaluated: n * k,
                forward_nodes_changed: n,
                backward_seeds: n,
                backward_nodes_evaluated: n * k,
                backward_nodes_changed: n,
            };
            (UpdateMode::Full(reason), analysis, stats)
        }
    };
    let backend = crate::engine::incr::session::backend_name(&options.analysis);
    let sta_store = build_store_from_analysis(
        &analysis,
        &corners,
        backend,
        precision,
        options.store_mode,
        options.top_k_paths,
    );
    if options.verify_full && mode == UpdateMode::Incremental {
        let full = EngineAnalysis::analyze(
            &frozen,
            &corner_refs,
            compile()?,
            &options.analysis,
            Ctx::NONE,
        )
        .map_err(SessionError::Diag)?;
        let full_store = build_store_from_analysis(
            &full,
            &corners,
            backend,
            precision,
            options.store_mode,
            options.top_k_paths,
        );
        if full_store != sta_store {
            return Err(UpdateError::BitwiseMismatch(BitwiseMismatch {
                corner: 0,
                category: "sta_store",
                node: None,
                node_name: String::from("<sta.rbin>"),
                detail: String::from("the incremental timing table differs from the full one"),
                incremental_bits: 1,
                full_bits: 0,
            }));
        }
    }
    let sta_summary = analysis.sta_summary;
    drop(analysis);

    // 4. Commit a new child revision with the table.
    let inv = Invocation::new(
        "ruststa",
        env!("CARGO_PKG_VERSION"),
        "ruststa update",
        ["ruststa", "update"],
    );
    let base = ws.resolve_base(&rev_id.to_string())?;
    let mut txn = ws.begin(base, &inv)?;
    let names: Vec<&str> = upstream_names.iter().map(String::as_str).collect();
    let upstream = txn.upstream(&names)?;
    sta_store.stage_to_txn(&mut txn, upstream)?;
    let committed = txn.commit(JsonMap::new())?;

    Ok(UpdateResult {
        mode,
        base_rev: rev_id,
        committed_rev: committed.rev,
        ancestor_sta_rev: choice.ancestor_rev,
        diff: choice.diff,
        stats,
        sta_summary,
        sta_store,
    })
}

fn sorted(names: &[String]) -> Vec<String> {
    let mut v = names.to_vec();
    v.sort();
    v
}

/// The libraries of the corners `requested` of `rev` (every corner when empty), in order.
fn load_revision_corners(
    rev: &Revision,
    requested: &[String],
) -> Result<Vec<(String, Vec<Library>)>, UpdateError> {
    let names = if requested.is_empty() {
        rev.corners()
    } else {
        requested.to_vec()
    };
    if names.is_empty() {
        return Err(UpdateError::Session(SessionError::Diag(vec![Diag::error(
            "T0005",
            "revision has no Liberty libraries",
        )])));
    }
    let mut out = Vec::with_capacity(names.len());
    for cname in names {
        let entries = rusteda_workspace::rlib::libraries_of_corner(rev, &cname);
        if entries.is_empty() {
            return Err(UpdateError::Session(SessionError::Diag(vec![Diag::error(
                "T0005",
                format!("corner '{cname}' has no libraries in {}", rev.rev()),
            )])));
        }
        let mut libs = Vec::with_capacity(entries.len());
        for entry in entries {
            libs.push(rusteda_workspace::rlib::load_entry(rev, entry)?);
        }
        out.push((cname, libs));
    }
    Ok(out)
}

/// The constraints of `rev` from its `CONSTR v1` table (none without the table).
fn load_revision_sdc(rev: &Revision) -> Result<Sdc, UpdateError> {
    if !rev.has_table("constraints") {
        return Ok(Sdc::default());
    }
    let rbin = rev.open_rbin("constraints", "CONSTR", 1..=1)?;
    rbin.verify_hash()?;
    let mut sections = Vec::new();
    for id in sdc_sec::STR_BYTES..=sdc_sec::DIAGS {
        if let Some(info) = rbin.sections().iter().find(|s| s.id == u32::from(id)) {
            let bytes = rbin.section_bytes(u32::from(id))?.to_vec();
            sections.push(ConstrSection {
                id,
                elem_size: info.elem_size,
                count: info.count(),
                bytes,
            });
        }
    }
    Sdc::from_sections(&sections).map_err(|e| {
        UpdateError::Session(SessionError::Diag(vec![Diag::error(
            "T0020",
            e.to_string(),
        )]))
    })
}
