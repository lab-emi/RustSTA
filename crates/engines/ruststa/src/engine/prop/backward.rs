// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Required times and slack per pin (owner: E6; `STA-REQ-0227`).
//!
//! Engine section D. After the endpoint checks have been evaluated, required times are seeded at
//! every constrained endpoint per `(tag, tr, an)` (`req_late = arr_late + setup_slack`,
//! `req_early = arr_early - hold_slack`) and propagated backwards in descending level order
//! ([`Levels`](crate::engine::graph::Levels)):
//! - across an enabled arc `u -> v` with forward delay `d`,
//!   `req[u][tag][tr_in][Late] = min(req[u][tag][tr_in][Late], req[v][tag][tr_out][Late] - d)` and
//!   `req[u][tag][tr_in][Early] = max(req[u][tag][tr_in][Early], req[v][tag][tr_out][Early] - d)`;
//! - across a latch `D -> Q` tag-mapping arc ([`TagMapRec`](rusteda_compute::timing::TagMapRec)),
//!   required times at `Q` in the latch's opening-edge tag `dst_tag` map back to each source tag
//!   `src_tag` at `D` whose arrival borrowed into the transparency window by
//!   `req[Q][dst_tag][tr_Q][Late] - delay(D -> Q) - shift` (`shift = -D_open`), combined (`min`)
//!   with the direct setup check required time at `D`.
//!
//! Each node stores the required time, arrival and slack (`req_late - arr_late` for setup,
//! `arr_early - req_early` for hold) of its worst-slack tag per transition, so
//! `slack = required - arrival` (setup) and `slack = arrival - required` (hold) hold on every pin.

use std::collections::{BTreeMap, HashMap};

use rayon::prelude::*;
use rusteda_compute::timing::absent;
use rusteda_core::job::{Ctx, Diag};

use crate::engine::delay::load::{NetLoadOverrides, NetLoads};
use crate::engine::delay::lut::NO_TABLE;
use crate::engine::delay::wire::Ideal;
use crate::engine::graph::{ArcKind, CheckKind, TimingGraph};
use crate::engine::input::NetlistView;
use crate::engine::input::constr::CompiledConstraints;
use crate::engine::input::libview::TimingLib;
pub use crate::engine::paths::relaunch::{
    lower_schedule_with_relaunch, schedule_and_outputs_from_compiled,
};
pub use crate::engine::paths::search::{
    hold_relation_ns, propagate_clock_sense, setup_relation_ns,
};
use crate::engine::paths::search::{pulse_width_ns, source_edges};
use crate::engine::prop::lower::{
    BackendKind, IdealSchedule, LoweredCorner, LoweredResult, Precision,
};
use crate::engine::types::{An, ClockId, NodeId, TagId, Tr};

mod required;

pub use required::{ByNode, PositionValues};

/// Canonicalizes `-0.0` to `+0.0`.
#[inline]
#[must_use]
pub fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}

/// Absent required-time sentinel for bound `an` (`-inf` for `An::Early`, `+inf` for `An::Late`).
#[inline]
#[must_use]
pub const fn absent_req(an: An) -> f64 {
    match an {
        An::Early => f64::NEG_INFINITY,
        An::Late => f64::INFINITY,
    }
}

/// Role of an endpoint timing check (`STA-REQ-0211`, `STA-REQ-0212`, `STA-REQ-0221`,
/// `STA-REQ-0224`, `STA-REQ-0225`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointCheckRole {
    Setup,
    Hold,
    Recovery,
    Removal,
    LatchSetup,
    LatchHold,
    OutputMax,
    OutputMin,
    ClockGatingSetup,
    ClockGatingHold,
}

impl EndpointCheckRole {
    /// Analysis bound of this check role (`An::Late` for setup-like, `An::Early` for hold-like).
    #[inline]
    #[must_use]
    pub const fn bound(self) -> An {
        match self {
            Self::Setup
            | Self::Recovery
            | Self::LatchSetup
            | Self::OutputMax
            | Self::ClockGatingSetup => An::Late,
            Self::Hold
            | Self::Removal
            | Self::LatchHold
            | Self::OutputMin
            | Self::ClockGatingHold => An::Early,
        }
    }

    /// True for late-path checks (`Setup`, `Recovery`, `LatchSetup`, `OutputMax`,
    /// `ClockGatingSetup`).
    #[inline]
    #[must_use]
    pub const fn is_setup_like(self) -> bool {
        matches!(self.bound(), An::Late)
    }

    /// True for early-path checks (`Hold`, `Removal`, `LatchHold`, `OutputMin`,
    /// `ClockGatingHold`).
    #[inline]
    #[must_use]
    pub const fn is_hold_like(self) -> bool {
        matches!(self.bound(), An::Early)
    }

    /// Canonical string identifier for reports.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Hold => "hold",
            Self::Recovery => "recovery",
            Self::Removal => "removal",
            Self::LatchSetup => "latch_setup",
            Self::LatchHold => "latch_hold",
            Self::OutputMax => "output_max",
            Self::OutputMin => "output_min",
            Self::ClockGatingSetup => "clock_gating_setup",
            Self::ClockGatingHold => "clock_gating_hold",
        }
    }

    /// Default path group name for this check role (`"async_default"` for recovery/removal,
    /// `"clock_gating_default"` for clock-gating checks, or the capture clock name).
    #[must_use]
    pub fn default_path_group(self, capture_clock_name: Option<&str>) -> String {
        match self {
            Self::Recovery | Self::Removal => String::from("async_default"),
            Self::ClockGatingSetup | Self::ClockGatingHold => String::from("clock_gating_default"),
            _ => capture_clock_name.unwrap_or("**default**").to_owned(),
        }
    }
}

/// Output delay specification on an output port node (`STA-REQ-0212`, `STA-REQ-0218`,
/// `STA-REQ-0219`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputDelaySpec {
    pub node: NodeId,
    pub clock: u32,
    pub edge: Tr,
    pub delay_ns: [[Option<f64>; 2]; 2],
    pub latency_included: bool,
}

impl OutputDelaySpec {
    /// Creates a symmetric output delay across rise/fall and min/max on `(node, clock, edge)`.
    #[must_use]
    pub const fn symmetric(node: NodeId, clock: u32, edge: Tr, value_ns: f64) -> Self {
        Self {
            node,
            clock,
            edge,
            delay_ns: [[Some(value_ns); 2]; 2],
            latency_included: false,
        }
    }
}

/// One evaluated endpoint check requirement at `(node, tag, tr, an)` (`STA-REQ-0214`,
/// `STA-REQ-0227`).
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointRequirement {
    pub node: NodeId,
    pub tag: TagId,
    pub tr: Tr,
    pub an: An,
    pub required_ns: f64,
    pub arrival_ns: f64,
    pub slack_ns: f64,
    pub check_ns: f64,
    pub capture_clock: Option<ClockId>,
    pub capture_edge: Option<Tr>,
    pub clock_pin: Option<NodeId>,
    pub role: EndpointCheckRole,
    pub path_group: String,
}

/// Transparency window record for a level-sensitive latch `D -> Q` arc and launch tag
/// (`STA-REQ-0221`, `STA-REQ-0222`, `STA-REQ-0227`).
#[derive(Debug, Clone, PartialEq)]
pub struct LatchWindowRecord {
    pub arc_idx: u32,
    pub inst: u32,
    pub data_pin: NodeId,
    pub enable_pin: NodeId,
    pub out_pin: NodeId,
    pub src_tag: TagId,
    pub open_tag: TagId,
    pub d_open_ns: f64,
    pub d_close_ns: f64,
    pub r_open_ns: f64,
    pub r_close_ns: [f64; 2],
    pub borrowed_ns: [f64; 2],
}

/// Summary of an endpoint (`(node, path_group, check_family)`) across all tags and transitions
/// (`STA-REQ-0214`, `STA-REQ-0224`).
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointSummary {
    pub node: NodeId,
    pub pin_name: String,
    pub clock_pin: Option<NodeId>,
    pub capture_clock: Option<ClockId>,
    pub path_group: String,
    pub setup_role: Option<EndpointCheckRole>,
    pub hold_role: Option<EndpointCheckRole>,
    pub setup_slack: Option<f64>,
    pub hold_slack: Option<f64>,
    pub worst_setup: Option<(TagId, Tr, f64, f64, f64)>,
    pub worst_hold: Option<(TagId, Tr, f64, f64, f64)>,
}

/// WNS, TNS, and endpoint counts for a path group or overall (`STA-REQ-0214`).
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSlackSummary {
    pub path_group: String,
    pub setup_wns: f64,
    pub setup_tns: f64,
    pub setup_violating: usize,
    pub setup_endpoints: usize,
    pub hold_wns: f64,
    pub hold_tns: f64,
    pub hold_violating: usize,
    pub hold_endpoints: usize,
}

impl GroupSlackSummary {
    /// Creates an empty summary for `path_group`.
    #[must_use]
    pub fn new(path_group: impl Into<String>) -> Self {
        Self {
            path_group: path_group.into(),
            setup_wns: 0.0,
            setup_tns: 0.0,
            setup_violating: 0,
            setup_endpoints: 0,
            hold_wns: 0.0,
            hold_tns: 0.0,
            hold_violating: 0,
            hold_endpoints: 0,
        }
    }
}

/// Backward required-time propagation state and per-pin timing summary (`STA-REQ-0227`).
#[derive(Debug, Clone)]
pub struct BackwardPass {
    n_nodes: usize,
    ntags: usize,
    tagged_req: Vec<f64>,
    pin_required: Vec<[[f64; 2]; 2]>,
    pin_arrival: Vec<[[f64; 2]; 2]>,
    pin_slack: Vec<[[f64; 2]; 2]>,
    worst_tag: Vec<[[Option<TagId>; 2]; 2]>,
    /// True for a pass that keeps only the per-pin required times and slacks
    /// ([`Self::required_and_slack_only`]).
    lean: bool,
    pub requirements: Vec<EndpointRequirement>,
    pub endpoints: Vec<EndpointSummary>,
    pub latch_windows: Vec<LatchWindowRecord>,
    pub overall_summary: GroupSlackSummary,
    pub group_summaries: Vec<GroupSlackSummary>,
}

impl BackwardPass {
    /// Allocates an unseeded [`BackwardPass`] for `n_nodes` graph nodes and `ntags` tags.
    #[must_use]
    pub fn new(n_nodes: usize, ntags: usize) -> Self {
        Self::allocate(n_nodes, ntags, false)
    }

    /// Allocates an unseeded [`BackwardPass`] that keeps only what a timing table records: the
    /// per-pin required times and slacks. It has no per-pin arrival or worst tag (reported as
    /// absent and `None`), and [`Self::propagate`] releases its per-tag required times (reported
    /// as absent afterwards). On a design with millions of nodes this saves `64` bytes per node,
    /// and `32` per node and tag once propagated (AGENTS rule 9).
    #[must_use]
    pub fn required_and_slack_only(n_nodes: usize, ntags: usize) -> Self {
        Self::allocate(n_nodes, ntags, true)
    }

    fn allocate(n_nodes: usize, ntags: usize, lean: bool) -> Self {
        let ntags = ntags.max(1);
        let w = n_nodes * ntags * 4;
        let tagged_req = (0..w)
            .into_par_iter()
            .map(|i| {
                if i % 2 == 0 {
                    absent_req(An::Early)
                } else {
                    absent_req(An::Late)
                }
            })
            .collect();
        let default_req = [
            [absent_req(An::Early), absent_req(An::Late)],
            [absent_req(An::Early), absent_req(An::Late)],
        ];
        let default_arr = [[absent(0), absent(1)], [absent(0), absent(1)]];
        let default_slack = [[f64::INFINITY; 2]; 2];
        Self {
            n_nodes,
            ntags,
            tagged_req,
            pin_required: vec![default_req; n_nodes],
            pin_arrival: if lean {
                Vec::new()
            } else {
                vec![default_arr; n_nodes]
            },
            pin_slack: vec![default_slack; n_nodes],
            worst_tag: if lean {
                Vec::new()
            } else {
                vec![[[None; 2]; 2]; n_nodes]
            },
            lean,
            requirements: Vec::new(),
            endpoints: Vec::new(),
            latch_windows: Vec::new(),
            overall_summary: GroupSlackSummary::new("**"),
            group_summaries: Vec::new(),
        }
    }

    #[inline]
    #[must_use]
    pub fn num_nodes(&self) -> usize {
        self.n_nodes
    }

    #[inline]
    #[must_use]
    pub fn num_tags(&self) -> usize {
        self.ntags
    }

    #[inline]
    fn req_idx(&self, node: usize, tag: usize, tr: usize, an: usize) -> usize {
        ((node * self.ntags + tag) * 2 + tr) * 2 + an
    }

    /// Seeds a required time `required_ns` at `(node, tag, tr, an)` (`min` for `An::Late`, `max`
    /// for `An::Early`; `STA-REQ-0227`).
    pub fn seed_required(&mut self, node: NodeId, tag: usize, tr: Tr, an: An, required_ns: f64) {
        if node.index() >= self.n_nodes || tag >= self.ntags || !required_ns.is_finite() {
            return;
        }
        let idx = self.req_idx(node.index(), tag, tr.index(), an.index());
        let val = canon_zero(required_ns);
        match an {
            An::Early => self.tagged_req[idx] = canon_zero(self.tagged_req[idx].max(val)),
            An::Late => self.tagged_req[idx] = canon_zero(self.tagged_req[idx].min(val)),
        }
    }

    /// Seeds a required time at `(node, tag, tr, an)` from `arrival_ns` and `slack_ns`.
    pub fn seed_from_slack(
        &mut self,
        node: NodeId,
        tag: usize,
        tr: Tr,
        an: An,
        arrival_ns: f64,
        slack_ns: f64,
    ) {
        if !arrival_ns.is_finite() || !slack_ns.is_finite() {
            return;
        }
        let req = match an {
            An::Late => arrival_ns + slack_ns,
            An::Early => arrival_ns - slack_ns,
        };
        self.seed_required(node, tag, tr, an, req);
    }

    /// Records `req` in `self.requirements` and seeds its required time into `self.tagged_req`.
    pub fn seed_requirement(&mut self, req: EndpointRequirement) {
        self.seed_required(req.node, req.tag.index(), req.tr, req.an, req.required_ns);
        self.requirements.push(req);
    }

    #[inline]
    #[must_use]
    pub fn tagged_required(&self, node: NodeId, tag: usize, tr: Tr, an: An) -> f64 {
        self.tagged_req
            .get(self.req_idx(node.index(), tag, tr.index(), an.index()))
            .copied()
            .unwrap_or(absent_req(an))
    }

    #[inline]
    #[must_use]
    pub fn pin_required(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.pin_required[node.index()][tr.index()][an.index()]
    }

    #[inline]
    #[must_use]
    pub fn pin_arrival(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        if self.lean {
            return absent(an.index());
        }
        self.pin_arrival[node.index()][tr.index()][an.index()]
    }

    #[inline]
    #[must_use]
    pub fn pin_slack(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.pin_slack[node.index()][tr.index()][an.index()]
    }

    #[inline]
    #[must_use]
    pub fn setup_slack(&self, node: NodeId, tr: Tr) -> f64 {
        self.pin_slack(node, tr, An::Late)
    }

    #[inline]
    #[must_use]
    pub fn hold_slack(&self, node: NodeId, tr: Tr) -> f64 {
        self.pin_slack(node, tr, An::Early)
    }

    #[inline]
    #[must_use]
    pub fn worst_setup_slack(&self, node: NodeId) -> f64 {
        self.setup_slack(node, Tr::Rise)
            .min(self.setup_slack(node, Tr::Fall))
    }

    #[inline]
    #[must_use]
    pub fn worst_hold_slack(&self, node: NodeId) -> f64 {
        self.hold_slack(node, Tr::Rise)
            .min(self.hold_slack(node, Tr::Fall))
    }

    #[inline]
    #[must_use]
    pub fn worst_tag(&self, node: NodeId, tr: Tr, an: An) -> Option<TagId> {
        if self.lean {
            return None;
        }
        self.worst_tag[node.index()][tr.index()][an.index()]
    }

    /// Builds `self.endpoints`, `self.overall_summary`, and `self.group_summaries` from
    /// `self.requirements` in canonical topological order (`STA-REQ-0214`, `STA-REQ-0224`).
    pub fn build_endpoint_summaries(&mut self, netview: &NetlistView<'_>, graph: &TimingGraph) {
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct EpKey {
            topo: u32,
            node: u32,
            is_async: bool,
            group: String,
        }

        let mut map: BTreeMap<EpKey, EndpointSummary> = BTreeMap::new();
        for req in &self.requirements {
            let is_async = matches!(
                req.role,
                EndpointCheckRole::Recovery | EndpointCheckRole::Removal
            );
            let key = EpKey {
                topo: graph.topo_rank(req.node),
                node: req.node.0,
                is_async,
                group: req.path_group.clone(),
            };
            let ep = map.entry(key).or_insert_with(|| EndpointSummary {
                node: req.node,
                pin_name: netview.node_name_str(req.node).into_owned(),
                clock_pin: req.clock_pin,
                capture_clock: req.capture_clock,
                path_group: req.path_group.clone(),
                setup_role: None,
                hold_role: None,
                setup_slack: None,
                hold_slack: None,
                worst_setup: None,
                worst_hold: None,
            });
            let cand = (
                req.tag,
                req.tr,
                req.arrival_ns,
                req.required_ns,
                req.check_ns,
            );
            if req.an == An::Late {
                if ep.setup_slack.is_none_or(|s| req.slack_ns < s) {
                    ep.setup_slack = Some(req.slack_ns);
                    ep.setup_role = Some(req.role);
                    ep.worst_setup = Some(cand);
                }
            } else if ep.hold_slack.is_none_or(|s| req.slack_ns < s) {
                ep.hold_slack = Some(req.slack_ns);
                ep.hold_role = Some(req.role);
                ep.worst_hold = Some(cand);
            }
        }

        self.endpoints = map.into_values().collect();

        let mut overall = GroupSlackSummary::new("**");
        let mut by_group: BTreeMap<String, GroupSlackSummary> = BTreeMap::new();

        for ep in &self.endpoints {
            let g_sum = by_group
                .entry(ep.path_group.clone())
                .or_insert_with(|| GroupSlackSummary::new(ep.path_group.clone()));
            if let Some(s) = ep.setup_slack {
                overall.setup_endpoints += 1;
                g_sum.setup_endpoints += 1;
                if s < 0.0 {
                    overall.setup_wns = canon_zero(overall.setup_wns.min(s));
                    overall.setup_tns = canon_zero(overall.setup_tns + s);
                    overall.setup_violating += 1;
                    g_sum.setup_wns = canon_zero(g_sum.setup_wns.min(s));
                    g_sum.setup_tns = canon_zero(g_sum.setup_tns + s);
                    g_sum.setup_violating += 1;
                }
            }
            if let Some(h) = ep.hold_slack {
                overall.hold_endpoints += 1;
                g_sum.hold_endpoints += 1;
                if h < 0.0 {
                    overall.hold_wns = canon_zero(overall.hold_wns.min(h));
                    overall.hold_tns = canon_zero(overall.hold_tns + h);
                    overall.hold_violating += 1;
                    g_sum.hold_wns = canon_zero(g_sum.hold_wns.min(h));
                    g_sum.hold_tns = canon_zero(g_sum.hold_tns + h);
                    g_sum.hold_violating += 1;
                }
            }
        }

        self.overall_summary = overall;
        self.group_summaries = by_group.into_values().collect();
    }
}

/// Evaluates all endpoint checks (`Setup`, `Hold`, `Recovery`, `Removal`, `LatchSetup`,
/// `LatchHold`, `OutputMax`, `OutputMin`), records latch transparency windows, and runs the
/// backward required-time pass (`STA-REQ-0211` to `STA-REQ-0214`, `STA-REQ-0221`, `STA-REQ-0222`,
/// `STA-REQ-0224`, `STA-REQ-0227`).
#[must_use]
pub fn evaluate_checks_and_backward(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    schedule: &IdealSchedule,
    output_delays: &[OutputDelaySpec],
    lowered: &LoweredCorner<'_>,
    result: &LoweredResult,
) -> BackwardPass {
    let n_nodes = graph.num_nodes();
    let ntags = lowered.ntags as usize;
    let mut bp = BackwardPass::new(n_nodes, ntags);
    let clock_sense = propagate_clock_sense(graph, &schedule.clocks);
    let frozen = netview.frozen();
    let g_arcs = graph.arcs();
    let g_checks = graph.checks();

    let mut latch_arc_by_check: HashMap<usize, usize> = HashMap::new();
    for (ai, arc) in g_arcs.iter().enumerate() {
        if !graph.is_arc_disabled(ai)
            && let ArcKind::LatchData { setup_check, .. } = arc.kind
        {
            latch_arc_by_check.insert(setup_check as usize, ai);
        }
    }

    for (ci, ch) in g_checks.iter().enumerate() {
        if graph.is_check_disabled(ci) {
            continue;
        }
        let cell_row = frozen.instances[ch.inst as usize].cell;
        let cell_name = netview.cell_name(cell_row);
        let Some(tc) = lib.cell(cell_name) else {
            continue;
        };
        let Some(cc) = tc.checks.get(ch.lib_arc.index()) else {
            continue;
        };

        let latch_arc_idx = latch_arc_by_check.get(&ci).copied();
        let is_latch_data_pin = g_arcs.iter().enumerate().any(|(ai, a)| {
            !graph.is_arc_disabled(ai) && a.kind.is_latch_data() && a.src == ch.data_pin
        });
        let role = match (ch.kind, latch_arc_idx.is_some() || is_latch_data_pin) {
            (CheckKind::Setup, true) => EndpointCheckRole::LatchSetup,
            (CheckKind::Hold, true) => EndpointCheckRole::LatchHold,
            (CheckKind::Setup, false) => EndpointCheckRole::Setup,
            (CheckKind::Hold, false) => EndpointCheckRole::Hold,
            (CheckKind::Recovery, _) => EndpointCheckRole::Recovery,
            (CheckKind::Removal, _) => EndpointCheckRole::Removal,
        };
        let an = role.bound();

        for &(c, bits) in &clock_sense[ch.clock_pin.index()] {
            let ck_c = &schedule.clocks[c as usize];
            let group = role.default_path_group(Some(&ck_c.name));
            let clk_slew = ck_c.transition[an.index()];

            for e_c in source_edges(ch.edge, bits) {
                for t in 0..ntags {
                    let (l_idx, el_idx) = (t / 2, t % 2);
                    if l_idx >= schedule.clocks.len() {
                        continue;
                    }
                    let ck_l = &schedule.clocks[l_idx];
                    let e_l = if el_idx == 0 { Tr::Rise } else { Tr::Fall };

                    if let Some(ai) = latch_arc_idx
                        && role == EndpointCheckRole::LatchSetup
                    {
                        let e_open = e_c.opposite();
                        let open_tag = TagId(c * 2 + e_open.index() as u32);
                        let w_ns = pulse_width_ns(ck_c, e_open, e_c);
                        let Some(d_close) = setup_relation_ns(ck_l, e_l, ck_c, e_c) else {
                            continue;
                        };
                        let d_open = d_close - w_ns;
                        let r_open = canon_zero(
                            d_open + ck_c.latency[An::Early.index()] - ck_c.uncertainty[0],
                        );
                        let max_b = schedule.max_time_borrow.get(&(ai as u32)).copied();
                        let mut r_close_arr = [f64::INFINITY; 2];
                        let mut borrowed_arr = [0.0; 2];
                        let mut any_arr = false;

                        for tr in Tr::ALL {
                            if cc.table[tr.index()] == NO_TABLE {
                                continue;
                            }
                            let d_slew = result.slew(graph, ch.data_pin, tr, An::Late);
                            let s_in = if d_slew.is_finite() { d_slew } else { 0.0 };
                            let s_val = lib.eval_check(cc, tr, clk_slew, s_in, 0.0);
                            let r_close_unb = d_close + ck_c.latency[An::Early.index()]
                                - s_val
                                - ck_c.uncertainty[0];
                            let r_close = canon_zero(match max_b {
                                Some(b) => r_close_unb.min(r_open + b),
                                None => r_close_unb,
                            });
                            r_close_arr[tr.index()] = r_close;

                            let arr = result.arrival(graph, ch.data_pin, t, tr, An::Late);
                            if !arr.is_finite() {
                                continue;
                            }
                            any_arr = true;
                            let (slack, req_ns, b_ns) = if arr <= r_open {
                                (canon_zero(r_open - arr), r_open, 0.0)
                            } else if arr <= r_close {
                                (0.0, canon_zero(arr), canon_zero(arr - r_open))
                            } else {
                                (
                                    canon_zero(r_close - arr),
                                    r_close,
                                    canon_zero(r_close - r_open),
                                )
                            };
                            borrowed_arr[tr.index()] = b_ns;
                            bp.seed_requirement(EndpointRequirement {
                                node: ch.data_pin,
                                tag: TagId(t as u32),
                                tr,
                                an: An::Late,
                                required_ns: req_ns,
                                arrival_ns: canon_zero(arr),
                                slack_ns: slack,
                                check_ns: canon_zero(s_val),
                                capture_clock: Some(ClockId(c)),
                                capture_edge: Some(e_c),
                                clock_pin: Some(ch.clock_pin),
                                role,
                                path_group: group.clone(),
                            });
                        }

                        if any_arr {
                            bp.latch_windows.push(LatchWindowRecord {
                                arc_idx: ai as u32,
                                inst: ch.inst,
                                data_pin: ch.data_pin,
                                enable_pin: ch.clock_pin,
                                out_pin: g_arcs[ai].dst,
                                src_tag: TagId(t as u32),
                                open_tag,
                                d_open_ns: canon_zero(d_open),
                                d_close_ns: canon_zero(d_close),
                                r_open_ns: r_open,
                                r_close_ns: r_close_arr,
                                borrowed_ns: borrowed_arr,
                            });
                        }
                        continue;
                    }

                    let rel = if an == An::Late {
                        setup_relation_ns(ck_l, e_l, ck_c, e_c)
                    } else {
                        hold_relation_ns(ck_l, e_l, ck_c, e_c)
                    };
                    let Some(rel_ns) = rel else {
                        continue;
                    };

                    for tr in Tr::ALL {
                        if cc.table[tr.index()] == NO_TABLE {
                            continue;
                        }
                        let arr = result.arrival(graph, ch.data_pin, t, tr, an);
                        if !arr.is_finite() {
                            continue;
                        }
                        let d_slew = result.slew(graph, ch.data_pin, tr, an);
                        let s_in = if d_slew.is_finite() { d_slew } else { 0.0 };
                        let chk_val = lib.eval_check(cc, tr, clk_slew, s_in, 0.0);
                        let (req_ns, slack_ns) = match an {
                            An::Late => {
                                let r = canon_zero(
                                    rel_ns + ck_c.latency[An::Early.index()]
                                        - chk_val
                                        - ck_c.uncertainty[0],
                                );
                                (r, canon_zero(r - arr))
                            }
                            An::Early => {
                                let r = canon_zero(
                                    rel_ns
                                        + ck_c.latency[An::Late.index()]
                                        + chk_val
                                        + ck_c.uncertainty[1],
                                );
                                (r, canon_zero(arr - r))
                            }
                        };
                        bp.seed_requirement(EndpointRequirement {
                            node: ch.data_pin,
                            tag: TagId(t as u32),
                            tr,
                            an,
                            required_ns: req_ns,
                            arrival_ns: canon_zero(arr),
                            slack_ns,
                            check_ns: canon_zero(chk_val),
                            capture_clock: Some(ClockId(c)),
                            capture_edge: Some(e_c),
                            clock_pin: Some(ch.clock_pin),
                            role,
                            path_group: group.clone(),
                        });
                    }
                }
            }
        }
    }

    for od in output_delays {
        let Some(ck_c) = schedule.clocks.get(od.clock as usize) else {
            continue;
        };
        let e_c = od.edge;
        for t in 0..ntags {
            let (l_idx, el_idx) = (t / 2, t % 2);
            if l_idx >= schedule.clocks.len() {
                continue;
            }
            let ck_l = &schedule.clocks[l_idx];
            let e_l = if el_idx == 0 { Tr::Rise } else { Tr::Fall };

            for tr in Tr::ALL {
                let ti = tr.index();
                if let Some(v_max) = od.delay_ns[An::Late.index()][ti]
                    && let Some(d_s) = setup_relation_ns(ck_l, e_l, ck_c, e_c)
                {
                    let arr = result.arrival(graph, od.node, t, tr, An::Late);
                    if arr.is_finite() {
                        let lat_c = if od.latency_included {
                            0.0
                        } else {
                            ck_c.latency[An::Early.index()]
                        };
                        let req_ns = canon_zero(d_s + lat_c - v_max - ck_c.uncertainty[0]);
                        bp.seed_requirement(EndpointRequirement {
                            node: od.node,
                            tag: TagId(t as u32),
                            tr,
                            an: An::Late,
                            required_ns: req_ns,
                            arrival_ns: canon_zero(arr),
                            slack_ns: canon_zero(req_ns - arr),
                            check_ns: canon_zero(v_max),
                            capture_clock: Some(ClockId(od.clock)),
                            capture_edge: Some(e_c),
                            clock_pin: None,
                            role: EndpointCheckRole::OutputMax,
                            path_group: ck_c.name.clone(),
                        });
                    }
                }

                if let Some(v_min) = od.delay_ns[An::Early.index()][ti]
                    && let Some(d_h) = hold_relation_ns(ck_l, e_l, ck_c, e_c)
                {
                    let arr = result.arrival(graph, od.node, t, tr, An::Early);
                    if arr.is_finite() {
                        let lat_c = if od.latency_included {
                            0.0
                        } else {
                            ck_c.latency[An::Late.index()]
                        };
                        let req_ns = canon_zero(d_h + lat_c - v_min + ck_c.uncertainty[1]);
                        bp.seed_requirement(EndpointRequirement {
                            node: od.node,
                            tag: TagId(t as u32),
                            tr,
                            an: An::Early,
                            required_ns: req_ns,
                            arrival_ns: canon_zero(arr),
                            slack_ns: canon_zero(arr - req_ns),
                            check_ns: canon_zero(v_min),
                            capture_clock: Some(ClockId(od.clock)),
                            capture_edge: Some(e_c),
                            clock_pin: None,
                            role: EndpointCheckRole::OutputMin,
                            path_group: ck_c.name.clone(),
                        });
                    }
                }
            }
        }
    }

    bp.propagate(graph, lowered, result);
    bp.build_endpoint_summaries(netview, graph);
    bp
}

/// Runs forward propagation and backward required-time analysis for one corner from
/// [`CompiledConstraints`] (`STA-REQ-0221`, `STA-REQ-0222`, `STA-REQ-0224`, `STA-REQ-0227`).
pub fn analyze_corner_with_constraints<'a>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &'a TimingLib,
    compiled: &CompiledConstraints,
    backend: BackendKind,
    precision: Precision,
    ctx: Ctx<'_>,
) -> Result<(LoweredCorner<'a>, LoweredResult, BackwardPass), Diag> {
    let mut overrides = NetLoadOverrides::new();
    for (&node, pl) in &compiled.port_loads {
        overrides.add_port_bit_load(node.0, pl.load_pf[0], pl.load_pf[1]);
    }
    for (&nr, nl) in &compiled.net_loads {
        if nl.wire_load {
            overrides.add_net_wire_load(nr, nl.load_pf[0], nl.load_pf[1]);
        } else {
            overrides.add_net_pin_load(nr, nl.load_pf[0], nl.load_pf[1]);
        }
    }
    let loads = NetLoads::compute(netview.frozen(), lib, &Ideal, &overrides);
    let (sched, outputs) = schedule_and_outputs_from_compiled(compiled, graph);
    let lowered = lower_schedule_with_relaunch(netview, graph, lib, &loads, &sched)?;
    let result = lowered
        .propagate(backend, precision, ctx)
        .map_err(|e| crate::engine::prop::lower::compute_error_diag(&e))?;
    let bp = evaluate_checks_and_backward(netview, graph, lib, &sched, &outputs, &lowered, &result);
    Ok((lowered, result, bp))
}
