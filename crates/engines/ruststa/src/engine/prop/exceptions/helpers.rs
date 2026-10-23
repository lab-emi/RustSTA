// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Clock sense, latency, uncertainty, and edge relation helpers for exception-aware timing
//! evaluation (`STA-REQ-0200` to `STA-REQ-0268`, owner: E8).

use std::borrow::Cow;

use rusteda_core::diag::Diag;
use rusteda_formats::sdc::{EdgeScope, SetupHoldScope};

use super::resolve::AnalysisKind;
use crate::engine::delay::load::NetLoads;
use crate::engine::graph::{ArcKind, ArcSense, CheckArc, CheckKind, TimingGraph};
use crate::engine::input::constr::{
    ClockLatencyTarget, CompiledConstraints, CompiledIoDelay, DesignIndex, MaxTimeBorrowTarget,
};
use crate::engine::input::libview::{CellCheck, CheckArcKind, PropArcKind, TimingCell, TimingLib};
use crate::engine::input::{NetTarget, NetlistView};
use crate::engine::types::{An, ClockId, LibArc, NetRow, NodeId, Tr};

#[derive(Debug, Clone, Copy)]
pub(super) struct InferredClockGateCheck {
    pub(super) enable_pin: NodeId,
    pub(super) clock_pin: NodeId,
    /// `false` = active-high (AND-like, leading Rise, trailing Fall);
    /// `true` = active-low (OR-like, leading Fall, trailing Rise).
    pub(super) active_low: bool,
}

pub(super) fn discover_clock_gating_checks(
    nv: &NetlistView<'_>,
    index: &DesignIndex,
    tlib: &TimingLib,
    clock_sense: &[Vec<(ClockId, u8)>],
) -> Vec<InferredClockGateCheck> {
    let mut out = Vec::new();
    for inst_info in &index.instances {
        if inst_info.is_sequential {
            continue;
        }
        let Some(tc) = tlib.cell(&inst_info.cell_name) else {
            continue;
        };
        let out_func = tc.pins.iter().find_map(|p| p.func.as_ref());
        let mut clk_inputs = Vec::new();
        let mut en_inputs = Vec::new();

        for &pi in &inst_info.pin_indices {
            let pinfo = &index.all_pins[pi];
            if pinfo.dir != 0 {
                continue;
            }
            let Some(nid) = pinfo.pin_ref.node else {
                continue;
            };
            let lib_pin_idx = tc.pins.iter().position(|lp| *lp.name == *pinfo.pin_name);
            if !clock_sense[nid.index()].is_empty() {
                clk_inputs.push((nid, lib_pin_idx));
            } else if index.inferred_cg_enable_pins.contains(&pi) {
                en_inputs.push((nid, lib_pin_idx));
            }
        }

        if clk_inputs.is_empty() || en_inputs.is_empty() {
            continue;
        }
        let (clk_node, clk_lp) = clk_inputs[0];
        let active_low = match (out_func, clk_lp) {
            (Some((ins, tt)), Some(c_lp)) => {
                if let Some(bit_pos) = ins.iter().position(|&k| k == c_lp) {
                    let n_in = ins.len();
                    let const_when_0 = is_truth_table_constant_when(*tt, n_in, bit_pos, false);
                    let const_when_1 = is_truth_table_constant_when(*tt, n_in, bit_pos, true);
                    if const_when_0 {
                        false
                    } else if const_when_1 {
                        true
                    } else {
                        continue;
                    }
                } else {
                    false
                }
            }
            _ => false,
        };

        for (en_node, _) in en_inputs {
            out.push(InferredClockGateCheck {
                enable_pin: en_node,
                clock_pin: clk_node,
                active_low,
            });
        }
    }
    let _ = nv;
    out
}

fn is_truth_table_constant_when(tt: u64, n_in: usize, bit_pos: usize, val: bool) -> bool {
    let mut first: Option<u64> = None;
    for row in 0..(1usize << n_in) {
        let bit = ((row >> bit_pos) & 1) != 0;
        if bit != val {
            continue;
        }
        let out = (tt >> row) & 1;
        match first {
            None => first = Some(out),
            Some(prev) if prev != out => return false,
            Some(_) => {}
        }
    }
    first.is_some()
}

pub(super) fn find_cell_check(tc: &TimingCell, ch: CheckArc) -> Option<&CellCheck> {
    let want_kind = match ch.kind {
        CheckKind::Setup => CheckArcKind::Setup,
        CheckKind::Hold => CheckArcKind::Hold,
        CheckKind::Recovery => CheckArcKind::Recovery,
        CheckKind::Removal => CheckArcKind::Removal,
    };
    tc.checks
        .iter()
        .find(|c| c.kind == want_kind && c.edge == ch.edge)
}

pub(super) fn propagate_clock_sense(
    graph: &TimingGraph,
    compiled: &CompiledConstraints,
) -> Vec<Vec<(ClockId, u8)>> {
    let n_nodes = graph.num_nodes();
    let mut clock_sense: Vec<Vec<(ClockId, u8)>> = vec![Vec::new(); n_nodes];
    let g_arcs = graph.arcs();

    for ck in &compiled.clocks {
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
                if graph.is_arc_disabled(ai as usize) {
                    continue;
                }
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
                clock_sense[v].push((ck.id, b));
            }
        }
    }

    clock_sense
}

pub(super) fn max_time_borrow_for_latch(
    compiled: &CompiledConstraints,
    inst: u32,
    data_pin: NodeId,
    clock_pin: NodeId,
    clock: ClockId,
) -> Option<f64> {
    if let Some(&b) = compiled.latch_max_borrow_ns.get(&inst) {
        return Some(b);
    }
    for spec in compiled.max_time_borrow.iter().rev() {
        match spec.target {
            MaxTimeBorrowTarget::Pin(nid) if nid == data_pin || nid == clock_pin => {
                return Some(spec.max_borrow_ns);
            }
            MaxTimeBorrowTarget::Cell(i) if i == inst => {
                return Some(spec.max_borrow_ns);
            }
            MaxTimeBorrowTarget::Clock(c) if c == clock => {
                return Some(spec.max_borrow_ns);
            }
            _ => {}
        }
    }
    None
}

pub(super) fn clock_latency(
    compiled: &CompiledConstraints,
    c_id: ClockId,
    edge: Tr,
    an: An,
    pin: Option<NodeId>,
) -> f64 {
    let ck = &compiled.clocks[c_id.index()];
    let src_lat = ck.source_latency_ns[an.index()][an.index()][edge.index()];
    let mut net_lat = ck.network_latency_ns[an.index()][edge.index()];
    if let Some(p) = pin {
        for spec in &compiled.clock_latencies {
            if !spec.source
                && spec.target == ClockLatencyTarget::Node(p)
                && spec.clock.is_none_or(|c| c == c_id)
            {
                net_lat = spec.delay_ns;
            }
        }
    }
    src_lat + net_lat
}

pub(super) fn io_clock_latency(
    compiled: &CompiledConstraints,
    d: &CompiledIoDelay,
    c_id: ClockId,
    edge: Tr,
    an: An,
) -> f64 {
    let ck = &compiled.clocks[c_id.index()];
    let src_lat = if d.source_latency_included {
        0.0
    } else {
        ck.source_latency_ns[an.index()][an.index()][edge.index()]
    };
    let net_lat = if d.network_latency_included {
        0.0
    } else {
        ck.network_latency_ns[an.index()][edge.index()]
    };
    src_lat + net_lat
}

pub(super) fn clock_uncertainty(
    compiled: &CompiledConstraints,
    l_id: ClockId,
    e_l: Tr,
    c_id: ClockId,
    e_c: Tr,
    analysis: AnalysisKind,
    pin: Option<NodeId>,
) -> f64 {
    for icu in compiled.inter_clock_uncertainties.iter().rev() {
        let sh_ok = matches!(
            (analysis, icu.setup_hold),
            (
                AnalysisKind::Setup,
                SetupHoldScope::Setup | SetupHoldScope::Both
            ) | (
                AnalysisKind::Hold,
                SetupHoldScope::Hold | SetupHoldScope::Both
            )
        );
        let fl_ok = matches!(
            (icu.from_edge, e_l),
            (EdgeScope::Both, _) | (EdgeScope::Rise, Tr::Rise) | (EdgeScope::Fall, Tr::Fall)
        );
        let tc_ok = matches!(
            (icu.to_edge, e_c),
            (EdgeScope::Both, _) | (EdgeScope::Rise, Tr::Rise) | (EdgeScope::Fall, Tr::Fall)
        );
        if sh_ok && fl_ok && tc_ok && icu.from_clock == l_id && icu.to_clock == c_id {
            return icu.uncertainty_ns;
        }
    }
    if let Some(p) = pin
        && let Some(pu) = compiled
            .pin_clock_uncertainties
            .iter()
            .rev()
            .find(|u| u.node == p)
    {
        let v = match analysis {
            AnalysisKind::Setup => pu.setup_ns,
            AnalysisKind::Hold => pu.hold_ns,
        };
        if let Some(u) = v {
            return u;
        }
    }
    let ck_c = &compiled.clocks[c_id.index()];
    match analysis {
        AnalysisKind::Setup => ck_c.uncertainty_setup_ns[e_c.index()],
        AnalysisKind::Hold => ck_c.uncertainty_hold_ns[e_c.index()],
    }
}

pub(super) fn port_input_slew(compiled: &CompiledConstraints, node: NodeId, tr: Tr) -> [f64; 2] {
    if let Some(it) = compiled.input_transitions.get(&node) {
        let e = it.slew_ns[An::Early.index()][tr.index()]
            .or(it.slew_ns[An::Early.index()][tr.opposite().index()])
            .or(it.slew_ns[An::Late.index()][tr.index()])
            .unwrap_or(0.0);
        let l = it.slew_ns[An::Late.index()][tr.index()]
            .or(it.slew_ns[An::Late.index()][tr.opposite().index()])
            .or(it.slew_ns[An::Early.index()][tr.index()])
            .unwrap_or(0.0);
        [e, l]
    } else {
        [0.0, 0.0]
    }
}

pub(super) fn source_edges(pin_edge: Tr, bits: u8) -> Vec<Tr> {
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
pub fn setup_relation_ns(
    l_period: f64,
    l_waveform: [f64; 2],
    e_l: Tr,
    c_period: f64,
    c_waveform: [f64; 2],
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(l_period, l_waveform, e_l, c_period, c_waveform, e_c, true)
}

/// Smallest non-negative `capture - launch` relation in ns over the common period (`STA-REQ-0225`).
pub fn open_relation_ns(
    l_period: f64,
    l_waveform: [f64; 2],
    e_l: Tr,
    c_period: f64,
    c_waveform: [f64; 2],
    e_c: Tr,
) -> Option<f64> {
    edge_relation_ns(l_period, l_waveform, e_l, c_period, c_waveform, e_c, false)
}

/// Largest non-positive `capture - launch` relation in ns over the common period (`STA-REQ-0220`).
pub fn hold_relation_ns(
    l_period: f64,
    l_waveform: [f64; 2],
    e_l: Tr,
    c_period: f64,
    c_waveform: [f64; 2],
    e_c: Tr,
) -> Option<f64> {
    let tl = to_fs(l_period);
    let tc = to_fs(c_period);
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let g = gcd(tl, tc);
    let mult = tc / g;
    if mult > 1000 {
        return None;
    }
    let wl = to_fs(l_waveform[e_l.index()]);
    let wc = to_fs(c_waveform[e_c.index()]);
    let mut best = i64::MIN;
    for k in 0..mult {
        let l = wl + k * tl;
        let diff = -((l - wc).rem_euclid(tc));
        best = best.max(diff);
    }
    (best > i64::MIN).then_some(best as f64 * 1e-6)
}

fn edge_relation_ns(
    l_period: f64,
    l_waveform: [f64; 2],
    e_l: Tr,
    c_period: f64,
    c_waveform: [f64; 2],
    e_c: Tr,
    strict: bool,
) -> Option<f64> {
    let tl = to_fs(l_period);
    let tc = to_fs(c_period);
    if tl <= 0 || tc <= 0 {
        return None;
    }
    let g = gcd(tl, tc);
    let mult = tc / g;
    if mult > 1000 {
        return None;
    }
    let wl = to_fs(l_waveform[e_l.index()]);
    let wc = to_fs(c_waveform[e_c.index()]);
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

pub(super) fn map_cell_arcs(
    nv: &NetlistView<'_>,
    lib: &TimingLib,
) -> Result<Vec<Vec<LibArc>>, Diag> {
    let n_cells = nv.num_cells();
    let mut out = Vec::with_capacity(n_cells);
    for cell_row in 0..n_cells {
        let cell_name = nv.cell_name(cell_row as u32);
        let tc = lib.cell(cell_name).ok_or_else(|| {
            Diag::error(
                "T0004",
                format!(
                    "corner '{}': unknown cell '{cell_name}' (STA-REQ-0003)",
                    lib.corner
                ),
            )
        })?;
        let _ = PropArcKind::Edge(Tr::Rise);
        out.push(tc.arcs.clone());
    }
    Ok(out)
}

pub(super) fn canonical_net_totals<'a>(
    nv: &NetlistView<'_>,
    net_loads: &'a NetLoads,
) -> Cow<'a, [[[f64; 2]; 2]]> {
    if nv.frozen().assigns.is_empty() {
        return Cow::Borrowed(&net_loads.totals);
    }
    let n_nets = nv.num_nets();
    let mut totals = vec![[[0.0f64; 2]; 2]; n_nets];
    for (raw_idx, raw_tot) in net_loads.totals.iter().enumerate() {
        if let NetTarget::Net(canon) = nv.resolve_net(NetRow(raw_idx as u32))
            && let Some(dst) = totals.get_mut(canon.index())
        {
            for tr in 0..2 {
                for an in 0..2 {
                    dst[tr][an] += raw_tot[tr][an];
                }
            }
        }
    }
    Cow::Owned(totals)
}
