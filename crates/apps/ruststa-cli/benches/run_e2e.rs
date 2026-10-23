// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! End-to-end workspace benchmark (`benches/run_e2e.rs`, Rule 8.3): builds workspace revisions at
//! 100k, 500k, and 1M cells (or `RUSTSTA_BENCH_CELLS=100000,500000,1000000`), runs end-to-end
//! `ruststa` timing analysis and store commit, and reports phase timings (`load_inputs`,
//! `analyze`, `store_build`, `commit`, `query_path`, `query_inspect`, `total`).

#![allow(clippy::print_stdout)]

use std::time::Instant;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::Library;
use rusteda_core::netlist::{Conn, FrozenNetlist, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc::{self, Env, Limits, Source};
use rusteda_workspace::{Invocation, JsonMap, RbinWriter, Upstream, Workspace, rlib};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis, WireModelOption};
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::prop::lower::{BackendKind, Precision};
use ruststa::engine::prop::tags::DEFAULT_TAG_BUDGET;
use ruststa::engine::store::{StoreMode, StorePrecision};
use ruststa_cli::args::{Format, InspectArgs, PathArgs, PathKind};
use ruststa_cli::store_build::build_store_from_analysis;
use ruststa_fixtures::liberty::{Variant, library};

fn build_synthetic_netlist(num_cells: usize) -> FrozenNetlist<'static> {
    let mut nl = GateNetlist::new("top").expect("module top");
    let buf_id = nl
        .add_cell_type("wt_buf_x1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .expect("wt_buf_x1");
    let dff_id = nl
        .add_cell_type(
            "wt_dffq_x1",
            &[
                ("CK", PinDir::Input),
                ("D", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .expect("wt_dffq_x1");

    nl.add_port("clk", PortDir::Input, None).expect("clk");
    nl.add_port("in0", PortDir::Input, None).expect("in0");
    nl.add_port("out0", PortDir::Output, None).expect("out0");

    let clk_net = nl.find_net("clk", None).expect("clk net");
    let in_net = nl.find_net("in0", None).expect("in0 net");
    let out_net = nl.find_net("out0", None).expect("out0 net");

    let stage_len = 8usize;
    let mut prev_net = in_net;
    for i in 0..num_cells {
        let next_net = if i + 1 == num_cells {
            out_net
        } else {
            nl.add_net(Some(&format!("n{i}")), None)
                .expect("scalar net")
        };
        if i % stage_len == stage_len - 1 {
            let inst = nl.add_instance(&format!("u{i}"), dff_id).expect("add DFF");
            nl.connect(inst, "CK", Conn::Net(clk_net)).expect("CK");
            nl.connect(inst, "D", Conn::Net(prev_net)).expect("D");
            nl.connect(inst, "Q", Conn::Net(next_net)).expect("Q");
        } else {
            let inst = nl.add_instance(&format!("u{i}"), buf_id).expect("add BUF");
            nl.connect(inst, "A", Conn::Net(prev_net)).expect("A");
            nl.connect(inst, "Y", Conn::Net(next_net)).expect("Y");
        }
        prev_net = next_net;
    }
    nl.freeze()
}

fn run_for_size(num_cells: usize, lib: &Library, lib_text: &str) {
    let tmp = std::env::temp_dir().join(format!(
        "ruststa-bench-e2e-{}-{num_cells}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);

    let inv_init = Invocation::new(
        "rusteda",
        "0.2.0",
        "rusteda init",
        vec!["rusteda".to_string()],
    );
    let init = Workspace::init(&tmp, &inv_init).expect("init workspace");
    let ws = init.workspace;

    let cached = rlib::put(&ws, lib, lib_text.as_bytes()).expect("rlib put");
    let frozen = build_synthetic_netlist(num_cells);
    let sdc_text = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                    set_input_delay -clock clk 0.1 [get_ports in0]\n\
                    set_output_delay -clock clk 0.1 [get_ports out0]\n";
    let (sdc_model, _) = sdc::read(
        &[Source::new("bench.sdc", sdc_text.to_string())],
        &Env::default(),
        &Limits::default(),
    );

    let base0 = ws.resolve_base("rev0").expect("rev0");
    let mut txn0 = ws.begin(base0, &inv_init).expect("begin rev1");
    txn0.add_library(cached.entry("default", "wt_full.lib"))
        .expect("add library");
    rusteda_workspace::netlist::add_to_txn(&mut txn0, &frozen, Upstream::new())
        .expect("write netlist");
    let sections = sdc_model.to_sections();
    let mut writer = RbinWriter::new("CONSTR", 1).expect("writer");
    for sec in &sections {
        writer
            .section_bytes(u32::from(sec.id), sec.elem_size, sec.bytes.as_slice())
            .expect("section");
    }
    txn0.add_rbin("constraints", "constraints.rbin", &writer, Upstream::new())
        .expect("write constraints");
    let committed1 = txn0.commit(JsonMap::new()).expect("commit rev1");

    let t_total = Instant::now();
    let t_load = Instant::now();
    let rev1 = ws.revision(committed1.rev).expect("open rev1");
    let nl_rbin = rusteda_workspace::netlist::open_rev(&rev1).expect("open netlist");
    let nl_view = rusteda_workspace::netlist::decode(&nl_rbin).expect("decode netlist");
    let (sdc_loaded, _) = ruststa_cli::ws_io::load_sdc_from_revision(&rev1).expect("load sdc");
    let mut diags = Vec::new();
    let corner_libs =
        ruststa_cli::ws_io::load_corner_libraries(&ws, &rev1, &[], &mut diags).expect("load libs");
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;

    let t_analyze = Instant::now();
    let corner_refs: Vec<CornerLibs<'_>> = corner_libs
        .iter()
        .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
        .collect();
    let compiled = CompiledConstraints::compile(&sdc_loaded, &nl_view, &corner_refs)
        .expect("compile constraints");
    let opts = AnalysisOptions {
        backend: BackendKind::Cpu,
        precision: Precision::F64,
        wire_model: WireModelOption::Auto,
        tag_budget: DEFAULT_TAG_BUDGET,
        ..AnalysisOptions::default()
    };
    let analysis = EngineAnalysis::analyze(&nl_view, &corner_refs, compiled, &opts, Ctx::NONE)
        .expect("analyze");
    let analyze_ms = t_analyze.elapsed().as_secs_f64() * 1000.0;

    let t_store = Instant::now();
    let store_data = build_store_from_analysis(
        &analysis,
        &corner_libs,
        "cpu",
        StorePrecision::F64,
        StoreMode::Full,
        100,
    );
    let store_build_ms = t_store.elapsed().as_secs_f64() * 1000.0;

    let t_commit = Instant::now();
    let base1 = ws.resolve_base("rev1").expect("resolve rev1");
    let inv_run = Invocation::new(
        "ruststa",
        "0.2.0",
        "ruststa run",
        vec!["ruststa".to_string()],
    );
    let mut txn1 = ws.begin(base1, &inv_run).expect("begin rev2");
    store_data
        .stage_to_txn(&mut txn1, Upstream::new())
        .expect("write sta.rbin");
    let _committed2 = txn1.commit(JsonMap::new()).expect("commit rev2");
    let commit_ms = t_commit.elapsed().as_secs_f64() * 1000.0;

    let t_path = Instant::now();
    let path_args = PathArgs {
        rev: "HEAD".to_string(),
        targets: vec!["path1".to_string()],
        to: Vec::new(),
        kind: PathKind::Both,
        corner: None,
        exact: false,
    };
    let ws_str = tmp.display().to_string();
    let _ = ruststa_cli::path::exec_path(&ws, &ws_str, &path_args, &mut diags).expect("path1");
    let query_path_ms = t_path.elapsed().as_secs_f64() * 1000.0;

    let t_insp = Instant::now();
    let insp_args = InspectArgs {
        rev: "HEAD".to_string(),
        targets: vec!["inst1".to_string()],
        inst: None,
        net: None,
        pin: None,
        corner: None,
    };
    let _ =
        ruststa_cli::inspect::exec_inspect(&ws, &ws_str, &insp_args, &mut diags).expect("inst1");
    let query_inspect_ms = t_insp.elapsed().as_secs_f64() * 1000.0;
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    let _ = Format::Json;
    println!(
        "cells={num_cells} load_inputs_ms={load_ms:.2} analyze_ms={analyze_ms:.2} \
         store_build_ms={store_build_ms:.2} commit_ms={commit_ms:.2} \
         query_path_ms={query_path_ms:.3} query_inspect_ms={query_inspect_ms:.3} total_ms={total_ms:.2}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

fn main() {
    let lib_text = library(Variant::Rich);
    let lib = read_text(&lib_text, "wt_rich.lib").expect("parse liberty");
    let sizes: Vec<usize> = match std::env::var("RUSTSTA_BENCH_CELLS") {
        Ok(raw) => raw
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .collect(),
        Err(_) => vec![100_000, 500_000, 1_000_000],
    };
    for n in sizes {
        run_for_size(n, &lib, &lib_text);
    }
}
