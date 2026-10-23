// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent checker on a revision of the RustEDA workspace (`STA-REQ-0500` to `0504`).
//!
//! Each verification package adds its own module here so that concurrent packages do not touch the
//! same files:
//! - [`lib_model`], [`verilog`], [`canon_sdc`], [`constants`], [`eval`], [`checks_eval`] (`R1`):
//!   core checker on the workspace;
//! - [`exceptions`] (`R2`): `set_false_path`, `set_multicycle_path`, `set_max_delay`,
//!   `set_min_delay` and `set_clock_groups`;
//! - [`through`] (`R3`): `-through`, `-rise_through` and `-fall_through` on exceptions;
//! - [`clocks`] (`R4`): generated clocks, latency and uncertainty matrices, sense propagation;
//! - [`cppr`] (`R5`): common-path pessimism removal and AOCV/POCV derating.

pub mod canon_sdc;
pub mod checks_eval;
pub mod clocks;
pub mod compare;
pub mod constants;
pub mod cppr;
pub mod eval;
pub mod exceptions;
pub mod lib_model;
pub mod paths;
pub mod through;
pub mod verilog;

use std::collections::BTreeMap;
use std::path::Path;

use rusteda_core::liberty::Library;
use rusteda_workspace::{Revision, Workspace};
use serde::{Deserialize, Serialize};

pub use checks_eval::{CheckKindEval, EndpointEval, EndpointStatus, PathEval};
pub use compare::{build_timing_and_scope_json, check_against_reports, create_timed_workspace};
pub use eval::{ArrivalEntry, ArrivalOrigin, ClockWave, Tag, TimingSolver};
pub use lib_model::{Analysis, CornerLib, Trans};

/// Default numerical tolerance for CPU `f64` reports (`1e-6 ns`, `STA-REQ-0500`).
pub const DEFAULT_TOLERANCE_NS: f64 = 1e-6;

/// Returned by a stub module when a revision uses a feature whose checker package has not been
/// merged yet; the top-level checker reports [`Verdict::Undecided`] for that construct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub feature: &'static str,
    pub planned: &'static str,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: not supported yet (planned: {})",
            self.feature, self.planned
        )
    }
}

impl std::error::Error for Unsupported {}

/// Structured diagnostic emitted by the independent checker (`C0500`–`C0999`, `D0102`,
/// `D0110`–`D0114`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagRecord {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub line: Option<usize>,
}

/// A single discrepancy between the independent checker and a workspace report, addressed by a
/// deterministic `mm<N>` handle (`STA-REQ-0500`, `0504`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mismatch {
    pub handle: String,
    pub code: String,
    pub corner: String,
    pub object: String,
    pub field: String,
    pub checker: String,
    pub report: String,
    pub diff_ns: Option<f64>,
    pub message: String,
}

/// Three-valued checker verdict (`STA-REQ-0500`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Agree,
    Disagree { mismatches: Vec<Mismatch> },
    Undecided { reason: String },
}

impl Verdict {
    #[must_use]
    pub const fn is_agree(&self) -> bool {
        matches!(self, Self::Agree)
    }

    #[must_use]
    pub const fn is_disagree(&self) -> bool {
        matches!(self, Self::Disagree { .. })
    }

    #[must_use]
    pub const fn is_undecided(&self) -> bool {
        matches!(self, Self::Undecided { .. })
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Agree => "agree",
            Self::Disagree { .. } => "disagree",
            Self::Undecided { .. } => "undecided",
        }
    }
}

/// Summary metrics for one path group or corner.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlackMetrics {
    pub wns_setup_ns: f64,
    pub tns_setup_ns: f64,
    pub violating_setup: usize,
    pub wns_hold_ns: f64,
    pub tns_hold_ns: f64,
    pub violating_hold: usize,
}

/// Completeness and scope summary for one corner (`STA-REQ-0125`, `0226`, `0270`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeReport {
    pub total_endpoints: usize,
    pub constrained_endpoints: usize,
    pub unconstrained_endpoints: usize,
    pub unconstrained_by_reason: BTreeMap<String, usize>,
    pub sccs_broken: usize,
    pub wire_model: String,
    pub wire_load_model: Option<String>,
    pub exceptions: BTreeMap<String, usize>,
    pub unused_exceptions: usize,
}

/// Independent analysis report for a single corner.
#[derive(Debug, Clone)]
pub struct CornerAnalysis {
    pub corner: String,
    pub metrics: SlackMetrics,
    pub path_groups: BTreeMap<String, SlackMetrics>,
    pub endpoints: Vec<EndpointEval>,
    pub paths: Vec<PathEval>,
    pub scope: ScopeReport,
    pub diagnostics: Vec<DiagRecord>,
    pub node_map: BTreeMap<String, usize>,
    pub net_map: BTreeMap<String, usize>,
    pub net_cap_pf: Vec<[[f64; 2]; 2]>,
    pub slews: Vec<[[Option<f64>; 2]; 2]>,
    pub arrivals: Vec<[[Vec<ArrivalEntry>; 2]; 2]>,
    pub node_req_ns: Vec<[[Option<f64>; 2]; 2]>,
    pub node_slack_ns: Vec<[[Option<f64>; 2]; 2]>,
}

impl CornerAnalysis {
    /// Returns `(setup_slack_ns, hold_slack_ns)` for endpoint `name` (taking the worst across check
    /// kinds if multiple check kinds exist on the same pin).
    #[must_use]
    pub fn endpoint_slack(&self, name: &str) -> (Option<f64>, Option<f64>) {
        let mut s: Option<f64> = None;
        let mut h: Option<f64> = None;
        for ep in self.endpoints.iter().filter(|e| e.endpoint == name) {
            if let Some(sv) = ep.setup_slack_ns {
                s = Some(s.map_or(sv, |m| m.min(sv)));
            }
            if let Some(hv) = ep.hold_slack_ns {
                h = Some(h.map_or(hv, |m| m.min(hv)));
            }
        }
        (s, h)
    }

    /// Returns `(setup_slack_ns, hold_slack_ns)` for endpoint `name` and check `kind`.
    #[must_use]
    pub fn endpoint_slack_kind(
        &self,
        name: &str,
        kind: CheckKindEval,
    ) -> (Option<f64>, Option<f64>) {
        self.endpoints
            .iter()
            .find(|e| e.endpoint == name && e.kind == kind)
            .map_or((None, None), |e| (e.setup_slack_ns, e.hold_slack_ns))
    }

    /// Returns the status of endpoint `name`.
    #[must_use]
    pub fn endpoint_status(&self, name: &str) -> Option<EndpointStatus> {
        self.endpoints
            .iter()
            .find(|e| e.endpoint == name)
            .map(|e| e.status)
    }

    /// Returns the worst `(setup_slack_ns, hold_slack_ns)` across all enumerated paths from
    /// `startpoint` to `endpoint`.
    #[must_use]
    pub fn path_slack(&self, startpoint: &str, endpoint: &str) -> (Option<f64>, Option<f64>) {
        let mut s: Option<f64> = None;
        let mut h: Option<f64> = None;
        for p in &self.paths {
            if p.startpoint == startpoint && p.endpoint == endpoint {
                if let Some(sv) = p.setup_slack_ns {
                    s = Some(s.map_or(sv, |m| m.min(sv)));
                }
                if let Some(hv) = p.hold_slack_ns {
                    h = Some(h.map_or(hv, |m| m.min(hv)));
                }
            }
        }
        (s, h)
    }

    /// Returns the worst arrival at `(node_name, tr, an)` across all tags.
    #[must_use]
    pub fn node_arrival(&self, node_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        let &ni = self.node_map.get(node_name)?;
        let entries = &self.arrivals[ni][tr.idx()][an.idx()];
        if entries.is_empty() {
            return None;
        }
        Some(entries.iter().map(|e| e.arrival_ns).fold(
            if an == Analysis::Late {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            },
            |a, b| {
                if an == Analysis::Late {
                    a.max(b)
                } else {
                    a.min(b)
                }
            },
        ))
    }

    /// Returns the per-pin slew at `(node_name, tr, an)`.
    #[must_use]
    pub fn node_slew(&self, node_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        let &ni = self.node_map.get(node_name)?;
        self.slews[ni][tr.idx()][an.idx()]
    }

    /// Returns the backward required time at `(node_name, tr, an)` (`STA-REQ-0227`).
    #[must_use]
    pub fn node_required(&self, node_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        let &ni = self.node_map.get(node_name)?;
        self.node_req_ns[ni][tr.idx()][an.idx()]
    }

    /// Returns the per-pin slack at `(node_name, tr, an)` (`STA-REQ-0227`).
    #[must_use]
    pub fn node_slack(&self, node_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        let &ni = self.node_map.get(node_name)?;
        self.node_slack_ns[ni][tr.idx()][an.idx()]
    }

    /// Returns the capacitive load of `net_name` in `pF` for `(tr, an)`.
    #[must_use]
    pub fn net_cap_pf(&self, net_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        let &ni = self.net_map.get(net_name)?;
        Some(self.net_cap_pf[ni][tr.idx()][an.idx()])
    }
}

/// Multi-corner independent analysis result.
#[derive(Debug, Clone)]
pub struct MultiCornerAnalysis {
    pub top: String,
    pub corners: BTreeMap<String, CornerAnalysis>,
    pub overall: SlackMetrics,
    pub diagnostics: Vec<DiagRecord>,
}

impl MultiCornerAnalysis {
    /// Returns the first (or default) corner analysis.
    #[must_use]
    pub fn default_corner(&self) -> &CornerAnalysis {
        self.corners
            .values()
            .next()
            .expect("MultiCornerAnalysis has at least one corner")
    }
}

/// Full outcome of checking a workspace revision or in-memory report (`STA-REQ-0500`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRevisionOutcome {
    pub verdict: Verdict,
    pub tolerance_ns: f64,
    pub endpoints_compared: usize,
    pub paths_compared: usize,
    pub max_abs_diff_ns: f64,
    pub diagnostics: Vec<DiagRecord>,
}

fn compute_metrics_from_endpoints(endpoints: &[EndpointEval]) -> SlackMetrics {
    let mut m = SlackMetrics::default();
    for ep in endpoints {
        if ep.status != EndpointStatus::Constrained {
            continue;
        }
        if let Some(s) = ep.setup_slack_ns
            && s < 0.0
        {
            m.wns_setup_ns = m.wns_setup_ns.min(s);
            m.tns_setup_ns += s;
            m.violating_setup += 1;
        }
        if let Some(h) = ep.hold_slack_ns
            && h < 0.0
        {
            m.wns_hold_ns = m.wns_hold_ns.min(h);
            m.tns_hold_ns += h;
            m.violating_hold += 1;
        }
    }
    m
}

fn build_corner_analysis(
    corner: &str,
    solver: TimingSolver<'_>,
    exhaustive_diags: &mut Vec<DiagRecord>,
) -> CornerAnalysis {
    // STA-REQ-0502: verify that for every constrained endpoint, the minimum setup and hold slack
    // across all enumerated paths equals the propagated endpoint slack within `1e-9 ns`.
    if solver.design.nodes.len() <= 5000 && solver.sccs_broken == 0 {
        for ep in &solver.endpoints {
            if ep.status != EndpointStatus::Constrained {
                continue;
            }
            let ep_paths: Vec<&PathEval> = solver
                .paths
                .iter()
                .filter(|p| p.endpoint == ep.endpoint && p.kind == ep.kind)
                .collect();
            let enum_setup = ep_paths
                .iter()
                .filter_map(|p| p.setup_slack_ns)
                .reduce(f64::min);
            let enum_hold = ep_paths
                .iter()
                .filter_map(|p| p.hold_slack_ns)
                .reduce(f64::min);
            if let (Some(a), Some(b)) = (ep.setup_slack_ns, enum_setup)
                && (a - b).abs() > 1e-9
            {
                exhaustive_diags.push(DiagRecord {
                    code: "C0502".to_string(),
                    severity: "error".to_string(),
                    message: format!(
                        "exhaustive path setup slack {b:.6} differs from propagated slack {a:.6} at '{}'",
                        ep.endpoint
                    ),
                    line: None,
                });
            }
            if let (Some(a), Some(b)) = (ep.hold_slack_ns, enum_hold)
                && (a - b).abs() > 1e-9
            {
                exhaustive_diags.push(DiagRecord {
                    code: "C0502".to_string(),
                    severity: "error".to_string(),
                    message: format!(
                        "exhaustive path hold slack {b:.6} differs from propagated slack {a:.6} at '{}'",
                        ep.endpoint
                    ),
                    line: None,
                });
            }
        }
    }

    let metrics = compute_metrics_from_endpoints(&solver.endpoints);
    let mut by_group: BTreeMap<String, Vec<EndpointEval>> = BTreeMap::new();
    for ep in &solver.endpoints {
        if ep.status == EndpointStatus::Constrained {
            by_group
                .entry(ep.path_group.clone())
                .or_default()
                .push(ep.clone());
        }
    }
    let path_groups = by_group
        .into_iter()
        .map(|(g, eps)| (g, compute_metrics_from_endpoints(&eps)))
        .collect();

    let mut unconstrained_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut constrained_count = 0usize;
    let mut unconstrained_count = 0usize;
    for ep in &solver.endpoints {
        if ep.status == EndpointStatus::Constrained {
            constrained_count += 1;
        } else {
            unconstrained_count += 1;
            *unconstrained_by_reason
                .entry(ep.status.as_str().to_string())
                .or_insert(0) += 1;
        }
    }

    let wire_model = if solver.wire_load_model.is_some() {
        "wireload_lumped".to_string()
    } else {
        "ideal".to_string()
    };

    let scope = ScopeReport {
        total_endpoints: solver.endpoints.len(),
        constrained_endpoints: constrained_count,
        unconstrained_endpoints: unconstrained_count,
        unconstrained_by_reason,
        sccs_broken: solver.sccs_broken,
        wire_model,
        wire_load_model: solver.wire_load_model.clone(),
        exceptions: solver.exception_counts.clone(),
        unused_exceptions: solver.unused_exceptions,
    };

    CornerAnalysis {
        corner: corner.to_string(),
        metrics,
        path_groups,
        endpoints: solver.endpoints,
        paths: solver.paths,
        scope,
        diagnostics: solver.diagnostics,
        node_map: solver.design.node_map.clone(),
        net_map: solver.design.net_map.clone(),
        net_cap_pf: solver.net_cap_pf,
        slews: solver.slews,
        arrivals: solver.arrivals,
        node_req_ns: solver.node_req_ns,
        node_slack_ns: solver.node_slack_ns,
    }
}

/// Runs the independent checker directly on in-memory Verilog, per-corner Liberty libraries, and
/// canonical SDC text (`STA-REQ-0500`, `0501`).
pub fn analyze_inputs(
    verilog_texts: &[(&str, &str)],
    top: Option<&str>,
    corner_libs: &[(&str, Vec<Library>)],
    sdc_text: &str,
) -> Result<MultiCornerAnalysis, Result<Unsupported, String>> {
    if corner_libs.is_empty() {
        return Err(Err("C0501: no corner libraries provided".to_string()));
    }

    let mut compiled_corners = Vec::with_capacity(corner_libs.len());
    for (cname, libs) in corner_libs {
        let clib = CornerLib::build(*cname, libs).map_err(Err)?;
        compiled_corners.push(clib);
    }

    let design = verilog::read_verilog_texts(verilog_texts, top, &compiled_corners).map_err(Err)?;

    let mut corners_map = BTreeMap::new();
    let mut all_diags = Vec::new();

    for clib in &compiled_corners {
        let sdc = canon_sdc::parse_canon_sdc(sdc_text, &design, Some(clib)).map_err(Err)?;
        if sdc.has_generated_clocks_or_sense {
            return Err(Ok(clocks::ClockNetwork::new().unwrap_err()));
        }
        if sdc.has_propagated_clock {
            return Err(Ok(cppr::ClockPathCredit::new().unwrap_err()));
        }
        let consts = constants::propagate_constants(&design, clib, &sdc);
        let solver = TimingSolver::solve(&design, clib, &sdc, &consts).map_err(Ok)?;
        let mut exhaustive_diags = Vec::new();
        let ca = build_corner_analysis(&clib.corner, solver, &mut exhaustive_diags);
        all_diags.extend(ca.diagnostics.iter().cloned());
        all_diags.extend(exhaustive_diags);
        corners_map.insert(clib.corner.clone(), ca);
    }

    // Compute overall metrics across all corners.
    let mut overall = SlackMetrics::default();
    let mut worst_ep_setup: BTreeMap<(String, CheckKindEval), f64> = BTreeMap::new();
    let mut worst_ep_hold: BTreeMap<(String, CheckKindEval), f64> = BTreeMap::new();
    for ca in corners_map.values() {
        for ep in &ca.endpoints {
            if ep.status != EndpointStatus::Constrained {
                continue;
            }
            if let Some(s) = ep.setup_slack_ns {
                worst_ep_setup
                    .entry((ep.endpoint.clone(), ep.kind))
                    .and_modify(|v| *v = v.min(s))
                    .or_insert(s);
            }
            if let Some(h) = ep.hold_slack_ns {
                worst_ep_hold
                    .entry((ep.endpoint.clone(), ep.kind))
                    .and_modify(|v| *v = v.min(h))
                    .or_insert(h);
            }
        }
    }
    for s in worst_ep_setup.values() {
        if *s < 0.0 {
            overall.wns_setup_ns = overall.wns_setup_ns.min(*s);
            overall.tns_setup_ns += *s;
            overall.violating_setup += 1;
        }
    }
    for h in worst_ep_hold.values() {
        if *h < 0.0 {
            overall.wns_hold_ns = overall.wns_hold_ns.min(*h);
            overall.tns_hold_ns += *h;
            overall.violating_hold += 1;
        }
    }

    Ok(MultiCornerAnalysis {
        top: design.top,
        corners: corners_map,
        overall,
        diagnostics: all_diags,
    })
}

/// Reads and analyzes a committed revision of a `rusteda-workspace` directory (`STA-REQ-0500`).
///
/// Extracts `netlist.rbin` -> in-memory Verilog text -> parses back via `rusteda_formats::verilog`,
/// reads `.lib` files via `rusteda_formats::liberty`, and parses `constraints/merged.sdc` with the
/// checker's independent canonical-SDC reader (`canon_sdc`).
pub fn analyze_revision(
    ws_path: &Path,
    rev_spec: Option<&str>,
) -> Result<(Workspace, Revision, MultiCornerAnalysis), Result<Unsupported, String>> {
    let ws = Workspace::open(ws_path).map_err(|e| Err(format!("C0501: {e}")))?;
    let rev_id = ws
        .resolve_query(rev_spec.unwrap_or("HEAD"))
        .map_err(|e| Err(format!("C0501: {e}")))?;
    let revision = ws
        .revision(rev_id)
        .map_err(|e| Err(format!("C0501: {e}")))?;

    // 1. Read `netlist.rbin` and round-trip through structural Verilog text.
    let gate_netlist =
        rusteda_workspace::netlist::thaw_rev(&revision).map_err(|e| Err(format!("C0501: {e}")))?;
    let v_text = verilog::gate_netlist_to_verilog(&gate_netlist);

    // 2. Read `.lib` files per corner.
    let mut corner_libs: Vec<(String, Vec<Library>)> = Vec::new();
    for entry in &revision.meta().libraries {
        let cache_lib_path =
            ws.root()
                .join("cache")
                .join(format!("{}-{}.lib", entry.name, entry.hash.hex()));
        let lib = if cache_lib_path.is_file() {
            let text = std::fs::read_to_string(&cache_lib_path)
                .map_err(|e| Err(format!("C0501: {}: {e}", cache_lib_path.display())))?;
            rusteda_formats::liberty::read_text(&text, &entry.name)
                .map_err(|e| Err(format!("C0501: {e}")))?
        } else {
            let rbin = revision
                .open_library(
                    entry,
                    rusteda_workspace::rlib::KIND,
                    rusteda_workspace::rlib::VERSIONS,
                )
                .map_err(|e| Err(format!("C0501: {e}")))?;
            rusteda_workspace::rlib::decode_rbin(&rbin).map_err(|e| Err(format!("C0501: {e}")))?
        };
        if let Some((_, vec)) = corner_libs.iter_mut().find(|(c, _)| *c == entry.corner) {
            vec.push(lib);
        } else {
            corner_libs.push((entry.corner.clone(), vec![lib]));
        }
    }

    // Also support `libs` directory table if `meta.libraries` is empty.
    if corner_libs.is_empty() && revision.has_table("libs") {
        let files = revision
            .table_files("libs")
            .map_err(|e| Err(format!("C0501: {e}")))?;
        for (rel, _) in files {
            if !rel.ends_with(".lib") {
                continue;
            }
            let bytes = revision
                .read_table_file("libs", &rel)
                .map_err(|e| Err(format!("C0501: {e}")))?;
            let text = String::from_utf8(bytes)
                .map_err(|e| Err(format!("C0501: invalid UTF-8 in {rel}: {e}")))?;
            let corner = rel
                .split('/')
                .next()
                .filter(|_| rel.contains('/'))
                .unwrap_or("default")
                .to_string();
            let lib = rusteda_formats::liberty::read_text(&text, &rel)
                .map_err(|e| Err(format!("C0501: {e}")))?;
            if let Some((_, vec)) = corner_libs.iter_mut().find(|(c, _)| *c == corner) {
                vec.push(lib);
            } else {
                corner_libs.push((corner, vec![lib]));
            }
        }
    }

    // 3. Read `constraints/merged.sdc` (canonical SDC).
    let sdc_bytes = if revision.has_table("constraints") {
        if revision
            .table_files("constraints")
            .is_ok_and(|f| f.iter().any(|(p, _)| p == "merged.sdc"))
        {
            revision
                .read_table_file("constraints", "merged.sdc")
                .map_err(|e| Err(format!("C0501: {e}")))?
        } else {
            revision
                .read_table("constraints")
                .map_err(|e| Err(format!("C0501: {e}")))?
        }
    } else if revision.has_table("sdc") {
        revision
            .read_table("sdc")
            .map_err(|e| Err(format!("C0501: {e}")))?
    } else {
        let p = revision.dir().join("constraints/merged.sdc");
        std::fs::read(&p).map_err(|e| Err(format!("C0501: {}: {e}", p.display())))?
    };
    let sdc_text = String::from_utf8(sdc_bytes)
        .map_err(|e| Err(format!("C0501: invalid UTF-8 in merged.sdc: {e}")))?;

    let corner_refs: Vec<(&str, Vec<Library>)> = corner_libs
        .iter()
        .map(|(c, v)| (c.as_str(), v.clone()))
        .collect();
    let analysis = analyze_inputs(
        &[("netlist.v", &v_text)],
        Some(gate_netlist.name()),
        &corner_refs,
        &sdc_text,
    )?;

    Ok((ws, revision, analysis))
}

/// Runs the independent checker on a workspace revision and compares with `reports/timing.json`
/// and `reports/scope.json` (`STA-REQ-0500`, `0502`, `0504`).
pub fn check_revision(
    ws_path: &Path,
    rev_spec: Option<&str>,
    tolerance_ns: Option<f64>,
) -> CheckRevisionOutcome {
    let tol = tolerance_ns.unwrap_or(DEFAULT_TOLERANCE_NS);
    let (_ws, revision, analysis) = match analyze_revision(ws_path, rev_spec) {
        Ok(res) => res,
        Err(Ok(unsup)) => {
            return CheckRevisionOutcome {
                verdict: Verdict::Undecided {
                    reason: unsup.to_string(),
                },
                tolerance_ns: tol,
                endpoints_compared: 0,
                paths_compared: 0,
                max_abs_diff_ns: 0.0,
                diagnostics: Vec::new(),
            };
        }
        Err(Err(msg)) => {
            return CheckRevisionOutcome {
                verdict: Verdict::Disagree {
                    mismatches: vec![Mismatch {
                        handle: "mm1".to_string(),
                        code: "C0501".to_string(),
                        corner: "default".to_string(),
                        object: "revision".to_string(),
                        field: "input".to_string(),
                        checker: "valid".to_string(),
                        report: "invalid".to_string(),
                        diff_ns: None,
                        message: msg.clone(),
                    }],
                },
                tolerance_ns: tol,
                endpoints_compared: 0,
                paths_compared: 0,
                max_abs_diff_ns: 0.0,
                diagnostics: vec![DiagRecord {
                    code: "C0501".to_string(),
                    severity: "error".to_string(),
                    message: msg,
                    line: None,
                }],
            };
        }
    };

    let timing_bytes = if revision.has_table("reports") {
        revision.read_table_file("reports", "timing.json").ok()
    } else if revision.has_table("timing") {
        revision.read_table("timing").ok()
    } else {
        std::fs::read(revision.dir().join("reports/timing.json")).ok()
    };

    let Some(t_bytes) = timing_bytes else {
        return CheckRevisionOutcome {
            verdict: Verdict::Disagree {
                mismatches: vec![Mismatch {
                    handle: "mm1".to_string(),
                    code: "C0501".to_string(),
                    corner: "default".to_string(),
                    object: "reports/timing.json".to_string(),
                    field: "table".to_string(),
                    checker: "present".to_string(),
                    report: "missing".to_string(),
                    diff_ns: None,
                    message: "revision is missing reports/timing.json".to_string(),
                }],
            },
            tolerance_ns: tol,
            endpoints_compared: 0,
            paths_compared: 0,
            max_abs_diff_ns: 0.0,
            diagnostics: analysis.diagnostics,
        };
    };

    let timing_json: serde_json::Value = match serde_json::from_slice(&t_bytes) {
        Ok(v) => v,
        Err(e) => {
            return CheckRevisionOutcome {
                verdict: Verdict::Disagree {
                    mismatches: vec![Mismatch {
                        handle: "mm1".to_string(),
                        code: "C0501".to_string(),
                        corner: "default".to_string(),
                        object: "reports/timing.json".to_string(),
                        field: "json".to_string(),
                        checker: "valid".to_string(),
                        report: "corrupt".to_string(),
                        diff_ns: None,
                        message: format!("invalid JSON in reports/timing.json: {e}"),
                    }],
                },
                tolerance_ns: tol,
                endpoints_compared: 0,
                paths_compared: 0,
                max_abs_diff_ns: 0.0,
                diagnostics: analysis.diagnostics,
            };
        }
    };

    let scope_bytes = if revision.has_table("reports") {
        revision.read_table_file("reports", "scope.json").ok()
    } else if revision.has_table("scope") {
        revision.read_table("scope").ok()
    } else {
        std::fs::read(revision.dir().join("reports/scope.json")).ok()
    };
    let scope_json: Option<serde_json::Value> =
        scope_bytes.and_then(|b| serde_json::from_slice(&b).ok());

    check_against_reports(&analysis, &timing_json, scope_json.as_ref(), tol)
}
