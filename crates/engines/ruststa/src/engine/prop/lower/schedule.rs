// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Ideal clock and input-delay schedules for lowering.

use std::collections::HashMap;

use rusteda_core::job::Diag;
use rusteda_core::liberty::Direction;

use super::{
    ArcTagMaps, GraphStartBuilder, LoweredCorner, StartRows, TagMapRec, canonical_net_totals,
    map_cell_arcs,
};
use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::DriverLoad;
use crate::engine::graph::{ArcKind, ArcSense, TimingGraph};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::types::{An, NodeId, Tr};

/// Ideal clock specification for seeding start rows and latch tag maps (`STA-REQ-0200`,
/// `STA-REQ-0201`, `STA-REQ-0213`).
#[derive(Debug, Clone)]
pub struct IdealClockSpec {
    /// Clock name.
    pub name: String,
    /// Clock period in ns (`> 0.0`).
    pub period: f64,
    /// Waveform `[rise_edge_ns, fall_edge_ns]` within `[0, period)`.
    pub waveform: [f64; 2],
    /// Source port/pin [`NodeId`]s of the clock.
    pub sources: Vec<NodeId>,
    /// Clock latency `[early_ns, late_ns]`.
    pub latency: [f64; 2],
    /// Clock transition `[early_ns, late_ns]`.
    pub transition: [f64; 2],
    /// Clock uncertainty `[setup_ns, hold_ns]`.
    pub uncertainty: [f64; 2],
}

impl IdealClockSpec {
    /// Creates an ideal clock with default 50% duty cycle `[0.0, period / 2.0]` and zero
    /// latency, transition and uncertainty.
    #[must_use]
    pub fn new(name: impl Into<String>, period: f64, sources: Vec<NodeId>) -> Self {
        Self {
            name: name.into(),
            period,
            waveform: [0.0, period * 0.5],
            sources,
            latency: [0.0, 0.0],
            transition: [0.0, 0.0],
            uncertainty: [0.0, 0.0],
        }
    }
}

/// Input delay specification on a primary input port node (`STA-REQ-0201`, `STA-REQ-0217`).
#[derive(Debug, Clone, Copy)]
pub struct InputDelaySpec {
    /// Target input port [`NodeId`].
    pub node: NodeId,
    /// Index of the launch clock in [`IdealSchedule::clocks`].
    pub clock: u32,
    /// Active launch clock source edge (`Rise` or `Fall`).
    pub edge: Tr,
    /// Input arrival `[early_ns, late_ns]` relative to the clock edge (including clock latency).
    pub delay: [f64; 2],
    /// Input pin transition `[early_ns, late_ns]`.
    pub transition: [f64; 2],
}

/// Ideal clock and I/O schedule used to seed [`StartRows`] and latch [`ArcTagMaps`] on a
/// [`TimingGraph`] (`STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0213`).
#[derive(Debug, Clone, Default)]
pub struct IdealSchedule {
    /// Ideal clocks in definition order. Tag `(clock_idx, edge)` is `clock_idx * 2 + edge.index()`.
    pub clocks: Vec<IdealClockSpec>,
    /// Input delays on primary input port nodes.
    pub input_delays: Vec<InputDelaySpec>,
    /// Untagged input transitions `(node, [early_ns, late_ns])` on unconstrained input ports
    /// (`STA-REQ-0115`).
    pub untagged_transitions: Vec<(NodeId, [f64; 2])>,
    /// Optional per-latch `set_max_time_borrow` override in ns keyed by latch `D -> Q` graph arc
    /// index (`STA-REQ-0221`).
    pub max_time_borrow: HashMap<u32, f64>,
}

impl IdealSchedule {
    /// Builds a default corpus schedule for `netview` and `graph`:
    /// - Any input port named `clk`, `clk_i`, `clock`, `ck`, `en`, `en_n`, `g`, `gate` becomes a
    ///   clock (or a virtual clock `vclk` of period `1.5 ns` is created on the first input port if
    ///   no clock-named port exists);
    /// - Every non-clock input port gets `set_input_delay 0.2` and `set_input_transition 0.03` on
    ///   the first clock.
    #[must_use]
    pub fn default_for_netlist(netview: &NetlistView<'_>) -> Self {
        let mut port_by_name: HashMap<String, NodeId> = HashMap::new();
        let mut input_nodes = Vec::new();
        for i in 0..netview.num_port_bits() {
            let node = NodeId(i as u32);
            if let NodeKind::PortBit { dir, .. } = netview.node_kind(node)
                && matches!(dir, Direction::Input | Direction::Inout)
            {
                let name = netview.node_name_str(node).into_owned();
                port_by_name.insert(name, node);
                input_nodes.push(node);
            }
        }

        let candidates = [
            ("clk", 1.2, [0.0, 0.6]),
            ("clk_i", 1.2, [0.0, 0.6]),
            ("clock", 1.2, [0.0, 0.6]),
            ("ck", 1.2, [0.0, 0.6]),
            ("en", 2.0, [0.0, 0.7]),
            ("en_n", 2.0, [1.0, 1.8]),
            ("g", 2.0, [0.0, 1.0]),
            ("gate", 2.0, [0.0, 1.0]),
        ];

        let mut clocks = Vec::new();
        let mut clock_port_set = std::collections::HashSet::new();
        for (name, period, waveform) in candidates {
            if let Some(&node) = port_by_name.get(name) {
                let mut ck = IdealClockSpec::new(name, period, vec![node]);
                ck.waveform = waveform;
                ck.transition = [0.03, 0.03];
                clocks.push(ck);
                clock_port_set.insert(node);
            }
        }

        if clocks.is_empty() {
            let sources = input_nodes.first().copied().into_iter().collect();
            let mut vclk = IdealClockSpec::new("vclk", 1.5, sources);
            vclk.transition = [0.03, 0.03];
            clocks.push(vclk);
        }

        let mut input_delays = Vec::new();
        for &node in &input_nodes {
            input_delays.push(InputDelaySpec {
                node,
                clock: 0,
                edge: Tr::Rise,
                delay: [0.2, 0.2],
                transition: [0.03, 0.03],
            });
            let _ = clock_port_set.contains(&node);
        }

        Self {
            clocks,
            input_delays,
            untagged_transitions: Vec::new(),
            max_time_borrow: HashMap::new(),
        }
    }

    /// Lowers `netview`, `graph`, `lib` and `net_loads` with the start rows and latch tag maps
    /// produced by this schedule (`STA-REQ-0200`, `STA-REQ-0201`, `STA-REQ-0213`, `STA-REQ-0401`).
    pub fn lower<'a>(
        &self,
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &'a TimingLib,
        net_loads: &NetLoads,
    ) -> Result<LoweredCorner<'a>, Diag> {
        let (ntags, starts, tag_maps) =
            self.build_starts_and_maps(netview, graph, lib, net_loads)?;
        LoweredCorner::lower(netview, graph, lib, net_loads, ntags, starts, &tag_maps)
    }

    /// Computes `(ntags, StartRows, ArcTagMaps)` for this schedule.
    pub fn build_starts_and_maps(
        &self,
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &TimingLib,
        net_loads: &NetLoads,
    ) -> Result<(u32, StartRows, ArcTagMaps), Diag> {
        let n_nodes = graph.num_nodes();
        let ntags = (2 * self.clocks.len().max(1)) as u32;
        let cell_arcs = map_cell_arcs(netview, lib)?;
        let canon_totals = canonical_net_totals(netview, net_loads);

        // 1. Propagate ideal clock sense bits (bit 0 positive, bit 1 negative) across enabled
        // net and combinational/tristate arcs, skipping latch data arcs (STA-REQ-0200).
        let mut clock_sense: Vec<Vec<(u32, u8)>> = vec![Vec::new(); n_nodes];
        let g_arcs = graph.arcs();
        for (ci, ck) in self.clocks.iter().enumerate() {
            let mut bits = vec![0u8; n_nodes];
            let mut stack = Vec::new();
            for &src in &ck.sources {
                if src.index() < n_nodes {
                    bits[src.index()] |= 1;
                    stack.push((src, 1u8));
                }
            }
            while let Some((v, b)) = stack.pop() {
                for &ai in graph.outgoing(v) {
                    let arc = &g_arcs[ai as usize];
                    if arc.kind.is_latch_data() {
                        continue;
                    }
                    let nb = match arc.kind {
                        ArcKind::Net { .. } => b,
                        _ => match arc.sense {
                            ArcSense::PositiveUnate => b,
                            ArcSense::NegativeUnate => ((b & 1) << 1) | ((b >> 1) & 1),
                            ArcSense::NonUnate => 3,
                        },
                    };
                    let w = arc.dst.index();
                    if (bits[w] | nb) != bits[w] {
                        bits[w] |= nb;
                        stack.push((arc.dst, bits[w]));
                    }
                }
            }
            for (v, &b) in bits.iter().enumerate() {
                if b != 0 {
                    clock_sense[v].push((ci as u32, b));
                }
            }
        }

        // 2. Seed startpoint rows (clock-as-data, input delays, untagged input slews, and
        // sequential CK -> Q launches).
        let mut starts = GraphStartBuilder::new(ntags);

        for (ci, ck) in self.clocks.iter().enumerate() {
            for &src in &ck.sources {
                if src.index() >= n_nodes {
                    continue;
                }
                for edge in Tr::ALL {
                    let t = ci as u32 * 2 + edge.index() as u32;
                    for an in An::ALL {
                        starts.merge_bound(
                            graph,
                            src,
                            t,
                            edge,
                            an,
                            ck.latency[an.index()],
                            ck.transition[an.index()],
                        );
                    }
                }
            }
        }

        for d in &self.input_delays {
            if d.node.index() >= n_nodes {
                continue;
            }
            let t = d.clock * 2 + d.edge.index() as u32;
            if t >= ntags {
                continue;
            }
            for tr in Tr::ALL {
                for an in An::ALL {
                    starts.merge_bound(
                        graph,
                        d.node,
                        t,
                        tr,
                        an,
                        d.delay[an.index()],
                        d.transition[an.index()],
                    );
                }
            }
        }

        for &(node, s) in &self.untagged_transitions {
            if node.index() < n_nodes {
                for tr in Tr::ALL {
                    for an in An::ALL {
                        starts.merge_untagged_slew(graph, node, tr, an, s[an.index()]);
                    }
                }
            }
        }

        for (li, l) in graph.launches().iter().enumerate() {
            if graph.is_launch_disabled(li) {
                continue;
            }
            let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
            let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
            let net_cap = match netview.node_net(l.out_pin) {
                NetTarget::Net(nr) => canon_totals.get(nr.index()).copied().unwrap_or_default(),
                NetTarget::Const(_) => [[0.0; 2]; 2],
            };
            for &(c, bits) in &clock_sense[l.clock_pin.index()] {
                let ck = &self.clocks[c as usize];
                for src_edge in source_edges(l.edge, bits) {
                    let t = c * 2 + src_edge.index() as u32;
                    for to in Tr::ALL {
                        for an in An::ALL {
                            let c_load = net_cap[to.index()][an.index()];
                            let ds = lib.arc_delay(
                                global_arc,
                                to,
                                an,
                                ck.transition[an.index()],
                                DriverLoad::Lumped { c: c_load },
                            );
                            starts.merge_bound(
                                graph,
                                l.out_pin,
                                t,
                                to,
                                an,
                                ck.latency[an.index()] + ds.delay,
                                ds.slew,
                            );
                        }
                    }
                }
            }
        }

        // 3. Build TagMapRec windows for transparent latch D -> Q arcs (STA-REQ-0213,
        // COMPUTE-REQ-0012, COMPUTE-REQ-0018).
        let mut tag_maps = ArcTagMaps::new();
        let g_checks = graph.checks();
        let g_launches = graph.launches();

        for (ai, arc) in g_arcs.iter().enumerate() {
            if graph.is_arc_disabled(ai) {
                continue;
            }
            let ArcKind::LatchData {
                inst, setup_check, ..
            } = arc.kind
            else {
                continue;
            };
            let Some(ch) = g_checks.get(setup_check as usize) else {
                continue;
            };
            let open_pin_edge = g_launches
                .iter()
                .find(|l| l.inst == inst && l.clock_pin == ch.clock_pin && l.out_pin == arc.dst)
                .map_or_else(|| ch.edge.opposite(), |l| l.edge);

            let close_slew = clock_sense[ch.clock_pin.index()]
                .first()
                .map_or([0.0, 0.0], |&(c, _)| self.clocks[c as usize].transition);
            let max_borrow = self.max_time_borrow.get(&(ai as u32)).copied();

            let mut recs = Vec::new();
            for src_tag in 0..ntags {
                let (l_idx, el_idx) = ((src_tag / 2) as usize, (src_tag % 2) as usize);
                if l_idx >= self.clocks.len() {
                    continue;
                }
                let ck_l = &self.clocks[l_idx];
                let e_l = if el_idx == 0 { Tr::Rise } else { Tr::Fall };

                let mut best_close = f64::NEG_INFINITY;
                let mut best_open = f64::INFINITY;
                for &(c, bits) in &clock_sense[ch.clock_pin.index()] {
                    let ck_c = &self.clocks[c as usize];
                    for ec in source_edges(ch.edge, bits) {
                        if let Some(ds) = setup_relation_ns(ck_l, e_l, ck_c, ec) {
                            best_close = best_close
                                .max(ds + ck_c.latency[An::Late.index()] - ck_c.uncertainty[0]);
                        }
                    }
                    for eo in source_edges(open_pin_edge, bits) {
                        if let Some(do_rel) = open_relation_ns(ck_l, e_l, ck_c, eo) {
                            best_open = best_open.min(
                                do_rel + ck_c.latency[An::Early.index()] - ck_c.uncertainty[0],
                            );
                        }
                    }
                }

                let open_arr = if best_open < f64::INFINITY {
                    best_open
                } else {
                    f64::NEG_INFINITY
                };
                let close_arr = if best_close > f64::NEG_INFINITY {
                    best_close
                } else {
                    f64::INFINITY
                };
                let limit_arr = match (max_borrow, best_open < f64::INFINITY) {
                    (Some(b), true) => best_open + b,
                    _ => f64::INFINITY,
                };

                if open_arr > f64::NEG_INFINITY || close_arr < f64::INFINITY {
                    recs.push(TagMapRec {
                        src_tag,
                        dst_tag: src_tag,
                        shift: 0.0,
                        open: [open_arr, f64::NEG_INFINITY],
                        close: [f64::INFINITY, close_arr],
                        close_slew,
                        limit: [f64::INFINITY, limit_arr],
                    });
                } else {
                    recs.push(TagMapRec::through(src_tag, src_tag));
                }
            }
            if !recs.is_empty() {
                tag_maps.insert(ai as u32, recs);
            }
        }

        Ok((ntags, starts.build(), tag_maps))
    }
}

fn source_edges(pin_edge: Tr, bits: u8) -> Vec<Tr> {
    let mut out = Vec::with_capacity(2);
    if bits & 1 != 0 {
        out.push(pin_edge);
    }
    if bits & 2 != 0 {
        out.push(pin_edge.opposite());
    }
    out
}

fn to_fs(ns: f64) -> i64 {
    (ns * 1e6).round() as i64
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.abs().max(1)
}

/// Smallest strictly positive `capture - launch` relation in ns over the common period (`STA-REQ-0220`).
fn setup_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(launch, e_l, capture, e_c, true)
}

/// Smallest non-negative `capture - launch` relation in ns over the common period.
fn open_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(launch, e_l, capture, e_c, false)
}

fn edge_relation_ns(
    launch: &IdealClockSpec,
    e_l: Tr,
    capture: &IdealClockSpec,
    e_c: Tr,
    strict: bool,
) -> Option<f64> {
    let tl = to_fs(launch.period);
    let tc = to_fs(capture.period);
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let g = gcd(tl, tc);
    let mult = tc / g;
    if mult > 1000 {
        return None;
    }
    let wl = to_fs(launch.waveform[e_l.index()]);
    let wc = to_fs(capture.waveform[e_c.index()]);
    let mut best = i64::MAX;
    for k in 0..mult {
        let l = wl + k * tl;
        let min_c = if strict { l + 1 } else { l };
        let j = (min_c - wc).div_euclid(tc);
        let mut c = wc + j * tc;
        if c < min_c {
            c += tc;
        }
        best = best.min(c - l);
    }
    (best < i64::MAX).then_some(best as f64 * 1e-6)
}
