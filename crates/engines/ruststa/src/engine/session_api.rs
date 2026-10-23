// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Slack queries on timing sessions for the optimizer of the synthesis tool, Phase 3 (owner:
//! X2; `STA-REQ-0620` to `STA-REQ-0628`).
//!
//! A query evaluates the edits of an edit file (`rusteda.edits.v1`): its base edits on a timing
//! session, then each candidate on its own copy of the base session. For the base and for every
//! candidate it reports the timing summary of every corner and the slacks of the requested
//! endpoints and ranked paths. Every reported value equals, bit for bit, the value that a full
//! analysis ([`EngineAnalysis::analyze`](super::analysis::EngineAnalysis::analyze)) of the
//! netlist with the edits applied gives (`STA-REQ-0625`), because the session is updated by the
//! code of the full analysis with an incremental propagation. `ruststa whatif` runs queries
//! from the command line, read-only and without a daemon.

use std::time::Instant;

use rusteda_workspace::edits::{Edit, EditFile};

use super::analysis::{CornerAnalysis, STATE_BYTES_PER_NODE_TAG};
use super::incr::{SessionError, SessionNodeKey, TimingSession};
use super::scope::StaSummary;
use super::store::layout::canon_f64;

/// An endpoint whose slacks a query reports (`STA-REQ-0623`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointTarget {
    /// The entry of the request: a name or an `ep<N>` handle.
    pub label: String,
    /// The pin `inst/pin` or port of the endpoint.
    pub name: String,
    /// Its node key in the base session.
    pub key: SessionNodeKey,
}

/// A ranked path of the revision whose slack a query reports (`STA-REQ-0623`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTarget {
    /// The handle `path<N>`.
    pub label: String,
    /// The endpoint of the path.
    pub endpoint: EndpointTarget,
    /// The corner of the path.
    pub corner: usize,
    /// The check of the path: `setup`, `hold`, `recovery` or `removal`.
    pub check: String,
}

impl PathTarget {
    /// True for the hold family (hold and removal), whose slack is the worst hold slack.
    #[must_use]
    pub fn is_hold_family(&self) -> bool {
        matches!(self.check.as_str(), "hold" | "removal")
    }
}

/// What a query reports besides the summaries (`STA-REQ-0623`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryTargets {
    /// Endpoints, in request order.
    pub endpoints: Vec<EndpointTarget>,
    /// Ranked paths, in request order.
    pub paths: Vec<PathTarget>,
}

/// The setup or hold summary of one corner (`STA-REQ-0624`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckSummary {
    /// The worst slack in ns when negative, `0.0` otherwise; `None` without a check.
    pub wns_ns: Option<f64>,
    /// The total negative slack in ns; `None` without a check.
    pub tns_ns: Option<f64>,
    /// Endpoints with a negative slack.
    pub violating_endpoints: u32,
    /// The worst slack in ns; `None` without a check.
    pub worst_slack_ns: Option<f64>,
}

impl CheckSummary {
    fn new(worst: Option<f64>, tns: f64, violating: u32) -> Self {
        let worst = worst.map(canon_f64);
        Self {
            wns_ns: worst.map(|w| if w < 0.0 { w } else { 0.0 }),
            tns_ns: worst.map(|_| canon_f64(tns)),
            violating_endpoints: violating,
            worst_slack_ns: worst,
        }
    }
}

/// The summary of one corner (`STA-REQ-0624`).
#[derive(Debug, Clone, PartialEq)]
pub struct CornerReport {
    /// The corner name.
    pub corner: String,
    /// Setup-family checks.
    pub setup: CheckSummary,
    /// Hold-family checks.
    pub hold: CheckSummary,
}

/// The slacks of a requested endpoint in one corner (`STA-REQ-0624`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndpointSlack {
    /// The worst setup-family slack in ns; `None` without such a check.
    pub setup_ns: Option<f64>,
    /// The worst hold-family slack in ns; `None` without such a check.
    pub hold_ns: Option<f64>,
}

/// The values a query reports of one analysis: the base or one candidate (`STA-REQ-0624`).
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    /// The summary of every corner, in corner order.
    pub corners: Vec<CornerReport>,
    /// The summary over the corners.
    pub summary: StaSummary,
    /// The slacks of each requested endpoint per corner; `None` when the endpoint's pin is gone.
    pub endpoints: Vec<Option<Vec<EndpointSlack>>>,
    /// The slack of each requested ranked path; `None` when its endpoint has no such check.
    pub paths: Vec<Option<f64>>,
    /// Node evaluations of the forward propagation that produced the values.
    pub forward_nodes_evaluated: usize,
}

/// The outcome of one candidate (`STA-REQ-0624`, `STA-REQ-0626`).
#[derive(Debug)]
pub enum CandidateOutcome {
    /// The candidate's edits applied and its timing was evaluated.
    Evaluated(Evaluation),
    /// An edit of the candidate was rejected; the error names the edit and its code.
    Rejected(SessionError),
    /// The time budget ran out before the candidate was evaluated (`T0108`).
    NotEvaluated,
}

/// The summary and requested slacks of the session's current analysis (`STA-REQ-0624`,
/// `STA-REQ-0625`).
#[must_use]
pub fn evaluate(session: &TimingSession, targets: &QueryTargets) -> Evaluation {
    let corners = session
        .corners()
        .iter()
        .map(|c| {
            let s = &c.endpoints.summary;
            CornerReport {
                corner: c.corner_name.clone(),
                setup: CheckSummary::new(s.wns_setup_ns, s.tns_setup_ns, s.violating_setup),
                hold: CheckSummary::new(s.wns_hold_ns, s.tns_hold_ns, s.violating_hold),
            }
        })
        .collect();
    let slacks = |c: &CornerAnalysis, key: SessionNodeKey| -> EndpointSlack {
        let rec = session
            .node_of_key(key)
            .and_then(|n| c.endpoints.endpoints.get(&n));
        EndpointSlack {
            setup_ns: rec
                .and_then(|r| r.setup.as_ref())
                .map(|s| canon_f64(s.slack)),
            hold_ns: rec
                .and_then(|r| r.hold.as_ref())
                .map(|s| canon_f64(s.slack)),
        }
    };
    let endpoints = targets
        .endpoints
        .iter()
        .map(|t| {
            session
                .node_of_key(t.key)
                .map(|_| session.corners().iter().map(|c| slacks(c, t.key)).collect())
        })
        .collect();
    let paths = targets
        .paths
        .iter()
        .map(|p| {
            let c = session.corners().get(p.corner)?;
            let s = slacks(c, p.endpoint.key);
            if p.is_hold_family() {
                s.hold_ns
            } else {
                s.setup_ns
            }
        })
        .collect();
    Evaluation {
        corners,
        summary: *session.sta_summary(),
        endpoints,
        paths,
        forward_nodes_evaluated: session.last_stats().forward_nodes_evaluated,
    }
}

/// Bytes of the state of one copy of `session`, as the analysis estimates it (`STA-REQ-0627`):
/// nodes times tags times [`STATE_BYTES_PER_NODE_TAG`], summed over the corners.
#[must_use]
pub fn session_state_bytes(session: &TimingSession) -> u64 {
    let n = session.graph().num_nodes() as u64;
    session
        .corners()
        .iter()
        .map(|c| n * u64::from(c.tags.ntags_for_compute()) * STATE_BYTES_PER_NODE_TAG)
        .sum()
}

/// Evaluates each candidate of `file` on its own copy of `base`, whose base edits are applied
/// already (`STA-REQ-0623`): the copy applies the candidate's edits, is evaluated with
/// [`evaluate`], and is dropped. Candidates after `deadline` are not evaluated.
pub fn evaluate_candidates(
    base: &TimingSession,
    file: &EditFile,
    targets: &QueryTargets,
    deadline: Option<Instant>,
) -> Vec<CandidateOutcome> {
    file.candidates
        .iter()
        .map(|c| {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return CandidateOutcome::NotEvaluated;
            }
            evaluate_edits(base, &c.edits, targets)
        })
        .collect()
}

/// Applies `edits` to a copy of `base` and evaluates it.
pub fn evaluate_edits(
    base: &TimingSession,
    edits: &[Edit],
    targets: &QueryTargets,
) -> CandidateOutcome {
    let mut s = base.clone();
    match s.apply_named_edits(edits) {
        Ok(_) => CandidateOutcome::Evaluated(evaluate(&s, targets)),
        Err(e) => CandidateOutcome::Rejected(e),
    }
}
