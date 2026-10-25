// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Start rows keyed by timing graph nodes (`STA-REQ-0401`, `COMPUTE-REQ-0015`).

use std::collections::BTreeMap;

use rusteda_compute::timing::{StartRows, absent};

use super::canon_zero;
use crate::engine::graph::TimingGraph;
use crate::engine::types::{An, NodeId, Tr};

/// Builder for [`StartRows`] keyed by [`TimingGraph`] [`NodeId`]s, automatically translating each
/// [`NodeId`] to its level-ordered topological rank (`graph.topo_rank(node)`, `STA-REQ-0401`,
/// `COMPUTE-REQ-0015`).
#[derive(Debug, Clone)]
pub struct GraphStartBuilder {
    ntags: u32,
    rows: BTreeMap<u32, (Vec<f64>, [f64; 4])>,
}

impl GraphStartBuilder {
    /// Creates a [`GraphStartBuilder`] for `ntags` propagation tags (`ntags >= 1`).
    #[must_use]
    pub fn new(ntags: u32) -> Self {
        let ntags = ntags.max(1);
        Self {
            ntags,
            rows: BTreeMap::new(),
        }
    }

    /// Number of tags.
    #[inline]
    #[must_use]
    pub fn ntags(&self) -> u32 {
        self.ntags
    }

    fn entry(&mut self, pos: u32) -> &mut (Vec<f64>, [f64; 4]) {
        let w = self.ntags as usize * 4;
        self.rows.entry(pos).or_insert_with(|| {
            let arr = (0..w).map(|i| absent(i % 2)).collect();
            let slew = [absent(0), absent(1), absent(0), absent(1)];
            (arr, slew)
        })
    }

    /// Overwrites the `[early, late]` arrival and transition on `node` for `tag` and `tr`.
    pub fn set_tagged(
        &mut self,
        graph: &TimingGraph,
        node: NodeId,
        tag: u32,
        tr: Tr,
        arr: [f64; 2],
        slew: [f64; 2],
    ) {
        let pos = graph.topo_rank(node);
        let (a, s) = self.entry(pos);
        let ti = tr.index();
        let base = (tag as usize * 2 + ti) * 2;
        a[base] = canon_zero(arr[0]);
        a[base + 1] = canon_zero(arr[1]);
        s[ti * 2] = canon_zero(slew[0].max(0.0));
        s[ti * 2 + 1] = canon_zero(slew[1].max(0.0));
    }

    /// Merges a single `(tag, tr, an)` arrival and transition into `node` (`min` for `An::Early`,
    /// `max` for `An::Late`; `STA-REQ-0115`, `STA-REQ-0201`).
    pub fn merge_bound(
        &mut self,
        graph: &TimingGraph,
        node: NodeId,
        tag: u32,
        tr: Tr,
        an: An,
        arr: f64,
        slew: f64,
    ) {
        let pos = graph.topo_rank(node);
        let (a, s) = self.entry(pos);
        let ti = tr.index();
        let ai = an.index();
        let idx = (tag as usize * 2 + ti) * 2 + ai;
        let s_idx = ti * 2 + ai;
        let arr = canon_zero(arr);
        let slew = canon_zero(slew.max(0.0));
        match an {
            An::Early => {
                a[idx] = canon_zero(a[idx].min(arr));
                s[s_idx] = canon_zero(s[s_idx].min(slew));
            }
            An::Late => {
                a[idx] = canon_zero(a[idx].max(arr));
                s[s_idx] = canon_zero(s[s_idx].max(slew));
            }
        }
    }

    /// Sets untagged `[early, late]` transition on `node` for transition `tr` while leaving its
    /// arrivals untouched (`STA-REQ-0115`).
    pub fn set_untagged_slew(&mut self, graph: &TimingGraph, node: NodeId, tr: Tr, slew: [f64; 2]) {
        let pos = graph.topo_rank(node);
        let (_, s) = self.entry(pos);
        let ti = tr.index();
        s[ti * 2] = canon_zero(slew[0].max(0.0));
        s[ti * 2 + 1] = canon_zero(slew[1].max(0.0));
    }

    /// Merges an untagged transition `slew` on `node` for `(tr, an)` (`STA-REQ-0115`).
    pub fn merge_untagged_slew(
        &mut self,
        graph: &TimingGraph,
        node: NodeId,
        tr: Tr,
        an: An,
        slew: f64,
    ) {
        let pos = graph.topo_rank(node);
        let (_, s) = self.entry(pos);
        let s_idx = tr.index() * 2 + an.index();
        let slew = canon_zero(slew.max(0.0));
        match an {
            An::Early => s[s_idx] = canon_zero(s[s_idx].min(slew)),
            An::Late => s[s_idx] = canon_zero(s[s_idx].max(slew)),
        }
    }

    /// Finalizes the [`StartRows`] sorted by level-ordered node index (`0..graph.num_nodes()`).
    #[must_use]
    pub fn build(self) -> StartRows {
        let mut out = StartRows::default();
        for (pos, (arr, slew)) in self.rows {
            out.push(pos, &arr, &slew);
        }
        out
    }
}
