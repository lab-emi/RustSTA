// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Fixed point of the constant propagation (owner: E7; `STA-REQ-0162`).

use std::collections::VecDeque;

use rusteda_core::liberty::{Cell, Direction, Func, Pin};

use super::{LogicVal, PinConst, PropEngine, eval_func, eval_func_with_origin, inst_ctx};
use crate::engine::graph::{ArcKind, TimingGraph};
use crate::engine::input::{NetTarget, NodeKind};
use crate::engine::types::{NetRow, NodeId};

impl PropEngine<'_> {
    /// Propagates the seeded constants to their fixed point (`STA-REQ-0162`) with a worklist: a
    /// net is evaluated again when one of its drivers changes, and an instance when one of its
    /// pins changes. A node changes at most twice (from `X` to a constant, and from a netlist to a
    /// case-analysis origin), so the work is linear in the pins of the netlist (AGENTS rule 3.4).
    pub(super) fn propagate_fixed_point(&mut self, graph: &TimingGraph) {
        let n_nodes = self.node_consts.len();
        // A load whose wire arc is disabled (`set_disable_timing` on a port) keeps its own state.
        let mut wire_disabled = vec![false; n_nodes];
        for (ai, a) in graph.arcs().iter().enumerate() {
            if matches!(a.kind, ArcKind::Net { .. })
                && graph.is_arc_disabled(ai)
                && let Some(slot) = wire_disabled.get_mut(a.dst.index())
            {
                *slot = true;
            }
        }
        // The canonical net that each driver node drives.
        let mut driven = vec![u32::MAX; n_nodes];
        for &net in self.netview.canonical_nets() {
            for &d in self.netview.drivers(net) {
                driven[d.index()] = net.0;
            }
        }

        let n_inst = self.netview.num_instances();
        let mut net_queue: VecDeque<NetRow> =
            self.netview.canonical_nets().iter().copied().collect();
        let mut net_queued = vec![false; self.net_consts.len()];
        for net in &net_queue {
            net_queued[net.index()] = true;
        }
        let mut inst_queue: VecDeque<u32> = (0..n_inst as u32).collect();
        let mut inst_queued = vec![true; n_inst];
        let mut changed_loads: Vec<NodeId> = Vec::new();
        let mut before: Vec<PinConst> = Vec::new();

        loop {
            if let Some(net) = net_queue.pop_front() {
                net_queued[net.index()] = false;
                changed_loads.clear();
                self.eval_net(net, &wire_disabled, &mut changed_loads);
                for &load in &changed_loads {
                    if let NodeKind::InstancePin { inst, .. } = self.netview.node_kind(load)
                        && !inst_queued[inst as usize]
                    {
                        inst_queued[inst as usize] = true;
                        inst_queue.push_back(inst);
                    }
                    // An `inout` load also drives its net.
                    let d = driven[load.index()];
                    if d != u32::MAX && !net_queued[d as usize] {
                        net_queued[d as usize] = true;
                        net_queue.push_back(NetRow(d));
                    }
                }
                continue;
            }
            let Some(inst) = inst_queue.pop_front() else {
                break;
            };
            inst_queued[inst as usize] = false;
            let n_port_bits = self.netview.num_port_bits();
            let pins = self.netview.frozen().instance_pins(inst);
            before.clear();
            before.extend(pins.clone().map(|p| self.node_consts[n_port_bits + p]));
            self.eval_instance(inst);
            for (p, old) in pins.zip(&before) {
                let node = n_port_bits + p;
                let d = driven[node];
                if self.node_consts[node] != *old && d != u32::MAX && !net_queued[d as usize] {
                    net_queued[d as usize] = true;
                    net_queue.push_back(NetRow(d));
                }
            }
        }

        for raw in 0..self.net_consts.len() {
            if let NetTarget::Net(canon) = self.netview.resolve_net(NetRow(raw as u32))
                && self.net_consts[canon.index()].is_const()
            {
                self.net_consts[raw] = self.net_consts[canon.index()];
            }
        }
    }

    /// Evaluates canonical net `net` from its drivers and pushes the loads whose state changed
    /// onto `changed`.
    fn eval_net(&mut self, net: NetRow, wire_disabled: &[bool], changed: &mut Vec<NodeId>) {
        let mut net_pc = PinConst::X;
        for &d in self.netview.drivers(net) {
            let d_pc = self.node_consts[d.index()];
            if !d_pc.is_const() {
                continue;
            }
            if !net_pc.is_const() {
                net_pc = d_pc;
            } else if net_pc.value != d_pc.value {
                net_pc = PinConst::X;
                break;
            } else if let (Some(o1), Some(o2)) = (net_pc.origin, d_pc.origin) {
                net_pc.origin = Some(o1.combine(o2));
            }
        }
        if !net_pc.is_const() {
            return;
        }
        self.net_consts[net.index()] = net_pc;
        for &load in self.netview.loads(net) {
            let i = load.index();
            if self.explicit_case_node[i] || self.node_consts[i] == net_pc || wire_disabled[i] {
                continue;
            }
            self.node_consts[i] = net_pc;
            changed.push(load);
        }
    }

    /// Evaluates the outputs of instance `inst_row` from the states of its pins: a clock gate
    /// output from its clock and enable inputs, any other output of a combinational cell from its
    /// `function`; the outputs of a sequential cell are never constant (`STA-REQ-0162`).
    fn eval_instance(&mut self, inst_row: u32) {
        let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, inst_row) else {
            return;
        };
        // The evaluation reads the pin states only through the instance's own pins (any other
        // name is `X`), so with every pin at `X` it is the all-`X` evaluation of the cell, which
        // changes nothing for an inert cell. Most instances of a design are in this state.
        if ictx.meta.x_inert
            && !ictx
                .frozen
                .instance_pins(inst_row)
                .any(|p| self.node_consts[ictx.n_port_bits + p].is_const())
        {
            return;
        }
        for p in ictx.frozen.instance_pins(inst_row) {
            let cp = ictx.frozen.pin_cellpin[p] as usize;
            let node_idx = ictx.n_port_bits + p;
            let Some(&Some(lp)) = ictx.meta.cp_to_pin.get(cp) else {
                continue;
            };
            if self.explicit_case_node[node_idx]
                || !matches!(lp.direction, Direction::Output | Direction::Inout)
                || lp.three_state_function.as_ref().is_some_and(|ts| {
                    eval_func(ts, &|n| ictx.pin(&self.node_consts, n).value) == LogicVal::One
                })
            {
                continue;
            }
            let Some(func) = output_func(lp, ictx.meta.sequential, ictx.meta.gate_func.as_ref())
            else {
                continue;
            };
            let out_pc = eval_func_with_origin(func, &|n| ictx.pin(&self.node_consts, n));
            if out_pc.is_const() && self.node_consts[node_idx] != out_pc {
                self.node_consts[node_idx] = out_pc;
            }
        }
    }
}

/// The function that gives the logic value of output `lp` (`STA-REQ-0162`): for a clock gate
/// output (`clock_gate_out_pin`) the conjunction `gate_func` of the cell's clock and enable inputs
/// (`clock_gate_clock_pin`, `clock_gate_enable_pin`; the test enable, the `function`, the
/// `state_function` and the `statetable` take no part), for any other output of a combinational
/// cell its `function`, and none for the outputs of a sequential cell, whose stored state is not
/// a constant even when an asynchronous `clear` or `preset` is held active.
fn output_func<'f>(lp: &'f Pin, sequential: bool, gate_func: Option<&'f Func>) -> Option<&'f Func> {
    if lp.clock_gate_out_pin {
        gate_func
    } else if sequential {
        None
    } else {
        lp.function.as_ref()
    }
}

/// The conjunction of the `clock_gate_clock_pin` and `clock_gate_enable_pin` inputs of `cell`, in
/// library pin order; `None` when the cell has neither (`STA-REQ-0162`).
pub(super) fn clock_gate_func(cell: &Cell) -> Option<Func> {
    cell.pins
        .iter()
        .filter(|p| {
            matches!(p.direction, Direction::Input | Direction::Inout)
                && (p.clock_gate_clock_pin || p.clock_gate_enable_pin)
        })
        .map(|p| Func::Var(p.name.clone()))
        .reduce(|a, b| Func::And(Box::new(a), Box::new(b)))
}

/// True when [`PropEngine::eval_instance`] changes no output of an instance whose pins are all
/// `X`: every output's function (see [`output_func`]) is `X` under the all-`X` environment. A
/// tautology such as `A | !A` is not inert, because the exhaustive evaluation makes it a constant.
pub(super) fn cell_is_x_inert(
    cp_to_pin: &[Option<&Pin>],
    sequential: bool,
    gate_func: Option<&Func>,
) -> bool {
    let all_x = |_: &str| PinConst::X;
    cp_to_pin.iter().flatten().all(|lp| {
        !matches!(lp.direction, Direction::Output | Direction::Inout)
            || output_func(lp, sequential, gate_func)
                .is_none_or(|f| !eval_func_with_origin(f, &all_x).is_const())
    })
}
