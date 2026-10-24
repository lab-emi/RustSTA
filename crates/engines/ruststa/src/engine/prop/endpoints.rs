// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Endpoints: status, path groups and completeness (owner: E5).
//!
//! Engine section C. Every endpoint has a status: constrained, unclocked, no data arrival, false
//! path only, unconstrained output or disabled. Every status other than constrained is counted by
//! reason in the completeness of the analysis scope.

use std::collections::{BTreeMap, BTreeSet};

use rusteda_core::liberty::Direction;

use super::checks::{ASYNC_DEFAULT_GROUP, EndpointCheckKind, SlackRecord};
use super::clocks::ClockModel;
use super::clocks::ideal::IdealClockModel;
use super::gating::ClockGatingSite;
use super::lower::LoweredResult;
use super::tags::TagTable;
use crate::engine::graph::{DisableReason, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, DisabledTiming};
use crate::engine::input::{NetlistView, NodeKind};
use crate::engine::scope::{CornerSummary, UnconstrainedCounts};
use crate::engine::types::{An, NodeId, Tr};

/// Status of a timing endpoint (`STA-REQ-0225`, `STA-REQ-0226`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointStatus {
    /// At least one timing check was evaluated at this endpoint (`constrained`).
    Constrained,
    /// Sequential check pin whose clock pin receives no clock edge (`unclocked`).
    Unclocked,
    /// Endpoint is clocked (or has `set_output_delay`) and not disabled, but no data tag reaches
    /// the data pin (`no_data_arrival`).
    NoArrival,
    /// Data tags reach the pin and clocks reach the clock pin, but every `(tag, capture)` pair is
    /// excluded by `set_false_path` or `set_clock_groups` (`false_path_only`).
    FalsePathOnly,
    /// Output or inout port with no `set_output_delay` (`unconstrained_output`).
    UnconstrainedOutput,
    /// Endpoint whose check arcs are disabled by `set_disable_timing` or `when` (`disabled`).
    Disabled,
}

impl EndpointStatus {
    /// Alias matching the `no_data_arrival` spelling in `STA-REQ-0226`.
    #[allow(non_upper_case_globals)]
    pub const NoDataArrival: Self = Self::NoArrival;

    /// Canonical string identifier for this endpoint status (`STA-REQ-0226`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constrained => "constrained",
            Self::Unclocked => "unclocked",
            Self::NoArrival => "no_data_arrival",
            Self::FalsePathOnly => "false_path_only",
            Self::UnconstrainedOutput => "unconstrained_output",
            Self::Disabled => "disabled",
        }
    }
}

/// Aggregated setup, hold, recovery, and removal slacks of one endpoint [`NodeId`]
/// (`STA-REQ-0229`–`STA-REQ-0214`, `STA-REQ-0225`, `STA-REQ-0313`).
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointRecord {
    /// Endpoint node in the timing graph.
    pub node: NodeId,
    /// Endpoint status (`STA-REQ-0225`).
    pub status: EndpointStatus,
    /// Worst setup-family slack record (`Setup`, `Recovery`, `LatchSetup`, `OutputSetup`,
    /// `ClockGatingSetup`) at `node`, if any.
    pub setup: Option<SlackRecord>,
    /// Worst hold-family slack record (`Hold`, `Removal`, `LatchHold`, `OutputHold`,
    /// `ClockGatingHold`) at `node`, if any.
    pub hold: Option<SlackRecord>,
    /// Worst asynchronous `Recovery` slack record at `node`, if any (`STA-REQ-0203`).
    pub recovery: Option<SlackRecord>,
    /// Worst asynchronous `Removal` slack record at `node`, if any (`STA-REQ-0203`).
    pub removal: Option<SlackRecord>,
}

/// Summary of one path group (`capture_clock`, `"async_default"`, or user `group_path`) in a
/// corner (`STA-REQ-0203`, `STA-REQ-0226`, `STA-REQ-0302`).
#[derive(Debug, Clone, PartialEq)]
pub struct PathGroupSummary {
    /// Path group name.
    pub name: String,
    /// Worst setup slack in this group (ns), or `None` when no setup check belongs to this group.
    pub wns_setup_ns: Option<f64>,
    /// Total negative setup slack in this group (ns, `<= 0.0`).
    pub tns_setup_ns: f64,
    /// Number of endpoints violating setup (`slack < 0.0`) in this group.
    pub violating_setup: u32,
    /// Number of endpoints with a setup check in this group.
    pub endpoints_setup: u32,
    /// Worst hold slack in this group (ns), or `None` when no hold check belongs to this group.
    pub wns_hold_ns: Option<f64>,
    /// Total negative hold slack in this group (ns, `<= 0.0`).
    pub tns_hold_ns: f64,
    /// Number of endpoints violating hold (`slack < 0.0`) in this group.
    pub violating_hold: u32,
    /// Number of endpoints with a hold check in this group.
    pub endpoints_hold: u32,
}

impl PathGroupSummary {
    /// Creates an empty [`PathGroupSummary`] for `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            wns_setup_ns: None,
            tns_setup_ns: 0.0,
            violating_setup: 0,
            endpoints_setup: 0,
            wns_hold_ns: None,
            tns_hold_ns: 0.0,
            violating_hold: 0,
            endpoints_hold: 0,
        }
    }
}

/// The endpoint records of a corner in ascending [`NodeId`] order, looked up by binary search.
///
/// A sorted vector rather than a `BTreeMap`: building a map of millions of these large records
/// copied all of them once more (`STA-REQ-0225`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EndpointMap {
    records: Vec<EndpointRecord>,
}

impl EndpointMap {
    /// The map of `records`, sorted by node with each node once.
    fn from_sorted(records: Vec<EndpointRecord>) -> Self {
        debug_assert!(records.windows(2).all(|w| w[0].node < w[1].node));
        Self { records }
    }

    /// The record of `node`, if it is an endpoint.
    #[must_use]
    pub fn get(&self, node: &NodeId) -> Option<&EndpointRecord> {
        self.records
            .binary_search_by_key(node, |r| r.node)
            .ok()
            .map(|i| &self.records[i])
    }

    /// True when `node` is an endpoint.
    #[must_use]
    pub fn contains_key(&self, node: &NodeId) -> bool {
        self.get(node).is_some()
    }

    /// The number of endpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True when the corner has no endpoint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The endpoint nodes in ascending order.
    pub fn keys(&self) -> impl Iterator<Item = &NodeId> + '_ {
        self.records.iter().map(|r| &r.node)
    }

    /// The records in ascending node order.
    pub fn values(&self) -> std::slice::Iter<'_, EndpointRecord> {
        self.records.iter()
    }

    /// `(node, record)` pairs in ascending node order.
    pub fn iter(&self) -> std::iter::Map<std::slice::Iter<'_, EndpointRecord>, EndpointPair> {
        self.records.iter().map(endpoint_pair)
    }
}

/// The `(node, record)` pair of an endpoint record.
type EndpointPair = for<'a> fn(&'a EndpointRecord) -> (&'a NodeId, &'a EndpointRecord);

fn endpoint_pair(r: &EndpointRecord) -> (&NodeId, &EndpointRecord) {
    (&r.node, r)
}

impl std::ops::Index<&NodeId> for EndpointMap {
    type Output = EndpointRecord;

    fn index(&self, node: &NodeId) -> &EndpointRecord {
        self.get(node).expect("no endpoint record for this node")
    }
}

impl<'a> IntoIterator for &'a EndpointMap {
    type Item = (&'a NodeId, &'a EndpointRecord);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, EndpointRecord>, EndpointPair>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Complete endpoint and check results of one corner (`STA-REQ-0225`, `STA-REQ-0226`).
#[derive(Debug, Clone)]
pub struct CornerEndpoints {
    /// All individual check evaluations in deterministic order.
    pub records: Vec<SlackRecord>,
    /// Per-endpoint status and worst setup/hold/recovery/removal slacks in ascending [`NodeId`]
    /// order.
    pub endpoints: EndpointMap,
    /// Per-path-group WNS/TNS/violating/endpoint summaries.
    pub path_groups: BTreeMap<String, PathGroupSummary>,
    /// Counts of unconstrained endpoints by reason (`STA-REQ-0225`).
    pub unconstrained: UnconstrainedCounts,
    /// Legacy-compatible unconstrained count (counting each unclocked `CheckArc` plus each
    /// unconstrained output port, matching `ruststa::checks::Endpoints::unconstrained`).
    pub legacy_unconstrained: usize,
    /// Corner-level timing summary (`STA-REQ-0226`).
    pub summary: CornerSummary,
}

impl CornerEndpoints {
    /// Builds [`CornerEndpoints`] from the evaluated `records` and classifies every candidate
    /// endpoint of the corner (`STA-REQ-0225`, `STA-REQ-0226`).
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn build(
        corner_name: &str,
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        constraints: &CompiledConstraints,
        clock_model: &IdealClockModel,
        tags: &TagTable,
        gating_sites: &[ClockGatingSite],
        prop_res: &LoweredResult,
        records: Vec<SlackRecord>,
    ) -> Self {
        // 1. Collect all candidate endpoints. A dense node -> slot index replaces a map with
        // millions of entries (OpenC910); the first insertion of a node fixes its initial status,
        // as `BTreeMap::entry().or_insert` did, because the insertion order is unchanged.
        let fresh = |node: NodeId, status: EndpointStatus| EndpointRecord {
            node,
            status,
            setup: None,
            hold: None,
            recovery: None,
            removal: None,
        };
        let mut slot_of: Vec<u32> = vec![u32::MAX; graph.num_nodes()];
        let mut slots: Vec<EndpointRecord> = Vec::new();
        let mut slot = |slots: &mut Vec<EndpointRecord>, node: NodeId, status: EndpointStatus| {
            let s = &mut slot_of[node.index()];
            if *s == u32::MAX {
                *s = slots.len() as u32;
                slots.push(fresh(node, status));
            }
            *s as usize
        };
        // `(data pin, instance, clock pin)` of every check and gating site, by data pin.
        let mut check_pins: Vec<(NodeId, u32, NodeId)> =
            Vec::with_capacity(graph.checks().len() + gating_sites.len());
        for ch in graph.checks() {
            check_pins.push((ch.data_pin, ch.inst, ch.clock_pin));
            slot(&mut slots, ch.data_pin, EndpointStatus::NoArrival);
        }
        for site in gating_sites {
            check_pins.push((site.enable_pin, site.inst, site.clock_pin));
            slot(&mut slots, site.enable_pin, EndpointStatus::NoArrival);
        }
        check_pins.sort_by_key(|&(node, _, _)| node);

        let mut output_delay_nodes = BTreeSet::new();
        for d in &constraints.output_delays {
            if d.node.index() < graph.num_nodes() {
                output_delay_nodes.insert(d.node);
                slot(&mut slots, d.node, EndpointStatus::NoArrival);
            }
        }

        let mut output_ports = BTreeSet::new();
        for i in 0..graph.num_port_bits() {
            let nid = NodeId(i as u32);
            if matches!(
                netview.node_kind(nid),
                NodeKind::PortBit {
                    dir: Direction::Output,
                    ..
                }
            ) {
                output_ports.insert(nid);
                slot(&mut slots, nid, EndpointStatus::UnconstrainedOutput);
            }
        }

        // 2. Fold `records` into the endpoints (keeping the worst slack per check family; on
        // ties, the first record in deterministic evaluation order wins, matching
        // `checks::keep`).
        for rec in &records {
            let s = slot(&mut slots, rec.endpoint, EndpointStatus::Constrained);
            let ep = &mut slots[s];
            ep.status = EndpointStatus::Constrained;
            if rec.kind.is_setup_family() {
                keep_worse(&mut ep.setup, rec);
                if rec.kind == EndpointCheckKind::Recovery {
                    keep_worse(&mut ep.recovery, rec);
                }
            } else {
                keep_worse(&mut ep.hold, rec);
                if rec.kind == EndpointCheckKind::Removal {
                    keep_worse(&mut ep.removal, rec);
                }
            }
        }
        drop(slot_of);
        // 3. Classify every non-Constrained candidate endpoint into its exact `EndpointStatus`
        // (`STA-REQ-0225`).
        let disabled_nodes = collect_disabled_nodes(netview, graph, constraints);
        let mut unconstrained = UnconstrainedCounts::default();

        for ep in &mut slots {
            let node = ep.node;
            if ep.status == EndpointStatus::Constrained {
                continue;
            }
            if disabled_nodes.contains(&node) {
                ep.status = EndpointStatus::Disabled;
                unconstrained.disabled += 1;
                continue;
            }
            if output_ports.contains(&node) && !output_delay_nodes.contains(&node) {
                ep.status = EndpointStatus::UnconstrainedOutput;
                unconstrained.unconstrained_output += 1;
                continue;
            }
            let first = check_pins.partition_point(|&(n, _, _)| n < node);
            let pairs = &check_pins
                [first..first + check_pins[first..].partition_point(|&(n, _, _)| n == node)];
            if !pairs.is_empty() {
                let any_clocked = pairs
                    .iter()
                    .any(|&(_, _, ck_pin)| clock_model.is_clock_network_pin(ck_pin));
                if !any_clocked {
                    ep.status = EndpointStatus::Unclocked;
                    unconstrained.unclocked += 1;
                    continue;
                }
            } else if output_delay_nodes.contains(&node) {
                let any_clocked = constraints
                    .output_delays
                    .iter()
                    .any(|d| d.node == node && d.clock.is_some());
                if !any_clocked {
                    ep.status = EndpointStatus::Unclocked;
                    unconstrained.unclocked += 1;
                    continue;
                }
            }

            let has_arr = node_has_any_arrival(graph, prop_res, tags, node);
            if !has_arr {
                ep.status = EndpointStatus::NoArrival;
                unconstrained.no_arrival += 1;
            } else if has_excluding_exception(constraints, node) {
                ep.status = EndpointStatus::FalsePathOnly;
                unconstrained.false_path_only += 1;
            } else {
                ep.status = EndpointStatus::NoArrival;
                unconstrained.no_arrival += 1;
            }
        }
        // Ascending node order (each node has one slot).
        slots.sort_unstable_by_key(|ep| ep.node);
        let endpoints = EndpointMap::from_sorted(slots);

        // Compute legacy unconstrained count for legacy parity verification.
        let unclocked_checks = graph
            .checks()
            .iter()
            .filter(|ch| !clock_model.is_clock_network_pin(ch.clock_pin))
            .count();
        let unclocked_outputs = output_ports
            .iter()
            .filter(|n| !output_delay_nodes.contains(n))
            .count();
        let legacy_unconstrained = unclocked_checks + unclocked_outputs;

        // 4. Build per-path-group summaries and overall corner summary (`STA-REQ-0226`).
        let mut path_groups: BTreeMap<String, PathGroupSummary> = BTreeMap::new();
        for ck in &constraints.clocks {
            path_groups.insert(ck.name.clone(), PathGroupSummary::new(ck.name.clone()));
        }

        // Each endpoint contributes its worst setup and worst hold slack within each path group,
        // visited by `(group name, endpoint)` as a map keyed so would visit them; a stable sort
        // keeps the records of one key in evaluation order, so the folded minima are the same.
        let names: Vec<&str> = records
            .iter()
            .map(|r| r.path_group.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let rank: rustc_hash::FxHashMap<&str, u32> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (*n, i as u32))
            .collect();
        let mut setup_keys: Vec<(u32, NodeId, f64)> = Vec::new();
        let mut hold_keys: Vec<(u32, NodeId, f64)> = Vec::new();
        for rec in &records {
            let key = (rank[rec.path_group.as_str()], rec.endpoint, rec.slack);
            if rec.kind.is_setup_family() {
                setup_keys.push(key);
            } else {
                hold_keys.push(key);
            }
        }
        let fold = |mut keys: Vec<(u32, NodeId, f64)>| -> Vec<(u32, f64)> {
            keys.sort_by_key(|&(g, n, _)| (g, n));
            let mut out: Vec<(u32, f64)> = Vec::new();
            let mut last: Option<(u32, NodeId)> = None;
            for (g, n, slack) in keys {
                if last == Some((g, n)) {
                    let s = &mut out.last_mut().expect("a folded key").1;
                    *s = s.min(slack);
                } else {
                    out.push((g, slack));
                    last = Some((g, n));
                }
            }
            out
        };

        for (g, slack) in fold(setup_keys) {
            let gname = names[g as usize];
            let g = path_groups
                .entry(gname.to_string())
                .or_insert_with(|| PathGroupSummary::new(gname.to_string()));
            g.endpoints_setup += 1;
            g.wns_setup_ns = Some(match g.wns_setup_ns {
                Some(prev) => canon_zero(prev.min(slack)),
                None => canon_zero(slack),
            });
            if slack < 0.0 {
                g.violating_setup += 1;
                g.tns_setup_ns = canon_zero(g.tns_setup_ns + slack);
            }
        }
        for (g, slack) in fold(hold_keys) {
            let gname = names[g as usize];
            let g = path_groups
                .entry(gname.to_string())
                .or_insert_with(|| PathGroupSummary::new(gname.to_string()));
            g.endpoints_hold += 1;
            g.wns_hold_ns = Some(match g.wns_hold_ns {
                Some(prev) => canon_zero(prev.min(slack)),
                None => canon_zero(slack),
            });
            if slack < 0.0 {
                g.violating_hold += 1;
                g.tns_hold_ns = canon_zero(g.tns_hold_ns + slack);
            }
        }

        // Overall corner summary across all constrained endpoints (`STA-REQ-0226`).
        let mut wns_setup_ns: Option<f64> = None;
        let mut tns_setup_ns: f64 = 0.0;
        let mut violating_setup = 0u32;
        let mut wns_hold_ns: Option<f64> = None;
        let mut tns_hold_ns: f64 = 0.0;
        let mut violating_hold = 0u32;
        let mut constrained_count = 0u32;

        for ep in endpoints.values() {
            if ep.status == EndpointStatus::Constrained {
                constrained_count += 1;
            }
            if let Some(s) = &ep.setup {
                wns_setup_ns = Some(match wns_setup_ns {
                    Some(prev) => canon_zero(prev.min(s.slack)),
                    None => canon_zero(s.slack),
                });
                if s.slack < 0.0 {
                    violating_setup += 1;
                    tns_setup_ns = canon_zero(tns_setup_ns + s.slack);
                }
            }
            if let Some(h) = &ep.hold {
                wns_hold_ns = Some(match wns_hold_ns {
                    Some(prev) => canon_zero(prev.min(h.slack)),
                    None => canon_zero(h.slack),
                });
                if h.slack < 0.0 {
                    violating_hold += 1;
                    tns_hold_ns = canon_zero(tns_hold_ns + h.slack);
                }
            }
        }

        let _ = ASYNC_DEFAULT_GROUP;
        let summary = CornerSummary {
            corner: corner_name.to_string(),
            wns_setup_ns,
            tns_setup_ns,
            wns_hold_ns,
            tns_hold_ns,
            violating_setup,
            violating_hold,
            endpoints: constrained_count,
            unconstrained_endpoints: unconstrained.total(),
        };

        Self {
            records,
            endpoints,
            path_groups,
            unconstrained,
            legacy_unconstrained,
            summary,
        }
    }

    /// Returns the worst setup slack (ns) at `node`, if any.
    #[inline]
    #[must_use]
    pub fn setup_slack(&self, node: NodeId) -> Option<f64> {
        self.endpoints
            .get(&node)
            .and_then(|e| e.setup.as_ref().map(|s| s.slack))
    }

    /// Returns the worst hold slack (ns) at `node`, if any.
    #[inline]
    #[must_use]
    pub fn hold_slack(&self, node: NodeId) -> Option<f64> {
        self.endpoints
            .get(&node)
            .and_then(|e| e.hold.as_ref().map(|s| s.slack))
    }
}

fn keep_worse(slot: &mut Option<SlackRecord>, cand: &SlackRecord) {
    match slot {
        Some(prev) if prev.slack <= cand.slack => {}
        _ => *slot = Some(cand.clone()),
    }
}

fn node_has_any_arrival(
    graph: &TimingGraph,
    prop_res: &LoweredResult,
    tags: &TagTable,
    node: NodeId,
) -> bool {
    for (tid, _) in tags.iter() {
        for tr in Tr::ALL {
            for an in An::ALL {
                if an.is_present(prop_res.arrival(graph, node, tid.index(), tr, an)) {
                    return true;
                }
            }
        }
    }
    false
}

fn has_excluding_exception(constraints: &CompiledConstraints, node: NodeId) -> bool {
    if !constraints
        .clock_groups
        .iter()
        .all(|cg| cg.allow_paths || cg.groups.len() <= 1)
    {
        return true;
    }
    constraints.exceptions.iter().any(|ex| {
        matches!(
            ex.kind,
            crate::engine::input::constr::ExceptionKind::FalsePath { .. }
        ) && ex
            .to
            .as_ref()
            .is_none_or(|to| to.is_empty() || to.has_node(node))
    })
}

fn collect_disabled_nodes(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for dt in &constraints.disabled_timing {
        match dt {
            DisabledTiming::Port { port } => {
                out.insert(*port);
            }
            DisabledTiming::Pin { pin } => {
                if let Some(nid) = pin.node {
                    out.insert(nid);
                }
            }
            _ => {}
        }
    }
    if !constraints.disabled_timing.is_empty() {
        let mut by_pin: BTreeMap<NodeId, (usize, usize)> = BTreeMap::new();
        for ch in graph.checks() {
            let entry = by_pin.entry(ch.data_pin).or_insert((0, 0));
            entry.0 += 1;
            if super::checks::is_check_disabled(netview, ch, &constraints.disabled_timing) {
                entry.1 += 1;
            }
        }
        for (pin, (total, disabled_cnt)) in by_pin {
            if total > 0 && total == disabled_cnt {
                out.insert(pin);
            }
        }
    }
    for (ai, arc) in graph.arcs().iter().enumerate() {
        if matches!(
            graph.arc_disable_reason(ai),
            Some(DisableReason::UserDisabled | DisableReason::FalseWhen)
        ) && graph.incoming(arc.dst).is_empty()
        {
            out.insert(arc.dst);
        }
    }
    out
}

#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}
