// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The gate delay seam: delay and output slew of a cell arc (owner: E3).
//!
//! Engine section B. Work item E3 implements [`GateDelay`] with the non-linear delay model of the
//! library tables. The load is the wire model's [`DriverLoad`]: a lumped capacitance or
//! a physical pi reduced through the bounded effective-capacitance/waveform model. The bound selects the library of a corner that has
//! separate early and late libraries (Phase 2b).

use std::borrow::Cow;
use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use rusteda_compute::timing::{
    CPU, InArc, LevelizedTimingCsr, NONE, StartRows, TimingPropagationBackend, TimingStateSoA,
    TimingValues, absent, out_adjacency,
};
use rusteda_core::netlist::FrozenNetlist;

use super::load::{NetLoads, map_netlist_cell_pins};
use super::lut::RangeTracker;
use super::wire::DriverLoad;
use crate::engine::input::libview::{PropArcKind, TimingCell, TimingLib};
use crate::engine::types::{An, DelaySlew, LibArc, Tr};

/// Delay calculation of cell arcs.
pub trait GateDelay {
    /// The delay (ns) of the library arc `arc` and the slew (ns) at the arc's output, for the
    /// output transition `out` and the bound `an`, when the arc's input has slew `slew` (ns) and
    /// its output drives `load`.
    fn arc_delay(&self, arc: LibArc, out: Tr, an: An, slew: f64, load: DriverLoad) -> DelaySlew;
}

impl GateDelay for TimingLib {
    #[inline]
    fn arc_delay(&self, arc: LibArc, out: Tr, _an: An, slew: f64, load: DriverLoad) -> DelaySlew {
        self.arc_delay_3d(arc, out, slew, load, 0.0)
    }
}

impl TimingLib {
    /// Evaluates the delay and output slew of `arc` for output transition `out`, input slew
    /// `slew` (ns), output `load`, and an optional third-axis argument `axis3` (`STA-REQ-0108`,
    /// `STA-REQ-0109`, `STA-REQ-0113`, `STA-REQ-0114`).
    #[inline]
    #[must_use]
    pub fn arc_delay_3d(
        &self,
        arc: LibArc,
        out: Tr,
        slew: f64,
        load: DriverLoad,
        axis3: f64,
    ) -> DelaySlew {
        let pa = self.arc(arc);
        if let DriverLoad::Pi { c_near, c_far, r } = load {
            let eval = |c| {
                let x = [slew, c, axis3];
                (
                    self.lut_bank.eval(pa.delay[out.index()], x),
                    self.lut_bank.eval(pa.slew[out.index()], x),
                )
            };
            return match rusteda_compute::rc::gate(
                rusteda_compute::rc::Pi {
                    near: c_near,
                    far: c_far,
                    resistance: r,
                },
                self.thresholds.rc(out == Tr::Fall),
                eval,
            ) {
                Ok((delay, slew, _)) => DelaySlew { delay, slew },
                Err(_) => DelaySlew {
                    delay: f64::NAN,
                    slew: f64::NAN,
                },
            };
        }
        let x = [slew, load.total(), axis3];
        let k = out.index();
        DelaySlew {
            delay: self.lut_bank.eval(pa.delay[k], x),
            slew: self.lut_bank.eval(pa.slew[k], x),
        }
    }

    /// Evaluates the delay and output slew of `arc` while recording any table extrapolation in
    /// `tracker` (`STA-REQ-0120`).
    #[inline]
    pub fn arc_delay_tracked(
        &self,
        arc: LibArc,
        out: Tr,
        slew: f64,
        load: DriverLoad,
        axis3: f64,
        tracker: &mut RangeTracker,
    ) -> DelaySlew {
        let pa = self.arc(arc);
        if let DriverLoad::Pi { c_near, c_far, r } = load {
            let eval = |c| {
                let x = [slew, c, axis3];
                (
                    self.lut_bank
                        .eval_tracked(pa.delay[out.index()], x, tracker),
                    self.lut_bank.eval_tracked(pa.slew[out.index()], x, tracker),
                )
            };
            return match rusteda_compute::rc::gate(
                rusteda_compute::rc::Pi {
                    near: c_near,
                    far: c_far,
                    resistance: r,
                },
                self.thresholds.rc(out == Tr::Fall),
                eval,
            ) {
                Ok((delay, slew, _)) => DelaySlew { delay, slew },
                Err(_) => DelaySlew {
                    delay: f64::NAN,
                    slew: f64::NAN,
                },
            };
        }
        let x = [slew, load.total(), axis3];
        let k = out.index();
        DelaySlew {
            delay: self.lut_bank.eval_tracked(pa.delay[k], x, tracker),
            slew: self.lut_bank.eval_tracked(pa.slew[k], x, tracker),
        }
    }
}

/// Non-linear delay model (NLDM) gate delay evaluator backed by one [`TimingLib`] per analysis
/// bound (`Early` and `Late`).
#[derive(Debug, Clone, Copy)]
pub struct NldmGateDelay<'a> {
    libs: [&'a TimingLib; 2],
}

impl<'a> NldmGateDelay<'a> {
    /// Creates an [`NldmGateDelay`] using the same [`TimingLib`] for both early and late bounds.
    #[must_use]
    pub fn new(lib: &'a TimingLib) -> Self {
        Self { libs: [lib, lib] }
    }

    /// Creates an [`NldmGateDelay`] with separate early and late [`TimingLib`] views.
    #[must_use]
    pub fn with_early_late(early: &'a TimingLib, late: &'a TimingLib) -> Self {
        Self {
            libs: [early, late],
        }
    }

    /// Returns the [`TimingLib`] for bound `an`.
    #[inline]
    #[must_use]
    pub fn lib(&self, an: An) -> &'a TimingLib {
        self.libs[an.index()]
    }

    /// Evaluates the 3-axis delay and output slew of `arc` for bound `an`.
    #[inline]
    #[must_use]
    pub fn arc_delay_3d(
        &self,
        arc: LibArc,
        out: Tr,
        an: An,
        slew: f64,
        load: DriverLoad,
        axis3: f64,
    ) -> DelaySlew {
        self.libs[an.index()].arc_delay_3d(arc, out, slew, load, axis3)
    }

    /// Evaluates the 3-axis delay and output slew of `arc` for bound `an`, recording any table
    /// extrapolation in `tracker`.
    #[inline]
    pub fn arc_delay_tracked(
        &self,
        arc: LibArc,
        out: Tr,
        an: An,
        slew: f64,
        load: DriverLoad,
        axis3: f64,
        tracker: &mut RangeTracker,
    ) -> DelaySlew {
        self.libs[an.index()].arc_delay_tracked(arc, out, slew, load, axis3, tracker)
    }
}

impl GateDelay for NldmGateDelay<'_> {
    #[inline]
    fn arc_delay(&self, arc: LibArc, out: Tr, an: An, slew: f64, load: DriverLoad) -> DelaySlew {
        self.libs[an.index()].arc_delay(arc, out, an, slew, load)
    }
}

/// Levelized netlist propagation harness exercising `rusteda_compute::timing::CPU` (`STA-REQ-0108`
/// to `STA-REQ-0121`, `COMPUTE-REQ-0011` to `COMPUTE-REQ-0019`).
#[derive(Debug, Clone)]
pub struct NetlistTiming<'a> {
    /// Compiled levelized CSR graph for `rusteda_compute::timing`.
    pub graph: LevelizedTimingCsr<'static>,
    /// Level-ordered node index lookup by port or `inst/pin` (`inst.pin`) name.
    pub node_by_name: FxHashMap<String, u32>,
    /// Canonical name of each level-ordered node in `0..graph.nodes()`.
    pub node_names: Vec<String>,
    /// Net index of each level-ordered node in `0..graph.nodes()`.
    pub node_net: Vec<u32>,
    /// Optional `max_transition` limit (in ns) and construct label per level-ordered node.
    pub node_max_transition: Vec<Option<(String, f64)>>,
    /// Reference to the corner's [`TimingLib`].
    pub lib: &'a TimingLib,
}

impl<'a> NetlistTiming<'a> {
    /// Builds a [`NetlistTiming`] from `netlist` and `lib`.
    #[must_use]
    pub fn build(netlist: &FrozenNetlist<'_>, lib: &'a TimingLib) -> Self {
        let n_port_bits = netlist.port_bit_net.len();
        let n_pins = netlist.pin_net.len();
        let n_nodes = n_port_bits + n_pins;
        let n_nets = netlist.nets.len();

        let cell_map: Vec<Option<&TimingCell>> = netlist
            .cells
            .iter()
            .map(|&s| netlist.string(s).ok().and_then(|n| lib.cell(n)))
            .collect();
        let cell_pin_map = map_netlist_cell_pins(netlist, lib);

        let mut raw_names = vec![String::new(); n_nodes];
        let mut raw_aliases: Vec<(String, usize)> = Vec::new();
        let mut raw_net = vec![u32::MAX; n_nodes];
        let mut raw_max_tr = vec![None; n_nodes];

        // Port bits: raw nodes `0..n_port_bits`.
        for (p_idx, port) in netlist.ports.iter().enumerate() {
            let pname = netlist.string(port.name).unwrap_or("");
            let bits = netlist.port_bits(p_idx as u32);
            let step = if port.msb >= port.lsb { -1 } else { 1 };
            for (k, &net_id) in bits.iter().enumerate() {
                let pb_node = netlist.port_bit_start[p_idx] as usize + k;
                let label = if port.is_bus == 1 {
                    let bit = port.msb + (k as i32) * step;
                    format!("{pname}[{bit}]")
                } else {
                    pname.to_string()
                };
                raw_names[pb_node] = label.clone();
                raw_aliases.push((label, pb_node));
                raw_net[pb_node] = net_id;
            }
        }

        // Instance pins: raw nodes `n_port_bits..n_nodes`.
        for (pin_row, &net_id) in netlist.pin_net.iter().enumerate() {
            let v = n_port_bits + pin_row;
            let inst_idx = netlist.pin_instance(pin_row as u32) as usize;
            let inst = &netlist.instances[inst_idx];
            let iname = netlist.string(inst.name).unwrap_or("");
            let nl_cell = inst.cell as usize;
            let nl_cp = netlist.pin_cellpin[pin_row] as usize;
            let cell_pin_range = netlist.cell_pins(nl_cell as u32);
            let pname = netlist
                .string(netlist.cell_pin_name[cell_pin_range.start + nl_cp])
                .unwrap_or("");
            let slash = format!("{iname}/{pname}");
            let dot = format!("{iname}.{pname}");
            raw_names[v] = slash.clone();
            raw_aliases.push((slash, v));
            raw_aliases.push((dot, v));
            raw_net[v] = net_id;

            if let Some(cp) = cell_pin_map
                .get(nl_cell)
                .and_then(|vec| vec.get(nl_cp))
                .copied()
                .flatten()
                && let (Some(tc), Some(limit)) = (cell_map[nl_cell], cp.max_transition)
            {
                raw_max_tr[v] = Some((format!("cell {} pin {}", tc.name, cp.name), limit));
            }
        }

        // Collect incoming arcs per raw node `v`: `(raw_src, class)`.
        let mut raw_incoming: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n_nodes];

        // 1. Wire arcs on each net `0..n_nets`.
        let mut net_sources: Vec<Vec<u32>> = vec![Vec::new(); n_nets];
        let mut net_sinks: Vec<Vec<u32>> = vec![Vec::new(); n_nets];
        for (p_idx, port) in netlist.ports.iter().enumerate() {
            let start = netlist.port_bit_start[p_idx] as usize;
            let end = netlist.port_bit_start[p_idx + 1] as usize;
            for pb in start..end {
                let net_id = netlist.port_bit_net[pb] as usize;
                if net_id >= n_nets {
                    continue;
                }
                if port.dir == 0 || port.dir == 2 {
                    net_sources[net_id].push(pb as u32);
                }
                if port.dir == 1 || port.dir == 2 {
                    net_sinks[net_id].push(pb as u32);
                }
            }
        }
        for net_idx in 0..n_nets {
            for &pin_row in netlist.net_pins_of(net_idx as u32) {
                let v = (n_port_bits + pin_row as usize) as u32;
                let inst_idx = netlist.pin_instance(pin_row) as usize;
                let nl_cell = netlist.instances[inst_idx].cell as usize;
                let nl_cp = netlist.pin_cellpin[pin_row as usize] as usize;
                let (is_in, is_out) = cell_pin_map
                    .get(nl_cell)
                    .and_then(|vec| vec.get(nl_cp))
                    .copied()
                    .flatten()
                    .map_or((false, false), |cp| (cp.input, cp.output));
                if is_out {
                    net_sources[net_idx].push(v);
                }
                if is_in {
                    net_sinks[net_idx].push(v);
                }
            }
        }
        for net_idx in 0..n_nets {
            for &src in &net_sources[net_idx] {
                for &dst in &net_sinks[net_idx] {
                    if src != dst {
                        raw_incoming[dst as usize].push((src, lib.wire_class));
                    }
                }
            }
        }
        for assign in netlist.assigns.iter() {
            let (lhs, rhs) = (assign.lhs as usize, assign.rhs as usize);
            if lhs < n_nets && rhs < n_nets && lhs != rhs {
                let srcs = net_sources[rhs].clone();
                let dsts = net_sinks[lhs].clone();
                for src in srcs {
                    for &dst in &dsts {
                        if src != dst {
                            raw_incoming[dst as usize].push((src, lib.wire_class));
                        }
                    }
                }
            }
        }

        // 2. Cell propagation arcs (combinational and three-state).
        for inst_idx in 0..netlist.instances.len() {
            let nl_cell = netlist.instances[inst_idx].cell as usize;
            let Some(tc) = cell_map.get(nl_cell).copied().flatten() else {
                continue;
            };
            let mut tc_pin_node = vec![None; tc.pins.len()];
            for pin_row in netlist.instance_pins(inst_idx as u32) {
                let nl_cp = netlist.pin_cellpin[pin_row] as usize;
                if let Some(cp) = cell_pin_map
                    .get(nl_cell)
                    .and_then(|vec| vec.get(nl_cp))
                    .copied()
                    .flatten()
                    && let Some(tc_pi) = tc.pin(&cp.name)
                {
                    tc_pin_node[tc_pi] = Some((n_port_bits + pin_row) as u32);
                }
            }
            for &lib_arc in &tc.arcs {
                let pa = lib.arc(lib_arc);
                if matches!(pa.kind, PropArcKind::Edge(_)) {
                    continue;
                }
                if let (Some(u), Some(v)) = (tc_pin_node[pa.from], tc_pin_node[pa.to]) {
                    raw_incoming[v as usize].push((u, lib_arc.0));
                }
            }
        }

        // Topological levelization and renumbering in level order.
        let mut level = vec![0u32; n_nodes];
        for _ in 0..n_nodes {
            let mut changed = false;
            for v in 0..n_nodes {
                for &(src, _) in &raw_incoming[v] {
                    let cand = level[src as usize] + 1;
                    if cand > level[v] {
                        level[v] = cand;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let max_level = level.iter().copied().max().unwrap_or(0) as usize;
        let mut raw_order: Vec<usize> = (0..n_nodes).collect();
        raw_order.sort_by_key(|&v| (level[v], v));
        let mut pos_of_raw = vec![0u32; n_nodes];
        for (pos, &raw_v) in raw_order.iter().enumerate() {
            pos_of_raw[raw_v] = pos as u32;
        }

        let mut level_ptr = Vec::with_capacity(max_level + 2);
        let mut cur = 0usize;
        for lv in 0..=max_level {
            level_ptr.push(cur as u32);
            while cur < raw_order.len() && level[raw_order[cur]] as usize == lv {
                cur += 1;
            }
        }
        level_ptr.push(raw_order.len() as u32);

        let mut in_ptr = Vec::with_capacity(n_nodes + 1);
        let mut in_arcs = Vec::new();
        let mut node_names = Vec::with_capacity(n_nodes);
        let mut node_net = Vec::with_capacity(n_nodes);
        let mut node_max_transition = Vec::with_capacity(n_nodes);

        for &raw_v in &raw_order {
            in_ptr.push(in_arcs.len() as u32);
            for &(raw_src, class) in &raw_incoming[raw_v] {
                in_arcs.push(InArc {
                    src: pos_of_raw[raw_src as usize],
                    class,
                    map: NONE,
                });
            }
            node_names.push(raw_names[raw_v].clone());
            node_net.push(raw_net[raw_v]);
            node_max_transition.push(raw_max_tr[raw_v].clone());
        }
        in_ptr.push(in_arcs.len() as u32);

        let (out_ptr, out_dst) = out_adjacency(n_nodes, &in_ptr, &in_arcs);
        let mut node_by_name = FxHashMap::default();
        for (alias, raw_v) in raw_aliases {
            node_by_name.insert(alias, pos_of_raw[raw_v]);
        }

        let graph = LevelizedTimingCsr {
            level_ptr: Cow::Owned(level_ptr),
            in_ptr: Cow::Owned(in_ptr),
            in_arcs: Cow::Owned(in_arcs),
            out_ptr: Cow::Owned(out_ptr),
            out_dst: Cow::Owned(out_dst),
            map_ptr: Cow::Owned(vec![0]),
            maps: Cow::Owned(Vec::new()),
        };

        Self {
            graph,
            node_by_name,
            node_names,
            node_net,
            node_max_transition,
            lib,
        }
    }

    /// Looks up a level-ordered node index by port name or `inst/pin` (`inst.pin`) name.
    #[must_use]
    pub fn node(&self, name: &str) -> Option<u32> {
        self.node_by_name.get(name).copied()
    }

    /// Runs `rusteda_compute::timing::CPU` propagation with `ntags` tags, `net_loads`, and
    /// `starts`.
    #[must_use]
    pub fn propagate(&self, ntags: u32, net_loads: &NetLoads, starts: StartRows) -> TimingValues {
        let load: Vec<[f64; 4]> = self
            .node_net
            .iter()
            .map(|&net_id| {
                net_loads
                    .totals
                    .get(net_id as usize)
                    .map_or([0.0; 4], |t| [t[0][0], t[0][1], t[1][0], t[1][1]])
            })
            .collect();
        let mut state = TimingStateSoA::new(ntags, load, starts);
        CPU.propagate_levels(
            &self.graph,
            &self.lib.lut_bank.as_compiled(),
            &mut state,
            true,
        )
        .expect("valid timing problem");
        state.values()
    }

    /// Scans all active transitions and arc evaluations in `values` to populate `tracker` with
    /// aggregated table extrapolation (`T0016`) and `max_transition` (`T0017`) warnings
    /// (`STA-REQ-0120`).
    pub fn track_ranges(
        &self,
        values: &TimingValues,
        net_loads: &NetLoads,
        tracker: &mut RangeTracker,
    ) {
        for v in 0..self.graph.nodes() {
            if let Some((ref construct, limit)) = self.node_max_transition[v] {
                for tr in Tr::ALL {
                    for an in An::ALL {
                        let s = values.transition(v, tr.index(), an.index());
                        if s != absent(an.index()) {
                            tracker.check_max_transition(construct, s, limit);
                        }
                    }
                }
            }
            let v_net = self.node_net[v] as usize;
            for arc in self.graph.incoming(v) {
                if arc.class == self.lib.wire_class {
                    continue;
                }
                let lib_arc = LibArc(arc.class);
                let pa = self.lib.arc(lib_arc);
                for ti in Tr::ALL {
                    let outs = pa.out_transitions(ti);
                    for to in Tr::ALL {
                        if !outs[to.index()] {
                            continue;
                        }
                        for an in An::ALL {
                            let s_in = values.transition(arc.src as usize, ti.index(), an.index());
                            if s_in == absent(an.index()) {
                                continue;
                            }
                            let l = net_loads
                                .totals
                                .get(v_net)
                                .map_or(0.0, |t| t[to.index()][an.index()]);
                            let _ = self.lib.arc_delay_tracked(
                                lib_arc,
                                to,
                                s_in,
                                DriverLoad::Lumped { c: l },
                                0.0,
                                tracker,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Builder for [`StartRows`] that accepts start arrivals and slews in any node order and produces
/// rows ordered by node index as required by `rusteda_compute::timing`.
#[derive(Debug, Clone)]
pub struct StartBuilder {
    ntags: u32,
    rows: BTreeMap<u32, (Vec<f64>, [f64; 4])>,
}

impl StartBuilder {
    /// Creates a [`StartBuilder`] for `ntags` tags.
    #[must_use]
    pub fn new(ntags: u32) -> Self {
        Self {
            ntags,
            rows: BTreeMap::new(),
        }
    }

    fn entry(&mut self, node: u32) -> &mut (Vec<f64>, [f64; 4]) {
        let w = self.ntags as usize * 4;
        self.rows.entry(node).or_insert_with(|| {
            let arr = (0..w).map(|i| absent(i % 2)).collect();
            let slew = [absent(0), absent(1), absent(0), absent(1)];
            (arr, slew)
        })
    }

    /// Sets tagged arrival `[early, late]` and transition `[early, late]` on `node` for `tag` and
    /// transition `tr`.
    pub fn set_tagged(&mut self, node: u32, tag: u32, tr: Tr, arr: [f64; 2], slew: [f64; 2]) {
        let (a, s) = self.entry(node);
        let ti = tr.index();
        let base = (tag as usize * 2 + ti) * 2;
        a[base] = arr[0];
        a[base + 1] = arr[1];
        s[ti * 2] = slew[0];
        s[ti * 2 + 1] = slew[1];
    }

    /// Sets untagged transition `[early, late]` on `node` for transition `tr` while leaving its
    /// arrivals absent, so the node's transition contributes to downstream per-pin slews
    /// (`STA-REQ-0115`).
    pub fn set_untagged_slew(&mut self, node: u32, tr: Tr, slew: [f64; 2]) {
        let (_, s) = self.entry(node);
        let ti = tr.index();
        s[ti * 2] = slew[0];
        s[ti * 2 + 1] = slew[1];
    }

    /// Finalizes the [`StartRows`] sorted by node index.
    #[must_use]
    pub fn build(self) -> StartRows {
        let mut out = StartRows::default();
        for (node, (arr, slew)) in self.rows {
            out.push(node, &arr, &slew);
        }
        out
    }
}
