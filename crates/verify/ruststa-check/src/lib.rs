// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent checker for RustSTA reports (STA-REQ-0500).
//!
//! The checker of revisions of the RustEDA workspace is built in [`rev`]. The other modules are
//! the legacy checker behind `ruststa check sta`; they keep their paths and their behaviour until
//! the new checker replaces them.

#![forbid(unsafe_code)]

pub mod cells;
pub mod checks;
pub mod design;
pub mod old_sdc;
pub mod rev;
pub mod sdc;
pub mod timing;

use std::path::PathBuf;

use rusteda_core::liberty::Library;
use rusteda_formats::verilog::{PpOptions, SourceMap, read_sources, read_texts};
use serde::Serialize;

/// Netlist sources.
#[derive(Debug, Clone)]
pub enum Netlist {
    Files(Vec<PathBuf>),
    Texts(Vec<(String, String)>),
}

/// Checker options.
#[derive(Debug, Clone)]
pub struct CheckStaOptions {
    pub netlist: Netlist,
    pub top: Option<String>,
    pub sdc: String,
    /// Largest accepted slack difference (ns).
    pub tolerance: f64,
}

/// A slack that differs.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Difference {
    pub pin: String,
    pub kind: String,
    pub checker: Option<f64>,
    pub report: Option<f64>,
}

/// Result of a check (STA-REQ-0512).
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CheckStaReport {
    /// `ruststa-check-sta-report`; empty in reports written before the field existed.
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub format_version: u32,
    pub verdict: String,
    pub endpoints_compared: usize,
    pub max_abs_difference: f64,
    pub differences: Vec<Difference>,
    pub errors: Vec<String>,
    /// Largest accepted slack difference (ns).
    #[serde(default)]
    pub tolerance: f64,
}

/// Value of `CheckStaReport::format`.
pub const REPORT_FORMAT: &str = "ruststa-check-sta-report";

/// Recomputes the worst setup and hold slack of every endpoint.
pub fn analyze(opts: &CheckStaOptions, libs: &[Library]) -> Result<checks::Slacks, String> {
    let (cells, tu, cu) = cells::build(libs);
    let mut sm = SourceMap::new();
    let (units, diags) = match &opts.netlist {
        Netlist::Files(f) => read_sources(&mut sm, f, PpOptions::default()),
        Netlist::Texts(t) => {
            let r: Vec<(&str, &str)> = t.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            read_texts(&mut sm, &r, PpOptions::default())
        }
    };
    if let Some(d) = diags.iter().find(|d| d.is_error()) {
        return Err(d.render(&sm));
    }
    let is_cell = |n: &str| cells.contains_key(n);
    let mut d = design::read(&units, opts.top.as_deref(), &is_cell)?;
    let mut holder = None;
    let mut t = timing::Timing::new(&mut d, &cells, &opts.sdc, tu, cu, &mut holder)?;
    checks::compute(&mut t)
}

/// Compares recomputed slacks with a RustSTA JSON report (`endpoints` list).
pub fn check(
    opts: &CheckStaOptions,
    libs: &[Library],
    report: &serde_json::Value,
) -> CheckStaReport {
    let mut out = CheckStaReport {
        format: REPORT_FORMAT.into(),
        format_version: 1,
        tolerance: opts.tolerance,
        verdict: "error".into(),
        endpoints_compared: 0,
        max_abs_difference: 0.0,
        differences: Vec::new(),
        errors: Vec::new(),
    };
    let ours = match analyze(opts, libs) {
        Ok(s) => s,
        Err(e) => {
            out.errors.push(e);
            return out;
        }
    };
    let Some(list) = report.get("endpoints").and_then(|v| v.as_array()) else {
        out.errors.push("report has no endpoints list".into());
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for e in list {
        let pin = e
            .get("pin")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        seen.insert(pin.clone());
        for (kind, key, map) in [
            ("setup", "setup_slack", &ours.setup),
            ("hold", "hold_slack", &ours.hold),
        ] {
            let theirs = e.get(key).and_then(serde_json::Value::as_f64);
            let mine = map.get(&pin).copied().filter(|x| x.is_finite());
            match (mine, theirs) {
                (Some(a), Some(b)) => {
                    out.endpoints_compared += 1;
                    let d = (a - b).abs();
                    out.max_abs_difference = out.max_abs_difference.max(d);
                    if d > opts.tolerance {
                        out.differences.push(Difference {
                            pin: pin.clone(),
                            kind: kind.into(),
                            checker: Some(a),
                            report: Some(b),
                        });
                    }
                }
                (None, None) => {}
                (a, b) => out.differences.push(Difference {
                    pin: pin.clone(),
                    kind: kind.into(),
                    checker: a,
                    report: b,
                }),
            }
        }
    }
    for (kind, map) in [("setup", &ours.setup), ("hold", &ours.hold)] {
        let mut missing: Vec<&String> = map
            .iter()
            .filter(|(p, v)| v.is_finite() && !seen.contains(*p))
            .map(|(p, _)| p)
            .collect();
        missing.sort();
        for p in missing {
            out.differences.push(Difference {
                pin: p.clone(),
                kind: kind.into(),
                checker: map.get(p).copied(),
                report: None,
            });
        }
    }
    out.verdict = if out.differences.is_empty() {
        "agree".into()
    } else {
        "disagree".into()
    };
    out
}
