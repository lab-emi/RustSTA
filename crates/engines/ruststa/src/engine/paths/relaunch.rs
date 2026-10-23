// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Schedule extraction and latch `D -> Q` re-launch lowering (`STA-REQ-0221`, `STA-REQ-0222`).

use std::collections::HashMap;

use rusteda_compute::timing::{LutDesc, NONE, TagMapRec};
use rusteda_core::job::Diag;

use super::search::{
    canonical_net_totals, map_cell_arcs, propagate_clock_sense, pulse_width_ns, setup_relation_ns,
    source_edges,
};
use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::DriverLoad;
use crate::engine::graph::{ArcKind, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, MaxTimeBorrowTarget};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView};
use crate::engine::prop::backward::{OutputDelaySpec, canon_zero};
use crate::engine::prop::lower::{
    ArcTagMaps, GraphStartBuilder, IdealClockSpec, IdealSchedule, InputDelaySpec, LoweredCorner,
};
use crate::engine::types::{An, NodeId, Tr};

/// Converts [`CompiledConstraints`] into an [`IdealSchedule`] and [`OutputDelaySpec`] list for a
/// corner (`STA-REQ-0212`, `STA-REQ-0217` to `STA-REQ-0221`).
#[must_use]
pub fn schedule_and_outputs_from_compiled(
    compiled: &CompiledConstraints,
    graph: &TimingGraph,
) -> (IdealSchedule, Vec<OutputDelaySpec>) {
    let mut clocks = Vec::with_capacity(compiled.clocks.len());
    for ck in &compiled.clocks {
        let lat_early = ck.source_latency_ns[An::Early.index()][0][Tr::Rise.index()]
            + ck.network_latency_ns[An::Early.index()][Tr::Rise.index()];
        let lat_late = ck.source_latency_ns[An::Late.index()][1][Tr::Rise.index()]
            + ck.network_latency_ns[An::Late.index()][Tr::Rise.index()];
        clocks.push(IdealClockSpec {
            name: ck.name.clone(),
            period: ck.period_ns,
            waveform: ck.waveform_ns,
            sources: ck.sources.clone(),
            latency: [lat_early, lat_late],
            transition: [
                ck.transition_ns[An::Early.index()][Tr::Rise.index()],
                ck.transition_ns[An::Late.index()][Tr::Rise.index()],
            ],
            uncertainty: [
                ck.uncertainty_setup_ns[Tr::Rise.index()],
                ck.uncertainty_hold_ns[Tr::Rise.index()],
            ],
        });
    }

    let mut input_delays = Vec::with_capacity(compiled.input_delays.len());
    for d in &compiled.input_delays {
        let Some(clk_id) = d.clock else {
            continue;
        };
        let ck = &compiled.clocks[clk_id.index()];
        let e_idx = d.clock_edge.index();
        let src_lat_e = if d.source_latency_included {
            0.0
        } else {
            ck.source_latency_ns[An::Early.index()][0][e_idx]
        };
        let src_lat_l = if d.source_latency_included {
            0.0
        } else {
            ck.source_latency_ns[An::Late.index()][1][e_idx]
        };
        let net_lat_e = if d.network_latency_included {
            0.0
        } else {
            ck.network_latency_ns[An::Early.index()][e_idx]
        };
        let net_lat_l = if d.network_latency_included {
            0.0
        } else {
            ck.network_latency_ns[An::Late.index()][e_idx]
        };
        let pair = d.min_max_pair();
        let tr_pair = compiled
            .input_transitions
            .get(&d.node)
            .map_or([0.0, 0.0], |it| it.min_max_pair());
        input_delays.push(InputDelaySpec {
            node: d.node,
            clock: clk_id.0,
            edge: d.clock_edge,
            delay: [
                pair[0] + src_lat_e + net_lat_e,
                pair[1] + src_lat_l + net_lat_l,
            ],
            transition: tr_pair,
        });
    }

    let mut untagged_transitions = Vec::new();
    for (&port, it) in &compiled.input_transitions {
        if !compiled.input_delays.iter().any(|d| d.node == port) {
            untagged_transitions.push((port, it.min_max_pair()));
        }
    }

    let mut max_time_borrow = HashMap::new();
    for (ai, arc) in graph.arcs().iter().enumerate() {
        if let ArcKind::LatchData { inst, .. } = arc.kind {
            if let Some(&b) = compiled.latch_max_borrow_ns.get(&inst) {
                max_time_borrow.insert(ai as u32, b);
            } else {
                for mtb in &compiled.max_time_borrow {
                    match mtb.target {
                        MaxTimeBorrowTarget::Cell(i) if i == inst => {
                            max_time_borrow.insert(ai as u32, mtb.max_borrow_ns);
                        }
                        MaxTimeBorrowTarget::Pin(p) if p == arc.src => {
                            max_time_borrow.insert(ai as u32, mtb.max_borrow_ns);
                        }
                        MaxTimeBorrowTarget::Clock(_) => {
                            max_time_borrow.insert(ai as u32, mtb.max_borrow_ns);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut outputs = Vec::with_capacity(compiled.output_delays.len());
    for d in &compiled.output_delays {
        let Some(clk_id) = d.clock else {
            continue;
        };
        outputs.push(OutputDelaySpec {
            node: d.node,
            clock: clk_id.0,
            edge: d.clock_edge,
            delay_ns: d.delay_ns,
            latency_included: d.network_latency_included || d.source_latency_included,
        });
    }

    (
        IdealSchedule {
            clocks,
            input_delays,
            untagged_transitions,
            max_time_borrow,
        },
        outputs,
    )
}

/// Lowers `schedule` into a [`LoweredCorner`] with full `STA-REQ-0221` / `STA-REQ-0222` latch
/// `D -> Q` re-launch into the opening-edge tag `tag_open` and `G -> Q` slew preservation on `Q`.
pub fn lower_schedule_with_relaunch<'a>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &'a TimingLib,
    net_loads: &NetLoads,
    schedule: &IdealSchedule,
) -> Result<LoweredCorner<'a>, Diag> {
    let n_nodes = graph.num_nodes();
    let ntags = (2 * schedule.clocks.len().max(1)) as u32;
    let cell_arcs = map_cell_arcs(netview, lib)?;
    let canon_totals = canonical_net_totals(netview, net_loads);
    let clock_sense = propagate_clock_sense(graph, &schedule.clocks);

    let mut starts = GraphStartBuilder::new(ntags);
    for (ci, ck) in schedule.clocks.iter().enumerate() {
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

    for d in &schedule.input_delays {
        if d.node.index() >= n_nodes {
            continue;
        }
        let t = d.clock * 2 + d.edge.index() as u32;
        if t < ntags {
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
    }

    for &(node, s) in &schedule.untagged_transitions {
        if node.index() < n_nodes {
            for tr in Tr::ALL {
                for an in An::ALL {
                    starts.merge_untagged_slew(graph, node, tr, an, s[an.index()]);
                }
            }
        }
    }

    // Track `G -> Q` early slews per latch output node `Q` so `LatchData` arcs preserve `Q`'s
    // `G -> Q` transition (`STA-REQ-0222`).
    let mut latch_q_slews: HashMap<NodeId, [f64; 2]> = HashMap::new();

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
            let ck = &schedule.clocks[c as usize];
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
                        if an == An::Early {
                            let entry =
                                latch_q_slews.entry(l.out_pin).or_insert([f64::INFINITY; 2]);
                            entry[to.index()] = entry[to.index()].min(canon_zero(ds.slew.max(0.0)));
                        }
                    }
                }
            }
        }
    }

    // Build `TagMapRec`s for transparent latch `D -> Q` arcs (`STA-REQ-0221`, `STA-REQ-0222`).
    let mut tag_maps = ArcTagMaps::new();
    let g_arcs = graph.arcs();
    let g_checks = graph.checks();

    for (ai, arc) in g_arcs.iter().enumerate() {
        if graph.is_arc_disabled(ai) {
            continue;
        }
        let ArcKind::LatchData { setup_check, .. } = arc.kind else {
            continue;
        };
        let Some(ch) = g_checks.get(setup_check as usize) else {
            continue;
        };
        let max_borrow = schedule.max_time_borrow.get(&(ai as u32)).copied();
        let mut recs = Vec::new();

        for &(c, bits) in &clock_sense[ch.clock_pin.index()] {
            let ck_c = &schedule.clocks[c as usize];
            for e_close in source_edges(ch.edge, bits) {
                let e_open = e_close.opposite();
                let dst_tag = c * 2 + e_open.index() as u32;
                let w_ns = pulse_width_ns(ck_c, e_open, e_close);

                for src_tag in 0..ntags {
                    let (l_idx, el_idx) = ((src_tag / 2) as usize, (src_tag % 2) as usize);
                    if l_idx >= schedule.clocks.len() {
                        continue;
                    }
                    let ck_l = &schedule.clocks[l_idx];
                    let e_l = if el_idx == 0 { Tr::Rise } else { Tr::Fall };
                    let Some(d_close) = setup_relation_ns(ck_l, e_l, ck_c, e_close) else {
                        continue;
                    };
                    let d_open = d_close - w_ns;
                    let shift = -d_open;
                    let r_open_shifted = ck_c.latency[An::Early.index()] - ck_c.uncertainty[0];
                    let close_shifted =
                        w_ns + ck_c.latency[An::Early.index()] - ck_c.uncertainty[0];
                    let limit_shifted = max_borrow.map_or(f64::INFINITY, |b| r_open_shifted + b);

                    recs.push(TagMapRec {
                        src_tag,
                        dst_tag,
                        shift: canon_zero(shift),
                        open: [f64::MAX, r_open_shifted.next_up()],
                        close: [f64::INFINITY, canon_zero(close_shifted)],
                        close_slew: ck_c.transition,
                        limit: [f64::INFINITY, canon_zero(limit_shifted)],
                    });
                }
            }
        }
        if !recs.is_empty() {
            tag_maps.insert(ai as u32, recs);
        }
    }

    let mut lowered = LoweredCorner::lower(
        netview,
        graph,
        lib,
        net_loads,
        ntags,
        starts.build(),
        &tag_maps,
    )?;

    // Override `slew` tables on `LatchData` CSR incoming arcs with scalar LUTs equal to `G -> Q`'s
    // early transition at `Q`, so `slew[Q]` is always the `G -> Q` transition (`STA-REQ-0222`).
    for (ai, arc) in g_arcs.iter().enumerate() {
        if !arc.kind.is_latch_data() || graph.is_arc_disabled(ai) {
            continue;
        }
        let csr_idx = lowered.graph_arc_to_csr_arc[ai];
        if csr_idx == NONE {
            continue;
        }
        let Some(&gq_slew) = latch_q_slews.get(&arc.dst) else {
            continue;
        };
        if !gq_slew[0].is_finite() || !gq_slew[1].is_finite() {
            continue;
        }
        let base_cls = lowered.luts.classes[lowered.csr.in_arcs[csr_idx as usize].class as usize];
        let voff_r = lowered.luts.data.len() as u32;
        lowered.luts.data.to_mut().push(gq_slew[0]);
        lowered.luts.data.to_mut().push(gq_slew[1]);
        let lut_r = lowered.luts.luts.len() as u32;
        lowered.luts.luts.to_mut().push(LutDesc::scalar(voff_r));
        let lut_f = lowered.luts.luts.len() as u32;
        lowered.luts.luts.to_mut().push(LutDesc::scalar(voff_r + 1));

        let mut cls = base_cls;
        cls.slew = [lut_r, lut_f];
        let cls_idx = lowered.luts.classes.len() as u32;
        lowered.luts.classes.to_mut().push(cls);
        lowered.csr.in_arcs.to_mut()[csr_idx as usize].class = cls_idx;
    }

    Ok(lowered)
}
