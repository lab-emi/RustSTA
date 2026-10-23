// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Workspace revision loading, `EngineAnalysis` execution, and `rusteda.v1` envelope header
//! (`scope`, `completeness`, `metrics`) builders.

use rusteda_core::diag as core_diag;
use rusteda_core::job::{Ctx, Diag as JobDiag, Observer};
use rusteda_core::liberty::Library;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::liberty;
use rusteda_formats::sdc::{self, ConstrSection, Sdc};
use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::schema::JsonMap;
use rusteda_workspace::{
    Corruption, DiagCode, Diagnostic, Error, JsonObject, Revision, Severity, Workspace, rlib,
};

use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis, Propagation, WireModelOption};
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::limits::status::{DRV_KINDS, limit_statuses};
use ruststa::engine::limits::table::LimitsData;
use ruststa::engine::limits::{LimitKind, MAX_LIMIT_ROWS_CEILING};
use ruststa::engine::prop::lower::LoweredCorner;
use ruststa::engine::prop::lower::{BackendKind, Precision};
use ruststa::engine::prop::tags::DEFAULT_TAG_BUDGET;
use ruststa::engine::store::{
    CornerLimitsReport, PathPointKind, QueriedPath, StaStore, StaStoreData, StaStoreError,
    StoreMode, StorePrecision, canon_f64,
};
use ruststa::engine::types::NodeId;

use crate::args::{PrecisionArg, StoreArg, WireModelArg};
use crate::phases::PhaseLog;
use ruststa::engine::store::from_analysis::{
    StoreInput, build_store_from_analysis_with_ctx, check_store_tables,
};

/// Options for running STA on a workspace revision.
#[derive(Debug, Clone)]
pub struct StaRunOptions {
    pub corners: Vec<String>,
    pub backend_arg: String,
    pub precision: PrecisionArg,
    pub max_paths: usize,
    pub store_mode: StoreArg,
    /// Limit of the propagation state of one corner in GiB (`STA-REQ-0024`).
    pub max_state_gib: f64,
    /// Rows kept per corner and kind by the limit checks (`STA-REQ-0810`).
    pub max_limit_rows: usize,
    /// The wire model every analyzed corner must use (`--require-wire-model`, `STA-REQ-0821`).
    pub require_wire_model: Option<WireModelArg>,
    /// The rules of the pulse checks (`--pulse-groups`, `--pulse-pins`; `STA-REQ-0816`,
    /// `STA-REQ-0817`).
    pub pulse_rules: ruststa::engine::limits::PulseRules,
}

/// Code of an analysis whose wire model is not the required one (`STA-REQ-0821`).
pub const CODE_WIRE_MODEL: DiagCode = DiagCode::of("T0135");

/// Rejects a revision without extracted parasitics before it is analyzed, when `require` asks
/// for them (`STA-REQ-0821`).
pub fn check_wire_model_inputs(
    rev: &Revision,
    require: Option<WireModelArg>,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    if require == Some(WireModelArg::SpefRc) && !rev.has_table("parasitics") {
        let msg = "--require-wire-model spef_rc: the revision has no extracted parasitics \
                   (import --spef FILE --spef-corner NAME with its netlist)";
        diags.push(Diagnostic::new(Severity::Error, CODE_WIRE_MODEL, msg));
        return Err(Error::input_rejected(msg));
    }
    Ok(())
}

/// Rejects an analysis whose corners do not all use the required wire model (`STA-REQ-0821`).
pub fn check_wire_model(
    analysis: &EngineAnalysis<'_>,
    require: Option<WireModelArg>,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    let Some(want) = require else {
        return Ok(());
    };
    for c in &analysis.corners {
        let got = c.wire_model.scope().wire_model;
        if got != want.as_str() {
            let msg = format!(
                "--require-wire-model {}: corner {} is analyzed with wire model {got}",
                want.as_str(),
                c.corner_name
            );
            diags.push(Diagnostic::new(
                Severity::Error,
                CODE_WIRE_MODEL,
                msg.clone(),
            ));
            return Err(Error::input_rejected(msg));
        }
    }
    Ok(())
}

/// Validates `--max-limit-rows` (`STA-REQ-0810`).
pub fn validate_limit_rows(rows: usize) -> Result<(), Error> {
    if rows > MAX_LIMIT_ROWS_CEILING {
        return Err(Error::usage(format!(
            "--max-limit-rows must be at most {MAX_LIMIT_ROWS_CEILING}"
        )));
    }
    Ok(())
}

/// The tables and envelope headers of an analysis (`STA-REQ-0302`, `STA-REQ-0806`).
#[derive(Debug)]
pub struct AnalysisTables {
    /// The timing table.
    pub store: StaStoreData,
    /// The pin limit table.
    pub limits: LimitsData,
    /// Envelope `scope`.
    pub scope: JsonObject,
    /// Envelope `completeness`.
    pub completeness: JsonObject,
    /// Envelope `metrics`.
    pub metrics: JsonObject,
}

/// Resolved backend and its canonical scope string (`"cpu"` or `"cuda"`).
pub fn parse_backend(raw: &str) -> Result<(BackendKind, &'static str), Error> {
    match raw {
        "cpu" => Ok((BackendKind::Cpu, "cpu")),
        "cuda" | "cuda-native" => {
            #[cfg(feature = "cuda")]
            {
                Ok((BackendKind::Cuda, "cuda"))
            }
            #[cfg(not(feature = "cuda"))]
            {
                Err(Error::BackendUnavailable(
                    "cuda: this build of ruststa was compiled without the 'cuda' feature"
                        .to_string(),
                ))
            }
        }
        "gpu" => {
            #[cfg(feature = "cuda")]
            {
                Ok((BackendKind::Gpu, "cuda"))
            }
            #[cfg(not(feature = "cuda"))]
            {
                Err(Error::BackendUnavailable(
                    "gpu: no GPU backend is compiled into this build of ruststa".to_string(),
                ))
            }
        }
        "wgpu" => Err(Error::BackendUnavailable(
            "wgpu: backend 'wgpu' is not supported by ruststa".to_string(),
        )),
        other => Err(Error::usage(format!(
            "invalid value '{other}' for '--backend <BACKEND>': expected cpu, cuda, or gpu"
        ))),
    }
}

/// Converts a `rusteda_core::job::Diag` into a workspace envelope [`Diagnostic`].
pub fn convert_job_diag(d: &JobDiag) -> Diagnostic {
    let sev = match d.severity {
        core_diag::Severity::Note => Severity::Note,
        core_diag::Severity::Warning => Severity::Warning,
        core_diag::Severity::Error => Severity::Error,
    };
    let code = DiagCode::parse(&d.code).unwrap_or(DiagCode::of("T0001"));
    let loc = d.location.as_ref().map(|l| {
        let file = if l.file.is_empty() {
            "<design>".to_string()
        } else {
            l.file.clone()
        };
        (file, l.line.max(1), l.column.max(1))
    });
    Diagnostic::from_parts(sev, code, loc, d.message.clone(), d.help.clone())
}

/// Maps a [`StaStoreError`] into a workspace [`Error`].
pub fn map_store_error(err: StaStoreError) -> Error {
    match err {
        StaStoreError::Workspace(w) => w,
        StaStoreError::CorruptTable { path, reason } => {
            Error::corrupt(&path, Corruption::Json { reason })
        }
        StaStoreError::SummaryModeUnavailable { operation } => Error::input_rejected(format!(
            "cannot {operation}: sta.rbin was written in summary mode (--sta-store summary); re-run with --sta-store full"
        )),
        StaStoreError::UnknownPin { name } => Error::UnknownHandle {
            handle: name,
            reason: "no such pin in timing table".to_string(),
        },
        StaStoreError::UnknownPort { name } => Error::UnknownHandle {
            handle: name,
            reason: "no such port in timing table".to_string(),
        },
        StaStoreError::UnknownInstance { name } => Error::UnknownHandle {
            handle: name,
            reason: "no such instance in timing table".to_string(),
        },
        StaStoreError::NotAnEndpoint { name } => {
            Error::input_rejected(format!("'{name}' is not a timing endpoint"))
        }
        StaStoreError::PathRankOutOfRange { rank, total } => Error::UnknownHandle {
            handle: format!("path{}", rank + 1),
            reason: format!("out of range (revision has {total} ranked paths)"),
        },
        StaStoreError::CornerOutOfRange { corner, total } => Error::input_rejected(format!(
            "corner index {corner} out of range (total {total})"
        )),
    }
}

/// Decodes the [`Sdc`] model from a revision's `"constraints"` table (`CONSTR v1` `.rbin` or
/// `constraints/merged.sdc`).
pub fn load_sdc_from_revision(rev: &Revision) -> Result<(Sdc, String), Error> {
    let table_path = rev.table_path("constraints")?;
    if std::fs::symlink_metadata(&table_path).is_ok_and(|m| m.is_dir()) {
        let bytes = rev.read_table_file("constraints", "merged.sdc")?;
        let text = String::from_utf8(bytes).map_err(|e| {
            Error::corrupt(
                &table_path,
                Corruption::Json {
                    reason: format!("invalid UTF-8 in merged.sdc: {e}"),
                },
            )
        })?;
        let (sdc_model, _) = sdc::read_str(&text);
        return Ok((sdc_model, text));
    }

    let rbin = rev.open_rbin("constraints", "CONSTR", 1..=1)?;
    rbin.verify_hash()?;
    let mut sections = Vec::with_capacity(rbin.sections().len());
    for s in rbin.sections() {
        let bytes = rbin.section_bytes(s.id)?.to_vec();
        sections.push(ConstrSection {
            id: s.id as u16,
            elem_size: s.elem_size,
            count: s.count(),
            bytes,
        });
    }
    let sdc_model = Sdc::from_sections(&sections).map_err(|e| {
        Error::corrupt(
            rbin.path(),
            Corruption::Json {
                reason: format!("invalid CONSTR v1 sections: {e}"),
            },
        )
    })?;
    let mut canon_text = sdc::write(&sdc_model);
    if sdc_model.units.time_to_ns.is_some() || sdc_model.units.cap_to_pf.is_some() {
        let mut prefix = String::from("set_units");
        if sdc_model.units.time_to_ns.is_some() {
            prefix.push_str(" -time 1ns");
        }
        if sdc_model.units.cap_to_pf.is_some() {
            prefix.push_str(" -capacitance 1pF");
        }
        prefix.push('\n');
        canon_text.insert_str(0, &prefix);
    }
    Ok((sdc_model, canon_text))
}

/// Loads the libraries of `rev` grouped by corner in revision order (or filtered by
/// `requested_corners`).
pub fn load_corner_libraries(
    ws: &Workspace,
    rev: &Revision,
    requested_corners: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Result<Vec<(String, Vec<Library>)>, Error> {
    if rev.meta().libraries.is_empty() {
        return Err(Error::MissingTable {
            rev: rev.rev(),
            table: "libraries".to_string(),
        });
    }

    let available_corners: Vec<String> = {
        let mut v = Vec::new();
        for entry in &rev.meta().libraries {
            if !v.contains(&entry.corner) {
                v.push(entry.corner.clone());
            }
        }
        v
    };

    let target_corners: Vec<String> = if requested_corners.is_empty() {
        available_corners.clone()
    } else {
        let mut v = Vec::new();
        for c in requested_corners {
            if !available_corners.contains(c) {
                let msg = format!(
                    "corner '{c}' has no libraries in revision {} (available: {})",
                    rev.rev(),
                    available_corners.join(", ")
                );
                diags.push(Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("T0005"),
                    msg.clone(),
                ));
                return Err(Error::input_rejected(msg));
            }
            if !v.contains(c) {
                v.push(c.clone());
            }
        }
        v
    };

    let mut out = Vec::with_capacity(target_corners.len());
    for corner in target_corners {
        let entries = rlib::libraries_of_corner(rev, &corner);
        let mut libs = Vec::with_capacity(entries.len());
        for entry in entries {
            let (lib, _) = rlib::load_or_recompile(ws, rev, &entry.name, &entry.corner, |raw| {
                let text = std::str::from_utf8(raw).map_err(|e| {
                    Error::input_rejected(format!(
                        "invalid UTF-8 in cached Liberty file {}: {e}",
                        entry.name
                    ))
                })?;
                let (parsed, _) = liberty::read_text_diags(text, &entry.source)
                    .map_err(|e| Error::input_rejected(format!("{e:?}")))?;
                Ok::<_, Error>(parsed)
            })
            .map_err(|e| match e {
                rusteda_workspace::PutError::Compile(err)
                | rusteda_workspace::PutError::Workspace(err) => err,
            })?;
            libs.push(lib);
        }
        out.push((corner, libs));
    }
    Ok(out)
}

/// The analysis options, the backend name of the scope, and the precision and mode of the table
/// that `opts` ask for.
pub fn analysis_settings(
    opts: &StaRunOptions,
) -> Result<(AnalysisOptions, &'static str, StorePrecision, StoreMode), Error> {
    let (backend_kind, backend_scope_name) = parse_backend(&opts.backend_arg)?;
    let (precision, store_precision) = match opts.precision {
        PrecisionArg::F64 => (Precision::F64, StorePrecision::F64),
        PrecisionArg::F32 => (Precision::F32, StorePrecision::F32),
    };
    let store_mode = match opts.store_mode {
        StoreArg::Full => StoreMode::Full,
        StoreArg::Summary => StoreMode::Summary,
    };
    let analysis_opts = AnalysisOptions {
        backend: backend_kind,
        precision,
        wire_model: WireModelOption::Auto,
        parasitics: Vec::new(),
        tag_budget: DEFAULT_TAG_BUDGET,
        max_state_bytes: (opts.max_state_gib * f64::from(1u32 << 30)) as u64,
        max_limit_rows: opts.max_limit_rows,
        pulse_rules: opts.pulse_rules,
    };
    Ok((
        analysis_opts,
        backend_scope_name,
        store_precision,
        store_mode,
    ))
}

/// Compiles the constraints of a revision, reporting their diagnostics as `ruststa run` does.
pub fn compile_constraints(
    sdc_model: &Sdc,
    frozen: &FrozenNetlist<'_>,
    corner_refs: &[CornerLibs<'_>],
    diags: &mut Vec<Diagnostic>,
) -> Result<CompiledConstraints, Error> {
    match CompiledConstraints::compile(sdc_model, frozen, corner_refs) {
        Ok(c) => Ok(c),
        Err(err) => {
            for d in &err.diags {
                diags.push(convert_job_diag(d));
            }
            let first_msg = err
                .diags
                .iter()
                .find(|d| d.severity == core_diag::Severity::Error)
                .map_or_else(
                    || "constraint compilation failed".to_string(),
                    |d| d.message.clone(),
                );
            Err(Error::input_rejected(first_msg))
        }
    }
}

/// The error of a failed analysis, its diagnostics added to `diags` (`STA-REQ-0304`).
pub fn analysis_error(err_diags: &[JobDiag], diags: &mut Vec<Diagnostic>) -> Error {
    if let Some(d) = err_diags
        .iter()
        .find(|d| d.code == "A0501" || d.code == "T0002")
    {
        diags.push(Diagnostic::new(
            Severity::Error,
            DiagCode::of("A0501"),
            d.message.clone(),
        ));
        return Error::BackendUnavailable(d.message.clone());
    }
    for d in err_diags {
        diags.push(convert_job_diag(d));
    }
    let first_msg = err_diags.first().map_or_else(
        || "timing analysis failed".to_string(),
        |d| d.message.clone(),
    );
    Error::input_rejected(first_msg)
}

/// The timing and limit tables of `analysis` and their envelope headers, the analysis
/// diagnostics added to `diags` (`STA-REQ-0302`, `STA-REQ-0806`).
#[allow(clippy::too_many_arguments)]
pub fn table_of_analysis(
    analysis: &EngineAnalysis<'_>,
    lowered: Option<&[LoweredCorner<'_>]>,
    corner_libs_owned: &[(String, Vec<Library>)],
    backend_scope_name: &str,
    store_precision: StorePrecision,
    store_mode: StoreMode,
    max_paths: usize,
    max_limit_rows: usize,
    diags: &mut Vec<Diagnostic>,
) -> Result<AnalysisTables, Error> {
    for d in &analysis.diags {
        diags.push(convert_job_diag(d));
    }
    let n_endpoints = analysis
        .corners
        .iter()
        .map(|c| c.endpoints.endpoints.len())
        .max()
        .unwrap_or(0);
    ruststa::engine::store::layout::check_store_capacity(n_endpoints, analysis.corners.len())
        .map_err(Error::input_rejected)?;
    check_store_tables(&StoreInput::of(analysis), backend_scope_name)
        .map_err(Error::input_rejected)?;
    let top_k = max_paths.max(100);
    let log = PhaseLog::get();
    let store_data = PhaseLog::time(log, "store_build", || {
        build_store_from_analysis_with_ctx(
            analysis,
            lowered,
            corner_libs_owned,
            backend_scope_name,
            store_precision,
            store_mode,
            top_k,
            Ctx::new(None, log.map(|l| l as &dyn Observer)),
        )
    });
    let limits = PhaseLog::time(log, "limits_build", || {
        LimitsData::from_analysis(analysis, max_limit_rows)
    });
    // The headers are read from the in-memory table: no bytes are encoded here; `ruststa run`
    // encodes the table once, when it stages `sta.rbin`.
    let (store_data, scope_obj, comp_obj, metrics_obj) =
        PhaseLog::time(log, "store_headers", || {
            let opened = StaStore::from_data(store_data).map_err(map_store_error)?;
            let (scope_obj, comp_obj, metrics_obj) = envelope_headers_from_store(&opened, None)?;
            let data = opened
                .into_data()
                .ok_or_else(|| Error::internal("an in-memory timing table lost its data"))?;
            Ok::<_, Error>((data, scope_obj, comp_obj, metrics_obj))
        })?;
    Ok(AnalysisTables {
        store: store_data,
        limits,
        scope: scope_obj,
        completeness: comp_obj,
        metrics: metrics_obj,
    })
}

/// Runs multi-corner static timing analysis on `rev` and returns its tables and envelope
/// headers.
pub fn run_sta_on_revision(
    ws: &Workspace,
    rev: &Revision,
    opts: &StaRunOptions,
    diags: &mut Vec<Diagnostic>,
) -> Result<AnalysisTables, Error> {
    let (mut analysis_opts, backend_scope_name, store_precision, store_mode) =
        analysis_settings(opts)?;
    check_wire_model_inputs(rev, opts.require_wire_model, diags)?;
    let log = PhaseLog::get();
    let netlist_rbin: Rbin = PhaseLog::time(log, "open_netlist", || {
        rusteda_workspace::netlist::open_rev(rev)
    })?;
    let frozen: FrozenNetlist<'_> = PhaseLog::time(log, "decode_netlist", || {
        rusteda_workspace::netlist::decode(&netlist_rbin)
    })?;
    let (sdc_model, _) = PhaseLog::time(log, "load_constraints", || load_sdc_from_revision(rev))?;
    let corner_libs_owned = PhaseLog::time(log, "load_libraries", || {
        load_corner_libraries(ws, rev, &opts.corners, diags)
    })?;

    let corner_refs: Vec<CornerLibs<'_>> = corner_libs_owned
        .iter()
        .map(|(name, libs)| CornerLibs::new(name.as_str(), libs.as_slice()))
        .collect();
    let analyzed: Vec<String> = corner_libs_owned.iter().map(|(n, _)| n.clone()).collect();
    analysis_opts.parasitics =
        PhaseLog::time(log, "load_parasitics", || load_parasitics(rev, &analyzed))?;

    let compiled = PhaseLog::time(log, "compile_constraints", || {
        compile_constraints(&sdc_model, &frozen, &corner_refs, diags)
    })?;
    let ctx = Ctx::new(None, log.map(|l| l as &dyn Observer));
    // A full table needs the backward pass of every corner: the analysis keeps its lowered
    // problems for it, so the store does not lower the corners a second time.
    let keep_lowered = store_mode == StoreMode::Full;
    let (mut analysis, record) = EngineAnalysis::analyze_with(
        &frozen,
        &corner_refs,
        None,
        compiled,
        &analysis_opts,
        ctx,
        Propagation::Full,
        keep_lowered,
    )
    .map_err(|err_diags| analysis_error(&err_diags, diags))?;
    check_wire_model(&analysis, opts.require_wire_model, diags)?;
    // The table reads the node-ordered propagation values of each corner; the level-ordered copy
    // is not read again, so it is released before the table is built (AGENTS rule 9: on OpenC910
    // about 3 GB).
    for corner in &mut analysis.corners {
        corner.propagated.values = Default::default();
    }
    table_of_analysis(
        &analysis,
        keep_lowered.then_some(record.lowered.as_slice()),
        &corner_libs_owned,
        backend_scope_name,
        store_precision,
        store_mode,
        opts.max_paths,
        opts.max_limit_rows,
        diags,
    )
}

/// Computes `(logic_levels, cell_delay_ns, wire_delay_ns, cell_delay_pct, wire_delay_pct)` for a
/// [`QueriedPath`].
pub fn path_delay_breakdown(p: &QueriedPath) -> (u32, f64, f64, f64, f64) {
    let mut logic_levels = 0u32;
    let mut cell_delay_ns = 0.0f64;
    let mut wire_delay_ns = 0.0f64;
    for (index, pt) in p.points.iter().enumerate() {
        match pt.kind {
            PathPointKind::Cell => {
                logic_levels += 1;
                cell_delay_ns += pt.incr_delay;
            }
            PathPointKind::Net => {
                wire_delay_ns += pt.incr_delay;
            }
            PathPointKind::Port if index > 0 => wire_delay_ns += pt.incr_delay,
            PathPointKind::Port | PathPointKind::ClockLaunch => {}
        }
    }
    let total = cell_delay_ns + wire_delay_ns;
    let (cell_pct, wire_pct) = if total > 0.0 {
        let cp = ((cell_delay_ns / total) * 1000.0).round() / 10.0;
        let wp = ((wire_delay_ns / total) * 1000.0).round() / 10.0;
        (canon_f64(cp), canon_f64(wp))
    } else if logic_levels > 0 {
        (100.0, 0.0)
    } else {
        (0.0, 0.0)
    };
    (
        logic_levels,
        canon_f64(cell_delay_ns),
        canon_f64(wire_delay_ns),
        cell_pct,
        wire_pct,
    )
}

/// Builds `(top_paths, truncated_paths)` from `store` for `ruststa run` and `ruststa update`.
pub fn top_paths_from_store(
    store: &StaStore,
    max_paths: usize,
) -> Result<(Vec<JsonObject>, u32), Error> {
    let v = store.view().map_err(map_store_error)?;
    let n_ranked = v.path_rank.len();
    let mut setup_violating = 0usize;
    let mut setup_total = 0usize;
    for pr in v.path_rank {
        if !pr.check_kind().is_hold() {
            setup_total += 1;
            if pr.slack_ns < 0.0 {
                setup_violating += 1;
            }
        }
    }

    let pool_count = if setup_violating > 0 {
        setup_violating
    } else if setup_total > 0 {
        setup_total
    } else {
        n_ranked
    };
    let take_count = max_paths.min(pool_count).min(n_ranked);
    let mut top_paths = Vec::with_capacity(take_count);

    for rank_idx in 0..take_count {
        let p = store.query_path_handle(rank_idx).map_err(map_store_error)?;
        let handle = format!("path{}", rank_idx + 1);
        let startpoint = p
            .points
            .first()
            .map_or_else(|| p.endpoint_name.clone(), |pt| pt.name.clone());
        let (logic_levels, _, _, cell_pct, wire_pct) = path_delay_breakdown(&p);
        let mut obj = JsonObject::new();
        obj.insert("handle", &handle)?;
        obj.insert("slack_ns", &canon_f64(p.slack))?;
        obj.insert("startpoint", &startpoint)?;
        obj.insert("endpoint", &p.endpoint_name)?;
        obj.insert("logic_levels", &logic_levels)?;
        obj.insert("cell_delay_pct", &cell_pct)?;
        obj.insert("wire_delay_pct", &wire_pct)?;
        obj.insert("kind", &p.check_kind)?;
        top_paths.push(obj);
    }

    let truncated = pool_count.saturating_sub(top_paths.len()) as u32;
    Ok((top_paths, truncated))
}

/// Builds the three standard `rusteda.v1` envelope header objects (`scope`, `completeness`,
/// `metrics`) in the exact key order of `final-cli.md` from an opened [`StaStore`].
pub fn envelope_headers_from_store(
    store: &StaStore,
    corner_override: Option<&str>,
) -> Result<(JsonObject, JsonObject, JsonObject), Error> {
    let v = store.view().map_err(map_store_error)?;
    let scope_rep = store.query_scope().map_err(map_store_error)?;
    let summary_rep = store.query_summary().map_err(map_store_error)?;

    let mut worst_corner_idx = 0usize;
    let mut worst_corner_setup = f64::INFINITY;
    for row in &summary_rep.rows {
        if row.path_group == "**" && row.setup_wns < worst_corner_setup {
            worst_corner_setup = row.setup_wns;
            worst_corner_idx = row.corner_idx;
        }
    }
    let primary_corner = if let Some(co) = corner_override {
        co.to_string()
    } else {
        scope_rep
            .corners
            .get(worst_corner_idx)
            .cloned()
            .unwrap_or_else(|| "default".to_string())
    };

    let wire_load_opt: Option<String> = scope_rep
        .exceptions_applied
        .iter()
        .find_map(|s| s.strip_prefix("wire_load:").map(str::to_owned));
    let unconstrained_inputs: u32 = scope_rep
        .exceptions_applied
        .iter()
        .find_map(|s| s.strip_prefix("unconstrained_inputs:")?.parse::<u32>().ok())
        .unwrap_or(0);

    // 1. `scope` in exact `final-cli.md` key order.
    let mut scope = JsonObject::new();
    scope.insert("corner", &primary_corner)?;
    scope.insert("wire_model", &scope_rep.wire_model)?;
    let extracted = scope_rep.wire_model == "spef_rc";
    scope.insert("parasitics", &if extracted { "spef" } else { "none" })?;
    scope.insert("drc_status", &"unchecked")?;
    scope.insert("wire_load", &wire_load_opt)?;
    scope.insert(
        "delay_calculator",
        &if extracted {
            "nldm_ceff_ramp"
        } else {
            "nldm_lumped"
        },
    )?;
    if extracted {
        scope.insert("coupling", &"grounded_factor_1")?;
        scope.insert(
            "physical_clock_domains",
            &v.clocks
                .iter()
                .filter(|c| c.source_node != ruststa::engine::store::NO_INDEX)
                .count(),
        )?;
    }
    scope.insert("clock_model", &scope_rep.clock_model)?;
    scope.insert("cppr", &scope_rep.cppr)?;
    scope.insert("backend", &scope_rep.backend)?;
    let prec_str = match scope_rep.precision {
        StorePrecision::F64 => "f64",
        StorePrecision::F32 => "f32",
    };
    scope.insert("precision", &prec_str)?;
    if scope_rep.corners.len() > 1 {
        scope.insert("corners", &scope_rep.corners)?;
    }

    // 2. `completeness` in exact `final-cli.md` key order.
    let mut clock_domains = Vec::with_capacity(v.clocks.len());
    for ck in v.clocks {
        if let Ok(name) = v.strings.get(ck.name_str) {
            clock_domains.push(name.to_string());
        }
    }
    let mut unc_by_reason = JsonObject::new();
    for (k, val) in &scope_rep.completeness.unconstrained_by_reason {
        unc_by_reason.insert(k, val)?;
    }
    let mut unanalyzed_arcs = JsonObject::new();
    for (k, val) in &scope_rep.completeness.unanalyzed_arcs_by_type {
        unanalyzed_arcs.insert(k, val)?;
    }

    let mut completeness = JsonObject::new();
    completeness.insert(
        "constrained_endpoints",
        &(scope_rep.completeness.analyzed_endpoints as u32),
    )?;
    completeness.insert(
        "unconstrained_endpoints",
        &(scope_rep.completeness.unconstrained_endpoints as u32),
    )?;
    completeness.insert("clock_domains", &clock_domains)?;
    completeness.insert("unconstrained_by_reason", &unc_by_reason)?;
    completeness.insert("unconstrained_inputs", &unconstrained_inputs)?;
    completeness.insert("disabled_arcs", &(v.disabled_arcs.len() as u32))?;
    completeness.insert("unanalyzed_arcs", &unanalyzed_arcs)?;

    // 3. `metrics` in exact `final-cli.md` key order.
    let mut worst_setup_slack: Option<f64> = None;
    let mut worst_hold_slack: Option<f64> = None;
    let mut tns_ns = 0.0f64;
    let mut ths_ns = 0.0f64;
    let mut violating_setup = 0u32;
    let mut violating_hold = 0u32;

    let n_ep = v.endpoints.len();
    let n_corners = v.meta.num_corners as usize;
    for c_idx in 0..n_corners {
        if let Some(co) = corner_override {
            let cname = v.strings.get(v.corners[c_idx].name_str).unwrap_or("");
            if cname != co {
                continue;
            }
        }
        let mut c_tns = 0.0f64;
        let mut c_ths = 0.0f64;
        let mut c_v_setup = 0u32;
        let mut c_v_hold = 0u32;
        let base = c_idx * n_ep;
        for ep_idx in 0..n_ep {
            let sl = &v.ep_slack[base + ep_idx];
            if sl.setup_slack.is_finite() {
                let s = canon_f64(sl.setup_slack);
                worst_setup_slack = Some(worst_setup_slack.map_or(s, |prev| prev.min(s)));
                if s < 0.0 {
                    c_tns = canon_f64(c_tns + s);
                    c_v_setup += 1;
                }
            }
            if sl.hold_slack.is_finite() {
                let h = canon_f64(sl.hold_slack);
                worst_hold_slack = Some(worst_hold_slack.map_or(h, |prev| prev.min(h)));
                if h < 0.0 {
                    c_ths = canon_f64(c_ths + h);
                    c_v_hold += 1;
                }
            }
        }
        tns_ns = canon_f64(tns_ns.min(c_tns));
        ths_ns = canon_f64(ths_ns.min(c_ths));
        violating_setup = violating_setup.max(c_v_setup);
        violating_hold = violating_hold.max(c_v_hold);
    }

    let wns_ns = worst_setup_slack.map(|s| if s < 0.0 { canon_f64(s) } else { 0.0 });
    let whs_ns = worst_hold_slack.map(|h| if h < 0.0 { canon_f64(h) } else { 0.0 });
    let tns_opt = worst_setup_slack.map(|_| canon_f64(tns_ns));
    let ths_opt = worst_hold_slack.map(|_| canon_f64(ths_ns));

    let mut metrics = JsonObject::new();
    metrics.insert("wns_ns", &wns_ns)?;
    metrics.insert("tns_ns", &tns_opt)?;
    metrics.insert("violating_endpoints", &violating_setup)?;
    metrics.insert("whs_ns", &whs_ns)?;
    metrics.insert("ths_ns", &ths_opt)?;
    metrics.insert("hold_violating_endpoints", &violating_hold)?;
    metrics.insert("worst_setup_slack_ns", &worst_setup_slack)?;
    metrics.insert("worst_hold_slack_ns", &worst_hold_slack)?;
    add_limit_headers(
        scope_rep.limits.as_deref(),
        corner_override,
        &mut completeness,
        &mut metrics,
    )?;
    // The rules of the pulse checks are part of their scope (`STA-REQ-0816`, `STA-REQ-0817`).
    let pulse_rules = match scope_rep.limits.as_deref().and_then(<[_]>::first) {
        Some(c) => {
            let mut r = JsonObject::new();
            r.insert("groups", &c.pulse_groups)?;
            r.insert("pins", &c.pulse_pins)?;
            Some(r)
        }
        None => None,
    };
    scope.insert("pulse_rules", &pulse_rules)?;

    Ok((scope, completeness, metrics))
}

/// Adds the counts of the pin limit checks of `scope` to `completeness` and `metrics`: the
/// worst corner of each count, or `corner_override` alone. The counts and statuses follow
/// [`limit_statuses`]: a table without limit counts reports them as `null` with the status
/// `unchecked`, and a count without a checked row is `null` (`STA-REQ-0807`, `STA-REQ-0813`,
/// AGENTS rule 4.2).
pub fn add_limit_headers(
    limits: Option<&[CornerLimitsReport]>,
    corner_override: Option<&str>,
    completeness: &mut JsonObject,
    metrics: &mut JsonObject,
) -> Result<(), Error> {
    let corners: Option<Vec<&CornerLimitsReport>> = limits.map(|all| {
        all.iter()
            .filter(|c| corner_override.is_none_or(|o| c.corner == o))
            .collect()
    });
    let worst = |kinds: &[LimitKind]| -> Option<f64> {
        corners.as_ref().and_then(|cs| {
            cs.iter()
                .flat_map(|c| kinds.iter().filter_map(|k| c.kinds.get(k.name())))
                .filter_map(|k| k.worst_slack_ns)
                .reduce(f64::min)
                .map(canon_f64)
        })
    };
    let ls = limit_statuses(corners.as_deref());
    let mpw = [LimitKind::MinPulseWidthHigh, LimitKind::MinPulseWidthLow];
    let per = [LimitKind::MinPeriod];
    metrics.insert("min_pulse_width_violating", &ls.min_pulse_width_violating)?;
    metrics.insert("worst_min_pulse_width_slack_ns", &worst(&mpw))?;
    metrics.insert("min_period_violating", &ls.min_period_violating)?;
    metrics.insert("worst_min_period_slack_ns", &worst(&per))?;
    metrics.insert("pulse_width_status", &ls.pulse_width.name())?;
    for (k, (violating, status)) in DRV_KINDS.into_iter().zip(ls.drv) {
        let (count_key, worst_key, status_key) = match k {
            LimitKind::MaxTransition => (
                "max_transition_violating",
                "worst_max_transition_slack_ns",
                "max_transition_status",
            ),
            LimitKind::MaxCapacitance => (
                "max_capacitance_violating",
                "worst_max_capacitance_slack_pf",
                "max_capacitance_status",
            ),
            _ => (
                "max_fanout_violating",
                "worst_max_fanout_slack",
                "max_fanout_status",
            ),
        };
        metrics.insert(count_key, &violating)?;
        metrics.insert(worst_key, &worst(&[k]))?;
        metrics.insert(status_key, &status.name())?;
    }
    metrics.insert("drv_status", &ls.drv_status.name())?;

    let sum = |f: fn(&CornerLimitsReport) -> u64| -> Option<u64> {
        corners
            .as_ref()
            .filter(|cs| !cs.is_empty())
            .map(|cs| cs.iter().map(|c| f(c)).max().unwrap_or(0))
    };
    completeness.insert(
        "unchecked_pulse_width_pins",
        &sum(|c| c.unclocked_pulse_pins),
    )?;
    completeness.insert(
        "pass_through_pulse_pins",
        &sum(|c| c.pass_through_pulse_pins),
    )?;
    let uneval = sum(|c| c.kinds.values().map(|k| k.unevaluated).sum());
    completeness.insert("unevaluated_limit_checks", &uneval)?;
    match corners.as_ref().filter(|cs| !cs.is_empty()) {
        Some(cs) => {
            let mut unlimited = JsonObject::new();
            for k in DRV_KINDS {
                let n = cs
                    .iter()
                    .filter_map(|c| c.kinds.get(k.name()))
                    .map(|r| r.unlimited)
                    .max()
                    .unwrap_or(0);
                unlimited.insert(k.name(), &n)?;
            }
            completeness.insert("drv_unlimited", &unlimited)?;
        }
        None => {
            completeness.insert("drv_unlimited", &Option::<u64>::None)?;
        }
    }
    Ok(())
}

/// Converts a `JsonObject` into a `serde_json::Map<String, serde_json::Value>` (`JsonMap`) for
/// `Txn::set_history_metrics`.
pub fn json_object_to_map(obj: &JsonObject) -> JsonMap {
    serde_json::from_str(obj.to_raw().as_str()).unwrap_or_default()
}

/// Computes `metric_deltas` between `old_metrics` and `new_metrics`.
pub fn compute_metric_deltas(
    old_map: Option<&JsonMap>,
    new_map: &JsonMap,
) -> Result<JsonObject, Error> {
    let mut deltas = JsonObject::new();
    let Some(old) = old_map else {
        return Ok(deltas);
    };
    for key in [
        "wns_ns",
        "tns_ns",
        "violating_endpoints",
        "whs_ns",
        "ths_ns",
        "hold_violating_endpoints",
        "worst_setup_slack_ns",
        "worst_hold_slack_ns",
    ] {
        if let (Some(ov), Some(nv)) = (old.get(key), new_map.get(key)) {
            if let (Some(oi), Some(ni)) = (ov.as_i64(), nv.as_i64()) {
                deltas.insert(key, &(ni - oi))?;
            } else if let (Some(of), Some(nf)) = (ov.as_f64(), nv.as_f64()) {
                deltas.insert(key, &canon_f64(nf - of))?;
            }
        }
    }
    Ok(deltas)
}

/// Checks if a handle string has an underscore (`inst_841`, `path_1`, etc.) and returns a
/// suggestion without the underscore (`CONTRACT.md` §2).
pub fn strip_underscore_handle_suggestion(raw: &str) -> Option<String> {
    for prefix in ["inst", "net", "pin", "port", "path", "mm", "sel"] {
        if let Some(rest) = raw.strip_prefix(prefix)
            && let Some(digits) = rest.strip_prefix('_')
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return Some(format!("{prefix}{digits}"));
        }
    }
    None
}

/// Resolves a corner index in `store` from an optional corner name (`None` selects the worst
/// setup corner).
pub fn resolve_store_corner(
    store: &StaStore,
    corner: Option<&str>,
    diags: &mut Vec<Diagnostic>,
) -> Result<(usize, String), Error> {
    let v = store.view().map_err(map_store_error)?;
    if let Some(want) = corner {
        for (idx, cr) in v.corners.iter().enumerate() {
            let name = v.strings.get(cr.name_str).unwrap_or("");
            if name == want {
                return Ok((idx, name.to_string()));
            }
        }
        let msg = format!("corner '{want}' not found in timing table");
        diags.push(Diagnostic::new(
            Severity::Error,
            DiagCode::of("T0005"),
            msg.clone(),
        ));
        return Err(Error::input_rejected(msg));
    }

    let summary_rep = store.query_summary().map_err(map_store_error)?;
    let mut best_idx = 0usize;
    let mut best_wns = f64::INFINITY;
    for row in &summary_rep.rows {
        if row.path_group == "**" && row.setup_wns < best_wns {
            best_wns = row.setup_wns;
            best_idx = row.corner_idx;
        }
    }
    let name = v
        .corners
        .get(best_idx)
        .and_then(|c| v.strings.get(c.name_str).ok())
        .unwrap_or("default")
        .to_string();
    Ok((best_idx, name))
}

/// Returns the canonical 1-based handle (`port<N>` or `pin<N>`) for a graph `NodeId`.
pub fn node_handle_string(node: NodeId, num_port_bits: u32) -> String {
    if node.0 < num_port_bits {
        format!("port{}", node.0 + 1)
    } else {
        format!("pin{}", (node.0 - num_port_bits) + 1)
    }
}

/// Resolves the hierarchical pin or port name of `node` from `StaStoreView`.
pub fn resolve_node_name(
    v: &ruststa::engine::store::StaStoreView<'_>,
    node: NodeId,
) -> Option<String> {
    if let Some(nr) = v.node_rec.get(node.index())
        && nr.pad != ruststa::engine::store::NO_INDEX
        && let Ok(s) = v.name_hash.format_bucket(nr.pad, &v.strings)
    {
        return Some(s);
    }
    if let Some(nb) = v.name_hash.find_node_bucket(node)
        && let Ok(s) = v.name_hash.format_bucket(nb, &v.strings)
    {
        return Some(s);
    }
    None
}

/// The extracted parasitics of the corners `corners` of `rev`, from its `PARASIT` table: a
/// version 1 table binds one corner, a version 2 table one source per corner (STA-REQ-0700,
/// `STA-REQ-0820`, RCLI-REQ-0075). Only the requested corners are parsed; a requested corner
/// without a source is left out, and the analysis then rejects it (`T0070`). No table: empty.
pub fn load_parasitics(
    rev: &Revision,
    corners: &[String],
) -> Result<Vec<std::sync::Arc<ruststa::engine::delay::spef::ParasiticInput>>, Error> {
    use rusteda_formats::spef::{CornerSetMetadata, Metadata};
    if !rev.has_table("parasitics") {
        return Ok(Vec::new());
    }
    let rbin = rev.open_rbin("parasitics", "PARASIT", 1..=2)?;
    rbin.verify_hash()?;
    let bad_meta = |e: String| Error::input_rejected(format!("invalid PARASIT metadata: {e}"));
    // `(corner, limits, section)` of every corner of the table.
    let entries: Vec<(String, rusteda_formats::spef::Limits, u32)> = if rbin.format_version() == 1 {
        let metadata = rbin.section_bytes(2)?;
        if metadata.len() > 4096 {
            return Err(Error::input_rejected(
                "T0072: parasitic metadata budget exceeded",
            ));
        }
        let meta: Metadata =
            serde_json::from_slice(metadata).map_err(|e| bad_meta(e.to_string()))?;
        vec![(meta.corner, meta.limits, 1)]
    } else {
        let metadata = rbin.section_bytes(1)?;
        if metadata.len() > 64 * 4096 {
            return Err(Error::input_rejected(
                "T0072: parasitic metadata budget exceeded",
            ));
        }
        let set: CornerSetMetadata =
            serde_json::from_slice(metadata).map_err(|e| bad_meta(e.to_string()))?;
        set.corners
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c.corner, c.limits, i as u32 + 2))
            .collect()
    };
    let mut out = Vec::new();
    for (corner, limits, section) in entries {
        if !corners.is_empty() && !corners.contains(&corner) {
            continue;
        }
        let source = std::str::from_utf8(rbin.section_bytes(section)?)
            .map_err(|_| Error::input_rejected("SPEF table is not UTF-8"))?;
        let data = rusteda_formats::spef::read(source, limits)
            .map_err(|e| Error::input_rejected(e.to_string()))?;
        out.push(std::sync::Arc::new(
            ruststa::engine::delay::spef::ParasiticInput { corner, data },
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod extracted_path_tests {
    use super::*;
    use ruststa::engine::{store::QueriedPathPoint, types::Tr};
    // STA-REQ-0707: an output port keeps its port identity but its incoming hop is a wire.
    #[test]
    fn output_port_hop_is_included_in_wire_breakdown() {
        let mut path = QueriedPath {
            handle: "path1".into(),
            rank_idx: 0,
            corner_idx: 0,
            corner_name: "tt".into(),
            endpoint_idx: 0,
            endpoint_name: "y".into(),
            check_kind: "setup".into(),
            path_group: "vclk".into(),
            launch_clock: "vclk".into(),
            capture_clock: "vclk".into(),
            slack: 1.0,
            arrival: 1.9,
            required: 2.9,
            check_value: 0.0,
            points: Vec::new(),
        };
        for (i, (kind, arrival, incr)) in [
            (PathPointKind::Port, 1.0, 1.0),
            (PathPointKind::Net, 1.1, 0.1),
            (PathPointKind::Cell, 1.6, 0.5),
            (PathPointKind::Port, 1.9, 0.3),
        ]
        .into_iter()
        .enumerate()
        {
            path.points.push(QueriedPathPoint {
                node: NodeId(i as u32),
                name: i.to_string(),
                tr: Tr::Rise,
                kind,
                arrival,
                incr_delay: incr,
                slew: 0.1,
            });
        }
        let (levels, cell, wire, _, _) = path_delay_breakdown(&path);
        assert_eq!(levels, 1);
        assert_eq!(cell, 0.5);
        assert_eq!(wire, 0.4);
        assert!((cell + wire - (path.arrival - path.points[0].arrival)).abs() < 1e-14);
    }
}
