// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! What front ends use of RustSTA (PROTO-REQ-0002): the run entry point, options, the analysis
//! and its queries, report types, and the names of phases and metrics (STA-REQ-0310 to
//! STA-REQ-0315).

use serde::{Deserialize, Serialize};

pub use crate::report::{
    ClockReport, Counts, EndpointReport, GroupSummary, PathPoint, PathReport, Report, Summary,
};
pub use crate::{
    Analysis, Backend, REPORT_FORMAT, ReportOptions, Source, StaError, StaOptions, run_with,
};

/// Check kind: setup (late) or hold (early).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Setup,
    Hold,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Setup, Kind::Hold];

    /// The spelling of `paths[].kind` in the report.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Setup => "setup",
            Kind::Hold => "hold",
        }
    }
}

/// Endpoints in ascending node order (the order of the report) with their worst slacks
/// (STA-REQ-0313). Row `r` is endpoint `node[r]`; a slack is NaN when the endpoint has no check
/// of that kind; `capture` is the clock of the worst setup check, else of the worst hold check,
/// `u16::MAX` when there is none.
#[derive(Debug, Clone, Default)]
pub struct EndpointTable {
    pub node: Vec<u32>,
    pub setup: Vec<f64>,
    pub hold: Vec<f64>,
    pub capture: Vec<u16>,
}

impl EndpointTable {
    pub fn len(&self) -> usize {
        self.node.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node.is_empty()
    }

    /// Bytes held on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.node.capacity() * 4
            + self.setup.capacity() * 8
            + self.hold.capacity() * 8
            + self.capture.capacity() * 2
    }

    /// Slacks of one kind.
    pub fn slacks(&self, kind: Kind) -> &[f64] {
        match kind {
            Kind::Setup => &self.setup,
            Kind::Hold => &self.hold,
        }
    }
}

/// Node ids ordered by (name, id), for lookups by name and prefix (STA-REQ-0315).
#[derive(Debug, Clone, Default)]
pub struct NameIndex {
    pub(crate) order: Vec<u32>,
}

impl NameIndex {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The lowest node id named `name`.
    pub fn get(&self, a: &Analysis, name: &str) -> Option<u32> {
        let i = self.order.partition_point(|&v| a.node_name(v) < name);
        self.order
            .get(i)
            .copied()
            .filter(|&v| a.node_name(v) == name)
    }

    /// Nodes whose name starts with `prefix`, in name order.
    pub fn with_prefix<'a>(
        &'a self,
        a: &'a Analysis,
        prefix: &'a str,
    ) -> impl Iterator<Item = u32> + 'a {
        let i = self.order.partition_point(|&v| a.node_name(v) < prefix);
        self.order[i..]
            .iter()
            .copied()
            .take_while(move |&v| a.node_name(v).starts_with(prefix))
    }
}

/// Phases of a timing run, in order (`report` is built by `Analysis::report`).
pub const PHASES: [&str; 6] = ["read", "graph", "setup", "propagate", "checks", "report"];

/// Metrics: name, unit and meaning.
pub const METRICS: &[(&str, &str, &str)] = &[
    ("graph_nodes", "", "nodes of the timing graph"),
    ("graph_arcs", "", "arcs of the timing graph"),
    ("graph_levels", "", "levels of the timing graph"),
    ("clocks", "", "clocks defined by the constraints"),
];
