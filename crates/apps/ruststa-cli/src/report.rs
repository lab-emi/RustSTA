// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa report` command (`STA-REQ-0550` to `STA-REQ-0560`).

use std::collections::VecDeque;

use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, JsonObject, Severity, Workspace, exit,
};
use ruststa::engine::limits::LimitKind;
use ruststa::engine::limits::table::LimitsTable;
use ruststa::engine::store::{
    QueriedPath, StaStore, StaStoreError, StaStoreView, UnconstrainedReasonTag, canon_f64,
};
use ruststa::engine::types::NodeId;

use crate::args::{PathKind, ReportArgs, ReportSection};
use crate::ws_io::{
    envelope_headers_from_store, map_store_error, node_handle_string, resolve_store_corner,
};

#[derive(Debug, Clone)]
enum FilterPred {
    SetupLt(f64),
    HoldLt(f64),
    SlackLt(f64),
    Violating,
    ClockEq(String),
    KindEq(String),
    NameContains(String),
}

fn parse_filter(raw: Option<&str>) -> Result<Option<FilterPred>, Error> {
    let Some(s) = raw else {
        return Ok(None);
    };
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::usage("--filter expression cannot be empty"));
    }
    if s == "violating" {
        return Ok(Some(FilterPred::Violating));
    }
    if let Some(val_str) = s.strip_prefix("setup<") {
        let v: f64 = val_str
            .parse()
            .map_err(|_| Error::usage(format!("invalid number in --filter '{s}'")))?;
        return Ok(Some(FilterPred::SetupLt(v)));
    }
    if let Some(val_str) = s.strip_prefix("hold<") {
        let v: f64 = val_str
            .parse()
            .map_err(|_| Error::usage(format!("invalid number in --filter '{s}'")))?;
        return Ok(Some(FilterPred::HoldLt(v)));
    }
    if let Some(val_str) = s.strip_prefix("slack<") {
        let v: f64 = val_str
            .parse()
            .map_err(|_| Error::usage(format!("invalid number in --filter '{s}'")))?;
        return Ok(Some(FilterPred::SlackLt(v)));
    }
    if let Some(clk) = s.strip_prefix("clock=") {
        if clk.is_empty() {
            return Err(Error::usage("empty clock name in --filter"));
        }
        return Ok(Some(FilterPred::ClockEq(clk.to_string())));
    }
    if let Some(k) = s.strip_prefix("kind=") {
        if !matches!(k, "setup" | "hold") {
            return Err(Error::usage(format!(
                "invalid kind '{k}' in --filter: expected setup or hold"
            )));
        }
        return Ok(Some(FilterPred::KindEq(k.to_string())));
    }
    for prefix in ["endpoint~", "pin~", "name~"] {
        if let Some(sub) = s.strip_prefix(prefix) {
            if sub.is_empty() {
                return Err(Error::usage(format!("empty substring in --filter '{s}'")));
            }
            return Ok(Some(FilterPred::NameContains(sub.to_string())));
        }
    }
    Err(Error::usage(format!(
        "unsupported --filter expression '{s}'"
    )))
}

/// The filter of a limit section: `violating`, `slack<X`, a name substring or `clock=NAME`
/// (`STA-REQ-0811`).
fn limit_filter(
    f: Option<&FilterPred>,
) -> Result<Option<crate::report_limits::LimitFilter>, Error> {
    use crate::report_limits::LimitFilter;
    Ok(match f {
        None => None,
        Some(FilterPred::Violating) => Some(LimitFilter::Violating),
        Some(FilterPred::SlackLt(t)) => Some(LimitFilter::SlackLt(*t)),
        Some(FilterPred::NameContains(s)) => Some(LimitFilter::NameContains(s.clone())),
        Some(FilterPred::ClockEq(c)) => Some(LimitFilter::ClockEq(c.clone())),
        Some(_) => {
            return Err(Error::usage(
                "limit sections accept --filter violating, slack<X, pin~TEXT or clock=NAME",
            ));
        }
    })
}

fn validate_sort_key(raw: Option<&str>) -> Result<Option<&str>, Error> {
    let Some(k) = raw else {
        return Ok(None);
    };
    match k {
        "slack" | "setup" | "hold" | "arrival" | "required" | "endpoint" | "pin" | "name"
        | "net" | "cap" | "slew" | "fanout" | "period" | "rank" | "code" | "value" => Ok(Some(k)),
        other => Err(Error::usage(format!("unknown --sort key '{other}'"))),
    }
}

pub(crate) fn section_envelope_obj(
    columns: &[&str],
    total: usize,
    matched: usize,
    rows: Vec<JsonObject>,
) -> Result<JsonObject, Error> {
    let shown = rows.len();
    let mut obj = JsonObject::new();
    obj.insert("columns", &columns)?;
    obj.insert("total", &(total as u64))?;
    obj.insert("matched", &(matched as u64))?;
    obj.insert("shown", &(shown as u64))?;
    obj.insert("rows", &rows)?;
    Ok(obj)
}

fn build_summary_section(
    store: &StaStore,
    corner_filter: Option<&str>,
    limit: usize,
) -> Result<JsonObject, Error> {
    let sum = store.query_summary().map_err(map_store_error)?;
    let total = sum.rows.len();
    let mut rows = Vec::new();
    for r in &sum.rows {
        if let Some(cf) = corner_filter
            && r.corner_name != cf
        {
            continue;
        }
        let mut obj = JsonObject::new();
        obj.insert("corner", &r.corner_name)?;
        obj.insert("path_group", &r.path_group)?;
        obj.insert("setup_wns_ns", &canon_f64(r.setup_wns))?;
        obj.insert("setup_tns_ns", &canon_f64(r.setup_tns))?;
        obj.insert("setup_violating", &r.setup_violating)?;
        obj.insert("hold_wns_ns", &canon_f64(r.hold_wns))?;
        obj.insert("hold_tns_ns", &canon_f64(r.hold_tns))?;
        obj.insert("hold_violating", &r.hold_violating)?;
        obj.insert("total_endpoints", &r.total_endpoints)?;
        obj.insert("unconstrained_endpoints", &r.unconstrained_endpoints)?;
        rows.push(obj);
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
    section_envelope_obj(
        &[
            "corner",
            "path_group",
            "setup_wns_ns",
            "setup_tns_ns",
            "setup_violating",
            "hold_wns_ns",
            "hold_tns_ns",
            "hold_violating",
            "total_endpoints",
            "unconstrained_endpoints",
        ],
        total,
        matched,
        rows,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_endpoints_section(
    store: &StaStore,
    v: &StaStoreView<'_>,
    corner_idx: usize,
    kind: PathKind,
    filter: Option<&FilterPred>,
    sort_key: Option<&str>,
    reverse: bool,
    limit: usize,
) -> Result<JsonObject, Error> {
    let mut eps = store
        .query_endpoints(corner_idx, None, usize::MAX)
        .map_err(map_store_error)?;
    let total = eps.len();
    // `--kind setup` or `--kind hold` keeps the endpoints with that slack and applies the slack
    // filters and the `slack` sort to it alone (`STA-REQ-0812`).
    let use_setup = kind != PathKind::Hold;
    let use_hold = kind != PathKind::Setup;
    eps.retain(|ep| match kind {
        PathKind::Setup => ep.setup_slack.is_finite(),
        PathKind::Hold => ep.hold_slack.is_finite(),
        PathKind::Both => true,
    });
    eps.retain(|ep| match filter {
        None => true,
        Some(FilterPred::SetupLt(th)) => ep.setup_slack.is_finite() && ep.setup_slack < *th,
        Some(FilterPred::HoldLt(th)) => ep.hold_slack.is_finite() && ep.hold_slack < *th,
        Some(FilterPred::SlackLt(th)) => {
            (use_setup && ep.setup_slack.is_finite() && ep.setup_slack < *th)
                || (use_hold && ep.hold_slack.is_finite() && ep.hold_slack < *th)
        }
        Some(FilterPred::Violating) => {
            (use_setup && ep.setup_slack.is_finite() && ep.setup_slack < 0.0)
                || (use_hold && ep.hold_slack.is_finite() && ep.hold_slack < 0.0)
        }
        Some(FilterPred::ClockEq(clk)) => ep.capture_clock == *clk || ep.path_group == *clk,
        Some(FilterPred::KindEq(k)) => {
            if k == "setup" {
                ep.setup_slack.is_finite()
            } else {
                ep.hold_slack.is_finite()
            }
        }
        Some(FilterPred::NameContains(sub)) => ep.name.contains(sub),
    });

    // `--kind hold` orders the endpoints by hold slack unless another key is given, so that
    // `--limit N` lists the N worst hold endpoints (`STA-REQ-0812`); otherwise the table order
    // (worst setup slack first) is kept.
    let sort_key = sort_key.or((kind == PathKind::Hold).then_some("hold"));
    if let Some(sk) = sort_key {
        let sk = if sk == "slack" && kind == PathKind::Hold {
            "hold"
        } else {
            sk
        };
        eps.sort_by(|a, b| match sk {
            "hold" => a
                .hold_slack
                .total_cmp(&b.hold_slack)
                .then_with(|| a.name.cmp(&b.name)),
            "endpoint" | "pin" | "name" => a.name.cmp(&b.name),
            _ => a
                .setup_slack
                .total_cmp(&b.setup_slack)
                .then_with(|| a.name.cmp(&b.name)),
        });
    }
    if reverse {
        eps.reverse();
    }

    let matched = eps.len();
    if limit > 0 && eps.len() > limit {
        eps.truncate(limit);
    }

    let num_port_bits = v.meta.num_port_bits;
    let mut rows = Vec::with_capacity(eps.len());
    for ep in &eps {
        let mut obj = JsonObject::new();
        obj.insert("endpoint", &ep.name)?;
        obj.insert("handle", &node_handle_string(ep.data_node, num_port_bits))?;
        obj.insert("kind", &ep.kind)?;
        obj.insert("path_group", &ep.path_group)?;
        obj.insert("capture_clock", &ep.capture_clock)?;
        obj.insert(
            "setup_slack_ns",
            &ep.setup_slack
                .is_finite()
                .then(|| canon_f64(ep.setup_slack)),
        )?;
        obj.insert(
            "hold_slack_ns",
            &ep.hold_slack.is_finite().then(|| canon_f64(ep.hold_slack)),
        )?;
        let ws_h = v.endpoints[ep.endpoint_idx].worst_setup_rank;
        let wh_h = v.endpoints[ep.endpoint_idx].worst_hold_rank;
        let ws_str =
            (ws_h != ruststa::engine::store::NO_INDEX).then(|| format!("path{}", ws_h + 1));
        let wh_str =
            (wh_h != ruststa::engine::store::NO_INDEX).then(|| format!("path{}", wh_h + 1));
        obj.insert("worst_setup_handle", &ws_str)?;
        obj.insert("worst_hold_handle", &wh_str)?;
        rows.push(obj);
    }

    section_envelope_obj(
        &[
            "endpoint",
            "handle",
            "kind",
            "path_group",
            "capture_clock",
            "setup_slack_ns",
            "hold_slack_ns",
            "worst_setup_handle",
            "worst_hold_handle",
        ],
        total,
        matched,
        rows,
    )
}

fn build_paths_section(
    store: &StaStore,
    v: &StaStoreView<'_>,
    corner_idx: usize,
    kind_arg: PathKind,
    filter: Option<&FilterPred>,
    reverse: bool,
    limit: usize,
) -> Result<JsonObject, Error> {
    let total = v.path_rank.len();
    let mut candidate_ranks: Vec<usize> = Vec::new();
    for (r_idx, pr) in v.path_rank.iter().enumerate() {
        if pr.corner_index() != corner_idx {
            continue;
        }
        match kind_arg {
            PathKind::Setup if pr.check_kind().is_hold() => continue,
            PathKind::Hold if !pr.check_kind().is_hold() => continue,
            _ => {}
        }
        candidate_ranks.push(r_idx);
    }

    // Only the paths shown are kept, `limit` of them (`0`: all): the table may rank millions of
    // paths (OpenC910: 8,749,528), each with all its points (AGENTS rule 3.4). Without a filter
    // every candidate matches, so only the shown paths are reconstructed.
    let window = if limit > 0 { limit } else { usize::MAX };
    let mut paths: VecDeque<QueriedPath> = VecDeque::new();
    let matched;
    if let Some(filter) = filter {
        let mut n = 0usize;
        for r_idx in candidate_ranks {
            let p = store.query_path_handle(r_idx).map_err(map_store_error)?;
            let keep = match filter {
                FilterPred::SetupLt(th) => p.check_kind == "setup" && p.slack < *th,
                FilterPred::HoldLt(th) => p.check_kind == "hold" && p.slack < *th,
                FilterPred::SlackLt(th) => p.slack < *th,
                FilterPred::Violating => p.slack < 0.0,
                FilterPred::ClockEq(clk) => {
                    p.capture_clock == *clk || p.launch_clock == *clk || p.path_group == *clk
                }
                FilterPred::KindEq(k) => p.check_kind == *k,
                FilterPred::NameContains(sub) => p.endpoint_name.contains(sub),
            };
            if !keep {
                continue;
            }
            n += 1;
            // Forward, the first matches are shown; reversed, the last ones.
            if reverse {
                paths.push_back(p);
                if paths.len() > window {
                    paths.pop_front();
                }
            } else if paths.len() < window {
                paths.push_back(p);
            }
        }
        matched = n;
    } else {
        matched = candidate_ranks.len();
        let shown: Box<dyn Iterator<Item = &usize>> = if reverse {
            Box::new(candidate_ranks.iter().rev().take(window))
        } else {
            Box::new(candidate_ranks.iter().take(window))
        };
        for &r_idx in shown {
            paths.push_back(store.query_path_handle(r_idx).map_err(map_store_error)?);
        }
    }
    if reverse && filter.is_some() {
        paths.make_contiguous().reverse();
    }

    let mut rows = Vec::with_capacity(paths.len());
    for p in &paths {
        let startpoint = p
            .points
            .first()
            .map_or_else(|| p.endpoint_name.clone(), |pt| pt.name.clone());
        let mut obj = JsonObject::new();
        obj.insert("handle", &format!("path{}", p.rank_idx + 1))?;
        obj.insert("kind", &p.check_kind)?;
        obj.insert("corner", &p.corner_name)?;
        obj.insert("path_group", &p.path_group)?;
        obj.insert("startpoint", &startpoint)?;
        obj.insert("endpoint", &p.endpoint_name)?;
        obj.insert("slack_ns", &canon_f64(p.slack))?;
        obj.insert("arrival_ns", &canon_f64(p.arrival))?;
        obj.insert("required_ns", &canon_f64(p.required))?;
        rows.push(obj);
    }

    section_envelope_obj(
        &[
            "handle",
            "kind",
            "corner",
            "path_group",
            "startpoint",
            "endpoint",
            "slack_ns",
            "arrival_ns",
            "required_ns",
        ],
        total,
        matched,
        rows,
    )
}

fn build_pins_section(
    v: &StaStoreView<'_>,
    corner_idx: usize,
    filter: Option<&FilterPred>,
    reverse: bool,
    limit: usize,
) -> Result<JsonObject, Error> {
    if v.node_rec.is_empty() {
        return Err(map_store_error(StaStoreError::SummaryModeUnavailable {
            operation: "report --section pins",
        }));
    }
    let n_nodes = v.meta.num_nodes as usize;
    let num_port_bits = v.meta.num_port_bits;
    let base = corner_idx * n_nodes;

    let finite_opt = |x: f64| x.is_finite().then(|| canon_f64(x));
    let max_opt = |a: f64, b: f64| match (finite_opt(a), finite_opt(b)) {
        (Some(x), Some(y)) => Some(canon_f64(x.max(y))),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    };
    let min_opt = |a: f64, b: f64| match (finite_opt(a), finite_opt(b)) {
        (Some(x), Some(y)) => Some(canon_f64(x.min(y))),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    };

    let mut rows = Vec::new();
    for idx in 0..n_nodes {
        let nr = &v.node_rec[base + idx];
        let node = NodeId(idx as u32);
        let name = crate::ws_io::resolve_node_name(v, node).unwrap_or_default();
        let setup_slack = min_opt(nr.slack[0][1], nr.slack[1][1]);
        let hold_slack = min_opt(nr.slack[0][0], nr.slack[1][0]);

        let keep = match filter {
            None => true,
            Some(FilterPred::SetupLt(th)) => setup_slack.is_some_and(|s| s < *th),
            Some(FilterPred::HoldLt(th)) => hold_slack.is_some_and(|h| h < *th),
            Some(FilterPred::SlackLt(th)) => {
                setup_slack.is_some_and(|s| s < *th) || hold_slack.is_some_and(|h| h < *th)
            }
            Some(FilterPred::Violating) => {
                setup_slack.is_some_and(|s| s < 0.0) || hold_slack.is_some_and(|h| h < 0.0)
            }
            Some(FilterPred::NameContains(sub)) => name.contains(sub),
            _ => true,
        };
        if !keep {
            continue;
        }

        let mut arr_late = max_opt(nr.arrival[0][1], nr.arrival[1][1]);
        let mut arr_early = min_opt(nr.arrival[0][0], nr.arrival[1][0]);
        let extra_begin = v.tag_extra_ptr[base + idx] as usize;
        let extra_end = v.tag_extra_ptr[base + idx + 1] as usize;
        for te in &v.tag_extra[extra_begin..extra_end] {
            if let Some(al) = max_opt(te.arrival[0][1], te.arrival[1][1]) {
                arr_late = Some(arr_late.map_or(al, |p| canon_f64(p.max(al))));
            }
            if let Some(ae) = min_opt(te.arrival[0][0], te.arrival[1][0]) {
                arr_early = Some(arr_early.map_or(ae, |p| canon_f64(p.min(ae))));
            }
        }

        let mut obj = JsonObject::new();
        obj.insert("pin", &name)?;
        obj.insert("handle", &node_handle_string(node, num_port_bits))?;
        obj.insert("is_port", &(node.0 < num_port_bits))?;
        obj.insert("arrival_late_ns", &arr_late)?;
        obj.insert("arrival_early_ns", &arr_early)?;
        obj.insert("slew_late_ns", &max_opt(nr.slew[0][1], nr.slew[1][1]))?;
        obj.insert("slew_early_ns", &min_opt(nr.slew[0][0], nr.slew[1][0]))?;
        let setup_req = crate::inspect::required_at_worst(nr.required, nr.slack, 1);
        obj.insert("required_ns", &setup_req)?;
        obj.insert("required_setup_ns", &setup_req)?;
        obj.insert(
            "required_hold_ns",
            &crate::inspect::required_at_worst(nr.required, nr.slack, 0),
        )?;
        obj.insert("slack_ns", &setup_slack)?;
        obj.insert("hold_slack_ns", &hold_slack)?;
        rows.push(obj);
    }

    if reverse {
        rows.reverse();
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }

    section_envelope_obj(
        &[
            "pin",
            "handle",
            "is_port",
            "arrival_late_ns",
            "arrival_early_ns",
            "slew_late_ns",
            "slew_early_ns",
            "required_ns",
            "required_setup_ns",
            "required_hold_ns",
            "slack_ns",
            "hold_slack_ns",
        ],
        n_nodes,
        matched,
        rows,
    )
}

fn build_clocks_section(v: &StaStoreView<'_>, limit: usize) -> Result<JsonObject, Error> {
    let total = v.clocks.len();
    let mut rows = Vec::with_capacity(total);
    for ck in v.clocks {
        let name = v.strings.get(ck.name_str).unwrap_or("");
        let mut obj = JsonObject::new();
        obj.insert("name", &name)?;
        obj.insert("period_ns", &canon_f64(ck.period_ns))?;
        obj.insert("rise_edge_ns", &canon_f64(ck.rise_edge_ns))?;
        obj.insert("fall_edge_ns", &canon_f64(ck.fall_edge_ns))?;
        obj.insert(
            "uncertainty_setup_ns",
            &canon_f64(f64::from(ck.uncertainty_setup_ns)),
        )?;
        obj.insert(
            "uncertainty_hold_ns",
            &canon_f64(f64::from(ck.uncertainty_hold_ns)),
        )?;
        obj.insert("is_virtual", &((ck.flags & 1) != 0))?;
        rows.push(obj);
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
    section_envelope_obj(
        &[
            "name",
            "period_ns",
            "rise_edge_ns",
            "fall_edge_ns",
            "uncertainty_setup_ns",
            "uncertainty_hold_ns",
            "is_virtual",
        ],
        total,
        matched,
        rows,
    )
}

fn build_unconstrained_section(v: &StaStoreView<'_>, limit: usize) -> Result<JsonObject, Error> {
    let total = v.unconstrained.len();
    let num_port_bits = v.meta.num_port_bits;
    let mut rows = Vec::with_capacity(total);
    for u in v.unconstrained {
        let node = NodeId(u.node);
        let name = crate::ws_io::resolve_node_name(v, node).unwrap_or_default();
        let reason_tag =
            UnconstrainedReasonTag::from_u32(u.reason).unwrap_or(UnconstrainedReasonTag::NoClock);
        let mut obj = JsonObject::new();
        obj.insert("pin", &name)?;
        obj.insert("handle", &node_handle_string(node, num_port_bits))?;
        obj.insert("reason", &reason_tag)?;
        rows.push(obj);
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
    section_envelope_obj(&["pin", "handle", "reason"], total, matched, rows)
}

fn build_notes_section(v: &StaStoreView<'_>, limit: usize) -> Result<JsonObject, Error> {
    let total = v.notes.len();
    let mut rows = Vec::with_capacity(total);
    for n in v.notes {
        let code = v.strings.get(n.code_str).unwrap_or("");
        let message = v.strings.get(n.message_str).unwrap_or("");
        let sev = if n.severity == 1 { "warning" } else { "note" };
        let mut obj = JsonObject::new();
        obj.insert("code", &code)?;
        obj.insert("severity", &sev)?;
        obj.insert("message", &message)?;
        obj.insert("count", &n.count)?;
        rows.push(obj);
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
    section_envelope_obj(
        &["code", "severity", "message", "count"],
        total,
        matched,
        rows,
    )
}

/// Executes `ruststa report`.
pub fn exec_report(
    ws: &Workspace,
    ws_arg: &str,
    args: &ReportArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    let filter = parse_filter(args.filter.as_deref())?;
    let sort_key = validate_sort_key(args.sort.as_deref())?;
    let limit = args.limit.unwrap_or(100);

    let rev = ws.resolve_query(&args.rev)?;
    let revision = ws.revision(rev)?;
    let store = StaStore::open_rev(&revision).map_err(map_store_error)?;
    let v = store.view().map_err(map_store_error)?;
    let (corner_idx, _) = resolve_store_corner(&store, args.corner.as_deref(), diags)?;

    let sections_to_run: Vec<ReportSection> = if args.sections.is_empty() {
        vec![ReportSection::Summary]
    } else {
        args.sections.clone()
    };

    let mut limits_table: Option<LimitsTable> = None;
    let mut sections_obj = JsonObject::new();
    for sec in &sections_to_run {
        match sec {
            ReportSection::Summary => {
                let s_obj = build_summary_section(&store, args.corner.as_deref(), limit)?;
                sections_obj.insert("summary", &s_obj)?;
            }
            ReportSection::Endpoints => {
                let s_obj = build_endpoints_section(
                    &store,
                    &v,
                    corner_idx,
                    args.kind,
                    filter.as_ref(),
                    sort_key,
                    args.reverse,
                    limit,
                )?;
                sections_obj.insert("endpoints", &s_obj)?;
            }
            ReportSection::Checks => {
                let s_obj = build_endpoints_section(
                    &store,
                    &v,
                    corner_idx,
                    args.kind,
                    filter.as_ref(),
                    sort_key,
                    args.reverse,
                    limit,
                )?;
                sections_obj.insert("checks", &s_obj)?;
            }
            ReportSection::Paths => {
                let s_obj = build_paths_section(
                    &store,
                    &v,
                    corner_idx,
                    args.kind,
                    filter.as_ref(),
                    args.reverse,
                    limit,
                )?;
                sections_obj.insert("paths", &s_obj)?;
            }
            ReportSection::Pins => {
                let s_obj =
                    build_pins_section(&v, corner_idx, filter.as_ref(), args.reverse, limit)?;
                sections_obj.insert("pins", &s_obj)?;
            }
            ReportSection::Clocks => {
                let s_obj = build_clocks_section(&v, limit)?;
                sections_obj.insert("clocks", &s_obj)?;
            }
            ReportSection::Unconstrained => {
                let s_obj = build_unconstrained_section(&v, limit)?;
                sections_obj.insert("unconstrained", &s_obj)?;
            }
            ReportSection::Notes => {
                let s_obj = build_notes_section(&v, limit)?;
                sections_obj.insert("notes", &s_obj)?;
            }
            ReportSection::HoldFix => {
                let s_obj = crate::holdfix::hold_fix_section(
                    &store,
                    &revision,
                    args.corner.as_ref().map(|_| corner_idx),
                    args.target_margin,
                    limit,
                )?;
                sections_obj.insert("hold_fix", &s_obj)?;
            }
            ReportSection::Limits
            | ReportSection::PulseWidth
            | ReportSection::MinPeriod
            | ReportSection::Drv
            | ReportSection::MaxTransition
            | ReportSection::MaxCapacitance
            | ReportSection::MaxFanout => {
                let table = match &limits_table {
                    Some(t) => t,
                    None => {
                        let t = LimitsTable::open_rev(&revision)?.ok_or_else(|| {
                            let msg = format!(
                                "revision {rev} has no limits table: analyze it again with \
                                 ruststa run (STA-REQ-0806)"
                            );
                            diags.push(Diagnostic::new(
                                Severity::Error,
                                DiagCode::of("T0136"),
                                msg.clone(),
                            ));
                            Error::input_rejected(msg)
                        })?;
                        limits_table.insert(t)
                    }
                };
                let lcorner = match args.corner.as_deref() {
                    Some(name) => Some(
                        table
                            .corners()?
                            .iter()
                            .position(|c| *c == name)
                            .ok_or_else(|| {
                                Error::input_rejected(format!(
                                    "corner '{name}' not found in the limits table"
                                ))
                            })?,
                    ),
                    None => None,
                };
                let (name, kinds): (&str, &[LimitKind]) = match sec {
                    ReportSection::PulseWidth => (
                        "pulse_width",
                        &[LimitKind::MinPulseWidthHigh, LimitKind::MinPulseWidthLow],
                    ),
                    ReportSection::MinPeriod => ("min_period", &[LimitKind::MinPeriod]),
                    ReportSection::Drv => (
                        "drv",
                        &[
                            LimitKind::MaxTransition,
                            LimitKind::MaxCapacitance,
                            LimitKind::MaxFanout,
                        ],
                    ),
                    ReportSection::MaxTransition => ("max_transition", &[LimitKind::MaxTransition]),
                    ReportSection::MaxCapacitance => {
                        ("max_capacitance", &[LimitKind::MaxCapacitance])
                    }
                    ReportSection::MaxFanout => ("max_fanout", &[LimitKind::MaxFanout]),
                    _ => ("limits", &[]),
                };
                let s_obj = if name == "limits" {
                    crate::report_limits::limits_section(table, lcorner)?
                } else {
                    let lfilter = limit_filter(filter.as_ref())?;
                    crate::report_limits::rows_section(
                        table,
                        kinds,
                        lcorner,
                        v.meta.num_port_bits,
                        lfilter.as_ref(),
                        sort_key,
                        args.reverse,
                        limit,
                    )?
                };
                sections_obj.insert(name, &s_obj)?;
            }
        }
    }

    let (scope, comp, metrics) = envelope_headers_from_store(&store, args.corner.as_deref())?;
    let mut env = Envelope::new("ruststa report");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(rev);
    env.set_scope(scope)?;
    env.set_completeness(comp)?;
    env.set_metrics(metrics.clone())?;
    env.set("sections", &sections_obj)?;

    let rev_str = rev.to_string();
    if v.meta.num_ranked_paths > 0 {
        env.push_next_action(&[
            "ruststa", "path", "-w", ws_arg, "--rev", &rev_str, "--target", "path1",
        ]);
    }

    let mut lines = Vec::new();
    lines.push(format!("ruststa report  (rev {rev})"));
    lines.push(format!("  metrics: {}", metrics.to_raw().as_str()));
    lines.push(format!("  sections: {}", sections_obj.to_raw().as_str()));
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok((env, lines, exit::OK))
}
