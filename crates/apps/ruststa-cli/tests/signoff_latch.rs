// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Signoff checks of the command line on a design with a transparent latch, a clock gater and
//! extracted parasitics: the required times that `inspect` reports at a latch data endpoint
//! (`STA-REQ-0812`), the hold order of the endpoints section (`STA-REQ-0812`), byte-identical
//! tables of `update` and `run` on a revision with extracted parasitics (`STA-REQ-0806`), and the
//! pulse rules of the command line and their scope (`STA-REQ-0816`, `STA-REQ-0817`).
//!
//! The library is the closed-form `Wt15` fixture (load-dependent tables, as extracted timing
//! needs). The oracles are the path reports of the same table (an independent query: chain headers
//! versus node records), the table bytes of a full `run`, and hand-derived counts.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_formats::spef::{CornerSetMetadata, CornerSource, Limits};
use rusteda_workspace::rbin::RbinWriter;
use rusteda_workspace::{Hash64, Invocation, JsonMap};
use ruststa_fixtures::liberty::{Variant, library};
use serde_json::Value;

/// A latch `l1` (enable `clk`) feeding the port `qx`, a flip-flop `f1` on the inverted clock
/// through a buffer and a flip-flop `f2` behind a clock gater `g1`.
const NETLIST: &str = "module top (input clk, input d, input en, output q1, output qx, output q2);\n\
    wire ckn, l1q, b1y, gck;\n\
    assign qx = l1q;\n\
    wt_inv_1 i1 (.A(clk), .Y(ckn));\n\
    wt_dlatch_p l1 (.G(clk), .D(d), .Q(l1q));\n\
    wt_buf_1 b1 (.A(l1q), .Y(b1y));\n\
    wt_dff_p f1 (.CLK(ckn), .D(b1y), .Q(q1));\n\
    wt_icg_p g1 (.CLK(clk), .EN(en), .GCLK(gck));\n\
    wt_dff_p f2 (.CLK(gck), .D(l1q), .Q(q2));\nendmodule\n";

/// The data at l1/D arrives while the latch is transparent (it borrows: slack 0); the port `qx`
/// must be stable 3.9 ns before the next rising edge, which the path through the latch misses.
const SDC: &str = "create_clock -name c -period 4 -waveform {0 2} [get_ports clk]\n\
    set_clock_transition 0.15 [get_clocks c]\n\
    set_input_delay -clock c 0.3 [get_ports {d en}]\n\
    set_output_delay -clock c 0.2 [get_ports {q1 q2}]\n\
    set_output_delay -clock c 3.9 [get_ports qx]\n\
    set_input_transition 0.1 [get_ports {clk d en}]\nset_load 0.002 [get_ports {q1 q2}]\n";

/// One resistive net per connection: `(net, driver, loads)` with the driver as `*P`/`*I`.
const NETS: &[(&str, &str, &[&str])] = &[
    ("clk", "clk", &["i1:A", "l1:G", "g1:CLK"]),
    ("d", "d", &["l1:D"]),
    ("en", "en", &["g1:EN"]),
    ("ckn", "i1:Y", &["f1:CLK"]),
    ("l1q", "l1:Q", &["b1:A", "qx", "f2:D"]),
    ("b1y", "b1:Y", &["f1:D"]),
    ("gck", "g1:GCLK", &["f2:CLK"]),
    ("q1", "f1:Q", &["q1"]),
    ("q2", "f2:Q", &["q2"]),
];

fn spef() -> String {
    let mut s = String::from(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n",
    );
    for (k, (net, drv, loads)) in NETS.iter().enumerate() {
        let node = |p: &str| -> (String, bool) { (p.to_string(), p.contains(':')) };
        let c = 0.001 + 0.0005 * k as f64;
        let total = c * (1 + loads.len()) as f64;
        s += &format!("*D_NET {net} {total}\n*CONN\n");
        let (d, inst) = node(drv);
        s += &if inst {
            format!("*I {d} O\n")
        } else {
            format!("*P {d} I\n")
        };
        for l in *loads {
            let (l, inst) = node(l);
            s += &if inst {
                format!("*I {l} I\n")
            } else {
                format!("*P {l} O\n")
            };
        }
        s += "*CAP\n";
        s += &format!("1 {drv} {c}\n");
        for (i, l) in loads.iter().enumerate() {
            s += &format!("{} {l} {c}\n", i + 2);
        }
        s += "*RES\n";
        for (i, l) in loads.iter().enumerate() {
            s += &format!("{} {drv} {l} {}\n", i + 1, 0.2 + 0.1 * i as f64);
        }
        s += "*END\n";
    }
    s
}

/// `lib` with `text` added at the end of the `clock : true` pin of cell `cell`.
fn add_to_clock_pin(lib: &str, cell: &str, text: &str) -> String {
    let start = lib.find(&format!("cell ({cell})")).expect(cell);
    let pin = start + lib[start..].find("clock : true;").expect("clock pin");
    let close = pin + lib[pin..].find('}').expect("pin end");
    format!("{} {text} {}", &lib[..close], &lib[close..])
}

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

fn temp_dir(tag: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("ruststa-latch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn rows<'v>(env: &'v Value, section: &str) -> &'v Vec<Value> {
    env["sections"][section]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("section {section}: {env}"))
}

/// The workspace `dir/ws` at `rev1` (library, netlist and constraints), with `rev2` adding the
/// extracted parasitics of corner `default` as `rusteda import --spef` commits them.
fn workspace(dir: &Path, with_spef: bool) -> String {
    let lib = library(Variant::Wt15);
    let ws = common::netlist_workspace(dir, &lib, "wt15.lib", NETLIST, SDC);
    if with_spef {
        let inv = Invocation::new(
            "rusteda",
            "0.4.0",
            "rusteda import",
            vec!["rusteda".to_string()],
        );
        let mut txn = ws.begin(ws.resolve_base("rev1").unwrap(), &inv).unwrap();
        let text = spef();
        let set = CornerSetMetadata {
            corners: vec![CornerSource {
                corner: "default".into(),
                limits: Limits::default(),
                source_path: "top.spef".into(),
                source_hash: Hash64::of(text.as_bytes()).to_string(),
            }],
        };
        let mut w = RbinWriter::new("PARASIT", 2).unwrap();
        w.section_bytes(1, 1, serde_json::to_vec(&set).unwrap())
            .unwrap();
        w.section_bytes(2, 1, text.as_bytes()).unwrap();
        let up = txn.upstream(&["netlist"]).unwrap();
        txn.add_rbin("parasitics", "parasitics.rbin", &w, up)
            .unwrap();
        txn.commit(JsonMap::new()).unwrap();
    }
    dir.join("ws").to_string_lossy().into_owned()
}

// STA-REQ-0706/0732: a topology edit needs replacement extracted data. A rejected
// candidate must leave the disk workspace intact and must not poison a resize
// evaluated after it. <50 pins, 9 tiny RC nets, one corner, three session copies:
// under 1 MiB of graph/RC state; this test has no enumerated paths.
#[test]
fn whatif_rejects_stale_spef_and_keeps_following_candidates_independent() {
    use rusteda_workspace::edits::{Candidate, Edit, EditFile, Sink};
    let dir = temp_dir("whatif-spef");
    let w = workspace(&dir, true);
    let root = Path::new(&w);
    let manifest = std::fs::read(root.join("manifest.json")).unwrap();
    let history = std::fs::read(root.join("history.jsonl")).unwrap();
    let parasitics = root.join("revisions/rev0002/parasitics.rbin");
    let original = std::fs::read(&parasitics).unwrap();
    let file = EditFile {
        candidates: vec![
            Candidate {
                id: "needs_new_extraction".into(),
                edits: vec![Edit::InsertBuffer {
                    net: "l1q".into(),
                    cell: "wt_buf_1".into(),
                    instance: "new_buffer".into(),
                    net_out: "new_net".into(),
                    sinks: vec![Sink::Pin {
                        instance: "b1".into(),
                        pin: "A".into(),
                    }],
                }],
            },
            Candidate {
                id: "same_topology".into(),
                edits: vec![Edit::Resize {
                    instance: "b1".into(),
                    cell: "wt_buf_2".into(),
                }],
            },
        ],
        ..EditFile::default()
    };
    let path = dir.join("edits.json");
    std::fs::write(&path, file.to_json()).unwrap();
    let (code, result) = run_json(&[
        "whatif",
        "-w",
        &w,
        "--rev",
        "rev2",
        "--edits",
        path.to_str().unwrap(),
        "--require-wire-model",
        "spef_rc",
        "--max-state-gib",
        "1",
    ]);
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["scope"]["wire_model"], "spef_rc", "{result}");
    assert_eq!(result["candidates"][0]["status"], "rejected", "{result}");
    assert_eq!(
        result["candidates"][0]["error"]["code"], "T0070",
        "{result}"
    );
    assert_eq!(result["candidates"][0]["values"], Value::Null);
    assert_eq!(result["candidates"][1]["status"], "evaluated", "{result}");
    assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), manifest);
    assert_eq!(std::fs::read(root.join("history.jsonl")).unwrap(), history);
    assert_eq!(std::fs::read(parasitics).unwrap(), original);
    assert!(!root.join("revisions/rev0003").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0812: `inspect --pin` of a latch data endpoint reports the required times of its worst
// setup and hold path, equal to `path --to`, with its endpoint slacks; the `pins` section reports
// the node values, which at the latch data pin also cover the path through the transparent latch
// to `qx` and are earlier than the latch's own setup check (it borrows, so its required time is
// the arrival).
#[test]
fn inspect_reports_the_check_required_times_of_a_latch_endpoint() {
    let dir = temp_dir("inspect");
    let w = workspace(&dir, false);
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1", "--max-paths", "16"]);
    assert_eq!(code, 0, "{run}");
    let mut node_differs = false;
    for pin in ["l1/D", "f1/D", "f2/D"] {
        let (code, ins) = run_json(&["inspect", "-w", &w, "--pin", pin]);
        assert_eq!(code, 0, "{ins}");
        let obj = &ins["objects"][0];
        for (kind, field, slack) in [
            ("setup", "required_setup_ns", "slack_ns"),
            ("hold", "required_hold_ns", "hold_slack_ns"),
        ] {
            let (code, p) = run_json(&["path", "-w", &w, "--to", pin, "--kind", kind]);
            assert_eq!(code, 0, "{p}");
            let path = &p["paths"][0];
            let req = path["required_ns"].as_f64().expect("required");
            let got = obj[field].as_f64().expect(field);
            assert!((got - req).abs() < 1e-12, "{pin} {kind}: {got} vs {req}");
            let s = path["slack_ns"].as_f64().expect("slack");
            let es = obj[slack].as_f64().expect(slack);
            assert!((s - es).abs() < 1e-12, "{pin} {kind} slack: {es} vs {s}");
        }
        if pin == "l1/D" {
            let (code, pins) = run_json(&[
                "report",
                "-w",
                &w,
                "--section",
                "pins",
                "--filter",
                "pin~l1/D",
            ]);
            assert_eq!(code, 0, "{pins}");
            let node = rows(&pins, "pins")[0]["required_setup_ns"]
                .as_f64()
                .expect("node required");
            let check = obj["required_setup_ns"].as_f64().unwrap();
            node_differs = node < check - 1e-9;
        }
    }
    assert!(
        node_differs,
        "the node required time of l1/D also covers the path through the latch"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0812: without `--sort`, `--kind hold` orders the endpoints section by hold slack, so
// `--limit 1` gives the worst hold endpoint (the table order is by setup slack).
#[test]
fn kind_hold_lists_the_worst_hold_endpoints_first() {
    let dir = temp_dir("holdorder");
    let w = workspace(&dir, false);
    let (code, run) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{run}");
    let section = |extra: &[&str]| -> Vec<(String, f64)> {
        let mut args = vec!["report", "-w", &w, "--section", "endpoints"];
        args.extend_from_slice(extra);
        let (code, rep) = run_json(&args);
        assert_eq!(code, 0, "{rep}");
        rows(&rep, "endpoints")
            .iter()
            .filter(|r| r["hold_slack_ns"].is_number())
            .map(|r| {
                (
                    r["endpoint"].as_str().unwrap().to_string(),
                    r["hold_slack_ns"].as_f64().unwrap(),
                )
            })
            .collect()
    };
    let all = section(&["--kind", "both"]);
    let mut want = all.clone();
    want.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    assert_ne!(all[0].0, want[0].0, "the table order is not the hold order");
    assert_eq!(section(&["--kind", "hold"]), want);
    assert_eq!(section(&["--kind", "hold", "--limit", "1"]), want[..1]);
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0806: on a revision with extracted parasitics, `update` (a full analysis: the base
// revision has no timing table) and `run` commit byte-identical `sta.rbin` and `limits.rbin`.
// The latch output l1/Q drives an RC net, so the latch transitions pinned on the D -> Q arc reach
// the prepared wire delays of the backward pass.
#[test]
fn update_and_run_commit_identical_tables_with_parasitics() {
    let dir = temp_dir("identity");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (wa, wb) = (workspace(&a, true), workspace(&b, true));
    let (code, up) = run_json(&[
        "update",
        "-w",
        &wa,
        "--base-rev",
        "rev2",
        "--max-paths",
        "16",
        "--require-wire-model",
        "spef_rc",
    ]);
    assert_eq!(code, 0, "{up}");
    let (code, run) = run_json(&[
        "run",
        "-w",
        &wb,
        "--base-rev",
        "rev2",
        "--max-paths",
        "16",
        "--require-wire-model",
        "spef_rc",
    ]);
    assert_eq!(code, 0, "{run}");
    for t in ["sta.rbin", "limits.rbin"] {
        let x = std::fs::read(a.join("ws/revisions/rev0003").join(t)).unwrap();
        let y = std::fs::read(b.join("ws/revisions/rev0003").join(t)).unwrap();
        assert!(x == y, "{t}: update and run differ");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0816, STA-REQ-0817: the pulse rules of the command line are the scope of the run, and
// `--pulse-pins clock-ends` leaves out a pin with pulse constraints whose clock passes on through
// its cell. The Wt15 clock pins state no pulse constraints, so the library is extended with a
// low-width group on the gater's CLK and pin attributes on the flip-flop's CLK.
#[test]
fn pulse_rules_are_the_scope_of_the_run() {
    let dir = temp_dir("rules");
    let lib = add_to_clock_pin(
        &library(Variant::Wt15),
        "wt_icg_p",
        "timing () { related_pin : \"CLK\"; timing_type : min_pulse_width; \
         fall_constraint (scalar) { values (\"0.3\"); } }",
    );
    let lib = add_to_clock_pin(
        &lib,
        "wt_dff_p",
        "min_pulse_width_high : 0.4; min_pulse_width_low : 0.4;",
    );
    assert!(lib.contains("min_pulse_width_high") && lib.contains("min_pulse_width;"));
    common::netlist_workspace(&dir, &lib, "wt15.lib", NETLIST, SDC);
    let w = dir.join("ws").to_string_lossy().into_owned();
    let (code, all) = run_json(&["run", "-w", &w, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{all}");
    assert_eq!(all["scope"]["pulse_rules"]["groups"], "worst", "{all}");
    assert_eq!(all["scope"]["pulse_rules"]["pins"], "all");
    assert_eq!(all["completeness"]["pass_through_pulse_pins"], 0);
    let (code, ends) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev2",
        "--pulse-groups",
        "last",
        "--pulse-pins",
        "clock-ends",
    ]);
    assert_eq!(code, 0, "{ends}");
    assert_eq!(ends["scope"]["pulse_rules"]["groups"], "last");
    assert_eq!(ends["scope"]["pulse_rules"]["pins"], "clock-ends");
    // g1/CLK passes the clock on to GCLK: counted, warned, and the pulse checks are partial.
    assert_eq!(ends["completeness"]["pass_through_pulse_pins"], 1, "{ends}");
    assert_eq!(ends["metrics"]["pulse_width_status"], "partial");
    assert!(
        ends["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "T0141")
    );
    let pins = |env_rev: &str| -> Vec<String> {
        let (code, rep) = run_json(&[
            "report",
            "-w",
            &w,
            "--rev",
            env_rev,
            "--section",
            "pulse_width",
        ]);
        assert_eq!(code, 0, "{rep}");
        let mut v: Vec<String> = rows(&rep, "pulse_width")
            .iter()
            .map(|r| r["pin"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(pins("rev2"), ["f1/CLK", "f2/CLK", "g1/CLK"]);
    assert_eq!(pins("rev3"), ["f1/CLK", "f2/CLK"]);
    // An unknown rule is a usage error.
    let (code, _) = run_json(&[
        "run",
        "-w",
        &w,
        "--base-rev",
        "rev3",
        "--pulse-groups",
        "first",
    ]);
    assert_ne!(code, 0, "an unknown --pulse-groups value is rejected");
    let _ = std::fs::remove_dir_all(&dir);
}
