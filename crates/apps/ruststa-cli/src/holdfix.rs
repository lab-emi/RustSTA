// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `hold_fix` section of `ruststa report` (`STA-REQ-0830` to `STA-REQ-0832`).
//!
//! For each corner, every endpoint whose hold slack is below the target margin is listed, worst
//! first, with the delay it needs (`margin - slack`), its setup slack, the net that drives it and
//! its driver, and the stages of its worst hold path in that corner, each with its net and the
//! setup and hold slacks of the pin (the window in which a delay can be added). A merged list
//! gives, per endpoint, the worst corner, the largest needed delay and the smallest setup slack
//! over all corners. The paths come from the ranked paths of the timing table: every corner
//! ranks its own worst hold path of each endpoint. The nets come from the revision's netlist.
//! Rows are bounded by `--limit` per corner (AGENTS rule 3.4).

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use rusteda_workspace::{Error, JsonObject, Revision};
use ruststa::engine::input::{NetTarget, NetlistView, NodeKind};
use ruststa::engine::store::{
    PathPointKind, QueriedEndpoint, QueriedPath, StaStore, StaStoreView, canon_f64,
};
use ruststa::engine::types::{NodeId, Tr};

use crate::ws_io::{map_store_error, node_handle_string};

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then(|| canon_f64(x))
}

/// The setup and hold slack of `node` in corner `corner`, the worst over transitions, from
/// `NODE_REC` (`None` in a summary table).
fn pin_slacks(v: &StaStoreView<'_>, corner: usize, node: NodeId) -> (Option<f64>, Option<f64>) {
    let n = v.meta.num_nodes as usize;
    let Some(rec) = v.node_rec.get(corner * n + node.index()) else {
        return (None, None);
    };
    let worst = |an: usize| {
        Tr::ALL
            .iter()
            .filter_map(|tr| finite(rec.slack[tr.index()][an]))
            .reduce(f64::min)
    };
    (worst(1), worst(0))
}

/// The net of `node`, its driver and its fanout (load pins and ports).
fn net_of(nv: &NetlistView<'_>, node: NodeId) -> (Option<String>, Option<String>, usize) {
    match nv.node_net(node) {
        NetTarget::Net(net) => (
            Some(nv.net_name_str(net).into_owned()),
            nv.driver(net).map(|d| nv.node_name_str(d).into_owned()),
            nv.loads(net).len(),
        ),
        NetTarget::Const(_) => (None, None, 0),
    }
}

fn stage_json(
    nv: &NetlistView<'_>,
    v: &StaStoreView<'_>,
    corner: usize,
    path: &QueriedPath,
) -> Result<Vec<JsonObject>, Error> {
    let num_port_bits = v.meta.num_port_bits;
    let mut out = Vec::with_capacity(path.points.len());
    for (i, pt) in path.points.iter().enumerate() {
        let (net, driver, fanout) = net_of(nv, pt.node);
        let (setup, hold) = pin_slacks(v, corner, pt.node);
        let (inst, cell) = match nv.node_kind(pt.node) {
            NodeKind::InstancePin { inst, cell, .. } => (
                Some(nv.instance_name(inst).to_owned()),
                Some(nv.cell_name(cell).to_owned()),
            ),
            NodeKind::PortBit { .. } => (None, None),
        };
        let arc = match pt.kind {
            PathPointKind::ClockLaunch => "launch",
            PathPointKind::Port if i == 0 => "input",
            PathPointKind::Port | PathPointKind::Net => "net",
            PathPointKind::Cell => "cell",
        };
        let mut o = JsonObject::new();
        o.insert("pin", &pt.name)?;
        o.insert("handle", &node_handle_string(pt.node, num_port_bits))?;
        o.insert("inst", &inst)?;
        o.insert("cell", &cell)?;
        o.insert("arc", &arc)?;
        o.insert(
            "transition",
            &match pt.tr {
                Tr::Rise => "rise",
                Tr::Fall => "fall",
            },
        )?;
        o.insert("net", &net)?;
        o.insert("net_driver", &driver)?;
        o.insert("net_fanout", &fanout)?;
        o.insert("incr_ns", &canon_f64(pt.incr_delay))?;
        o.insert("arrival_ns", &canon_f64(pt.arrival))?;
        o.insert("slew_ns", &canon_f64(pt.slew))?;
        o.insert("pin_setup_slack_ns", &setup)?;
        o.insert("pin_hold_slack_ns", &hold)?;
        out.push(o);
    }
    Ok(out)
}

/// The `hold_fix` section of `store` on `rev` for the corners `corners` (all when `None`).
pub(crate) fn hold_fix_section(
    store: &StaStore,
    rev: &Revision,
    corner: Option<usize>,
    target_margin: f64,
    limit: usize,
) -> Result<JsonObject, Error> {
    if !target_margin.is_finite() {
        return Err(Error::usage("--target-margin must be a finite number"));
    }
    let v = store.view().map_err(map_store_error)?;
    let netlist_rbin = rusteda_workspace::netlist::open_rev(rev)?;
    let frozen = rusteda_workspace::netlist::decode(&netlist_rbin)?;
    let nv = NetlistView::without_libraries(&frozen)
        .map_err(|e| Error::input_rejected(format!("netlist view: {e}")))?;
    if nv.num_nodes() != v.meta.num_nodes as usize {
        return Err(Error::input_rejected(
            "the timing table and the netlist of the revision have different pins",
        ));
    }
    let n_corners = v.meta.num_corners as usize;
    let corner_names: Vec<String> = v
        .corners
        .iter()
        .map(|c| v.strings.get(c.name_str).unwrap_or("").to_owned())
        .collect();
    let window = if limit > 0 { limit } else { usize::MAX };

    // Every endpoint of every corner (the merged list needs the slacks of all corners).
    let mut per_corner: Vec<Vec<QueriedEndpoint>> = Vec::with_capacity(n_corners);
    for c in 0..n_corners {
        per_corner.push(
            store
                .query_endpoints(c, None, usize::MAX)
                .map_err(map_store_error)?,
        );
    }
    // The listed endpoints of each corner: hold slack below the margin, worst first.
    let mut listed: Vec<Vec<&QueriedEndpoint>> = Vec::with_capacity(n_corners);
    for (c, eps) in per_corner.iter().enumerate() {
        let mut sel: Vec<&QueriedEndpoint> = if corner.is_none_or(|w| w == c) {
            eps.iter()
                .filter(|e| e.hold_slack.is_finite() && e.hold_slack < target_margin)
                .collect()
        } else {
            Vec::new()
        };
        sel.sort_by(|a, b| {
            a.hold_slack
                .total_cmp(&b.hold_slack)
                .then_with(|| a.name.cmp(&b.name))
        });
        listed.push(sel);
    }
    // The rank of the worst hold path of each shown endpoint in its corner: the first hold
    // record of the pair in rank order.
    let mut wanted: FxHashMap<(usize, usize), Option<usize>> = FxHashMap::default();
    for (c, sel) in listed.iter().enumerate() {
        for e in sel.iter().take(window) {
            wanted.insert((c, e.endpoint_idx), None);
        }
    }
    if !wanted.is_empty() {
        let mut open = wanted.len();
        for (r, pr) in v.path_rank.iter().enumerate() {
            if !pr.is_hold() {
                continue;
            }
            if let Some(slot) = wanted.get_mut(&(pr.corner_index(), pr.endpoint_index()))
                && slot.is_none()
            {
                *slot = Some(r);
                open -= 1;
                if open == 0 {
                    break;
                }
            }
        }
    }

    let num_port_bits = v.meta.num_port_bits;
    let mut corners_json = Vec::new();
    for (c, sel) in listed.iter().enumerate() {
        if corner.is_some_and(|w| w != c) {
            continue;
        }
        let eps = &per_corner[c];
        let checked = eps.iter().filter(|e| e.hold_slack.is_finite()).count();
        let violating = eps
            .iter()
            .filter(|e| e.hold_slack.is_finite() && e.hold_slack < 0.0)
            .count();
        let mut rows = Vec::new();
        for e in sel.iter().take(window) {
            let (net, driver, fanout) = net_of(&nv, e.data_node);
            let (inst, cell) = match nv.node_kind(e.data_node) {
                NodeKind::InstancePin { inst, cell, .. } => (
                    Some(nv.instance_name(inst).to_owned()),
                    Some(nv.cell_name(cell).to_owned()),
                ),
                NodeKind::PortBit { .. } => (None, None),
            };
            let mut o = JsonObject::new();
            o.insert("endpoint", &e.name)?;
            o.insert("handle", &node_handle_string(e.data_node, num_port_bits))?;
            o.insert("inst", &inst)?;
            o.insert("cell", &cell)?;
            o.insert("capture_clock", &e.capture_clock)?;
            o.insert("path_group", &e.path_group)?;
            o.insert("hold_slack_ns", &canon_f64(e.hold_slack))?;
            o.insert(
                "required_delay_ns",
                &canon_f64((target_margin - e.hold_slack).max(0.0)),
            )?;
            o.insert("setup_slack_ns", &finite(e.setup_slack))?;
            o.insert("net", &net)?;
            o.insert("net_driver", &driver)?;
            o.insert("net_fanout", &fanout)?;
            match wanted.get(&(c, e.endpoint_idx)).copied().flatten() {
                Some(rank) => {
                    let p = store.query_path_handle(rank).map_err(map_store_error)?;
                    let start = p.points.first().map(|pt| pt.name.clone());
                    let mut po = JsonObject::new();
                    po.insert("handle", &format!("path{}", rank + 1))?;
                    po.insert("startpoint", &start)?;
                    po.insert("launch_clock", &p.launch_clock)?;
                    po.insert("arrival_ns", &canon_f64(p.arrival))?;
                    po.insert("required_ns", &canon_f64(p.required))?;
                    po.insert("check_value_ns", &canon_f64(p.check_value))?;
                    po.insert("slack_ns", &canon_f64(p.slack))?;
                    po.insert("stages", &stage_json(&nv, &v, c, &p)?)?;
                    o.insert("startpoint", &start)?;
                    o.insert("path", &po)?;
                }
                None => {
                    o.insert("startpoint", &Option::<String>::None)?;
                    o.insert("path", &Option::<String>::None)?;
                }
            }
            rows.push(o);
        }
        let mut co = JsonObject::new();
        co.insert("corner", &corner_names[c])?;
        co.insert("hold_endpoints", &checked)?;
        co.insert("hold_violating", &violating)?;
        co.insert("listed", &sel.len())?;
        co.insert("shown", &rows.len())?;
        co.insert(
            "worst_hold_slack_ns",
            &eps.iter()
                .filter_map(|e| finite(e.hold_slack))
                .reduce(f64::min),
        )?;
        co.insert("endpoints", &rows)?;
        corners_json.push(co);
    }

    // The merged list over the corners (`STA-REQ-0831`).
    struct Merged {
        name: String,
        node: NodeId,
        worst: f64,
        worst_corner: usize,
        min_setup: Option<f64>,
        holds: BTreeMap<String, Option<f64>>,
    }
    let mut merged: BTreeMap<usize, Merged> = BTreeMap::new();
    for (c, eps) in per_corner.iter().enumerate() {
        if corner.is_some_and(|w| w != c) {
            continue;
        }
        for e in eps {
            let m = merged.entry(e.endpoint_idx).or_insert_with(|| Merged {
                name: e.name.clone(),
                node: e.data_node,
                worst: f64::INFINITY,
                worst_corner: c,
                min_setup: None,
                holds: BTreeMap::new(),
            });
            let h = finite(e.hold_slack);
            m.holds.insert(corner_names[c].clone(), h);
            if let Some(h) = h
                && h < m.worst
            {
                m.worst = h;
                m.worst_corner = c;
            }
            if let Some(s) = finite(e.setup_slack) {
                m.min_setup = Some(m.min_setup.map_or(s, |p| p.min(s)));
            }
        }
    }
    let mut merged: Vec<Merged> = merged
        .into_values()
        .filter(|m| m.worst < target_margin)
        .collect();
    merged.sort_by(|a, b| {
        a.worst
            .total_cmp(&b.worst)
            .then_with(|| a.name.cmp(&b.name))
    });
    let merged_total = merged.len();
    let mut merged_rows = Vec::new();
    for m in merged.iter().take(window) {
        let mut o = JsonObject::new();
        o.insert("endpoint", &m.name)?;
        o.insert("handle", &node_handle_string(m.node, num_port_bits))?;
        o.insert("worst_hold_slack_ns", &canon_f64(m.worst))?;
        o.insert("worst_corner", &corner_names[m.worst_corner])?;
        o.insert(
            "required_delay_ns",
            &canon_f64((target_margin - m.worst).max(0.0)),
        )?;
        o.insert("min_setup_slack_ns", &m.min_setup)?;
        o.insert("hold_slack_by_corner_ns", &m.holds)?;
        merged_rows.push(o);
    }

    let mut out = JsonObject::new();
    out.insert("target_margin_ns", &canon_f64(target_margin))?;
    out.insert("corners", &corners_json)?;
    out.insert("merged_total", &merged_total)?;
    out.insert("merged", &merged_rows)?;
    Ok(out)
}
