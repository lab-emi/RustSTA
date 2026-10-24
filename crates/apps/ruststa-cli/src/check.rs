// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa check` command and `--check` verification helper (`STA-REQ-0520` to `STA-REQ-0529`).

use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    BaseRev, Diagnostic, Envelope, Error, Invocation, JsonObject, Revision, Workspace, exit,
};
use ruststa::engine::store::{StaStore, StorePrecision, canon_f64};
use ruststa_check::rev::{self, DEFAULT_TOLERANCE_NS};

use crate::args::{CheckArgs, CheckMethod, PrecisionArg, StoreArg};
use crate::ws_io::{
    StaRunOptions, envelope_headers_from_store, json_object_to_map, load_corner_libraries,
    load_sdc_from_revision, map_store_error, run_sta_on_revision,
};

/// Result of running `verify_sta_store` on a revision's `sta` table.
#[derive(Debug, Clone)]
pub struct CheckExecutionResult {
    pub verdict: &'static str,
    pub exit_code: i32,
    pub method: &'static str,
    pub methods: Vec<&'static str>,
    pub tolerance_ns: f64,
    pub slacks_compared: usize,
    pub max_abs_difference_ns: f64,
    pub mismatches: Vec<JsonObject>,
    pub undecided_reasons: Vec<String>,
    pub check_table_json: JsonObject,
}

impl CheckExecutionResult {
    /// Builds the compact `check` summary object embedded in `ruststa run --check` and
    /// `ruststa update --check` envelopes.
    pub fn to_summary_json(&self) -> Result<JsonObject, Error> {
        let mut obj = JsonObject::new();
        obj.insert("verdict", &self.verdict)?;
        obj.insert("method", &self.method)?;
        obj.insert("methods", &self.methods)?;
        obj.insert("tolerance_ns", &self.tolerance_ns)?;
        obj.insert("slacks_compared", &(self.slacks_compared as u64))?;
        obj.insert("max_abs_difference_ns", &self.max_abs_difference_ns)?;
        obj.insert("mismatches", &self.mismatches)?;
        if !self.undecided_reasons.is_empty() {
            obj.insert("undecided_reasons", &self.undecided_reasons)?;
        }
        Ok(obj)
    }
}

pub fn validate_tolerance(tolerance: Option<f64>) -> Result<(), Error> {
    if let Some(t) = tolerance
        && !(t.is_finite() && t > 0.0)
    {
        return Err(Error::usage("--tolerance must be a finite number > 0"));
    }
    Ok(())
}

fn effective_tolerance(
    override_tol: Option<f64>,
    precision: StorePrecision,
    ref_value: f64,
) -> f64 {
    if let Some(tol) = override_tol {
        tol
    } else if precision == StorePrecision::F32 {
        1e-4_f64.max(2e-5 * ref_value.abs())
    } else {
        DEFAULT_TOLERANCE_NS
    }
}

/// Records a mismatch where exactly one side times an endpoint check (`STA-REQ-0307`).
#[allow(clippy::too_many_arguments)]
fn push_status_mismatch(
    mismatches: &mut Vec<JsonObject>,
    code: &str,
    method: &str,
    corner: &str,
    object: &str,
    field: &str,
    reference_timed: bool,
    store_timed: bool,
) -> Result<(), Error> {
    let status = |timed: bool| if timed { "timed" } else { "untimed" };
    let handle = format!("mm{}", mismatches.len() + 1);
    let mut mm = JsonObject::new();
    mm.insert("handle", &handle)?;
    mm.insert("code", &code)?;
    mm.insert("method", &method)?;
    mm.insert("corner", &corner)?;
    mm.insert("object", &object)?;
    mm.insert("field", &field)?;
    mm.insert("checker", &status(reference_timed))?;
    mm.insert("sta", &status(store_timed))?;
    mm.insert(
        "message",
        &format!(
            "{field} at '{object}' in corner '{corner}': {} by the {method} check, {} in the \
             timing table",
            status(reference_timed),
            status(store_timed)
        ),
    )?;
    mismatches.push(mm);
    Ok(())
}

/// Verifies an existing `StaStore` against `--method independent|full|both`.
pub fn verify_sta_store(
    ws: &Workspace,
    base_rev: &Revision,
    store: &StaStore,
    method: CheckMethod,
    tolerance_override: Option<f64>,
    diags: &mut Vec<Diagnostic>,
) -> Result<CheckExecutionResult, Error> {
    validate_tolerance(tolerance_override)?;

    let scope_rep = store.query_scope().map_err(map_store_error)?;
    let summary_rep = store.query_summary().map_err(map_store_error)?;
    let precision = scope_rep.precision;
    let nominal_tol = tolerance_override.unwrap_or(if precision == StorePrecision::F32 {
        1e-4
    } else {
        DEFAULT_TOLERANCE_NS
    });
    let corner_names = scope_rep.corners.clone();

    let mut mismatches: Vec<JsonObject> = Vec::new();
    let mut undecided_reasons: Vec<String> = Vec::new();
    let mut slacks_compared = 0usize;
    let mut max_abs_diff = 0.0f64;

    let wants_independent = matches!(method, CheckMethod::Independent | CheckMethod::Both);
    let extracted = base_rev.has_table("parasitics") || scope_rep.clock_model != "ideal";
    let run_independent = wants_independent && !extracted;
    if wants_independent && extracted {
        undecided_reasons.push("independent checker does not yet support extracted RC or propagated clocks; use full recomputation and external RC differential validation".into());
    }
    let run_full = matches!(method, CheckMethod::Full | CheckMethod::Both);
    let methods: Vec<&'static str> = match method {
        CheckMethod::Independent => vec!["independent"],
        CheckMethod::Full => vec!["full"],
        CheckMethod::Both => vec!["independent", "full"],
    };

    if run_independent {
        let gate_nl = rusteda_workspace::netlist::thaw_rev(base_rev)?;
        let verilog_text = rev::verilog::gate_netlist_to_verilog(&gate_nl);
        let (_, sdc_text) = load_sdc_from_revision(base_rev)?;
        let corner_libs = load_corner_libraries(ws, base_rev, &corner_names, diags)?;
        let corner_libs_ref: Vec<(&str, Vec<_>)> = corner_libs
            .iter()
            .map(|(n, l)| (n.as_str(), l.clone()))
            .collect();

        match rev::analyze_inputs(
            &[("netlist.v", &verilog_text)],
            Some(gate_nl.name()),
            &corner_libs_ref,
            &sdc_text,
        ) {
            Ok(mc) => {
                for (c_idx, cname) in corner_names.iter().enumerate() {
                    let Some(ca) = mc.corners.get(cname.as_str()) else {
                        continue;
                    };
                    let mut store_eps = store
                        .query_endpoints(c_idx, None, usize::MAX)
                        .map_err(map_store_error)?;
                    store_eps.sort_by_key(|ep| ep.endpoint_idx);
                    for ep in &store_eps {
                        let (ref_setup, ref_hold) = ca.endpoint_slack(&ep.name);
                        for (field, store_val, ref_opt) in [
                            ("setup_slack_ns", ep.setup_slack, ref_setup),
                            ("hold_slack_ns", ep.hold_slack, ref_hold),
                        ] {
                            // An endpoint check that only one side times is a difference too
                            // (`STA-REQ-0307`): constants, exceptions and clock groups decide
                            // whether a check is timed at all.
                            let ref_timed = ref_opt.is_some_and(f64::is_finite);
                            if ref_timed != store_val.is_finite() {
                                slacks_compared += 1;
                                push_status_mismatch(
                                    &mut mismatches,
                                    "C0502",
                                    "independent",
                                    cname,
                                    &ep.name,
                                    field,
                                    ref_timed,
                                    store_val.is_finite(),
                                )?;
                                continue;
                            }
                            if let Some(ref_val) = ref_opt
                                && store_val.is_finite()
                                && ref_val.is_finite()
                            {
                                let diff = canon_f64((store_val - ref_val).abs());
                                max_abs_diff = max_abs_diff.max(diff);
                                slacks_compared += 1;
                                let tol =
                                    effective_tolerance(tolerance_override, precision, ref_val);
                                if diff > tol {
                                    let handle = format!("mm{}", mismatches.len() + 1);
                                    let mut mm = JsonObject::new();
                                    mm.insert("handle", &handle)?;
                                    mm.insert("code", &"C0500")?;
                                    mm.insert("method", &"independent")?;
                                    mm.insert("corner", cname)?;
                                    mm.insert("object", &ep.name)?;
                                    mm.insert("field", &field)?;
                                    mm.insert("checker", &canon_f64(ref_val))?;
                                    mm.insert("sta", &canon_f64(store_val))?;
                                    mm.insert("diff_ns", &diff)?;
                                    mm.insert(
                                        "message",
                                        &format!(
                                            "{} at '{}' in corner '{}': checker {:.6} ns vs sta {:.6} ns (diff {:.6e} ns > tol {:.6e} ns)",
                                            field, ep.name, cname, ref_val, store_val, diff, tol
                                        ),
                                    )?;
                                    mismatches.push(mm);
                                }
                            }
                        }
                    }

                    // Endpoints the checker times that the timing table does not list at all.
                    let listed: std::collections::BTreeSet<&str> =
                        store_eps.iter().map(|ep| ep.name.as_str()).collect();
                    let mut unlisted: Vec<&str> = ca
                        .endpoints
                        .iter()
                        .filter(|e| {
                            e.setup_slack_ns.is_some_and(f64::is_finite)
                                || e.hold_slack_ns.is_some_and(f64::is_finite)
                        })
                        .map(|e| e.endpoint.as_str())
                        .filter(|name| !listed.contains(name))
                        .collect();
                    unlisted.sort_unstable();
                    unlisted.dedup();
                    for name in unlisted {
                        slacks_compared += 1;
                        push_status_mismatch(
                            &mut mismatches,
                            "C0502",
                            "independent",
                            cname,
                            name,
                            "endpoint",
                            true,
                            false,
                        )?;
                    }

                    // Also compare corner-wide WNS.
                    if let Some(row) = summary_rep
                        .rows
                        .iter()
                        .find(|r| r.corner_idx == c_idx && r.path_group == "**")
                    {
                        for (field, store_val, ref_val) in [
                            ("setup_wns_ns", row.setup_wns, ca.metrics.wns_setup_ns),
                            ("hold_wns_ns", row.hold_wns, ca.metrics.wns_hold_ns),
                        ] {
                            if store_val.is_finite() && ref_val.is_finite() {
                                let diff = canon_f64((store_val - ref_val).abs());
                                max_abs_diff = max_abs_diff.max(diff);
                                let tol =
                                    effective_tolerance(tolerance_override, precision, ref_val);
                                if diff > tol {
                                    let handle = format!("mm{}", mismatches.len() + 1);
                                    let mut mm = JsonObject::new();
                                    mm.insert("handle", &handle)?;
                                    mm.insert("code", &"C0500")?;
                                    mm.insert("method", &"independent")?;
                                    mm.insert("corner", cname)?;
                                    mm.insert("object", cname)?;
                                    mm.insert("field", &field)?;
                                    mm.insert("checker", &canon_f64(ref_val))?;
                                    mm.insert("sta", &canon_f64(store_val))?;
                                    mm.insert("diff_ns", &diff)?;
                                    mm.insert(
                                        "message",
                                        &format!(
                                            "{} in corner '{}': checker {:.6} ns vs sta {:.6} ns",
                                            field, cname, ref_val, store_val
                                        ),
                                    )?;
                                    mismatches.push(mm);
                                }
                            }
                        }
                    }
                }
            }
            Err(Ok(unsupported)) => {
                undecided_reasons.push(unsupported.to_string());
            }
            Err(Err(msg)) => {
                undecided_reasons.push(msg);
            }
        }
    }

    if run_full {
        let opts = StaRunOptions {
            corners: corner_names.clone(),
            backend_arg: "cpu".to_string(),
            precision: PrecisionArg::F64,
            max_paths: 5,
            store_mode: StoreArg::Full,
            max_state_gib: 16.0,
            max_limit_rows: ruststa::engine::limits::DEFAULT_MAX_LIMIT_ROWS,
            require_wire_model: None,
            pulse_rules: ruststa::engine::limits::PulseRules::default(),
            clock_limits: ruststa::engine::limits::ClockLimitScope::default(),
        };
        let mut full_diags = Vec::new();
        let fresh_data = run_sta_on_revision(ws, base_rev, &opts, &mut full_diags)?.store;
        let fresh_bytes = fresh_data.to_bytes()?;
        let fresh_store = StaStore::from_bytes(fresh_bytes).map_err(map_store_error)?;

        for (c_idx, cname) in corner_names.iter().enumerate() {
            let mut store_eps = store
                .query_endpoints(c_idx, None, usize::MAX)
                .map_err(map_store_error)?;
            let mut fresh_eps = fresh_store
                .query_endpoints(c_idx, None, usize::MAX)
                .map_err(map_store_error)?;
            store_eps.sort_by_key(|ep| ep.endpoint_idx);
            fresh_eps.sort_by_key(|ep| ep.endpoint_idx);
            if store_eps.len() != fresh_eps.len() {
                let handle = format!("mm{}", mismatches.len() + 1);
                let mut mm = JsonObject::new();
                mm.insert("handle", &handle)?;
                mm.insert("code", &"C0504")?;
                mm.insert("method", &"full")?;
                mm.insert("corner", cname)?;
                mm.insert("object", cname)?;
                mm.insert("field", &"endpoints")?;
                mm.insert("checker", &(fresh_eps.len() as u64))?;
                mm.insert("sta", &(store_eps.len() as u64))?;
                mm.insert(
                    "message",
                    &format!(
                        "corner '{cname}': the timing table lists {} endpoints, a fresh full run {}",
                        store_eps.len(),
                        fresh_eps.len()
                    ),
                )?;
                mismatches.push(mm);
            }

            for (ep, f_ep) in store_eps.iter().zip(fresh_eps.iter()) {
                for (field, store_val, ref_val) in [
                    ("setup_slack_ns", ep.setup_slack, f_ep.setup_slack),
                    ("hold_slack_ns", ep.hold_slack, f_ep.hold_slack),
                ] {
                    if store_val.is_finite() != ref_val.is_finite() || ep.name != f_ep.name {
                        if !run_independent {
                            slacks_compared += 1;
                        }
                        push_status_mismatch(
                            &mut mismatches,
                            "C0504",
                            "full",
                            cname,
                            &ep.name,
                            field,
                            ref_val.is_finite(),
                            store_val.is_finite(),
                        )?;
                        continue;
                    }
                    if store_val.is_finite() && ref_val.is_finite() {
                        let diff = canon_f64((store_val - ref_val).abs());
                        max_abs_diff = max_abs_diff.max(diff);
                        if !run_independent {
                            slacks_compared += 1;
                        }
                        let tol = effective_tolerance(tolerance_override, precision, ref_val);
                        if diff > tol {
                            let handle = format!("mm{}", mismatches.len() + 1);
                            let mut mm = JsonObject::new();
                            mm.insert("handle", &handle)?;
                            mm.insert("code", &"C0501")?;
                            mm.insert("method", &"full")?;
                            mm.insert("corner", cname)?;
                            mm.insert("object", &ep.name)?;
                            mm.insert("field", &field)?;
                            mm.insert("checker", &canon_f64(ref_val))?;
                            mm.insert("sta", &canon_f64(store_val))?;
                            mm.insert("diff_ns", &diff)?;
                            mm.insert(
                                "message",
                                &format!(
                                    "{} at '{}' in corner '{}' differs from fresh full run by {:.6e} ns",
                                    field, ep.name, cname, diff
                                ),
                            )?;
                            mismatches.push(mm);
                        }
                    }
                }
            }
        }
    }

    let (verdict, exit_code) = if !mismatches.is_empty() {
        ("disagree", exit::NEGATIVE_VERDICT)
    } else if !undecided_reasons.is_empty() {
        ("undecided", exit::INCONCLUSIVE)
    } else {
        ("agree", exit::OK)
    };

    let mut check_table_json = JsonObject::new();
    check_table_json.insert("schema", &"ruststa.sta_check.v1")?;
    check_table_json.insert("verdict", &verdict)?;
    check_table_json.insert("method", &method.as_str())?;
    check_table_json.insert("methods", &methods)?;
    check_table_json.insert("tolerance_ns", &nominal_tol)?;
    check_table_json.insert("slacks_compared", &(slacks_compared as u64))?;
    check_table_json.insert("max_abs_difference_ns", &max_abs_diff)?;
    check_table_json.insert("mismatches", &mismatches)?;
    if !undecided_reasons.is_empty() {
        check_table_json.insert("undecided_reasons", &undecided_reasons)?;
    }

    Ok(CheckExecutionResult {
        verdict,
        exit_code,
        method: method.as_str(),
        methods,
        tolerance_ns: nominal_tol,
        slacks_compared,
        max_abs_difference_ns: max_abs_diff,
        mismatches,
        undecided_reasons,
        check_table_json,
    })
}

/// Runs `ruststa check`, committing the `sta_check` table (`sta_check.json`) into a new revision.
pub fn exec_check(
    ws: &Workspace,
    ws_arg: &str,
    args: &CheckArgs,
    inv: &Invocation,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    if let Err(err) = validate_tolerance(args.tolerance) {
        let base_opt = BaseRev::parse(&args.base_rev).ok().map(|b| b.rev());
        let _ = ws.log_rejected(inv, base_opt, &err);
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
    let step_res =
        (|| -> Result<(CheckExecutionResult, JsonObject, JsonObject, JsonObject), Error> {
            let base_rev = txn.base().clone();
            let store = StaStore::open_for_mutation_input(&base_rev).map_err(map_store_error)?;
            let (scope, comp, metrics) = envelope_headers_from_store(&store, None)?;
            let check_res =
                verify_sta_store(ws, &base_rev, &store, args.method, args.tolerance, diags)?;

            let check_upstream = txn.upstream(&["sta", "netlist", "constraints", "libraries"])?;
            let check_bytes = format!("{}\n", check_res.check_table_json.to_raw().as_str());
            txn.add_bytes(
                "sta_check",
                "sta_check.json",
                check_bytes.as_bytes(),
                check_upstream,
            )?;
            txn.set_status("sta_check", "verdict", check_res.verdict)?;
            txn.set_status("sta_check", "timing_check", check_res.verdict)?;
            txn.set_exit(check_res.exit_code)?;
            if let Some(msg) = &args.message {
                txn.set_message(msg.clone());
            }
            Ok((check_res, scope, comp, metrics))
        })();

    let (check_res, scope, comp, metrics) = match step_res {
        Ok(v) => v,
        Err(err) => {
            let _ = txn.reject(&err);
            return Err(err);
        }
    };

    let committed = txn.commit(json_object_to_map(&metrics))?;
    diags.extend(committed.warnings);

    let mut env = Envelope::new("ruststa check");
    env.workspace = Some(ws_arg.to_string());
    env.base_rev = Some(base.rev());
    env.new_rev = Some(committed.rev);
    env.set_scope(scope)?;
    env.set_completeness(comp)?;
    env.set_metrics(metrics)?;
    env.set("verdict", &check_res.verdict)?;
    env.set("method", &check_res.method)?;
    env.set("methods", &check_res.methods)?;
    env.set("tolerance_ns", &check_res.tolerance_ns)?;
    env.set("slacks_compared", &(check_res.slacks_compared as u64))?;
    env.set("max_abs_difference_ns", &check_res.max_abs_difference_ns)?;
    env.set("mismatches", &check_res.mismatches)?;
    if !check_res.undecided_reasons.is_empty() {
        env.set("undecided_reasons", &check_res.undecided_reasons)?;
    }

    let rev_str = committed.rev.to_string();
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

    let mut lines = Vec::new();
    lines.push(format!(
        "{}  ruststa check  (base {})",
        committed.rev,
        base.rev()
    ));
    lines.push(format!(
        "  verdict={}  method={}  tolerance={:.3e} ns  slacks_compared={}  max_abs_difference={:.3e} ns",
        check_res.verdict,
        check_res.method,
        check_res.tolerance_ns,
        check_res.slacks_compared,
        check_res.max_abs_difference_ns
    ));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok((env, lines, check_res.exit_code))
}
