// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Hash-consed exception states, propagation tag interning, and per-node state budget
//! enforcement (`STA-REQ-0269`, owner: E8).

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;
use rusteda_core::diag::Diag;

use crate::engine::graph::TimingGraph;
use crate::engine::input::constr::DesignIndex;
use crate::engine::types::{ClockId, NodeId, Tr};

/// Default per-node exception state budget (`>= 64`, `STA-REQ-0269`).
pub const DEFAULT_NODE_EXCEPTION_STATE_BUDGET: usize = 64;

/// Dense 0-based index of a compiled timing exception in [`crate::engine::input::constr::CompiledConstraints::exceptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExceptionId(pub u32);

impl ExceptionId {
    /// Returns the index as `usize`.
    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Hash-consed identifier of an active exception state carried in a propagation tag (`STA-REQ-0269`).
///
/// `ExceptionState(0)` ([`ExceptionState::EMPTY`]) is always the empty `-from` match set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ExceptionState(pub u32);

impl ExceptionState {
    /// The empty exception state (no `-from`-restricted exceptions matched at the startpoint).
    pub const EMPTY: Self = Self(0);

    /// Returns the state index as `usize`.
    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A single `-from` exception matched at a startpoint, paired with its `-from` specificity
/// contribution (`16` for port/pin/cell match, `2` for clock match; `STA-REQ-0255`, `STA-REQ-0257`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StartMatchEntry {
    /// Matched exception index.
    pub exc_id: ExceptionId,
    /// Specificity contribution from `-from` (`16` for port/pin/cell, `2` for clock).
    pub from_spec: u8,
}

/// Canonical sorted list of `-from` exception matches representing an [`ExceptionState`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ExceptionStateData {
    /// Sorted by `exc_id` in strictly increasing order.
    pub entries: Vec<StartMatchEntry>,
}

impl ExceptionStateData {
    /// Looks up the `-from` specificity for `exc_id` if present in this state.
    #[inline]
    #[must_use]
    pub fn from_specificity(&self, exc_id: ExceptionId) -> Option<u8> {
        self.entries
            .binary_search_by_key(&exc_id, |e| e.exc_id)
            .ok()
            .map(|i| self.entries[i].from_spec)
    }

    /// True if no `-from` exceptions are active in this state.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Hash-consed table mapping canonical [`ExceptionStateData`] sets to dense [`ExceptionState`] IDs
/// (`STA-REQ-0269`).
#[derive(Debug, Clone)]
pub struct ExceptionStateTable {
    states: Vec<ExceptionStateData>,
    by_data: FxHashMap<ExceptionStateData, ExceptionState>,
}

impl Default for ExceptionStateTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ExceptionStateTable {
    /// Creates a new hash-consed exception state table with [`ExceptionState::EMPTY`] at index 0.
    #[must_use]
    pub fn new() -> Self {
        let empty = ExceptionStateData::default();
        let mut by_data = FxHashMap::default();
        by_data.insert(empty.clone(), ExceptionState::EMPTY);
        Self {
            states: vec![empty],
            by_data,
        }
    }

    /// Interns `entries` (normalizing and deduplicating by `exc_id`, keeping highest `from_spec`)
    /// and returns its canonical [`ExceptionState`].
    pub fn intern(&mut self, mut entries: Vec<StartMatchEntry>) -> ExceptionState {
        if entries.is_empty() {
            return ExceptionState::EMPTY;
        }
        entries.sort_by(|a, b| a.exc_id.cmp(&b.exc_id).then(b.from_spec.cmp(&a.from_spec)));
        entries.dedup_by_key(|e| e.exc_id);
        let data = ExceptionStateData { entries };
        if let Some(&id) = self.by_data.get(&data) {
            return id;
        }
        let id = ExceptionState(self.states.len() as u32);
        self.by_data.insert(data.clone(), id);
        self.states.push(data);
        id
    }

    /// Returns the [`ExceptionStateData`] for `state`.
    #[inline]
    #[must_use]
    pub fn get(&self, state: ExceptionState) -> &ExceptionStateData {
        &self.states[state.index()]
    }

    /// Returns the total number of distinct interned [`ExceptionState`]s.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Returns true if only [`ExceptionState::EMPTY`] exists.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.states.len() <= 1
    }
}

/// Startpoint launch descriptor used to resolve `-from` / `-rise_from` / `-fall_from` exceptions
/// (`STA-REQ-0251`, `STA-REQ-0252`, `STA-REQ-0255`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StartLaunchKey {
    /// Startpoint node (input port bit, sequential clock pin, or latch enable pin).
    pub start_node: NodeId,
    /// Start transition at the startpoint (`Rise`/`Fall` active pin transition for sequential
    /// clock/enable pins, or data transition at an input port; `STA-REQ-0251`).
    pub start_tr: Tr,
    /// Launch clock and source edge (`Rise` or `Fall` at the clock source), or `None` for an
    /// unclocked input port (`STA-REQ-0264`).
    pub launch_clock: Option<(ClockId, Tr)>,
}

/// Distinct propagation tag key combining launch clock edge, hash-consed [`ExceptionState`], and
/// launch clock latency `[early_ns, late_ns]` (`STA-REQ-0265`, `STA-REQ-0269`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropagationTagKey {
    /// Launch clock and source edge (`None` for unclocked input port paths).
    pub launch_clock: Option<(ClockId, Tr)>,
    /// Hash-consed `-from` exception state.
    pub exc_state: ExceptionState,
    /// IEEE-754 bits of `[early_launch_latency_ns, late_launch_latency_ns]` included in the tag's
    /// initial arrival (subtracted when `-ignore_clock_latency` wins; `STA-REQ-0265`).
    pub launch_latency_bits: [u64; 2],
}

impl PropagationTagKey {
    /// Creates a propagation tag key with canonicalized `+0.0` latency bits.
    #[must_use]
    pub fn new(
        launch_clock: Option<(ClockId, Tr)>,
        exc_state: ExceptionState,
        launch_latency: [f64; 2],
    ) -> Self {
        let canon = |v: f64| {
            if v == 0.0 {
                0.0f64.to_bits()
            } else {
                v.to_bits()
            }
        };
        Self {
            launch_clock,
            exc_state,
            launch_latency_bits: [canon(launch_latency[0]), canon(launch_latency[1])],
        }
    }

    /// Returns the `[early_ns, late_ns]` launch clock latency stored on this tag.
    #[inline]
    #[must_use]
    pub fn launch_latency(&self) -> [f64; 2] {
        [
            f64::from_bits(self.launch_latency_bits[0]),
            f64::from_bits(self.launch_latency_bits[1]),
        ]
    }
}

/// Interning table of [`PropagationTagKey`]s for lowering onto `rusteda_compute::timing`.
#[derive(Debug, Clone, Default)]
pub struct PropagationTagTable {
    tags: Vec<PropagationTagKey>,
    by_key: FxHashMap<PropagationTagKey, u32>,
}

impl PropagationTagTable {
    /// Creates an empty propagation tag table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `key` and returns its 0-based tag index.
    pub fn intern(&mut self, key: PropagationTagKey) -> u32 {
        if let Some(&id) = self.by_key.get(&key) {
            return id;
        }
        let id = self.tags.len() as u32;
        self.by_key.insert(key, id);
        self.tags.push(key);
        id
    }

    /// Looks up a tag index by `key`.
    #[inline]
    #[must_use]
    pub fn find(&self, key: &PropagationTagKey) -> Option<u32> {
        self.by_key.get(key).copied()
    }

    /// Returns the slice of all interned [`PropagationTagKey`]s in tag index order.
    #[inline]
    #[must_use]
    pub fn tags(&self) -> &[PropagationTagKey] {
        &self.tags
    }

    /// Returns the number of interned tags.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Returns true if no tags have been interned.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

/// Propagates reachable [`ExceptionState`] sets forward across `graph` in topological order and
/// verifies that no node exceeds `budget` distinct exception states (`STA-REQ-0269`).
///
/// Returns `Ok(max_states_at_any_node)` if every node is within `budget`, or `Err(Diag)` with a
/// `T0060` error diagnostic naming the offending node if any node exceeds `budget`.
pub fn check_node_exception_state_budget(
    graph: &TimingGraph,
    index: &DesignIndex,
    seeded_node_states: &[(NodeId, ExceptionState)],
    budget: usize,
) -> Result<usize, Diag> {
    let n_nodes = graph.num_nodes();
    let mut node_states: Vec<BTreeSet<ExceptionState>> = vec![BTreeSet::new(); n_nodes];
    for &(node, st) in seeded_node_states {
        if node.index() < n_nodes {
            node_states[node.index()].insert(st);
        }
    }

    let g_arcs = graph.arcs();
    let mut max_seen = 0usize;

    for &u in graph.order() {
        let u_idx = u.index();
        let count = node_states[u_idx].len();
        if count > max_seen {
            max_seen = count;
        }
        if count > budget {
            let node_name = index.node_name(u);
            return Err(Diag::error(
                "T0060",
                format!(
                    "node '{node_name}' has {count} distinct exception states, exceeding the per-node exception state budget of {budget} (STA-REQ-0269)"
                ),
            ));
        }
        if count == 0 {
            continue;
        }
        let u_set = node_states[u_idx].clone();
        for &ai in graph.outgoing(u) {
            if graph.is_arc_disabled(ai as usize) {
                continue;
            }
            let arc = &g_arcs[ai as usize];
            if arc.kind.is_latch_data() {
                // Paths across latch D -> Q re-launch in the latch enable pin's exception state
                // (already seeded at Q by the G -> Q launch arc; STA-REQ-0267).
                continue;
            }
            let v_idx = arc.dst.index();
            node_states[v_idx].extend(u_set.iter().copied());
        }
    }

    Ok(max_seen)
}
