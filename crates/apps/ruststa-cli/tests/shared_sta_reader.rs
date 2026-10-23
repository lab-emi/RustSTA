// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The shared read-only view of the timing table (`rusteda_workspace::sta`, WS-REQ-0126 to
//! WS-REQ-0134) and the engine's reader (`StaStore`, STA-REQ-0312 to STA-REQ-0315) were written
//! separately from the format specification, by different tools of the suite. On tables that
//! `ruststa run` writes (one and two corners, both store modes, ranked paths beyond the
//! materialized ones) they must read the same counts, corners, summaries, endpoints, endpoint
//! orders, ranked paths with their points, slacks and required times, and names.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_workspace::Workspace;
use rusteda_workspace::sta::StaTable;
use ruststa::engine::graph::CheckKind;
use ruststa::engine::store::StaStore;
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::corpus;
use serde_json::Value;

mod common;

const SDC: &str = "create_clock -name clk -period 0.8 [get_ports clk_i]\n\
                   set_input_delay -clock clk 0.1 [get_ports rst_ni]\n\
                   set_input_delay -clock clk 0.1 [get_ports en_i]\n\
                   set_input_delay -clock clk 0.1 [get_ports clr_i]\n\
                   set_output_delay -clock clk 0.1 [all_outputs]\n";

fn temp_dir(tag: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "ruststa-shared-reader-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("create temp dir");
    base
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruststa"))
        .arg("--format")
        .arg("json")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn ruststa");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let env: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "no JSON envelope ({e}): {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), env)
}

/// `None` for an untimed check, as the shared reader reports it.
fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

fn bits(v: Option<f64>) -> Option<u64> {
    v.map(f64::to_bits)
}

/// Compares the two readers on the timing table of `rev`; returns the ranked paths compared.
fn compare_readers(label: &str, ws: &Workspace, rev: &str) -> usize {
    let rev = ws
        .revision(ws.resolve_query(rev).expect("revision"))
        .expect("open revision");
    let shared = StaTable::open_rev(&rev).expect("shared reader");
    shared.verify_hash().expect("shared hash");
    let engine = StaStore::open_rev(&rev).expect("engine reader");
    let summary = engine.query_summary().expect("engine summary");

    let counts = shared.counts();
    assert_eq!(counts.nodes, summary.num_nodes, "{label}: nodes");
    assert_eq!(counts.cells, summary.num_cells, "{label}: cells");
    assert_eq!(
        counts.endpoints, summary.num_endpoints,
        "{label}: endpoints"
    );
    assert_eq!(
        format!("{:?}", shared.store_mode()).to_lowercase(),
        format!("{:?}", summary.store_mode).to_lowercase(),
        "{label}: store mode"
    );

    // Summaries, row by row (corner, then path group).
    let rows = shared.summary().expect("shared summary");
    assert_eq!(rows.len(), summary.rows.len(), "{label}: summary rows");
    for (s, e) in rows.iter().zip(&summary.rows) {
        let ctx = format!("{label}: summary {}/{}", e.corner_name, e.path_group);
        assert_eq!(
            (s.corner, &s.corner_name, &s.group),
            (e.corner_idx, &e.corner_name, &e.path_group),
            "{ctx}"
        );
        let setup_wns = (e.setup_endpoints > 0).then_some(e.setup_wns);
        let hold_wns = (e.hold_endpoints > 0).then_some(e.hold_wns);
        assert_eq!(bits(s.setup_wns_ns), bits(setup_wns), "{ctx}: setup wns");
        assert_eq!(bits(s.hold_wns_ns), bits(hold_wns), "{ctx}: hold wns");
        assert_eq!(
            s.setup_tns_ns.to_bits(),
            e.setup_tns.to_bits(),
            "{ctx}: setup tns"
        );
        assert_eq!(
            s.hold_tns_ns.to_bits(),
            e.hold_tns.to_bits(),
            "{ctx}: hold tns"
        );
        assert_eq!(
            (
                s.setup_violating,
                s.setup_endpoints,
                s.hold_violating,
                s.hold_endpoints
            ),
            (
                e.setup_violating,
                e.setup_endpoints,
                e.hold_violating,
                e.hold_endpoints
            ),
            "{ctx}: counts"
        );
        assert_eq!(
            (s.total_endpoints, s.unconstrained_endpoints),
            (e.total_endpoints, e.unconstrained_endpoints),
            "{ctx}: endpoints"
        );
    }

    // Endpoints and their orders in every corner.
    let n_ep = counts.endpoints as usize;
    for corner in 0..counts.corners as usize {
        let eps = shared.endpoints(corner).expect("shared endpoints");
        for (hold, kind) in [(false, None), (true, Some(CheckKind::Hold))] {
            let order = shared.endpoint_order(corner, hold).expect("shared order");
            let queried = engine
                .query_endpoints(corner, kind, n_ep)
                .expect("engine endpoints");
            assert_eq!(
                order.len(),
                queried.len(),
                "{label}: corner {corner} order length"
            );
            for (&row, q) in order.iter().zip(&queried) {
                let ctx = format!("{label}: corner {corner} endpoint {}", q.name);
                let s = &eps[row];
                assert_eq!(row, q.endpoint_idx, "{ctx}: order");
                assert_eq!(s.name, q.name, "{ctx}: name");
                assert_eq!(s.node, q.data_node.0, "{ctx}: node");
                assert_eq!(
                    bits(s.setup_slack_ns),
                    bits(finite(q.setup_slack)),
                    "{ctx}: setup"
                );
                assert_eq!(
                    bits(s.hold_slack_ns),
                    bits(finite(q.hold_slack)),
                    "{ctx}: hold"
                );
                let handle = |r: Option<usize>| r.map(|r| format!("path{}", r + 1));
                assert_eq!(
                    handle(s.worst_setup_path),
                    q.worst_setup_handle,
                    "{ctx}: worst setup"
                );
                assert_eq!(
                    handle(s.worst_hold_path),
                    q.worst_hold_handle,
                    "{ctx}: worst hold"
                );
                assert_eq!(
                    shared.node_by_name(&s.name).expect("name lookup"),
                    Some(s.node),
                    "{ctx}: name lookup"
                );
            }
        }
    }

    // Every ranked path: rank record, chain or materialized points, required time.
    let ranks = shared.path_ranks().expect("shared ranks");
    for r in &ranks {
        let s = shared.path(r.row).expect("shared path");
        let e = engine.query_path_handle(r.row).expect("engine path");
        let ctx = format!("{label}: {}", e.handle);
        assert_eq!(s.handle(), e.handle, "{ctx}: handle");
        assert_eq!(s.kind.as_str(), e.check_kind, "{ctx}: check");
        assert_eq!(
            (s.corner, &s.corner_name),
            (e.corner_idx, &e.corner_name),
            "{ctx}: corner"
        );
        assert_eq!(
            (s.endpoint, &s.endpoint_name),
            (e.endpoint_idx, &e.endpoint_name),
            "{ctx}: endpoint"
        );
        if s.slack_exact {
            assert_eq!(s.slack_ns.to_bits(), e.slack.to_bits(), "{ctx}: slack");
        } else {
            assert_eq!(s.slack_ns, f64::from(e.slack as f32), "{ctx}: rank slack");
        }
        assert_eq!(r.slack_ns, f64::from(e.slack as f32), "{ctx}: rank record");
        let (required, check_value) = shared.path_required(r.row).expect("shared required");
        assert_eq!(required.to_bits(), e.required.to_bits(), "{ctx}: required");
        assert_eq!(
            check_value.to_bits(),
            e.check_value.to_bits(),
            "{ctx}: check value"
        );
        assert_eq!(s.points.len(), e.points.len(), "{ctx}: points");
        for (sp, ep) in s.points.iter().zip(&e.points) {
            assert_eq!((sp.node, &sp.name), (ep.node.0, &ep.name), "{ctx}: point");
            assert_eq!(
                format!("{:?}", sp.kind),
                format!("{:?}", ep.kind),
                "{ctx}: {}",
                ep.name
            );
            assert_eq!(
                format!("{:?}", sp.transition),
                format!("{:?}", ep.tr),
                "{ctx}: {}",
                ep.name
            );
            match &sp.instance {
                Some(inst) => assert_eq!(format!("{inst}/{}", sp.pin), sp.name, "{ctx}"),
                None => assert_eq!(sp.pin, sp.name, "{ctx}: port"),
            }
            if s.materialized {
                assert_eq!(
                    bits(sp.arrival_ns),
                    Some(ep.arrival.to_bits()),
                    "{ctx}: {}",
                    ep.name
                );
            }
        }
    }
    ranks.len()
}

/// `n` flip-flops of `wt_full` whose data inputs all take a balanced XOR tree of their outputs:
/// `2 n` ranked paths, more than the table materializes.
fn xor_feedback(n: usize) -> String {
    let mut v =
        String::from("module xr (input clk_i, input rst_ni, input en_i, input clr_i, output y);\n");
    let mut level: Vec<String> = (0..n).map(|i| format!("q{i}")).collect();
    let mut cells = String::new();
    let mut g = 0;
    while level.len() > 1 {
        let mut next = Vec::new();
        for pair in level.chunks(2) {
            if let [a, b] = pair {
                let o = format!("x{g}");
                cells.push_str(&format!("  wtf_xor2_1 g{g} (.A({a}), .B({b}), .X({o}));\n"));
                g += 1;
                next.push(o);
            } else {
                next.push(pair[0].clone());
            }
        }
        level = next;
    }
    let root = level[0].clone();
    let wires: Vec<String> = (0..n)
        .map(|i| format!("q{i}"))
        .chain((0..g).map(|i| format!("x{i}")))
        .collect();
    v.push_str(&format!("  wire {};\n", wires.join(", ")));
    v.push_str(&cells);
    for i in 0..n {
        v.push_str(&format!(
            "  wtf_dff_1 r{i} (.D({root}), .CLK(clk_i), .Q(q{i}));\n"
        ));
    }
    v.push_str(&format!("  assign y = {root};\nendmodule\n"));
    v
}

// WS-REQ-0126 to WS-REQ-0134, STA-REQ-0312 to STA-REQ-0315: both readers agree on every table
// that `ruststa run` writes for two designs in one and two corners and in both store modes,
// including ranked paths beyond the materialized ones.
#[test]
fn shared_reader_agrees_with_engine_reader() {
    let counter = corpus("counter").expect("counter fixture").text.to_string();
    let xor = xor_feedback(64);
    let full = library(Variant::Full);
    // The second corner is the same library under another name, so that its paths tie with
    // those of the first corner and the rank order has to keep the corner of every path.
    let renamed = full.replacen("wt_full", "wt_full_c2", 1);
    let setups: [(&str, Vec<(&str, &str, &str)>); 2] = [
        ("one-corner", vec![("default", &full, "wt_full.lib")]),
        (
            "two-corners",
            vec![
                ("c1", &full, "wt_full.lib"),
                ("c2", &renamed, "wt_full_c2.lib"),
            ],
        ),
    ];
    let mut materialized_and_chain = (0, 0);
    for ((tag, corners), (design, verilog)) in setups
        .iter()
        .flat_map(|s| [(s, ("counter", &counter)), (s, ("xor", &xor))])
    {
        for store in ["full", "summary"] {
            let label = format!("{design}/{tag}/{store}");
            let dir = temp_dir(&format!("{design}-{tag}-{store}"));
            let ws = common::corners_workspace(&dir, corners, verilog, SDC);
            let ws_s = dir.join("ws").to_string_lossy().into_owned();
            let (code, env) = run_json(&[
                "run",
                "-w",
                &ws_s,
                "--base-rev",
                "rev1",
                "--sta-store",
                store,
                "--max-paths",
                "40",
            ]);
            assert_eq!(code, 0, "{label}: {env}");
            let n = compare_readers(&label, &ws, "rev2");
            assert!(n > 0, "{label}: no ranked path");
            let rev = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
            let shared = StaTable::open_rev(&rev).unwrap();
            let c = shared.counts();
            materialized_and_chain.0 += c.top_k_paths as usize;
            materialized_and_chain.1 += (c.ranked_paths - c.top_k_paths) as usize;
            let _ = fs::remove_dir_all(dir);
        }
    }
    assert!(
        materialized_and_chain.0 > 0,
        "no materialized path was compared"
    );
    assert!(
        materialized_and_chain.1 > 0,
        "no path beyond the materialized ones was compared"
    );
}
