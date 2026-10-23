// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! JSON report (STA-REQ-0302).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockReport {
    pub name: String,
    pub period: f64,
    pub waveform: [f64; 2],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupSummary {
    pub clock: String,
    pub wns: f64,
    pub tns: f64,
    pub violating: usize,
    pub endpoints: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub wns: f64,
    pub tns: f64,
    pub violating: usize,
    pub endpoints: usize,
    pub per_clock: Vec<GroupSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointReport {
    pub pin: String,
    pub setup_slack: Option<f64>,
    pub hold_slack: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathPoint {
    pub pin: String,
    pub cell: Option<String>,
    pub transition: String,
    pub incr: f64,
    pub arrival: f64,
    pub transition_time: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathReport {
    pub kind: String,
    pub startpoint: String,
    pub endpoint: String,
    pub launch_clock: String,
    pub capture_clock: String,
    pub arrival: f64,
    pub required: f64,
    pub slack: f64,
    pub points: Vec<PathPoint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub nodes: usize,
    pub arcs: usize,
    pub cells: usize,
    pub levels: usize,
    pub startpoints: usize,
    pub disabled_arcs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// `ruststa-sta-report`; empty in reports written before the field existed (STA-REQ-0306).
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub format_version: u32,
    pub top: String,
    pub backend: String,
    pub clocks: Vec<ClockReport>,
    pub setup: Summary,
    pub hold: Summary,
    pub unconstrained_endpoints: usize,
    pub endpoints: Vec<EndpointReport>,
    pub paths: Vec<PathReport>,
    pub counts: Counts,
    pub notes: Vec<String>,
    pub runtime_s: f64,
    /// Seconds spent per phase, in execution order.
    #[serde(default)]
    pub phase_s: Vec<(String, f64)>,
}
