// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa run` applies timing exceptions, constants and case analysis, and `ruststa check`
//! reports every endpoint check that only one side times (`STA-REQ-0150` to `STA-REQ-0157`,
//! `STA-REQ-0271`, `STA-REQ-0307`).
//!
//! The expectations come from the specification, not from the engine: the independent checker
//! must agree, a multicycle path moves the setup and hold relations by whole periods, and a maximum
//! delay replaces the relation of an output port.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_workspace::{Invocation, JsonMap, Workspace, exit};
use ruststa::engine::store::{StoreMode, StorePrecision};
use ruststa_cli::store_build::build_store_from_analysis;
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::corpus;
use serde_json::Value;

mod common;

const PERIOD: f64 = 2.0;
const BASE_SDC: &str = "create_clock -name clk -period 2.0 [get_ports clk_i]\n\
                        set_input_delay -clock clk 0.1 [get_ports rst_ni]\n\
                        set_input_delay -clock clk 0.1 [get_ports en_i]\n\
                        set_input_delay -clock clk 0.1 [get_ports clr_i]\n\
                        set_output_delay -clock clk 0.1 [all_outputs]\n";
const TOL: f64 = 1e-9;

fn temp_dir(tag: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ruststa-exc-{}-{tag}", std::process::id()));
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

fn inv(command: &str) -> Invocation {
    Invocation::new("ruststa", "0.2.0", command, vec!["ruststa".to_string()])
}

/// A workspace whose `rev1` holds the counter of the corpus, `wt_full` and `sdc_text`.
fn counter_workspace(dir: &Path, sdc_text: &str) -> Workspace {
    let c = corpus("counter").expect("counter fixture");
    netlist_workspace(dir, c.text, sdc_text)
}

/// A workspace whose `rev1` holds the netlist `verilog` over `wt_full`, and `sdc_text`.
fn netlist_workspace(dir: &Path, verilog: &str, sdc_text: &str) -> Workspace {
    common::netlist_workspace(
        dir,
        &library(Variant::Full),
        "wt_full.lib",
        verilog,
        sdc_text,
    )
}

/// Endpoint name to `(setup, hold)` slacks of `rev`, `None` for an untimed check.
fn endpoint_slacks(ws: &str, rev: &str) -> BTreeMap<String, (Option<f64>, Option<f64>)> {
    let (code, env) = run_json(&[
        "report",
        "-w",
        ws,
        "--rev",
        rev,
        "--section",
        "endpoints",
        "--limit",
        "0",
        "--exact",
    ]);
    assert_eq!(code, exit::OK, "{env}");
    env["sections"]["endpoints"]["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| {
            (
                r["endpoint"].as_str().expect("endpoint").to_string(),
                (r["setup_slack_ns"].as_f64(), r["hold_slack_ns"].as_f64()),
            )
        })
        .collect()
}

/// Runs `ruststa run` on `rev1` and the independent checker on `rev2`, which must agree.
fn run_and_check(dir: &Path, sdc_text: &str) -> (String, Value) {
    let _ws = counter_workspace(dir, sdc_text);
    let ws = dir.join("ws").to_str().expect("path").to_string();
    let (code, run) = run_json(&["run", "-w", &ws, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK, "{run}");
    let (code, check) = run_json(&["check", "-w", &ws, "--base-rev", "rev2", "--method", "both"]);
    assert_eq!(code, exit::OK, "checker verdict: {check}");
    assert_eq!(check["verdict"], "agree");
    assert!(check["slacks_compared"].as_u64().expect("count") > 0);
    (ws, run)
}

fn close(a: Option<f64>, b: f64, ctx: &str) {
    let a = a.unwrap_or_else(|| panic!("{ctx}: untimed"));
    assert!((a - b).abs() <= TOL, "{ctx}: {a} != {b}");
}

// STA-REQ-0271, STA-REQ-0259 to STA-REQ-0262: each exception changes exactly the checks its
// definition names, and the independent checker agrees with every revision.
#[test]
fn exceptions_change_the_checks_they_name() {
    let base_dir = temp_dir("base");
    let (base_ws, _) = run_and_check(&base_dir, BASE_SDC);
    let base = endpoint_slacks(&base_ws, "rev2");

    // A multicycle setup of 2 moves the setup relation of r3/D by one period, and the hold check
    // follows the setup multiplier (STA-REQ-0260, STA-REQ-0261).
    let mcp_dir = temp_dir("mcp");
    let (mcp_ws, _) = run_and_check(
        &mcp_dir,
        &format!("{BASE_SDC}set_multicycle_path 2 -setup -to [get_pins r3/D]\n"),
    );
    let mcp = endpoint_slacks(&mcp_ws, "rev2");
    let (s0, h0) = base["r3/D"];
    close(mcp["r3/D"].0, s0.expect("setup") + PERIOD, "r3/D setup");
    close(mcp["r3/D"].1, h0.expect("hold") - PERIOD, "r3/D hold");
    for (name, slacks) in &base {
        if name != "r3/D" {
            assert_eq!(mcp[name], *slacks, "{name} must not change");
        }
    }

    // A maximum delay at an output port replaces the relation: with ideal clocks the setup slack
    // of wrap_o moves by `V - T` (STA-REQ-0262).
    let max_dir = temp_dir("max");
    let (max_ws, _) = run_and_check(
        &max_dir,
        &format!("{BASE_SDC}set_max_delay 0.6 -to [get_ports wrap_o]\n"),
    );
    let max = endpoint_slacks(&max_ws, "rev2");
    close(
        max["wrap_o"].0,
        base["wrap_o"].0.expect("setup") + 0.6 - PERIOD,
        "wrap_o setup",
    );
    assert_eq!(max["wrap_o"].1, base["wrap_o"].1, "wrap_o hold");

    // A false path removes the checks it names: every check from clr_i to wrap_o, which no
    // other startpoint reaches in this case.
    let fp_dir = temp_dir("fp");
    let (fp_ws, _) = run_and_check(
        &fp_dir,
        &format!(
            "{BASE_SDC}set_false_path -to [get_ports wrap_o]\n\
             set_false_path -to [get_pins r3/D]\n"
        ),
    );
    let fp = endpoint_slacks(&fp_ws, "rev2");
    assert!(!fp.contains_key("wrap_o") && !fp.contains_key("r3/D"));
    assert_eq!(fp.len(), base.len() - 2);

    for dir in [base_dir, mcp_dir, max_dir, fp_dir] {
        let _ = fs::remove_dir_all(dir);
    }
}

// STA-REQ-0150, STA-REQ-0152, STA-REQ-0157, STA-REQ-0271: case analysis disables the arcs it
// controls in `ruststa run` and leaves endpoints without data arrival; together with all
// exceptions the independent checker still agrees.
#[test]
fn case_analysis_and_exceptions_together() {
    let dir = temp_dir("case");
    let sdc_text = format!(
        "{BASE_SDC}set_false_path -from [get_ports clr_i]\n\
         set_multicycle_path 2 -setup -to [get_pins r3/D]\n\
         set_multicycle_path 1 -hold -to [get_pins r3/D]\n\
         set_max_delay 0.6 -to [get_ports wrap_o]\n\
         set_min_delay 0.05 -to [get_ports {{cnt_o[0]}}]\n\
         set_case_analysis 0 [get_ports en_i]\n"
    );
    let (_, run) = run_and_check(&dir, &sdc_text);
    assert!(run["completeness"]["disabled_arcs"].as_u64().expect("arcs") > 0);
    assert_eq!(
        run["completeness"]["unconstrained_by_reason"]["no_data_arrival"],
        1
    );
    let _ = fs::remove_dir_all(dir);
}

// STA-REQ-0307: a timing table that does not time a check the independent checker times, or
// that a fresh full run times, is a difference (`C0502`, `C0504`), never a vacuous agreement.
#[test]
fn checker_reports_timed_status_disagreements() {
    let dir = temp_dir("status");
    let ws = counter_workspace(&dir, BASE_SDC);
    let ws_s = dir.join("ws").to_str().expect("path").to_string();
    let rev1 = ws
        .revision(ws.resolve_query("rev1").expect("rev1"))
        .expect("rev1");
    let nl_rbin = rusteda_workspace::netlist::open_rev(&rev1).expect("netlist");
    let nl_view = rusteda_workspace::netlist::decode(&nl_rbin).expect("decode");
    let (sdc_loaded, _) = ruststa_cli::ws_io::load_sdc_from_revision(&rev1).expect("sdc");
    let mut diags = Vec::new();
    let corner_libs =
        ruststa_cli::ws_io::load_corner_libraries(&ws, &rev1, &[], &mut diags).expect("libs");
    let corners: Vec<ruststa::engine::input::constr::CornerLibs<'_>> = corner_libs
        .iter()
        .map(|(n, l)| ruststa::engine::input::constr::CornerLibs::new(n.as_str(), l.as_slice()))
        .collect();
    let compiled = ruststa::engine::input::constr::CompiledConstraints::compile(
        &sdc_loaded,
        &nl_view,
        &corners,
    )
    .expect("compile");
    let analysis = ruststa::engine::analysis::EngineAnalysis::analyze(
        &nl_view,
        &corners,
        compiled,
        &ruststa::engine::analysis::AnalysisOptions::default(),
        rusteda_core::job::Ctx::NONE,
    )
    .expect("analysis");
    let mut store = build_store_from_analysis(
        &analysis,
        &corner_libs,
        "cpu",
        StorePrecision::F64,
        StoreMode::Full,
        100,
    );
    // Plant the defect: the table claims that the first endpoint's setup check is untimed.
    assert!(store.ep_slack[0].setup_slack.is_finite());
    store.ep_slack[0].setup_slack = f64::NAN;
    let mut txn = ws
        .begin(ws.resolve_base("rev1").expect("rev1"), &inv("ruststa run"))
        .expect("begin");
    let up = txn
        .upstream(&["netlist", "constraints", "libraries"])
        .expect("upstream");
    store.stage_to_txn(&mut txn, up).expect("stage");
    // Each check commits a revision that carries the planted table forward.
    let mut base = txn.commit(JsonMap::new()).expect("commit").rev.to_string();

    for (method, code) in [("independent", "C0502"), ("full", "C0504")] {
        let (exit_code, env) = run_json(&[
            "check",
            "-w",
            &ws_s,
            "--base-rev",
            &base,
            "--method",
            method,
        ]);
        assert_eq!(exit_code, exit::NEGATIVE_VERDICT, "{method}: {env}");
        assert_eq!(env["verdict"], "disagree");
        let mismatches = env["mismatches"].as_array().expect("mismatches");
        assert!(
            mismatches
                .iter()
                .any(|m| m["code"] == code && m["sta"] == "untimed" && m["checker"] == "timed"),
            "{method}: {env}"
        );
        base = env["new_rev"].as_str().expect("new revision").to_string();
    }
    let _ = fs::remove_dir_all(dir);
}

// STA-REQ-0308, STA-REQ-0022, STA-REQ-0158, STA-REQ-0255: a successful run reports the warnings of
// its constraints and analysis: options it does not model, a transition case analysis, and an
// exception whose `-from` objects are all ignored (which then matches no path).
#[test]
fn successful_run_reports_its_warnings() {
    let dir = temp_dir("warnings");
    let _ws = counter_workspace(
        &dir,
        &format!(
            "{BASE_SDC}set_input_delay -clock clk -level_sensitive 0.1 [get_ports en_i]\n\
             set_load -subtract_pin_load 0.05 [get_ports wrap_o]\n\
             set_operating_conditions -analysis_type on_chip_variation\n\
             set_case_analysis rising [get_ports clr_i]\n\
             set_false_path -from [get_pins r3/D] -to [get_ports wrap_o]\n"
        ),
    );
    let ws = dir.join("ws").to_str().expect("path").to_string();
    let (code, run) = run_json(&["run", "-w", &ws, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK, "{run}");
    let codes: Vec<&str> = run["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    for code in ["D0110", "T0040", "T0041", "T0042", "T0043"] {
        assert!(codes.contains(&code), "missing {code} in {codes:?}");
    }
    // The exception matches no path: wrap_o stays timed and the checker agrees.
    assert!(endpoint_slacks(&ws, "rev2")["wrap_o"].0.is_some());
    let (code, check) = run_json(&["check", "-w", &ws, "--base-rev", "rev2"]);
    assert_eq!(code, exit::OK, "{check}");
    let _ = fs::remove_dir_all(dir);
}

// STA-REQ-0024: the propagation state is budgeted before it is allocated.
#[test]
fn propagation_state_budget_is_enforced() {
    let dir = temp_dir("budget");
    let _ws = counter_workspace(&dir, BASE_SDC);
    let ws = dir.join("ws").to_str().expect("path").to_string();
    let (code, env) = run_json(&[
        "run",
        "-w",
        &ws,
        "--base-rev",
        "rev1",
        "--max-state-gib",
        "0.000001",
    ]);
    assert_eq!(code, exit::REJECTED, "{env}");
    assert!(
        env["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .iter()
            .any(|d| d["code"] == "T0023"),
        "{env}"
    );
    let (code, env) = run_json(&[
        "run",
        "-w",
        &ws,
        "--base-rev",
        "rev1",
        "--max-state-gib",
        "0",
    ]);
    assert_eq!(code, exit::REJECTED, "{env}");
    assert_eq!(env["error"]["code"], "USAGE", "{env}");
    let (code, env) = run_json(&["run", "-w", &ws, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK, "{env}");
    let _ = fs::remove_dir_all(dir);
}

// STA-REQ-0506: the independent checker bounds the paths it enumerates for exception matching; a
// design beyond the budget is `undecided` (exit 5) with the reason, not an exhausted memory. Each
// of 240 flip-flops reaches every flip-flop through a balanced XOR tree, so matching the exception
// needs about 460,000 paths.
#[test]
fn checker_path_budget_gives_undecided() {
    let n = 240;
    let mut v = String::from("module xr (input clk, output y);\n");
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
            "  wtf_dff_1 r{i} (.D({root}), .CLK(clk), .Q(q{i}));\n"
        ));
    }
    v.push_str(&format!("  assign y = {root};\nendmodule\n"));
    let dir = temp_dir("path-budget");
    let _ws = netlist_workspace(
        &dir,
        &v,
        "create_clock -name clk -period 2.0 [get_ports clk]\n\
         set_multicycle_path 2 -setup -from [all_registers] -to [all_registers]\n",
    );
    let ws = dir.join("ws").to_str().expect("path").to_string();
    let (code, run) = run_json(&["run", "-w", &ws, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK, "{run}");
    let (code, check) = run_json(&[
        "check",
        "-w",
        &ws,
        "--base-rev",
        "rev2",
        "--method",
        "independent",
    ]);
    assert_eq!(code, exit::INCONCLUSIVE, "{check}");
    assert_eq!(check["verdict"], "undecided");
    let _ = fs::remove_dir_all(dir);
}
