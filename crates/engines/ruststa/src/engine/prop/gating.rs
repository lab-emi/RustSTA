// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Clock-gating checks (owner: E5).
//!
//! Engine section C. A combinational cell where a clock meets a data signal gets a setup and a
//! hold check of the data against the clock: pairing for AND-type gates, and for OR-type gates
//! as derived in the specification.

use rusteda_core::job::Diag;

use super::checks::{EndpointCheckKind, SlackRecord, resolve_path_group};
use super::clocks::ideal::IdealClockModel;
use super::clocks::{ClockModel, SENSE_NEG, SENSE_POS};
use super::cppr::Cppr;
use super::lower::LoweredResult;
use super::relation::{non_negative_relation, setup_relation};
use super::tags::TagTable;
use crate::engine::graph::TimingGraph;
use crate::engine::input::NetlistView;
use crate::engine::input::constr::CompiledConstraints;
use crate::engine::input::libview::TimingLib;
use crate::engine::types::{An, NodeId, Tr};

/// A discovered combinational clock-gating check site (`STA-REQ-0224`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockGatingSite {
    /// Cell instance index in `FrozenNetlist::instances`.
    pub inst: u32,
    /// Constrained enable/data input pin [`NodeId`].
    pub enable_pin: NodeId,
    /// Related clock input pin [`NodeId`].
    pub clock_pin: NodeId,
    /// Clock pin transition leaving the controlling (inactive) value (`Rise` for AND/NAND-type
    /// where `inactive == 0`; `Fall` for OR/NOR-type where `inactive == 1`).
    pub lead_pin_tr: Tr,
    /// Clock pin transition returning to the controlling (inactive) value (`Fall` for AND/NAND-type;
    /// `Rise` for OR/NOR-type).
    pub trail_pin_tr: Tr,
}

fn find_instance_pin_node(
    netview: &NetlistView<'_>,
    inst: u32,
    cell_row: u32,
    pin_name: &str,
) -> Option<NodeId> {
    let frozen = netview.frozen();
    for pin_row in frozen.instance_pins(inst) {
        let cp = frozen.pin_cellpin[pin_row];
        if netview.cell_pin_name(cell_row, cp) == pin_name {
            return Some(netview.pin_row_node(pin_row as u32));
        }
    }
    None
}

/// Discovers all combinational clock-gating check sites in `netview` (`STA-REQ-0224`).
#[must_use]
pub fn discover_gating_sites(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    clock_model: &IdealClockModel,
) -> Vec<ClockGatingSite> {
    let mut sites = Vec::new();
    let frozen = netview.frozen();

    for inst in 0..netview.num_instances() {
        let cell_row = frozen.instances[inst].cell;
        let cell_name = netview.cell_name(cell_row);
        let Some(tc) = lib.cell(cell_name) else {
            continue;
        };
        if tc.is_sequential {
            continue;
        }
        for out_pin in &tc.pins {
            let Some((ins, tt)) = &out_pin.func else {
                continue;
            };
            let nv = ins.len();
            if nv < 2 {
                continue;
            }
            for (kpos, &kpin) in ins.iter().enumerate() {
                let Some(ck_node) =
                    find_instance_pin_node(netview, inst as u32, cell_row, &tc.pins[kpin].name)
                else {
                    continue;
                };
                if !clock_model.is_clock_network_pin(ck_node) {
                    continue;
                }
                let constant_at = |v: u64| -> bool {
                    let mut first = None;
                    for mt in 0..(1u64 << nv) {
                        if ((mt >> kpos) & 1) == v {
                            let bit = (tt >> mt) & 1;
                            match first {
                                None => first = Some(bit),
                                Some(prev) if prev != bit => return false,
                                Some(_) => {}
                            }
                        }
                    }
                    first.is_some()
                };
                let (lead_pin_tr, trail_pin_tr) = match (constant_at(0), constant_at(1)) {
                    (true, false) => (Tr::Rise, Tr::Fall),
                    (false, true) => (Tr::Fall, Tr::Rise),
                    _ => continue,
                };
                for (jpos, &jpin) in ins.iter().enumerate() {
                    if jpos == kpos {
                        continue;
                    }
                    let Some(en_node) =
                        find_instance_pin_node(netview, inst as u32, cell_row, &tc.pins[jpin].name)
                    else {
                        continue;
                    };
                    if clock_model.is_clock_network_pin(en_node) {
                        continue;
                    }
                    // Skip if all arcs from en_node in this instance are disabled.
                    if graph.outgoing(en_node).is_empty() && !graph.incoming(en_node).is_empty() {
                        // A combinational input pin whose outgoing cell arcs were all disabled by
                        // `set_disable_timing` or constant propagation is not gated.
                        let has_any_cell_arc = graph
                            .arcs()
                            .iter()
                            .any(|a| a.src == en_node && !a.kind.is_net());
                        if has_any_cell_arc {
                            continue;
                        }
                    }
                    let site = ClockGatingSite {
                        inst: inst as u32,
                        enable_pin: en_node,
                        clock_pin: ck_node,
                        lead_pin_tr,
                        trail_pin_tr,
                    };
                    if !sites.contains(&site) {
                        sites.push(site);
                    }
                }
            }
        }
    }
    sites
}

/// Evaluates setup and hold clock-gating checks across `sites` (`STA-REQ-0224`).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_gating_sites<C: Cppr>(
    sites: &[ClockGatingSite],
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    clock_model: &IdealClockModel,
    cppr_model: &C,
    tags: &TagTable,
    prop_res: &LoweredResult,
    mut on_slack: impl FnMut(SlackRecord),
) -> Result<(), Diag> {
    if tags.is_empty() {
        return Ok(());
    }
    for site in sites {
        for &(capture_clock, bits) in clock_model.pin_clocks(site.clock_pin) {
            let ck_c = &constraints.clocks[capture_clock.index()];
            for (sense_bit, neg_sense) in [(SENSE_POS, false), (SENSE_NEG, true)] {
                if bits & sense_bit == 0 {
                    continue;
                }
                let e_setup = if neg_sense {
                    site.lead_pin_tr.opposite()
                } else {
                    site.lead_pin_tr
                };
                let e_hold = if neg_sense {
                    site.trail_pin_tr.opposite()
                } else {
                    site.trail_pin_tr
                };
                let setup_ck_arr = clock_model.arrival(
                    capture_clock,
                    e_setup,
                    site.clock_pin,
                    site.lead_pin_tr,
                    An::Early,
                );
                let hold_ck_arr = clock_model.arrival(
                    capture_clock,
                    e_hold,
                    site.clock_pin,
                    site.trail_pin_tr,
                    An::Late,
                );

                for (tid, tag) in tags.iter() {
                    let ck_l = &constraints.clocks[tag.clock.index()];
                    let d_s = setup_relation(ck_l, tag.edge, ck_c, e_setup)?;
                    let d_h = non_negative_relation(ck_l, tag.edge, ck_c, e_hold)?;

                    for data_tr in Tr::ALL {
                        if let Some(ck_arr) = setup_ck_arr {
                            let arr_l = prop_res.arrival(
                                graph,
                                site.enable_pin,
                                tid.index(),
                                data_tr,
                                An::Late,
                            );
                            if An::Late.is_present(arr_l) {
                                let u_s = clock_model.uncertainty(
                                    constraints,
                                    tag.clock,
                                    tag.edge,
                                    capture_clock,
                                    e_setup,
                                    Some(site.enable_pin),
                                    true,
                                );
                                let cppr = if tag.clock == capture_clock {
                                    cppr_model.credit(capture_clock, site.clock_pin, site.clock_pin)
                                } else {
                                    0.0
                                };
                                let req_s = d_s + ck_arr.latency - u_s + cppr;
                                let slack_s = canon_zero(req_s - arr_l);
                                let group = resolve_path_group(
                                    constraints,
                                    EndpointCheckKind::ClockGatingSetup,
                                    tag.clock,
                                    capture_clock,
                                    site.enable_pin,
                                );
                                on_slack(SlackRecord {
                                    endpoint: site.enable_pin,
                                    clock_pin: Some(site.clock_pin),
                                    kind: EndpointCheckKind::ClockGatingSetup,
                                    slack: slack_s,
                                    arrival: canon_zero(arr_l),
                                    required: canon_zero(req_s),
                                    relation: d_s,
                                    borrowed: 0.0,
                                    tag: tid,
                                    launch_clock: tag.clock,
                                    launch_edge: tag.edge,
                                    data_tr,
                                    capture_clock,
                                    capture_edge: e_setup,
                                    path_group: group,
                                });
                            }
                        }

                        if let Some(ck_arr) = hold_ck_arr {
                            let arr_e = prop_res.arrival(
                                graph,
                                site.enable_pin,
                                tid.index(),
                                data_tr,
                                An::Early,
                            );
                            if An::Early.is_present(arr_e) {
                                let u_h = clock_model.uncertainty(
                                    constraints,
                                    tag.clock,
                                    tag.edge,
                                    capture_clock,
                                    e_hold,
                                    Some(site.enable_pin),
                                    false,
                                );
                                let cppr = if tag.clock == capture_clock {
                                    cppr_model.credit(capture_clock, site.clock_pin, site.clock_pin)
                                } else {
                                    0.0
                                };
                                let req_h = d_h + ck_arr.latency + u_h - cppr;
                                let slack_h = canon_zero(arr_e - req_h);
                                let group = resolve_path_group(
                                    constraints,
                                    EndpointCheckKind::ClockGatingHold,
                                    tag.clock,
                                    capture_clock,
                                    site.enable_pin,
                                );
                                on_slack(SlackRecord {
                                    endpoint: site.enable_pin,
                                    clock_pin: Some(site.clock_pin),
                                    kind: EndpointCheckKind::ClockGatingHold,
                                    slack: slack_h,
                                    arrival: canon_zero(arr_e),
                                    required: canon_zero(req_h),
                                    relation: d_h,
                                    borrowed: 0.0,
                                    tag: tid,
                                    launch_clock: tag.clock,
                                    launch_edge: tag.edge,
                                    data_tr,
                                    capture_clock,
                                    capture_edge: e_hold,
                                    path_group: group,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[inline]
fn canon_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}
