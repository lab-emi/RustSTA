// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Planted-defect tests (`STA-REQ-0504`) for the independent workspace checker
//! (`crates/verify/ruststa-check/src/rev`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusteda_core::liberty::Library;
use rusteda_formats::liberty::read_text;
use ruststa_check::rev::{
    DEFAULT_TOLERANCE_NS, analyze_inputs, build_timing_and_scope_json, check_against_reports,
    check_revision, create_timed_workspace,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch_ws_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "ruststa_check_planted_{}_{}_{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn build_lib(scale: f64, include_inv_fall: bool) -> (String, Library) {
    let d_inv_r = 0.10 * scale;
    let d_inv_f = 0.08 * scale;
    let d_ckq_r = 0.40 * scale;
    let d_ckq_f = 0.35 * scale;
    let fall_arc = if include_inv_fall {
        format!(
            "        cell_fall (t2) {{ values (\"{d_inv_f:.6}, {:.6}\", \"{:.6}, {:.6}\"); }}\n        \
             fall_transition (t2) {{ values (\"0.04, 1.24\", \"0.04, 1.24\"); }}\n",
            d_inv_f + 1.5 * scale,
            d_inv_f + 0.15 * scale,
            d_inv_f + 1.65 * scale
        )
    } else {
        String::new()
    };
    let text = format!(
        "library (lin) {{\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  \
         lu_table_template (t2) {{\n    variable_1 : input_net_transition;\n    \
         variable_2 : total_output_net_capacitance;\n    index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }}\n  \
         cell (INV) {{\n    area : 1;\n    pin (A) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (Y) {{\n      direction : output;\n      function : \"!A\";\n      timing () {{\n        \
         related_pin : \"A\";\n        timing_sense : negative_unate;\n        timing_type : combinational;\n        \
         cell_rise (t2) {{ values (\"{d_inv_r:.6}, {:.6}\", \"{:.6}, {:.6}\"); }}\n        \
         rise_transition (t2) {{ values (\"0.05, 1.55\", \"0.05, 1.55\"); }}\n{fall_arc}      }}\n    }}\n  }}\n  \
         cell (DFF) {{\n    area : 4;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; }}\n    \
         pin (D) {{\n      direction : input;\n      capacitance : 0.01;\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : setup_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.10\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.10\"); }}\n      }}\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : hold_rising;\n        \
         rise_constraint (scalar) {{ values (\"0.05\"); }}\n        \
         fall_constraint (scalar) {{ values (\"0.05\"); }}\n      }}\n    }}\n    \
         pin (CK) {{ direction : input; capacitance : 0.01; clock : true; }}\n    \
         pin (Q) {{\n      direction : output;\n      function : \"IQ\";\n      timing () {{\n        \
         related_pin : \"CK\";\n        timing_sense : non_unate;\n        timing_type : rising_edge;\n        \
         cell_rise (t2) {{ values (\"{d_ckq_r:.6}, {:.6}\", \"{d_ckq_r:.6}, {:.6}\"); }}\n        \
         rise_transition (t2) {{ values (\"0.08, 1.08\", \"0.08, 1.08\"); }}\n        \
         cell_fall (t2) {{ values (\"{d_ckq_f:.6}, {:.6}\", \"{d_ckq_f:.6}, {:.6}\"); }}\n        \
         fall_transition (t2) {{ values (\"0.07, 1.07\", \"0.07, 1.07\"); }}\n      }}\n    }}\n  }}\n}}\n",
        d_inv_r + 2.0 * scale,
        d_inv_r + 0.20 * scale,
        d_inv_r + 2.20 * scale,
        d_ckq_r + 1.0 * scale,
        d_ckq_r + 1.0 * scale,
        d_ckq_f + 0.9 * scale,
        d_ckq_f + 0.9 * scale,
    );
    let lib = read_text(&text, "lin.lib").expect("valid library");
    (text, lib)
}

const NETLIST: &str = "
module top (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";

const SDC: &str = "
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_output_delay -clock clk 0.3 [get_ports q]
";

#[test]
fn planted_altered_endpoint_slacks_and_metrics_sta_req_0504() {
    let (_, lib) = build_lib(1.0, true);
    let analysis = analyze_inputs(&[("top.v", NETLIST)], None, &[("default", vec![lib])], SDC)
        .expect("baseline analysis");
    let (base_timing, base_scope) = build_timing_and_scope_json(&analysis);

    let ok_rep = check_against_reports(
        &analysis,
        &base_timing,
        Some(&base_scope),
        DEFAULT_TOLERANCE_NS,
    );
    assert!(ok_rep.verdict.is_agree(), "diags: {:?}", ok_rep.diagnostics);

    // 1. Altered endpoint setup_slack
    let mut t1 = base_timing.clone();
    t1["corners"]["default"]["endpoints"][0]["setup_slack"] = serde_json::json!(9.999);
    assert!(
        check_against_reports(&analysis, &t1, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 2. Altered endpoint hold_slack
    let mut t2 = base_timing.clone();
    t2["corners"]["default"]["endpoints"][0]["hold_slack"] = serde_json::json!(-1.234);
    assert!(
        check_against_reports(&analysis, &t2, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 3. Altered corner WNS / TNS
    let mut t3 = base_timing.clone();
    t3["corners"]["default"]["wns_setup"] = serde_json::json!(-0.5);
    assert!(
        check_against_reports(&analysis, &t3, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 4. Altered path slack
    let mut t4 = base_timing.clone();
    t4["paths"][0]["slack"] = serde_json::json!(-0.25);
    assert!(
        check_against_reports(&analysis, &t4, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 5. Altered endpoint status
    let mut t5 = base_timing.clone();
    t5["corners"]["default"]["endpoints"][0]["status"] = serde_json::json!("unclocked");
    assert!(
        check_against_reports(&analysis, &t5, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 6. Altered path handle order (hold path before setup path, STA-REQ-0228)
    let mut t6 = base_timing.clone();
    t6["paths"][0]["kind"] = serde_json::json!("hold");
    assert!(
        check_against_reports(&analysis, &t6, Some(&base_scope), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );

    // 7. Altered scope report constrained_endpoints
    let mut s7 = base_scope.clone();
    s7["constrained_endpoints"] = serde_json::json!(99);
    assert!(
        check_against_reports(&analysis, &base_timing, Some(&s7), DEFAULT_TOLERANCE_NS)
            .verdict
            .is_disagree()
    );
}

#[test]
fn planted_scaled_table_and_removed_arc_sta_req_0504() {
    let (_, lib_1x) = build_lib(1.0, true);
    let analysis_1x = analyze_inputs(
        &[("top.v", NETLIST)],
        None,
        &[("default", vec![lib_1x])],
        SDC,
    )
    .expect("1x analysis");
    let (timing_1x, scope_1x) = build_timing_and_scope_json(&analysis_1x);

    // 1. Scaled cell table in the checker's input (1.5x) vs 1.0x report (`STA-REQ-0504`)
    let (_, lib_15x) = build_lib(1.5, true);
    let analysis_15x = analyze_inputs(
        &[("top.v", NETLIST)],
        None,
        &[("default", vec![lib_15x])],
        SDC,
    )
    .expect("1.5x analysis");
    assert!(
        check_against_reports(
            &analysis_15x,
            &timing_1x,
            Some(&scope_1x),
            DEFAULT_TOLERANCE_NS
        )
        .verdict
        .is_disagree()
    );

    // 2. Removed timing arc (`INV` missing `cell_fall`) vs full report (`STA-REQ-0504`)
    let (lib_no_fall_text, lib_no_fall) = build_lib(1.0, false);
    let analysis_no_fall = analyze_inputs(
        &[("top.v", NETLIST)],
        None,
        &[("default", vec![lib_no_fall])],
        SDC,
    )
    .expect("removed-arc analysis");
    assert!(
        check_against_reports(
            &analysis_no_fall,
            &timing_1x,
            Some(&scope_1x),
            DEFAULT_TOLERANCE_NS
        )
        .verdict
        .is_disagree()
    );

    // 3. End-to-end workspace revision with planted defect
    let ws_path = scratch_ws_path("removed_arc");
    create_timed_workspace(
        &ws_path,
        NETLIST,
        &[("default", "lin", &lib_no_fall_text)],
        SDC,
        &timing_1x,
        &scope_1x,
    )
    .expect("create workspace");
    let ws_rep = check_revision(&ws_path, None, Some(DEFAULT_TOLERANCE_NS));
    let _ = std::fs::remove_dir_all(&ws_path);
    assert!(ws_rep.verdict.is_disagree());
}
