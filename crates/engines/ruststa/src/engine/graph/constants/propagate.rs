// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Fixed point of the constant propagation (owner: E7; `STA-REQ-0151`, `STA-REQ-0161`).

use std::collections::VecDeque;

use rusteda_core::liberty::{Cell, Direction, Func, Pin, Sequential};

use super::{
    ConstOrigin, InstCtx, LogicVal, PinConst, PropEngine, eval_func, eval_func_with_origin,
    inst_ctx,
};
use crate::engine::graph::{ArcKind, TimingGraph};
use crate::engine::input::{NetTarget, NodeKind};
use crate::engine::types::{NetRow, NodeId};

impl PropEngine<'_> {
    /// Propagates the seeded constants to their fixed point (`STA-REQ-0151`) with a worklist: a
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

    /// Evaluates the outputs of instance `inst_row` from the states of its pins.
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
        if !ictx.meta.cell.sequentials.is_empty() || !ictx.meta.async_arcs.is_empty() {
            eval_seq_async_consts(ictx, &self.explicit_case_node, &mut self.node_consts);
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
            if let Some(ref func) = lp.function {
                let out_pc = eval_func_with_origin(func, &|n| ictx.pin(&self.node_consts, n));
                if out_pc.is_const() && self.node_consts[node_idx] != out_pc {
                    self.node_consts[node_idx] = out_pc;
                }
            }
        }
    }
}

/// True when [`PropEngine::eval_instance`] changes no output of an instance of `cell` whose pins
/// are all `X`: every output function, and for a sequential cell every `clear`/`preset` state, is
/// `X` under the all-`X` environment (asynchronous arcs need a constant input). A tautology such as
/// `A | !A` is not inert, because the exhaustive evaluation makes it a constant.
pub(super) fn cell_is_x_inert(cell: &Cell, cp_to_pin: &[Option<&Pin>], has_async: bool) -> bool {
    let all_x = |_: &str| PinConst::X;
    if !cell.sequentials.is_empty() || has_async {
        return cell.sequentials.iter().all(|seq| {
            let (state_pc, state_n_pc) = eval_seq_state(seq, &all_x);
            !state_pc.is_const() && !state_n_pc.is_const()
        });
    }
    cp_to_pin.iter().flatten().all(|lp| {
        !matches!(lp.direction, Direction::Output | Direction::Inout)
            || lp
                .function
                .as_ref()
                .is_none_or(|f| !eval_func_with_origin(f, &all_x).is_const())
    })
}

fn eval_seq_async_consts(
    ictx: InstCtx<'_>,
    explicit_case_node: &[bool],
    node_consts: &mut [PinConst],
) -> bool {
    let mut changed = false;
    for seq in &ictx.meta.cell.sequentials {
        let (state_pc, state_n_pc) = eval_seq_state(seq, &|n| ictx.pin(node_consts, n));
        if !state_pc.is_const() && !state_n_pc.is_const() {
            continue;
        }
        for p in ictx.frozen.instance_pins(ictx.inst) {
            let cp = ictx.frozen.pin_cellpin[p] as usize;
            let node_idx = ictx.n_port_bits + p;
            let Some(&Some(lp)) = ictx.meta.cp_to_pin.get(cp) else {
                continue;
            };
            if explicit_case_node[node_idx]
                || !matches!(lp.direction, Direction::Output | Direction::Inout)
            {
                continue;
            }
            let out_pc = lp.function.as_ref().map_or(state_pc, |func| {
                eval_func_with_origin(func, &|name| {
                    if !seq.state.is_empty() && name == seq.state {
                        state_pc
                    } else if !seq.state_n.is_empty() && name == seq.state_n {
                        state_n_pc
                    } else {
                        ictx.pin(node_consts, name)
                    }
                })
            });
            if out_pc.is_const() && node_consts[node_idx] != out_pc {
                node_consts[node_idx] = out_pc;
                changed = true;
            }
        }
    }

    for &(from_cp, to_cp, is_preset, active_high) in &ictx.meta.async_arcs {
        let (Some(from_idx), Some(out_idx)) = (ictx.cp_node(from_cp), ictx.cp_node(to_cp)) else {
            continue;
        };
        let from_pc = node_consts[from_idx];
        if let (Some(b), Some(origin)) = (from_pc.value.as_bool(), from_pc.origin)
            && b == active_high
            && !explicit_case_node[out_idx]
        {
            let pc = PinConst::new(LogicVal::from_bool(is_preset), origin);
            if node_consts[out_idx] != pc {
                node_consts[out_idx] = pc;
                changed = true;
            }
        }
    }
    changed
}

fn eval_seq_state(seq: &Sequential, env: &impl Fn(&str) -> PinConst) -> (PinConst, PinConst) {
    let ev = |f: Option<&Func>| f.map_or(PinConst::X, |fn_| eval_func_with_origin(fn_, env));
    let (c, p) = (ev(seq.clear.as_ref()), ev(seq.preset.as_ref()));
    if c.value == LogicVal::One {
        let o = c.origin.unwrap_or(ConstOrigin::Netlist);
        let h = p.value == LogicVal::One && seq.clear_preset_var1.as_deref() == Some("H");
        let v = LogicVal::from_bool(h);
        (PinConst::new(v, o), PinConst::new(v.not(), o))
    } else if p.value == LogicVal::One {
        let o = p.origin.unwrap_or(ConstOrigin::Netlist);
        (
            PinConst::new(LogicVal::One, o),
            PinConst::new(LogicVal::Zero, o),
        )
    } else {
        (PinConst::X, PinConst::X)
    }
}
