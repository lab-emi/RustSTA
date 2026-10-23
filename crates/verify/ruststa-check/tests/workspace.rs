// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! End-to-end workspace revision tests (`STA-REQ-0500`, `0502`) for the independent checker
//! (`crates/verify/ruststa-check/src/rev`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ruststa_check::rev::{
    DEFAULT_TOLERANCE_NS, analyze_inputs, build_timing_and_scope_json, check_revision,
    create_timed_workspace,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch_ws_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "ruststa_check_ws_{}_{}_{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

const LIB_TEXT: &str = "
library (lin) {
  delay_model : table_lookup;
  time_unit : \"1ns\";
  capacitive_load_unit (1, pf);
  lu_table_template (t2) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1 (\"0, 1\");
    index_2 (\"0, 1\");
  }
  cell (INV) {
    area : 1;
    pin (A) { direction : input; capacitance : 0.01; }
    pin (Y) {
      direction : output;
      function : \"!A\";
      timing () {
        related_pin : \"A\";
        timing_sense : negative_unate;
        timing_type : combinational;
        cell_rise (t2) { values (\"0.10, 2.10\", \"0.30, 2.30\"); }
        rise_transition (t2) { values (\"0.05, 1.55\", \"0.05, 1.55\"); }
        cell_fall (t2) { values (\"0.08, 1.58\", \"0.23, 1.73\"); }
        fall_transition (t2) { values (\"0.04, 1.24\", \"0.04, 1.24\"); }
      }
    }
  }
  cell (BUF) {
    area : 1;
    pin (A) { direction : input; capacitance : 0.01; }
    pin (Y) {
      direction : output;
      function : \"A\";
      timing () {
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (t2) { values (\"0.20, 1.20\", \"0.30, 1.30\"); }
        rise_transition (t2) { values (\"0.06, 1.06\", \"0.06, 1.06\"); }
        cell_fall (t2) { values (\"0.18, 1.38\", \"0.28, 1.48\"); }
        fall_transition (t2) { values (\"0.05, 1.15\", \"0.05, 1.15\"); }
      }
    }
  }
  cell (AND2) {
    area : 1;
    pin (A) { direction : input; capacitance : 0.01; }
    pin (B) { direction : input; capacitance : 0.01; }
    pin (Y) {
      direction : output;
      function : \"A&B\";
      timing () {
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (t2) { values (\"0.20, 1.20\", \"0.30, 1.30\"); }
        rise_transition (t2) { values (\"0.06, 1.06\", \"0.06, 1.06\"); }
        cell_fall (t2) { values (\"0.18, 1.38\", \"0.28, 1.48\"); }
        fall_transition (t2) { values (\"0.05, 1.15\", \"0.05, 1.15\"); }
      }
      timing () {
        related_pin : \"B\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (t2) { values (\"0.20, 1.20\", \"0.30, 1.30\"); }
        rise_transition (t2) { values (\"0.06, 1.06\", \"0.06, 1.06\"); }
        cell_fall (t2) { values (\"0.18, 1.38\", \"0.28, 1.48\"); }
        fall_transition (t2) { values (\"0.05, 1.15\", \"0.05, 1.15\"); }
      }
    }
  }
  cell (DFF) {
    area : 4;
    ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }
    pin (D) {
      direction : input;
      capacitance : 0.01;
      timing () {
        related_pin : \"CK\";
        timing_type : setup_rising;
        rise_constraint (scalar) { values (\"0.10\"); }
        fall_constraint (scalar) { values (\"0.10\"); }
      }
      timing () {
        related_pin : \"CK\";
        timing_type : hold_rising;
        rise_constraint (scalar) { values (\"0.05\"); }
        fall_constraint (scalar) { values (\"0.05\"); }
      }
    }
    pin (CK) { direction : input; capacitance : 0.01; clock : true; }
    pin (Q) {
      direction : output;
      function : \"IQ\";
      timing () {
        related_pin : \"CK\";
        timing_sense : non_unate;
        timing_type : rising_edge;
        cell_rise (t2) { values (\"0.40, 1.40\", \"0.40, 1.40\"); }
        rise_transition (t2) { values (\"0.08, 1.08\", \"0.08, 1.08\"); }
        cell_fall (t2) { values (\"0.35, 1.25\", \"0.35, 1.25\"); }
        fall_transition (t2) { values (\"0.07, 1.07\", \"0.07, 1.07\"); }
      }
    }
  }
}
";

#[test]
fn workspace_check_revision_agree_and_exhaustive_paths_sta_req_0500_0502() {
    let netlist = "
module top (input clk, input a, input b, output q);
 wire n1, n2, n3, n4;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 DFF r2 (.D(b), .CK(clk), .Q(n2));
 BUF b1 (.A(n1), .Y(n3));
 AND2 g1 (.A(n3), .B(n2), .Y(n4));
 DFF r3 (.D(n4), .CK(clk), .Q(q));
endmodule
";
    let sdc = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a b}]
set_output_delay -clock clk 0.3 [get_ports q]
";
    let lib = rusteda_formats::liberty::read_text(LIB_TEXT, "lin.lib").expect("valid lib");
    let analysis = analyze_inputs(
        &[("top.v", netlist)],
        Some("top"),
        &[("fast", vec![lib.clone()]), ("slow", vec![lib])],
        sdc,
    )
    .expect("analysis");
    assert!(
        analysis.diagnostics.iter().all(|d| d.code != "C0508"),
        "unexpected exhaustive path mismatch: {:?}",
        analysis.diagnostics
    );

    let (timing_json, scope_json) = build_timing_and_scope_json(&analysis);

    let ws_path = scratch_ws_path("agree");
    create_timed_workspace(
        &ws_path,
        netlist,
        &[("fast", "lin", LIB_TEXT), ("slow", "lin", LIB_TEXT)],
        sdc,
        &timing_json,
        &scope_json,
    )
    .expect("create workspace");

    let rep = check_revision(&ws_path, None, Some(DEFAULT_TOLERANCE_NS));
    let _ = std::fs::remove_dir_all(&ws_path);
    assert!(rep.verdict.is_agree(), "diags: {:?}", rep.diagnostics);
    assert!(rep.endpoints_compared >= 6);
}

#[test]
fn workspace_combinational_loop_scc_breaking_and_undecided_stubs() {
    let loop_v = "
module top (input clk, input a, output q);
 wire qa, x, y;
 DFF r1 (.D(a), .CK(clk), .Q(qa));
 AND2 g1 (.A(qa), .B(y), .Y(x));
 BUF g2 (.A(x), .Y(y));
 DFF r2 (.D(y), .CK(clk), .Q(q));
endmodule
";
    let sdc = "create_clock -name clk -period 2 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports a]\n";
    let lib = rusteda_formats::liberty::read_text(LIB_TEXT, "lin.lib").expect("valid lib");
    let analysis = analyze_inputs(
        &[("top.v", loop_v)],
        Some("top"),
        &[("default", vec![lib])],
        sdc,
    )
    .expect("loop analysis");
    assert_eq!(analysis.corners["default"].scope.sccs_broken, 1);
    assert!(analysis.diagnostics.iter().any(|d| d.code == "D0102"));

    let (timing_json, scope_json) = build_timing_and_scope_json(&analysis);

    for (idx, unsupported_sdc) in [
        "create_clock -name clk -period 2 [get_ports clk]\ncreate_generated_clock -name gclk -source [get_ports clk] -divide_by 2 [get_pins r1/Q]\n",
        "create_clock -name clk -period 2 [get_ports clk]\nset_propagated_clock [get_clocks clk]\n",
        "create_clock -name clk -period 2 [get_ports clk]\nset_false_path -through [get_pins g2/Y]\n",
    ]
    .into_iter()
    .enumerate()
    {
        let ws_path = scratch_ws_path(&format!("undecided_{idx}"));
        create_timed_workspace(
            &ws_path,
            loop_v,
            &[("default", "lin", LIB_TEXT)],
            unsupported_sdc,
            &timing_json,
            &scope_json,
        )
        .expect("create workspace");
        let rep = check_revision(&ws_path, None, Some(DEFAULT_TOLERANCE_NS));
        let _ = std::fs::remove_dir_all(&ws_path);
        assert!(rep.verdict.is_undecided(), "expected Undecided, got {:?}", rep.verdict);
    }
}

#[test]
fn reconvergent_diamond_ladder_bounded_memory_and_time_sta_req_0502() {
    // A 30-stage reconvergent diamond ladder has < 200 pins (<= 5000) but > 2^30 (~10^9) simple
    // paths. Verify that both normal path enumeration and exception path evaluation use O(V + E)
    // topological DP rather than O(2^L) unbounded DFS, completing in < 500ms with 0 C0502 errors.
    let mut v = String::from("module top (input clk, input a, input b, output q);\n wire s0a, s0b");
    for i in 1..=30 {
        v.push_str(&format!(", s{i}a, s{i}b"));
    }
    v.push_str(";\n DFF r1 (.D(a), .CK(clk), .Q(s0a));\n DFF r2 (.D(b), .CK(clk), .Q(s0b));\n");
    for i in 1..=30 {
        let p = i - 1;
        v.push_str(&format!(
            " AND2 g{i}a (.A(s{p}a), .B(s{p}b), .Y(s{i}a));\n INV g{i}b (.A(s{p}a), .Y(s{i}b));\n"
        ));
    }
    v.push_str(" DFF r3 (.D(s30a), .CK(clk), .Q(q));\nendmodule\n");

    let lib = rusteda_formats::liberty::read_text(LIB_TEXT, "lin.lib").expect("valid lib");
    for sdc in [
        "create_clock -name clk -period 20 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a b}]\nset_output_delay -clock clk 0.1 [get_ports q]\n",
        "create_clock -name clk -period 20 [get_ports clk]\nset_input_delay -clock clk 0.1 [get_ports {a b}]\nset_output_delay -clock clk 0.1 [get_ports q]\nset_multicycle_path 2 -setup -from [get_pins r1/CK] -to [get_pins r3/D]\n",
    ] {
        let t0 = std::time::Instant::now();
        let analysis = analyze_inputs(
            &[("top.v", &v)],
            Some("top"),
            &[("default", vec![lib.clone()])],
            sdc,
        )
        .expect("diamond ladder analysis");
        assert!(t0.elapsed().as_millis() < 500, "took {:?}", t0.elapsed());
        assert!(
            analysis.diagnostics.iter().all(|d| d.code != "C0502"),
            "unexpected C0502: {:?}",
            analysis.diagnostics
        );
        assert!(analysis.corners["default"].paths.len() <= 64);
    }
}
