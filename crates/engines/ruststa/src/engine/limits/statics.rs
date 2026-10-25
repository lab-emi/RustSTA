// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Transitions of the pins that the propagation leaves without one (`STA-REQ-0809`).
//!
//! The delay calculation ignores logic values (`STA-REQ-0117`): the propagation of a corner
//! carries transitions through the arcs that constants, case analysis and false `when`
//! conditions disable and through the clock-to-output arcs of sequential cells that no clock
//! reaches, from the tie outputs (transition 0) and the ports. The pins left without a
//! transition are the netlist literals and their cones, the pins reached only through an arc
//! that `set_disable_timing` or loop breaking disables, and the pins behind an arc that logic
//! values disable but that would close a loop (`T0149`). This pass completes the last kind with
//! the same arc evaluation (`STA-REQ-0723`, `STA-REQ-0217`) carried in topological order from
//! the propagated transitions; a constant pin that it does not reach either (a literal and its
//! cone) never switches and has the transition 0. Any other pin without a transition is
//! untimed: its check is counted and not evaluated.
//!
//! The pass visits each node and arc once (`O(V + E)`, AGENTS rule 3.4) and keeps one entry per
//! pin without a propagated transition.

use rustc_hash::FxHashMap;
use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{ArcClass, NONE};

use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::{DriverLoad, WireModel};
use crate::engine::graph::{ArcKind, ArcSense, DisableReason, GraphArc, LaunchArc, TimingGraph};
use crate::engine::input::libview::{Sense, TimingLib};
use crate::engine::input::{NetTarget, NetlistView};
use crate::engine::prop::lower::{LoweredCorner, TimingValues, arc_value};
use crate::engine::prop::starts::canonical_net_totals;
use crate::engine::types::{An, NodeId, Tr};

/// How a pin without a propagated transition is checked (`STA-REQ-0809`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StaticTransition {
    /// The late `[rise, fall]` transition of the static delay calculation (ns); a side without
    /// one is not finite.
    Computed([f64; 2]),
    /// A constant pin that no transition reaches: it never switches, transition 0.
    Constant,
}

/// The transitions of the pins of a corner without a propagated transition (`STA-REQ-0809`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticTransitions {
    by_node: FxHashMap<NodeId, StaticTransition>,
}

impl StaticTransitions {
    /// The static transition of `node`, `None` when it has none (an untimed pin) or has a
    /// propagated one.
    #[must_use]
    pub fn get(&self, node: NodeId) -> Option<StaticTransition> {
        self.by_node.get(&node).copied()
    }

    /// The number of pins with a static transition.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    /// True without any pin with a static transition.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }
}

/// True when the logic values alone disable an arc: the delay calculation still evaluates it
/// (`STA-REQ-0809`).
fn logic_disabled(reason: Option<DisableReason>) -> bool {
    matches!(
        reason,
        None | Some(
            DisableReason::Constant | DisableReason::CaseAnalysis | DisableReason::FalseWhen
        )
    )
}

/// The class of the cell arc `arc` with the sense of the timing graph, as the lowering builds it
/// for an enabled arc.
fn cell_class(view: &NetlistView<'_>, lib: &TimingLib, arc: &GraphArc) -> Option<ArcClass> {
    let (inst, local) = arc.kind.inst_and_lib_arc()?;
    let cell_row = view.frozen().instances.get(inst as usize)?.cell;
    let tc = lib.cell(view.cell_name(cell_row))?;
    let global = *tc.arcs.get(local.index())?;
    let mut class = *lib.lut_bank.classes.get(global.0 as usize)?;
    let desired = match arc.sense {
        ArcSense::PositiveUnate => Sense::Positive,
        ArcSense::NegativeUnate => Sense::Negative,
        ArcSense::NonUnate => Sense::Non,
    };
    let prop = lib.arc(global);
    if prop.sense != desired {
        let mut refined = prop.clone();
        refined.sense = desired;
        class.flags = refined.transition_flags();
    }
    Some(class)
}

/// The late output transitions `[rise, fall]` of the clock-to-output arc `l` when its clock pin
/// has the transition `s` at the arc's edge, as the launch of the start rows evaluates it
/// (`STA-REQ-0217`), with the driver resistances of their waveforms (`STA-REQ-0729`).
fn launch_transition(
    view: &NetlistView<'_>,
    lib: &TimingLib,
    net_loads: &NetLoads,
    totals: &[[[f64; 2]; 2]],
    l: &LaunchArc,
    s: f64,
) -> ([f64; 2], [Option<(f64, f64)>; 2]) {
    let mut out = [f64::NEG_INFINITY; 2];
    let mut drives = [None; 2];
    let Some(global) = view
        .frozen()
        .instances
        .get(l.inst as usize)
        .and_then(|i| lib.cell(view.cell_name(i.cell)))
        .and_then(|tc| tc.arcs.get(l.lib_arc.index()).copied())
    else {
        return (out, drives);
    };
    let prop = lib.arc(global);
    let net = match view.node_net(l.out_pin) {
        NetTarget::Net(net) => Some(net),
        NetTarget::Const(_) => None,
    };
    let late = An::Late;
    for to in Tr::ALL {
        if prop.delay[to.index()] == crate::engine::delay::lut::NO_TABLE
            && prop.slew[to.index()] == crate::engine::delay::lut::NO_TABLE
        {
            continue;
        }
        let c = net
            .and_then(|n| totals.get(n.index()))
            .map_or(0.0, |t| t[to.index()][late.index()]);
        let load = match net {
            Some(n) => net_loads.physical_load(n, to, late, c),
            None => DriverLoad::Lumped { c },
        };
        let (ds, drive) = lib.arc_delay_drive_3d(global, to, s, load, 0.0);
        if ds.slew.is_finite() {
            out[to.index()] = ds.slew.max(0.0);
            drives[to.index()] = drive;
        }
    }
    (out, drives)
}

/// The static transitions of the pins without a propagated transition in `values` (node order)
/// of the corner lowered as `lowered` with the wire model `wire` and the net loads `net_loads`
/// (`STA-REQ-0809`). `constant` marks the nodes with a constant logic value.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn static_transitions(
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    lib: &TimingLib,
    lowered: &LoweredCorner<'_>,
    wire: &dyn WireModel,
    net_loads: &NetLoads,
    values: &TimingValues,
    constant: &[bool],
) -> StaticTransitions {
    let n = graph.num_nodes();
    let late = An::Late.index();
    let propagated = |i: usize| -> bool {
        i < values.nodes()
            && Tr::ALL
                .iter()
                .any(|tr| An::Late.is_present(values.transition(i, tr.index(), late)))
    };
    let missing: Vec<u32> = (0..n as u32).filter(|&i| !propagated(i as usize)).collect();
    let mut out = StaticTransitions::default();
    if missing.is_empty() {
        return out;
    }
    let mut slot = vec![u32::MAX; n];
    for (k, &i) in missing.iter().enumerate() {
        slot[i as usize] = k as u32;
    }
    let m = missing.len();
    let arcs = graph.arcs();
    let disabled = graph.disabled();
    let launches = graph.launches();
    let disabled_launches = graph.disabled_launches();
    // The evaluated incoming arcs of the missing nodes, `(src, dst, index)`: a propagation arc
    // `index < arcs.len()`, else the launch arc `index - arcs.len()`.
    let edges = || {
        let props = arcs
            .iter()
            .enumerate()
            .map(|(ai, a)| (a.src, a.dst, ai, logic_disabled(disabled[ai])));
        let lns = launches.iter().enumerate().map(|(li, l)| {
            (
                l.clock_pin,
                l.out_pin,
                arcs.len() + li,
                logic_disabled(disabled_launches[li]),
            )
        });
        props.chain(lns)
    };
    let mut any_in = vec![false; m];
    let mut in_off = vec![0u32; m + 1];
    let mut out_off = vec![0u32; m + 1];
    for (src, dst, _, evaluated) in edges() {
        let d = slot[dst.index()];
        if d == u32::MAX {
            continue;
        }
        any_in[d as usize] = true;
        if !evaluated {
            continue;
        }
        in_off[d as usize + 1] += 1;
        let s = slot[src.index()];
        if s != u32::MAX {
            out_off[s as usize + 1] += 1;
        }
    }
    for k in 0..m {
        in_off[k + 1] += in_off[k];
        out_off[k + 1] += out_off[k];
    }
    let mut in_arcs = vec![0u32; in_off[m] as usize];
    let mut out_dst = vec![0u32; out_off[m] as usize];
    let mut indeg = vec![0u32; m];
    {
        let mut in_fill = in_off.clone();
        let mut out_fill = out_off.clone();
        for (src, dst, ai, evaluated) in edges() {
            let d = slot[dst.index()];
            if d == u32::MAX || !evaluated {
                continue;
            }
            in_arcs[in_fill[d as usize] as usize] = ai as u32;
            in_fill[d as usize] += 1;
            let s = slot[src.index()];
            if s != u32::MAX {
                out_dst[out_fill[s as usize] as usize] = d;
                out_fill[s as usize] += 1;
                indeg[d as usize] += 1;
            }
        }
    }

    let totals = canonical_net_totals(view, net_loads);
    let none = f64::NEG_INFINITY;
    let mut value: Vec<Option<[f64; 2]>> = vec![None; m];
    // The driver resistance of each static transition (`STA-REQ-0729`).
    let mut drive_of: Vec<[Option<(f64, f64)>; 2]> = vec![[None; 2]; m];
    let mut queue: Vec<u32> = (0..m as u32).filter(|&k| indeg[k as usize] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let k = queue[head] as usize;
        head += 1;
        let node = NodeId(missing[k]);
        let mut tr = [none; 2];
        let mut dr = [None; 2];
        for &ai in &in_arcs[in_off[k] as usize..in_off[k + 1] as usize] {
            let launch = (ai as usize)
                .checked_sub(arcs.len())
                .map(|li| &launches[li]);
            let src = match launch {
                Some(l) => l.clock_pin.index(),
                None => arcs[ai as usize].src.index(),
            };
            let (s_in, d_in) = if slot[src] == u32::MAX {
                let d = lowered
                    .drives
                    .get(graph.topo_rank(NodeId(src as u32)) as usize)
                    .map_or([None; 2], |d| [d[late], d[2 + late]]);
                (
                    [
                        values.transition(src, 0, late),
                        values.transition(src, 1, late),
                    ],
                    d,
                )
            } else {
                match value[slot[src] as usize] {
                    Some(v) => (v, drive_of[slot[src] as usize]),
                    None => continue,
                }
            };
            if let Some(l) = launch {
                let s = s_in[l.edge.index()];
                if s.is_finite() {
                    let (q, d) = launch_transition(view, lib, net_loads, &totals, l, s);
                    for to in 0..2 {
                        if q[to] > tr[to] {
                            tr[to] = q[to];
                            dr[to] = d[to];
                        }
                    }
                }
                continue;
            }
            let a = &arcs[ai as usize];
            let class = match a.kind {
                ArcKind::Net { .. } => Some(lowered.luts.classes[lib.wire_class as usize]),
                _ => match lowered.graph_arc_to_csr_arc.get(ai as usize) {
                    Some(&c) if c != NONE => {
                        Some(lowered.luts.classes[lowered.csr.in_arcs[c as usize].class as usize])
                    }
                    _ => cell_class(view, lib, a),
                },
            };
            let Some(class) = class else {
                continue;
            };
            let load = lowered.load.get(graph.topo_rank(node) as usize);
            for ti in 0..2 {
                let s = s_in[ti];
                if !s.is_finite() {
                    continue;
                }
                for to in 0..2 {
                    if !class.allows(ti, to) {
                        continue;
                    }
                    let (q, drive) = if lowered.arc_values.is_empty() {
                        let q = if class.is_wire() {
                            s
                        } else {
                            let l = load.map_or(0.0, |l| l[to * 2 + late]);
                            let q = lookup(
                                &lowered.luts.luts,
                                &lowered.luts.data,
                                class.slew[to],
                                [s, l, l],
                            );
                            if q < 0.0 { 0.0 } else { q }
                        };
                        (q, None)
                    } else {
                        let l = load.map_or(0.0, |l| l[to * 2 + late]);
                        match arc_value(
                            &lowered.luts,
                            &class,
                            a,
                            view,
                            wire,
                            lib,
                            l,
                            s,
                            to,
                            late,
                            d_in[ti],
                        ) {
                            Ok(v) => (v.slew, v.drive),
                            Err(_) => continue,
                        }
                    };
                    if q > tr[to] {
                        tr[to] = q;
                        dr[to] = drive;
                    }
                }
            }
        }
        if tr.iter().any(|x| x.is_finite()) {
            value[k] = Some(tr);
            drive_of[k] = dr;
        } else if !any_in[k]
            && constant.get(node.index()).copied().unwrap_or(false)
            && view.is_driver(node)
        {
            // A tie cell: a constant driver without timing arcs.
            value[k] = Some([0.0, 0.0]);
        }
        for &d in &out_dst[out_off[k] as usize..out_off[k + 1] as usize] {
            indeg[d as usize] -= 1;
            if indeg[d as usize] == 0 {
                queue.push(d);
            }
        }
    }

    for (k, &i) in missing.iter().enumerate() {
        let node = NodeId(i);
        let st = match value[k] {
            Some(v) => Some(StaticTransition::Computed(v)),
            None if constant.get(i as usize).copied().unwrap_or(false)
                || view.is_const_node(node).is_some() =>
            {
                Some(StaticTransition::Constant)
            }
            None => None,
        };
        if let Some(st) = st {
            out.by_node.insert(node, st);
        }
    }
    out
}
