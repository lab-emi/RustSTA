// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa run` command (`STA-REQ-0302`); `ruststa update` is in [`crate::update`].

use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    BaseRev, Diagnostic, Envelope, Error, Invocation, JsonObject, Rev, Workspace, exit,
};
use ruststa::engine::store::StaStore;

use crate::args::{CheckMethod, RunArgs};
use crate::check::{validate_tolerance, verify_sta_store};
use crate::phases::PhaseLog;
use crate::ws_io::{
    StaRunOptions, json_object_to_map, map_store_error, parse_backend, run_sta_on_revision,
    top_paths_from_store,
};

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_run_text_lines(
    verb: &str,
    new_rev: Rev,
    base_rev: Rev,
    scope: &JsonObject,
    completeness: &JsonObject,
    metrics: &JsonObject,
    top_paths: &[JsonObject],
    next_actions: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("{new_rev}  {verb}  (base {base_rev})"));
    lines.push(format!("  scope: {}", scope.to_raw().as_str()));
    lines.push(format!("  endpoints: {}", completeness.to_raw().as_str()));
    lines.push(format!("  metrics: {}", metrics.to_raw().as_str()));
    for tp in top_paths {
        lines.push(format!("  path: {}", tp.to_raw().as_str()));
    }
    for next in next_actions {
        lines.push(next_line(next));
    }
    lines
}

/// `--max-state-gib` must be a finite positive number (`STA-REQ-0024`).
pub(crate) fn validate_state_gib(gib: f64) -> Result<(), Error> {
    if gib.is_finite() && gib > 0.0 {
        Ok(())
    } else {
        Err(Error::usage("--max-state-gib must be a finite number > 0"))
    }
}

/// Executes `ruststa run`.
pub fn exec_run(
    ws: &Workspace,
    ws_arg: &str,
    args: &RunArgs,
    inv: &Invocation,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    if let Err(err) =
        validate_tolerance(args.tolerance).and_then(|()| validate_state_gib(args.max_state_gib))
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
    let opts = StaRunOptions {
        corners: args.corners.clone(),
        backend_arg: args.backend.clone(),
        precision: args.precision,
        max_paths: args.max_paths,
        store_mode: args.sta_store,
        max_state_gib: args.max_state_gib,
    };

    let step_res = (|| {
        let base_rev = txn.base().clone();
        let (store_data, scope, comp, metrics) = run_sta_on_revision(ws, &base_rev, &opts, diags)?;

        let mut upstream_names = vec!["netlist", "constraints"];
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
        let upstream = txn.upstream(&upstream_names)?;
        let log = PhaseLog::get();
        PhaseLog::time(log, "stage_store", || {
            store_data.stage_to_txn(&mut txn, upstream)
        })?;

        // The staged table is the in-memory one: queried in place, never encoded twice.
        let opened_store = PhaseLog::time(log, "reopen_store", || {
            StaStore::from_data(store_data).map_err(map_store_error)
        })?;
        let (top_paths, truncated_paths) = PhaseLog::time(log, "top_paths", || {
            top_paths_from_store(&opened_store, args.max_paths)
        })?;

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

        Ok((scope, comp, metrics, top_paths, truncated_paths, check_res))
    })();

    let (scope, comp, metrics, top_paths, truncated_paths, check_res) = match step_res {
        Ok(v) => v,
        Err(err) => {
            let _ = txn.reject(&err);
            return Err(err);
        }
    };

    let committed = PhaseLog::time(PhaseLog::get(), "commit", || {
        txn.commit(json_object_to_map(&metrics))
    })?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("ruststa run");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base.rev());
    env.new_rev = Some(committed.rev);
    env.set_scope(scope.clone())?;
    env.set_completeness(comp.clone())?;
    env.set_metrics(metrics.clone())?;
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
        "ruststa run",
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
