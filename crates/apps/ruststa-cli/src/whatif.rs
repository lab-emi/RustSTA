// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa whatif` command: slack queries of candidate netlist edits, read-only
//! (`STA-REQ-0620` to `STA-REQ-0628`, roadmap X2).
//!
//! `whatif` opens a revision, builds a timing session of its netlist, libraries and constraints,
//! applies the base edits of an edit file (`rusteda.edits.v1`) and evaluates every candidate on
//! its own copy of the base session. It prints, for the base and for each candidate, the summary
//! of every corner and the slacks of the requested endpoints and ranked paths, each value equal
//! bit for bit to a full analysis of the edited netlist. It commits nothing, logs nothing, reads
//! no standard input and starts no process that outlives it.

use std::path::Path;
use std::time::{Duration, Instant};

use rusteda_formats::sdc::Sdc;
use rusteda_workspace::edits::{self, EditFile, Limits, is_numbered_handle};
use rusteda_workspace::sta::StaTable;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, JsonObject, Revision, Severity, Workspace, exit,
};
use ruststa::engine::analysis::AnalysisOptions;
use ruststa::engine::incr::{SessionError, SessionOptions, TimingSession};
use ruststa::engine::session_api::{
    CandidateOutcome, CheckSummary, EndpointTarget, Evaluation, PathTarget, QueryTargets, evaluate,
    evaluate_candidates, session_state_bytes,
};
use ruststa::engine::store::canon_f64;

use crate::args::WhatifArgs;
use crate::ws_io::{
    analysis_error, convert_job_diag, load_corner_libraries, load_sdc_from_revision,
};

/// Code of a requested endpoint or path that the revision does not have (`STA-REQ-0626`).
pub const CODE_UNKNOWN_TARGET: DiagCode = DiagCode::of("T0101");
/// Code of an edit file beyond `--max-candidates` or `--max-edits` (`STA-REQ-0627`).
pub const CODE_BOUNDS: DiagCode = DiagCode::of("T0106");
/// Code of an edit file that cannot be read (`STA-REQ-0622`).
pub const CODE_EDITS_FILE: DiagCode = DiagCode::of("T0107");
/// Code of candidates left unevaluated when `--timeout` ran out (`STA-REQ-0627`).
pub const CODE_TIMEOUT: DiagCode = DiagCode::of("T0108");
/// Code of a rejected candidate (`STA-REQ-0626`).
pub const CODE_REJECTED: DiagCode = DiagCode::of("T0111");
/// Code of the memory estimate of the sessions above `--max-state-gib` (`STA-REQ-0627`).
pub const CODE_STATE: DiagCode = DiagCode::of("T0023");

/// Hard upper bounds of `--max-candidates` and `--max-edits` (`STA-REQ-0627`).
pub const MAX_CANDIDATES: usize = 1024;
pub const MAX_EDITS: usize = 4096;

/// Copies of the session state alive at once: the base, a candidate and the analysis of an edit
/// (`STA-REQ-0627`).
const STATE_COPIES: u64 = 3;

fn reject(diags: &mut Vec<Diagnostic>, code: DiagCode, msg: String) -> Error {
    diags.push(Diagnostic::new(Severity::Error, code, msg.clone()));
    Error::input_rejected(msg)
}

/// Reads and checks the edit file at `path` within the bounds of `args` (`STA-REQ-0622`).
fn read_edit_file(
    path: &Path,
    args: &WhatifArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<EditFile, Error> {
    let limits = Limits {
        max_candidates: args.max_candidates,
        max_edits: args.max_edits,
        ..Limits::default()
    };
    let len = std::fs::metadata(path)
        .map_err(|e| reject(diags, CODE_EDITS_FILE, format!("{}: {e}", path.display())))?
        .len();
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) || len > limits.max_bytes as u64 {
        return Err(reject(
            diags,
            CODE_EDITS_FILE,
            format!(
                "{}: not a regular file of at most {} bytes",
                path.display(),
                limits.max_bytes
            ),
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| reject(diags, CODE_EDITS_FILE, format!("{}: {e}", path.display())))?;
    edits::parse(&text, &limits).map_err(|e| {
        let code = if e.code == edits::codes::LIMIT {
            CODE_BOUNDS
        } else {
            e.code
        };
        reject(diags, code, format!("{}: {e}", path.display()))
    })
}

/// The error of a session that failed, its diagnostics added to `diags`.
fn session_error(e: SessionError, diags: &mut Vec<Diagnostic>) -> Error {
    match e {
        SessionError::Diag(d) => analysis_error(&d, diags),
        SessionError::Edit(err) => {
            diags.push(err.to_diagnostic());
            Error::input_rejected(err.to_string())
        }
        other => Error::input_rejected(other.to_string()),
    }
}

/// Resolves the report request of the edit file against the base session and the timing table of
/// `rev` (`STA-REQ-0628`).
fn resolve_targets(
    session: &TimingSession,
    rev: &Revision,
    file: &EditFile,
    diags: &mut Vec<Diagnostic>,
) -> Result<QueryTargets, Error> {
    let mut table: Option<StaTable> = None;
    fn table_of<'t>(
        table: &'t mut Option<StaTable>,
        rev: &Revision,
        diags: &mut Vec<Diagnostic>,
    ) -> Result<&'t StaTable, Error> {
        if table.is_none() {
            let t = StaTable::open_rev(rev).map_err(|e| {
                diags.push(Diagnostic::new(
                    Severity::Error,
                    CODE_UNKNOWN_TARGET,
                    format!(
                        "handles ep<N> and path<N> need the timing table of {}: {e}",
                        rev.rev()
                    ),
                ));
                e
            })?;
            *table = Some(t);
        }
        table
            .as_ref()
            .ok_or_else(|| Error::internal("timing table not opened"))
    }
    let unknown = |diags: &mut Vec<Diagnostic>, handle: &str, reason: String| {
        diags.push(Diagnostic::new(
            Severity::Error,
            CODE_UNKNOWN_TARGET,
            format!("'{handle}': {reason}"),
        ));
        Error::UnknownHandle {
            handle: handle.to_string(),
            reason,
        }
    };
    let mut targets = QueryTargets::default();
    for label in &file.report.endpoints {
        let name = if session.key_of_name(label).is_some() {
            label.clone()
        } else if is_numbered_handle(label, "ep") {
            let n: usize = label[2..].parse().unwrap_or(0);
            let t = table_of(&mut table, rev, diags)?;
            t.endpoint(n - 1, 0)
                .map_err(|e| unknown(diags, label, e.to_string()))?
                .name
        } else {
            return Err(unknown(diags, label, "no such pin or port".into()));
        };
        let Some(key) = session.key_of_name(&name) else {
            return Err(unknown(
                diags,
                label,
                format!("'{name}' is not a connected pin or port"),
            ));
        };
        targets.endpoints.push(EndpointTarget {
            label: label.clone(),
            name,
            key,
        });
    }
    for label in &file.report.paths {
        let n: usize = label[4..].parse().unwrap_or(0);
        let t = table_of(&mut table, rev, diags)?;
        let p = t
            .path(n - 1)
            .map_err(|e| unknown(diags, label, e.to_string()))?;
        let Some(key) = session.key_of_name(&p.endpoint_name) else {
            return Err(unknown(
                diags,
                label,
                format!("its endpoint '{}' is not in the netlist", p.endpoint_name),
            ));
        };
        // The path's corner by name: `--corner` may analyze a subset of the table's corners.
        let Some(corner) = session
            .corners()
            .iter()
            .position(|c| c.corner_name == p.corner_name)
        else {
            return Err(unknown(
                diags,
                label,
                format!("its corner '{}' is not analyzed", p.corner_name),
            ));
        };
        targets.paths.push(PathTarget {
            label: label.clone(),
            endpoint: EndpointTarget {
                label: p.endpoint_name.clone(),
                name: p.endpoint_name,
                key,
            },
            corner,
            check: p.kind.as_str().to_string(),
        });
    }
    Ok(targets)
}

fn check_json(s: &CheckSummary) -> Result<JsonObject, Error> {
    let mut o = JsonObject::new();
    o.insert("wns_ns", &s.wns_ns)?;
    o.insert("tns_ns", &s.tns_ns)?;
    o.insert("violating_endpoints", &s.violating_endpoints)?;
    o.insert("worst_slack_ns", &s.worst_slack_ns)?;
    Ok(o)
}

/// The values of one evaluation in the envelope (`STA-REQ-0624`).
fn evaluation_json(
    e: &Evaluation,
    targets: &QueryTargets,
    corner_names: &[String],
) -> Result<(JsonObject, JsonObject), Error> {
    let s = &e.summary;
    let mut summary = JsonObject::new();
    let worst_setup = s.wns_setup_ns.map(canon_f64);
    let worst_hold = s.wns_hold_ns.map(canon_f64);
    summary.insert(
        "wns_ns",
        &worst_setup.map(|w| if w < 0.0 { w } else { 0.0 }),
    )?;
    summary.insert("tns_ns", &worst_setup.map(|_| canon_f64(s.tns_setup_ns)))?;
    summary.insert("violating_endpoints", &s.violating_setup)?;
    summary.insert("whs_ns", &worst_hold.map(|w| if w < 0.0 { w } else { 0.0 }))?;
    summary.insert("ths_ns", &worst_hold.map(|_| canon_f64(s.tns_hold_ns)))?;
    summary.insert("hold_violating_endpoints", &s.violating_hold)?;
    summary.insert("worst_setup_slack_ns", &worst_setup)?;
    summary.insert("worst_hold_slack_ns", &worst_hold)?;
    let mut corners = Vec::with_capacity(e.corners.len());
    for c in &e.corners {
        let mut o = JsonObject::new();
        o.insert("corner", &c.corner)?;
        o.insert("setup", &check_json(&c.setup)?)?;
        o.insert("hold", &check_json(&c.hold)?)?;
        corners.push(o);
    }
    let mut endpoints = Vec::with_capacity(targets.endpoints.len());
    for (t, slacks) in targets.endpoints.iter().zip(&e.endpoints) {
        let mut o = JsonObject::new();
        o.insert("endpoint", &t.label)?;
        o.insert("name", &t.name)?;
        o.insert("present", &slacks.is_some())?;
        let mut per = Vec::new();
        for (k, sl) in slacks.iter().flatten().enumerate() {
            let mut c = JsonObject::new();
            c.insert("corner", &corner_names.get(k))?;
            c.insert("setup_slack_ns", &sl.setup_ns)?;
            c.insert("hold_slack_ns", &sl.hold_ns)?;
            per.push(c);
        }
        o.insert("corners", &per)?;
        endpoints.push(o);
    }
    let mut paths = Vec::with_capacity(targets.paths.len());
    for (t, slack) in targets.paths.iter().zip(&e.paths) {
        let mut o = JsonObject::new();
        o.insert("path", &t.label)?;
        o.insert("endpoint", &t.endpoint.name)?;
        o.insert("corner", &corner_names.get(t.corner))?;
        o.insert("check", &t.check)?;
        o.insert("slack_ns", slack)?;
        paths.push(o);
    }
    let mut out = JsonObject::new();
    out.insert("summary", &summary)?;
    out.insert("corners", &corners)?;
    out.insert("endpoints", &endpoints)?;
    out.insert("paths", &paths)?;
    out.insert(
        "forward_nodes_evaluated",
        &(e.forward_nodes_evaluated as u64),
    )?;
    Ok((summary, out))
}

fn text_line(label: &str, e: &Evaluation) -> String {
    let f = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |x| format!("{x:.4}"));
    format!(
        "  {label}: wns {} tns {} ({} violating)  whs {} ths {} ({} violating)",
        f(e.summary.wns_setup_ns.map(|w| w.min(0.0))),
        f(e.summary.wns_setup_ns.map(|_| e.summary.tns_setup_ns)),
        e.summary.violating_setup,
        f(e.summary.wns_hold_ns.map(|w| w.min(0.0))),
        f(e.summary.wns_hold_ns.map(|_| e.summary.tns_hold_ns)),
        e.summary.violating_hold
    )
}

/// Executes `ruststa whatif` (`STA-REQ-0620` to `STA-REQ-0628`).
pub fn exec_whatif(
    ws: &Workspace,
    ws_arg: &str,
    args: &WhatifArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    if args.max_candidates > MAX_CANDIDATES || args.max_edits > MAX_EDITS {
        return Err(Error::usage(format!(
            "--max-candidates is at most {MAX_CANDIDATES} and --max-edits at most {MAX_EDITS}"
        )));
    }
    if !(args.max_state_gib.is_finite() && args.max_state_gib > 0.0) {
        return Err(Error::usage("--max-state-gib must be a finite number > 0"));
    }
    if args
        .timeout
        .is_some_and(|t| !(t.is_finite() && t > 0.0 && t < 1e9))
    {
        return Err(Error::usage(
            "--timeout must be a finite number of seconds > 0",
        ));
    }
    let start = Instant::now();
    let rev_id = ws.resolve_query(&args.rev)?;
    let rev = ws.revision(rev_id)?;
    let file = read_edit_file(&args.edits, args, diags)?;

    // The session of the revision (`STA-REQ-0621`).
    let max_state_bytes = (args.max_state_gib * f64::from(1u32 << 30)) as u64;
    let netlist_rbin = rusteda_workspace::netlist::open_rev(&rev)?;
    let netlist = rusteda_workspace::netlist::decode(&netlist_rbin)?
        .thaw()
        .map_err(|e| Error::internal(format!("cannot thaw the netlist of {rev_id}: {e}")))?;
    let sdc: Sdc = if rev.has_table("constraints") {
        load_sdc_from_revision(&rev)?.0
    } else {
        Sdc::default()
    };
    let corner_libs = load_corner_libraries(ws, &rev, &args.corners, diags)?;
    let corner_names: Vec<String> = corner_libs.iter().map(|(n, _)| n.clone()).collect();
    let options = SessionOptions {
        analysis: AnalysisOptions {
            parasitics: crate::ws_io::load_parasitics(&rev)?,
            max_state_bytes,
            ..AnalysisOptions::default()
        },
        intern_library_cells: false,
        backward: false,
    };
    let mut session = TimingSession::with_options(netlist, corner_libs, sdc, options)
        .map_err(|e| session_error(e, diags))?;
    let state = session_state_bytes(&session).saturating_mul(STATE_COPIES);
    if state > max_state_bytes {
        let gib = |b: u64| b as f64 / f64::from(1u32 << 30);
        return Err(reject(
            diags,
            CODE_STATE,
            format!(
                "the timing sessions of whatif need about {:.2} GiB, above the limit of {:.2} \
                 GiB; raise --max-state-gib",
                gib(state),
                gib(max_state_bytes)
            ),
        ));
    }
    for d in &session.state().diags {
        diags.push(convert_job_diag(d));
    }

    // Base edits, then the requested targets (`STA-REQ-0623`, `STA-REQ-0628`).
    if !file.base.is_empty() {
        session.apply_named_edits(&file.base).map_err(|e| {
            let e = match e {
                SessionError::Edit(err) => SessionError::Edit(err.within("base")),
                other => other,
            };
            session_error(e, diags)
        })?;
    }
    let targets = resolve_targets(&session, &rev, &file, diags)?;
    let base = evaluate(&session, &targets);
    let deadline = args.timeout.map(|t| start + Duration::from_secs_f64(t));
    let outcomes = evaluate_candidates(&session, &file, &targets, deadline);

    let mut lines = vec![format!(
        "{rev_id}  ruststa whatif  ({} base edits, {} candidates, read-only)",
        file.base.len(),
        file.candidates.len()
    )];
    lines.push(text_line("base", &base));
    let mut candidates = Vec::with_capacity(outcomes.len());
    let mut not_evaluated = 0usize;
    for (c, outcome) in file.candidates.iter().zip(&outcomes) {
        let mut o = JsonObject::new();
        o.insert("id", &c.id)?;
        match outcome {
            CandidateOutcome::Evaluated(e) => {
                o.insert("status", &"evaluated")?;
                o.insert_raw("error", rusteda_workspace::RawJson::null());
                o.insert("values", &evaluation_json(e, &targets, &corner_names)?.1)?;
                lines.push(text_line(&c.id, e));
            }
            CandidateOutcome::Rejected(err) => {
                let code = err.code().unwrap_or_else(|| "T0111".to_string());
                let message = err.to_string();
                o.insert("status", &"rejected")?;
                let mut eo = JsonObject::new();
                eo.insert("code", &code)?;
                eo.insert("message", &message)?;
                o.insert("error", &eo)?;
                o.insert_raw("values", rusteda_workspace::RawJson::null());
                diags.push(Diagnostic::new(
                    Severity::Warning,
                    CODE_REJECTED,
                    format!("candidate '{}' was rejected: {message}", c.id),
                ));
                lines.push(format!("  {}: rejected ({code}): {message}", c.id));
            }
            CandidateOutcome::NotEvaluated => {
                not_evaluated += 1;
                o.insert("status", &"not_evaluated")?;
                o.insert_raw("error", rusteda_workspace::RawJson::null());
                o.insert_raw("values", rusteda_workspace::RawJson::null());
                lines.push(format!("  {}: not evaluated (--timeout)", c.id));
            }
        }
        candidates.push(o);
    }
    if not_evaluated > 0 {
        diags.push(Diagnostic::new(
            Severity::Warning,
            CODE_TIMEOUT,
            format!(
                "--timeout ran out: {not_evaluated} of {} candidates were not evaluated",
                file.candidates.len()
            ),
        ));
    }

    let mut scope = JsonObject::new();
    let st = &session.state().scope;
    scope.insert("corners", &corner_names)?;
    scope.insert("wire_model", &st.wire_model)?;
    scope.insert(
        "parasitics",
        &if st.wire_model == "spef_rc" {
            "spef"
        } else {
            "none"
        },
    )?;
    if st.wire_model == "spef_rc" {
        scope.insert("delay_calculator", &"nldm_ceff_ramp")?;
        scope.insert("coupling", &"grounded_factor_1")?;
    }
    scope.insert("drc_status", &"unchecked")?;
    scope.insert("clock_model", &st.clock_model)?;
    scope.insert("cppr", &"off")?;
    scope.insert("backend", &"cpu")?;
    scope.insert("precision", &"f64")?;
    let comp = &session.state().completeness;
    let mut completeness = JsonObject::new();
    completeness.insert("constrained_endpoints", &session.sta_summary().endpoints)?;
    completeness.insert(
        "unconstrained_endpoints",
        &session.sta_summary().unconstrained_endpoints,
    )?;
    completeness.insert("unanalyzed_arcs", &comp.unanalyzed_arcs)?;
    let (base_summary, base_json) = evaluation_json(&base, &targets, &corner_names)?;

    let mut env = Envelope::new("ruststa whatif");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(rev_id);
    env.set_scope(scope)?;
    env.set_completeness(completeness)?;
    env.set_metrics(base_summary)?;
    let mut edits_info = JsonObject::new();
    edits_info.insert("schema", &edits::SCHEMA)?;
    edits_info.insert("base_edits", &(file.base.len() as u64))?;
    edits_info.insert("candidates", &(file.candidates.len() as u64))?;
    env.set("edits", &edits_info)?;
    env.set("base", &base_json)?;
    env.set("candidates", &candidates)?;
    let exit_code = if not_evaluated > 0 {
        exit::INCONCLUSIVE
    } else {
        exit::OK
    };
    Ok((env, lines, exit_code))
}
