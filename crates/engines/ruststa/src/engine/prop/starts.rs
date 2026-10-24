// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Start rows: the arrivals that seed the propagation (owner: E5).
//!
//! Engine section C. Input delays start at the clock edge plus the clock's source and network
//! latency, unless the delay includes them; a one-sided `-max` or `-min` delay leaves the other
//! bound unset. Clock sources start the clock network, and clocks used as data launch at their
//! latency plus the clock-to-output delay.

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;
use rusteda_formats::sdc::{EdgeScope, MinMaxScope};

use super::clocks::ideal::IdealClockModel;
use super::clocks::{ClockModel, source_edges_for_pin_tr};
use super::exceptions::ExceptionContext;
use super::lower::{GraphStartBuilder, StartRows};
use super::tags::TagTable;
use crate::engine::delay::derate::{DelayKind, Derate, PathKind};
use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::lut::NO_TABLE;
use crate::engine::delay::wire::DriverLoad;
use crate::engine::graph::TimingGraph;
use crate::engine::input::constr::{CompiledConstraints, DrivingCellSpec};
use crate::engine::input::libview::{PropArcKind, TimingLib};
use crate::engine::input::{NetTarget, NetlistView};
use crate::engine::types::{An, ClockId, LibArc, NodeId, Tr};

/// Incremental drive delay and output slew produced by a `set_driving_cell` on a port for one
/// `(tr, an)` (`STA-REQ-0218`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriveCellEffect {
    /// Incremental drive delay `delay(s_in, c_eff) - delay(s_in, 0)` (ns).
    pub incr_delay_ns: f64,
    /// Port transition time `slew(s_in, c_eff)` (ns, `>= 0`).
    pub slew_ns: f64,
}

/// Output of startpoint seeding for one corner (`STA-REQ-0201`, `STA-REQ-0217`–`STA-REQ-0219`).
#[derive(Debug, Clone)]
pub struct SeededStarts {
    /// Compute-task [`StartRows`] sorted by topological rank.
    pub rows: StartRows,
    /// Per-node `[Rise/Early, Rise/Late, Fall/Early, Fall/Late]` slews launched by sequential
    /// `CK -> Q` / `G -> Q` launch arcs, used to preserve `G -> Q` slews at latch `Q` pins
    /// (`STA-REQ-0222`).
    pub launch_slews: FxHashMap<NodeId, [f64; 4]>,
    /// The driver waveform (resistance, source duration) of the launch that set each slew of
    /// `launch_slews`, when that launch drives a pi load outside the lumped regime; the net's
    /// wires follow it (`STA-REQ-0729`).
    pub launch_drives: FxHashMap<NodeId, [Option<(f64, f64)>; 4]>,
    /// Per-port resolved driving-cell effects `[tr][an]`.
    pub driving_cells: FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
}

/// Seeds the [`StartRows`] of a corner from `constraints`, `clock_model`, `tags`, `lib`,
/// `net_loads`, and `derate` (`STA-REQ-0201`, `STA-REQ-0217`, `STA-REQ-0218`, `STA-REQ-0219`).
/// Each startpoint launches the tag of its `-from` exception state in `exceptions`
/// (`STA-REQ-0271`).
#[allow(clippy::too_many_arguments)]
pub fn build_start_rows<D: Derate>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    constraints: &CompiledConstraints,
    clock_model: &IdealClockModel,
    tags: &TagTable,
    exceptions: &ExceptionContext,
    net_loads: &NetLoads,
    derate: &D,
) -> Result<SeededStarts, Diag> {
    let n_nodes = graph.num_nodes();
    let ntags = tags.ntags_for_compute();
    let mut builder = GraphStartBuilder::new(ntags);
    let canon_totals = canonical_net_totals(netview, net_loads);
    let cell_arcs = map_cell_arcs(netview, lib)?;

    // 1. Evaluate `set_driving_cell` specifications per port (`STA-REQ-0218`).
    let driving_cells =
        driving_cell_effects(netview, graph, lib, constraints, &canon_totals, derate);

    // Helper returning `(incr_drive_delay_ns, port_slew_ns)` for `(node, tr, an)`.
    // `set_driving_cell` replaces `set_input_transition` for the transitions and bounds it covers
    // (`STA-REQ-0218`).
    let resolve_port_drive = |node: NodeId, tr: Tr, an: An| {
        resolve_port_drive(&driving_cells, constraints, node, tr, an)
    };

    // 2. Seed clock source nodes (`create_clock` used as data; `STA-REQ-0201`, `STA-REQ-0217`).
    for (ci, ck) in constraints.clocks.iter().enumerate() {
        let cid = ClockId(ci as u32);
        for &src in &ck.sources {
            if src.index() >= n_nodes {
                continue;
            }
            for edge in Tr::ALL {
                let Some(tid) = exceptions.start_tag(tags, src, edge, cid, edge) else {
                    continue;
                };
                for an in An::ALL {
                    let Some(arrival) = clock_model.arrival(cid, edge, src, edge, an) else {
                        continue;
                    };
                    builder.merge_bound(graph, src, tid.0, edge, an, arrival.latency, arrival.slew);
                }
            }
        }
    }

    // 3. Seed `set_input_delay` startpoints (`STA-REQ-0218`, `STA-REQ-0219`).
    // A one-sided `-max` or `-min` `set_input_delay` leaves `delay_ns[other_an][tr] == None`,
    // which keeps the opposite bound at its absent sentinel (`+inf` for `Early`, `-inf` for `Late`).
    for d in &constraints.input_delays {
        if d.node.index() >= n_nodes {
            continue;
        }
        let Some(clock_id) = d.clock else {
            continue;
        };
        for tr in Tr::ALL {
            let Some(tid) = exceptions.start_tag(tags, d.node, tr, clock_id, d.clock_edge) else {
                continue;
            };
            for an in An::ALL {
                let (incr_d, port_slew) = resolve_port_drive(d.node, tr, an);
                if let Some(delay_val) = d.delay_ns[an.index()][tr.index()] {
                    let lat = clock_model.io_latency(
                        clock_id,
                        d.clock_edge,
                        an,
                        d.source_latency_included,
                        d.network_latency_included,
                    );
                    let arr = lat + delay_val + incr_d;
                    builder.merge_bound(graph, d.node, tid.0, tr, an, arr, port_slew);
                }
            }
        }
    }

    // 4. Seed untagged slews on all non-clock primary input/inout ports (`STA-REQ-0108`) so
    // downstream combinational cones always have the port's transition time (from
    // `set_driving_cell`, `set_input_transition`, or default `0.0`), and the transition 0 on the
    // outputs of tie cells (`STA-REQ-0117`).
    let skip = clock_source_nodes(constraints);
    for (node, slew) in untagged_seeds(netview, graph, constraints, &driving_cells, &skip) {
        for tr in Tr::ALL {
            for an in An::ALL {
                builder.merge_untagged_slew(graph, node, tr, an, slew[tr.index()][an.index()]);
            }
        }
    }

    // 5. Seed sequential `CK -> Q` and latch `G -> Q` launch arcs (`STA-REQ-0217`, `STA-REQ-0222`).
    // A disabled launch launches no arrival, also where a clock reaches its clock pin: an output
    // pin held by a constant or case analysis (`STA-REQ-0164`), a false `when` (`STA-REQ-0155`),
    // `set_disable_timing` (`STA-REQ-0156`). The launches that logic values disable carry their
    // transition as slew-only arcs (`STA-REQ-0117`); a user-disabled one carries nothing.
    let mut launch_slews: FxHashMap<NodeId, [f64; 4]> = FxHashMap::default();
    let mut launch_drives: FxHashMap<NodeId, [Option<(f64, f64)>; 4]> = FxHashMap::default();
    let wins = |an: An, s: f64, old: f64| match an {
        An::Early => s < old,
        An::Late => s > old,
    };
    for (li, l) in graph.launches().iter().enumerate() {
        if graph.is_launch_disabled(li) {
            continue;
        }
        let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
        let global_arc = cell_arcs[cell_row][l.lib_arc.index()];

        for &(cid, bits) in clock_model.pin_clocks(l.clock_pin) {
            for src_edge in source_edges_for_pin_tr(l.edge, bits) {
                let Some(tid) = exceptions.start_tag(tags, l.clock_pin, l.edge, cid, src_edge)
                else {
                    continue;
                };
                for an in An::ALL {
                    let Some(ck_arr) = clock_model.arrival(cid, src_edge, l.clock_pin, l.edge, an)
                    else {
                        continue;
                    };
                    for to in Tr::ALL {
                        let Some((delay, slew, drive)) = launch_value(
                            netview,
                            lib,
                            net_loads,
                            &canon_totals,
                            global_arc,
                            l.out_pin,
                            to,
                            an,
                            ck_arr.slew,
                        )?
                        else {
                            continue;
                        };
                        let factor =
                            derate.factor(DelayKind::Cell, PathKind::Data, l.out_pin, an, to);
                        let arr = ck_arr.latency + delay * factor;
                        builder.merge_bound(graph, l.out_pin, tid.0, to, an, arr, slew);

                        let entry = launch_slews.entry(l.out_pin).or_insert([
                            An::Early.absent(),
                            An::Late.absent(),
                            An::Early.absent(),
                            An::Late.absent(),
                        ]);
                        let s_idx = to.index() * 2 + an.index();
                        if wins(an, slew, entry[s_idx]) {
                            launch_drives.entry(l.out_pin).or_insert([None; 4])[s_idx] = drive;
                        }
                        entry[s_idx] = an.merge(slew, entry[s_idx]);
                    }
                }
            }
        }
    }

    // 6. Launches that an ideal clock reaches only through arcs that logic values disable start
    // their output transition at the ideal clock transition, without an arrival (`STA-REQ-0822`).
    for (li, l) in graph.launches().iter().enumerate() {
        let Some(slew) = ideal_launch_transition(
            netview,
            graph,
            lib,
            clock_model,
            net_loads,
            &canon_totals,
            &cell_arcs,
            li,
        )?
        else {
            continue;
        };
        for to in Tr::ALL {
            for an in An::ALL {
                let s = slew[to.index()][an.index()];
                if !s.is_finite() {
                    continue;
                }
                builder.merge_untagged_slew(graph, l.out_pin, to, an, s);
                let entry = launch_slews.entry(l.out_pin).or_insert([
                    An::Early.absent(),
                    An::Late.absent(),
                    An::Early.absent(),
                    An::Late.absent(),
                ]);
                let s_idx = to.index() * 2 + an.index();
                if wins(an, s, entry[s_idx]) {
                    launch_drives.entry(l.out_pin).or_insert([None; 4])[s_idx] = None;
                }
                entry[s_idx] = an.merge(s, entry[s_idx]);
            }
        }
    }

    Ok(SeededStarts {
        rows: builder.build(),
        launch_slews,
        launch_drives,
        driving_cells,
    })
}

/// The delay, the output transition (`>= 0`) and the driver resistance of the waveform that the
/// net's wires follow (`STA-REQ-0729`) of the clock-to-output arc with the library arc
/// `global_arc` and the output pin `out_pin` for the output transition `to` and the bound `an`
/// when its clock pin has the transition `ck_slew`, at the load of the output's net; `None` when
/// the arc has no table for `to` (`STA-REQ-0217`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn launch_value(
    netview: &NetlistView<'_>,
    lib: &TimingLib,
    net_loads: &NetLoads,
    canon_totals: &[[[f64; 2]; 2]],
    global_arc: LibArc,
    out_pin: NodeId,
    to: Tr,
    an: An,
    ck_slew: f64,
) -> Result<Option<(f64, f64, Option<(f64, f64)>)>, Diag> {
    let prop_arc = lib.arc(global_arc);
    if prop_arc.delay[to.index()] == NO_TABLE && prop_arc.slew[to.index()] == NO_TABLE {
        return Ok(None);
    }
    let c_load = node_net_cap(netview, canon_totals, out_pin)[to.index()][an.index()];
    let (ds, drive) = lib.arc_delay_drive_3d(
        global_arc,
        to,
        ck_slew,
        match netview.node_net(out_pin) {
            NetTarget::Net(net) => net_loads.physical_load(net, to, an, c_load),
            _ => DriverLoad::Lumped { c: c_load },
        },
        0.0,
    );
    if !ds.delay.is_finite() || !ds.slew.is_finite() {
        return Err(Diag::error("T0077", "nonfinite RC sequential launch"));
    }
    Ok(Some((ds.delay, ds.slew.max(0.0), drive)))
}

/// The output transitions `[tr][an]` of launch arc `li` when it is an ideal launch of `graph`:
/// the arc evaluated at the ideal transition of each ideal clock that reaches its clock pin
/// (through arcs that logic values disable, or in its clock network), merged; `None` for any
/// other launch (`STA-REQ-0822`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn ideal_launch_transition(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    clock_model: &IdealClockModel,
    net_loads: &NetLoads,
    canon_totals: &[[[f64; 2]; 2]],
    cell_arcs: &[&[LibArc]],
    li: usize,
) -> Result<Option<[[f64; 2]; 2]>, Diag> {
    if !graph.is_ideal_launch(li) {
        return Ok(None);
    }
    let l = graph.launches()[li];
    let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
    let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
    let mut slew = [[An::Early.absent(), An::Late.absent()]; 2];
    let clocks = clock_model
        .logic_network_clocks(l.clock_pin)
        .iter()
        .copied()
        .chain(
            clock_model
                .pin_clocks(l.clock_pin)
                .iter()
                .map(|&(c, _)| c)
                .filter(|&c| !clock_model.is_propagated(c)),
        );
    for cid in clocks {
        for an in An::ALL {
            let ck_slew = clock_model.clock_transition(cid, l.edge, an);
            for to in Tr::ALL {
                if let Some((_, s, _)) = launch_value(
                    netview,
                    lib,
                    net_loads,
                    canon_totals,
                    global_arc,
                    l.out_pin,
                    to,
                    an,
                    ck_slew,
                )? {
                    let e = &mut slew[to.index()][an.index()];
                    *e = an.merge(s, *e);
                }
            }
        }
    }
    Ok(Some(slew))
}

/// The output transitions `[tr][an]` of the launch arcs of `graph` that `launch` selects (by
/// index), merged over the clocks and edges of `clock_model` that reach their clock pins exactly
/// as the start rows merge them (`STA-REQ-0217`), in launch order; a launch output that no clock
/// reaches is left out.
pub(crate) fn launch_transitions(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    clock_model: &IdealClockModel,
    net_loads: &NetLoads,
    launch: &[bool],
) -> Result<Vec<(NodeId, [[f64; 2]; 2])>, Diag> {
    let canon_totals = canonical_net_totals(netview, net_loads);
    let cell_arcs = map_cell_arcs(netview, lib)?;
    let mut out: Vec<(NodeId, [[f64; 2]; 2])> = Vec::new();
    for (li, l) in graph.launches().iter().enumerate() {
        if !launch.get(li).copied().unwrap_or(false) {
            continue;
        }
        if let Some(slew) = ideal_launch_transition(
            netview,
            graph,
            lib,
            clock_model,
            net_loads,
            &canon_totals,
            &cell_arcs,
            li,
        )? {
            out.push((l.out_pin, slew));
            continue;
        }
        let cell_row = netview.frozen().instances[l.inst as usize].cell as usize;
        let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
        let mut slew = [[An::Early.absent(), An::Late.absent()]; 2];
        let mut any = false;
        for &(cid, bits) in clock_model.pin_clocks(l.clock_pin) {
            for src_edge in source_edges_for_pin_tr(l.edge, bits) {
                for an in An::ALL {
                    let Some(ck_arr) = clock_model.arrival(cid, src_edge, l.clock_pin, l.edge, an)
                    else {
                        continue;
                    };
                    for to in Tr::ALL {
                        if let Some((_, s, _)) = launch_value(
                            netview,
                            lib,
                            net_loads,
                            &canon_totals,
                            global_arc,
                            l.out_pin,
                            to,
                            an,
                            ck_arr.slew,
                        )? {
                            let e = &mut slew[to.index()][an.index()];
                            *e = an.merge(s, *e);
                            any = true;
                        }
                    }
                }
            }
        }
        if any {
            out.push((l.out_pin, slew));
        }
    }
    Ok(out)
}

/// The `set_driving_cell` effects of every port with one, per `[tr][an]` (`STA-REQ-0218`).
pub(crate) fn driving_cell_effects<D: Derate>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    constraints: &CompiledConstraints,
    canon_totals: &[[[f64; 2]; 2]],
    derate: &D,
) -> FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]> {
    let n_nodes = graph.num_nodes();
    let mut driving_cells: FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]> =
        FxHashMap::default();
    for (&port_node, specs) in &constraints.driving_cells {
        if port_node.index() >= n_nodes {
            continue;
        }
        let net_cap = node_net_cap(netview, canon_totals, port_node);
        let entry = driving_cells.entry(port_node).or_insert([[None; 2]; 2]);
        for spec in specs {
            let evaled = eval_driving_cell(spec, port_node, lib, derate, net_cap);
            for tr in Tr::ALL {
                if !edge_matches(spec.edge, tr) {
                    continue;
                }
                for an in An::ALL {
                    if !minmax_matches(spec.min_max, an) {
                        continue;
                    }
                    if let Some(eff) = evaled[tr.index()][an.index()] {
                        entry[tr.index()][an.index()] = Some(eff);
                    }
                }
            }
        }
    }
    driving_cells
}

/// `(incr_drive_delay_ns, port_slew_ns)` of port `node` for `(tr, an)`: `set_driving_cell`
/// replaces `set_input_transition` for the transitions and bounds it covers (`STA-REQ-0218`).
pub(crate) fn resolve_port_drive(
    driving_cells: &FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
    constraints: &CompiledConstraints,
    node: NodeId,
    tr: Tr,
    an: An,
) -> (f64, f64) {
    if let Some(eff) = driving_cells
        .get(&node)
        .and_then(|t| t[tr.index()][an.index()])
    {
        return (eff.incr_delay_ns, eff.slew_ns);
    }
    let slew = constraints
        .input_transitions
        .get(&node)
        .and_then(|it| it.slew_ns[an.index()][tr.index()])
        .unwrap_or(0.0);
    (0.0, slew)
}

/// The source nodes of every clock.
pub(crate) fn clock_source_nodes(
    constraints: &CompiledConstraints,
) -> rustc_hash::FxHashSet<NodeId> {
    constraints
        .clocks
        .iter()
        .flat_map(|ck| ck.sources.iter().copied())
        .collect()
}

/// The transitions `[tr][an]` that seed a propagation without an arrival of their own, in node
/// order: every primary input or inout port outside `skip`, from its driving cell or input
/// transition (default 0, `STA-REQ-0108`), and the outputs of tie cells, 0 (`STA-REQ-0117`).
pub(crate) fn untagged_seeds(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    driving_cells: &FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
    skip: &rustc_hash::FxHashSet<NodeId>,
) -> Vec<(NodeId, [[f64; 2]; 2])> {
    let n_nodes = graph.num_nodes();
    let mut out = Vec::new();
    for i in 0..netview.num_port_bits().min(n_nodes) {
        let node = NodeId(i as u32);
        if skip.contains(&node) {
            continue;
        }
        let is_in = matches!(
            netview.node_kind(node),
            crate::engine::input::NodeKind::PortBit {
                dir: rusteda_core::liberty::Direction::Input
                    | rusteda_core::liberty::Direction::Inout,
                ..
            }
        );
        if !is_in {
            continue;
        }
        let mut slew = [[0.0; 2]; 2];
        for tr in Tr::ALL {
            for an in An::ALL {
                slew[tr.index()][an.index()] =
                    resolve_port_drive(driving_cells, constraints, node, tr, an).1;
            }
        }
        out.push((node, slew));
    }
    for &node in graph.tie_outputs() {
        if node.index() < n_nodes && !skip.contains(&node) {
            out.push((node, [[0.0; 2]; 2]));
        }
    }
    out
}

/// Evaluates a `set_driving_cell` specification across `[tr][an]` (`STA-REQ-0218`).
pub fn eval_driving_cell<D: Derate>(
    spec: &DrivingCellSpec,
    port_node: NodeId,
    lib: &TimingLib,
    derate: &D,
    net_cap: [[f64; 2]; 2],
) -> [[Option<DriveCellEffect>; 2]; 2] {
    let mut out = [[None; 2]; 2];
    let Some(tc) = lib.cell(&spec.lib_cell) else {
        return out;
    };
    let to_pin = if spec.pin.is_empty() {
        tc.pins.iter().position(|p| p.output)
    } else {
        tc.pin(&spec.pin)
    };
    let Some(to_pin) = to_pin else {
        return out;
    };
    let from_pin = match &spec.from_pin {
        Some(fp) => {
            let Some(idx) = tc.pin(fp) else {
                return out;
            };
            Some(idx)
        }
        None => None,
    };

    let s_in = [
        spec.input_transition_rise_ns.unwrap_or(0.0),
        spec.input_transition_fall_ns.unwrap_or(0.0),
    ];
    let mult = spec.multiply_by.unwrap_or(1.0);

    for &arc_id in &tc.arcs {
        let pa = lib.arc(arc_id);
        if pa.to != to_pin || !matches!(pa.kind, PropArcKind::Combinational { .. }) {
            continue;
        }
        if let Some(fp) = from_pin
            && pa.from != fp
        {
            continue;
        }
        for in_tr in Tr::ALL {
            let outs = pa.out_transitions(in_tr);
            let s_in_val = s_in[in_tr.index()];
            for to in Tr::ALL {
                if !outs[to.index()] {
                    continue;
                }
                for an in An::ALL {
                    let c_net = net_cap[to.index()][an.index()];
                    let c_eff = if mult != 0.0 { c_net / mult } else { c_net };
                    let ds_full =
                        lib.arc_delay(arc_id, to, an, s_in_val, DriverLoad::Lumped { c: c_eff });
                    let ds_zero =
                        lib.arc_delay(arc_id, to, an, s_in_val, DriverLoad::Lumped { c: 0.0 });
                    let raw_incr = ds_full.delay - ds_zero.delay;
                    let incr_delay_ns = if spec.dont_scale {
                        raw_incr
                    } else {
                        raw_incr * derate.factor(DelayKind::Cell, PathKind::Data, port_node, an, to)
                    };
                    let slew_ns = ds_full.slew.max(0.0);
                    let slot = &mut out[to.index()][an.index()];
                    *slot = Some(match *slot {
                        None => DriveCellEffect {
                            incr_delay_ns,
                            slew_ns,
                        },
                        Some(prev) => DriveCellEffect {
                            incr_delay_ns: an.merge(incr_delay_ns, prev.incr_delay_ns),
                            slew_ns: an.merge(slew_ns, prev.slew_ns),
                        },
                    });
                }
            }
        }
    }
    out
}

#[inline]
fn edge_matches(scope: EdgeScope, tr: Tr) -> bool {
    match scope {
        EdgeScope::Rise => tr == Tr::Rise,
        EdgeScope::Fall => tr == Tr::Fall,
        EdgeScope::Both => true,
    }
}

#[inline]
fn minmax_matches(scope: MinMaxScope, an: An) -> bool {
    match scope {
        MinMaxScope::Min => an == An::Early,
        MinMaxScope::Max => an == An::Late,
        MinMaxScope::Both => true,
    }
}

pub(crate) fn map_cell_arcs<'a>(
    netview: &NetlistView<'_>,
    lib: &'a TimingLib,
) -> Result<Vec<&'a [LibArc]>, Diag> {
    let n_cells = netview.num_cells();
    let mut out = Vec::with_capacity(n_cells);
    for c in 0..n_cells {
        let name = netview.cell_name(c as u32);
        let tc = lib.cell(name).ok_or_else(|| {
            Diag::error(
                "T0013",
                format!(
                    "corner '{}': netlist cell '{name}' is not present in the corner library",
                    lib.corner
                ),
            )
        })?;
        out.push(tc.arcs.as_slice());
    }
    Ok(out)
}

pub(crate) fn canonical_net_totals(
    netview: &NetlistView<'_>,
    net_loads: &NetLoads,
) -> Vec<[[f64; 2]; 2]> {
    let n_nets = netview.num_nets();
    let mut totals = vec![[[0.0; 2]; 2]; n_nets];
    for r in 0..n_nets {
        let nr = crate::engine::types::NetRow(r as u32);
        let c = net_loads.totals.get(r).copied().unwrap_or_default();
        let canon = match netview.resolve_net(nr) {
            NetTarget::Net(cn) => cn.index(),
            NetTarget::Const(_) => r,
        };
        if canon < n_nets {
            for tr in 0..2 {
                for an in 0..2 {
                    totals[canon][tr][an] += c[tr][an];
                }
            }
        }
    }
    for r in 0..n_nets {
        let nr = crate::engine::types::NetRow(r as u32);
        if let NetTarget::Net(cn) = netview.resolve_net(nr)
            && cn.index() < n_nets
        {
            totals[r] = totals[cn.index()];
        }
    }
    totals
}

#[inline]
pub(crate) fn node_net_cap(
    netview: &NetlistView<'_>,
    canon_totals: &[[[f64; 2]; 2]],
    node: NodeId,
) -> [[f64; 2]; 2] {
    match netview.node_net(node) {
        NetTarget::Net(nr) => canon_totals.get(nr.index()).copied().unwrap_or_default(),
        NetTarget::Const(_) => [[0.0; 2]; 2],
    }
}
