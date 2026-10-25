// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for `ruststa-cli` on the workspace (CLI-REQ-0001 to 0016,
//! CLI-REQ-0100 to 0103, CLI-REQ-0150, PROTO-REQ-0001 to 0062,
//! PROTO-REQ-0100 to 0132, STA-REQ-0300 to 0315,
//! STA-REQ-0500 to 0570, JOB-REQ-0005).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc::{self, Env as SdcEnv, Limits as SdcLimits, Source as SdcSource};
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_workspace::text::{shell_join, shell_quote};
use rusteda_workspace::{
    ErrorCode, Hash64, Invocation, JsonMap, RbinWriter, Upstream, Workspace, exit, rlib,
};
use ruststa::engine::store::{StaStore, StoreMode, StorePrecision};
use ruststa_cli::store_build::build_store_from_analysis;
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::corpus;
use serde_json::Value;

const PREFIX_KEYS: &[&str] = &[
    "schema",
    "ok",
    "command",
    "workspace",
    "base_rev",
    "rev",
    "new_rev",
    "scope",
    "completeness",
    "metrics",
];
const SUFFIX_KEYS: &[&str] = &["diagnostics", "error", "next_actions"];

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ruststa"))
}

fn temp_dir(tag: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "ruststa-cli-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("create temp dir");
    base
}

fn run_cmd(args: &[&str]) -> Output {
    run_cmd_in(Path::new("."), args)
}

fn run_cmd_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(cwd)
        .args(args)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .expect("spawn ruststa")
}

fn run_json(args: &[&str]) -> (i32, Value, String, String) {
    let mut full_args = vec!["--format", "json"];
    full_args.extend_from_slice(args);
    let out = run_cmd(&full_args);
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert_envelope_key_order(&stdout);
    let val: Value = serde_json::from_str(stdout.trim()).expect("valid json envelope on stdout");
    (code, val, stdout, stderr)
}

/// Verifies top-level `rusteda.v1` envelope prefix and suffix keys appear in exact contract order.
fn assert_envelope_key_order(raw_json: &str) {
    let keys = top_level_keys(raw_json);
    assert!(
        keys.len() >= PREFIX_KEYS.len() + SUFFIX_KEYS.len(),
        "envelope has fewer keys than standard keys: {keys:?}\nraw:\n{raw_json}"
    );
    assert_eq!(
        &keys[..PREFIX_KEYS.len()],
        PREFIX_KEYS,
        "envelope prefix standard keys out of order"
    );
    assert_eq!(
        &keys[keys.len() - SUFFIX_KEYS.len()..],
        SUFFIX_KEYS,
        "envelope suffix standard keys out of order"
    );
}

fn top_level_keys(raw_json: &str) -> Vec<&str> {
    raw_json
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("  \"")?;
            let (key, _) = rest.split_once("\": ")?;
            Some(key)
        })
        .collect()
}

fn nested_object_keys<'a>(raw_json: &'a str, field_indent_prefix: &str) -> Vec<&'a str> {
    raw_json
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix(field_indent_prefix)?;
            if rest.starts_with(' ') || !rest.starts_with('"') {
                return None;
            }
            let rest = rest.strip_prefix('"')?;
            let (key, _) = rest.split_once("\": ")?;
            Some(key)
        })
        .collect()
}

/// Parses a POSIX-quoted command line (`WS-REQ-0096`) back into words.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut in_word) = (false, false);
    let mut it = line.chars();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                quoted = !quoted;
                in_word = true;
            }
            '\\' if !quoted => {
                cur.extend(it.next());
                in_word = true;
            }
            ' ' if !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

fn parse_verilog_to_frozen(
    name: &str,
    verilog_text: &str,
    lib: &Library,
) -> FrozenNetlist<'static> {
    read_structural(verilog_text, name, &|cell_name| {
        let cell = lib.cells.iter().find(|c| c.name == cell_name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => PinDir::Input,
                        Direction::Output => PinDir::Output,
                        Direction::Inout => PinDir::Inout,
                        Direction::Internal => PinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    })
    .expect("read_structural")
    .freeze()
}

/// Initializes a workspace at `ws_path` (`rev0`) and commits `rev1` with `wt_full.lib`,
/// `verilog_text`, and `sdc_text` (matching `rusteda init` + `rusteda import`).
fn init_and_import_ws(
    ws_path: &Path,
    verilog_name: &str,
    verilog_text: &str,
    sdc_text: &str,
) -> Workspace {
    let inv_init = Invocation::new(
        "rusteda",
        "0.2.0",
        "rusteda init",
        vec!["rusteda".to_string(), "init".to_string()],
    );
    let init = Workspace::init(ws_path, &inv_init).expect("init workspace");
    let ws = init.workspace;

    import_revision(&ws, "rev0", verilog_name, verilog_text, sdc_text);
    ws
}

fn import_revision(
    ws: &Workspace,
    base_rev_str: &str,
    verilog_name: &str,
    verilog_text: &str,
    sdc_text: &str,
) {
    let lib_text = library(Variant::Full);
    let lib = read_text(&lib_text, "wt_full.lib").expect("parse wt_full.lib");
    let cached = rlib::put(ws, &lib, lib_text.as_bytes()).expect("rlib put");
    let frozen = parse_verilog_to_frozen(verilog_name, verilog_text, &lib);
    let (sdc_model, _) = sdc::read(
        &[SdcSource::with_hash(
            "counter.sdc",
            sdc_text,
            Hash64::of(sdc_text.as_bytes()).0,
        )],
        &SdcEnv::default(),
        &SdcLimits::default(),
    );

    let inv_imp = Invocation::new(
        "rusteda",
        "0.2.0",
        "rusteda import",
        vec!["rusteda".to_string(), "import".to_string()],
    );
    let base = ws.resolve_base(base_rev_str).expect("resolve base");
    let mut txn = ws.begin(base, &inv_imp).expect("begin import txn");
    txn.add_library(cached.entry("default", "wt_full.lib"))
        .expect("add library");
    rusteda_workspace::netlist::add_to_txn(&mut txn, &frozen, Upstream::new())
        .expect("add netlist");
    txn.add_source(
        "netlist",
        verilog_name.to_string(),
        Hash64::of(verilog_text.as_bytes()),
    )
    .expect("add netlist source");

    let sections = sdc_model.to_sections();
    let mut writer = RbinWriter::new("CONSTR", 1).expect("CONSTR writer");
    for sec in &sections {
        writer
            .section_bytes(u32::from(sec.id), sec.elem_size, sec.bytes.as_slice())
            .expect("section_bytes");
    }
    txn.add_rbin("constraints", "constraints.rbin", &writer, Upstream::new())
        .expect("add constraints");
    txn.add_source(
        "constraints",
        "counter.sdc".to_string(),
        Hash64::of(sdc_text.as_bytes()),
    )
    .expect("add sdc source");
    txn.commit(JsonMap::new()).expect("commit import rev");
}

fn setup_counter_ws(ws_path: &Path) -> Workspace {
    let c = corpus("counter").expect("counter fixture");
    let sdc_text = "create_clock -name clk -period 2.0 [get_ports clk_i]\n\
                    set_input_delay -clock clk 0.1 [get_ports rst_ni]\n\
                    set_input_delay -clock clk 0.1 [get_ports en_i]\n\
                    set_input_delay -clock clk 0.1 [get_ports clr_i]\n\
                    set_output_delay -clock clk 0.1 [all_outputs]\n";
    init_and_import_ws(ws_path, "counter.v", c.text, sdc_text)
}

fn is_valid_handle(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Executes every `ruststa ...` command in `next_actions` and verifies it exits `0`.
fn assert_next_actions_run_and_exit_0(env: &Value, cwd: Option<&Path>) {
    let actions = env["next_actions"].as_array().expect("next_actions array");
    assert!(!actions.is_empty(), "expected non-empty next_actions");
    for action_val in actions {
        let action_str = action_val.as_str().expect("next_action string");
        let parsed = words(action_str);
        assert!(!parsed.is_empty());
        if parsed[0] == "ruststa" {
            let arg_refs: Vec<&str> = parsed[1..].iter().map(String::as_str).collect();
            let out = match cwd {
                Some(dir) => run_cmd_in(dir, &arg_refs),
                None => run_cmd(&arg_refs),
            };
            assert_eq!(
                out.status.code(),
                Some(0),
                "next_action `{action_str}` failed with {:?}\nstdout: {}\nstderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// STA-REQ-0500 to STA-REQ-0504, STA-REQ-0508, STA-REQ-0570:
/// Counter flow (`init` -> `import` -> `ruststa run --format json`), envelope key order,
/// `scope`/`completeness`/`metrics`/`top_paths` key order, `meta.json` `summary.sta` and
/// `status.sta`, and `next_actions` execution.
#[test]
fn counter_flow_and_envelope_key_order() {
    let dir = temp_dir("counter-flow");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let ws = setup_counter_ws(&ws_path);

    let (code, env, raw_stdout, stderr) = run_json(&["run", "-w", ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK, "stderr: {stderr}");
    assert_eq!(env["schema"], "rusteda.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["command"], "ruststa run");
    assert_eq!(env["workspace"], ws_s);
    assert_eq!(env["base_rev"], "rev1");
    assert_eq!(env["rev"], Value::Null);
    assert_eq!(env["new_rev"], "rev2");
    assert_eq!(env["error"], Value::Null);

    // Check exact top-level key order of `ruststa run`.
    assert_eq!(
        top_level_keys(&raw_stdout),
        vec![
            "schema",
            "ok",
            "command",
            "workspace",
            "base_rev",
            "rev",
            "new_rev",
            "scope",
            "completeness",
            "metrics",
            "top_paths",
            "truncated_paths",
            "diagnostics",
            "error",
            "next_actions",
        ]
    );

    // Check `scope`, `completeness`, `metrics` key orders.
    let raw_scope_start = raw_stdout.find("  \"scope\": {").unwrap();
    let raw_scope_end = raw_stdout[raw_scope_start..].find("\n  },").unwrap() + raw_scope_start;
    assert_eq!(
        nested_object_keys(&raw_stdout[raw_scope_start..raw_scope_end], "    "),
        vec![
            "corner",
            "wire_model",
            "parasitics",
            "drc_status",
            "wire_load",
            "delay_calculator",
            "clock_model",
            "cppr",
            "backend",
            "precision",
            "pulse_rules",
            "drv_rules",
        ]
    );

    let raw_comp_start = raw_stdout.find("  \"completeness\": {").unwrap();
    let raw_comp_end = raw_stdout[raw_comp_start..].find("\n  },").unwrap() + raw_comp_start;
    assert_eq!(
        nested_object_keys(&raw_stdout[raw_comp_start..raw_comp_end], "    "),
        vec![
            "constrained_endpoints",
            "unconstrained_endpoints",
            "clock_domains",
            "unconstrained_by_reason",
            "unconstrained_inputs",
            "disabled_arcs",
            "unanalyzed_arcs",
            "unchecked_pulse_width_pins",
            "pass_through_pulse_pins",
            "unevaluated_limit_checks",
            "drv_constant_drivers",
            "drv_static_transition_pins",
            "drv_ideal_clock_pins",
            "drv_untimed_pins",
            "drv_clock_scope_pins",
            "drv_unconnected_output_pins",
            "drv_unlimited",
        ]
    );

    let raw_met_start = raw_stdout.find("  \"metrics\": {").unwrap();
    let raw_met_end = raw_stdout[raw_met_start..].find("\n  },").unwrap() + raw_met_start;
    let metric_keys = nested_object_keys(&raw_stdout[raw_met_start..raw_met_end], "    ");
    assert_eq!(
        metric_keys[..8],
        [
            "wns_ns",
            "tns_ns",
            "violating_endpoints",
            "whs_ns",
            "ths_ns",
            "hold_violating_endpoints",
            "worst_setup_slack_ns",
            "worst_hold_slack_ns",
        ]
    );
    // The fifteen pin limit metrics follow (STA-REQ-0807; their order: `tests/signoff_cli.rs`).
    assert_eq!(metric_keys.len(), 23);

    let top_paths = env["top_paths"].as_array().expect("top_paths array");
    assert!(!top_paths.is_empty());
    assert_eq!(top_paths[0]["handle"], "path1");
    assert!(is_valid_handle(top_paths[0]["handle"].as_str().unwrap()));

    // Check `rev2/meta.json` has `tables.sta`, `summary.sta`, and `status.sta`.
    let rev2 = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
    let meta = rev2.meta();
    assert!(meta.tables.contains_key("sta"));
    assert_eq!(meta.tables["sta"].path, "sta.rbin");
    assert!(meta.summary.contains_key("sta"));
    assert!(meta.status.contains_key("sta"));
    assert_eq!(meta.status["sta"]["timing"], "met");

    // Every printed next_action runs and exits 0.
    assert_next_actions_run_and_exit_0(&env, None);

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0510 to STA-REQ-0515:
/// `ruststa update` modes (`up_to_date`, `incremental`, `full`, `--verify-full`, `--check`).
#[test]
fn update_modes_and_verify_full() {
    let dir = temp_dir("update-modes");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let _ws = setup_counter_ws(&ws_path);

    // 1. Run `ruststa update` on `rev1` (no prior `sta` table) -> `incremental.mode == "full"`.
    let (code, env_full, raw_full, _) = run_json(&["update", "-w", ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, exit::OK);
    assert_eq!(env_full["command"], "ruststa update");
    assert_eq!(env_full["incremental"]["mode"], "full");
    assert_eq!(env_full["new_rev"], "rev2");
    assert_eq!(
        top_level_keys(&raw_full),
        vec![
            "schema",
            "ok",
            "command",
            "workspace",
            "base_rev",
            "rev",
            "new_rev",
            "scope",
            "completeness",
            "metrics",
            "incremental",
            "metric_deltas",
            "top_paths",
            "truncated_paths",
            "diagnostics",
            "error",
            "next_actions",
        ]
    );
    assert_next_actions_run_and_exit_0(&env_full, None);

    // 2. Run `ruststa update` on `rev2` (`sta` already up to date) -> `incremental.mode == "up_to_date"`, no new revision.
    let (code_utd, env_utd, _, _) = run_json(&["update", "-w", ws_s, "--base-rev", "rev2"]);
    assert_eq!(code_utd, exit::OK);
    assert_eq!(env_utd["incremental"]["mode"], "up_to_date");
    assert_eq!(env_utd["incremental"]["computation"], "none");
    assert_eq!(env_utd["base_rev"], "rev2");
    assert_eq!(env_utd["new_rev"], Value::Null);
    assert_next_actions_run_and_exit_0(&env_utd, None);

    // 3. Create a 10-cell netlist in a second workspace, run STA (`rev2`), then swap 1 cell (10% <= 25%)
    // in `rev3` while carrying `sta` from `rev2`, and run `ruststa update --verify-full --check`.
    let ws2_path = dir.join("ws_incr");
    let ws2_s = ws2_path.to_str().unwrap();
    let v10_a = "\
module top (input clk, input rst_ni, input a, output y);
  wire n1, n2, n3, n4, n5, n6, n7, n8, n9;
  wtf_buf_1 u1 (.A(a), .X(n1));
  wtf_buf_1 u2 (.A(n1), .X(n2));
  wtf_buf_1 u3 (.A(n2), .X(n3));
  wtf_buf_1 u4 (.A(n3), .X(n4));
  wtf_dffrq_1 r1 (.CLK(clk), .RESET_B(rst_ni), .D(n4), .Q(n5));
  wtf_buf_1 u5 (.A(n5), .X(n6));
  wtf_buf_1 u6 (.A(n6), .X(n7));
  wtf_buf_1 u7 (.A(n7), .X(n8));
  wtf_buf_1 u8 (.A(n8), .X(n9));
  wtf_dffrq_1 r2 (.CLK(clk), .RESET_B(rst_ni), .D(n9), .Q(y));
endmodule
";
    let sdc10 = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                 set_input_delay -clock clk 0.1 [get_ports rst_ni]\n\
                 set_input_delay -clock clk 0.1 [get_ports a]\n\
                 set_output_delay -clock clk 0.1 [get_ports y]\n";
    let ws2 = init_and_import_ws(&ws2_path, "top.v", v10_a, sdc10);
    let (c_run, _, _, _) = run_json(&["run", "-w", ws2_s, "--base-rev", "rev1"]);
    assert_eq!(c_run, exit::OK);

    // Modify 1 of 10 instances (`u7` from `wtf_buf_1` to `wtf_buf_2`) on top of `rev2` so `rev3` still carries `sta` from `rev2`.
    let v10_b = v10_a.replace(
        "wtf_buf_1 u7 (.A(n7), .X(n8));",
        "wtf_buf_2 u7 (.A(n7), .X(n8));",
    );
    let lib_text = library(Variant::Full);
    let lib = read_text(&lib_text, "wt_full.lib").unwrap();
    let frozen_b = parse_verilog_to_frozen("top.v", &v10_b, &lib);
    let inv_eco = Invocation::new(
        "rusteda",
        "0.2.0",
        "rusteda import",
        vec!["rusteda".to_string()],
    );
    let mut txn_eco = ws2
        .begin(ws2.resolve_base("rev2").unwrap(), &inv_eco)
        .unwrap();
    rusteda_workspace::netlist::add_to_txn(&mut txn_eco, &frozen_b, Upstream::new()).unwrap();
    txn_eco.commit(JsonMap::new()).unwrap(); // commits rev3 with new netlist and carried `sta`

    let (c_inc, env_inc, _, stderr_inc) = run_json(&[
        "update",
        "-w",
        ws2_s,
        "--base-rev",
        "rev3",
        "--verify-full",
        "--check",
    ]);
    assert_eq!(c_inc, exit::OK, "env: {env_inc}\nstderr: {stderr_inc}");
    assert_eq!(env_inc["incremental"]["mode"], "incremental");
    // STA-REQ-0610: the values come from the cone of the change over the table of rev2.
    assert_eq!(env_inc["incremental"]["computation"], "incremental");
    assert_eq!(env_inc["incremental"]["from_rev"], "rev2");
    assert!(
        env_inc["incremental"]["changed_instances"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(env_inc["incremental"]["dirty_nodes"].as_u64().unwrap() > 0);
    assert!(env_inc["metric_deltas"].is_object());
    assert_eq!(env_inc["check"]["verdict"], "agree");
    assert_next_actions_run_and_exit_0(&env_inc, None);

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0520 to STA-REQ-0526:
/// `ruststa check` (`independent`, `full`, `both`) and planted-defect detection (`exit 3`, `mm1`).
#[test]
fn check_methods_and_planted_defect_exit_3() {
    let dir = temp_dir("check-methods");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let ws = setup_counter_ws(&ws_path);

    let (c_run, _, _, _) = run_json(&["run", "-w", ws_s, "--base-rev", "rev1"]);
    assert_eq!(c_run, exit::OK);

    // 1. `independent`, `full`, `both` all pass on clean `rev2` -> `rev3` -> `rev4` -> `rev5`.
    for (idx, method) in ["independent", "full", "both"].iter().enumerate() {
        let base_rev = format!("rev{}", idx + 2);
        let (code, env, raw, stderr) = run_json(&[
            "check",
            "-w",
            ws_s,
            "--base-rev",
            &base_rev,
            "--method",
            method,
        ]);
        assert_eq!(
            code,
            exit::OK,
            "method {method} env: {env}\nstderr: {stderr}"
        );
        assert_eq!(env["ok"], true);
        assert_eq!(env["command"], "ruststa check");
        assert_eq!(env["verdict"], "agree");
        assert_eq!(env["method"], *method);
        assert_eq!(env["mismatches"].as_array().unwrap().len(), 0);
        if idx == 0 {
            assert_eq!(
                top_level_keys(&raw),
                vec![
                    "schema",
                    "ok",
                    "command",
                    "workspace",
                    "base_rev",
                    "rev",
                    "new_rev",
                    "scope",
                    "completeness",
                    "metrics",
                    "verdict",
                    "method",
                    "methods",
                    "tolerance_ns",
                    "slacks_compared",
                    "max_abs_difference_ns",
                    "mismatches",
                    "diagnostics",
                    "error",
                    "next_actions",
                ]
            );
        }
        assert_next_actions_run_and_exit_0(&env, None);
    }

    // 2. Plant a timing defect in a new revision's `sta.rbin` and verify `ruststa check` exits 3 with `mm1`.
    let rev1 = ws.revision(ws.resolve_query("rev1").unwrap()).unwrap();
    let nl_rbin = rusteda_workspace::netlist::open_rev(&rev1).unwrap();
    let nl_view = rusteda_workspace::netlist::decode(&nl_rbin).unwrap();
    let (sdc_loaded, _) = ruststa_cli::ws_io::load_sdc_from_revision(&rev1).unwrap();
    let mut diags = Vec::new();
    let corner_libs =
        ruststa_cli::ws_io::load_corner_libraries(&ws, &rev1, &[], &mut diags).unwrap();
    let corner_refs: Vec<ruststa::engine::input::constr::CornerLibs<'_>> = corner_libs
        .iter()
        .map(|(n, l)| ruststa::engine::input::constr::CornerLibs::new(n.as_str(), l.as_slice()))
        .collect();
    let compiled = ruststa::engine::input::constr::CompiledConstraints::compile(
        &sdc_loaded,
        &nl_view,
        &corner_refs,
    )
    .unwrap();
    let analysis = ruststa::engine::analysis::EngineAnalysis::analyze(
        &nl_view,
        &corner_refs,
        compiled,
        &ruststa::engine::analysis::AnalysisOptions::default(),
        rusteda_core::job::Ctx::NONE,
    )
    .unwrap();
    let mut store_data = build_store_from_analysis(
        &analysis,
        &corner_libs,
        "cpu",
        StorePrecision::F64,
        StoreMode::Full,
        100,
    );
    // Perturb the first endpoint's setup slack by +0.25 ns.
    store_data.ep_slack[0].setup_slack += 0.25;

    let inv_bad = Invocation::new(
        "ruststa",
        "0.2.0",
        "ruststa run",
        vec!["ruststa".to_string()],
    );
    let mut txn_bad = ws
        .begin(ws.resolve_base("rev5").unwrap(), &inv_bad)
        .unwrap();
    let up = txn_bad
        .upstream(&["netlist", "constraints", "libraries"])
        .unwrap();
    store_data.stage_to_txn(&mut txn_bad, up).unwrap();
    let bad_committed = txn_bad.commit(JsonMap::new()).unwrap();
    let bad_rev_str = bad_committed.rev.to_string();

    let (code_fail, env_fail, _, stderr_fail) = run_json(&[
        "check",
        "-w",
        ws_s,
        "--base-rev",
        &bad_rev_str,
        "--method",
        "both",
    ]);
    assert_eq!(code_fail, exit::NEGATIVE_VERDICT, "stderr: {stderr_fail}");
    assert_eq!(env_fail["ok"], true);
    assert_eq!(env_fail["verdict"], "disagree");
    let mismatches = env_fail["mismatches"].as_array().unwrap();
    assert!(!mismatches.is_empty());
    assert_eq!(mismatches[0]["handle"], "mm1");
    assert!(is_valid_handle(mismatches[0]["handle"].as_str().unwrap()));

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0530 to STA-REQ-0560:
/// `ruststa path`, `ruststa inspect`, `ruststa report`, handle grammar `[A-Za-z0-9_-]+`,
/// and `--sta-store summary` behavior.
#[test]
fn path_inspect_report_and_handles() {
    let dir = temp_dir("queries");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let _ws = setup_counter_ws(&ws_path);

    let (c_run, _, _, _) = run_json(&["run", "-w", ws_s, "--base-rev", "rev1"]);
    assert_eq!(c_run, exit::OK);

    // 1. `ruststa path` by `--target path1`.
    let (c_path, env_path, raw_path, _) =
        run_json(&["path", "-w", ws_s, "--rev", "rev2", "--target", "path1"]);
    assert_eq!(c_path, exit::OK);
    assert_eq!(
        top_level_keys(&raw_path),
        vec![
            "schema",
            "ok",
            "command",
            "workspace",
            "base_rev",
            "rev",
            "new_rev",
            "scope",
            "completeness",
            "metrics",
            "path",
            "paths",
            "diagnostics",
            "error",
            "next_actions",
        ]
    );
    let paths = env_path["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0]["handle"], "path1");
    for pt in paths[0]["points"].as_array().unwrap() {
        let h = pt["handle"].as_str().unwrap();
        assert!(is_valid_handle(h), "invalid point handle: {h}");
    }
    assert_next_actions_run_and_exit_0(&env_path, None);

    // 2. `ruststa inspect` on `inst1`, `net1`, `pin1`, `port1`, and separately `--pin clk_i`.
    let (c_ins, env_ins, raw_ins, _) = run_json(&[
        "inspect", "-w", ws_s, "--rev", "rev2", "--target", "inst1", "--target", "net1",
        "--target", "pin1", "--target", "port1",
    ]);
    assert_eq!(c_ins, exit::OK);
    assert_eq!(
        top_level_keys(&raw_ins),
        vec![
            "schema",
            "ok",
            "command",
            "workspace",
            "base_rev",
            "rev",
            "new_rev",
            "scope",
            "completeness",
            "metrics",
            "object",
            "objects",
            "diagnostics",
            "error",
            "next_actions",
        ]
    );
    let objs = env_ins["objects"].as_array().unwrap();
    assert_eq!(objs.len(), 4);
    for obj in objs {
        let h = obj["handle"].as_str().unwrap();
        assert!(is_valid_handle(h), "invalid object handle: {h}");
    }
    assert_next_actions_run_and_exit_0(&env_ins, None);

    let (c_ins_pin, env_ins_pin, _, _) =
        run_json(&["inspect", "-w", ws_s, "--rev", "rev2", "--pin", "clk_i"]);
    assert_eq!(c_ins_pin, exit::OK);
    assert_eq!(env_ins_pin["object"]["handle"], "port1");

    // 3. Underscore handle `inst_1` is rejected with `UNKNOWN_HANDLE` (exit 1) and suggests `inst1`.
    let (c_uh, env_uh, _, _) =
        run_json(&["inspect", "-w", ws_s, "--rev", "rev2", "--target", "inst_1"]);
    assert_eq!(c_uh, exit::REJECTED);
    assert_eq!(env_uh["ok"], false);
    assert_eq!(env_uh["error"]["code"], ErrorCode::UnknownHandle.as_str());
    assert!(
        env_uh["error"]["message"]
            .as_str()
            .unwrap()
            .contains("did you mean 'inst1'")
    );

    // 4. `ruststa report` across all 8 sections + filtering/sorting/limiting.
    let (c_rep, env_rep, raw_rep, _) = run_json(&[
        "report",
        "-w",
        ws_s,
        "--rev",
        "rev2",
        "--section",
        "summary,endpoints,checks,paths,pins,clocks,unconstrained,notes",
        "--limit",
        "5",
    ]);
    assert_eq!(c_rep, exit::OK);
    assert_eq!(
        top_level_keys(&raw_rep),
        vec![
            "schema",
            "ok",
            "command",
            "workspace",
            "base_rev",
            "rev",
            "new_rev",
            "scope",
            "completeness",
            "metrics",
            "sections",
            "diagnostics",
            "error",
            "next_actions",
        ]
    );
    for sec_name in [
        "summary",
        "endpoints",
        "checks",
        "paths",
        "pins",
        "clocks",
        "unconstrained",
        "notes",
    ] {
        let sec = &env_rep["sections"][sec_name];
        assert!(sec["columns"].is_array(), "missing columns in {sec_name}");
        assert!(sec["total"].is_number(), "missing total in {sec_name}");
        assert!(sec["matched"].is_number(), "missing matched in {sec_name}");
        assert!(sec["shown"].is_number(), "missing shown in {sec_name}");
        assert!(sec["rows"].is_array(), "missing rows in {sec_name}");
    }
    assert_next_actions_run_and_exit_0(&env_rep, None);

    // 5. `--sta-store summary` commits a summary store; `report --section pins` fails with `INPUT_REJECTED` (exit 1).
    let (c_sum, env_sum, _, _) = run_json(&[
        "run",
        "-w",
        ws_s,
        "--base-rev",
        "rev2",
        "--sta-store",
        "summary",
    ]);
    assert_eq!(c_sum, exit::OK);
    let sum_rev = env_sum["new_rev"].as_str().unwrap();
    let (c_pins, env_pins, _, _) =
        run_json(&["report", "-w", ws_s, "--rev", sum_rev, "--section", "pins"]);
    assert_eq!(c_pins, exit::REJECTED);
    assert_eq!(env_pins["error"]["code"], ErrorCode::InputRejected.as_str());

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0500, STA-REQ-0505, STA-REQ-0560 to STA-REQ-0565:
/// All error classes (`USAGE`, `INPUT_REJECTED`, `NOT_A_WORKSPACE`, `UNKNOWN_REVISION`,
/// `MISSING_TABLE`, `UNKNOWN_HANDLE`, `BACKEND_UNAVAILABLE` with `A0501`, `CORRUPT_TABLE`),
/// exit codes (`0`, `1`, `2`, `3`, `4`, `5`), and help/version/completions envelopes.
#[test]
fn error_classes_and_exit_codes() {
    let dir = temp_dir("errors");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let ws = setup_counter_ws(&ws_path);

    // Verify exit code mapping constants (`0`, `1`, `2`, `3`, `4`, `5`).
    assert_eq!(exit::OK, 0);
    assert_eq!(exit::REJECTED, 1);
    assert_eq!(exit::INTERNAL, 2);
    assert_eq!(exit::NEGATIVE_VERDICT, 3);
    assert_eq!(exit::REVISION_CONFLICT, 4);
    assert_eq!(exit::INCONCLUSIVE, 5);
    assert_eq!(ErrorCode::Usage.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::InputRejected.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::NotAWorkspace.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::UnknownRevision.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::MissingTable.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::UnknownHandle.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::BackendUnavailable.exit_code(), exit::REJECTED);
    assert_eq!(ErrorCode::CorruptTable.exit_code(), exit::INTERNAL);
    assert_eq!(ErrorCode::Internal.exit_code(), exit::INTERNAL);
    assert_eq!(
        ErrorCode::RevisionConflict.exit_code(),
        exit::REVISION_CONFLICT
    );

    // 1. `--help --format json`, `--version --format json`, and `completions bash --format json` -> exit 0.
    let (c_help, env_help, _, _) = run_json(&["--help"]);
    assert_eq!(c_help, exit::OK);
    assert_eq!(env_help["ok"], true);
    assert!(env_help["help"].as_str().unwrap().contains("ruststa"));

    let (c_ver, env_ver, _, _) = run_json(&["--version"]);
    assert_eq!(c_ver, exit::OK);
    assert_eq!(env_ver["ok"], true);
    assert!(env_ver["version"].is_string());
    assert!(env_ver["backends"].is_array());

    let (c_comp, env_comp, _, _) = run_json(&["completions", "bash"]);
    assert_eq!(c_comp, exit::OK);
    assert_eq!(env_comp["ok"], true);
    assert_eq!(env_comp["shell"], "bash");

    // 2. `USAGE` (exit 1): unknown flag, negative tolerance, `--threads not_a_number`, bad filter.
    for bad_args in [
        &["--bogus-flag"][..],
        &[
            "run",
            "-w",
            ws_s,
            "--base-rev",
            "rev1",
            "--tolerance",
            "-1.0",
        ][..],
        &[
            "--threads",
            "not_a_number",
            "run",
            "-w",
            ws_s,
            "--base-rev",
            "rev1",
        ][..],
        &[
            "report",
            "-w",
            ws_s,
            "--rev",
            "rev1",
            "--filter",
            "not_a_filter",
        ][..],
    ] {
        let (code, env, _, _) = run_json(bad_args);
        assert_eq!(code, exit::REJECTED, "args: {bad_args:?}");
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], ErrorCode::Usage.as_str());
    }

    // 3. `NOT_A_WORKSPACE` (exit 1).
    let non_ws = dir.join("not_a_ws");
    let (c_naw, env_naw, _, _) = run_json(&["report", "-w", non_ws.to_str().unwrap()]);
    assert_eq!(c_naw, exit::REJECTED);
    assert_eq!(env_naw["error"]["code"], ErrorCode::NotAWorkspace.as_str());

    // 4. `UNKNOWN_REVISION` (exit 1).
    let (c_ur, env_ur, _, _) = run_json(&["report", "-w", ws_s, "--rev", "rev999"]);
    assert_eq!(c_ur, exit::REJECTED);
    assert_eq!(env_ur["error"]["code"], ErrorCode::UnknownRevision.as_str());

    // 5. `MISSING_TABLE` (exit 1): `ruststa report` on `rev1` (no `sta` table yet).
    let (c_mt1, env_mt1, _, _) = run_json(&["report", "-w", ws_s, "--rev", "rev1"]);
    assert_eq!(c_mt1, exit::REJECTED);
    assert_eq!(env_mt1["error"]["code"], ErrorCode::MissingTable.as_str());
    assert_next_actions_run_and_exit_0(&env_mt1, None); // runs `ruststa run -w ws --base-rev rev1` -> commits rev2!

    // 6. `BACKEND_UNAVAILABLE` (exit 1) with diagnostic `A0501`.
    #[cfg(not(feature = "cuda"))]
    let unavailable_backends: &[&str] = &["cuda", "gpu", "wgpu"];
    #[cfg(feature = "cuda")]
    let unavailable_backends: &[&str] = &["wgpu"];
    for &backend in unavailable_backends {
        let (c_bu, env_bu, _, _) = run_json(&[
            "run",
            "-w",
            ws_s,
            "--base-rev",
            "rev2",
            "--backend",
            backend,
        ]);
        assert_eq!(c_bu, exit::REJECTED);
        assert_eq!(
            env_bu["error"]["code"],
            ErrorCode::BackendUnavailable.as_str()
        );
        assert!(
            env_bu["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "A0501"),
            "expected A0501 diagnostic for --backend {backend}"
        );
    }

    // 6b. `UNKNOWN_HANDLE` (exit 1) for a path handle past the ranked paths (STA-REQ-0305): the
    // error and every diagnostic name the requested handle, never its 0-based row.
    let (c_uh, env_uh, _, _) = run_json(&[
        "path",
        "-w",
        ws_s,
        "--rev",
        "rev2",
        "--target",
        "path999999",
    ]);
    assert_eq!(c_uh, exit::REJECTED);
    assert_eq!(env_uh["error"]["code"], ErrorCode::UnknownHandle.as_str());
    let texts: Vec<String> = std::iter::once(env_uh["error"]["message"].to_string())
        .chain(
            env_uh["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["message"].to_string()),
        )
        .collect();
    assert!(texts.iter().all(|t| !t.contains("path999998")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("path999999")), "{texts:?}");

    // 7. `INPUT_REJECTED` (exit 1): import an SDC referencing a nonexistent clock/port on `rev2` (`-> rev3`) and run STA.
    import_revision(
        &ws,
        "rev2",
        "counter.v",
        corpus("counter").unwrap().text,
        "create_clock -name clk -period 2.0 [get_ports no_such_port_xyz]\n",
    );
    let (c_ir, env_ir, _, _) = run_json(&["run", "-w", ws_s, "--base-rev", "rev3"]);
    assert_eq!(c_ir, exit::REJECTED);
    assert_eq!(env_ir["error"]["code"], ErrorCode::InputRejected.as_str());

    // 8. `CORRUPT_TABLE` (exit 2): corrupt a byte in `rev2`'s `sta.rbin` header and run `ruststa report`.
    let rev2 = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
    let sta_rbin_path = rev2.table_path("sta").unwrap();
    let mut bytes = fs::read(&sta_rbin_path).expect("read sta.rbin");
    bytes[0] ^= 0xFF;
    fs::write(&sta_rbin_path, &bytes).expect("write header-corrupted sta.rbin");
    let (c_ct, env_ct, _, _) = run_json(&["report", "-w", ws_s, "--rev", "rev2"]);
    assert_eq!(c_ct, exit::INTERNAL);
    assert_eq!(env_ct["error"]["code"], ErrorCode::CorruptTable.as_str());

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0506:
/// Of two concurrent mutations on the same base revision, exactly one commits and the other
/// exits `4` (`REVISION_CONFLICT`).
#[test]
fn concurrent_mutations_on_one_base_one_commits_one_exits_4() {
    let dir = temp_dir("concurrent");
    let ws_path = dir.join("ws");
    let ws_s = ws_path.to_str().unwrap();
    let _ws = setup_counter_ws(&ws_path);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let b = std::sync::Arc::clone(&barrier);
        let ws_arg = ws_s.to_string();
        handles.push(std::thread::spawn(move || {
            b.wait();
            run_json(&["run", "-w", &ws_arg, "--base-rev", "rev1"])
        }));
    }
    let r1 = handles.remove(0).join().unwrap();
    let r2 = handles.remove(0).join().unwrap();
    let codes = [r1.0, r2.0];
    assert!(
        codes == [0, 4] || codes == [4, 0],
        "expected one exit 0 and one exit 4, got {codes:?}\nr1: {}\nr2: {}",
        r1.1,
        r2.1
    );
    let loser = if r1.0 == exit::REVISION_CONFLICT {
        &r1.1
    } else {
        &r2.1
    };
    assert_eq!(loser["ok"], false);
    assert_eq!(loser["error"]["code"], ErrorCode::RevisionConflict.as_str());
    assert_next_actions_run_and_exit_0(loser, None);

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0507:
/// `sta.rbin` and stdout are byte-for-byte identical for `--threads 1` and `--threads 32`.
#[test]
fn sta_rbin_and_stdout_byte_identical_threads_1_and_32() {
    let dir = temp_dir("determinism");
    let dir1 = dir.join("t1");
    let dir32 = dir.join("t32");
    fs::create_dir_all(&dir1).unwrap();
    fs::create_dir_all(&dir32).unwrap();

    setup_counter_ws(&dir1.join("ws"));
    setup_counter_ws(&dir32.join("ws"));

    let out1 = run_cmd_in(
        &dir1,
        &[
            "--threads",
            "1",
            "--format",
            "json",
            "run",
            "-w",
            "ws",
            "--base-rev",
            "rev1",
        ],
    );
    let out32 = run_cmd_in(
        &dir32,
        &[
            "--threads",
            "32",
            "--format",
            "json",
            "run",
            "-w",
            "ws",
            "--base-rev",
            "rev1",
        ],
    );
    assert_eq!(out1.status.code(), Some(0));
    assert_eq!(out32.status.code(), Some(0));
    assert_eq!(
        out1.stdout, out32.stdout,
        "stdout differed between --threads 1 and --threads 32"
    );

    let ws1 = Workspace::open(dir1.join("ws")).unwrap();
    let ws32 = Workspace::open(dir32.join("ws")).unwrap();
    let rev2_1 = ws1.revision(ws1.resolve_query("rev2").unwrap()).unwrap();
    let rev2_32 = ws32.revision(ws32.resolve_query("rev2").unwrap()).unwrap();
    let rbin1 = fs::read(rev2_1.table_path("sta").unwrap()).unwrap();
    let rbin32 = fs::read(rev2_32.table_path("sta").unwrap()).unwrap();
    assert_eq!(
        rbin1, rbin32,
        "sta.rbin differed between --threads 1 and --threads 32"
    );

    let store = StaStore::open_rev(&rev2_1).unwrap();
    assert!(store.view().unwrap().meta.num_nodes > 0);

    let _ = fs::remove_dir_all(&dir);
}

/// STA-REQ-0500, WS-REQ-0096:
/// Standard input is closed/never read, and the quote round-trip handles a leading `'='`.
#[test]
fn stdin_closed_and_quote_round_trip_leading_equals() {
    // 1. Direct unit check of `shell_quote` / `words` round-trip on leading `'='`.
    assert_eq!(shell_quote("=ws"), "'=ws'");
    let joined = shell_join(["ruststa", "report", "-w", "=ws", "--rev", "rev2"]);
    assert_eq!(joined, "ruststa report -w '=ws' --rev rev2");
    assert_eq!(
        words(&joined),
        vec!["ruststa", "report", "-w", "=ws", "--rev", "rev2"]
    );

    // 2. End-to-end workspace named `=ws` with closed piped stdin and `next_actions` round-trip.
    let dir = temp_dir("leading-eq");
    let eq_ws = dir.join("=ws");
    setup_counter_ws(&eq_ws);

    let mut child = Command::new(bin())
        .current_dir(&dir)
        .args(["--format", "json", "run", "-w", "=ws", "--base-rev", "rev1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn with piped stdin");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait with closed stdin");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_envelope_key_order(&stdout);
    let env: Value = serde_json::from_str(stdout.trim()).unwrap();
    let first_next = env["next_actions"][0].as_str().unwrap();
    assert!(
        first_next.contains("'=ws'"),
        "expected next_action to quote '=ws', got: {first_next}"
    );
    assert_next_actions_run_and_exit_0(&env, Some(&dir));

    let _ = fs::remove_dir_all(&dir);
}
