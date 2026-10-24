// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent demand-driven timing evaluator (`STA-REQ-0101` to `0125`, `0200` to `0228`,
//! `0500` to `0504`).
//!
//! Computes net capacitances (including driver output-pin capacitance and lumped wire loads),
//! breaks combinational cycles via Tarjan's strongly-connected components algorithm, floods clock
//! networks, memoizes per-pin worst slews (including untagged arcs), and propagates tagged early
//! and late arrivals across combinational gates, three-state buses, and level-sensitive latches.

use std::collections::{BTreeMap, BTreeSet};

use rusteda_formats::liberty::{Direction, TimingSense};

use super::Unsupported;
use super::canon_sdc::CanonSdc;
use super::constants::{ConstPropResult, LogicVal};
use super::lib_model::{Analysis, CellDef, CombArc, CornerLib, LatchDef, Trans};
use super::verilog::{FlatDesign, NodeKind};

mod transitions;

/// Launch tag `(clock_name, source_edge)` (STA-REQ-0217).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag {
    pub clock: String,
    pub edge: Trans,
}

/// Provenance of a tagged arrival at a node, used for backward path reconstruction.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrivalOrigin {
    InputPort {
        port: String,
    },
    RegisterLaunch {
        ck_pin: usize,
        q_pin: usize,
        clk_slew_ns: f64,
        arc_delay_ns: f64,
        launch_lat_ns: f64,
    },
    LatchBorrow {
        d_pin: usize,
        g_pin: usize,
        q_pin: usize,
        src_tag: Tag,
        src_tr: Trans,
        borrowed_entry_ns: f64,
        dq_delay_ns: f64,
    },
    CombArc {
        from_pin: usize,
        from_tr: Trans,
        to_pin: usize,
        arc_delay_ns: f64,
    },
    NetArc {
        driver_pin: usize,
    },
}

/// Tagged arrival entry at a `(node, transition, analysis)` point.
#[derive(Debug, Clone)]
pub struct ArrivalEntry {
    pub tag: Tag,
    pub arrival_ns: f64,
    pub origin: ArrivalOrigin,
}

/// Clock wave reaching a pin in the clock network (STA-REQ-0200).
#[derive(Debug, Clone, PartialEq)]
pub struct ClockWave {
    pub clock: String,
    pub source_port: String,
    pub source_edge: Trans,
    pub pin_edge: Trans,
}

/// Fallback trait for R1 -> R2 zero-touch exception dispatch.
///
/// In `R1`, `TimingSolver` has no inherent `apply_exceptions` method, so calling
/// `self.apply_exceptions()` resolves to this trait method, which delegates to
/// `super::exceptions::ExceptionPaths::new()` and returns `Err(Unsupported)` (`Verdict::Undecided`).
/// In `R2`, `src/rev/exceptions.rs` defines an inherent `TimingSolver::apply_exceptions` method,
/// which shadows this trait method without modifying `eval.rs` or `mod.rs`.
#[allow(dead_code)]
pub(crate) trait ExceptionSolverFallback {
    fn apply_exceptions(&mut self) -> Result<(), Unsupported> {
        super::exceptions::ExceptionPaths::new().map(|_| ())
    }
}

impl ExceptionSolverFallback for TimingSolver<'_> {}

/// Independent per-corner timing solver.
#[derive(Debug)]
pub struct TimingSolver<'a> {
    pub(crate) design: &'a FlatDesign,
    pub(crate) lib: &'a CornerLib,
    pub(crate) sdc: &'a CanonSdc,
    pub(crate) consts: &'a ConstPropResult,
    /// Net capacitances `net_cap_pf[net_idx][tr][an]` in `pF`.
    pub(crate) net_cap_pf: Vec<[[f64; 2]; 2]>,
    /// Selected wire-load model name (if any).
    pub(crate) wire_load_model: Option<String>,
    /// Combinational arcs disabled to break SCC cycles `(inst_idx, comb_arc_idx)`.
    pub(crate) cycle_disabled_arcs: BTreeSet<(usize, usize)>,
    /// Number of non-trivial combinational SCCs broken.
    pub(crate) sccs_broken: usize,
    /// Topological order of node indices.
    pub(crate) topo_order: Vec<usize>,
    /// Clock waves reaching each node `node_idx -> Vec<ClockWave>`.
    pub(crate) clk_waves: BTreeMap<usize, Vec<ClockWave>>,
    /// Per-node slews `slews[node_idx][tr][an]` in `ns` (`STA-REQ-0108`).
    pub(crate) slews: Vec<[[Option<f64>; 2]; 2]>,
    /// Per-node tagged arrivals `arrivals[node_idx][tr][an]`.
    pub(crate) arrivals: Vec<[[Vec<ArrivalEntry>; 2]; 2]>,
    /// Evaluated endpoints in canonical order.
    pub(crate) endpoints: Vec<super::checks_eval::EndpointEval>,
    /// Exhaustively enumerated paths (for designs `<= 5000` pins).
    pub(crate) paths: Vec<super::checks_eval::PathEval>,
    /// Per-node backward required times `req[node_idx][tr][an]` and slacks `slack[node_idx][tr][an]`.
    pub(crate) node_req_ns: Vec<[[Option<f64>; 2]; 2]>,
    pub(crate) node_slack_ns: Vec<[[Option<f64>; 2]; 2]>,
    /// Structured diagnostics emitted during analysis (`D0102`, `D0110`–`D0114`, etc.).
    pub(crate) diagnostics: Vec<super::DiagRecord>,
    /// Exception counts by kind (`false_path`, `multicycle_path`, `max_delay`, `min_delay`, `clock_groups`).
    pub(crate) exception_counts: BTreeMap<String, usize>,
    /// Count of unused exceptions (`D0112`).
    pub(crate) unused_exceptions: usize,
}

impl<'a> TimingSolver<'a> {
    /// Creates and runs the independent timing solver on `(design, lib, sdc, consts)`.
    pub fn solve(
        design: &'a FlatDesign,
        lib: &'a CornerLib,
        sdc: &'a CanonSdc,
        consts: &'a ConstPropResult,
    ) -> Result<Self, Unsupported> {
        let n_nodes = design.nodes.len();
        let mut solver = Self {
            design,
            lib,
            sdc,
            consts,
            net_cap_pf: vec![[[0.0; 2]; 2]; design.nets.len()],
            wire_load_model: None,
            cycle_disabled_arcs: BTreeSet::new(),
            sccs_broken: 0,
            topo_order: Vec::with_capacity(n_nodes),
            clk_waves: BTreeMap::new(),
            slews: vec![[[None; 2]; 2]; n_nodes],
            arrivals: (0..n_nodes)
                .map(|_| [[Vec::new(), Vec::new()], [Vec::new(), Vec::new()]])
                .collect(),
            endpoints: Vec::new(),
            paths: Vec::new(),
            node_req_ns: vec![[[None; 2]; 2]; n_nodes],
            node_slack_ns: vec![[[None; 2]; 2]; n_nodes],
            diagnostics: Vec::new(),
            exception_counts: BTreeMap::new(),
            unused_exceptions: 0,
        };

        solver.compute_net_loads();
        solver.break_cycles_and_toposort();
        solver.propagate_clock_networks();
        solver.propagate_slews();
        solver.propagate_arrivals()?;
        solver.evaluate_endpoints()?;
        solver.propagate_required_times();
        if solver.sdc.has_exceptions_or_clock_groups() {
            solver.apply_exceptions()?;
        } else if solver.design.nodes.len() <= 5000 {
            solver.enumerate_paths()?;
        }
        Ok(solver)
    }

    /// Computes total capacitive load per net `C_net[net][tr][an]` in `pF`
    /// (`STA-REQ-0110`, `0121`, `0123`, `0124`, `0125`).
    fn compute_net_loads(&mut self) {
        let total_area: f64 = self
            .design
            .instances
            .iter()
            .filter_map(|inst| self.lib.cells.get(&inst.cell_type))
            .map(|c| c.area)
            .sum();

        let wl_opt = self.lib.select_wire_load(
            self.sdc.wire_load_model.as_deref(),
            self.sdc.wire_load_selection.as_deref(),
            total_area,
        );
        self.wire_load_model = wl_opt.map(|w| w.name.clone());

        for (net_idx, net) in self.design.nets.iter().enumerate() {
            let mut fanout = 0usize;
            for &pin_node in &net.connected_pins {
                if let NodeKind::InstancePin { inst_idx, ref pin } =
                    self.design.nodes[pin_node].kind
                {
                    let inst = &self.design.instances[inst_idx];
                    if let Some(cdef) = self.lib.cells.get(&inst.cell_type)
                        && let Some(pdef) = cdef.pins.get(pin)
                        && matches!(pdef.direction, Direction::Input | Direction::Inout)
                    {
                        fanout += 1;
                    }
                }
            }

            for tr in Trans::ALL {
                for an in Analysis::ALL {
                    let mut c_pf = 0.0;
                    // STA-REQ-0110, 0121: sum all connected cell pin capacitances (both load input
                    // pins and driver/three-state output pins).
                    for &pin_node in &net.connected_pins {
                        if let NodeKind::InstancePin { inst_idx, ref pin } =
                            self.design.nodes[pin_node].kind
                        {
                            let inst = &self.design.instances[inst_idx];
                            if let Some(cdef) = self.lib.cells.get(&inst.cell_type)
                                && let Some(pdef) = cdef.pins.get(pin)
                            {
                                c_pf += pdef.cap(tr, an);
                            }
                        }
                    }

                    // Port pin loads and port wire loads.
                    for port_name in &net.connected_ports {
                        c_pf += self.sdc.port_pin_load(port_name, tr, an);
                        c_pf += self.sdc.port_wire_load(port_name, tr, an);
                    }

                    // Net explicit wire load or wire-load table capacitance (STA-REQ-0123, 0124).
                    if let Some(explicit_net_wl) = self.sdc.net_wire_load(&net.name, tr, an) {
                        c_pf += explicit_net_wl;
                    } else if !net.is_port_connected
                        && let Some(wl) = wl_opt
                    {
                        c_pf += wl.length_for_fanout(fanout as u32) * wl.capacitance;
                    }

                    self.net_cap_pf[net_idx][tr.idx()][an.idx()] = c_pf;
                }
            }
        }
    }

    /// Returns the load capacitance in `pF` driven by node `node_idx`.
    #[must_use]
    pub(crate) fn node_load_pf(&self, node_idx: usize, tr: Trans, an: Analysis) -> f64 {
        self.design.nodes[node_idx]
            .net_idx
            .map_or(0.0, |ni| self.net_cap_pf[ni][tr.idx()][an.idx()])
    }

    /// Checks whether combinational arc `(inst_idx, comb_arc_idx)` is enabled.
    #[must_use]
    pub(crate) fn is_comb_arc_enabled(&self, inst_idx: usize, comb_arc_idx: usize) -> bool {
        !self
            .consts
            .disabled_comb_arcs
            .contains_key(&(inst_idx, comb_arc_idx))
            && !self.cycle_disabled_arcs.contains(&(inst_idx, comb_arc_idx))
    }

    /// Returns the effective `TimingSense` of combinational arc `(inst_idx, comb_arc_idx)` after
    /// constant propagation (`STA-REQ-0153`).
    #[must_use]
    pub(crate) fn effective_comb_sense(
        &self,
        inst_idx: usize,
        comb_arc_idx: usize,
        default_sense: &TimingSense,
    ) -> TimingSense {
        self.consts
            .refined_comb_sense
            .get(&(inst_idx, comb_arc_idx))
            .cloned()
            .unwrap_or_else(|| default_sense.clone())
    }

    /// Breaks combinational cycles using Tarjan's SCC algorithm and builds a topological order of
    /// all nodes (`STA-REQ-0102`).
    fn break_cycles_and_toposort(&mut self) {
        let n = self.design.nodes.len();
        loop {
            let adj = self.build_comb_adj(false);
            let sccs = tarjan_sccs(n, &adj);
            let mut broke_any = false;
            for scc in sccs {
                if scc.len() <= 1 {
                    continue;
                }
                let scc_set: BTreeSet<usize> = scc.iter().copied().collect();
                // Deterministic cycle breaking: pick the combinational cell arc inside the SCC
                // whose `(to_pin_name, from_pin_name, comb_arc_idx)` is lexicographically largest.
                let mut candidate: Option<((String, String, usize), (usize, usize))> = None;
                for &u in &scc {
                    if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind {
                        let inst = &self.design.instances[inst_idx];
                        let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                            continue;
                        };
                        for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                            if ca.from_pin != *pin
                                || !self.is_comb_arc_enabled(inst_idx, ca_idx)
                                || cdef.is_latch_dq(&ca.from_pin, &ca.to_pin)
                            {
                                continue;
                            }
                            if let Some(&v) = inst.pin_nodes.get(&ca.to_pin)
                                && scc_set.contains(&v)
                            {
                                let key = (
                                    self.design.nodes[v].name.clone(),
                                    self.design.nodes[u].name.clone(),
                                    ca_idx,
                                );
                                if candidate.as_ref().is_none_or(|(k, _)| key > *k) {
                                    candidate = Some((key, (inst_idx, ca_idx)));
                                }
                            }
                        }
                    }
                }
                if let Some(((to_name, from_name, _), arc_key)) = candidate {
                    self.cycle_disabled_arcs.insert(arc_key);
                    self.sccs_broken += 1;
                    self.diagnostics.push(super::DiagRecord {
                        code: "D0102".to_string(),
                        severity: "warning".to_string(),
                        message: format!("combinational cycle broken at {from_name} -> {to_name}"),
                        line: None,
                    });
                    broke_any = true;
                }
            }
            if !broke_any {
                break;
            }
        }

        // Build topological order (including acyclic latch D -> Q arcs so latch stages are ordered
        // after their D inputs, while omitting latch D -> Q arcs that close a sequential loop).
        let adj_no_latch = self.build_comb_adj(false);
        let mut adj_with_latch = self.build_comb_adj(true);
        for scc in tarjan_sccs(n, &adj_with_latch) {
            if scc.len() > 1 {
                let scc_set: BTreeSet<usize> = scc.into_iter().collect();
                for &u in &scc_set {
                    adj_with_latch[u]
                        .retain(|v| !scc_set.contains(v) || adj_no_latch[u].contains(v));
                }
            }
        }
        let mut indeg = vec![0usize; n];
        for edges in &adj_with_latch {
            for &v in edges {
                indeg[v] += 1;
            }
        }
        // Use a min-heap (via BTreeSet) on node index for canonical deterministic topological order.
        let mut ready: BTreeSet<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(u) = ready.pop_first() {
            order.push(u);
            for &v in &adj_with_latch[u] {
                indeg[v] -= 1;
                if indeg[v] == 0 {
                    ready.insert(v);
                }
            }
        }
        for i in 0..n {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        self.topo_order = order;
    }

    fn build_comb_adj(&self, include_latch_dq: bool) -> Vec<Vec<usize>> {
        let n = self.design.nodes.len();
        let mut adj = vec![Vec::new(); n];
        for net in &self.design.nets {
            for &drv in &net.drivers {
                if self.consts.pin_vals[drv] != LogicVal::X {
                    continue;
                }
                for &snk in &net.sinks {
                    if self.consts.pin_vals[snk] == LogicVal::X {
                        adj[drv].push(snk);
                    }
                }
            }
        }
        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                continue;
            };
            for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                if !self.is_comb_arc_enabled(inst_idx, ca_idx) {
                    continue;
                }
                if !include_latch_dq && cdef.is_latch_dq(&ca.from_pin, &ca.to_pin) {
                    continue;
                }
                if let (Some(&u), Some(&v)) = (
                    inst.pin_nodes.get(&ca.from_pin),
                    inst.pin_nodes.get(&ca.to_pin),
                ) && self.consts.pin_vals[u] == LogicVal::X
                    && self.consts.pin_vals[v] == LogicVal::X
                {
                    adj[u].push(v);
                }
            }
        }
        for v in &mut adj {
            v.sort_unstable();
            v.dedup();
        }
        adj
    }

    /// Floods clock networks from `create_clock` source ports through enabled net arcs and
    /// combinational cell arcs (`STA-REQ-0200`).
    fn propagate_clock_networks(&mut self) {
        for (clk_name, clk_def) in &self.sdc.clocks {
            for src_name in &clk_def.sources {
                let Some(&src_node) = self.design.node_map.get(src_name) else {
                    continue;
                };
                if self.consts.pin_vals[src_node] != LogicVal::X {
                    continue;
                }
                for src_edge in Trans::ALL {
                    self.push_clock_wave(
                        src_node,
                        ClockWave {
                            clock: clk_name.clone(),
                            source_port: src_name.clone(),
                            source_edge: src_edge,
                            pin_edge: src_edge,
                        },
                    );
                }
            }
        }

        let topo = self.topo_order.clone();
        for u in topo {
            if self.consts.pin_vals[u] != LogicVal::X {
                continue;
            }
            let Some(waves) = self.clk_waves.get(&u).cloned() else {
                continue;
            };
            // Propagate across net arcs if `u` is a net driver.
            if let Some(net_idx) = self.design.nodes[u].net_idx {
                let net = &self.design.nets[net_idx];
                if net.drivers.contains(&u) {
                    for &snk in &net.sinks {
                        if self.consts.pin_vals[snk] != LogicVal::X {
                            continue;
                        }
                        for w in &waves {
                            self.push_clock_wave(snk, w.clone());
                        }
                    }
                }
            }
            // Propagate across combinational cell arcs (excluding latch D -> Q arcs, STA-REQ-0200).
            if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind {
                let inst = &self.design.instances[inst_idx];
                let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                    continue;
                };
                // Do not propagate a clock through a sequential clock pin or latch data pin.
                if cdef.is_latch_dq(pin, "") || cdef.latches.iter().any(|l| l.data_pin == *pin) {
                    continue;
                }
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
                    for w in &waves {
                        for out_tr in Trans::ALL {
                            if ca.can_propagate(&sense, w.pin_edge, out_tr) {
                                let mut nw = w.clone();
                                nw.pin_edge = out_tr;
                                self.push_clock_wave(v, nw);
                            }
                        }
                    }
                }
            }
        }
    }

    fn push_clock_wave(&mut self, node_idx: usize, wave: ClockWave) {
        let entry = self.clk_waves.entry(node_idx).or_default();
        if !entry.contains(&wave) {
            entry.push(wave);
        }
    }

    pub(crate) fn merge_slew(&mut self, node_idx: usize, tr: Trans, an: Analysis, val: f64) {
        let slot = &mut self.slews[node_idx][tr.idx()][an.idx()];
        *slot = Some(match *slot {
            None => val,
            Some(prev) => {
                if an == Analysis::Late {
                    prev.max(val)
                } else {
                    prev.min(val)
                }
            }
        });
    }

    /// Evaluates `set_driving_cell` drive delay (`delay(s_in, C_port) - delay(s_in, 0.0)`) and
    /// output transition `slew(s_in, C_port)` on an input port (`STA-REQ-0218`).
    #[must_use]
    pub(crate) fn driving_cell_delay_and_slew(
        &self,
        port_node: usize,
        port_name: &str,
        tr: Trans,
        an: Analysis,
    ) -> Option<(f64, f64)> {
        let dc = self.sdc.driving_cell(port_name, tr, an)?;
        let cdef = self.lib.cells.get(&dc.lib_cell)?;
        let arc = cdef.comb_arcs.iter().find(|ca| {
            ca.to_pin == dc.pin && dc.from_pin.as_ref().is_none_or(|fp| ca.from_pin == *fp)
        })?;
        let s_in = if tr == Trans::Rise {
            dc.input_transition_rise
        } else {
            dc.input_transition_fall
        };
        let c_port = self.node_load_pf(port_node, tr, an);
        let (d_loaded, slew_loaded) = arc.eval(tr, s_in, c_port)?;
        let (d_unloaded, _) = arc.eval(tr, s_in, 0.0)?;
        Some((d_loaded - d_unloaded, slew_loaded))
    }

    /// Propagates tagged early and late arrivals across the timing graph (`STA-REQ-0101` to
    /// `0115`, `0217` to `0223`).
    fn propagate_arrivals(&mut self) -> Result<(), Unsupported> {
        // 1. Seed primary input ports with `set_input_delay` (STA-REQ-0217, 0218, 0219).
        for &port_node in &self.design.input_ports {
            if self.consts.pin_vals[port_node] != LogicVal::X {
                continue;
            }
            let port_name = self.design.nodes[port_node].name.clone();
            let idelays = self.sdc.input_delays_for_port(&port_name);
            for id in idelays {
                let Some(clk_def) = self.sdc.clocks.get(&id.clock) else {
                    continue;
                };
                let tag = Tag {
                    clock: id.clock.clone(),
                    edge: id.clock_edge,
                };
                for tr in Trans::ALL {
                    for an in Analysis::ALL {
                        let Some(val) = id.bound(tr, an) else {
                            continue;
                        };
                        let lat_src = if id.source_latency_included {
                            0.0
                        } else {
                            clk_def.source_latency(id.clock_edge, an)
                        };
                        let lat_net = if id.network_latency_included {
                            0.0
                        } else {
                            clk_def.network_latency(id.clock_edge, an)
                        };
                        let drv_delay = self
                            .driving_cell_delay_and_slew(port_node, &port_name, tr, an)
                            .map_or(0.0, |(d, _)| d);
                        let arr = val + lat_src + lat_net + drv_delay;
                        self.merge_arrival(
                            port_node,
                            tr,
                            an,
                            ArrivalEntry {
                                tag: tag.clone(),
                                arrival_ns: arr,
                                origin: ArrivalOrigin::InputPort {
                                    port: port_name.clone(),
                                },
                            },
                        );
                    }
                }
            }
        }

        // 2. Seed sequential `CK -> Q` and latch `G -> Q` launch arcs (STA-REQ-0217, 0222).
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
                    let tag = Tag {
                        clock: w.clock.clone(),
                        edge: w.source_edge,
                    };
                    for out_tr in Trans::ALL {
                        for an in Analysis::ALL {
                            let clk_slew = clk_def.transition(w.pin_edge, an);
                            let c_load = self.node_load_pf(q_node, out_tr, an);
                            let Some((arc_delay, _)) = la.eval(out_tr, clk_slew, c_load) else {
                                continue;
                            };
                            let launch_lat = clk_def.total_latency(w.source_edge, an);
                            self.merge_arrival(
                                q_node,
                                out_tr,
                                an,
                                ArrivalEntry {
                                    tag: tag.clone(),
                                    arrival_ns: launch_lat + arc_delay,
                                    origin: ArrivalOrigin::RegisterLaunch {
                                        ck_pin: ck_node,
                                        q_pin: q_node,
                                        clk_slew_ns: clk_slew,
                                        arc_delay_ns: arc_delay,
                                        launch_lat_ns: launch_lat,
                                    },
                                },
                            );
                        }
                    }
                }
            }
        }

        // 3. Propagate arrivals in topological order across nets, combinational arcs, and
        // transparent latch `D -> Q` borrowing arcs (`STA-REQ-0221`, `0222`).
        let topo = self.topo_order.clone();
        for u in topo {
            if self.consts.pin_vals[u] != LogicVal::X {
                continue;
            }
            // Net driver -> sink pins.
            if let Some(net_idx) = self.design.nodes[u].net_idx {
                let net = &self.design.nets[net_idx];
                if net.drivers.contains(&u) {
                    let u_arrs = self.arrivals[u].clone();
                    for &snk in &net.sinks {
                        if self.consts.pin_vals[snk] == LogicVal::X {
                            for tr in Trans::ALL {
                                for an in Analysis::ALL {
                                    for entry in &u_arrs[tr.idx()][an.idx()] {
                                        self.merge_arrival(
                                            snk,
                                            tr,
                                            an,
                                            ArrivalEntry {
                                                tag: entry.tag.clone(),
                                                arrival_ns: entry.arrival_ns,
                                                origin: ArrivalOrigin::NetArc { driver_pin: u },
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Instance input pin -> output pin arcs.
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
                    if self.consts.pin_vals[v] != LogicVal::X {
                        continue;
                    }
                    let mut late_only = false;
                    if let Some((l_idx, latch)) = cdef
                        .latches
                        .iter()
                        .enumerate()
                        .find(|(_, l)| l.data_pin == ca.from_pin && l.q_pin == ca.to_pin)
                    {
                        // A latch that a constant active enable holds transparent passes the
                        // late arrivals in their own tags; held opaque it passes none
                        // (`STA-REQ-0161`).
                        match self.consts.constant_enable_latches.get(&(inst_idx, l_idx)) {
                            Some(true) => late_only = true,
                            Some(false) => continue,
                            None => {
                                self.propagate_latch_dq(inst_idx, cdef, latch, ca, u, v)?;
                                continue;
                            }
                        }
                    }
                    let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
                    let u_arrs = self.arrivals[u].clone();
                    for in_tr in Trans::ALL {
                        for out_tr in Trans::ALL {
                            if !ca.can_propagate(&sense, in_tr, out_tr) {
                                continue;
                            }
                            for an in Analysis::ALL {
                                if late_only && an == Analysis::Early {
                                    continue;
                                }
                                let Some(s_in) = self.slews[u][in_tr.idx()][an.idx()] else {
                                    continue;
                                };
                                let c_load = self.node_load_pf(v, out_tr, an);
                                let Some((d_arc, _)) = ca.eval(out_tr, s_in, c_load) else {
                                    continue;
                                };
                                for entry in &u_arrs[in_tr.idx()][an.idx()] {
                                    self.merge_arrival(
                                        v,
                                        out_tr,
                                        an,
                                        ArrivalEntry {
                                            tag: entry.tag.clone(),
                                            arrival_ns: entry.arrival_ns + d_arc,
                                            origin: ArrivalOrigin::CombArc {
                                                from_pin: u,
                                                from_tr: in_tr,
                                                to_pin: v,
                                                arc_delay_ns: d_arc,
                                            },
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Propagates borrowing late arrivals across a transparent latch `D -> Q` arc into the
    /// opening-edge tag `tag_open = (C, e_open)` (`STA-REQ-0221`, `0222`).
    fn propagate_latch_dq(
        &mut self,
        inst_idx: usize,
        cdef: &CellDef,
        latch: &LatchDef,
        ca: &CombArc,
        d_node: usize,
        q_node: usize,
    ) -> Result<(), Unsupported> {
        let inst = &self.design.instances[inst_idx];
        let Some(&g_node) = inst.pin_nodes.get(&latch.enable_pin) else {
            return Ok(());
        };
        if self.consts.pin_vals[g_node] != LogicVal::X {
            return Ok(());
        }
        let Some(g_waves) = self.clk_waves.get(&g_node).cloned() else {
            return Ok(());
        };

        // Find the closing-edge setup check arc on D.
        let close_edge = latch.close_edge();
        let open_edge = latch.open_edge();
        let setup_chk = cdef.check_arcs.iter().find(|chk| {
            chk.data_pin == latch.data_pin
                && chk.clock_pin == latch.enable_pin
                && chk.kind.is_setup_like()
                && chk.clock_edge == close_edge
        });

        for w_close in &g_waves {
            if w_close.pin_edge != close_edge {
                continue;
            }
            // Find matching opening wave from the same clock source.
            let Some(w_open) = g_waves.iter().find(|w| {
                w.clock == w_close.clock
                    && w.source_port == w_close.source_port
                    && w.pin_edge == open_edge
            }) else {
                continue;
            };
            let Some(cap_clk) = self.sdc.clocks.get(&w_close.clock) else {
                continue;
            };
            let pulse_w = super::checks_eval::pulse_width_ns(
                cap_clk,
                w_open.source_edge,
                w_close.source_edge,
            );
            let tag_open = Tag {
                clock: w_open.clock.clone(),
                edge: w_open.source_edge,
            };
            let d_arrs = self.arrivals[d_node].clone();

            for in_tr in Trans::ALL {
                let Some(s_d) = self.slews[d_node][in_tr.idx()][Analysis::Late.idx()] else {
                    continue;
                };
                let s_g = cap_clk.transition(close_edge, Analysis::Late);
                let s_setup = setup_chk
                    .and_then(|chk| chk.constraint_opt(in_tr, s_d, s_g, 0.0))
                    .unwrap_or(0.0);

                for entry in &d_arrs[in_tr.idx()][Analysis::Late.idx()] {
                    let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                        continue;
                    };
                    let (d_close, _) = super::checks_eval::clock_relations_ns(
                        launch_clk,
                        entry.tag.edge,
                        cap_clk,
                        w_close.source_edge,
                    )?;
                    let d_open = d_close - pulse_w;
                    let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Early);
                    let u_s = self.sdc.clock_uncertainty(
                        &entry.tag.clock,
                        &w_close.clock,
                        Analysis::Late,
                    );
                    let r_open = d_open + lat_c - u_s;
                    let r_close_unclamped = d_close + lat_c - s_setup - u_s;
                    let r_close = match self.sdc.max_time_borrow_for(
                        &inst.name,
                        &self.design.nodes[d_node].name,
                        &w_close.clock,
                    ) {
                        Some(b) => r_close_unclamped.min(r_open + b),
                        None => r_close_unclamped,
                    };

                    // STA-REQ-0222: only late arrivals with `a > R_open` borrow and pass D -> Q.
                    if entry.arrival_ns <= r_open {
                        continue;
                    }
                    let borrowed_entry = entry.arrival_ns.min(r_close) - d_open;
                    for out_tr in Trans::ALL {
                        if !ca.can_propagate(&ca.sense, in_tr, out_tr) {
                            continue;
                        }
                        let c_q = self.node_load_pf(q_node, out_tr, Analysis::Late);
                        let Some((dq_delay, _)) = ca.eval(out_tr, s_d, c_q) else {
                            continue;
                        };
                        self.merge_arrival(
                            q_node,
                            out_tr,
                            Analysis::Late,
                            ArrivalEntry {
                                tag: tag_open.clone(),
                                arrival_ns: borrowed_entry + dq_delay,
                                origin: ArrivalOrigin::LatchBorrow {
                                    d_pin: d_node,
                                    g_pin: g_node,
                                    q_pin: q_node,
                                    src_tag: entry.tag.clone(),
                                    src_tr: in_tr,
                                    borrowed_entry_ns: borrowed_entry,
                                    dq_delay_ns: dq_delay,
                                },
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn merge_arrival(
        &mut self,
        node_idx: usize,
        tr: Trans,
        an: Analysis,
        entry: ArrivalEntry,
    ) {
        let vec = &mut self.arrivals[node_idx][tr.idx()][an.idx()];
        if let Some(existing) = vec.iter_mut().find(|e| e.tag == entry.tag) {
            let replace = if an == Analysis::Late {
                entry.arrival_ns > existing.arrival_ns
            } else {
                entry.arrival_ns < existing.arrival_ns
            };
            if replace {
                *existing = entry;
            }
        } else {
            vec.push(entry);
        }
    }
}

/// Tarjan's strongly-connected components algorithm in canonical node order.
fn tarjan_sccs(n: usize, adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct Env<'a> {
        adj: &'a [Vec<usize>],
        index: usize,
        indices: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }

    fn strongconnect(u: usize, env: &mut Env<'_>) {
        env.indices[u] = Some(env.index);
        env.lowlink[u] = env.index;
        env.index += 1;
        env.stack.push(u);
        env.on_stack[u] = true;

        for &v in &env.adj[u] {
            if env.indices[v].is_none() {
                strongconnect(v, env);
                env.lowlink[u] = env.lowlink[u].min(env.lowlink[v]);
            } else if env.on_stack[v] {
                env.lowlink[u] = env.lowlink[u].min(env.indices[v].unwrap());
            }
        }

        if Some(env.lowlink[u]) == env.indices[u] {
            let mut scc = Vec::new();
            while let Some(w) = env.stack.pop() {
                env.on_stack[w] = false;
                scc.push(w);
                if w == u {
                    break;
                }
            }
            env.sccs.push(scc);
        }
    }

    let mut env = Env {
        adj,
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for u in 0..n {
        if env.indices[u].is_none() {
            strongconnect(u, &mut env);
        }
    }
    env.sccs
}
