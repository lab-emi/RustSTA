// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Scope, completeness and metrics headers for the workspace timing envelope.

use rusteda_workspace::{Error, JsonObject};
use ruststa::engine::limits::LimitKind;
use ruststa::engine::limits::status::{DRV_KINDS, limit_statuses};
use ruststa::engine::store::{CornerLimitsReport, StaStore, StorePrecision, canon_f64};

use super::map_store_error;

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
    if let Some(factors) = scope_rep.global_timing_derate {
        scope.insert("global_timing_derate", &factors)?;
    }
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
    // The clock limit scope of the design rules (`STA-REQ-0808`).
    let drv_rules = match scope_rep.limits.as_deref().and_then(<[_]>::first) {
        Some(c) => {
            let mut r = JsonObject::new();
            r.insert("clock_limits", &c.clock_limits)?;
            Some(r)
        }
        None => None,
    };
    scope.insert("drv_rules", &drv_rules)?;

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
    // Pins of the design rules left out or checked by a special rule (`STA-REQ-0809`,
    // `STA-REQ-0822`), and the clock network pins whose limit the scope decides (`STA-REQ-0808`).
    completeness.insert("drv_constant_drivers", &sum(|c| c.constant_drivers))?;
    completeness.insert(
        "drv_static_transition_pins",
        &sum(|c| c.static_transition_pins),
    )?;
    completeness.insert("drv_ideal_clock_pins", &sum(|c| c.ideal_clock_pins))?;
    completeness.insert("drv_untimed_pins", &sum(|c| c.untimed_pins))?;
    completeness.insert("drv_clock_scope_pins", &sum(|c| c.clock_scope_pins))?;
    // Unconnected instance output pins, not checked (`STA-REQ-0819`).
    completeness.insert(
        "drv_unconnected_output_pins",
        &sum(|c| c.unconnected_output_pins),
    )?;
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
