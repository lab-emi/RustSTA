// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Report comparison (`reports/timing.json`, `reports/scope.json`) and workspace test helpers
//! (`STA-REQ-0500`, `0502`, `0504`).

use std::collections::BTreeSet;
use std::path::Path;

use rusteda_workspace::{Invocation, JsonMap, Upstream, Workspace};

use super::checks_eval::{CheckKindEval, EndpointStatus, PathEval};
use super::{
    CheckRevisionOutcome, CornerAnalysis, Mismatch, MultiCornerAnalysis, ScopeReport, SlackMetrics,
    Verdict,
};

/// Checks an in-memory analysis against `timing_json` and optional `scope_json` within
/// `tolerance_ns` (`STA-REQ-0500`, `0504`).
pub fn check_against_reports(
    analysis: &MultiCornerAnalysis,
    timing_json: &serde_json::Value,
    scope_json: Option<&serde_json::Value>,
    tolerance_ns: f64,
) -> CheckRevisionOutcome {
    let mut mismatches = Vec::new();
    let mut endpoints_compared = 0usize;
    let mut paths_compared = 0usize;
    let mut max_abs_diff = 0.0f64;

    let mut mm_counter = 0usize;
    let mut next_mm = || {
        mm_counter += 1;
        format!("mm{mm_counter}")
    };

    let default_ca = analysis.default_corner();

    // 1. Compare overall WNS / TNS / violating counts if present in `timing_json`.
    compare_summary_metrics(
        &analysis.overall,
        timing_json,
        "overall",
        tolerance_ns,
        &mut max_abs_diff,
        &mut mismatches,
        &mut next_mm,
    );

    // 2. Compare per-corner reports if `corners` object exists in `timing_json`.
    if let Some(corners_obj) = timing_json.get("corners").and_then(|v| v.as_object()) {
        for (cname, c_val) in corners_obj {
            if let Some(ca) = analysis.corners.get(cname) {
                compare_summary_metrics(
                    &ca.metrics,
                    c_val,
                    cname,
                    tolerance_ns,
                    &mut max_abs_diff,
                    &mut mismatches,
                    &mut next_mm,
                );
                if let Some(eps_arr) = c_val.get("endpoints").and_then(|v| v.as_array()) {
                    compare_endpoint_array(
                        ca,
                        cname,
                        eps_arr,
                        tolerance_ns,
                        &mut endpoints_compared,
                        &mut max_abs_diff,
                        &mut mismatches,
                        &mut next_mm,
                    );
                }
            } else {
                let h = next_mm();
                mismatches.push(Mismatch {
                    handle: h,
                    code: "C0520".to_string(),
                    corner: cname.clone(),
                    object: cname.clone(),
                    field: "corner".to_string(),
                    checker: "absent".to_string(),
                    report: "present".to_string(),
                    diff_ns: None,
                    message: format!("report contains unknown corner '{cname}'"),
                });
            }
        }
    }

    // 3. Compare top-level `endpoints` array in `timing_json`.
    if let Some(eps_arr) = timing_json.get("endpoints").and_then(|v| v.as_array()) {
        compare_endpoint_array(
            default_ca,
            &default_ca.corner,
            eps_arr,
            tolerance_ns,
            &mut endpoints_compared,
            &mut max_abs_diff,
            &mut mismatches,
            &mut next_mm,
        );
    }

    // 4. Compare top-level `paths` array in `timing_json` (`STA-REQ-0215`, `0228`).
    if let Some(paths_arr) = timing_json.get("paths").and_then(|v| v.as_array()) {
        compare_paths_array(
            default_ca,
            paths_arr,
            tolerance_ns,
            &mut paths_compared,
            &mut max_abs_diff,
            &mut mismatches,
            &mut next_mm,
        );
    }

    // 5. Compare `scope_json` (or `scope` / `completeness` inside `timing_json`).
    if let Some(s_val) = scope_json {
        compare_scope_report(
            &default_ca.scope,
            &default_ca.corner,
            s_val,
            &mut mismatches,
            &mut next_mm,
        );
    }

    for d in &analysis.diagnostics {
        if d.code == "C0502" {
            let h = next_mm();
            mismatches.push(Mismatch {
                handle: h,
                code: "C0502".to_string(),
                corner: default_ca.corner.clone(),
                object: "paths".to_string(),
                field: "exhaustive_enumeration".to_string(),
                checker: "consistent".to_string(),
                report: "inconsistent".to_string(),
                diff_ns: None,
                message: d.message.clone(),
            });
        }
    }

    let verdict = if mismatches.is_empty() {
        Verdict::Agree
    } else {
        Verdict::Disagree { mismatches }
    };

    CheckRevisionOutcome {
        verdict,
        tolerance_ns,
        endpoints_compared,
        paths_compared,
        max_abs_diff_ns: max_abs_diff,
        diagnostics: analysis.diagnostics.clone(),
    }
}

fn compare_summary_metrics(
    ours: &SlackMetrics,
    json: &serde_json::Value,
    corner_label: &str,
    tolerance_ns: f64,
    max_abs_diff: &mut f64,
    mismatches: &mut Vec<Mismatch>,
    next_mm: &mut impl FnMut() -> String,
) {
    let setup_wns = json
        .get("wns_setup")
        .or_else(|| json.get("setup").and_then(|s| s.get("wns")))
        .and_then(serde_json::Value::as_f64);
    let setup_tns = json
        .get("tns_setup")
        .or_else(|| json.get("setup").and_then(|s| s.get("tns")))
        .and_then(serde_json::Value::as_f64);
    let setup_viol = json
        .get("violating_setup")
        .or_else(|| json.get("setup").and_then(|s| s.get("violating")))
        .and_then(serde_json::Value::as_u64);

    let hold_wns = json
        .get("wns_hold")
        .or_else(|| json.get("hold").and_then(|s| s.get("wns")))
        .and_then(serde_json::Value::as_f64);
    let hold_tns = json
        .get("tns_hold")
        .or_else(|| json.get("hold").and_then(|s| s.get("tns")))
        .and_then(serde_json::Value::as_f64);
    let hold_viol = json
        .get("violating_hold")
        .or_else(|| json.get("hold").and_then(|s| s.get("violating")))
        .and_then(serde_json::Value::as_u64);

    for (field, mine, theirs_opt) in [
        ("wns_setup", ours.wns_setup_ns, setup_wns),
        ("tns_setup", ours.tns_setup_ns, setup_tns),
        ("wns_hold", ours.wns_hold_ns, hold_wns),
        ("tns_hold", ours.tns_hold_ns, hold_tns),
    ] {
        if let Some(theirs) = theirs_opt {
            let d = (mine - theirs).abs();
            *max_abs_diff = max_abs_diff.max(d);
            if d > tolerance_ns {
                let h = next_mm();
                mismatches.push(Mismatch {
                    handle: h,
                    code: "C0520".to_string(),
                    corner: corner_label.to_string(),
                    object: corner_label.to_string(),
                    field: field.to_string(),
                    checker: format!("{mine:.6}"),
                    report: format!("{theirs:.6}"),
                    diff_ns: Some(d),
                    message: format!(
                        "{corner_label} {field}: checker {mine:.6} ns vs report {theirs:.6} ns (diff {d:.6} ns)"
                    ),
                });
            }
        }
    }

    for (field, mine, theirs_opt) in [
        ("violating_setup", ours.violating_setup, setup_viol),
        ("violating_hold", ours.violating_hold, hold_viol),
    ] {
        if let Some(theirs) = theirs_opt
            && (theirs as usize) != mine
        {
            let h = next_mm();
            mismatches.push(Mismatch {
                handle: h,
                code: "C0520".to_string(),
                corner: corner_label.to_string(),
                object: corner_label.to_string(),
                field: field.to_string(),
                checker: mine.to_string(),
                report: theirs.to_string(),
                diff_ns: None,
                message: format!("{corner_label} {field}: checker {mine} vs report {theirs}"),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_endpoint_array(
    ca: &CornerAnalysis,
    corner_label: &str,
    eps_arr: &[serde_json::Value],
    tolerance_ns: f64,
    endpoints_compared: &mut usize,
    max_abs_diff: &mut f64,
    mismatches: &mut Vec<Mismatch>,
    next_mm: &mut impl FnMut() -> String,
) {
    let mut seen_pins = BTreeSet::new();
    for item in eps_arr {
        let pin = item
            .get("endpoint")
            .or_else(|| item.get("pin"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if pin.is_empty() {
            continue;
        }
        seen_pins.insert(pin.to_string());

        let (our_s, our_h) = if let Some(k_str) = item.get("kind").and_then(|v| v.as_str()) {
            let kind = match k_str {
                "recovery" | "removal" | "recovery_removal" => CheckKindEval::RecoveryRemoval,
                "latch" | "latch_setup" | "latch_hold" | "latch_setup_hold" => {
                    CheckKindEval::LatchSetupHold
                }
                "clock_gating" | "clock_gating_setup" | "clock_gating_hold" => {
                    CheckKindEval::ClockGating
                }
                "output" | "output_delay" | "output_setup" | "output_hold" => {
                    CheckKindEval::OutputDelay
                }
                _ => CheckKindEval::SetupHold,
            };
            ca.endpoint_slack_kind(pin, kind)
        } else {
            ca.endpoint_slack(pin)
        };

        if let Some(rep_status) = item.get("status").and_then(|v| v.as_str())
            && let Some(our_status) = ca.endpoint_status(pin)
            && our_status.as_str() != rep_status
        {
            let h = next_mm();
            mismatches.push(Mismatch {
                handle: h,
                code: "C0512".to_string(),
                corner: corner_label.to_string(),
                object: pin.to_string(),
                field: "status".to_string(),
                checker: our_status.as_str().to_string(),
                report: rep_status.to_string(),
                diff_ns: None,
                message: format!(
                    "endpoint '{pin}' status: checker '{}' vs report '{rep_status}'",
                    our_status.as_str()
                ),
            });
        }

        let rep_s = item
            .get("setup_slack")
            .or_else(|| item.get("setup"))
            .and_then(serde_json::Value::as_f64);
        let rep_h = item
            .get("hold_slack")
            .or_else(|| item.get("hold"))
            .and_then(serde_json::Value::as_f64);

        for (code, field, mine, theirs) in [
            ("C0510", "setup_slack", our_s, rep_s),
            ("C0511", "hold_slack", our_h, rep_h),
        ] {
            match (mine, theirs) {
                (Some(a), Some(b)) => {
                    *endpoints_compared += 1;
                    let d = (a - b).abs();
                    *max_abs_diff = max_abs_diff.max(d);
                    if d > tolerance_ns {
                        let h = next_mm();
                        mismatches.push(Mismatch {
                            handle: h,
                            code: code.to_string(),
                            corner: corner_label.to_string(),
                            object: pin.to_string(),
                            field: field.to_string(),
                            checker: format!("{a:.6}"),
                            report: format!("{b:.6}"),
                            diff_ns: Some(d),
                            message: format!(
                                "endpoint '{pin}' {field}: checker {a:.6} ns vs report {b:.6} ns (diff {d:.6} ns)"
                            ),
                        });
                    }
                }
                (None, None) => {}
                (a, b) => {
                    let h = next_mm();
                    mismatches.push(Mismatch {
                        handle: h,
                        code: code.to_string(),
                        corner: corner_label.to_string(),
                        object: pin.to_string(),
                        field: field.to_string(),
                        checker: a.map_or_else(|| "none".to_string(), |v| format!("{v:.6}")),
                        report: b.map_or_else(|| "none".to_string(), |v| format!("{v:.6}")),
                        diff_ns: None,
                        message: format!(
                            "endpoint '{pin}' {field}: checker {:?} vs report {:?}",
                            a, b
                        ),
                    });
                }
            }
        }
    }

    // Check for constrained endpoints in the checker that are missing from `eps_arr`.
    for ep in &ca.endpoints {
        if ep.status == EndpointStatus::Constrained && !seen_pins.contains(&ep.endpoint) {
            let h = next_mm();
            mismatches.push(Mismatch {
                handle: h,
                code: "C0513".to_string(),
                corner: corner_label.to_string(),
                object: ep.endpoint.clone(),
                field: "endpoint".to_string(),
                checker: "constrained".to_string(),
                report: "missing".to_string(),
                diff_ns: None,
                message: format!("constrained endpoint '{}' missing from report", ep.endpoint),
            });
        }
    }
}

fn compare_paths_array(
    ca: &CornerAnalysis,
    paths_arr: &[serde_json::Value],
    tolerance_ns: f64,
    paths_compared: &mut usize,
    max_abs_diff: &mut f64,
    mismatches: &mut Vec<Mismatch>,
    next_mm: &mut impl FnMut() -> String,
) {
    let mut seen_early = false;
    for (idx, p_val) in paths_arr.iter().enumerate() {
        let handle = p_val
            .get("handle")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("path{}", idx + 1));
        let kind_str = p_val
            .get("kind")
            .or_else(|| p_val.get("check"))
            .and_then(|v| v.as_str())
            .unwrap_or("setup");
        let is_late = matches!(
            kind_str,
            "setup" | "recovery" | "latch_setup" | "clock_gating_setup" | "output_setup" | "max"
        );
        // STA-REQ-0228: all late paths must precede any early paths in handle order.
        if is_late && seen_early {
            let h = next_mm();
            mismatches.push(Mismatch {
                handle: h,
                code: "C0530".to_string(),
                corner: ca.corner.clone(),
                object: handle.clone(),
                field: "handle_order".to_string(),
                checker: "setup_before_hold".to_string(),
                report: "late_after_early".to_string(),
                diff_ns: None,
                message: format!(
                    "path '{handle}' ({kind_str}) appears after an early/hold path (violates STA-REQ-0228)"
                ),
            });
        }
        if !is_late {
            seen_early = true;
        }

        let ep = p_val
            .get("endpoint")
            .or_else(|| p_val.get("to"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sp = p_val
            .get("startpoint")
            .or_else(|| p_val.get("from"))
            .and_then(|v| v.as_str());
        let rep_slack = p_val.get("slack").and_then(serde_json::Value::as_f64);

        if let Some(r_slk) = rep_slack
            && !ep.is_empty()
        {
            let our_slk = if let Some(s_name) = sp {
                let (ps, ph) = ca.path_slack(s_name, ep);
                if is_late { ps } else { ph }
            } else {
                let (es, eh) = ca.endpoint_slack(ep);
                if is_late { es } else { eh }
            };
            if let Some(m_slk) = our_slk {
                *paths_compared += 1;
                let d = (m_slk - r_slk).abs();
                *max_abs_diff = max_abs_diff.max(d);
                if d > tolerance_ns {
                    let h = next_mm();
                    mismatches.push(Mismatch {
                        handle: h,
                        code: "C0530".to_string(),
                        corner: ca.corner.clone(),
                        object: handle.clone(),
                        field: "slack".to_string(),
                        checker: format!("{m_slk:.6}"),
                        report: format!("{r_slk:.6}"),
                        diff_ns: Some(d),
                        message: format!(
                            "path '{handle}' to '{ep}' ({kind_str}): checker {m_slk:.6} ns vs report {r_slk:.6} ns"
                        ),
                    });
                }
            }
        }
    }
}

fn compare_scope_report(
    ours: &ScopeReport,
    corner_label: &str,
    scope_json: &serde_json::Value,
    mismatches: &mut Vec<Mismatch>,
    next_mm: &mut impl FnMut() -> String,
) {
    if let Some(rep_con) = scope_json
        .get("constrained_endpoints")
        .or_else(|| {
            scope_json
                .get("completeness")
                .and_then(|c| c.get("constrained_endpoints"))
        })
        .and_then(serde_json::Value::as_u64)
        && (rep_con as usize) != ours.constrained_endpoints
    {
        let h = next_mm();
        mismatches.push(Mismatch {
            handle: h,
            code: "C0540".to_string(),
            corner: corner_label.to_string(),
            object: "scope".to_string(),
            field: "constrained_endpoints".to_string(),
            checker: ours.constrained_endpoints.to_string(),
            report: rep_con.to_string(),
            diff_ns: None,
            message: format!(
                "constrained_endpoints: checker {} vs report {rep_con}",
                ours.constrained_endpoints
            ),
        });
    }

    if let Some(rep_unc) = scope_json
        .get("unconstrained_endpoints")
        .or_else(|| {
            scope_json
                .get("completeness")
                .and_then(|c| c.get("unconstrained_endpoints"))
        })
        .and_then(serde_json::Value::as_u64)
        && (rep_unc as usize) != ours.unconstrained_endpoints
    {
        let h = next_mm();
        mismatches.push(Mismatch {
            handle: h,
            code: "C0540".to_string(),
            corner: corner_label.to_string(),
            object: "scope".to_string(),
            field: "unconstrained_endpoints".to_string(),
            checker: ours.unconstrained_endpoints.to_string(),
            report: rep_unc.to_string(),
            diff_ns: None,
            message: format!(
                "unconstrained_endpoints: checker {} vs report {rep_unc}",
                ours.unconstrained_endpoints
            ),
        });
    }

    if let Some(reasons_obj) = scope_json
        .get("unconstrained_by_reason")
        .or_else(|| {
            scope_json
                .get("completeness")
                .and_then(|c| c.get("unconstrained_by_reason"))
        })
        .and_then(|v| v.as_object())
    {
        for (reason, val) in reasons_obj {
            if let Some(rep_cnt) = val.as_u64() {
                let our_cnt = ours
                    .unconstrained_by_reason
                    .get(reason)
                    .copied()
                    .unwrap_or(0);
                if (rep_cnt as usize) != our_cnt {
                    let h = next_mm();
                    mismatches.push(Mismatch {
                        handle: h,
                        code: "C0540".to_string(),
                        corner: corner_label.to_string(),
                        object: "scope".to_string(),
                        field: format!("unconstrained_by_reason.{reason}"),
                        checker: our_cnt.to_string(),
                        report: rep_cnt.to_string(),
                        diff_ns: None,
                        message: format!(
                            "unconstrained_by_reason.{reason}: checker {our_cnt} vs report {rep_cnt}"
                        ),
                    });
                }
            }
        }
    }

    if let Some(rep_sccs) = scope_json
        .get("sccs_broken")
        .and_then(serde_json::Value::as_u64)
        && (rep_sccs as usize) != ours.sccs_broken
    {
        let h = next_mm();
        mismatches.push(Mismatch {
            handle: h,
            code: "C0540".to_string(),
            corner: corner_label.to_string(),
            object: "scope".to_string(),
            field: "sccs_broken".to_string(),
            checker: ours.sccs_broken.to_string(),
            report: rep_sccs.to_string(),
            diff_ns: None,
            message: format!(
                "sccs_broken: checker {} vs report {rep_sccs}",
                ours.sccs_broken
            ),
        });
    }

    if let Some(rep_wm) = scope_json.get("wire_model").and_then(|v| v.as_str())
        && rep_wm != ours.wire_model
    {
        let h = next_mm();
        mismatches.push(Mismatch {
            handle: h,
            code: "C0540".to_string(),
            corner: corner_label.to_string(),
            object: "scope".to_string(),
            field: "wire_model".to_string(),
            checker: ours.wire_model.clone(),
            report: rep_wm.to_string(),
            diff_ns: None,
            message: format!(
                "wire_model: checker '{}' vs report '{rep_wm}'",
                ours.wire_model
            ),
        });
    }
}

/// Serializes an [`Analysis`][MultiCornerAnalysis] into canonical `timing.json` and `scope.json`
/// values for workspace testing and comparison.
#[must_use]
pub fn build_timing_and_scope_json(
    analysis: &MultiCornerAnalysis,
) -> (serde_json::Value, serde_json::Value) {
    let default_ca = analysis.default_corner();
    let endpoints_json: Vec<serde_json::Value> = default_ca
        .endpoints
        .iter()
        .map(|ep| {
            serde_json::json!({
                "endpoint": ep.endpoint,
                "kind": ep.kind.setup_role_str(),
                "path_group": ep.path_group,
                "status": ep.status.as_str(),
                "setup_slack": ep.setup_slack_ns,
                "hold_slack": ep.hold_slack_ns,
            })
        })
        .collect();

    let mut paths_json = Vec::new();
    let mut late_paths: Vec<&PathEval> = default_ca
        .paths
        .iter()
        .filter(|p| p.setup_slack_ns.is_some())
        .collect();
    late_paths.sort_by(|a, b| {
        a.setup_slack_ns
            .unwrap()
            .total_cmp(&b.setup_slack_ns.unwrap())
            .then_with(|| a.endpoint.cmp(&b.endpoint))
            .then_with(|| a.startpoint.cmp(&b.startpoint))
    });
    let mut early_paths: Vec<&PathEval> = default_ca
        .paths
        .iter()
        .filter(|p| p.hold_slack_ns.is_some())
        .collect();
    early_paths.sort_by(|a, b| {
        a.hold_slack_ns
            .unwrap()
            .total_cmp(&b.hold_slack_ns.unwrap())
            .then_with(|| a.endpoint.cmp(&b.endpoint))
            .then_with(|| a.startpoint.cmp(&b.startpoint))
    });

    let mut seen_late = BTreeSet::new();
    for p in late_paths {
        if !seen_late.insert((p.startpoint.clone(), p.endpoint.clone(), p.kind)) {
            continue;
        }
        let idx = paths_json.len() + 1;
        paths_json.push(serde_json::json!({
            "handle": format!("path{idx}"),
            "kind": p.kind.setup_role_str(),
            "startpoint": p.startpoint,
            "endpoint": p.endpoint,
            "arrival": p.arrival_late_ns,
            "required": p.setup_required_ns,
            "slack": p.setup_slack_ns,
        }));
        if seen_late.len() >= 10 {
            break;
        }
    }
    let mut seen_early = BTreeSet::new();
    for p in early_paths {
        if !seen_early.insert((p.startpoint.clone(), p.endpoint.clone(), p.kind)) {
            continue;
        }
        let idx = paths_json.len() + 1;
        paths_json.push(serde_json::json!({
            "handle": format!("path{idx}"),
            "kind": p.kind.hold_role_str(),
            "startpoint": p.startpoint,
            "endpoint": p.endpoint,
            "arrival": p.arrival_early_ns,
            "required": p.hold_required_ns,
            "slack": p.hold_slack_ns,
        }));
        if seen_early.len() >= 10 {
            break;
        }
    }

    let mut corners_json = serde_json::Map::new();
    for (cname, ca) in &analysis.corners {
        let c_eps: Vec<serde_json::Value> = ca
            .endpoints
            .iter()
            .map(|ep| {
                serde_json::json!({
                    "endpoint": ep.endpoint,
                    "kind": ep.kind.setup_role_str(),
                    "status": ep.status.as_str(),
                    "setup_slack": ep.setup_slack_ns,
                    "hold_slack": ep.hold_slack_ns,
                })
            })
            .collect();
        corners_json.insert(
            cname.clone(),
            serde_json::json!({
                "wns_setup": ca.metrics.wns_setup_ns,
                "tns_setup": ca.metrics.tns_setup_ns,
                "violating_setup": ca.metrics.violating_setup,
                "wns_hold": ca.metrics.wns_hold_ns,
                "tns_hold": ca.metrics.tns_hold_ns,
                "violating_hold": ca.metrics.violating_hold,
                "endpoints": c_eps,
            }),
        );
    }

    let timing_val = serde_json::json!({
        "top": analysis.top,
        "wns_setup": analysis.overall.wns_setup_ns,
        "tns_setup": analysis.overall.tns_setup_ns,
        "violating_setup": analysis.overall.violating_setup,
        "wns_hold": analysis.overall.wns_hold_ns,
        "tns_hold": analysis.overall.tns_hold_ns,
        "violating_hold": analysis.overall.violating_hold,
        "endpoints": endpoints_json,
        "paths": paths_json,
        "corners": corners_json,
    });

    let scope_val = serde_json::to_value(&default_ca.scope).unwrap_or_default();
    (timing_val, scope_val)
}

/// Helper to initialize a `rusteda-workspace` at `ws_path` and commit a revision containing
/// `netlist.rbin`, cached `.lib` / `.rlib` libraries, `constraints/merged.sdc`,
/// `reports/timing.json`, and `reports/scope.json`.
pub fn create_timed_workspace(
    ws_path: &Path,
    verilog_text: &str,
    corner_lib_texts: &[(&str, &str, &str)],
    sdc_text: &str,
    timing_json: &serde_json::Value,
    scope_json: &serde_json::Value,
) -> Result<Workspace, String> {
    let inv = Invocation::new("ruststa", "0.1.0", "ruststa time", ["ruststa", "time"]);
    let init = Workspace::init(ws_path, &inv).map_err(|e| e.to_string())?;
    let ws = init.workspace;
    let base = ws.resolve_base("rev0").map_err(|e| e.to_string())?;
    let mut txn = ws.begin(base, &inv).map_err(|e| e.to_string())?;

    let mut parsed_libs = Vec::with_capacity(corner_lib_texts.len());
    for &(corner, lib_name, lib_text) in corner_lib_texts {
        let lib =
            rusteda_formats::liberty::read_text(lib_text, lib_name).map_err(|e| e.to_string())?;
        let cached = rusteda_workspace::rlib::put(&ws, &lib, lib_text.as_bytes())
            .map_err(|e| e.to_string())?;
        txn.add_library(cached.entry(corner, format!("{lib_name}.lib")))
            .map_err(|e| e.to_string())?;
        parsed_libs.push(lib);
    }

    let gate_netlist =
        rusteda_formats::verilog::netlist::read_structural(verilog_text, "top.v", &|cell_name| {
            for lib in &parsed_libs {
                if let Some(cell) = lib.cells.iter().find(|c| c.name == cell_name) {
                    let pins = cell
                        .pins
                        .iter()
                        .filter_map(|p| {
                            let dir = match p.direction {
                                rusteda_core::liberty::Direction::Input => {
                                    rusteda_core::netlist::PinDir::Input
                                }
                                rusteda_core::liberty::Direction::Output => {
                                    rusteda_core::netlist::PinDir::Output
                                }
                                rusteda_core::liberty::Direction::Inout => {
                                    rusteda_core::netlist::PinDir::Inout
                                }
                                rusteda_core::liberty::Direction::Internal => return None,
                            };
                            Some((p.name.clone(), dir))
                        })
                        .collect();
                    return Some(pins);
                }
            }
            None
        })
        .map_err(|e| e.to_string())?;

    let frozen = gate_netlist.freeze();
    rusteda_workspace::netlist::add_to_txn(&mut txn, &frozen, Upstream::new())
        .map_err(|e| e.to_string())?;

    let constraints_dir = txn.staging_path("constraints").map_err(|e| e.to_string())?;
    let reports_dir = txn.staging_path("reports").map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&constraints_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&reports_dir).map_err(|e| e.to_string())?;
    std::fs::write(constraints_dir.join("merged.sdc"), sdc_text.as_bytes())
        .map_err(|e| e.to_string())?;
    std::fs::write(
        reports_dir.join("timing.json"),
        serde_json::to_vec_pretty(timing_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        reports_dir.join("scope.json"),
        serde_json::to_vec_pretty(scope_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    txn.add_dir("constraints", "constraints", Upstream::new())
        .map_err(|e| e.to_string())?;
    txn.add_dir("reports", "reports", Upstream::new())
        .map_err(|e| e.to_string())?;

    txn.commit(JsonMap::new()).map_err(|e| e.to_string())?;
    Ok(ws)
}
