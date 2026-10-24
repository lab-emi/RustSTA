// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent per-pin transitions (`STA-REQ-0108`, `0109`, `0117`): the delay calculation
//! ignores logic values, so every arc that only constants, case analysis or `when` disable
//! carries its transition with its library sense, tie cells start at 0 and the clock-to-output
//! arcs of unclocked or constant-clocked sequential cells carry their clock pin's transition.

use std::collections::BTreeSet;

use super::super::canon_sdc::ClockDef;
use super::super::constants::{DisableReason, LogicVal};
use super::super::lib_model::{Analysis, LatchDef, Trans};
use super::TimingSolver;

impl TimingSolver<'_> {
    /// True when the latch `l_idx` of instance `inst_idx` carries its `D -> Q` transition: its
    /// enable is constant (transparent or opaque, `STA-REQ-0163`) or no clock reaches it; a
    /// clocked latch's `Q` transition comes from `G -> Q` only (`STA-REQ-0222`).
    fn latch_dq_carries_transition(&self, inst_idx: usize, latch: &LatchDef, l_idx: usize) -> bool {
        if self
            .consts
            .constant_enable_latches
            .contains_key(&(inst_idx, l_idx))
        {
            return true;
        }
        let inst = &self.design.instances[inst_idx];
        inst.pin_nodes
            .get(&latch.enable_pin)
            .is_none_or(|g| !self.clk_waves.contains_key(g))
    }

    /// True when combinational arc `(inst_idx, ca_idx)` carries its output transition: it is
    /// enabled, or only logic values (constants, case analysis, `when`) disable it; arcs that
    /// break a cycle or that `set_disable_timing` disables carry none (`STA-REQ-0117`).
    fn comb_arc_carries_transition(&self, inst_idx: usize, ca_idx: usize) -> bool {
        !self.cycle_disabled_arcs.contains(&(inst_idx, ca_idx))
            && self.consts.disabled_comb_arcs.get(&(inst_idx, ca_idx))
                != Some(&DisableReason::UserDisabled)
    }

    /// True when launch arc `(inst_idx, la_idx)` carries its output transition without launching
    /// an arrival: only logic values disable it, or no clock reaches its clock pin
    /// (`STA-REQ-0117`).
    fn launch_carries_transition(&self, inst_idx: usize, la_idx: usize, ck_node: usize) -> bool {
        match self.consts.disabled_launch_arcs.get(&(inst_idx, la_idx)) {
            Some(DisableReason::UserDisabled) => false,
            Some(_) => true,
            None => !self.clk_waves.contains_key(&ck_node),
        }
    }

    /// The directed `(from, to)` node pairs of every arc that carries a transition, and for each
    /// its kind: net arc, combinational arc `(inst, arc)` or launch arc `(inst, arc)`.
    fn transition_edges(&self) -> Vec<(usize, usize, TransitionArc)> {
        let mut edges = Vec::new();
        for net in &self.design.nets {
            for &drv in &net.drivers {
                for &snk in &net.sinks {
                    if snk != drv {
                        edges.push((drv, snk, TransitionArc::Net));
                    }
                }
            }
        }
        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                continue;
            };
            for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                if !self.comb_arc_carries_transition(inst_idx, ca_idx) {
                    continue;
                }
                if let Some((l_idx, latch)) = cdef
                    .latches
                    .iter()
                    .enumerate()
                    .find(|(_, l)| l.data_pin == ca.from_pin && l.q_pin == ca.to_pin)
                    && !self.latch_dq_carries_transition(inst_idx, latch, l_idx)
                {
                    continue;
                }
                if let (Some(&u), Some(&v)) = (
                    inst.pin_nodes.get(&ca.from_pin),
                    inst.pin_nodes.get(&ca.to_pin),
                ) {
                    edges.push((u, v, TransitionArc::Comb(inst_idx, ca_idx)));
                }
            }
            for (la_idx, la) in cdef.launch_arcs.iter().enumerate() {
                if let (Some(&ck), Some(&q)) = (
                    inst.pin_nodes.get(&la.clock_pin),
                    inst.pin_nodes.get(&la.to_pin),
                ) && self.launch_carries_transition(inst_idx, la_idx, ck)
                {
                    edges.push((ck, q, TransitionArc::Launch(inst_idx, la_idx)));
                }
            }
        }
        edges
    }

    /// Computes per-pin slews `slews[node][tr][an]` (`STA-REQ-0108`, `0109`, `0117`, `0218`,
    /// `0222`): the delay calculation ignores logic values, so every arc that carries a
    /// transition (an enabled arc, or one that constants, case analysis or `when` disable)
    /// merges its output transition with its library sense, evaluated at its source pin's
    /// transition; tie cells start at 0 and every input port at its input transition.
    pub(super) fn propagate_slews(&mut self) {
        // 1. Seed primary input ports (constant ones too: case analysis keeps the transition).
        for &port_node in &self.design.input_ports {
            let port_name = &self.design.nodes[port_node].name;
            // If this port is a clock source port, its slew comes from `set_clock_transition`.
            let clk_on_port: Vec<&ClockDef> = self
                .sdc
                .clocks
                .values()
                .filter(|c| c.sources.contains(port_name))
                .collect();
            if !clk_on_port.is_empty() {
                for tr in Trans::ALL {
                    for an in Analysis::ALL {
                        let s = clk_on_port.iter().map(|c| c.transition(tr, an)).fold(
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
                        );
                        self.merge_slew(port_node, tr, an, s);
                    }
                }
                continue;
            }

            // Otherwise, check `set_driving_cell` (STA-REQ-0218) or `set_input_transition`.
            for tr in Trans::ALL {
                for an in Analysis::ALL {
                    let s = if let Some((_, drv_slew)) =
                        self.driving_cell_delay_and_slew(port_node, port_name, tr, an)
                    {
                        drv_slew
                    } else {
                        self.sdc.input_transition(port_name, tr, an)
                    };
                    self.merge_slew(port_node, tr, an, s);
                }
            }
        }
        // Tie cells: constant drivers without timing arcs switch with transition 0.
        for &t in &self.consts.tie_outputs {
            for tr in Trans::ALL {
                for an in Analysis::ALL {
                    self.merge_slew(t, tr, an, 0.0);
                }
            }
        }

        // 2. Seed sequential launch output pins (`CK -> Q` and latch `G -> Q`, STA-REQ-0222).
        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                continue;
            };
            for (la_idx, la) in cdef.launch_arcs.iter().enumerate() {
                if self
                    .consts
                    .disabled_launch_arcs
                    .contains_key(&(inst_idx, la_idx))
                {
                    continue;
                }
                let (Some(&ck_node), Some(&q_node)) = (
                    inst.pin_nodes.get(&la.clock_pin),
                    inst.pin_nodes.get(&la.to_pin),
                ) else {
                    continue;
                };
                if self.consts.pin_vals[ck_node] != LogicVal::X
                    || self.consts.pin_vals[q_node] != LogicVal::X
                {
                    continue;
                }
                let Some(waves) = self.clk_waves.get(&ck_node).cloned() else {
                    continue;
                };
                for w in &waves {
                    if w.pin_edge != la.clock_edge {
                        continue;
                    }
                    let Some(clk_def) = self.sdc.clocks.get(&w.clock) else {
                        continue;
                    };
                    for out_tr in Trans::ALL {
                        for an in Analysis::ALL {
                            let clk_slew = clk_def.transition(w.pin_edge, an);
                            let c_load = self.node_load_pf(q_node, out_tr, an);
                            if let Some((_, out_slew)) = la.eval(out_tr, clk_slew, c_load) {
                                self.merge_slew(q_node, out_tr, an, out_slew);
                            }
                        }
                    }
                }
            }
        }

        // 3. Propagate in topological order over the arcs that carry a transition. Arcs that
        // logic values disable but that would close a loop are left out (launches first).
        let n = self.design.nodes.len();
        let edges = self.transition_edges();
        let order = transition_order(n, &edges, true)
            .or_else(|| transition_order(n, &edges, false))
            .unwrap_or_else(|| self.topo_order.clone());
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (k, &(u, _, _)) in edges.iter().enumerate() {
            out[u].push(k);
        }
        for u in order {
            // Clock network pins have ideal clock slew from their clock definitions (STA-REQ-0200).
            if self.consts.pin_vals[u] == LogicVal::X
                && let Some(waves) = self.clk_waves.get(&u).cloned()
            {
                for w in &waves {
                    if let Some(clk_def) = self.sdc.clocks.get(&w.clock) {
                        for an in Analysis::ALL {
                            let s = clk_def.transition(w.pin_edge, an);
                            self.merge_slew(u, w.pin_edge, an, s);
                        }
                    }
                }
            }
            let u_slews = self.slews[u];
            for &k in &out[u] {
                let (_, v, kind) = edges[k];
                match kind {
                    TransitionArc::Net => {
                        for tr in Trans::ALL {
                            for an in Analysis::ALL {
                                if let Some(s) = u_slews[tr.idx()][an.idx()] {
                                    self.merge_slew(v, tr, an, s);
                                }
                            }
                        }
                    }
                    TransitionArc::Comb(inst_idx, ca_idx) => {
                        let inst = &self.design.instances[inst_idx];
                        let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                            continue;
                        };
                        let ca = &cdef.comb_arcs[ca_idx];
                        for in_tr in Trans::ALL {
                            for out_tr in Trans::ALL {
                                if !ca.can_propagate(&ca.sense, in_tr, out_tr) {
                                    continue;
                                }
                                for an in Analysis::ALL {
                                    let Some(s_in) = u_slews[in_tr.idx()][an.idx()] else {
                                        continue;
                                    };
                                    let c_load = self.node_load_pf(v, out_tr, an);
                                    if let Some((_, out_slew)) = ca.eval(out_tr, s_in, c_load) {
                                        self.merge_slew(v, out_tr, an, out_slew);
                                    }
                                }
                            }
                        }
                    }
                    TransitionArc::Launch(inst_idx, la_idx) => {
                        let inst = &self.design.instances[inst_idx];
                        let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                            continue;
                        };
                        let la = &cdef.launch_arcs[la_idx];
                        for out_tr in Trans::ALL {
                            for an in Analysis::ALL {
                                let Some(s_in) = u_slews[la.clock_edge.idx()][an.idx()] else {
                                    continue;
                                };
                                let c_load = self.node_load_pf(v, out_tr, an);
                                if let Some((_, out_slew)) = la.eval(out_tr, s_in, c_load) {
                                    self.merge_slew(v, out_tr, an, out_slew);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// An arc that carries a transition (`STA-REQ-0117`).
#[derive(Debug, Clone, Copy)]
enum TransitionArc {
    Net,
    Comb(usize, usize),
    Launch(usize, usize),
}

/// A topological order of `0..n` over `edges` (with the launch arcs when `with_launches`), the
/// smallest node index first among the ready nodes, or `None` when the edges close a loop.
fn transition_order(
    n: usize,
    edges: &[(usize, usize, TransitionArc)],
    with_launches: bool,
) -> Option<Vec<usize>> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(u, v, kind) in edges {
        if with_launches || !matches!(kind, TransitionArc::Launch(..)) {
            adj[u].push(v);
        }
    }
    let mut indeg = vec![0usize; n];
    for vs in &adj {
        for &v in vs {
            indeg[v] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(u) = ready.pop_first() {
        order.push(u);
        for &v in &adj[u] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                ready.insert(v);
            }
        }
    }
    (order.len() == n).then_some(order)
}
