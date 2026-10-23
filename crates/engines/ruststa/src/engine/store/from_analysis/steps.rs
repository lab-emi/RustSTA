// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The steps of a traced path and the check value of its endpoint, for `PATH_CHAIN` and the
//! materialized paths (owner: E10; `STA-REQ-0228`).

use super::{PinIndex, StoreInput};
use crate::engine::analysis::CornerAnalysis;
use crate::engine::input::netview::NodeKind;
use crate::engine::prop::checks::SlackRecord;
use crate::engine::store::{NO_PRED, PathPointKind, canon_f64};
use crate::engine::types::{NodeId, Tr};

pub(super) fn compute_check_intrinsic(
    analysis: &StoreInput<'_>,
    ca: &CornerAnalysis,
    pin_index: &PinIndex,
    rec: &SlackRecord,
) -> f64 {
    let Some(clk_pin) = rec.clock_pin else {
        let ck = &analysis.constraints.clocks[rec.capture_clock.index()];
        return if rec.kind.is_setup_family() {
            canon_f64(
                ck.period_ns - ck.uncertainty_setup_ns[rec.capture_edge.index()] - rec.required,
            )
        } else {
            canon_f64(rec.required - ck.uncertainty_hold_ns[rec.capture_edge.index()])
        };
    };
    let frozen = analysis.netview.frozen();
    let want_setup = rec.kind.is_setup_family();
    for &ci in pin_index.checks_of(rec.endpoint) {
        let ch = &analysis.graph.checks()[ci as usize];
        if ch.data_pin == rec.endpoint
            && ch.clock_pin == clk_pin
            && ch.kind.is_setup() == want_setup
        {
            let cell_row = frozen.instances[ch.inst as usize].cell;
            let cell_name = analysis.netview.cell_name(cell_row);
            if let Some(tc) = ca.lib.cell(cell_name)
                && let Some(cc) = tc.checks.get(ch.lib_arc.index())
            {
                let an = rec.kind.data_bound();
                let clk_slew = ca.slew(clk_pin, ch.edge, an);
                let data_slew = ca.slew(rec.endpoint, rec.data_tr, an);
                return ca.lib.eval_check(
                    cc,
                    rec.data_tr,
                    if clk_slew.is_finite() { clk_slew } else { 0.0 },
                    if data_slew.is_finite() {
                        data_slew
                    } else {
                        0.0
                    },
                    0.0,
                );
            }
        }
    }
    0.0
}

/// Calls `step(node, tr, kind, tag_slot, name_bucket, arrival, slew)` for every step of the path
/// of `rec`, from its launch to its endpoint (`STA-REQ-0228`).
pub(super) fn for_each_path_step(
    analysis: &StoreInput<'_>,
    ca: &CornerAnalysis,
    pin_index: &PinIndex,
    node_bucket: &[u32],
    rec: &SlackRecord,
    mut step: impl FnMut(NodeId, Tr, PathPointKind, usize, u32, f64, f64),
) {
    let nv = &analysis.netview;
    let tv = &ca.node_values;
    let ntags = tv.ntags as usize;
    let tag = rec.tag.index();
    let an = rec.kind.data_bound();
    let an_i = an.index();

    let mut rev_nodes: Vec<(NodeId, Tr)> = Vec::new();
    let (mut curr_node, mut curr_tr) = (rec.endpoint, rec.data_tr);
    // A predecessor lies on a strictly lower level of the levelized graph, so a path has at most
    // one node per level: the bound cuts only a corrupt (cyclic) predecessor chain.
    let max_steps = nv.num_nodes().min(analysis.graph.levels().num_levels() + 1);
    for _ in 0..max_steps {
        rev_nodes.push((curr_node, curr_tr));
        let raw_pred = tv.predecessor(curr_node.index(), tag, curr_tr.index(), an_i);
        if raw_pred == NO_PRED {
            break;
        }
        let src_node = NodeId(raw_pred & 0x7FFF_FFFF);
        let src_tr = if (raw_pred >> 31) == 0 {
            Tr::Rise
        } else {
            Tr::Fall
        };
        if src_node.index() >= nv.num_nodes() || src_node == curr_node {
            break;
        }
        let src_arr = tv.arrival(src_node.index(), tag, src_tr.index(), an_i);
        if !an.is_present(src_arr) {
            break;
        }
        (curr_node, curr_tr) = (src_node, src_tr);
    }
    rev_nodes.reverse();

    // Prepend sequential launch clock pin (`r/CK` or `l/G`) if the first data node is a sequential
    // output launched by a clock arc (`STA-REQ-0228`).
    if let Some(&(first_node, _)) = rev_nodes.first()
        && first_node.index() >= nv.num_port_bits()
        && let Some(&li) = pin_index.first_launch.get(&first_node)
    {
        let launch = &analysis.graph.launches()[li as usize];
        let ck_pin = launch.clock_pin;
        let ck_tr = launch.edge;
        let ck_v = ck_pin.index();
        let ck_slew = canon_f64(tv.transition(ck_v, ck_tr.index(), an_i));
        step(
            ck_pin,
            ck_tr,
            PathPointKind::ClockLaunch,
            0,
            node_bucket[ck_v],
            0.0,
            if ck_slew.is_finite() { ck_slew } else { 0.0 },
        );
    }

    for (k, &(node, tr)) in rev_nodes.iter().enumerate() {
        let v = node.index();
        let kind = if k == 0 {
            if node.0 < nv.num_port_bits() as u32 {
                PathPointKind::Port
            } else {
                PathPointKind::Cell
            }
        } else {
            match (nv.node_kind(rev_nodes[k - 1].0), nv.node_kind(node)) {
                (
                    NodeKind::InstancePin { inst: i1, .. },
                    NodeKind::InstancePin { inst: i2, .. },
                ) if i1 == i2 => PathPointKind::Cell,
                (_, NodeKind::PortBit { .. }) => PathPointKind::Port,
                _ => PathPointKind::Net,
            }
        };
        let active_rank = (0..tag.min(ntags))
            .filter(|&t| {
                (0..2).any(|tr_i| (0..2).any(|a_i| tv.arrival(v, t, tr_i, a_i).is_finite()))
            })
            .count();
        let arr = canon_f64(tv.arrival(v, tag, tr.index(), an_i));
        let slew = canon_f64(tv.transition(v, tr.index(), an_i));
        step(node, tr, kind, active_rank, node_bucket[v], arr, slew);
    }
}
