// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The signoff checks of the command line: the `limits` table and its report sections
//! (`STA-REQ-0806`, `STA-REQ-0807`, `STA-REQ-0811`), the required times of `inspect` and the
//! `--kind` of the endpoints section (`STA-REQ-0812`), extracted parasitics bound to several
//! corners (`STA-REQ-0820`), the wire-model requirement (`STA-REQ-0821`), the hold-fix report
//! (`STA-REQ-0830` to `STA-REQ-0832`), the status rules (`STA-REQ-0813`) and the paths to an
//! endpoint by corner (`STA-REQ-0814`).
//!
//! The library has constant tables, so every expected value follows by hand; the oracles are the
//! hand derivations in the comments and, for the required times, the path reports of the same
//! table (an independent query path: chain headers versus node records).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_formats::spef::{CornerSetMetadata, CornerSource, Limits, Metadata};
use rusteda_workspace::rbin::RbinWriter;
use rusteda_workspace::{Hash64, Invocation, JsonMap, Workspace};
use serde_json::Value;

const LIB: &str = r#"library (sig) {
  time_unit : "1ns"; capacitive_load_unit (1, pf);
  default_max_fanout : 4; default_fanout_load : 1;
  lu_table_template (mpw_t) { variable_1 : related_pin_transition; index_1 ("0.1, 0.5"); }
  cell (BUF) {
    pin (A) { direction : input; capacitance : 0.002; fanout_load : 2; max_transition : 0.5; }
    pin (Y) { direction : output; function : "A"; max_capacitance : 0.009;
      timing () { related_pin : "A"; timing_sense : positive_unate;
        cell_rise (scalar) { values ("0.1"); } cell_fall (scalar) { values ("0.1"); }
        rise_transition (scalar) { values ("0.2"); } fall_transition (scalar) { values ("0.2"); } } }
  }
  cell (INV) {
    pin (A) { direction : input; capacitance : 0.001; }
    pin (Y) { direction : output; function : "!A";
      timing () { related_pin : "A"; timing_sense : negative_unate;
        cell_rise (scalar) { values ("0.05"); } cell_fall (scalar) { values ("0.05"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
  cell (DFF) {
    ff (IQ, IQN) { next_state : "D"; clocked_on : "CK"; }
    pin (CK) { direction : input; clock : true; capacitance : 0.001;
      timing () { related_pin : "CK"; timing_type : min_pulse_width;
        rise_constraint (mpw_t) { index_1 ("0.1, 0.5"); values ("0.3, 0.7"); }
        fall_constraint (scalar) { values ("0.5"); } }
      timing () { related_pin : "CK"; timing_type : minimum_period;
        rise_constraint (scalar) { values ("1.5"); } } }
    pin (D) { direction : input; capacitance : 0.001;
      timing () { related_pin : "CK"; timing_type : setup_rising;
        rise_constraint (scalar) { values ("0.05"); } fall_constraint (scalar) { values ("0.05"); } }
      timing () { related_pin : "CK"; timing_type : hold_rising;
        rise_constraint (scalar) { values ("0.02"); } fall_constraint (scalar) { values ("0.02"); } } }
    pin (Q) { direction : output; function : "IQ";
      timing () { related_pin : "CK"; timing_type : rising_edge;
        cell_rise (scalar) { values ("0.15"); } cell_fall (scalar) { values ("0.15"); }
        rise_transition (scalar) { values ("0.1"); } fall_transition (scalar) { values ("0.1"); } } }
  }
}
"#;

const DRV_NETLIST: &str = "module top (input a, output y, output o2, output o3, output o4);\n\
    wire n1;\n\
    BUF u1 (.A(a), .Y(n1));\nBUF u2 (.A(n1), .Y(o2));\nBUF u3 (.A(n1), .Y(o3));\n\
    BUF u4 (.A(n1), .Y(o4));\nassign y = n1;\nendmodule\n";

const DRV_SDC: &str = "set_input_transition 0.6 [get_ports a]\nset_load 0.004 [get_ports y]\n\
    set_fanout_load 1.5 [get_ports y]\nset_max_fanout 5 [current_design]\n\
    set_max_capacitance 0.001 [get_ports a]\nset_max_transition 0.55 [get_ports a]\n";

const FF_NETLIST: &str = "module top (input clk, input d, output q1, output q2);\n\
    wire nck;\n\
    DFF f1 (.CK(clk), .D(d), .Q(q1));\n\
    INV i1 (.A(clk), .Y(nck));\n\
    DFF f2 (.CK(nck), .D(d), .Q(q2));\nendmodule\n";

// The input arrives early (-min -0.5) for hold and late (1.0) for setup.
const FF_SDC: &str = "create_clock -name c -period 10 -waveform {0 4} [get_ports clk]\n\
    set_clock_latency -min 0.1 [get_clocks c]\nset_clock_latency -max 0.3 [get_clocks c]\n\
    set_clock_transition 0.2 [get_clocks c]\n\
    set_input_delay -clock c -max 1.0 [get_ports d]\nset_input_delay -clock c -min -0.5 [get_ports d]\n\
    set_output_delay -clock c 1 [get_ports {q1 q2}]\n";

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ruststa"))
        .args(["--format", "json"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn ruststa");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let v: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"));
    (out.status.code().unwrap_or(-1), v)
}

/// The keys of the top-level object `field` of the envelope, in output order.
fn envelope_keys(args: &[&str], field: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_ruststa"))
        .args(["--format", "json"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn ruststa");
    let text = String::from_utf8(out.stdout).expect("utf8");
    let start = text
        .find(&format!("  \"{field}\": {{"))
        .unwrap_or_else(|| panic!("{field}: {text}"));
    let end = text[start..].find("\n  }").unwrap() + start;
    text[start..end]
        .lines()
        .filter_map(|l| l.strip_prefix("    \""))
        .filter_map(|l| l.split('"').next())
        .map(str::to_string)
        .collect()
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ruststa-signoff-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn rows<'v>(env: &'v Value, section: &str) -> &'v Vec<Value> {
    env["sections"][section]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("section {section}: {env}"))
}

fn close(a: &Value, b: f64, what: &str) {
    let a = a.as_f64().unwrap_or_else(|| panic!("{what}: {a}"));
    assert!((a - b).abs() < 1e-12, "{what}: {a} vs {b}");
}

// STA-REQ-0806, STA-REQ-0807, STA-REQ-0811: `run` commits the `limits` table with `sta.rbin`,
// reports the counts in its metrics and the revision summary, and `report` lists the rows.
// The values are those of the engine suite (`tests/limits.rs`): violators max_transition
// {a, u1/A}, max_capacitance {a, u1/Y}, max_fanout {u1/Y}.
#[test]
fn run_commits_limits_and_reports_them() {
    let dir = temp_dir("limits");
    let ws = common::netlist_workspace(&dir, LIB, "sig.lib", DRV_NETLIST, DRV_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    // The pin limit metrics follow the timing metrics in this order (STA-REQ-0807).
    let keys = envelope_keys(&["report", "-w", &w, "--section", "summary"], "metrics");
    assert_eq!(
        keys[8..],
        [
            "min_pulse_width_violating",
            "worst_min_pulse_width_slack_ns",
            "min_period_violating",
            "worst_min_period_slack_ns",
            "pulse_width_status",
            "max_transition_violating",
            "worst_max_transition_slack_ns",
            "max_transition_status",
            "max_capacitance_violating",
            "worst_max_capacitance_slack_pf",
            "max_capacitance_status",
            "max_fanout_violating",
            "worst_max_fanout_slack",
            "max_fanout_status",
            "drv_status",
        ]
    );
    let m = &run["metrics"];
    assert_eq!(m["max_transition_violating"], 2);
    assert_eq!(m["max_capacitance_violating"], 2);
    assert_eq!(m["max_fanout_violating"], 1);
    assert_eq!(m["drv_status"], "violated");
    close(
        &m["worst_max_transition_slack_ns"],
        -0.1,
        "worst transition slack",
    );
    close(&m["worst_max_fanout_slack"], -3.5, "worst fanout slack");
    // No pin has a pulse constraint: nothing to check, and no count (STA-REQ-0813).
    assert_eq!(m["min_pulse_width_violating"], Value::Null);
    assert_eq!(m["pulse_width_status"], "unconstrained");
    for k in ["max_transition", "max_capacitance", "max_fanout"] {
        assert_eq!(m[format!("{k}_status")], "violated", "{k}");
    }
    assert_eq!(run["completeness"]["drv_unlimited"]["max_transition"], 8);
    assert_eq!(
        run["completeness"]["unanalyzed_arcs"],
        serde_json::json!({})
    );
    let rev = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
    let meta = rev.meta();
    assert!(meta.tables.contains_key("limits"));
    let sta = &meta.summary["sta"];
    assert_eq!(sta["drv_status"], "violated");
    assert_eq!(sta["max_trans_violating"], 2);
    assert_eq!(sta["max_cap_violating"], 2);
    assert_eq!(sta["max_fanout_violating"], 1);
    assert_eq!(sta["max_fanout_status"], "violated");
    assert_eq!(sta["pulse_width_status"], "unconstrained");
    assert_eq!(sta["min_pulse_width_violating"], Value::Null);
    assert_eq!(meta.status["sta"]["drv"], "violated");
    assert_eq!(meta.summary["limits"]["limit_rows_truncated"], 0);
    // The table depends on the inputs of the timing table.
    assert_eq!(meta.tables["limits"].upstream, meta.tables["sta"].upstream);

    let (code, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "limits,drv,max_fanout",
        "--limit",
        "0",
    ]);
    assert_eq!(code, 0, "{rep}");
    let limits = rows(&rep, "limits");
    assert_eq!(limits.len(), 6);
    let fan = limits
        .iter()
        .find(|r| r["kind"] == "max_fanout")
        .expect("fanout counts");
    assert_eq!(fan["violating"], 1);
    let drv = rows(&rep, "drv");
    let violating: Vec<&Value> = drv
        .iter()
        .filter(|r| r["slack"].as_f64().unwrap() < 0.0)
        .collect();
    assert_eq!(violating.len(), 5);
    // Worst slack first.
    assert!(
        drv.windows(2)
            .all(|w| w[0]["slack"].as_f64() <= w[1]["slack"].as_f64())
    );
    let u1 = rows(&rep, "max_fanout")
        .iter()
        .find(|r| r["pin"] == "u1/Y")
        .expect("u1/Y fanout row");
    close(&u1["value"], 7.5, "fanout of u1/Y");
    // `assign y = n1` makes the port net the representative of the class.
    assert_eq!(u1["net"], "y");
    assert_eq!(u1["source"], "liberty");
    assert_eq!(u1["unit"], "fanout_load");
    let (code, f) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "drv",
        "--filter",
        "violating",
        "--sort",
        "pin",
    ]);
    assert_eq!(code, 0, "{f}");
    let pins: Vec<&str> = rows(&f, "drv")
        .iter()
        .map(|r| r["pin"].as_str().unwrap())
        .collect();
    assert_eq!(pins, ["a", "a", "u1/A", "u1/Y", "u1/Y"]);
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0811: the pulse sections; STA-REQ-0800 values from the engine suite.
#[test]
fn pulse_sections_list_widths_and_periods() {
    let dir = temp_dir("pulse");
    common::netlist_workspace(&dir, LIB, "sig.lib", FF_NETLIST, FF_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["metrics"]["pulse_width_status"], "met");
    close(
        &run["metrics"]["worst_min_pulse_width_slack_ns"],
        3.3,
        "worst pulse slack",
    );
    let (code, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "pulse_width,min_period",
        "--limit",
        "0",
    ]);
    assert_eq!(code, 0, "{rep}");
    let pw = rows(&rep, "pulse_width");
    assert_eq!(pw.len(), 4);
    let f2_low = pw
        .iter()
        .find(|r| r["pin"] == "f2/CK" && r["kind"] == "min_pulse_width_low")
        .expect("f2/CK low");
    close(&f2_low["value"], 3.8, "f2/CK low width");
    close(&f2_low["limit"], 0.5, "f2/CK low required");
    assert_eq!(f2_low["clock"], "c");
    assert_eq!(f2_low["transition"], "fall");
    let per = rows(&rep, "min_period");
    assert_eq!(per.len(), 2);
    close(&per[0]["slack"], 8.5, "period slack");
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0812: `inspect` and the pins section report the setup and the hold required time of
// the worst path of each bound, which `ruststa path` reports from the path chains.
#[test]
fn inspect_required_times_equal_the_worst_paths() {
    let dir = temp_dir("required");
    common::netlist_workspace(&dir, LIB, "sig.lib", FF_NETLIST, FF_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1", "--max-paths", "16"]);
    assert_eq!(code, 0, "{run}");
    for pin in ["f1/D", "f2/D"] {
        let (code, ins) = run_json(&["inspect", "-w", &w, "--pin", pin]);
        assert_eq!(code, 0, "{ins}");
        let obj = &ins["objects"][0];
        let req = |kind: &str| {
            let (code, p) = run_json(&["path", "-w", &w, "--to", pin, "--kind", kind]);
            assert_eq!(code, 0, "{p}");
            p["paths"][0]["required_ns"].as_f64().expect("required")
        };
        let (setup, hold) = (req("setup"), req("hold"));
        close(
            &obj["required_setup_ns"],
            setup,
            &format!("{pin} setup required"),
        );
        close(
            &obj["required_ns"],
            setup,
            &format!("{pin} required_ns is the setup time"),
        );
        close(
            &obj["required_hold_ns"],
            hold,
            &format!("{pin} hold required"),
        );
        // Setup: the next capture edge less the setup time; hold: the same edge plus the hold
        // time. The hold required time is far below the setup required time.
        assert!(hold < setup, "{pin}: hold {hold} setup {setup}");
        let (code, pins) = run_json(&[
            "report",
            "-w",
            &w,
            "--section",
            "pins",
            "--filter",
            &format!("pin~{pin}"),
        ]);
        assert_eq!(code, 0, "{pins}");
        let r = &rows(&pins, "pins")[0];
        close(
            &r["required_setup_ns"],
            setup,
            &format!("{pin} pins setup required"),
        );
        close(
            &r["required_hold_ns"],
            hold,
            &format!("{pin} pins hold required"),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0812: `--kind` selects the slack of the endpoints section for its filters.
// f1/D fails hold (early arrival -0.5 against the rising edge at 0) and meets setup; f2/D,
// captured at the falling source edge, meets both.
#[test]
fn endpoint_section_honours_kind() {
    let dir = temp_dir("kind");
    common::netlist_workspace(&dir, LIB, "sig.lib", FF_NETLIST, FF_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let names = |kind: &str| -> Vec<String> {
        let (code, rep) = run_json(&[
            "report",
            "-w",
            &w,
            "--section",
            "endpoints",
            "--kind",
            kind,
            "--filter",
            "violating",
            "--sort",
            "slack",
        ]);
        assert_eq!(code, 0, "{rep}");
        let mut v: Vec<String> = rows(&rep, "endpoints")
            .iter()
            .map(|r| r["endpoint"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    assert!(names("setup").is_empty());
    assert_eq!(names("hold"), ["f1/D"]);
    assert_eq!(names("both"), ["f1/D"]);
    // Without a filter, --kind setup keeps every endpoint with a setup slack.
    let (code, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "endpoints",
        "--kind",
        "setup",
    ]);
    assert_eq!(code, 0, "{rep}");
    assert!(
        rows(&rep, "endpoints")
            .iter()
            .all(|r| r["setup_slack_ns"].is_number())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0830 to STA-REQ-0832: the hold-fix report lists each hold violator with the delay it
// needs, its driving net and the stages of its worst hold path with their nets.
// f1/D: early arrival 0.1 - 0.5 = -0.4 (the input delay after the earliest launch latency,
// STA-REQ-0218) against the latest capture clock 0.3 plus the hold time 0.02: slack -0.72; with
// --target-margin 0.1 the needed delay is 0.82.
#[test]
fn hold_fix_report_names_nets_and_needed_delay() {
    let dir = temp_dir("holdfix");
    common::netlist_workspace(&dir, LIB, "sig.lib", FF_NETLIST, FF_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let (code, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "hold_fix",
        "--target-margin",
        "0.1",
    ]);
    assert_eq!(code, 0, "{rep}");
    let hf = &rep["sections"]["hold_fix"];
    close(&hf["target_margin_ns"], 0.1, "margin");
    let c = &hf["corners"][0];
    assert_eq!(c["corner"], "default");
    assert_eq!(c["hold_violating"], 1);
    let eps = c["endpoints"].as_array().expect("endpoints");
    assert_eq!(eps.len(), 1);
    for e in eps {
        let slack = e["hold_slack_ns"].as_f64().unwrap();
        close(&e["required_delay_ns"], 0.1 - slack, "needed delay");
        assert_eq!(e["net"], "d");
        assert_eq!(e["net_driver"], "d");
        assert_eq!(e["startpoint"], "d");
        assert!(e["setup_slack_ns"].as_f64().unwrap() > 0.0);
        let stages = e["path"]["stages"].as_array().expect("stages");
        assert_eq!(stages.first().unwrap()["pin"], "d");
        assert_eq!(stages.last().unwrap()["pin"], e["endpoint"]);
        assert!(stages.iter().all(|s| s["net"] == "d"));
        close(&e["path"]["slack_ns"], slack, "path slack");
    }
    let f1 = eps.iter().find(|e| e["endpoint"] == "f1/D").expect("f1/D");
    close(&f1["hold_slack_ns"], -0.72, "f1/D hold slack");
    close(&f1["required_delay_ns"], 0.82, "f1/D needed delay");
    let merged = hf["merged"].as_array().expect("merged");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0]["worst_corner"], "default");
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0821: --require-wire-model rejects an analysis with another wire model, before it
// commits anything; the satisfied requirement commits normally.
#[test]
fn wire_model_requirement_rejects_other_models() {
    let dir = temp_dir("wiremodel");
    let ws = common::netlist_workspace(&dir, LIB, "sig.lib", DRV_NETLIST, DRV_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    for want in ["spef_rc", "wireload_lumped"] {
        let (code, r) = run_json(&[
            "run",
            "-w",
            &w,
            "--base-rev",
            "rev1",
            "--require-wire-model",
            want,
        ]);
        assert_eq!(code, 1, "{want}: {r}");
        assert_eq!(r["error"]["code"], "INPUT_REJECTED");
        assert!(
            r["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "T0135"),
            "{r}"
        );
        assert_eq!(ws.head().unwrap().to_string(), "rev1");
    }
    let (code, r) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev1",
        "--require-wire-model",
        "ideal",
    ]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["scope"]["wire_model"], "ideal");
    let _ = std::fs::remove_dir_all(&dir);
}

const WT_NETLIST: &str = "module top (input a, output y);\nwt_buf_1 u (.A(a), .Y(y));\nendmodule\n";

fn wt_spef(cap: f64) -> String {
    format!(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n\
         *D_NET a 0.003\n*CONN\n*P a I\n*I u:A I\n*CAP\n1 a 0.001\n2 u:A 0.002\n*RES\n1 a u:A 2\n*END\n\
         *D_NET y {cap}\n*CONN\n*P y O\n*I u:Y O\n*CAP\n1 u:Y {cap}\n*RES\n1 u:Y y 3\n*END\n"
    )
}

/// Commits a `parasitics` table of `corners` as `rusteda import --spef` writes it: version 2
/// with every corner, or version 1 with the one corner (RCLI-REQ-0070, RCLI-REQ-0075).
fn commit_parasitics(ws: &Workspace, corners: &[(&str, f64)], version: u32) {
    let inv = Invocation::new(
        "rusteda",
        "0.4.0",
        "rusteda import",
        vec!["rusteda".to_string()],
    );
    let head = ws.head().unwrap().to_string();
    let mut txn = ws.begin(ws.resolve_base(&head).unwrap(), &inv).unwrap();
    let texts: Vec<String> = corners.iter().map(|(_, c)| wt_spef(*c)).collect();
    let mut w = RbinWriter::new("PARASIT", version).unwrap();
    if version == 1 {
        let meta = serde_json::to_vec(&Metadata {
            corner: corners[0].0.into(),
            limits: Limits::default(),
        })
        .unwrap();
        w.section_bytes(1, 1, texts[0].as_bytes()).unwrap();
        w.section_bytes(2, 1, meta).unwrap();
    } else {
        let set = CornerSetMetadata {
            corners: corners
                .iter()
                .zip(&texts)
                .map(|((c, _), t)| CornerSource {
                    corner: (*c).into(),
                    limits: Limits::default(),
                    source_path: format!("{c}.spef"),
                    source_hash: Hash64::of(t.as_bytes()).to_string(),
                })
                .collect(),
        };
        w.section_bytes(1, 1, serde_json::to_vec(&set).unwrap())
            .unwrap();
        for (i, t) in texts.iter().enumerate() {
            w.section_bytes(i as u32 + 2, 1, t.as_bytes()).unwrap();
        }
    }
    let up = txn.upstream(&["netlist"]).unwrap();
    txn.add_rbin("parasitics", "parasitics.rbin", &w, up)
        .unwrap();
    txn.commit(JsonMap::new()).unwrap();
}

// STA-REQ-0820: a revision binds one SPEF per corner; each corner is analyzed with its own, a
// corner without one is an explicit error, and a version 1 table names its one corner.
#[test]
fn every_corner_is_analyzed_with_its_own_spef() {
    let dir = temp_dir("corners");
    let wt = ruststa_fixtures::liberty::library(ruststa_fixtures::liberty::Variant::Wt15);
    let ws = common::corners_workspace(
        &dir,
        &[("fast", &wt, "wt_fast.lib"), ("slow", &wt, "wt_slow.lib")],
        WT_NETLIST,
        "set_input_transition 0.1 [get_ports a]\nset_max_capacitance 1 [current_design]\n",
    );
    let w = dir.join("ws").to_string_lossy().into_owned();
    commit_parasitics(&ws, &[("fast", 0.001), ("slow", 0.008)], 2);
    let (code, run) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev2",
        "--require-wire-model",
        "spef_rc",
    ]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["scope"]["wire_model"], "spef_rc");
    let (code, rep) = run_json(&["report", "-w", &w, "--section", "max_capacitance"]);
    assert_eq!(code, 0, "{rep}");
    let cap = |corner: &str| {
        rows(&rep, "max_capacitance")
            .iter()
            .find(|r| r["corner"] == corner && r["pin"] == "u/Y")
            .unwrap()["value"]
            .as_f64()
            .unwrap()
    };
    let d = cap("slow") - cap("fast");
    assert!(
        (d - 0.007).abs() < 1e-12,
        "the corners differ by their SPEF: {d}"
    );

    // Only one corner annotated: analyzing both fails (T0070), the annotated one alone passes.
    commit_parasitics(&ws, &[("slow", 0.008)], 2);
    let (code, r) = run_json(&["run", "-w", &w, "--base-rev", "rev4"]);
    assert_eq!(code, 1, "{r}");
    assert!(
        r["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "T0070" && d["message"].as_str().unwrap().contains("fast")),
        "{r}"
    );
    let (code, r) = run_json(&["run", "-w", &w, "--base-rev", "rev4", "--corner", "slow"]);
    assert_eq!(code, 0, "{r}");

    // A version 1 table annotates its one corner.
    commit_parasitics(&ws, &[("fast", 0.001)], 1);
    let head = ws.head().unwrap().to_string();
    let (code, r) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        &head,
        "--corner",
        "fast",
        "--require-wire-model",
        "spef_rc",
    ]);
    assert_eq!(code, 0, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0802, STA-REQ-0813: a design whose pulse pins no clock reaches reports its pulse checks
// as unchecked with null counts, in the envelope and in the revision summary; a design rule
// without any limit is unconstrained with a null count, and the design rules together partial.
#[test]
fn unchecked_and_unconstrained_checks_are_never_met() {
    let dir = temp_dir("status");
    // No default_max_fanout in the library and no set_max_fanout: max_fanout has no limit.
    let lib = LIB.replace("default_max_fanout : 4; ", "");
    let ws = common::netlist_workspace(
        &dir,
        &lib,
        "sig.lib",
        FF_NETLIST,
        "set_input_delay 1 [get_ports d]\nset_max_transition 2 [current_design]\n\
         set_max_capacitance 1 [current_design]\n",
    );
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let m = &run["metrics"];
    assert_eq!(m["pulse_width_status"], "unchecked");
    assert_eq!(m["min_pulse_width_violating"], Value::Null);
    assert_eq!(m["min_period_violating"], Value::Null);
    assert_eq!(m["worst_min_pulse_width_slack_ns"], Value::Null);
    assert_eq!(run["completeness"]["unchecked_pulse_width_pins"], 2);
    assert_eq!(run["completeness"]["unevaluated_limit_checks"], 0);
    assert_eq!(m["max_fanout_status"], "unconstrained");
    assert_eq!(m["max_fanout_violating"], Value::Null);
    assert_eq!(m["max_transition_status"], "met");
    assert_eq!(m["max_transition_violating"], 0);
    assert_eq!(m["drv_status"], "partial");
    let diags: Vec<&str> = run["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert!(diags.contains(&"T0133"), "{diags:?}");
    let rev = ws.revision(ws.resolve_query("rev2").unwrap()).unwrap();
    let meta = rev.meta();
    let sta = &meta.summary["sta"];
    assert_eq!(sta["pulse_width_status"], "unchecked");
    assert_eq!(sta["min_pulse_width_violating"], Value::Null);
    assert_eq!(sta["max_fanout_violating"], Value::Null);
    assert_eq!(sta["drv_status"], "partial");
    assert_eq!(meta.status["sta"]["pulse_width"], "unchecked");
    assert_eq!(meta.status["sta"]["drv"], "partial");
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0811: with a row budget, a row section's total is the checked count, and it names the
// kept and the truncated rows. DRV_SDC checks 9 transitions (the engine suite: 8 unlimited of 17
// pins with a transition time); a budget of 2 keeps 2 of them.
#[test]
fn row_sections_count_checked_and_truncated_rows() {
    let dir = temp_dir("budget");
    common::netlist_workspace(&dir, LIB, "sig.lib", DRV_NETLIST, DRV_SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev1",
        "--max-limit-rows",
        "2",
    ]);
    assert_eq!(code, 0, "{run}");
    let (code, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "limits,max_transition",
        "--limit",
        "0",
    ]);
    assert_eq!(code, 0, "{rep}");
    let checked = rows(&rep, "limits")
        .iter()
        .find(|r| r["kind"] == "max_transition")
        .unwrap()["checked"]
        .as_u64()
        .unwrap();
    assert!(checked > 2, "{checked}");
    let sec = &rep["sections"]["max_transition"];
    assert_eq!(sec["total"].as_u64(), Some(checked), "{sec}");
    assert_eq!(sec["stored"], 2);
    assert_eq!(sec["truncated"].as_u64(), Some(checked - 2));
    assert_eq!(sec["unevaluated"], 0);
    assert_eq!(sec["shown"], 2);
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0814: `path --to` gives the worst path of the endpoint in `--corner`, or over all
// corners without it, for each requested family, and warns T0138 for a family without a path.
// Corner a: setup 0.05, hold 0.02; corner b: setup 0.5, hold 0.01 (from the constraint tables),
// so the worst setup corner of f1/D is b and its worst hold corner is a.
#[test]
fn path_to_an_endpoint_honours_the_corner() {
    let dir = temp_dir("pathcorner");
    let lib_b = LIB
        .replace(
            "timing_type : setup_rising;\n        rise_constraint (scalar) { values (\"0.05\"); } fall_constraint (scalar) { values (\"0.05\"); }",
            "timing_type : setup_rising;\n        rise_constraint (scalar) { values (\"0.5\"); } fall_constraint (scalar) { values (\"0.5\"); }",
        )
        .replace(
            "timing_type : hold_rising;\n        rise_constraint (scalar) { values (\"0.02\"); } fall_constraint (scalar) { values (\"0.02\"); }",
            "timing_type : hold_rising;\n        rise_constraint (scalar) { values (\"0.01\"); } fall_constraint (scalar) { values (\"0.01\"); }",
        );
    assert_ne!(lib_b, LIB, "the corner b library differs");
    let sdc = FF_SDC.replace(
        "set_output_delay -clock c 1 [get_ports {q1 q2}]",
        "set_output_delay -clock c 1 [get_ports q2]\nset_output_delay -clock c -max 1 [get_ports q1]",
    );
    common::corners_workspace(
        &dir,
        &[("a", LIB, "sig_a.lib"), ("b", &lib_b, "sig_b.lib")],
        FF_NETLIST,
        &sdc,
    );
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let slack_of = |corner: &str, field: &str| -> f64 {
        let (code, rep) = run_json(&[
            "report",
            "-w",
            &w,
            "--section",
            "endpoints",
            "--corner",
            corner,
            "--limit",
            "0",
        ]);
        assert_eq!(code, 0, "{rep}");
        rows(&rep, "endpoints")
            .iter()
            .find(|r| r["endpoint"] == "f1/D")
            .unwrap()[field]
            .as_f64()
            .unwrap()
    };
    let path = |extra: &[&str]| -> Value {
        let mut args = vec!["path", "-w", w.as_str(), "--to", "f1/D"];
        args.extend_from_slice(extra);
        let (code, p) = run_json(&args);
        assert_eq!(code, 0, "{p}");
        p
    };
    let one = |p: &Value| -> (String, String, f64) {
        let paths = p["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1, "{p}");
        (
            paths[0]["corner"].as_str().unwrap().to_string(),
            paths[0]["kind"].as_str().unwrap().to_string(),
            paths[0]["slack_ns"].as_f64().unwrap(),
        )
    };
    for corner in ["a", "b"] {
        for (kind, field) in [("hold", "hold_slack_ns"), ("setup", "setup_slack_ns")] {
            let (c, k, slack) = one(&path(&["--kind", kind, "--corner", corner]));
            assert_eq!((c.as_str(), k.as_str()), (corner, kind));
            let want = slack_of(corner, field);
            assert!(
                (slack - want).abs() < 1e-9,
                "{corner} {kind}: {slack} vs {want}"
            );
        }
    }
    // Without --corner: the worst corner of each family.
    let hold_a = slack_of("a", "hold_slack_ns");
    let hold_b = slack_of("b", "hold_slack_ns");
    let setup_a = slack_of("a", "setup_slack_ns");
    let setup_b = slack_of("b", "setup_slack_ns");
    assert!(
        hold_a < hold_b && setup_b < setup_a,
        "the worst corners differ"
    );
    assert_eq!(one(&path(&["--kind", "hold"])).0, "a");
    assert_eq!(one(&path(&["--kind", "setup"])).0, "b");
    let both = path(&[]);
    assert_eq!(both["paths"].as_array().unwrap().len(), 2, "{both}");
    // q1 has only a setup check: asking for its hold path warns T0138 and gives no path.
    let (code, p) = run_json(&[
        "path", "-w", &w, "--to", "q1", "--kind", "hold", "--corner", "a",
    ]);
    assert_eq!(code, 0, "{p}");
    assert!(p["paths"].as_array().unwrap().is_empty(), "{p}");
    assert!(
        p["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "T0138"),
        "{p}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0808, STA-REQ-0809, STA-REQ-0818, STA-REQ-0819: the envelope states the clock limit scope of the
// design rules (`scope.drv_rules.clock_limits`) and counts the pins left out or checked by a
// special rule. FF_NETLIST plus a buffer of the literal 1'b1 driving the port q3: u/Y is the
// driver of a constant net (no capacitance or fanout check), and u/A, u/Y and q3 are checked with
// the static transition 0 (a literal cone). Propagated, nothing is in an ideal clock network; the
// -clock_path limit 0.15 applies to the clock pins with the transition 0.2 (clk, f1/CK, i1/A)
// under `paths`, and nowhere under `domain` (warning T0144). Ideal, the five clock network pins
// are left out (T0146) and their limited checks are unevaluated.
#[test]
fn envelope_counts_constant_and_ideal_clock_pins() {
    let dir = temp_dir("drvrules");
    let netlist = FF_NETLIST
        .replace("output q2);", "output q2, output q3);")
        .replace("endmodule", "BUF u (.A(1'b1), .Y(q3));\nendmodule");
    let sdc = format!(
        "{FF_SDC}set_propagated_clock [get_clocks c]\nset_input_transition 0.2 [get_ports clk]\n\
         set_max_transition 0.15 -clock_path [get_clocks c]\n"
    );
    let _ws = common::netlist_workspace(&dir, LIB, "sig.lib", &netlist, &sdc);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    assert_eq!(run["scope"]["drv_rules"]["clock_limits"], "paths");
    let c = &run["completeness"];
    assert_eq!(c["drv_constant_drivers"], 1, "{c}");
    assert_eq!(c["drv_static_transition_pins"], 3, "{c}");
    assert_eq!(c["drv_ideal_clock_pins"], 0, "{c}");
    assert_eq!(c["drv_untimed_pins"], 0, "{c}");
    // The five clock network pins have a -clock_path limit but no -data_path limit.
    assert_eq!(c["drv_clock_scope_pins"], 5, "{c}");
    assert_eq!(c["drv_unconnected_output_pins"], 0, "{c}");
    let codes = |env: &Value| -> Vec<String> {
        env["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(codes(&run).contains(&"T0143".to_owned()));
    assert!(codes(&run).contains(&"T0147".to_owned()));
    let (_, rep) = run_json(&[
        "report",
        "-w",
        &w,
        "--section",
        "max_transition",
        "--filter",
        "violating",
        "--limit",
        "0",
    ]);
    let mut viol: Vec<&str> = rows(&rep, "max_transition")
        .iter()
        .map(|r| r["pin"].as_str().unwrap())
        .collect();
    viol.sort_unstable();
    assert_eq!(viol, ["clk", "f1/CK", "i1/A"]);

    let (code, dom) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev2",
        "--clock-limits",
        "domain",
    ]);
    assert_eq!(code, 0, "{dom}");
    assert_eq!(dom["scope"]["drv_rules"]["clock_limits"], "domain");
    assert!(codes(&dom).contains(&"T0144".to_owned()), "{dom}");
    assert_eq!(dom["completeness"]["drv_clock_scope_pins"], 5, "{dom}");
    assert_eq!(dom["metrics"]["max_transition_violating"], 0);

    // The same constraints on an ideal clock.
    let dir2 = temp_dir("drvideal");
    let ideal = sdc.replace("set_propagated_clock [get_clocks c]\n", "");
    let _ws2 = common::netlist_workspace(&dir2, LIB, "sig.lib", &netlist, &ideal);
    let w2 = dir2.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &w2, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let c = &run["completeness"];
    assert_eq!(c["drv_ideal_clock_pins"], 5, "{c}");
    assert!(c["unevaluated_limit_checks"].as_u64().unwrap() >= 5, "{c}");
    assert!(codes(&run).contains(&"T0146".to_owned()));
    assert_ne!(run["metrics"]["drv_status"], "met");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
