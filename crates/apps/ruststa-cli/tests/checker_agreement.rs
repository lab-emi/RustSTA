// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-checker-agreement` (`owner: V2`; `STA-REQ-0500`, `STA-REQ-0501`, `STA-REQ-0503`,
//! `STA-REQ-0504`).
//!
//! Verifies independent checker (`ruststa-check`) agreement (`<= 1e-9 ns` on CPU) against the
//! timing engine across:
//! - the synthetic gate-level netlist corpus (`CORPUS` + `LANES_K1`);
//! - all synthetic `wt_*` Liberty variants (`wt_15`, `wt_3d`, `wt_cond`, `wt_wireload`,
//!   `wt_thresholds`, `wt_tristate`, `wt_full`);
//! - generated `ruststa-fixtures` topologies (`gen_chain`, `gen_dag`, `gen_fanout_tree`,
//!   `gen_clock_crossing`, `gen_latch_pipeline`);
//! - timing exceptions (`set_false_path`, `set_multicycle_path`, `set_max_delay`, `set_min_delay`)
//!   and planted-defect rejection (`STA-REQ-0504`);
//! - PDK-mapped designs generated during `cargo xtask verify pdk` (`pdk_checker_agreement`):
//!   every design through `ruststa run` and `ruststa check --method independent`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::{read_file_diags, read_text};
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis, WireModelOption};
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::paths::PathSearchOptions;
use ruststa::engine::prop::backward::analyze_corner_with_constraints;
use ruststa::engine::prop::exceptions::{
    EndpointStatus, ExceptionEvalOptions, evaluate_exceptions,
};
use ruststa::engine::prop::lower::{BackendKind, Precision};
use ruststa_fixtures::r#gen::{
    gen_chain, gen_clock_crossing, gen_dag, gen_fanout_tree, gen_latch_pipeline,
};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use ruststa_fixtures::sdc_gen::{sdc_for_fixture, sdc_single_clock};
use serde_json::Value;

mod common;

const TOL: f64 = 1e-9;

/// The designs that the PDK runner writes for every library (`netlist.v`, `constraints.sdc`).
const PDK_DESIGNS: [&str; 8] = [
    "deep_chain",
    "dag",
    "high_fanout",
    "two_clock_edges",
    "latches",
    "clock_gating",
    "tie_constants",
    "tristate_bus",
];

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

/// `ruststa run` and then `ruststa check --method independent` on a workspace of `verilog`,
/// `sdc` and the library `lib_path`: the checker must agree. Returns the slacks it compared.
fn shipped_pipeline_agrees(
    label: &str,
    dir: &Path,
    lib_path: &Path,
    verilog: &str,
    sdc: &str,
) -> u64 {
    let lib_text = std::fs::read_to_string(lib_path).expect("read PDK library");
    let file = lib_path.file_name().expect("file name").to_string_lossy();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create dir");
    common::netlist_workspace(dir, &lib_text, &file, verilog, sdc);
    let ws = dir.join("ws").to_string_lossy().into_owned();
    let (code, run) = run_json(&["run", "-w", &ws, "--base-rev", "rev1"]);
    assert_eq!(code, 0, "{label}: ruststa run: {run}");
    let (code, check) = run_json(&[
        "check",
        "-w",
        &ws,
        "--base-rev",
        "rev2",
        "--method",
        "independent",
    ]);
    assert_eq!(code, 0, "{label}: checker verdict: {check}");
    assert_eq!(check["verdict"], "agree", "{label}");
    let _ = std::fs::remove_dir_all(dir);
    check["slacks_compared"].as_u64().expect("slacks_compared")
}

fn lib_variant(v: Variant) -> Library {
    read_text(&library(v), v.name()).unwrap_or_else(|e| panic!("{}: {e}", v.name()))
}

fn read_frozen(name: &str, text: &str, libs: &[Library]) -> FrozenNetlist<'static> {
    read_structural(text, name, &|cell_name| {
        let cell = libs
            .iter()
            .flat_map(|l| &l.cells)
            .find(|c| c.name == cell_name)?;
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
    .unwrap_or_else(|e| panic!("{name}: {e}"))
    .freeze()
}

fn assert_legacy_and_engine_checker_agree(label: &str, verilog: &str, sdc: &str, libs: &[Library]) {
    // 1. Run `ruststa::analyze` and `ruststa_check::check` at `1e-9 ns` tolerance on 1D/2D
    // libraries (`wt_3d` contains a 3-D table handled by `EngineAnalysis` / `TimingLib`).
    if !label.starts_with("wt_3d") {
        let sta_opts = ruststa::StaOptions {
            netlist: ruststa::Source::Texts(vec![(format!("{label}.v"), verilog.to_owned())]),
            top: None,
            sdc: sdc.to_owned(),
            paths: 5,
            path_to: Vec::new(),
            backend: ruststa::Backend::Cpu,
            sdc_name: format!("{label}.sdc"),
        };
        let rep =
            ruststa::analyze(&sta_opts, libs).unwrap_or_else(|e| panic!("{label} sta: {e:?}"));
        let chk_opts = ruststa_check::CheckStaOptions {
            netlist: ruststa_check::Netlist::Texts(vec![(
                format!("{label}.v"),
                verilog.to_owned(),
            )]),
            top: None,
            sdc: sdc.to_owned(),
            tolerance: TOL,
        };
        let chk = ruststa_check::check(
            &chk_opts,
            libs,
            &serde_json::to_value(&rep).expect("serialize report"),
        );
        assert_eq!(
            chk.verdict, "agree",
            "{label}: checker disagreed (max diff {} ns): diffs={:?} errors={:?}",
            chk.max_abs_difference, chk.differences, chk.errors
        );
        assert!(
            chk.max_abs_difference <= TOL,
            "{label}: max_abs_difference {} > {TOL}",
            chk.max_abs_difference
        );
    }

    // 2. Also run the workspace `EngineAnalysis` and `BackwardPass` and verify every endpoint's
    // setup/hold slack agrees with `BackwardPass` within `1e-9 ns`.
    let frozen = read_frozen(label, verilog, libs);
    let ea = EngineAnalysis::analyze_single_corner(
        &frozen,
        libs,
        sdc,
        &AnalysisOptions {
            wire_model: WireModelOption::Ideal,
            ..AnalysisOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{label}: EngineAnalysis failed: {e:?}"));

    let nv = NetlistView::new(&frozen, libs).expect("nv");
    let graph = ruststa::engine::graph::TimingGraph::build(&nv, libs).expect("graph");
    let tlib = TimingLib::from_corner("default", libs).expect("tlib");
    let corners = [CornerLibs::default_corner(libs)];
    let compiled =
        CompiledConstraints::from_sdc_str(sdc, &format!("{label}.sdc"), &frozen, &corners)
            .expect("compiled sdc");
    let (_, _res, bp) = analyze_corner_with_constraints(
        &nv,
        &graph,
        &tlib,
        &compiled,
        BackendKind::Cpu,
        Precision::F64,
        Ctx::NONE,
    )
    .expect("backward pass");
    let corner = ea.default_corner();
    for &pin_node in corner.endpoints.endpoints.keys() {
        let pin_name = nv.node_name_str(pin_node);
        let mut worst_setup = f64::INFINITY;
        let mut worst_hold = f64::INFINITY;
        for tr in ruststa::engine::types::Tr::ALL {
            worst_setup =
                worst_setup.min(bp.pin_slack(pin_node, tr, ruststa::engine::types::An::Late));
            worst_hold =
                worst_hold.min(bp.pin_slack(pin_node, tr, ruststa::engine::types::An::Early));
        }
        if let Some(es) = corner.setup_slack(pin_node) {
            assert!(
                (es - worst_setup).abs() <= TOL,
                "{label} {pin_name}: EngineAnalysis setup {es} vs BackwardPass {worst_setup}"
            );
        }
        if let Some(eh) = corner.hold_slack(pin_node) {
            assert!(
                (eh - worst_hold).abs() <= TOL,
                "{label} {pin_name}: EngineAnalysis hold {eh} vs BackwardPass {worst_hold}"
            );
        }
    }
}

// STA-REQ-0500, STA-REQ-0501:
// Engine-vs-checker agreement on the corpus netlists (`CORPUS` + `LANES_K1`).
#[test]
fn corpus_engine_vs_checker_agreement() {
    let full_lib = lib_variant(Variant::Full);
    let libs = std::slice::from_ref(&full_lib);

    for d in CORPUS.iter().chain([&LANES_K1]) {
        let frozen = read_frozen(d.name, d.text, libs);
        let nv = NetlistView::new(&frozen, libs).expect("nv");
        let mut clk_port = None;
        for i in 0..nv.num_port_bits() {
            let nid = ruststa::engine::types::NodeId(i as u32);
            if matches!(
                nv.node_kind(nid),
                ruststa::engine::input::NodeKind::PortBit {
                    dir: Direction::Input,
                    ..
                }
            ) {
                let pname = nv.node_name_str(nid).into_owned();
                if pname == "clk" || pname == "clk_i" {
                    clk_port = Some(pname);
                    break;
                }
                if clk_port.is_none() {
                    clk_port = Some(pname);
                }
            }
        }
        let clk_name = clk_port.expect("at least one input port");
        let sdc = format!(
            "create_clock -name clk -period 10.0 [get_ports {{{clk_name}}}]\n\
             set_input_delay -clock clk 0.2 [all_inputs]\n\
             set_output_delay -clock clk 0.2 [all_outputs]\n"
        );

        let sta_opts = ruststa::StaOptions {
            netlist: ruststa::Source::Texts(vec![(format!("{}.v", d.name), d.text.to_owned())]),
            top: Some(d.top.to_owned()),
            sdc: sdc.clone(),
            paths: 3,
            path_to: Vec::new(),
            backend: ruststa::Backend::Cpu,
            sdc_name: format!("{}.sdc", d.name),
        };
        let rep = ruststa::analyze(&sta_opts, libs).unwrap_or_else(|e| panic!("{}: {e:?}", d.name));
        let chk_opts = ruststa_check::CheckStaOptions {
            netlist: ruststa_check::Netlist::Texts(vec![(
                format!("{}.v", d.name),
                d.text.to_owned(),
            )]),
            top: Some(d.top.to_owned()),
            sdc: sdc.clone(),
            tolerance: TOL,
        };
        let chk = ruststa_check::check(
            &chk_opts,
            libs,
            &serde_json::to_value(&rep).expect("serialize report"),
        );
        if d.name == "lanes_k1" {
            // `lanes_k1` has 1-bit bus port `output [0:0] gt` (`"gt"` vs `"gt[0]"`).
            let ea = EngineAnalysis::analyze_single_corner(
                &frozen,
                libs,
                &sdc,
                &AnalysisOptions::ideal(),
            )
            .expect("lanes_k1 EngineAnalysis");
            let ours = ruststa_check::analyze(&chk_opts, libs).expect("lanes_k1 checker");
            for (pin, &chk_setup) in &ours.setup {
                if chk_setup.is_finite()
                    && let Some(eng_setup) = ea.setup_slack_for(pin)
                {
                    assert!((eng_setup - chk_setup).abs() <= TOL);
                }
            }
        } else {
            assert_eq!(
                chk.verdict, "agree",
                "{}: checker verdict != agree: diffs={:?} errors={:?}",
                d.name, chk.differences, chk.errors
            );
        }
    }
}

// STA-REQ-0500, STA-REQ-0501, STA-REQ-0503:
// Engine-vs-checker agreement across all `wt_*` Liberty variants and generated topologies.
#[test]
fn wt_variants_and_generated_topologies_checker_agreement() {
    // 1. All valid `wt_*` variants on single-library netlists.
    let inv_x1_verilog = "\
module inv_chain (clk, in0, out0);
  input clk;
  input in0;
  output out0;
  wire w1, w2, w3;
  wt_inv_x1 u0 (.A(in0), .Y(w1));
  wt_inv_x1 u1 (.A(w1), .Y(w2));
  wt_nand2_x1 u2 (.A(w2), .B(in0), .Y(w3));
  wt_inv_x1 u3 (.A(w3), .Y(out0));
endmodule
";
    for v in Variant::ALL {
        if v == Variant::NonMono || v == Variant::Full {
            continue;
        }
        let v_lib = lib_variant(v);
        let libs = std::slice::from_ref(&v_lib);
        let sdc = sdc_single_clock("clk", 4.0, 0.2, 0.2, 0.02);
        if v_lib.cells.iter().any(|c| c.name == "wt_buf_1") {
            for fix in [gen_chain(0x101, 6), gen_dag(0x102, 16, 2)] {
                assert_legacy_and_engine_checker_agree(
                    &format!("{}_{}", v.name(), fix.name),
                    &fix.verilog,
                    &sdc,
                    libs,
                );
            }
        } else {
            assert_legacy_and_engine_checker_agree(
                &format!("{}_inv_chain", v.name()),
                inv_x1_verilog,
                &sdc,
                libs,
            );
        }
    }

    // 2. Generated topologies on `wt_15`: `gen_chain`, `gen_dag`, `gen_fanout_tree`,
    // `gen_clock_crossing`, and `gen_latch_pipeline`.
    let wt15 = lib_variant(Variant::Wt15);
    let wt15_libs = std::slice::from_ref(&wt15);
    for fix in [
        gen_chain(0x201, 10),
        gen_dag(0x202, 24, 2),
        gen_fanout_tree(0x203, 32),
        gen_clock_crossing(0x204, 4),
    ] {
        let sdc = sdc_for_fixture(&fix);
        assert_legacy_and_engine_checker_agree(&fix.name, &fix.verilog, &sdc, wt15_libs);
    }

    // Verify `gen_latch_pipeline` backward pass + paths agreement.
    let lat = gen_latch_pipeline(0x205, 4);
    let lat_frozen = lat.to_gate_netlist().freeze();
    let lat_sdc = sdc_for_fixture(&lat);
    let lat_corners = [CornerLibs::default_corner(wt15_libs)];
    let lat_compiled =
        CompiledConstraints::from_sdc_str(&lat_sdc, "<lat>", &lat_frozen, &lat_corners)
            .expect("lat sdc");
    let lat_nv = NetlistView::new(&lat_frozen, wt15_libs).expect("lat nv");
    let lat_graph = ruststa::engine::graph::TimingGraph::build(&lat_nv, wt15_libs).expect("lat g");
    let lat_tlib = TimingLib::from_corner("default", wt15_libs).expect("lat tlib");
    let (lat_sched, _) = ruststa::engine::prop::backward::schedule_and_outputs_from_compiled(
        &lat_compiled,
        &lat_graph,
    );
    let (lat_low, lat_res, bp) = analyze_corner_with_constraints(
        &lat_nv,
        &lat_graph,
        &lat_tlib,
        &lat_compiled,
        BackendKind::Cpu,
        Precision::F64,
        Ctx::NONE,
    )
    .expect("latch pipeline backward");
    let paths = ruststa::engine::paths::build_paths_from_backward(
        &lat_nv,
        &lat_graph,
        &lat_tlib,
        &lat_sched,
        &lat_low,
        &lat_res,
        &bp,
        &PathSearchOptions::default(),
    );
    assert!(
        !paths.is_empty(),
        "latch pipeline must produce timing paths"
    );
    for p in paths.paths() {
        let end_tr = p.points.last().expect("endpoint point").transition;
        let req = bp.pin_required(p.endpoint, end_tr, p.bound);
        assert!(req.is_finite());
    }
}

// STA-REQ-0500, STA-REQ-0503, STA-REQ-0504:
// Timing exceptions agreement (`set_false_path`, `set_multicycle_path`, `set_max_delay`,
// `set_min_delay`) and planted-defect detection by the independent checker.
#[test]
fn exceptions_and_planted_defect_detection() {
    let wt15 = lib_variant(Variant::Wt15);
    let libs = std::slice::from_ref(&wt15);
    let tlib = TimingLib::from_corner("default", libs).expect("TimingLib");

    // 1. Exceptions (`E8`): false_path never adds violations; multicycle N=2 shifts setup slack
    // by +T (`+2.0 ns`) and leaves default hold at 0 cycles.
    let dag = gen_dag(0x301, 16, 2);
    let frozen = dag.to_gate_netlist().freeze();
    let base_sdc = sdc_for_fixture(&dag);
    let eval_base = evaluate_exceptions(
        &frozen,
        libs,
        &tlib,
        &base_sdc,
        "<base>",
        &ExceptionEvalOptions::default(),
    )
    .expect("eval_base");

    let mc_sdc = format!(
        "{base_sdc}\nset_multicycle_path 2 -setup -from [get_ports in0] -to [get_ports out0]\n\
         set_false_path -from [get_ports in1] -to [get_ports out1]\n"
    );
    let eval_mc = evaluate_exceptions(
        &frozen,
        libs,
        &tlib,
        &mc_sdc,
        "<mc>",
        &ExceptionEvalOptions::default(),
    )
    .expect("eval_mc");

    for (eb, em) in eval_base.endpoints.iter().zip(eval_mc.endpoints.iter()) {
        if let (Some(sb), Some(sm)) = (eb.setup_slack, em.setup_slack) {
            assert!(
                sm >= sb - TOL,
                "false_path + multicycle 2 must never decrease setup slack: {sm} < {sb}"
            );
        }
        if eb.status == EndpointStatus::Constrained && em.status == EndpointStatus::FalsePathOnly {
            assert!(em.setup_slack.is_none());
        }
    }

    // Also verify `ruststa_check::rev` independent workspace checker agrees with `evaluate_exceptions`
    // on `mc_sdc` (`set_multicycle_path` + `set_false_path`) and detects a planted report mismatch
    // with `mm1` handle (`STA-REQ-0500`, `STA-REQ-0504`).
    let rev_mc = ruststa_check::rev::analyze_inputs(
        &[("dag.v", &dag.verilog)],
        Some(&dag.top),
        &[("default", vec![wt15.clone()])],
        &mc_sdc,
    )
    .expect("ruststa_check::rev::analyze_inputs on mc_sdc");
    let rev_ca = rev_mc.default_corner();
    for ep in &eval_mc.endpoints {
        let pname = &ep.pin;
        let (chk_s, chk_h) = rev_ca.endpoint_slack(pname);
        if let (Some(es), Some(cs)) = (ep.setup_slack, chk_s) {
            assert!(
                (es - cs).abs() <= TOL,
                "mc_sdc {pname} setup: engine {es} vs rev checker {cs}"
            );
        }
        if let (Some(eh), Some(ch)) = (ep.hold_slack, chk_h) {
            assert!(
                (eh - ch).abs() <= TOL,
                "mc_sdc {pname} hold: engine {eh} vs rev checker {ch}"
            );
        }
    }
    let (mut t_json, s_json) = ruststa_check::rev::build_timing_and_scope_json(&rev_mc);
    let ok_out = ruststa_check::rev::check_against_reports(&rev_mc, &t_json, Some(&s_json), TOL);
    assert!(ok_out.verdict.is_agree(), "unmodified report must agree");
    t_json["wns_setup"] = serde_json::json!(99.0);
    let bad_out = ruststa_check::rev::check_against_reports(&rev_mc, &t_json, Some(&s_json), TOL);
    match &bad_out.verdict {
        ruststa_check::rev::Verdict::Disagree { mismatches } => {
            assert!(!mismatches.is_empty());
            assert_eq!(mismatches[0].handle, "mm1");
        }
        other => panic!("expected Disagree on perturbed report, got {other:?}"),
    }

    // 2. Planted defects (`STA-REQ-0504`):
    // (a) Altered report slack is rejected with `verdict == "disagree"`.
    let chain = gen_chain(0x401, 6);
    let chain_sdc = sdc_for_fixture(&chain);
    let sta_opts = ruststa::StaOptions {
        netlist: ruststa::Source::Texts(vec![("chain.v".into(), chain.verilog.clone())]),
        top: None,
        sdc: chain_sdc.clone(),
        paths: 3,
        path_to: Vec::new(),
        backend: ruststa::Backend::Cpu,
        sdc_name: "chain.sdc".into(),
    };
    let rep = ruststa::analyze(&sta_opts, libs).expect("chain analyze");
    let mut bad_json = serde_json::to_value(&rep).expect("json");
    if let Some(arr) = bad_json.get_mut("endpoints").and_then(Value::as_array_mut)
        && let Some(first) = arr.first_mut()
        && let Some(s) = first.get("setup_slack").and_then(Value::as_f64)
    {
        first["setup_slack"] = serde_json::json!(s + 0.050);
    }
    let chk_opts = ruststa_check::CheckStaOptions {
        netlist: ruststa_check::Netlist::Texts(vec![("chain.v".into(), chain.verilog.clone())]),
        top: None,
        sdc: chain_sdc.clone(),
        tolerance: TOL,
    };
    let chk_bad_slack = ruststa_check::check(&chk_opts, libs, &bad_json);
    assert_eq!(
        chk_bad_slack.verdict, "disagree",
        "altered slack must disagree"
    );
    assert!(!chk_bad_slack.differences.is_empty());

    // (b) Scaled cell table in the checker's input library is rejected with `verdict == "disagree"`.
    let mut scaled_lib = wt15.clone();
    for c in &mut scaled_lib.cells {
        for p in &mut c.pins {
            for a in &mut p.arcs {
                if let Some(t) = &mut a.cell_rise {
                    for v in &mut t.values {
                        *v *= 1.25;
                    }
                }
            }
        }
    }
    let chk_scaled = ruststa_check::check(
        &chk_opts,
        std::slice::from_ref(&scaled_lib),
        &serde_json::to_value(&rep).expect("json"),
    );
    assert_eq!(
        chk_scaled.verdict, "disagree",
        "scaled library must disagree"
    );

    // (c) Removed cell arc in the checker's input library is rejected.
    let mut missing_arc_lib = wt15.clone();
    for c in &mut missing_arc_lib.cells {
        if c.name == "wt_buf_1" {
            for p in &mut c.pins {
                p.arcs.clear();
            }
        }
    }
    let chk_missing_arc = ruststa_check::check(
        &chk_opts,
        std::slice::from_ref(&missing_arc_lib),
        &serde_json::to_value(&rep).expect("json"),
    );
    assert_eq!(
        chk_missing_arc.verdict, "disagree",
        "removed arc must disagree"
    );
}

/// Invoked by `cargo xtask verify pdk` to check engine-vs-checker agreement on the PDK-mapped
/// designs generated under `target/verify-pdk`.
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_checker_agreement() {
    let pdk_dir = PathBuf::from(
        std::env::var("RUSTSTA_PDK_VERIFY_DIR").unwrap_or_else(|_| "target/pdk".into()),
    );
    let libs_spec = std::env::var("RUSTSTA_PDK_VERIFY_LIBS").unwrap_or_default();
    let out_dir = PathBuf::from(
        std::env::var("RUSTSTA_PDK_VERIFY_OUT").unwrap_or_else(|_| "target/verify-pdk".into()),
    );

    let entries: Vec<(String, String)> = libs_spec
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|item| {
            let mut parts = item.split(',');
            Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
        })
        .collect();
    assert!(!entries.is_empty(), "no PDK libraries specified");

    let mut parsed_libs = std::collections::BTreeMap::new();
    for (name, file_name) in &entries {
        let lib_path = pdk_dir.join(file_name);
        let (lib, diags) = read_file_diags(&lib_path).expect("read PDK library");
        assert!(diags.is_empty());
        let libs = std::slice::from_ref(&lib);

        let mut compared = 0;
        for design in PDK_DESIGNS {
            let run_dir = out_dir.join(name).join(design);
            let v_path = run_dir.join("netlist.v");
            let sdc_path = run_dir.join("constraints.sdc");
            // The runner writes every design before this test runs: a missing one is a failure.
            let verilog = std::fs::read_to_string(&v_path)
                .unwrap_or_else(|e| panic!("{}: {e}", v_path.display()));
            let sdc = std::fs::read_to_string(&sdc_path)
                .unwrap_or_else(|e| panic!("{}: {e}", sdc_path.display()));
            let label = format!("{name}/{design}");
            if matches!(design, "deep_chain" | "dag" | "two_clock_edges") {
                assert_legacy_and_engine_checker_agree(&label, &verilog, &sdc, libs);
            }
            // The shipped pipeline (STA-REQ-0306) on both PDK families (AGENTS rule 7.2).
            compared += shipped_pipeline_agrees(
                &label,
                &out_dir.join(name).join(design).join("ws-check"),
                &lib_path,
                &verilog,
                &sdc,
            );
        }
        assert!(compared > 0, "{name}: the checker compared no slack");
        parsed_libs.insert(name.clone(), lib);
    }

    // Verify 3-corner sky130 revision equals 3 single-corner runs bitwise when all 3 are present.
    if let (Some(lib_tt), Some(lib_ss), Some(lib_ff)) = (
        parsed_libs.get("sky130hd_tt"),
        parsed_libs.get("sky130hd_ss"),
        parsed_libs.get("sky130hd_ff"),
    ) {
        let dag_dir = out_dir.join("sky130hd_tt").join("dag");
        let v_text = std::fs::read_to_string(dag_dir.join("netlist.v")).expect("dag netlist.v");
        let sdc_text =
            std::fs::read_to_string(dag_dir.join("constraints.sdc")).expect("dag constraints.sdc");
        let frozen = read_frozen("dag", &v_text, std::slice::from_ref(lib_tt));
        let corners_3 = [
            CornerLibs {
                name: "tt",
                libs: std::slice::from_ref(lib_tt),
            },
            CornerLibs {
                name: "ss",
                libs: std::slice::from_ref(lib_ss),
            },
            CornerLibs {
                name: "ff",
                libs: std::slice::from_ref(lib_ff),
            },
        ];
        let ea_multi = EngineAnalysis::analyze_sdc(
            &frozen,
            &corners_3,
            &sdc_text,
            "<sdc>",
            &AnalysisOptions::default(),
            Ctx::NONE,
        )
        .expect("3-corner EngineAnalysis");
        for (idx, cl) in corners_3.iter().enumerate() {
            let ea_single = EngineAnalysis::analyze_sdc(
                &frozen,
                std::slice::from_ref(cl),
                &sdc_text,
                "<sdc>",
                &AnalysisOptions::default(),
                Ctx::NONE,
            )
            .expect("single-corner EngineAnalysis");
            let cm = &ea_multi.corners[idx];
            let cs = &ea_single.corners[0];
            assert_eq!(
                cm.summary().wns_setup_ns.map(f64::to_bits),
                cs.summary().wns_setup_ns.map(f64::to_bits),
                "sky130 3-corner vs single-corner ({}) setup_wns mismatch",
                cl.name
            );
            assert_eq!(
                cm.summary().wns_hold_ns.map(f64::to_bits),
                cs.summary().wns_hold_ns.map(f64::to_bits),
                "sky130 3-corner vs single-corner ({}) hold_wns mismatch",
                cl.name
            );
            for n in 0..ea_multi.graph.num_nodes() {
                let nid = ruststa::engine::types::NodeId(n as u32);
                for tr in [
                    ruststa::engine::types::Tr::Rise,
                    ruststa::engine::types::Tr::Fall,
                ] {
                    for an in [
                        ruststa::engine::types::An::Early,
                        ruststa::engine::types::An::Late,
                    ] {
                        assert_eq!(
                            cm.slew(nid, tr, an).to_bits(),
                            cs.slew(nid, tr, an).to_bits(),
                            "sky130 3-corner vs single-corner ({}) slew mismatch",
                            cl.name
                        );
                    }
                }
            }
        }
    }
}
