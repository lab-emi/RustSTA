// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Backward required-time propagation, clock-gating topological detection, and exhaustive path
//! enumeration (`STA-REQ-0215`, `0216`, `0225`, `0227`, `0228`, `0502`).

use std::collections::BTreeMap;

use rusteda_core::liberty::{Direction, Func};

use super::Unsupported;
use super::checks_eval::{
    CheckKindEval, ClockGateCheckInfo, EndpointEval, EndpointStatus, PathEval, clock_relations_ns,
    gating_trailing_relation_ns, pulse_width_ns,
};
use super::constants::LogicVal;
use super::eval::{Tag, TimingSolver};
use super::lib_model::{Analysis, CheckArcKind, Trans};
use super::verilog::NodeKind;

impl TimingSolver<'_> {
    /// Computes the set of nodes from which a sequential clock pin (`is_clock` or latch enable) is
    /// reachable through active combinational and net arcs.
    pub(crate) fn nodes_reaching_clock_pins(&self) -> Vec<bool> {
        let n = self.design.nodes.len();
        let mut reaches = vec![false; n];
        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                continue;
            };
            for (pname, pdef) in &cdef.pins {
                if pdef.is_clock || cdef.latches.iter().any(|l| l.enable_pin == *pname) {
                    if let Some(&ni) = inst.pin_nodes.get(pname) {
                        reaches[ni] = true;
                    }
                }
            }
            for chk in &cdef.check_arcs {
                if let Some(&ni) = inst.pin_nodes.get(&chk.clock_pin) {
                    reaches[ni] = true;
                }
            }
            let _ = inst_idx;
        }
        for &u in self.topo_order.iter().rev() {
            if let Some(net_idx) = self.design.nodes[u].net_idx {
                let net = &self.design.nets[net_idx];
                if net.drivers.contains(&u) && net.sinks.iter().any(|&snk| reaches[snk]) {
                    reaches[u] = true;
                }
            }
            if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind {
                let inst = &self.design.instances[inst_idx];
                if let Some(cdef) = self.lib.cells.get(&inst.cell_type) {
                    for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                        if ca.from_pin == *pin
                            && self.is_comb_arc_enabled(inst_idx, ca_idx)
                            && !cdef.is_latch_dq(&ca.from_pin, &ca.to_pin)
                            && let Some(&v) = inst.pin_nodes.get(&ca.to_pin)
                            && reaches[v]
                        {
                            reaches[u] = true;
                        }
                    }
                }
            }
        }
        reaches
    }

    /// Detects combinational or integrated clock-gating enable pins on `inst_idx`
    /// (`STA-REQ-0216`, `0225`).
    #[must_use]
    pub(crate) fn detect_clock_gates_on_inst(
        &self,
        inst_idx: usize,
        clk_reaching: &[bool],
    ) -> Vec<ClockGateCheckInfo> {
        let inst = &self.design.instances[inst_idx];
        let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
            return Vec::new();
        };
        if !cdef.launch_arcs.is_empty() || !cdef.check_arcs.is_empty() || !cdef.latches.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        for (out_pin_name, out_pdef) in &cdef.pins {
            if out_pdef.direction != Direction::Output {
                continue;
            }
            let Some(&out_node) = inst.pin_nodes.get(out_pin_name) else {
                continue;
            };
            if self.consts.pin_vals[out_node] != LogicVal::X
                || (!clk_reaching[out_node] && !cdef.is_clock_gating)
            {
                continue;
            }

            // Identify input pins of `inst` that feed `out_pin_name`.
            let mut clk_inputs = Vec::new();
            let mut en_inputs = Vec::new();
            for (in_name, in_pdef) in &cdef.pins {
                if !matches!(in_pdef.direction, Direction::Input | Direction::Inout) {
                    continue;
                }
                let Some(&in_node) = inst.pin_nodes.get(in_name) else {
                    continue;
                };
                if self.consts.pin_vals[in_node] != LogicVal::X {
                    continue;
                }
                let has_enabled_arc = cdef.comb_arcs.iter().enumerate().any(|(idx, ca)| {
                    ca.from_pin == *in_name
                        && ca.to_pin == *out_pin_name
                        && self.is_comb_arc_enabled(inst_idx, idx)
                });
                if !has_enabled_arc {
                    continue;
                }
                if self.clk_waves.get(&in_node).is_some_and(|w| !w.is_empty()) {
                    clk_inputs.push((in_name.clone(), in_node));
                } else {
                    en_inputs.push((in_name.clone(), in_node));
                }
            }

            for (ck_name, ck_node) in &clk_inputs {
                let active_low = if let Some(f) = &out_pdef.function {
                    let const_at = |ck_val: bool| -> bool {
                        is_func_const_when(f, ck_name, ck_val, inst, &self.consts.pin_vals)
                    };
                    if const_at(false) {
                        Some(false)
                    } else if const_at(true) {
                        Some(true)
                    } else {
                        None
                    }
                } else if cdef.is_clock_gating {
                    Some(false)
                } else {
                    None
                };

                if let Some(act_low) = active_low {
                    for (_, en_node) in &en_inputs {
                        if !out
                            .iter()
                            .any(|cg: &ClockGateCheckInfo| cg.en_node == *en_node)
                        {
                            out.push(ClockGateCheckInfo {
                                en_node: *en_node,
                                ck_node: *ck_node,
                                active_low: act_low,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Propagates required times backwards in reverse topological order and computes per-node
    /// slacks (`STA-REQ-0227`).
    pub(crate) fn propagate_required_times(&mut self) {
        let n = self.design.nodes.len();
        let mut req_late: Vec<[BTreeMap<Tag, f64>; 2]> =
            (0..n).map(|_| [BTreeMap::new(), BTreeMap::new()]).collect();
        let mut req_early: Vec<[BTreeMap<Tag, f64>; 2]> =
            (0..n).map(|_| [BTreeMap::new(), BTreeMap::new()]).collect();

        // Seed at constrained endpoints.
        for ep in &self.endpoints {
            if ep.status != EndpointStatus::Constrained {
                continue;
            }
            for ((tag, tr), &slack) in &ep.tag_setup_slack {
                if let Some(arr) = self.arrivals[ep.node_idx][tr.idx()][Analysis::Late.idx()]
                    .iter()
                    .find(|e| e.tag == *tag)
                {
                    let req = arr.arrival_ns + slack;
                    req_late[ep.node_idx][tr.idx()]
                        .entry(tag.clone())
                        .and_modify(|v| *v = v.min(req))
                        .or_insert(req);
                }
            }
            for ((tag, tr), &slack) in &ep.tag_hold_slack {
                if let Some(arr) = self.arrivals[ep.node_idx][tr.idx()][Analysis::Early.idx()]
                    .iter()
                    .find(|e| e.tag == *tag)
                {
                    let req = arr.arrival_ns - slack;
                    req_early[ep.node_idx][tr.idx()]
                        .entry(tag.clone())
                        .and_modify(|v| *v = v.max(req))
                        .or_insert(req);
                }
            }
        }

        // Propagate backwards in reverse topological order.
        let topo = self.topo_order.clone();
        for &u in topo.iter().rev() {
            if self.consts.pin_vals[u] != LogicVal::X {
                continue;
            }
            // Net driver <- sink pins.
            if let Some(net_idx) = self.design.nodes[u].net_idx {
                let net = &self.design.nets[net_idx];
                if net.drivers.contains(&u) {
                    for &snk in &net.sinks {
                        for tr in Trans::ALL {
                            let snk_late = req_late[snk][tr.idx()].clone();
                            for (tag, rq) in snk_late {
                                req_late[u][tr.idx()]
                                    .entry(tag)
                                    .and_modify(|v| *v = v.min(rq))
                                    .or_insert(rq);
                            }
                            let snk_early = req_early[snk][tr.idx()].clone();
                            for (tag, rq) in snk_early {
                                req_early[u][tr.idx()]
                                    .entry(tag)
                                    .and_modify(|v| *v = v.max(rq))
                                    .or_insert(rq);
                            }
                        }
                    }
                }
            }

            // Instance input pin <- output pin across enabled combinational arcs.
            if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind {
                let inst = &self.design.instances[inst_idx];
                let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                    continue;
                };
                for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                    if ca.from_pin != *pin || !self.is_comb_arc_enabled(inst_idx, ca_idx) {
                        continue;
                    }
                    let Some(&v) = inst.pin_nodes.get(&ca.to_pin) else {
                        continue;
                    };
                    if cdef.is_latch_dq(&ca.from_pin, &ca.to_pin) {
                        // Propagate across latch D -> Q for borrowed arrivals (STA-REQ-0227).
                        for out_tr in Trans::ALL {
                            let v_late = req_late[v][out_tr.idx()].clone();
                            for entry in &self.arrivals[v][out_tr.idx()][Analysis::Late.idx()] {
                                if let super::eval::ArrivalOrigin::LatchBorrow {
                                    d_pin,
                                    ref src_tag,
                                    src_tr,
                                    borrowed_entry_ns,
                                    dq_delay_ns,
                                    ..
                                } = entry.origin
                                    && d_pin == u
                                    && let Some(&rq_v) = v_late.get(&entry.tag)
                                    && let Some(d_arr) = self.arrivals[u][src_tr.idx()]
                                        [Analysis::Late.idx()]
                                    .iter()
                                    .find(|e| e.tag == *src_tag)
                                {
                                    let d_open = d_arr.arrival_ns - borrowed_entry_ns;
                                    let rq_u = rq_v - dq_delay_ns + d_open;
                                    req_late[u][src_tr.idx()]
                                        .entry(src_tag.clone())
                                        .and_modify(|val| *val = val.min(rq_u))
                                        .or_insert(rq_u);
                                }
                            }
                        }
                        continue;
                    }

                    let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
                    for in_tr in Trans::ALL {
                        for out_tr in Trans::ALL {
                            if !ca.can_propagate(&sense, in_tr, out_tr) {
                                continue;
                            }
                            if let Some(s_in) = self.slews[u][in_tr.idx()][Analysis::Late.idx()] {
                                let c_load = self.node_load_pf(v, out_tr, Analysis::Late);
                                if let Some((d_arc, _)) = ca.eval(out_tr, s_in, c_load) {
                                    let v_late = req_late[v][out_tr.idx()].clone();
                                    for (tag, rq) in v_late {
                                        req_late[u][in_tr.idx()]
                                            .entry(tag)
                                            .and_modify(|val| *val = val.min(rq - d_arc))
                                            .or_insert(rq - d_arc);
                                    }
                                }
                            }
                            if let Some(s_in) = self.slews[u][in_tr.idx()][Analysis::Early.idx()] {
                                let c_load = self.node_load_pf(v, out_tr, Analysis::Early);
                                if let Some((d_arc, _)) = ca.eval(out_tr, s_in, c_load) {
                                    let v_early = req_early[v][out_tr.idx()].clone();
                                    for (tag, rq) in v_early {
                                        req_early[u][in_tr.idx()]
                                            .entry(tag)
                                            .and_modify(|val| *val = val.max(rq - d_arc))
                                            .or_insert(rq - d_arc);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Record worst-slack required time and slack per `(node, tr, an)`.
        for u in 0..n {
            for tr in Trans::ALL {
                let mut best_late: Option<(f64, f64)> = None;
                for entry in &self.arrivals[u][tr.idx()][Analysis::Late.idx()] {
                    if let Some(&rq) = req_late[u][tr.idx()].get(&entry.tag) {
                        let slk = rq - entry.arrival_ns;
                        if best_late.is_none_or(|(_, prev_slk)| slk < prev_slk) {
                            best_late = Some((rq, slk));
                        }
                    }
                }
                if let Some((rq, slk)) = best_late {
                    self.node_req_ns[u][tr.idx()][Analysis::Late.idx()] = Some(rq);
                    self.node_slack_ns[u][tr.idx()][Analysis::Late.idx()] = Some(slk);
                }

                let mut best_early: Option<(f64, f64)> = None;
                for entry in &self.arrivals[u][tr.idx()][Analysis::Early.idx()] {
                    if let Some(&rq) = req_early[u][tr.idx()].get(&entry.tag) {
                        let slk = entry.arrival_ns - rq;
                        if best_early.is_none_or(|(_, prev_slk)| slk < prev_slk) {
                            best_early = Some((rq, slk));
                        }
                    }
                }
                if let Some((rq, slk)) = best_early {
                    self.node_req_ns[u][tr.idx()][Analysis::Early.idx()] = Some(rq);
                    self.node_slack_ns[u][tr.idx()][Analysis::Early.idx()] = Some(slk);
                }
            }
        }
    }

    /// Exhaustively enumerates all simple startpoint-to-endpoint paths for designs with `<= 5000`
    /// pins (`STA-REQ-0502`, `0228`).
    pub(crate) fn enumerate_paths(&mut self) -> Result<(), Unsupported> {
        let mut paths = Vec::new();
        let clk_reaching = self.nodes_reaching_clock_pins();

        // Map `node_idx -> Vec<usize>` of indices into `self.endpoints`.
        let mut ep_by_node: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (idx, ep) in self.endpoints.iter().enumerate() {
            if ep.status == EndpointStatus::Constrained {
                ep_by_node.entry(ep.node_idx).or_default().push(idx);
            }
        }
        let _ = clk_reaching;

        // 1. Primary input ports with `set_input_delay`.
        for &port_node in &self.design.input_ports {
            if self.consts.pin_vals[port_node] != LogicVal::X {
                continue;
            }
            let port_name = self.design.nodes[port_node].name.clone();
            for id in self.sdc.input_delays_for_port(&port_name) {
                let Some(clk_def) = self.sdc.clocks.get(&id.clock) else {
                    continue;
                };
                let tag = Tag {
                    clock: id.clock.clone(),
                    edge: id.clock_edge,
                };
                for tr in Trans::ALL {
                    let arr_late = id.bound(tr, Analysis::Late).map(|v| {
                        let l_src = if id.source_latency_included {
                            0.0
                        } else {
                            clk_def.source_latency(id.clock_edge, Analysis::Late)
                        };
                        let l_net = if id.network_latency_included {
                            0.0
                        } else {
                            clk_def.network_latency(id.clock_edge, Analysis::Late)
                        };
                        let drv = self
                            .driving_cell_delay_and_slew(port_node, &port_name, tr, Analysis::Late)
                            .map_or(0.0, |(d, _)| d);
                        v + l_src + l_net + drv
                    });
                    let arr_early = id.bound(tr, Analysis::Early).map(|v| {
                        let l_src = if id.source_latency_included {
                            0.0
                        } else {
                            clk_def.source_latency(id.clock_edge, Analysis::Early)
                        };
                        let l_net = if id.network_latency_included {
                            0.0
                        } else {
                            clk_def.network_latency(id.clock_edge, Analysis::Early)
                        };
                        let drv = self
                            .driving_cell_delay_and_slew(port_node, &port_name, tr, Analysis::Early)
                            .map_or(0.0, |(d, _)| d);
                        v + l_src + l_net + drv
                    });
                    if arr_late.is_none() && arr_early.is_none() {
                        continue;
                    }
                    let mut trace = vec![(port_name.clone(), tr)];
                    let mut visited = vec![port_node];
                    self.enumerate_from_seed(
                        port_node,
                        tr,
                        &port_name,
                        tr,
                        &tag,
                        arr_late,
                        arr_early,
                        &mut trace,
                        &mut visited,
                        &ep_by_node,
                        &mut paths,
                    )?;
                }
            }
        }

        // 2. Sequential `CK -> Q` and latch `G -> Q` launch arcs (startpoint is `CK` / `G` pin,
        // STA-REQ-0228).
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
                let Some(waves) = self.clk_waves.get(&ck_node) else {
                    continue;
                };
                let ck_name = self.design.nodes[ck_node].name.clone();
                let q_name = self.design.nodes[q_node].name.clone();
                for w in waves {
                    if w.pin_edge != la.clock_edge {
                        continue;
                    }
                    let Some(clk_def) = self.sdc.clocks.get(&w.clock) else {
                        continue;
                    };
                    let tag = Tag {
                        clock: w.clock.clone(),
                        edge: w.source_edge,
                    };
                    for out_tr in Trans::ALL {
                        let eval_la = |an: Analysis| {
                            let (d, _) = la.eval(
                                out_tr,
                                clk_def.transition(w.pin_edge, an),
                                self.node_load_pf(q_node, out_tr, an),
                            )?;
                            Some(clk_def.total_latency(w.source_edge, an) + d)
                        };
                        let (arr_late, arr_early) =
                            (eval_la(Analysis::Late), eval_la(Analysis::Early));
                        if arr_late.is_none() && arr_early.is_none() {
                            continue;
                        }
                        let mut trace =
                            vec![(ck_name.clone(), w.pin_edge), (q_name.clone(), out_tr)];
                        let mut visited = vec![q_node];
                        self.enumerate_from_seed(
                            q_node,
                            out_tr,
                            &ck_name,
                            w.pin_edge,
                            &tag,
                            arr_late,
                            arr_early,
                            &mut trace,
                            &mut visited,
                            &ep_by_node,
                            &mut paths,
                        )?;
                    }
                }
            }

            // 3. Latch `D -> Q` borrowed re-launches (startpoint is `l/G`, STA-REQ-0222, 0267).
            for latch in &cdef.latches {
                let (Some(&d_node), Some(&g_node), Some(&q_node)) = (
                    inst.pin_nodes.get(&latch.data_pin),
                    inst.pin_nodes.get(&latch.enable_pin),
                    inst.pin_nodes.get(&latch.q_pin),
                ) else {
                    continue;
                };
                let g_name = self.design.nodes[g_node].name.clone();
                let d_name = self.design.nodes[d_node].name.clone();
                let q_name = self.design.nodes[q_node].name.clone();
                for out_tr in Trans::ALL {
                    for entry in &self.arrivals[q_node][out_tr.idx()][Analysis::Late.idx()] {
                        if let super::eval::ArrivalOrigin::LatchBorrow { src_tr, .. } = entry.origin
                        {
                            let mut trace = vec![
                                (g_name.clone(), latch.open_edge()),
                                (d_name.clone(), src_tr),
                                (q_name.clone(), out_tr),
                            ];
                            let mut visited = vec![q_node];
                            self.enumerate_from_seed(
                                q_node,
                                out_tr,
                                &g_name,
                                latch.open_edge(),
                                &entry.tag,
                                Some(entry.arrival_ns),
                                None,
                                &mut trace,
                                &mut visited,
                                &ep_by_node,
                                &mut paths,
                            )?;
                        }
                    }
                }
            }
        }

        // Rank setup-first (`STA-REQ-0228`): late paths by increasing setup slack before early
        // paths, with deterministic tie-breaking by `(endpoint, startpoint, end_tr)`.
        paths.sort_by(|a, b| {
            let a_early = a.setup_slack_ns.is_none();
            let b_early = b.setup_slack_ns.is_none();
            let a_key = a
                .setup_slack_ns
                .or(a.hold_slack_ns)
                .unwrap_or(f64::INFINITY);
            let b_key = b
                .setup_slack_ns
                .or(b.hold_slack_ns)
                .unwrap_or(f64::INFINITY);
            a_early
                .cmp(&b_early)
                .then_with(|| a_key.total_cmp(&b_key))
                .then_with(|| a.endpoint.cmp(&b.endpoint))
                .then_with(|| a.startpoint.cmp(&b.startpoint))
                .then_with(|| a.end_tr.cmp(&b.end_tr))
        });

        self.paths = paths;
        Ok(())
    }

    const MAX_DFS_VISITS_PER_SEED: usize = 512;
    const MAX_TOTAL_ENUM_PATHS: usize = 2048;

    #[allow(clippy::too_many_arguments)]
    fn enumerate_from_seed(
        &self,
        u: usize,
        u_tr: Trans,
        startpoint: &str,
        start_tr: Trans,
        tag: &Tag,
        arr_late: Option<f64>,
        arr_early: Option<f64>,
        trace: &mut Vec<(String, Trans)>,
        visited: &mut Vec<usize>,
        ep_by_node: &BTreeMap<usize, Vec<usize>>,
        out_paths: &mut Vec<PathEval>,
    ) -> Result<(), Unsupported> {
        if self.design.nodes.len() <= 64 && out_paths.len() < Self::MAX_TOTAL_ENUM_PATHS {
            let saved_len = out_paths.len();
            let mut visits = 0usize;
            self.dfs_enumerate_from(
                u,
                u_tr,
                startpoint,
                start_tr,
                tag,
                arr_late,
                arr_early,
                trace,
                visited,
                ep_by_node,
                &mut visits,
                out_paths,
            )?;
            if visits <= Self::MAX_DFS_VISITS_PER_SEED
                && out_paths.len() < Self::MAX_TOTAL_ENUM_PATHS
            {
                return Ok(());
            }
            out_paths.truncate(saved_len);
        }
        self.dp_enumerate_from_seed(
            u, u_tr, startpoint, start_tr, tag, arr_late, arr_early, trace, ep_by_node, out_paths,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dp_enumerate_from_seed(
        &self,
        u: usize,
        u_tr: Trans,
        startpoint: &str,
        start_tr: Trans,
        tag: &Tag,
        arr_late: Option<f64>,
        arr_early: Option<f64>,
        seed_trace: &[(String, Trans)],
        ep_by_node: &BTreeMap<usize, Vec<usize>>,
        out_paths: &mut Vec<PathEval>,
    ) -> Result<(), Unsupported> {
        let n = self.design.nodes.len();
        let mut best_late: Vec<[Option<(f64, Option<(usize, Trans)>)>; 2]> = vec![[None, None]; n];
        let mut best_early: Vec<[Option<(f64, Option<(usize, Trans)>)>; 2]> = vec![[None, None]; n];
        best_late[u][u_tr.idx()] = arr_late.map(|a| (a, None));
        best_early[u][u_tr.idx()] = arr_early.map(|a| (a, None));

        for &node in &self.topo_order {
            for ntr in Trans::ALL {
                let cur_l = best_late[node][ntr.idx()].map(|x| x.0);
                let cur_e = best_early[node][ntr.idx()].map(|x| x.0);
                if cur_l.is_none() && cur_e.is_none() {
                    continue;
                }
                if let Some(net_idx) = self.design.nodes[node].net_idx {
                    let net = &self.design.nets[net_idx];
                    if net.drivers.contains(&node) {
                        for &snk in &net.sinks {
                            if self.consts.pin_vals[snk] != LogicVal::X {
                                continue;
                            }
                            if let Some(al) = cur_l
                                && best_late[snk][ntr.idx()].is_none_or(|(p, _)| al > p)
                            {
                                best_late[snk][ntr.idx()] = Some((al, Some((node, ntr))));
                            }
                            if let Some(ae) = cur_e
                                && best_early[snk][ntr.idx()].is_none_or(|(p, _)| ae < p)
                            {
                                best_early[snk][ntr.idx()] = Some((ae, Some((node, ntr))));
                            }
                        }
                    }
                }
                if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[node].kind {
                    let inst = &self.design.instances[inst_idx];
                    if let Some(cdef) = self.lib.cells.get(&inst.cell_type) {
                        for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                            if ca.from_pin != *pin
                                || !self.is_comb_arc_enabled(inst_idx, ca_idx)
                                || cdef.is_latch_dq(&ca.from_pin, &ca.to_pin)
                            {
                                continue;
                            }
                            let Some(&v) = inst.pin_nodes.get(&ca.to_pin) else {
                                continue;
                            };
                            if self.consts.pin_vals[v] != LogicVal::X {
                                continue;
                            }
                            let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
                            for out_tr in Trans::ALL {
                                if !ca.can_propagate(&sense, ntr, out_tr) {
                                    continue;
                                }
                                if let Some(al) = cur_l
                                    && let Some(s_in) =
                                        self.slews[node][ntr.idx()][Analysis::Late.idx()]
                                    && let Some((d, _)) = ca.eval(
                                        out_tr,
                                        s_in,
                                        self.node_load_pf(v, out_tr, Analysis::Late),
                                    )
                                    && best_late[v][out_tr.idx()].is_none_or(|(p, _)| al + d > p)
                                {
                                    best_late[v][out_tr.idx()] = Some((al + d, Some((node, ntr))));
                                }
                                if let Some(ae) = cur_e
                                    && let Some(s_in) =
                                        self.slews[node][ntr.idx()][Analysis::Early.idx()]
                                    && let Some((d, _)) = ca.eval(
                                        out_tr,
                                        s_in,
                                        self.node_load_pf(v, out_tr, Analysis::Early),
                                    )
                                    && best_early[v][out_tr.idx()].is_none_or(|(p, _)| ae + d < p)
                                {
                                    best_early[v][out_tr.idx()] = Some((ae + d, Some((node, ntr))));
                                }
                            }
                        }
                    }
                }
            }
        }

        let reconstruct =
            |tbl: &[[Option<(f64, Option<(usize, Trans)>)>; 2]], ep_n: usize, etr: Trans| {
                let (arr, _) = tbl[ep_n][etr.idx()]?;
                let mut rev = Vec::new();
                let mut cur = (ep_n, etr);
                for _ in 0..=n {
                    rev.push((self.design.nodes[cur.0].name.clone(), cur.1));
                    let Some(prev) = tbl[cur.0][cur.1.idx()].and_then(|x| x.1) else {
                        break;
                    };
                    cur = prev;
                }
                let mut trc = seed_trace[..seed_trace.len().saturating_sub(1)].to_vec();
                rev.reverse();
                trc.extend(rev);
                Some((arr, trc))
            };

        for (&ep_node, ep_indices) in ep_by_node {
            for end_tr in Trans::ALL {
                let l_res = reconstruct(&best_late, ep_node, end_tr);
                let e_res = reconstruct(&best_early, ep_node, end_tr);
                match (l_res, e_res) {
                    (Some((al, tl)), Some((ae, te))) if tl == te => {
                        for &ep_idx in ep_indices {
                            self.record_path_at_endpoint(
                                &self.endpoints[ep_idx],
                                ep_node,
                                end_tr,
                                startpoint,
                                start_tr,
                                tag,
                                Some(al),
                                Some(ae),
                                &tl,
                                out_paths,
                            )?;
                        }
                    }
                    (l_opt, e_opt) => {
                        if let Some((al, tl)) = l_opt {
                            for &ep_idx in ep_indices {
                                self.record_path_at_endpoint(
                                    &self.endpoints[ep_idx],
                                    ep_node,
                                    end_tr,
                                    startpoint,
                                    start_tr,
                                    tag,
                                    Some(al),
                                    None,
                                    &tl,
                                    out_paths,
                                )?;
                            }
                        }
                        if let Some((ae, te)) = e_opt {
                            for &ep_idx in ep_indices {
                                self.record_path_at_endpoint(
                                    &self.endpoints[ep_idx],
                                    ep_node,
                                    end_tr,
                                    startpoint,
                                    start_tr,
                                    tag,
                                    None,
                                    Some(ae),
                                    &te,
                                    out_paths,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs_enumerate_from(
        &self,
        u: usize,
        u_tr: Trans,
        startpoint: &str,
        start_tr: Trans,
        tag: &Tag,
        arr_late: Option<f64>,
        arr_early: Option<f64>,
        trace: &mut Vec<(String, Trans)>,
        visited: &mut Vec<usize>,
        ep_by_node: &BTreeMap<usize, Vec<usize>>,
        visits: &mut usize,
        out_paths: &mut Vec<PathEval>,
    ) -> Result<(), Unsupported> {
        *visits += 1;
        if *visits > Self::MAX_DFS_VISITS_PER_SEED || out_paths.len() >= Self::MAX_TOTAL_ENUM_PATHS
        {
            return Ok(());
        }
        if let Some(ep_indices) = ep_by_node.get(&u) {
            for &ep_idx in ep_indices {
                let ep = &self.endpoints[ep_idx];
                self.record_path_at_endpoint(
                    ep, u, u_tr, startpoint, start_tr, tag, arr_late, arr_early, trace, out_paths,
                )?;
            }
        }

        // Follow net arcs if `u` is a net driver.
        if let Some(net_idx) = self.design.nodes[u].net_idx {
            let net = &self.design.nets[net_idx];
            if net.drivers.contains(&u) {
                for &snk in &net.sinks {
                    if self.consts.pin_vals[snk] != LogicVal::X || visited.contains(&snk) {
                        continue;
                    }
                    trace.push((self.design.nodes[snk].name.clone(), u_tr));
                    visited.push(snk);
                    self.dfs_enumerate_from(
                        snk, u_tr, startpoint, start_tr, tag, arr_late, arr_early, trace, visited,
                        ep_by_node, visits, out_paths,
                    )?;
                    visited.pop();
                    trace.pop();
                }
            }
        }

        // Follow enabled combinational cell arcs (excluding latch D -> Q, which ends at D and
        // re-launches from G).
        if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind {
            let inst = &self.design.instances[inst_idx];
            if let Some(cdef) = self.lib.cells.get(&inst.cell_type) {
                for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                    if ca.from_pin != *pin
                        || !self.is_comb_arc_enabled(inst_idx, ca_idx)
                        || cdef.is_latch_dq(&ca.from_pin, &ca.to_pin)
                    {
                        continue;
                    }
                    let Some(&v) = inst.pin_nodes.get(&ca.to_pin) else {
                        continue;
                    };
                    if self.consts.pin_vals[v] != LogicVal::X || visited.contains(&v) {
                        continue;
                    }
                    let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
                    for out_tr in Trans::ALL {
                        if !ca.can_propagate(&sense, u_tr, out_tr) {
                            continue;
                        }
                        let eval_step = |arr: Option<f64>, an: Analysis| {
                            let a = arr?;
                            let s_in = self.slews[u][u_tr.idx()][an.idx()]?;
                            let (d, _) = ca.eval(out_tr, s_in, self.node_load_pf(v, out_tr, an))?;
                            Some(a + d)
                        };
                        let (next_late, next_early) = (
                            eval_step(arr_late, Analysis::Late),
                            eval_step(arr_early, Analysis::Early),
                        );
                        if next_late.is_none() && next_early.is_none() {
                            continue;
                        }
                        trace.push((self.design.nodes[v].name.clone(), out_tr));
                        visited.push(v);
                        self.dfs_enumerate_from(
                            v, out_tr, startpoint, start_tr, tag, next_late, next_early, trace,
                            visited, ep_by_node, visits, out_paths,
                        )?;
                        visited.pop();
                        trace.pop();
                    }
                }
            }
        }
        Ok(())
    }

    #[rustfmt::skip]
    #[allow(clippy::too_many_arguments)]
    fn record_path_at_endpoint(
        &self, ep: &EndpointEval, u: usize, u_tr: Trans, startpoint: &str, start_tr: Trans,
        tag: &Tag, arr_late: Option<f64>, arr_early: Option<f64>, trace: &[(String, Trans)],
        out_paths: &mut Vec<PathEval>,
    ) -> Result<(), Unsupported> {
        let Some(launch_clk) = self.sdc.clocks.get(&tag.clock) else { return Ok(()) };
        let mut best_setup_req: Option<(f64, String, Trans)> = None;
        let mut best_hold_req: Option<(f64, String, Trans)> = None;

        match ep.kind {
            CheckKindEval::SetupHold | CheckKindEval::RecoveryRemoval => {
                let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind else { return Ok(()) };
                let inst = &self.design.instances[inst_idx];
                let cdef = &self.lib.cells[&inst.cell_type];
                let is_async = ep.kind == CheckKindEval::RecoveryRemoval;
                let rel_out_cap = cdef.pins.iter().find(|(_, p)| p.direction == Direction::Output)
                    .and_then(|(pname, _)| inst.pin_nodes.get(pname))
                    .map_or(0.0, |&qn| self.node_load_pf(qn, Trans::Rise, Analysis::Late));

                for (chk_idx, chk) in cdef.check_arcs.iter().enumerate() {
                    if chk.data_pin != *pin || chk.kind.is_async() != is_async
                        || self.consts.disabled_check_arcs.contains_key(&(inst_idx, chk_idx))
                    {
                        continue;
                    }
                    let Some(&ck_node) = inst.pin_nodes.get(&chk.clock_pin) else { continue };
                    let Some(waves) = self.clk_waves.get(&ck_node) else { continue };
                    for w in waves {
                        if w.pin_edge != chk.clock_edge { continue; }
                        let Some(cap_clk) = self.sdc.clocks.get(&w.clock) else { continue };
                        let (d_s, d_h) = clock_relations_ns(launch_clk, tag.edge, cap_clk, w.source_edge)?;
                        if chk.kind.is_setup_like() {
                            if let (Some(_), Some(s_d)) = (arr_late, self.slews[u][u_tr.idx()][Analysis::Late.idx()]) {
                                let s_ck = cap_clk.transition(w.pin_edge, Analysis::Late);
                                if let Some(s_val) = chk.constraint_opt(u_tr, s_d, s_ck, rel_out_cap) {
                                    let lat_c = cap_clk.total_latency(w.source_edge, Analysis::Early);
                                    let u_s = self.sdc.clock_uncertainty(&tag.clock, &w.clock, Analysis::Late);
                                    let req = d_s + lat_c - s_val - u_s;
                                    if best_setup_req.as_ref().is_none_or(|(r, _, _)| req < *r) {
                                        best_setup_req = Some((req, w.clock.clone(), w.source_edge));
                                    }
                                }
                            }
                        } else if let (Some(_), Some(s_d)) = (arr_early, self.slews[u][u_tr.idx()][Analysis::Early.idx()]) {
                            let s_ck = cap_clk.transition(w.pin_edge, Analysis::Early);
                            if let Some(h_val) = chk.constraint_opt(u_tr, s_d, s_ck, rel_out_cap) {
                                let lat_c = cap_clk.total_latency(w.source_edge, Analysis::Late);
                                let u_h = self.sdc.clock_uncertainty(&tag.clock, &w.clock, Analysis::Early);
                                let req = d_h + lat_c + h_val + u_h;
                                if best_hold_req.as_ref().is_none_or(|(r, _, _)| req > *r) {
                                    best_hold_req = Some((req, w.clock.clone(), w.source_edge));
                                }
                            }
                        }
                    }
                }
            }
            CheckKindEval::LatchSetupHold => {
                let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind else { return Ok(()) };
                let inst = &self.design.instances[inst_idx];
                let cdef = &self.lib.cells[&inst.cell_type];
                let Some(latch) = cdef.latch_for_d(pin) else { return Ok(()) };
                let Some(&g_node) = inst.pin_nodes.get(&latch.enable_pin) else { return Ok(()) };
                let Some(g_waves) = self.clk_waves.get(&g_node) else { return Ok(()) };
                let (close_edge, open_edge) = (latch.close_edge(), latch.open_edge());
                let find_chk = |k| cdef.check_arcs.iter().find(|chk| {
                    chk.data_pin == latch.data_pin && chk.clock_pin == latch.enable_pin
                        && chk.kind == k && chk.clock_edge == close_edge
                });
                let (setup_chk, hold_chk) = (find_chk(CheckArcKind::Setup), find_chk(CheckArcKind::Hold));
                for w_close in g_waves {
                    if w_close.pin_edge != close_edge { continue; }
                    let Some(w_open) = g_waves.iter().find(|w| {
                        w.clock == w_close.clock && w.source_port == w_close.source_port && w.pin_edge == open_edge
                    }) else {
                        continue;
                    };
                    let Some(cap_clk) = self.sdc.clocks.get(&w_close.clock) else { continue };
                    let pulse_w = pulse_width_ns(cap_clk, w_open.source_edge, w_close.source_edge);
                    let (d_close, d_h) = clock_relations_ns(launch_clk, tag.edge, cap_clk, w_close.source_edge)?;
                    if let (Some(al), Some(s_d)) = (arr_late, self.slews[u][u_tr.idx()][Analysis::Late.idx()]) {
                        let s_g = cap_clk.transition(close_edge, Analysis::Late);
                        let s_val = setup_chk.and_then(|chk| chk.constraint_opt(u_tr, s_d, s_g, 0.0)).unwrap_or(0.0);
                        let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Early);
                        let u_s = self.sdc.clock_uncertainty(&tag.clock, &w_close.clock, Analysis::Late);
                        let r_open = (d_close - pulse_w) + lat_c - u_s;
                        let r_close_u = d_close + lat_c - s_val - u_s;
                        let r_close = self.sdc.max_time_borrow_for(&inst.name, &ep.endpoint, &w_close.clock)
                            .map_or(r_close_u, |b| r_close_u.min(r_open + b));
                        let eff_req = al.clamp(r_open, r_close);
                        if best_setup_req.as_ref().is_none_or(|(r, _, _)| eff_req < *r) {
                            best_setup_req = Some((eff_req, w_close.clock.clone(), w_close.source_edge));
                        }
                    }
                    if let (Some(_), Some(s_d)) = (arr_early, self.slews[u][u_tr.idx()][Analysis::Early.idx()]) {
                        let s_g = cap_clk.transition(close_edge, Analysis::Early);
                        let h_val = hold_chk.and_then(|chk| chk.constraint_opt(u_tr, s_d, s_g, 0.0)).unwrap_or(0.0);
                        let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Late);
                        let u_h = self.sdc.clock_uncertainty(&tag.clock, &w_close.clock, Analysis::Early);
                        let req = d_h + lat_c + h_val + u_h;
                        if best_hold_req.as_ref().is_none_or(|(r, _, _)| req > *r) {
                            best_hold_req = Some((req, w_close.clock.clone(), w_close.source_edge));
                        }
                    }
                }
            }
            CheckKindEval::ClockGating => {
                let NodeKind::InstancePin { inst_idx, .. } = self.design.nodes[u].kind else { return Ok(()) };
                let clk_reaching = self.nodes_reaching_clock_pins();
                for cg in self.detect_clock_gates_on_inst(inst_idx, &clk_reaching) {
                    if cg.en_node != u { continue; }
                    let Some(waves) = self.clk_waves.get(&cg.ck_node) else { continue };
                    let (lead_edge, trail_edge) = if cg.active_low { (Trans::Fall, Trans::Rise) } else { (Trans::Rise, Trans::Fall) };
                    for w_lead in waves {
                        if w_lead.pin_edge != lead_edge { continue; }
                        let Some(w_trail) = waves.iter().find(|w| {
                            w.clock == w_lead.clock && w.source_port == w_lead.source_port && w.pin_edge == trail_edge
                        }) else {
                            continue;
                        };
                        let Some(cap_clk) = self.sdc.clocks.get(&w_lead.clock) else { continue };
                        if arr_late.is_some() {
                            let (d_s, _) = clock_relations_ns(launch_clk, tag.edge, cap_clk, w_lead.source_edge)?;
                            let lat_c = cap_clk.total_latency(w_lead.source_edge, Analysis::Early);
                            let u_s = self.sdc.clock_uncertainty(&tag.clock, &w_lead.clock, Analysis::Late);
                            let req = d_s + lat_c - u_s;
                            if best_setup_req.as_ref().is_none_or(|(r, _, _)| req < *r) {
                                best_setup_req = Some((req, w_lead.clock.clone(), w_lead.source_edge));
                            }
                        }
                        if arr_early.is_some() {
                            let h_trail = gating_trailing_relation_ns(launch_clk, tag.edge, cap_clk, w_trail.source_edge)?;
                            let lat_c = cap_clk.total_latency(w_trail.source_edge, Analysis::Late);
                            let u_h = self.sdc.clock_uncertainty(&tag.clock, &w_trail.clock, Analysis::Early);
                            let req = h_trail + lat_c + u_h;
                            if best_hold_req.as_ref().is_none_or(|(r, _, _)| req > *r) {
                                best_hold_req = Some((req, w_trail.clock.clone(), w_trail.source_edge));
                            }
                        }
                    }
                }
            }
            CheckKindEval::OutputDelay => {
                for od in self.sdc.output_delays_for_port(&ep.endpoint) {
                    let Some(cap_clk) = self.sdc.clocks.get(&od.clock) else { continue };
                    let (d_s, d_h) = clock_relations_ns(launch_clk, tag.edge, cap_clk, od.clock_edge)?;
                    if arr_late.is_some() && let Some(v_max) = od.bound(u_tr, Analysis::Late) {
                        let l_src = if od.source_latency_included { 0.0 } else { cap_clk.source_latency(od.clock_edge, Analysis::Early) };
                        let l_net = if od.network_latency_included { 0.0 } else { cap_clk.network_latency(od.clock_edge, Analysis::Early) };
                        let u_s = self.sdc.clock_uncertainty(&tag.clock, &od.clock, Analysis::Late);
                        let req = d_s + l_src + l_net - v_max - u_s;
                        if best_setup_req.as_ref().is_none_or(|(r, _, _)| req < *r) {
                            best_setup_req = Some((req, od.clock.clone(), od.clock_edge));
                        }
                    }
                    if arr_early.is_some() && let Some(v_min) = od.bound(u_tr, Analysis::Early) {
                        let l_src = if od.source_latency_included { 0.0 } else { cap_clk.source_latency(od.clock_edge, Analysis::Late) };
                        let l_net = if od.network_latency_included { 0.0 } else { cap_clk.network_latency(od.clock_edge, Analysis::Late) };
                        let u_h = self.sdc.clock_uncertainty(&tag.clock, &od.clock, Analysis::Early);
                        let req = d_h + l_src + l_net - v_min + u_h;
                        if best_hold_req.as_ref().is_none_or(|(r, _, _)| req > *r) {
                            best_hold_req = Some((req, od.clock.clone(), od.clock_edge));
                        }
                    }
                }
            }
        }

        let setup_slack = best_setup_req.as_ref().zip(arr_late).map(|((req, _, _), al)| req - al);
        let hold_slack = best_hold_req.as_ref().zip(arr_early).map(|((req, _, _), ae)| ae - req);
        if setup_slack.is_none() && hold_slack.is_none() {
            return Ok(());
        }

        let cap_clk = best_setup_req.as_ref().map(|(_, c, _)| c.clone())
            .or_else(|| best_hold_req.as_ref().map(|(_, c, _)| c.clone()));
        let cap_edge = best_setup_req.as_ref().map(|(_, _, e)| *e)
            .or_else(|| best_hold_req.as_ref().map(|(_, _, e)| *e));

        out_paths.push(PathEval {
            startpoint: startpoint.to_string(),
            endpoint: ep.endpoint.clone(),
            start_tr,
            end_tr: u_tr,
            launch_clock: Some(tag.clock.clone()),
            launch_edge: Some(tag.edge),
            capture_clock: cap_clk,
            capture_edge: cap_edge,
            kind: ep.kind,
            path_group: ep.path_group.clone(),
            pins: trace.to_vec(),
            arrival_late_ns: arr_late,
            arrival_early_ns: arr_early,
            setup_required_ns: best_setup_req.map(|(r, _, _)| r),
            hold_required_ns: best_hold_req.map(|(r, _, _)| r),
            setup_slack_ns: setup_slack,
            hold_slack_ns: hold_slack,
        });
        Ok(())
    }
}

fn is_func_const_when(
    f: &Func,
    var: &str,
    val: bool,
    inst: &super::verilog::CellInst,
    pin_vals: &[LogicVal],
) -> bool {
    let mut vars = Vec::new();
    super::lib_model::collect_vars(f, &mut vars);
    let others: Vec<&str> = vars
        .iter()
        .map(String::as_str)
        .filter(|&v| {
            if v == var {
                return false;
            }
            if let Some(&ni) = inst.pin_nodes.get(v) {
                pin_vals[ni] == LogicVal::X
            } else {
                true
            }
        })
        .collect();
    if others.len() > 8 {
        return false;
    }
    let mut seen: Option<bool> = None;
    for mask in 0..(1usize << others.len()) {
        let out = f.eval(&|name| {
            if name == var {
                return Some(val);
            }
            if let Some(&ni) = inst.pin_nodes.get(name) {
                match pin_vals[ni] {
                    LogicVal::Zero => return Some(false),
                    LogicVal::One => return Some(true),
                    LogicVal::X => {}
                }
            }
            let idx = others.iter().position(|&o| o == name)?;
            Some(((mask >> idx) & 1) != 0)
        });
        let Some(b) = out else {
            return false;
        };
        if let Some(prev) = seen {
            if prev != b {
                return false;
            }
        } else {
            seen = Some(b);
        }
    }
    seen.is_some()
}
