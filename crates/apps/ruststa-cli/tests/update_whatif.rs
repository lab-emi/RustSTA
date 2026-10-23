// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa update` from an ancestor's timing table and `ruststa whatif` (`STA-REQ-0610` to
//! `STA-REQ-0613`, `STA-REQ-0620` to `STA-REQ-0628`).
//!
//! Oracles: the timing table that `update` commits is compared byte for byte with the table
//! `ruststa run` commits on the same netlist, and the independent checker (`ruststa check`) must
//! agree with it; every value `whatif` prints is compared with the timing table and the metrics
//! of `ruststa run` on a revision whose netlist has the edits applied.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{Conn, GateNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_workspace::edits::{self, Candidate, Edit, EditFile, ReportRequest, Sink};
use rusteda_workspace::sta::StaTable;
use rusteda_workspace::{Invocation, JsonMap, Rev, Upstream, Workspace};
use ruststa::engine::incr::named::{is_buffer, same_function, symmetric};
use ruststa::engine::incr::revision_cell_pins;
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::corpus;
use serde_json::Value;

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
        .join(format!("ruststa-upd-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// A deterministic SplitMix64 generator.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) % n.max(1) as u64) as usize
    }
}

/// A random named edit that keeps the logic of `g`, from the cells of `lib`, or `None`.
fn random_edit(g: &GateNetlist, lib: &Library, rng: &mut Rng, uid: &mut usize) -> Option<Edit> {
    let sym = g.symbols();
    let insts: Vec<_> = g.instances().collect();
    let (_, inst) = insts[rng.below(insts.len())];
    let name = sym.resolve(inst.name).to_string();
    let ct = g.cell(inst.cell).ok()?;
    let lc = lib.cells.iter().find(|c| c.name == sym.resolve(ct.name))?;
    let out_net = inst.connected.iter().find_map(|&p| {
        let pin = g.pin(p).ok()?;
        match (ct.pins[pin.cell_pin as usize].dir, pin.conn) {
            (PinDir::Output, Some(Conn::Net(n))) => Some(n),
            _ => None,
        }
    });
    let sinks = |net| -> Vec<Sink> {
        g.net(net)
            .map(|n| {
                n.pins
                    .iter()
                    .filter_map(|&p| {
                        let pin = g.pin(p).ok()?;
                        let i = g.instance(pin.inst).ok()?;
                        let cp = &g.cell(i.cell).ok()?.pins[pin.cell_pin as usize];
                        (cp.dir == PinDir::Input).then(|| Sink::Pin {
                            instance: sym.resolve(i.name).to_string(),
                            pin: sym.resolve(cp.name).to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let net_name = |net| {
        let n = g.net(net).ok()?;
        let base = sym.resolve(n.name?).to_string();
        Some(match n.index {
            Some(i) => format!("{base}[{i}]"),
            None => base,
        })
    };
    *uid += 1;
    match rng.below(4) {
        0 => {
            let alts: Vec<_> = lib
                .cells
                .iter()
                .filter(|c| c.name != lc.name && !c.dont_use && same_function(lc, c))
                .collect();
            let to = alts.get(rng.below(alts.len()))?;
            Some(Edit::Resize {
                instance: name,
                cell: to.name.clone(),
            })
        }
        1 => {
            let net = out_net?;
            let mut s = sinks(net);
            s.truncate(1 + rng.below(s.len()));
            let buf = lib.cells.iter().find(|c| !c.dont_use && is_buffer(c))?;
            (!s.is_empty()).then(|| Edit::InsertBuffer {
                net: net_name(net).unwrap_or_default(),
                cell: buf.name.clone(),
                instance: format!("w_buf_{uid}"),
                net_out: format!("w_nbuf_{uid}"),
                sinks: s,
            })
        }
        2 => {
            let ins: Vec<&str> = lc
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Input)
                .map(|p| p.name.as_str())
                .collect();
            let (a, b) = (ins.first()?, ins.get(1)?);
            symmetric(lc, a, b).then(|| Edit::SwapPins {
                instance: name,
                pin_a: (*a).to_string(),
                pin_b: (*b).to_string(),
            })
        }
        _ => {
            if !lc.sequentials.is_empty() || lc.outputs().count() != 1 {
                return None;
            }
            let net = out_net?;
            let mut s = sinks(net);
            s.truncate(1 + rng.below(s.len()));
            (!s.is_empty()).then(|| Edit::Clone {
                instance: name,
                new_instance: format!("w_cln_{uid}"),
                new_net: format!("w_ncln_{uid}"),
                sinks: s,
            })
        }
    }
}

fn head(ws: &Workspace) -> Rev {
    ws.head().expect("head")
}

/// The netlist of `rev` as `FrozenNetlist::thaw` gives it.
fn thaw(ws: &Workspace, rev: Rev) -> GateNetlist {
    let r = ws.revision(rev).expect("revision");
    let rbin = rusteda_workspace::netlist::open_rev(&r).expect("netlist");
    rusteda_workspace::netlist::decode(&rbin)
        .expect("decode")
        .thaw()
        .expect("thaw")
}

/// Commits `g` as the netlist of a new revision on HEAD; the other tables are carried.
fn commit_netlist(ws: &Workspace, g: &GateNetlist) -> Rev {
    let inv = Invocation::new("test", "0", "edit", vec!["test".to_string()]);
    let base = ws.resolve_base(&head(ws).to_string()).expect("base");
    let mut txn = ws.begin(base, &inv).expect("begin");
    rusteda_workspace::netlist::add_to_txn(&mut txn, &g.freeze(), Upstream::new()).expect("add");
    txn.commit(JsonMap::new()).expect("commit").rev
}

fn sta_bytes(ws: &Workspace, rev: Rev) -> Vec<u8> {
    let r = ws.revision(rev).expect("revision");
    std::fs::read(r.table_path("sta").expect("sta table")).expect("read sta.rbin")
}

/// Applies `edits` to a copy of `g` with the shared edits module.
fn applied(g: &GateNetlist, corners: &[(String, Vec<Library>)], edits: &[Edit]) -> GateNetlist {
    let mut g = g.clone();
    for e in edits {
        edits::apply(&mut g, e, &|n| revision_cell_pins(corners, n)).expect("edit applies");
    }
    g
}

/// `steps` rounds of random edits, each committed and updated: `update` computes incrementally
/// (in at least `min_incremental` of the steps) and commits the bytes `run` commits on the same
/// netlist; the checker agrees every 3rd step.
fn update_matches_run(
    label: &str,
    dir: &Path,
    lib: &Library,
    (steps, seed, min_incremental): (usize, u64, usize),
) {
    let ws_s = dir.join("ws").to_string_lossy().into_owned();
    let ws = Workspace::open(&ws_s).expect("open");
    let corners = vec![(String::from("default"), vec![lib.clone()])];
    let (code, env) = run_json(&["run", "-w", &ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{label}: {env}");
    let mut rng = Rng(seed);
    let mut uid = 0usize;
    let mut incremental = 0;
    for step in 0..steps {
        let mut g = thaw(&ws, head(&ws));
        let mut n = 0;
        while n < 1 + step % 3 {
            if let Some(e) = random_edit(&g, lib, &mut rng, &mut uid)
                && let Ok(_) = edits::apply(&mut g, &e, &|c| revision_cell_pins(&corners, c))
            {
                n += 1;
            }
        }
        let edited = commit_netlist(&ws, &g).to_string();
        let (code, upd) = run_json(&["update", "-w", &ws_s, "--base-rev", &edited]);
        assert_eq!(code, 0, "{label} step {step}: {upd}");
        let inc = &upd["incremental"];
        if inc["computation"] == "incremental" {
            incremental += 1;
            assert_eq!(inc["reason"], Value::Null);
            assert!(inc["evaluated_nodes"].as_u64() < inc["total_nodes"].as_u64());
        } else {
            assert_eq!(inc["reason"], "dirty_fraction_exceeded", "{label}: {inc}");
        }
        let u = head(&ws);
        let (code, full) = run_json(&["run", "-w", &ws_s, "--base-rev", &u.to_string()]);
        assert_eq!(code, 0, "{label}: {full}");
        let r = head(&ws);
        assert!(
            sta_bytes(&ws, u) == sta_bytes(&ws, r),
            "{label} step {step}: the sta.rbin of update ({u}) differs from run ({r})"
        );
        assert_eq!(upd["metrics"], full["metrics"], "{label} step {step}");
        if step % 3 == 2 {
            let (code, chk) = run_json(&["check", "-w", &ws_s, "--base-rev", &r.to_string()]);
            assert_eq!(code, 0, "{label} step {step}: {chk}");
            assert_eq!(chk["verdict"], "agree", "{label}");
        }
    }
    assert!(
        incremental >= min_incremental,
        "{label}: {incremental} of {steps} incremental"
    );
}

fn corpus_ws(dir: &Path, name: &str, sdc: &str) -> Library {
    let text = library(Variant::Full);
    let c = corpus(name).expect("corpus netlist");
    common::netlist_workspace(dir, &text, "wt_full.lib", c.text, sdc);
    read_text(&text, "wt_full.lib").expect("wt_full")
}

const FIFO_SDC: &str = "create_clock -name clk -period 3.0 [get_ports clk_i]\n\
    set_input_delay -clock clk 0.2 [all_inputs]\n\
    set_output_delay -clock clk 0.3 [all_outputs]\n\
    set_false_path -from [get_ports rst_ni]\n\
    set_multicycle_path 2 -setup -to [all_outputs]\n\
    set_multicycle_path 1 -hold -to [all_outputs]\n";

/// A small `wt_rich` design: two register stages with gates between them.
const RICH_V: &str = "\
module top (input clk, input a, input b, input c, output y, output z);
  wire n1, n2, n3, n4, n5, q1, q2, n6, n7;
  wt_nand2_x1 u1 (.A(a), .B(b), .Y(n1));
  wt_nor2_x1 u2 (.A(n1), .B(c), .Y(n2));
  wt_inv_x1 u3 (.A(n2), .Y(n3));
  wt_and2_x1 u4 (.A(n3), .B(a), .Y(n4));
  wt_dff_x1 r1 (.CK(clk), .D(n4), .Q(q1));
  wt_nand2_x2 u5 (.A(q1), .B(n1), .Y(n5));
  wt_xor2_x1 u6 (.A(n5), .B(q1), .Y(n6));
  wt_dff_x1 r2 (.CK(clk), .D(n6), .Q(q2));
  wt_inv_x2 u7 (.A(q2), .Y(n7));
  wt_buf_x1 u8 (.A(n7), .Y(y));
  wt_buf_x1 u9 (.A(n5), .Y(z));
endmodule
";

const RICH_SDC: &str = "create_clock -name clk -period 2.0 [get_ports clk]\n\
    set_input_delay -clock clk 0.1 [get_ports {a b c}]\n\
    set_output_delay -clock clk 0.1 [all_outputs]\n\
    set_max_delay 1.5 -from [get_ports a] -to [get_ports z]\n";

const COUNTER_SDC: &str = "create_clock -name clk -period 2.0 [get_ports clk_i]\n\
    set_input_delay -clock clk 0.2 [all_inputs]\n\
    set_output_delay -clock clk 0.3 [all_outputs]\n\
    set_max_delay 1.5 -from [get_ports en_i] -to [get_ports wrap_o]\n\
    set_case_analysis 0 [get_ports clr_i]\n\
    set_false_path -from [get_ports rst_ni]\n";

// STA-REQ-0610, STA-REQ-0611: on synthetic designs of both synthetic libraries, with timing
// exceptions and case analysis, `update` computes incrementally and its sta.rbin is
// byte-identical to the one `run` commits on the same revision; `ruststa check` agrees.
#[test]
fn update_commits_the_table_of_run() {
    let dir = temp_dir("synthetic");
    let l = corpus_ws(&dir.join("fifo"), "fifo", FIFO_SDC);
    update_matches_run("fifo", &dir.join("fifo"), &l, (9, 0xF1F0, 6));
    let l = corpus_ws(&dir.join("counter"), "counter", COUNTER_SDC);
    update_matches_run("counter", &dir.join("counter"), &l, (6, 0xC0C0, 3));
    let rich = library(Variant::Rich);
    common::netlist_workspace(&dir.join("rich"), &rich, "wt_rich.lib", RICH_V, RICH_SDC);
    let l = read_text(&rich, "wt_rich.lib").expect("wt_rich");
    update_matches_run("rich", &dir.join("rich"), &l, (3, 0x51C4, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Commits the constraints `sdc_text` on HEAD.
fn commit_sdc(ws: &Workspace, sdc_text: &str) -> Rev {
    let (model, _) = rusteda_formats::sdc::read_str(sdc_text);
    let mut w = rusteda_workspace::RbinWriter::new("CONSTR", 1).expect("writer");
    let sections = model.to_sections();
    for sec in &sections {
        w.section_bytes(u32::from(sec.id), sec.elem_size, &sec.bytes)
            .expect("section");
    }
    let inv = Invocation::new("test", "0", "sdc", vec!["test".to_string()]);
    let base = ws.resolve_base(&head(ws).to_string()).expect("base");
    let mut txn = ws.begin(base, &inv).expect("begin");
    txn.add_rbin("constraints", "constraints.rbin", &w, Upstream::new())
        .expect("constraints");
    txn.commit(JsonMap::new()).expect("commit").rev
}

/// One random edit of the netlist at HEAD, committed.
fn commit_random_edit(ws: &Workspace, lib: &Library, rng: &mut Rng, uid: &mut usize) -> Rev {
    let corners = vec![(String::from("default"), vec![lib.clone()])];
    let mut g = thaw(ws, head(ws));
    loop {
        if let Some(e) = random_edit(&g, lib, rng, uid)
            && edits::apply(&mut g, &e, &|c| revision_cell_pins(&corners, c)).is_ok()
        {
            return commit_netlist(ws, &g);
        }
    }
}

// STA-REQ-0303, STA-REQ-0610, STA-REQ-0612: `update` computes the whole graph when the ancestor's
// table cannot serve, and says why; `--verify-full` checks an incremental table against a full
// computation; a revision without an ancestor table is analyzed in full (the rule of
// STA-REQ-0303 that still holds).
#[test]
fn update_fallbacks_and_verify_full() {
    let dir = temp_dir("fallback");
    let l = corpus_ws(&dir, "fifo", FIFO_SDC);
    let ws_s = dir.join("ws").to_string_lossy().into_owned();
    let ws = Workspace::open(&ws_s).expect("open");
    let (mut rng, mut uid) = (Rng(0xFA11), 0usize);
    let reason = |v: &Value| {
        (
            v["incremental"]["computation"].clone(),
            v["incremental"]["reason"].clone(),
        )
    };

    let (code, first) = run_json(&["update", "-w", &ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{first}");
    assert_eq!(reason(&first), ("full".into(), "no_ancestor_table".into()));
    assert_eq!(first["incremental"]["from_rev"], Value::Null);

    let e = commit_random_edit(&ws, &l, &mut rng, &mut uid).to_string();
    let (code, inc) = run_json(&["update", "-w", &ws_s, "--base-rev", &e, "--verify-full"]);
    assert_eq!(code, 0, "{inc}");
    assert_eq!(reason(&inc), ("incremental".into(), Value::Null));
    assert_eq!(inc["incremental"]["from_rev"], "rev2");

    let e = commit_random_edit(&ws, &l, &mut rng, &mut uid).to_string();
    let (code, _) = run_json(&[
        "run",
        "-w",
        &ws_s,
        "--base-rev",
        &e,
        "--sta-store",
        "summary",
    ]);
    assert_eq!(code, 0);
    let e = commit_random_edit(&ws, &l, &mut rng, &mut uid).to_string();
    let (code, summ) = run_json(&["update", "-w", &ws_s, "--base-rev", &e]);
    assert_eq!(code, 0, "{summ}");
    assert_eq!(
        reason(&summ),
        ("full".into(), "ancestor_summary_only".into())
    );

    let e = commit_sdc(&ws, &FIFO_SDC.replace("3.0", "3.5")).to_string();
    let (code, sdc) = run_json(&["update", "-w", &ws_s, "--base-rev", &e]);
    assert_eq!(code, 0, "{sdc}");
    assert_eq!(reason(&sdc), ("full".into(), "constraints_changed".into()));
    assert!(
        sdc["diagnostics"]
            .as_array()
            .is_some_and(|d| d.iter().any(|x| x["code"] == "T0110")),
        "a note says why the whole graph was computed: {sdc}"
    );
    let (code, utd) = run_json(&["update", "-w", &ws_s, "--base-rev", &head(&ws).to_string()]);
    assert_eq!(code, 0);
    assert_eq!(utd["incremental"]["computation"], "none");
    assert_eq!(utd["new_rev"], Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The value of `v` as an exact `f64`, `None` for `null`.
fn num(v: &Value) -> Option<u64> {
    v.as_f64().map(f64::to_bits)
}

/// `whatif` with base edits, random candidates and endpoint and path requests on `rev2` of the
/// workspace in `dir`; every value it prints equals the table and metrics of `ruststa run` on a
/// revision with the edits applied (`STA-REQ-0625`), and `whatif` changes nothing in the
/// workspace (`STA-REQ-0621`).
fn whatif_matches_run(label: &str, dir: &Path, lib: &Library, seed: u64, n_candidates: usize) {
    let ws_s = dir.join("ws").to_string_lossy().into_owned();
    let ws = Workspace::open(&ws_s).expect("open");
    let corners = vec![(String::from("default"), vec![lib.clone()])];
    let (code, run2) = run_json(&["run", "-w", &ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{label}: {run2}");
    let g0 = thaw(&ws, Rev(2));
    let table = StaTable::open_rev(&ws.revision(Rev(2)).expect("rev2")).expect("table");
    let eps = table.endpoints(0).expect("endpoints");
    let (mut rng, mut uid) = (Rng(seed), 0usize);
    let mut file = EditFile::default();
    let mut g = g0.clone();
    while file.base.is_empty() {
        if let Some(e) = random_edit(&g, lib, &mut rng, &mut uid)
            && edits::apply(&mut g, &e, &|c| revision_cell_pins(&corners, c)).is_ok()
        {
            file.base.push(e);
        }
    }
    for k in 0..n_candidates {
        let mut c = g.clone();
        let mut list = Vec::new();
        while list.len() < 1 + k % 2 {
            if let Some(e) = random_edit(&c, lib, &mut rng, &mut uid)
                && edits::apply(&mut c, &e, &|n| revision_cell_pins(&corners, n)).is_ok()
            {
                list.push(e);
            }
        }
        file.candidates.push(Candidate {
            id: format!("c{k}"),
            edits: list,
        });
    }
    file.candidates.push(Candidate {
        id: "bad".into(),
        edits: vec![Edit::Resize {
            instance: "no_such_instance".into(),
            cell: lib.cells[0].name.clone(),
        }],
    });
    file.report = ReportRequest {
        endpoints: vec![
            eps[0].name.clone(),
            eps[eps.len() / 2].name.clone(),
            "ep1".into(),
        ],
        paths: vec!["path1".into(), "path2".into()],
    };
    let path = dir.join("edits.json");
    std::fs::write(&path, file.to_json()).expect("write edits");
    let p = path.to_string_lossy().into_owned();
    let history = std::fs::read(dir.join("ws/history.jsonl")).expect("history");
    let before = head(&ws);
    let (code, q) = run_json(&["whatif", "-w", &ws_s, "--rev", "rev2", "--edits", &p]);
    assert_eq!(code, 0, "{label}: {q}");
    assert_eq!(head(&ws), before, "{label}: whatif commits nothing");
    assert_eq!(
        std::fs::read(dir.join("ws/history.jsonl")).expect("history"),
        history
    );
    assert_eq!(q["rev"], "rev2");
    assert_eq!(q["new_rev"], Value::Null);

    let check = |which: &str, values: &Value, edits_of: &[Edit]| {
        let netlist = applied(&g0, &corners, edits_of);
        let e = commit_netlist(&ws, &netlist).to_string();
        let (code, run) = run_json(&["run", "-w", &ws_s, "--base-rev", &e]);
        assert_eq!(code, 0, "{label}/{which}: {run}");
        let t = StaTable::open_rev(&ws.revision(head(&ws)).expect("rev")).expect("table");
        for (k, v) in run["metrics"].as_object().expect("metrics") {
            assert_eq!(num(v), num(&values["summary"][k]), "{label}/{which}: {k}");
        }
        for (c, cv) in values["corners"]
            .as_array()
            .expect("corners")
            .iter()
            .enumerate()
        {
            let row = t.corner_summary(c).expect("summary row");
            let s = &cv["setup"];
            assert_eq!(
                num(&s["wns_ns"]),
                row.setup_wns_ns.map(f64::to_bits),
                "{label}/{which}"
            );
            assert_eq!(
                num(&s["tns_ns"]).unwrap_or(0),
                row.setup_tns_ns.to_bits(),
                "{label}/{which}"
            );
            assert_eq!(
                s["violating_endpoints"], row.setup_violating,
                "{label}/{which}"
            );
            let h = &cv["hold"];
            assert_eq!(
                num(&h["wns_ns"]),
                row.hold_wns_ns.map(f64::to_bits),
                "{label}/{which}"
            );
            assert_eq!(
                num(&h["tns_ns"]).unwrap_or(0),
                row.hold_tns_ns.to_bits(),
                "{label}/{which}"
            );
            assert_eq!(
                h["violating_endpoints"], row.hold_violating,
                "{label}/{which}"
            );
        }
        let eps = t.endpoints(0).expect("endpoints");
        let slack_of = |name: &str| eps.iter().find(|e| e.name == name);
        for ev in values["endpoints"].as_array().expect("endpoints") {
            let name = ev["name"].as_str().expect("name");
            let got = &ev["corners"][0];
            let want = slack_of(name);
            assert_eq!(
                num(&got["setup_slack_ns"]),
                want.and_then(|e| e.setup_slack_ns).map(f64::to_bits),
                "{label}/{which}: {name}"
            );
            assert_eq!(
                num(&got["hold_slack_ns"]),
                want.and_then(|e| e.hold_slack_ns).map(f64::to_bits),
                "{label}/{which}: {name}"
            );
        }
        for pv in values["paths"].as_array().expect("paths") {
            let e = slack_of(pv["endpoint"].as_str().expect("endpoint"));
            let hold = matches!(pv["check"].as_str(), Some("hold" | "removal"));
            let want = e.and_then(|e| {
                if hold {
                    e.hold_slack_ns
                } else {
                    e.setup_slack_ns
                }
            });
            assert_eq!(
                num(&pv["slack_ns"]),
                want.map(f64::to_bits),
                "{label}/{which}: {pv}"
            );
        }
    };
    check("base", &q["base"], &file.base);
    let cands = q["candidates"].as_array().expect("candidates");
    assert_eq!(cands.len(), file.candidates.len());
    for (c, cv) in file.candidates.iter().zip(cands) {
        assert_eq!(cv["id"], c.id.as_str());
        if c.id == "bad" {
            assert_eq!(cv["status"], "rejected", "{label}: {cv}");
            assert_eq!(cv["error"]["code"], "W0305", "{label}: {cv}");
            continue;
        }
        assert_eq!(cv["status"], "evaluated", "{label}: {cv}");
        let mut all = file.base.clone();
        all.extend(c.edits.iter().cloned());
        check(&c.id, &cv["values"], &all);
    }
}

// STA-REQ-0620 to STA-REQ-0625: `whatif` on synthetic designs: every value equals `ruststa run` on
// the edited netlist, and nothing is committed.
#[test]
fn whatif_values_equal_run() {
    let dir = temp_dir("whatif");
    let l = corpus_ws(&dir.join("fifo"), "fifo", FIFO_SDC);
    whatif_matches_run("fifo", &dir.join("fifo"), &l, 0x3A7F, 4);
    let l = corpus_ws(&dir.join("counter"), "counter", COUNTER_SDC);
    whatif_matches_run("counter", &dir.join("counter"), &l, 0x3A80, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0622, STA-REQ-0626, STA-REQ-0627: unknown targets, files beyond the bounds, unreadable
// files and an exhausted time budget are coded, and nothing is committed.
#[test]
fn whatif_errors_and_bounds() {
    let dir = temp_dir("whatif-errors");
    corpus_ws(&dir, "counter", COUNTER_SDC);
    let ws_s = dir.join("ws").to_string_lossy().into_owned();
    let (code, _) = run_json(&["run", "-w", &ws_s, "--base-rev", "rev1"]);
    assert_eq!(code, 0);
    let write = |name: &str, f: &EditFile| {
        let p = dir.join(name);
        std::fs::write(&p, f.to_json()).expect("write");
        p.to_string_lossy().into_owned()
    };
    let codes = |v: &Value| -> Vec<String> {
        v["diagnostics"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|x| x["code"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut f = EditFile::default();
    f.report.endpoints = vec!["no_such_pin/D".into()];
    let p = write("unknown.json", &f);
    let (code, v) = run_json(&["whatif", "-w", &ws_s, "--rev", "rev2", "--edits", &p]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "UNKNOWN_HANDLE");
    assert!(codes(&v).contains(&"T0101".to_string()), "{v}");

    let mut f = EditFile::default();
    for k in 0..3 {
        f.candidates.push(Candidate {
            id: format!("c{k}"),
            edits: Vec::new(),
        });
    }
    let p = write("many.json", &f);
    let (code, v) = run_json(&[
        "whatif",
        "-w",
        &ws_s,
        "--edits",
        &p,
        "--max-candidates",
        "2",
    ]);
    assert_eq!(code, 1, "{v}");
    assert!(codes(&v).contains(&"T0106".to_string()), "{v}");
    let (code, v) = run_json(&[
        "whatif",
        "-w",
        &ws_s,
        "--edits",
        &p,
        "--timeout",
        "0.000000001",
    ]);
    assert_eq!(code, 5, "{v}");
    assert!(codes(&v).contains(&"T0108".to_string()), "{v}");
    assert!(
        v["candidates"]
            .as_array()
            .is_some_and(|c| c.iter().all(|x| x["status"] == "not_evaluated")),
        "{v}"
    );
    let missing = dir.join("missing.json").to_string_lossy().into_owned();
    let (code, v) = run_json(&["whatif", "-w", &ws_s, "--edits", &missing]);
    assert_eq!(code, 1, "{v}");
    assert!(codes(&v).contains(&"T0107".to_string()), "{v}");
    std::fs::write(dir.join("bad.json"), "{\"schema\": \"x\"}").expect("write");
    let bad = dir.join("bad.json").to_string_lossy().into_owned();
    let (code, v) = run_json(&["whatif", "-w", &ws_s, "--edits", &bad]);
    assert_eq!(code, 1, "{v}");
    assert!(codes(&v).contains(&"W0301".to_string()), "{v}");
    let ws = Workspace::open(&ws_s).expect("open");
    assert_eq!(head(&ws), Rev(2), "whatif committed nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

// STA-REQ-0611, STA-REQ-0625 on both PDK families: on the designs of the PDK suite that
// `cargo xtask verify pdk` writes, `update` commits the bytes of `run` after random edits and the
// checker agrees, and `whatif` prints the values of `run` on the edited netlists. Invoked by
// `cargo xtask verify pdk`.
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_update_and_whatif() {
    let pdk_dir = PathBuf::from(std::env::var("RUSTSTA_PDK_VERIFY_DIR").expect("PDK directory"));
    let out = PathBuf::from(std::env::var("RUSTSTA_PDK_VERIFY_OUT").expect("output directory"));
    let libs: Vec<(String, String)> = std::env::var("RUSTSTA_PDK_VERIFY_LIBS")
        .unwrap_or_default()
        .split(';')
        .filter_map(|item| {
            let f: Vec<&str> = item.split(',').collect();
            (f.len() == 4 && f[3].starts_with("tt")).then(|| (f[0].to_string(), f[1].to_string()))
        })
        .collect();
    let family = |p: &str| libs.iter().any(|(n, _)| n.starts_with(p));
    assert!(
        family("sky130") && family("gf180"),
        "both PDK families: {libs:?}"
    );
    for (idx, (name, file)) in libs.iter().enumerate() {
        let lib_text = std::fs::read_to_string(pdk_dir.join(file)).expect("read PDK library");
        let lib = read_text(&lib_text, file).expect("parse PDK library");
        for (k, (design, steps, min_inc)) in [
            ("dag", 6, 3),
            ("high_fanout", 4, 2),
            ("deep_chain", 4, 1),
            ("two_clock_edges", 3, 0),
            ("latches", 2, 0),
        ]
        .into_iter()
        .enumerate()
        {
            let src = out.join(name).join(design);
            let verilog = std::fs::read_to_string(src.join("netlist.v")).expect("netlist.v");
            let sdc = std::fs::read_to_string(src.join("constraints.sdc")).expect("sdc");
            let label = format!("{name}/{design}");
            let seed = 0x9D50_0000 + (idx * 16 + k) as u64;
            let dir = src.join("ws-update");
            let _ = std::fs::remove_dir_all(&dir);
            common::netlist_workspace(&dir, &lib_text, file, &verilog, &sdc);
            update_matches_run(&label, &dir, &lib, (steps, seed, min_inc));
            let _ = std::fs::remove_dir_all(&dir);
            let dir = src.join("ws-whatif");
            let _ = std::fs::remove_dir_all(&dir);
            common::netlist_workspace(&dir, &lib_text, file, &verilog, &sdc);
            whatif_matches_run(&label, &dir, &lib, seed ^ 0xFF, 3);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
