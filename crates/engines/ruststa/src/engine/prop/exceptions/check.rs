// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Endpoint check evaluation under timing exceptions (`STA-REQ-0250` to `STA-REQ-0270`, owner: E8).

use std::collections::BTreeMap;

use rusteda_core::liberty::Direction;

use super::helpers::{
    InferredClockGateCheck, clock_latency, clock_uncertainty, discover_clock_gating_checks,
    find_cell_check, hold_relation_ns, io_clock_latency, max_time_borrow_for_latch,
    open_relation_ns, setup_relation_ns, source_edges,
};
use super::resolve::{
    AnalysisKind, EndpointExceptionResult, EndpointStatus, ExceptionTable, PathAnalysisResolution,
};
use super::state::{ExceptionStateTable, PropagationTagTable};
use crate::engine::delay::lut::NO_TABLE;
use crate::engine::graph::{ArcKind, CheckArc, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, CompiledIoDelay, DesignIndex};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::prop::lower::LoweredResult;
use crate::engine::types::{An, ClockId, NodeId, Tr};

#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate_all_endpoints(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    index: &DesignIndex,
    tlib: &TimingLib,
    compiled: &CompiledConstraints,
    clock_sense: &[Vec<(ClockId, u8)>],
    canon_totals: &[[[f64; 2]; 2]],
    state_table: &ExceptionStateTable,
    tag_table: &PropagationTagTable,
    exc_table: &mut ExceptionTable,
    prop_res: &LoweredResult,
) -> Vec<EndpointExceptionResult> {
    let mut results = Vec::new();

    // Group `graph.checks()` by `data_pin` in first-seen order.
    let mut check_pins_order: Vec<NodeId> = Vec::new();
    let mut checks_by_pin: BTreeMap<NodeId, Vec<(usize, CheckArc)>> = BTreeMap::new();
    for (ci, &ch) in graph.checks().iter().enumerate() {
        let entry = checks_by_pin.entry(ch.data_pin).or_default();
        if entry.is_empty() {
            check_pins_order.push(ch.data_pin);
        }
        entry.push((ci, ch));
    }

    // Identify latch data pins (`ArcKind::LatchData` sources).
    let mut latch_data_pins: BTreeMap<NodeId, (u32, NodeId)> = BTreeMap::new();
    for arc in graph.arcs() {
        if let ArcKind::LatchData { inst, .. } = arc.kind {
            latch_data_pins.insert(arc.src, (inst, arc.dst));
        }
    }

    // 1. Evaluate sequential & latch check endpoints.
    for data_pin in check_pins_order {
        let checks = &checks_by_pin[&data_pin];
        let pin_name = index.node_name(data_pin).to_string();
        let latch_info = latch_data_pins.get(&data_pin).copied();

        let ep_res = evaluate_pin_endpoint(
            nv,
            graph,
            tlib,
            compiled,
            clock_sense,
            canon_totals,
            state_table,
            tag_table,
            exc_table,
            prop_res,
            data_pin,
            pin_name,
            checks,
            latch_info,
        );
        results.push(ep_res);
    }

    // 2. Evaluate inferred clock-gating enable endpoints (`STA-REQ-0216`, `STA-REQ-0225`,
    // `STA-REQ-0268`).
    let cg_checks = discover_clock_gating_checks(nv, index, tlib, clock_sense);
    for cg in cg_checks {
        if checks_by_pin.contains_key(&cg.enable_pin) {
            continue;
        }
        let pin_name = index.node_name(cg.enable_pin).to_string();
        let ep_res = evaluate_clock_gating_endpoint(
            graph,
            compiled,
            clock_sense,
            state_table,
            tag_table,
            exc_table,
            prop_res,
            cg,
            pin_name,
        );
        results.push(ep_res);
    }

    // 3. Evaluate top-level output ports (`STA-REQ-0212`, `STA-REQ-0262`..`0265`, `STA-REQ-0268`).
    for i in 0..nv.num_port_bits() {
        let port_node = NodeId(i as u32);
        let NodeKind::PortBit { dir, .. } = nv.node_kind(port_node) else {
            continue;
        };
        if !matches!(dir, Direction::Output | Direction::Inout) {
            continue;
        }
        let pin_name = index.node_name(port_node).to_string();
        let ep_res = evaluate_output_port_endpoint(
            graph,
            compiled,
            state_table,
            tag_table,
            exc_table,
            prop_res,
            port_node,
            pin_name,
        );
        results.push(ep_res);
    }

    results
}

#[allow(clippy::too_many_arguments)]
fn evaluate_pin_endpoint(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    tlib: &TimingLib,
    compiled: &CompiledConstraints,
    clock_sense: &[Vec<(ClockId, u8)>],
    canon_totals: &[[[f64; 2]; 2]],
    state_table: &ExceptionStateTable,
    tag_table: &PropagationTagTable,
    exc_table: &mut ExceptionTable,
    prop_res: &LoweredResult,
    data_pin: NodeId,
    pin_name: String,
    checks: &[(usize, CheckArc)],
    latch_info: Option<(u32, NodeId)>,
) -> EndpointExceptionResult {
    let any_clocked = checks
        .iter()
        .any(|(_, ch)| !clock_sense[ch.clock_pin.index()].is_empty());
    if !any_clocked {
        return EndpointExceptionResult {
            pin: pin_name,
            node: data_pin,
            setup_slack: None,
            hold_slack: None,
            status: EndpointStatus::Unclocked,
        };
    }

    let mut setup_slack: Option<f64> = None;
    let mut hold_slack: Option<f64> = None;
    let mut had_reaching_arrival = false;
    let mut had_excluded_path = false;

    for &(_ci, ch) in checks {
        let cell_row = nv.frozen().instances[ch.inst as usize].cell as usize;
        let cell_name = nv.cell_name(cell_row as u32);
        let Some(tc) = tlib.cell(cell_name) else {
            continue;
        };
        let Some(lib_check) = find_cell_check(tc, ch) else {
            continue;
        };
        let analysis = if ch.kind.is_setup() {
            AnalysisKind::Setup
        } else {
            AnalysisKind::Hold
        };
        let an_arr = match analysis {
            AnalysisKind::Setup => An::Late,
            AnalysisKind::Hold => An::Early,
        };

        for &(c_id, bits) in &clock_sense[ch.clock_pin.index()] {
            let ck_c = &compiled.clocks[c_id.index()];
            for e_c in source_edges(ch.edge, bits) {
                let cap_clk = Some((c_id, e_c));
                let rel_slew = ck_c.transition_ns[an_arr.index()][e_c.index()];

                let latch_pulse = latch_info.and_then(|(inst, out_pin)| {
                    let open_pin_edge = graph
                        .launches()
                        .iter()
                        .find(|l| {
                            l.inst == inst && l.clock_pin == ch.clock_pin && l.out_pin == out_pin
                        })
                        .map_or_else(|| ch.edge.opposite(), |l| l.edge);
                    let e_open = *source_edges(open_pin_edge, bits).first()?;
                    let w = setup_relation_ns(
                        ck_c.period_ns,
                        ck_c.waveform_ns,
                        e_open,
                        ck_c.period_ns,
                        ck_c.waveform_ns,
                        e_c,
                    )?;
                    let max_b =
                        max_time_borrow_for_latch(compiled, inst, data_pin, ch.clock_pin, c_id);
                    Some((w, max_b))
                });

                for (tag_idx, tag_key) in tag_table.tags().iter().enumerate() {
                    for tr in Tr::ALL {
                        if lib_check.table[tr.index()] == NO_TABLE {
                            continue;
                        }
                        let raw_arr = prop_res.arrival(graph, data_pin, tag_idx, tr, an_arr);
                        if !raw_arr.is_finite() {
                            continue;
                        }
                        if tag_key.launch_clock.is_some() {
                            had_reaching_arrival = true;
                        }

                        let res = exc_table.resolve_path_analysis(
                            state_table,
                            tag_key.exc_state,
                            tag_key.launch_clock,
                            data_pin,
                            tr,
                            cap_clk,
                            analysis,
                        );
                        if res.is_excluded_by_exception_or_group() {
                            had_excluded_path = true;
                            continue;
                        }
                        if !res.is_timed() {
                            continue;
                        }
                        had_reaching_arrival = true;

                        let data_slew = prop_res.slew(graph, data_pin, tr, an_arr);
                        let load = match latch_info {
                            Some((_, out_pin)) => match nv.node_net(out_pin) {
                                NetTarget::Net(nr) => canon_totals
                                    .get(nr.index())
                                    .map_or(0.0, |t| t[tr.index()][an_arr.index()]),
                                NetTarget::Const(_) => 0.0,
                            },
                            None => 0.0,
                        };
                        let margin = tlib.eval_check(lib_check, tr, rel_slew, data_slew, load);

                        match (analysis, res) {
                            (
                                AnalysisKind::Setup,
                                PathAnalysisResolution::TimedSetupMulticycle { setup },
                            ) => {
                                let (l_id, e_l) =
                                    tag_key.launch_clock.expect("clocked multicycle path");
                                let ck_l = &compiled.clocks[l_id.index()];
                                let Some(d_s_base) = setup_relation_ns(
                                    ck_l.period_ns,
                                    ck_l.waveform_ns,
                                    e_l,
                                    ck_c.period_ns,
                                    ck_c.waveform_ns,
                                    e_c,
                                ) else {
                                    continue;
                                };
                                let delta_s = setup.shift_ns(ck_l.period_ns, ck_c.period_ns);
                                let d_s = d_s_base + delta_s;
                                let lat_c = clock_latency(
                                    compiled,
                                    c_id,
                                    e_c,
                                    An::Early,
                                    Some(ch.clock_pin),
                                );
                                let u_s = clock_uncertainty(
                                    compiled,
                                    l_id,
                                    e_l,
                                    c_id,
                                    e_c,
                                    AnalysisKind::Setup,
                                    Some(ch.clock_pin),
                                );
                                let slk = if let Some((w_pulse, max_b)) = latch_pulse {
                                    let d_open = (d_s_base - w_pulse) + delta_s;
                                    let r_open = d_open + lat_c - u_s;
                                    let r_close_raw = d_s + lat_c - margin - u_s;
                                    let r_close = match max_b {
                                        Some(b) => r_close_raw.min(r_open + b),
                                        None => r_close_raw,
                                    };
                                    if raw_arr <= r_open {
                                        r_open - raw_arr
                                    } else if raw_arr <= r_close {
                                        0.0
                                    } else {
                                        r_close - raw_arr
                                    }
                                } else {
                                    let req = d_s + lat_c - margin - u_s;
                                    req - raw_arr
                                };
                                setup_slack = Some(setup_slack.map_or(slk, |prev| prev.min(slk)));
                            }
                            (
                                AnalysisKind::Setup,
                                PathAnalysisResolution::TimedMaxDelay {
                                    max_delay_ns,
                                    ignore_clock_latency,
                                    ..
                                },
                            ) => {
                                let launch_lat = tag_key.launch_latency()[An::Late.index()];
                                let eff_arr = if ignore_clock_latency {
                                    raw_arr - launch_lat
                                } else {
                                    raw_arr
                                };
                                let req = if ignore_clock_latency {
                                    max_delay_ns - margin
                                } else {
                                    let lat_c = clock_latency(
                                        compiled,
                                        c_id,
                                        e_c,
                                        An::Early,
                                        Some(ch.clock_pin),
                                    );
                                    let u_s = match tag_key.launch_clock {
                                        Some((l_id, e_l)) => clock_uncertainty(
                                            compiled,
                                            l_id,
                                            e_l,
                                            c_id,
                                            e_c,
                                            AnalysisKind::Setup,
                                            Some(ch.clock_pin),
                                        ),
                                        None => ck_c.uncertainty_setup_ns[e_c.index()],
                                    };
                                    max_delay_ns + lat_c - margin - u_s
                                };
                                let slk = req - eff_arr;
                                setup_slack = Some(setup_slack.map_or(slk, |prev| prev.min(slk)));
                            }
                            (
                                AnalysisKind::Hold,
                                PathAnalysisResolution::TimedHoldMulticycle { setup, hold },
                            ) => {
                                let (l_id, e_l) =
                                    tag_key.launch_clock.expect("clocked multicycle path");
                                let ck_l = &compiled.clocks[l_id.index()];
                                let Some(d_h_base) = hold_relation_ns(
                                    ck_l.period_ns,
                                    ck_l.waveform_ns,
                                    e_l,
                                    ck_c.period_ns,
                                    ck_c.waveform_ns,
                                    e_c,
                                ) else {
                                    continue;
                                };
                                let d_h = d_h_base + setup.shift_ns(ck_l.period_ns, ck_c.period_ns)
                                    - hold.shift_ns(ck_l.period_ns, ck_c.period_ns);
                                let lat_c = clock_latency(
                                    compiled,
                                    c_id,
                                    e_c,
                                    An::Late,
                                    Some(ch.clock_pin),
                                );
                                let u_h = clock_uncertainty(
                                    compiled,
                                    l_id,
                                    e_l,
                                    c_id,
                                    e_c,
                                    AnalysisKind::Hold,
                                    Some(ch.clock_pin),
                                );
                                let req = d_h + lat_c + margin + u_h;
                                let slk = raw_arr - req;
                                hold_slack = Some(hold_slack.map_or(slk, |prev| prev.min(slk)));
                            }
                            (
                                AnalysisKind::Hold,
                                PathAnalysisResolution::TimedMinDelay {
                                    min_delay_ns,
                                    ignore_clock_latency,
                                    ..
                                },
                            ) => {
                                let launch_lat = tag_key.launch_latency()[An::Early.index()];
                                let eff_arr = if ignore_clock_latency {
                                    raw_arr - launch_lat
                                } else {
                                    raw_arr
                                };
                                let req = if ignore_clock_latency {
                                    min_delay_ns + margin
                                } else {
                                    let lat_c = clock_latency(
                                        compiled,
                                        c_id,
                                        e_c,
                                        An::Late,
                                        Some(ch.clock_pin),
                                    );
                                    let u_h = match tag_key.launch_clock {
                                        Some((l_id, e_l)) => clock_uncertainty(
                                            compiled,
                                            l_id,
                                            e_l,
                                            c_id,
                                            e_c,
                                            AnalysisKind::Hold,
                                            Some(ch.clock_pin),
                                        ),
                                        None => ck_c.uncertainty_hold_ns[e_c.index()],
                                    };
                                    min_delay_ns + lat_c + margin + u_h
                                };
                                let slk = eff_arr - req;
                                hold_slack = Some(hold_slack.map_or(slk, |prev| prev.min(slk)));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    let status = if setup_slack.is_some() || hold_slack.is_some() {
        EndpointStatus::Constrained
    } else if had_reaching_arrival && had_excluded_path {
        EndpointStatus::FalsePathOnly
    } else {
        EndpointStatus::NoDataArrival
    };

    EndpointExceptionResult {
        pin: pin_name,
        node: data_pin,
        setup_slack,
        hold_slack,
        status,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_clock_gating_endpoint(
    graph: &TimingGraph,
    compiled: &CompiledConstraints,
    clock_sense: &[Vec<(ClockId, u8)>],
    state_table: &ExceptionStateTable,
    tag_table: &PropagationTagTable,
    exc_table: &mut ExceptionTable,
    prop_res: &LoweredResult,
    cg: InferredClockGateCheck,
    pin_name: String,
) -> EndpointExceptionResult {
    let lead_pin_edge = if cg.active_low { Tr::Fall } else { Tr::Rise };
    let trail_pin_edge = lead_pin_edge.opposite();

    let mut setup_slack: Option<f64> = None;
    let mut hold_slack: Option<f64> = None;
    let mut had_reaching_arrival = false;
    let mut had_excluded_path = false;

    for &(c_id, bits) in &clock_sense[cg.clock_pin.index()] {
        let ck_c = &compiled.clocks[c_id.index()];

        // 1. Setup against leading edge (`STA-REQ-0225`, `STA-REQ-0268`).
        for e_lead in source_edges(lead_pin_edge, bits) {
            let cap_clk = Some((c_id, e_lead));
            for (tag_idx, tag_key) in tag_table.tags().iter().enumerate() {
                for tr in Tr::ALL {
                    let raw_arr = prop_res.arrival(graph, cg.enable_pin, tag_idx, tr, An::Late);
                    if !raw_arr.is_finite() {
                        continue;
                    }
                    if tag_key.launch_clock.is_some() {
                        had_reaching_arrival = true;
                    }
                    let res = exc_table.resolve_path_analysis(
                        state_table,
                        tag_key.exc_state,
                        tag_key.launch_clock,
                        cg.enable_pin,
                        tr,
                        cap_clk,
                        AnalysisKind::Setup,
                    );
                    if res.is_excluded_by_exception_or_group() {
                        had_excluded_path = true;
                        continue;
                    }
                    if !res.is_timed() {
                        continue;
                    }
                    had_reaching_arrival = true;
                    match res {
                        PathAnalysisResolution::TimedSetupMulticycle { setup } => {
                            let (l_id, e_l) =
                                tag_key.launch_clock.expect("clocked multicycle path");
                            let ck_l = &compiled.clocks[l_id.index()];
                            let Some(d_s_base) = setup_relation_ns(
                                ck_l.period_ns,
                                ck_l.waveform_ns,
                                e_l,
                                ck_c.period_ns,
                                ck_c.waveform_ns,
                                e_lead,
                            ) else {
                                continue;
                            };
                            let d_s = d_s_base + setup.shift_ns(ck_l.period_ns, ck_c.period_ns);
                            let lat_c = clock_latency(
                                compiled,
                                c_id,
                                e_lead,
                                An::Early,
                                Some(cg.clock_pin),
                            );
                            let u_s = clock_uncertainty(
                                compiled,
                                l_id,
                                e_l,
                                c_id,
                                e_lead,
                                AnalysisKind::Setup,
                                Some(cg.clock_pin),
                            );
                            let req = d_s + lat_c - u_s;
                            let slk = req - raw_arr;
                            setup_slack = Some(setup_slack.map_or(slk, |prev| prev.min(slk)));
                        }
                        PathAnalysisResolution::TimedMaxDelay {
                            max_delay_ns,
                            ignore_clock_latency,
                            ..
                        } => {
                            let launch_lat = tag_key.launch_latency()[An::Late.index()];
                            let eff_arr = if ignore_clock_latency {
                                raw_arr - launch_lat
                            } else {
                                raw_arr
                            };
                            let req = if ignore_clock_latency {
                                max_delay_ns
                            } else {
                                let lat_c = clock_latency(
                                    compiled,
                                    c_id,
                                    e_lead,
                                    An::Early,
                                    Some(cg.clock_pin),
                                );
                                let u_s = match tag_key.launch_clock {
                                    Some((l_id, e_l)) => clock_uncertainty(
                                        compiled,
                                        l_id,
                                        e_l,
                                        c_id,
                                        e_lead,
                                        AnalysisKind::Setup,
                                        Some(cg.clock_pin),
                                    ),
                                    None => ck_c.uncertainty_setup_ns[e_lead.index()],
                                };
                                max_delay_ns + lat_c - u_s
                            };
                            let slk = req - eff_arr;
                            setup_slack = Some(setup_slack.map_or(slk, |prev| prev.min(slk)));
                        }
                        _ => {}
                    }
                }
            }
        }

        // 2. Hold against trailing edge (`STA-REQ-0225`, `STA-REQ-0268`).
        for e_trail in source_edges(trail_pin_edge, bits) {
            let cap_clk = Some((c_id, e_trail));
            for (tag_idx, tag_key) in tag_table.tags().iter().enumerate() {
                for tr in Tr::ALL {
                    let raw_arr = prop_res.arrival(graph, cg.enable_pin, tag_idx, tr, An::Early);
                    if !raw_arr.is_finite() {
                        continue;
                    }
                    if tag_key.launch_clock.is_some() {
                        had_reaching_arrival = true;
                    }
                    let res = exc_table.resolve_path_analysis(
                        state_table,
                        tag_key.exc_state,
                        tag_key.launch_clock,
                        cg.enable_pin,
                        tr,
                        cap_clk,
                        AnalysisKind::Hold,
                    );
                    if res.is_excluded_by_exception_or_group() {
                        had_excluded_path = true;
                        continue;
                    }
                    if !res.is_timed() {
                        continue;
                    }
                    had_reaching_arrival = true;
                    match res {
                        PathAnalysisResolution::TimedHoldMulticycle { setup, hold } => {
                            let (l_id, e_l) =
                                tag_key.launch_clock.expect("clocked multicycle path");
                            let ck_l = &compiled.clocks[l_id.index()];
                            let Some(d_h_base) = open_relation_ns(
                                ck_l.period_ns,
                                ck_l.waveform_ns,
                                e_l,
                                ck_c.period_ns,
                                ck_c.waveform_ns,
                                e_trail,
                            ) else {
                                continue;
                            };
                            let d_h = d_h_base + setup.shift_ns(ck_l.period_ns, ck_c.period_ns)
                                - hold.shift_ns(ck_l.period_ns, ck_c.period_ns);
                            let lat_c = clock_latency(
                                compiled,
                                c_id,
                                e_trail,
                                An::Late,
                                Some(cg.clock_pin),
                            );
                            let u_h = clock_uncertainty(
                                compiled,
                                l_id,
                                e_l,
                                c_id,
                                e_trail,
                                AnalysisKind::Hold,
                                Some(cg.clock_pin),
                            );
                            let req = d_h + lat_c + u_h;
                            let slk = raw_arr - req;
                            hold_slack = Some(hold_slack.map_or(slk, |prev| prev.min(slk)));
                        }
                        PathAnalysisResolution::TimedMinDelay {
                            min_delay_ns,
                            ignore_clock_latency,
                            ..
                        } => {
                            let launch_lat = tag_key.launch_latency()[An::Early.index()];
                            let eff_arr = if ignore_clock_latency {
                                raw_arr - launch_lat
                            } else {
                                raw_arr
                            };
                            let req = if ignore_clock_latency {
                                min_delay_ns
                            } else {
                                let lat_c = clock_latency(
                                    compiled,
                                    c_id,
                                    e_trail,
                                    An::Late,
                                    Some(cg.clock_pin),
                                );
                                let u_h = match tag_key.launch_clock {
                                    Some((l_id, e_l)) => clock_uncertainty(
                                        compiled,
                                        l_id,
                                        e_l,
                                        c_id,
                                        e_trail,
                                        AnalysisKind::Hold,
                                        Some(cg.clock_pin),
                                    ),
                                    None => ck_c.uncertainty_hold_ns[e_trail.index()],
                                };
                                min_delay_ns + lat_c + u_h
                            };
                            let slk = eff_arr - req;
                            hold_slack = Some(hold_slack.map_or(slk, |prev| prev.min(slk)));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let status = if setup_slack.is_some() || hold_slack.is_some() {
        EndpointStatus::Constrained
    } else if had_reaching_arrival && had_excluded_path {
        EndpointStatus::FalsePathOnly
    } else {
        EndpointStatus::NoDataArrival
    };

    EndpointExceptionResult {
        pin: pin_name,
        node: cg.enable_pin,
        setup_slack,
        hold_slack,
        status,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_output_port_endpoint(
    graph: &TimingGraph,
    compiled: &CompiledConstraints,
    state_table: &ExceptionStateTable,
    tag_table: &PropagationTagTable,
    exc_table: &mut ExceptionTable,
    prop_res: &LoweredResult,
    port_node: NodeId,
    pin_name: String,
) -> EndpointExceptionResult {
    let out_delays: Vec<&CompiledIoDelay> = compiled
        .output_delays
        .iter()
        .filter(|d| d.node == port_node && d.clock.is_some())
        .collect();

    let mut setup_slack: Option<f64> = None;
    let mut hold_slack: Option<f64> = None;
    let mut had_reaching_arrival = false;
    let mut had_excluded_path = false;

    if !out_delays.is_empty() {
        for od in &out_delays {
            let c_id = od.clock.expect("filtered Some");
            let e_c = od.clock_edge;
            let ck_c = &compiled.clocks[c_id.index()];
            let cap_clk = Some((c_id, e_c));

            for (tag_idx, tag_key) in tag_table.tags().iter().enumerate() {
                for tr in Tr::ALL {
                    // Setup (`-max` output delay).
                    if let Some(v_max) = od.delay_ns[An::Late.index()][tr.index()] {
                        let raw_arr = prop_res.arrival(graph, port_node, tag_idx, tr, An::Late);
                        if raw_arr.is_finite() {
                            if tag_key.launch_clock.is_some() {
                                had_reaching_arrival = true;
                            }
                            let res = exc_table.resolve_path_analysis(
                                state_table,
                                tag_key.exc_state,
                                tag_key.launch_clock,
                                port_node,
                                tr,
                                cap_clk,
                                AnalysisKind::Setup,
                            );
                            if res.is_excluded_by_exception_or_group() {
                                had_excluded_path = true;
                            } else if res.is_timed() {
                                had_reaching_arrival = true;
                                match res {
                                    PathAnalysisResolution::TimedSetupMulticycle { setup } => {
                                        let (l_id, e_l) =
                                            tag_key.launch_clock.expect("clocked multicycle path");
                                        let ck_l = &compiled.clocks[l_id.index()];
                                        if let Some(d_s_base) = setup_relation_ns(
                                            ck_l.period_ns,
                                            ck_l.waveform_ns,
                                            e_l,
                                            ck_c.period_ns,
                                            ck_c.waveform_ns,
                                            e_c,
                                        ) {
                                            let d_s = d_s_base
                                                + setup.shift_ns(ck_l.period_ns, ck_c.period_ns);
                                            let lat_c = io_clock_latency(
                                                compiled,
                                                od,
                                                c_id,
                                                e_c,
                                                An::Early,
                                            );
                                            let u_s = clock_uncertainty(
                                                compiled,
                                                l_id,
                                                e_l,
                                                c_id,
                                                e_c,
                                                AnalysisKind::Setup,
                                                Some(port_node),
                                            );
                                            let req = d_s + lat_c - v_max - u_s;
                                            let slk = req - raw_arr;
                                            setup_slack =
                                                Some(setup_slack.map_or(slk, |p| p.min(slk)));
                                        }
                                    }
                                    PathAnalysisResolution::TimedMaxDelay {
                                        max_delay_ns,
                                        ignore_clock_latency,
                                        ..
                                    } => {
                                        // Under `STA-REQ-0262`, `lat_C = 0` for output ports!
                                        let launch_lat = tag_key.launch_latency()[An::Late.index()];
                                        let eff_arr = if ignore_clock_latency {
                                            raw_arr - launch_lat
                                        } else {
                                            raw_arr
                                        };
                                        let req = if ignore_clock_latency {
                                            max_delay_ns - v_max
                                        } else {
                                            let u_s = match tag_key.launch_clock {
                                                Some((l_id, e_l)) => clock_uncertainty(
                                                    compiled,
                                                    l_id,
                                                    e_l,
                                                    c_id,
                                                    e_c,
                                                    AnalysisKind::Setup,
                                                    Some(port_node),
                                                ),
                                                None => ck_c.uncertainty_setup_ns[e_c.index()],
                                            };
                                            max_delay_ns - v_max - u_s
                                        };
                                        let slk = req - eff_arr;
                                        setup_slack = Some(setup_slack.map_or(slk, |p| p.min(slk)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // Hold (`-min` output delay).
                    if let Some(v_min) = od.delay_ns[An::Early.index()][tr.index()] {
                        let raw_arr = prop_res.arrival(graph, port_node, tag_idx, tr, An::Early);
                        if raw_arr.is_finite() {
                            if tag_key.launch_clock.is_some() {
                                had_reaching_arrival = true;
                            }
                            let res = exc_table.resolve_path_analysis(
                                state_table,
                                tag_key.exc_state,
                                tag_key.launch_clock,
                                port_node,
                                tr,
                                cap_clk,
                                AnalysisKind::Hold,
                            );
                            if res.is_excluded_by_exception_or_group() {
                                had_excluded_path = true;
                            } else if res.is_timed() {
                                had_reaching_arrival = true;
                                match res {
                                    PathAnalysisResolution::TimedHoldMulticycle { setup, hold } => {
                                        let (l_id, e_l) =
                                            tag_key.launch_clock.expect("clocked multicycle path");
                                        let ck_l = &compiled.clocks[l_id.index()];
                                        if let Some(d_h_base) = hold_relation_ns(
                                            ck_l.period_ns,
                                            ck_l.waveform_ns,
                                            e_l,
                                            ck_c.period_ns,
                                            ck_c.waveform_ns,
                                            e_c,
                                        ) {
                                            let d_h = d_h_base
                                                + setup.shift_ns(ck_l.period_ns, ck_c.period_ns)
                                                - hold.shift_ns(ck_l.period_ns, ck_c.period_ns);
                                            let lat_c =
                                                io_clock_latency(compiled, od, c_id, e_c, An::Late);
                                            let u_h = clock_uncertainty(
                                                compiled,
                                                l_id,
                                                e_l,
                                                c_id,
                                                e_c,
                                                AnalysisKind::Hold,
                                                Some(port_node),
                                            );
                                            let req = d_h + lat_c - v_min + u_h;
                                            let slk = raw_arr - req;
                                            hold_slack =
                                                Some(hold_slack.map_or(slk, |p| p.min(slk)));
                                        }
                                    }
                                    PathAnalysisResolution::TimedMinDelay {
                                        min_delay_ns,
                                        ignore_clock_latency,
                                        ..
                                    } => {
                                        // Under `STA-REQ-0263`, `lat_C = 0` for output ports!
                                        let launch_lat =
                                            tag_key.launch_latency()[An::Early.index()];
                                        let eff_arr = if ignore_clock_latency {
                                            raw_arr - launch_lat
                                        } else {
                                            raw_arr
                                        };
                                        let req = if ignore_clock_latency {
                                            min_delay_ns - v_min
                                        } else {
                                            let u_h = match tag_key.launch_clock {
                                                Some((l_id, e_l)) => clock_uncertainty(
                                                    compiled,
                                                    l_id,
                                                    e_l,
                                                    c_id,
                                                    e_c,
                                                    AnalysisKind::Hold,
                                                    Some(port_node),
                                                ),
                                                None => ck_c.uncertainty_hold_ns[e_c.index()],
                                            };
                                            min_delay_ns - v_min + u_h
                                        };
                                        let slk = eff_arr - req;
                                        hold_slack = Some(hold_slack.map_or(slk, |p| p.min(slk)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    } else if exc_table.has_unclocked_to_delay_exception(port_node) {
        // Unclocked output port (`STA-REQ-0264`): timed only when `set_max_delay` or
        // `set_min_delay` wins, with `required = V`.
        for (tag_idx, tag_key) in tag_table.tags().iter().enumerate() {
            for tr in Tr::ALL {
                let raw_late = prop_res.arrival(graph, port_node, tag_idx, tr, An::Late);
                if raw_late.is_finite() {
                    let res = exc_table.resolve_path_analysis(
                        state_table,
                        tag_key.exc_state,
                        tag_key.launch_clock,
                        port_node,
                        tr,
                        None,
                        AnalysisKind::Setup,
                    );
                    if let PathAnalysisResolution::TimedMaxDelay {
                        max_delay_ns,
                        ignore_clock_latency,
                        ..
                    } = res
                    {
                        had_reaching_arrival = true;
                        let launch_lat = tag_key.launch_latency()[An::Late.index()];
                        let eff_arr = if ignore_clock_latency {
                            raw_late - launch_lat
                        } else {
                            raw_late
                        };
                        let slk = max_delay_ns - eff_arr;
                        setup_slack = Some(setup_slack.map_or(slk, |p| p.min(slk)));
                    } else if matches!(res, PathAnalysisResolution::UntimedFalsePath { .. }) {
                        had_reaching_arrival = true;
                        had_excluded_path = true;
                    }
                }

                let raw_early = prop_res.arrival(graph, port_node, tag_idx, tr, An::Early);
                if raw_early.is_finite() {
                    let res = exc_table.resolve_path_analysis(
                        state_table,
                        tag_key.exc_state,
                        tag_key.launch_clock,
                        port_node,
                        tr,
                        None,
                        AnalysisKind::Hold,
                    );
                    if let PathAnalysisResolution::TimedMinDelay {
                        min_delay_ns,
                        ignore_clock_latency,
                        ..
                    } = res
                    {
                        had_reaching_arrival = true;
                        let launch_lat = tag_key.launch_latency()[An::Early.index()];
                        let eff_arr = if ignore_clock_latency {
                            raw_early - launch_lat
                        } else {
                            raw_early
                        };
                        let slk = eff_arr - min_delay_ns;
                        hold_slack = Some(hold_slack.map_or(slk, |p| p.min(slk)));
                    } else if matches!(res, PathAnalysisResolution::UntimedFalsePath { .. }) {
                        had_reaching_arrival = true;
                        had_excluded_path = true;
                    }
                }
            }
        }
    }

    let status = if setup_slack.is_some() || hold_slack.is_some() {
        EndpointStatus::Constrained
    } else if !out_delays.is_empty() {
        if had_reaching_arrival && had_excluded_path {
            EndpointStatus::FalsePathOnly
        } else {
            EndpointStatus::NoDataArrival
        }
    } else if had_reaching_arrival && had_excluded_path {
        EndpointStatus::FalsePathOnly
    } else {
        EndpointStatus::UnconstrainedOutput
    };

    EndpointExceptionResult {
        pin: pin_name,
        node: port_node,
        setup_slack,
        hold_slack,
        status,
    }
}
