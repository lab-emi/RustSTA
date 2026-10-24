// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa update` command (`STA-REQ-0610` to `STA-REQ-0613`).
//!
//! `update` analyzes `--base-rev` from the timing table of the nearest ancestor revision that has
//! one: the ancestor's netlist is lowered as the full analysis lowers it, the values of every node
//! and tag come from the ancestor's table, and only the cone of the nodes whose inputs changed is
//! propagated. Everything else is the code of `ruststa run`, so the committed `sta.rbin` is
//! byte-identical to the one `run` commits on the same revision (`STA-REQ-0611`). When the
//! ancestor's table cannot serve (no table, summary mode, other libraries, constraints, corners,
//! precision or ports, or too many changes), the revision is analyzed in full and the envelope
//! says why (`STA-REQ-0612`).

use rusteda_core::job::Ctx;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    BaseRev, DiagCode, Diagnostic, Envelope, Error, Invocation, JsonObject, Revision, Severity,
    Workspace, exit,
};
use ruststa::engine::analysis::EngineAnalysis;
use ruststa::engine::incr::{
    DEFAULT_MAX_DIRTY_FRACTION, FullUpdateReason, IncrementalStats, analyze_from_ancestor,
    select_ancestor,
};
use ruststa::engine::input::constr::CornerLibs;
use ruststa::engine::store::{StaStore, StaStoreData, StoreMode, StorePrecision, TABLE_NAME};

use crate::args::{CheckMethod, PrecisionArg, StoreArg, UpdateArgs};
use crate::check::{validate_tolerance, verify_sta_store};
use crate::run::{build_run_text_lines, validate_state_gib};
use crate::ws_io::{
    AnalysisTables, StaRunOptions, analysis_error, analysis_settings, check_wire_model,
    check_wire_model_inputs, compile_constraints, compute_metric_deltas,
    envelope_headers_from_store, json_object_to_map, load_corner_libraries, load_sdc_from_revision,
    map_store_error, parse_backend, table_of_analysis, top_paths_from_store, validate_limit_rows,
};
use ruststa::engine::limits::table::{LimitsData, TABLE_NAME as LIMITS_TABLE};

/// Code of an incremental table that differs from the table of a full computation
/// (`--verify-full`, `STA-REQ-0612`).
pub const CODE_VERIFY_FULL: DiagCode = DiagCode::of("T0109");
/// Code of the note that an update computed the whole graph, with the reason (`STA-REQ-0612`).
pub const CODE_FULL_FALLBACK: DiagCode = DiagCode::of("T0110");

/// How the timing of an updated revision was computed (`STA-REQ-0610`).
#[derive(Debug)]
pub struct UpdateOutcome {
    /// The timing table to commit.
    pub store: StaStoreData,
    /// The pin limit table to commit (`STA-REQ-0806`).
    pub limits: LimitsData,
    /// Envelope headers of the table.
    pub scope: JsonObject,
    pub completeness: JsonObject,
    pub metrics: JsonObject,
    /// The nearest ancestor revision with a timing table.
    pub from_rev: Option<String>,
    /// Why the whole graph was computed; `None` for an incremental computation.
    pub full_reason: Option<FullUpdateReason>,
    /// Changed instances and nets since the ancestor, when the netlists were compared.
    pub changed: Option<(usize, usize)>,
    /// Propagation statistics of the update.
    pub stats: IncrementalStats,
}

/// Analyzes `rev` incrementally from its nearest ancestor table when it can, else in full, and
/// builds the table that `ruststa run` would build (`STA-REQ-0610`, `STA-REQ-0611`). With
/// `verify_full`, an incremental table must equal the table of a full analysis byte for byte
/// (`STA-REQ-0612`).
pub fn update_sta_on_revision(
    ws: &Workspace,
    rev: &Revision,
    opts: &StaRunOptions,
    verify_full: bool,
    diags: &mut Vec<Diagnostic>,
) -> Result<UpdateOutcome, Error> {
    let (mut analysis_opts, backend, store_precision, store_mode) = analysis_settings(opts)?;
    check_wire_model_inputs(rev, opts.require_wire_model, diags)?;
    let netlist_rbin: Rbin = rusteda_workspace::netlist::open_rev(rev)?;
    let frozen: FrozenNetlist<'_> = rusteda_workspace::netlist::decode(&netlist_rbin)?;
    let (sdc_model, _) = load_sdc_from_revision(rev)?;
    let corner_libs = load_corner_libraries(ws, rev, &opts.corners, diags)?;
    let analyzed: Vec<String> = corner_libs.iter().map(|(n, _)| n.clone()).collect();
    analysis_opts.parasitics = crate::ws_io::load_parasitics(rev, &analyzed)?;
    let corner_refs: Vec<CornerLibs<'_>> = corner_libs
        .iter()
        .map(|(name, libs)| CornerLibs::new(name.as_str(), libs.as_slice()))
        .collect();
    let corner_names: Vec<String> = corner_libs.iter().map(|(n, _)| n.clone()).collect();
    let compiled = compile_constraints(&sdc_model, &frozen, &corner_refs, diags)?;

    let engine_err = |e: ruststa::engine::incr::UpdateError| match e {
        ruststa::engine::incr::UpdateError::Workspace(w) => w,
        ruststa::engine::incr::UpdateError::StaStore(s) => map_store_error(s),
        other => Error::internal(other.to_string()),
    };
    let choice = select_ancestor(
        ws,
        rev,
        &frozen,
        &corner_names,
        analysis_opts.precision,
        DEFAULT_MAX_DIRTY_FRACTION,
        false,
    )
    .map_err(engine_err)?;
    let from_rev = choice.ancestor_rev.map(|r| r.to_string());
    let changed = choice
        .diff
        .as_ref()
        .map(|d| (d.dirty_instances(), d.dirty_nets()));

    let mut incremental = None;
    let mut full_reason = None;
    match &choice.table {
        Ok(ancestor) => {
            match analyze_from_ancestor(
                &frozen,
                &corner_refs,
                &sdc_model,
                compiled.clone(),
                &analysis_opts,
                Ctx::NONE,
                ancestor,
            ) {
                Ok(Ok(done)) => incremental = Some(done),
                Ok(Err(reason)) => full_reason = Some(reason),
                Err(ruststa::engine::incr::UpdateError::Session(
                    ruststa::engine::incr::SessionError::Diag(d),
                )) => return Err(analysis_error(&d, diags)),
                Err(e) => return Err(engine_err(e)),
            }
        }
        Err(reason) => full_reason = Some(*reason),
    }

    let (analysis, stats) = match incremental {
        Some(done) => done,
        None => {
            let analysis = EngineAnalysis::analyze(
                &frozen,
                &corner_refs,
                compiled.clone(),
                &analysis_opts,
                Ctx::NONE,
            )
            .map_err(|d| analysis_error(&d, diags))?;
            let n = analysis.graph.num_nodes();
            let k = analysis.corners.len();
            let stats = IncrementalStats {
                total_nodes: n,
                forward_seeds: n,
                forward_nodes_evaluated: n * k,
                forward_nodes_changed: n,
                backward_seeds: n,
                backward_nodes_evaluated: n * k,
                backward_nodes_changed: n,
                ..IncrementalStats::default()
            };
            (analysis, stats)
        }
    };
    check_wire_model(&analysis, opts.require_wire_model, diags)?;
    let AnalysisTables {
        store,
        limits,
        scope,
        completeness,
        metrics,
    } = table_of_analysis(
        &analysis,
        None,
        &corner_libs,
        backend,
        store_precision,
        store_mode,
        opts.max_paths,
        opts.max_limit_rows,
        diags,
    )?;
    drop(analysis);

    if verify_full && full_reason.is_none() {
        let full =
            EngineAnalysis::analyze(&frozen, &corner_refs, compiled, &analysis_opts, Ctx::NONE)
                .map_err(|d| analysis_error(&d, diags))?;
        let mut scratch = Vec::new();
        let full_tables = table_of_analysis(
            &full,
            None,
            &corner_libs,
            backend,
            store_precision,
            store_mode,
            opts.max_paths,
            opts.max_limit_rows,
            &mut scratch,
        )?;
        if full_tables.store.to_bytes()? != store.to_bytes()? {
            let msg = "--verify-full: the incrementally computed sta.rbin differs from the \
                       sta.rbin of a full computation";
            diags.push(Diagnostic::new(Severity::Error, CODE_VERIFY_FULL, msg));
            return Err(Error::input_rejected(msg));
        }
        // The limit table of the same analysis is byte-identical too (`STA-REQ-0806`).
        if full_tables.limits.to_bytes()? != limits.to_bytes()? {
            let msg = "--verify-full: the incrementally computed limits.rbin differs from the \
                       limits.rbin of a full computation";
            diags.push(Diagnostic::new(Severity::Error, CODE_VERIFY_FULL, msg));
            return Err(Error::input_rejected(msg));
        }
    }
    if let Some(reason) = full_reason {
        diags.push(Diagnostic::new(
            Severity::Note,
            CODE_FULL_FALLBACK,
            format!(
                "computed the whole timing graph: {} (from {})",
                reason.as_str(),
                from_rev.as_deref().unwrap_or("no ancestor table")
            ),
        ));
    }
    Ok(UpdateOutcome {
        store,
        limits,
        scope,
        completeness,
        metrics,
        from_rev,
        full_reason,
        changed,
        stats,
    })
}

/// The `incremental` object of the envelope (`STA-REQ-0610`).
fn incremental_object(o: &UpdateOutcome) -> Result<JsonObject, Error> {
    let mut inc = JsonObject::new();
    let (mode, computation) = match o.full_reason {
        None => ("incremental", "incremental"),
        Some(_) => ("full", "full"),
    };
    inc.insert("from_rev", &o.from_rev)?;
    inc.insert("mode", &mode)?;
    inc.insert("computation", &computation)?;
    inc.insert("reason", &o.full_reason.map(FullUpdateReason::as_str))?;
    let (ci, cn) = o.changed.unwrap_or((0, 0));
    inc.insert("changed_instances", &(ci as u64))?;
    inc.insert("changed_nets", &(cn as u64))?;
    inc.insert("dirty_nodes", &(o.stats.forward_seeds as u64))?;
    inc.insert("evaluated_nodes", &(o.stats.forward_nodes_evaluated as u64))?;
    inc.insert("changed_nodes", &(o.stats.forward_nodes_changed as u64))?;
    inc.insert("total_nodes", &(o.stats.total_nodes as u64))?;
    Ok(inc)
}

fn find_ancestor_store(ws: &Workspace, start: &Revision) -> Option<StaStore> {
    let mut cur = Some(start.rev());
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = cur {
        if !seen.insert(id) {
            break;
        }
        let rev = ws.revision(id).ok()?;
        if rev.has_table(TABLE_NAME)
            && let Ok(store) = StaStore::open_for_mutation_input(&rev)
        {
            return Some(store);
        }
        cur = rev.meta().parent;
    }
    None
}

/// Executes `ruststa update` (`STA-REQ-0610` to `STA-REQ-0613`).
pub fn exec_update(
    ws: &Workspace,
    ws_arg: &str,
    args: &UpdateArgs,
    inv: &Invocation,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    if let Err(err) = validate_tolerance(args.tolerance)
        .and_then(|()| validate_state_gib(args.max_state_gib))
        .and_then(|()| validate_limit_rows(args.max_limit_rows))
    {
        let parsed = BaseRev::parse(&args.base_rev).ok().map(|b| b.rev());
        let _ = ws.log_rejected(inv, parsed, &err);
        return Err(err);
    }
    if let Err(err) = parse_backend(&args.backend) {
        let parsed = BaseRev::parse(&args.base_rev).ok().map(|b| b.rev());
        let _ = ws.log_rejected(inv, parsed, &err);
        return Err(err);
    }

    let base: BaseRev = match ws.resolve_base(&args.base_rev) {
        Ok(b) => b,
        Err(err) => {
            let parsed = BaseRev::parse(&args.base_rev).ok().map(|b| b.rev());
            let _ = ws.log_rejected(inv, parsed, &err);
            return Err(err);
        }
    };

    let mut txn = ws.begin(base, inv)?;
    let base_rev = txn.base().clone();

    let mut upstream_names = vec!["netlist", "constraints"];
    if txn.base().has_table("parasitics") {
        upstream_names.push("parasitics");
    }
    let corner_lib_tables: Vec<String>;
    if args.corners.is_empty() {
        upstream_names.push("libraries");
    } else {
        corner_lib_tables = args
            .corners
            .iter()
            .map(|c| format!("libraries.{c}"))
            .collect();
        for s in &corner_lib_tables {
            upstream_names.push(s.as_str());
        }
    }
    let current_upstream = match txn.upstream(&upstream_names) {
        Ok(u) => u,
        Err(err) => {
            let _ = txn.reject(&err);
            return Err(err);
        }
    };

    // A current table of the same options needs no update (`STA-REQ-0610`).
    if base_rev.has_table(TABLE_NAME) {
        let existing_store = match StaStore::open_for_mutation_input(&base_rev) {
            Ok(s) => s,
            Err(e) => {
                let err = map_store_error(e);
                let _ = txn.reject(&err);
                return Err(err);
            }
        };
        let scope_rep = existing_store.query_scope().map_err(map_store_error)?;
        let want_prec = match args.precision {
            PrecisionArg::F64 => StorePrecision::F64,
            PrecisionArg::F32 => StorePrecision::F32,
        };
        let want_mode = match args.sta_store {
            StoreArg::Full => StoreMode::Full,
            StoreArg::Summary => StoreMode::Summary,
        };
        let entry_matches = base_rev
            .table(TABLE_NAME)
            .is_ok_and(|t| t.upstream == current_upstream);
        // A table of an older analysis without the limit checks is analyzed again.
        let limits_current = base_rev
            .table(LIMITS_TABLE)
            .is_ok_and(|t| t.upstream == current_upstream);
        let wire_ok = args
            .require_wire_model
            .is_none_or(|w| scope_rep.wire_model == w.as_str());

        if entry_matches
            && limits_current
            && wire_ok
            && scope_rep.precision == want_prec
            && scope_rep.store_mode == want_mode
            && !args.check
        {
            if args.verify_full {
                let vres = match verify_sta_store(
                    ws,
                    &base_rev,
                    &existing_store,
                    CheckMethod::Full,
                    args.tolerance,
                    diags,
                ) {
                    Ok(v) => v,
                    Err(err) => {
                        let _ = txn.reject(&err);
                        return Err(err);
                    }
                };
                if vres.exit_code != exit::OK {
                    let err = Error::input_rejected(format!(
                        "--verify-full disagreed with stored sta.rbin (max_abs_difference_ns={:.3e})",
                        vres.max_abs_difference_ns
                    ));
                    let _ = txn.reject(&err);
                    return Err(err);
                }
            }

            let _ = txn.abort();
            let v = existing_store.view().map_err(map_store_error)?;
            let total_nodes = v.meta.num_nodes;
            let (scope, comp, metrics) = envelope_headers_from_store(&existing_store, None)?;
            let (top_paths, truncated_paths) =
                top_paths_from_store(&existing_store, args.max_paths)?;

            let mut inc = JsonObject::new();
            inc.insert("from_rev", &base.rev().to_string())?;
            inc.insert("mode", &"up_to_date")?;
            inc.insert("computation", &"none")?;
            inc.insert("reason", &"up_to_date")?;
            inc.insert("changed_instances", &0u32)?;
            inc.insert("changed_nets", &0u32)?;
            inc.insert("dirty_nodes", &0u32)?;
            inc.insert("evaluated_nodes", &0u32)?;
            inc.insert("changed_nodes", &0u32)?;
            inc.insert("total_nodes", &total_nodes)?;

            let mut env = Envelope::new("ruststa update");
            env.workspace = Some(ws_arg.to_string());
            env.base_rev = Some(base.rev());
            env.new_rev = None;
            env.set_scope(scope)?;
            env.set_completeness(comp)?;
            env.set_metrics(metrics)?;
            env.set("incremental", &inc)?;
            env.set("metric_deltas", &JsonObject::new())?;
            env.set("top_paths", &top_paths)?;
            env.set("truncated_paths", &truncated_paths)?;

            let rev_str = base.rev().to_string();
            env.push_next_action(&[
                "ruststa",
                "report",
                "-w",
                ws_arg,
                "--rev",
                &rev_str,
                "--section",
                "summary",
            ]);

            let mut lines = vec![format!(
                "{}  ruststa update  (up to date, no new revision)",
                base.rev()
            )];
            for next in &env.next_actions {
                lines.push(next_line(next));
            }
            return Ok((env, lines, exit::OK));
        }
    }

    let old_metrics_map = find_ancestor_store(ws, &base_rev)
        .and_then(|s| envelope_headers_from_store(&s, None).ok())
        .map(|(_, _, m)| json_object_to_map(&m));

    let opts = StaRunOptions {
        corners: args.corners.clone(),
        backend_arg: args.backend.clone(),
        precision: args.precision,
        max_paths: args.max_paths,
        store_mode: args.sta_store,
        max_state_gib: args.max_state_gib,
        max_limit_rows: args.max_limit_rows,
        require_wire_model: args.require_wire_model,
        pulse_rules: crate::args::pulse_rules(args.pulse_groups, args.pulse_pins),
        clock_limits: crate::args::clock_limits(args.clock_limits),
    };

    let step_res = (|| {
        let outcome = update_sta_on_revision(ws, &base_rev, &opts, args.verify_full, diags)?;
        let inc = incremental_object(&outcome)?;
        let UpdateOutcome {
            store,
            limits,
            scope,
            completeness,
            metrics,
            ..
        } = outcome;

        store.stage_to_txn(&mut txn, current_upstream.clone())?;
        limits.stage_to_txn(&mut txn, current_upstream)?;
        let store_bytes = store.to_bytes()?;
        let opened_store = StaStore::from_bytes(store_bytes).map_err(map_store_error)?;
        let (top_paths, truncated_paths) = top_paths_from_store(&opened_store, args.max_paths)?;

        let check_res = if args.check {
            let res = verify_sta_store(
                ws,
                &base_rev,
                &opened_store,
                CheckMethod::Independent,
                args.tolerance,
                diags,
            )?;
            let check_upstream = txn.upstream(&["sta", "netlist", "constraints", "libraries"])?;
            let check_bytes = format!("{}\n", res.check_table_json.to_raw().as_str());
            txn.add_bytes(
                "sta_check",
                "sta_check.json",
                check_bytes.as_bytes(),
                check_upstream,
            )?;
            txn.set_status("sta_check", "verdict", res.verdict)?;
            txn.set_status("sta_check", "timing_check", res.verdict)?;
            txn.set_exit(res.exit_code)?;
            Some(res)
        } else {
            None
        };

        if let Some(msg) = &args.message {
            txn.set_message(msg.clone());
        }
        Ok((
            scope,
            completeness,
            metrics,
            inc,
            top_paths,
            truncated_paths,
            check_res,
        ))
    })();

    let (scope, comp, metrics, inc, top_paths, truncated_paths, check_res) = match step_res {
        Ok(v) => v,
        Err(err) => {
            let _ = txn.reject(&err);
            return Err(err);
        }
    };

    let new_metrics_map = json_object_to_map(&metrics);
    let metric_deltas = compute_metric_deltas(old_metrics_map.as_ref(), &new_metrics_map)?;
    let committed = txn.commit(new_metrics_map)?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("ruststa update");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base.rev());
    env.new_rev = Some(committed.rev);
    env.set_scope(scope.clone())?;
    env.set_completeness(comp.clone())?;
    env.set_metrics(metrics.clone())?;
    env.set("incremental", &inc)?;
    env.set("metric_deltas", &metric_deltas)?;
    env.set("top_paths", &top_paths)?;
    env.set("truncated_paths", &truncated_paths)?;
    if let Some(cr) = &check_res {
        env.set("check", &cr.to_summary_json()?)?;
    }

    let rev_str = committed.rev.to_string();
    if !top_paths.is_empty() {
        env.push_next_action(&[
            "ruststa", "path", "-w", ws_arg, "--rev", &rev_str, "--target", "path1", "--format",
            "json",
        ]);
    }
    env.push_next_action(&[
        "ruststa",
        "report",
        "-w",
        ws_arg,
        "--rev",
        &rev_str,
        "--section",
        "endpoints",
        "--filter",
        "setup<0",
        "--format",
        "json",
    ]);

    let exit_code = check_res.map_or(exit::OK, |c| c.exit_code);
    let lines = build_run_text_lines(
        "ruststa update",
        committed.rev,
        base.rev(),
        &scope,
        &comp,
        &metrics,
        &top_paths,
        &env.next_actions,
    );

    Ok((env, lines, exit_code))
}
