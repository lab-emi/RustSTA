// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Equivalence tests between the new [`TimingGraph`] (over both `legacy_import` and `RS-VLOG`
//! [`read_structural`]) and the legacy [`ruststa::graph::Graph`] across all 32 corpus netlists
//! and combinational loop fixtures (owner: E1).

mod support;

use std::collections::HashMap;

use rusteda_formats::liberty::{Library, read_text};
use ruststa::engine::graph::{ArcKind, DisableReason, TimingGraph};
use ruststa::engine::input::NetlistView;
use ruststa::engine::types::NodeId;
use ruststa::graph::{ArcRef, Graph as LegacyGraph};
use ruststa::model::{CheckKind as LegacyCheckKind, Model as LegacyModel};
use ruststa::netlist::{CONST0, CONST1};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use support::legacy_import::{import_frozen, read_flat, read_structural_frozen};

fn wt_full() -> Library {
    read_text(&library(Variant::Full), Variant::Full.name()).expect("synthetic library")
}

/// Compares a [`TimingGraph`] and its [`NetlistView`] against a [`LegacyGraph`] on the same
/// netlist, checking node names, constant connections, net and cell propagation arcs, launch arcs,
/// timing checks, cycle-broken arcs, and exact per-node levels.
fn assert_matches_legacy(
    design: &str,
    nv: &NetlistView<'_>,
    g: &TimingGraph,
    legacy: &LegacyGraph,
    legacy_model: &LegacyModel,
) {
    assert_eq!(
        g.num_nodes(),
        legacy.nodes.len(),
        "{design}: node count mismatch"
    );
    assert_eq!(
        g.num_port_bits(),
        legacy
            .nodes
            .iter()
            .filter(|n| !matches!(n.kind, ruststa::graph::NodeKind::Pin { .. }))
            .count(),
        "{design}: port bit count mismatch"
    );

    // Map node names to NodeId and legacy node index.
    // Note: the legacy flattener (`Flat`) collapses a 1-bit bus port `[0:0] p` to a scalar port
    // `"p"`, whereas `RS-VLOG` (`read_structural`) preserves its bus range `[0:0]` and formats
    // the port bit as `"p[0]"`.
    let norm_name = |name: String| -> String {
        if legacy.nodes.iter().all(|n| n.name != name)
            && let Some(base) = name.strip_suffix("[0]")
            && legacy.nodes.iter().any(|n| n.name == base)
        {
            return base.to_string();
        }
        name
    };

    let mut new_by_name: HashMap<String, NodeId> = HashMap::with_capacity(g.num_nodes());
    for i in 0..g.num_nodes() {
        let id = NodeId(i as u32);
        let name = norm_name(nv.node_name_str(id).into_owned());
        assert!(
            new_by_name.insert(name.clone(), id).is_none(),
            "{design}: duplicate node name {name}"
        );
    }

    let mut legacy_by_name: HashMap<&str, u32> = HashMap::with_capacity(legacy.nodes.len());
    for (li, ln) in legacy.nodes.iter().enumerate() {
        legacy_by_name.insert(ln.name.as_str(), li as u32);
        let Some(&nid) = new_by_name.get(ln.name.as_str()) else {
            panic!("{design}: legacy node '{}' missing in TimingGraph", ln.name);
        };
        // Check constant net status per node.
        let legacy_const = if ln.net == CONST0 {
            Some(false)
        } else if ln.net == CONST1 {
            Some(true)
        } else {
            None
        };
        assert_eq!(
            nv.is_const_node(nid),
            legacy_const,
            "{design}: constant status mismatch on node '{}'",
            ln.name
        );
    }

    // Compare propagation arcs (net and cell) as multisets of (src_name, dst_name, is_net, disabled).
    let mut new_arcs: Vec<(String, String, bool, bool)> = g
        .arcs()
        .iter()
        .enumerate()
        .map(|(ai, a)| {
            (
                norm_name(nv.node_name_str(a.src).into_owned()),
                norm_name(nv.node_name_str(a.dst).into_owned()),
                a.kind.is_net(),
                g.arc_disable_reason(ai) == Some(DisableReason::LoopBreak),
            )
        })
        .collect();
    new_arcs.sort();

    let mut leg_arcs: Vec<(String, String, bool, bool)> = legacy
        .arcs
        .iter()
        .enumerate()
        .map(|(ai, a)| {
            (
                legacy.nodes[a.from as usize].name.clone(),
                legacy.nodes[a.to as usize].name.clone(),
                matches!(a.kind, ArcRef::Net),
                legacy.disabled[ai],
            )
        })
        .collect();
    leg_arcs.sort();
    assert_eq!(new_arcs, leg_arcs, "{design}: propagation arcs mismatch");

    // Compare sequential launch arcs.
    let mut new_launches: Vec<(String, String)> = g
        .launches()
        .iter()
        .map(|l| {
            (
                norm_name(nv.node_name_str(l.clock_pin).into_owned()),
                norm_name(nv.node_name_str(l.out_pin).into_owned()),
            )
        })
        .collect();
    new_launches.sort();

    let mut leg_launches: Vec<(String, String)> = legacy
        .launches
        .iter()
        .map(|l| {
            (
                legacy.nodes[l.clock_pin as usize].name.clone(),
                legacy.nodes[l.out as usize].name.clone(),
            )
        })
        .collect();
    leg_launches.sort();
    assert_eq!(new_launches, leg_launches, "{design}: launch arcs mismatch");

    // Compare timing check arcs.
    let mut new_checks: Vec<(String, String, bool)> = g
        .checks()
        .iter()
        .map(|c| {
            (
                norm_name(nv.node_name_str(c.data_pin).into_owned()),
                norm_name(nv.node_name_str(c.clock_pin).into_owned()),
                c.kind.is_setup(),
            )
        })
        .collect();
    new_checks.sort();

    let mut leg_checks: Vec<(String, String, bool)> = legacy
        .checks
        .iter()
        .map(|c| {
            let cell = &legacy_model.cells[legacy.inst_cell[c.inst as usize] as usize];
            let is_setup = cell.checks[c.check as usize].kind == LegacyCheckKind::Setup;
            (
                legacy.nodes[c.data as usize].name.clone(),
                legacy.nodes[c.clock as usize].name.clone(),
                is_setup,
            )
        })
        .collect();
    leg_checks.sort();
    assert_eq!(new_checks, leg_checks, "{design}: timing checks mismatch");

    // Reconstruct legacy per-node levels from legacy.order and legacy.level_start.
    let mut legacy_level = vec![0u32; legacy.nodes.len()];
    for (lv, win) in legacy.level_start.windows(2).enumerate() {
        for &u in &legacy.order[win[0] as usize..win[1] as usize] {
            legacy_level[u as usize] = lv as u32;
        }
    }

    for (name, &nid) in &new_by_name {
        let li = legacy_by_name[name.as_str()];
        assert_eq!(
            g.level(nid),
            legacy_level[li as usize],
            "{design}: level mismatch on node '{name}'"
        );
    }

    // Verify canonical topological order and CSR invariants on TimingGraph.
    assert_eq!(g.order().len(), g.num_nodes());
    for (rank, &nid) in g.order().iter().enumerate() {
        assert_eq!(g.topo_rank(nid) as usize, rank);
        if rank > 0 {
            let prev = g.order()[rank - 1];
            assert!(
                g.topo_id(prev) < g.topo_id(nid),
                "{design}: order not strictly increasing in topo_id"
            );
        }
    }
    for (ai, arc) in g.arcs().iter().enumerate() {
        if !g.is_arc_disabled(ai) {
            assert!(
                g.level(arc.src) < g.level(arc.dst),
                "{design}: enabled arc {} -> {} violates level order",
                nv.node_name(arc.src),
                nv.node_name(arc.dst)
            );
            assert!(g.incoming(arc.dst).contains(&(ai as u32)));
            assert!(g.outgoing(arc.src).contains(&(ai as u32)));
        }
    }
    let _ = ArcKind::Net {
        net: ruststa::engine::types::NetRow(0),
    };
}

// STA-REQ-0001, STA-REQ-0100, STA-REQ-0102, STA-REQ-0401: TimingGraph built via legacy_import
// and via RS-VLOG (read_structural) both agree with legacy Graph on all 32 corpus netlists.
#[test]
fn corpus_32_netlists_match_legacy_and_rsvlog() {
    let libs = [wt_full()];
    let legacy_model = LegacyModel::build(&libs).expect("legacy model");
    let mut count = 0usize;

    for netlist in CORPUS.iter().chain([&LANES_K1]) {
        let flat = read_flat(netlist.name, netlist.text, Some(netlist.top), &libs)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        let legacy_graph = LegacyGraph::build(&flat, &legacy_model)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));

        // 1. Via legacy_import -> GateNetlist -> FrozenNetlist.
        let frozen_imp = import_frozen(netlist.name, netlist.text, Some(netlist.top), &libs)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        let nv_imp = NetlistView::new(&frozen_imp, &libs)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        let g_imp =
            TimingGraph::build(&nv_imp, &libs).unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        assert_matches_legacy(netlist.name, &nv_imp, &g_imp, &legacy_graph, &legacy_model);

        // 2. Via RS-VLOG (read_structural) -> GateNetlist -> FrozenNetlist.
        let frozen_vlog = read_structural_frozen(netlist.name, netlist.text, &libs)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        let nv_vlog = NetlistView::new(&frozen_vlog, &libs)
            .unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        let g_vlog =
            TimingGraph::build(&nv_vlog, &libs).unwrap_or_else(|e| panic!("{}: {e}", netlist.name));
        assert_matches_legacy(
            netlist.name,
            &nv_vlog,
            &g_vlog,
            &legacy_graph,
            &legacy_model,
        );

        count += 1;
    }
    assert_eq!(count, 32);
}

// STA-REQ-0102: combinational cycle breaking agrees between TimingGraph (both loaders) and
// legacy Graph.
#[test]
fn combinational_cycle_breaking_matches_legacy() {
    let libs = [wt_full()];
    let legacy_model = LegacyModel::build(&libs).expect("legacy model");
    let vlog = "\
module ring_loop(input z_in, output z_out);
  wire n1, n2, n3;
  wtf_nand2_1 u1 (.A(n3), .B(z_in), .Y(n1));
  wtf_inv_1 u2 (.A(n1), .Y(n2));
  wtf_inv_1 u3 (.A(n2), .Y(n3));
  wtf_buf_1 u4 (.A(n3), .X(z_out));
endmodule
";
    let flat = read_flat("ring_loop.v", vlog, None, &libs).unwrap();
    let legacy_graph = LegacyGraph::build(&flat, &legacy_model).unwrap();
    assert_eq!(
        legacy_graph.disabled.iter().filter(|&&d| d).count(),
        1,
        "expected 1 broken cycle arc in legacy graph"
    );

    let frozen_imp = import_frozen("ring_loop.v", vlog, None, &libs).unwrap();
    let nv_imp = NetlistView::new(&frozen_imp, &libs).unwrap();
    let g_imp = TimingGraph::build(&nv_imp, &libs).unwrap();
    assert_matches_legacy(
        "ring_loop_imp",
        &nv_imp,
        &g_imp,
        &legacy_graph,
        &legacy_model,
    );

    let frozen_vlog = read_structural_frozen("ring_loop.v", vlog, &libs).unwrap();
    let nv_vlog = NetlistView::new(&frozen_vlog, &libs).unwrap();
    let g_vlog = TimingGraph::build(&nv_vlog, &libs).unwrap();
    assert_matches_legacy(
        "ring_loop_vlog",
        &nv_vlog,
        &g_vlog,
        &legacy_graph,
        &legacy_model,
    );
    assert!(
        g_vlog.diagnostics().iter().any(|d| d.code == "T0012"),
        "expected T0012 cycle-break diagnostic"
    );
}
