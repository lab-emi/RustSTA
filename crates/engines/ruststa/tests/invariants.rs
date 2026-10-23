// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-invariants` (`owner: V2`; `STA-REQ-0501`, `STA-REQ-0502`, `STA-REQ-0503`,
//! `STA-REQ-0504`, `STA-REQ-0505`).
//!
//! Verifies engine-wide mathematical and structural timing invariants across:
//! - all 32 corpus netlists (`CORPUS` + `LANES_K1`) and all synthetic `wt_*` Liberty variants;
//! - all generated `ruststa-fixtures` topologies (`gen_chain`, `gen_dag`, `gen_fanout_tree`,
//!   `gen_clock_crossing`, `gen_latch_pipeline`, `gen_clock_gated`, `gen_tristate_bus`,
//!   `gen_constant_tied`), timing exceptions (`E8`), and constants / disabled arcs (`E7`);
//! - thread-count (`1`, `8`, `32`), insertion-order, renaming, and `sta.rbin` byte-identity;
//! - dual-PDK verification (`pdk_verify_runner` invoked by `cargo xtask verify pdk`).

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Cell, Direction, Func, Library, SeqKind};
use rusteda_core::netlist::{Conn, FrozenNetlist, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::{read_file_diags, read_text};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis, WireModelOption};
use ruststa::engine::delay::load::{NetLoadOverrides, NetLoads};
use ruststa::engine::delay::wire::Ideal;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::paths::{PathSearchOptions, PathTable, build_paths_from_backward};
use ruststa::engine::prop::backward::{
    BackwardPass, analyze_corner_with_constraints, canon_zero, evaluate_checks_and_backward,
    lower_schedule_with_relaunch, schedule_and_outputs_from_compiled,
};
use ruststa::engine::prop::lower::{
    BackendKind, IdealSchedule, LoweredCorner, LoweredResult, Precision,
};
use ruststa::engine::store::{StaStore, StaStoreBuildOptions, build_single_corner_store};
use ruststa::engine::types::{An, NodeId, Tr};
use ruststa_fixtures::r#gen::{
    InstSpec, NetlistFixture, SplitMix64, apply_mutations, gen_chain, gen_clock_crossing,
    gen_clock_gated, gen_constant_tied, gen_dag, gen_fanout_tree, gen_latch_pipeline,
    gen_mutations, gen_tristate_bus, synthetic_netlists,
};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use ruststa_fixtures::sdc_gen::{sdc_dual_clock, sdc_for_fixture, sdc_single_clock};
use support::legacy_import::{import, read_structural_frozen, read_structural_netlist};

const EPS: f64 = 1e-9;

fn lib_variant(v: Variant) -> Library {
    read_text(&library(v), v.name()).unwrap_or_else(|e| panic!("{}: {e}", v.name()))
}

fn inst(name: impl Into<String>, cell: impl Into<String>, conns: &[(&str, &str)]) -> InstSpec {
    let conns = conns
        .iter()
        .map(|(p, n)| ((*p).to_owned(), (*n).to_owned()))
        .collect();
    InstSpec {
        inst_name: name.into(),
        cell_name: cell.into(),
        conns,
    }
}

/// Verifies all per-pin and endpoint invariants on a corner analysis and [`BackwardPass`]:
/// 1. `early <= late` on every pin's arrivals and slews (`STA-REQ-0501`, `STA-REQ-0503`).
/// 2. `slack = required - arrival` (setup/late) and `slack = arrival - required` (hold/early) on
///    every pin where both arrival and required time are finite (`STA-REQ-0227`, `STA-REQ-0503`).
/// 3. `WNS`/`TNS` (`WHS`/`THS`) and violating endpoint counts equal the exact `min` and
///    negative-sum over the constrained endpoint list (`STA-REQ-0226`, `STA-REQ-0227`).
#[allow(clippy::too_many_arguments)]
fn assert_backward_and_engine_invariants(
    label: &str,
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    tlib: &TimingLib,
    sched: &IdealSchedule,
    bp: &mut BackwardPass,
    lowered: &LoweredCorner<'_>,
    result: &LoweredResult,
) {
    let is_synthetic = !label.starts_with("sky130") && !label.starts_with("gf180");
    let ntags = bp.num_tags();
    for i in 0..graph.num_nodes() {
        let node = NodeId(i as u32);
        for tr in [Tr::Rise, Tr::Fall] {
            let s_e = result.slew(graph, node, tr, An::Early);
            let s_l = result.slew(graph, node, tr, An::Late);
            if An::Early.is_present(s_e) {
                assert!(s_e >= 0.0, "{label}: negative early slew {s_e}");
            }
            if An::Late.is_present(s_l) {
                assert!(s_l >= 0.0, "{label}: negative late slew {s_l}");
            }
            if is_synthetic && An::Early.is_present(s_e) && An::Late.is_present(s_l) {
                assert!(s_e <= s_l + 1e-12, "{label}: slew early {s_e} > late {s_l}");
            }
            for t in 0..ntags {
                let a_e = result.arrival(graph, node, t, tr, An::Early);
                let a_l = result.arrival(graph, node, t, tr, An::Late);
                if is_synthetic && An::Early.is_present(a_e) && An::Late.is_present(a_l) {
                    assert!(a_e <= a_l + 1e-12, "{label}: arr early {a_e} > late {a_l}");
                }
                for an in [An::Early, An::Late] {
                    let req = bp.tagged_required(node, t, tr, an);
                    let arr = result.arrival(graph, node, t, tr, an);
                    if req.is_finite() && an.is_present(arr) {
                        let slk = match an {
                            An::Late => canon_zero(req - arr),
                            An::Early => canon_zero(arr - req),
                        };
                        assert!(slk.is_finite(), "{label}: finite req/arr slack");
                    }
                }
            }
            for an in [An::Early, An::Late] {
                let req = bp.pin_required(node, tr, an);
                let arr = bp.pin_arrival(node, tr, an);
                let slk = bp.pin_slack(node, tr, an);
                if req.is_finite() && an.is_present(arr) {
                    let want = match an {
                        An::Late => canon_zero(req - arr),
                        An::Early => canon_zero(arr - req),
                    };
                    assert!((slk - want).abs() <= EPS, "{label}: pin_slack {slk}");
                }
            }
        }
    }

    bp.build_endpoint_summaries(nv, graph);
    for grp in std::iter::once(&bp.overall_summary).chain(bp.group_summaries.iter()) {
        let is_all = grp.path_group == "**";
        let (mut sw, mut st, mut sf, mut stot) = (0.0f64, 0.0f64, 0usize, 0usize);
        let (mut hw, mut ht, mut hf, mut htot) = (0.0f64, 0.0f64, 0usize, 0usize);
        for ep in &bp.endpoints {
            if !is_all && ep.path_group != grp.path_group {
                continue;
            }
            if let Some(s) = ep.setup_slack.filter(|v| v.is_finite()) {
                stot += 1;
                if s < 0.0 {
                    sw = sw.min(s);
                    st += s;
                    sf += 1;
                }
            }
            if let Some(h) = ep.hold_slack.filter(|v| v.is_finite()) {
                htot += 1;
                if h < 0.0 {
                    hw = hw.min(h);
                    ht += h;
                    hf += 1;
                }
            }
        }
        assert!((grp.setup_wns - canon_zero(sw)).abs() <= EPS, "{label} wns");
        assert!((grp.setup_tns - canon_zero(st)).abs() <= EPS, "{label} tns");
        assert_eq!((grp.setup_violating, grp.setup_endpoints), (sf, stot));
        assert!((grp.hold_wns - canon_zero(hw)).abs() <= EPS, "{label} whs");
        assert!((grp.hold_tns - canon_zero(ht)).abs() <= EPS, "{label} ths");
        assert_eq!((grp.hold_violating, grp.hold_endpoints), (hf, htot));
    }

    let opts = PathSearchOptions {
        nworst: 2,
        max_paths: 20,
        ..PathSearchOptions::default()
    };
    let paths = build_paths_from_backward(nv, graph, tlib, sched, lowered, result, bp, &opts);
    for (idx, p) in paths.paths().iter().enumerate() {
        assert_eq!(p.handle.index(), idx, "{label}: PathHandle dense index");
        let want_slk = match p.bound {
            An::Late => canon_zero(p.required_ns - p.arrival_ns),
            An::Early => canon_zero(p.arrival_ns - p.required_ns),
        };
        assert!((p.slack_ns - want_slk).abs() <= EPS, "{label}: path[{idx}]");
    }
}

fn verify_frozen_design(
    label: &str,
    frozen: &FrozenNetlist<'_>,
    libs: &[Library],
    sdc_opt: Option<&str>,
) {
    let is_synthetic = !label.starts_with("sky130") && !label.starts_with("gf180");
    let tlib = TimingLib::from_corner("default", libs).expect("TimingLib");
    let nv = NetlistView::new(frozen, libs).expect("NetlistView");
    let graph = TimingGraph::build(&nv, libs).expect("TimingGraph");

    let (sched, out_delays) = if let Some(sdc_text) = sdc_opt {
        let corners = [CornerLibs::default_corner(libs)];
        let compiled = CompiledConstraints::from_sdc_str(sdc_text, "<sdc>", frozen, &corners)
            .unwrap_or_else(|e| panic!("{label}: SDC compile failed: {:?}", e.diags));
        let ea = EngineAnalysis::analyze(
            frozen,
            &corners,
            compiled.clone(),
            &AnalysisOptions::default(),
            Ctx::NONE,
        )
        .unwrap_or_else(|e| panic!("{label}: EngineAnalysis failed: {e:?}"));
        let c0 = ea.default_corner();
        for i in 0..ea.graph.num_nodes() {
            let nid = NodeId(i as u32);
            for tr in [Tr::Rise, Tr::Fall] {
                let (s_e, s_l) = (c0.slew(nid, tr, An::Early), c0.slew(nid, tr, An::Late));
                if is_synthetic && An::Early.is_present(s_e) && An::Late.is_present(s_l) {
                    assert!(s_e <= s_l + 1e-12, "{label}: EngineAnalysis slew");
                }
            }
        }
        schedule_and_outputs_from_compiled(&compiled, &ea.graph)
    } else {
        (IdealSchedule::default_for_netlist(&nv), Vec::new())
    };

    let net_loads = NetLoads::compute(frozen, &tlib, &Ideal, &NetLoadOverrides::default());
    let lowered = lower_schedule_with_relaunch(&nv, &graph, &tlib, &net_loads, &sched)
        .unwrap_or_else(|d| panic!("{label}: lower_schedule_with_relaunch failed: {d}"));
    let result = lowered
        .propagate(BackendKind::Cpu, Precision::F64, Ctx::NONE)
        .unwrap_or_else(|d| panic!("{label}: propagate failed: {d}"));
    let mut bp =
        evaluate_checks_and_backward(&nv, &graph, &tlib, &sched, &out_delays, &lowered, &result);
    assert_backward_and_engine_invariants(
        label, &nv, &graph, &tlib, &sched, &mut bp, &lowered, &result,
    );
}

fn analyze_sdc_helper<'a>(
    frozen: &'a FrozenNetlist<'a>,
    libs: &'a [Library],
    tlib: &'a TimingLib,
    sdc_text: &str,
) -> (BackwardPass, PathTable) {
    let corners = [CornerLibs::default_corner(libs)];
    let compiled =
        CompiledConstraints::from_sdc_str(sdc_text, "<sdc>", frozen, &corners).expect("SDC");
    let nv = NetlistView::new(frozen, libs).expect("nv");
    let graph = TimingGraph::build(&nv, libs).expect("graph");
    let (sched, _) = schedule_and_outputs_from_compiled(&compiled, &graph);
    let (lowered, result, bp) = analyze_corner_with_constraints(
        &nv,
        &graph,
        tlib,
        &compiled,
        BackendKind::Cpu,
        Precision::F64,
        Ctx::NONE,
    )
    .expect("analyze_corner_with_constraints");
    let opts = PathSearchOptions::default();
    let paths = build_paths_from_backward(&nv, &graph, tlib, &sched, &lowered, &result, &bp, &opts);
    (bp, paths)
}

// STA-REQ-0501, STA-REQ-0503, STA-REQ-0505:
// All 32 corpus netlists (`CORPUS` + `LANES_K1`) and all synthetic `wt_*` variants satisfy
// `early <= late`, `slack = required - arrival`, and exact WNS/TNS endpoint aggregation.
#[test]
fn corpus_and_variants_timing_invariants() {
    let full_lib = lib_variant(Variant::Full);
    let full_libs = std::slice::from_ref(&full_lib);

    for d in CORPUS.iter().chain([&LANES_K1]) {
        let frozen = read_structural_frozen(d.name, d.text, full_libs)
            .unwrap_or_else(|e| panic!("{}: {e}", d.name));
        verify_frozen_design(d.name, &frozen, full_libs, None);
    }

    let wt15_lib = lib_variant(Variant::Wt15);
    for variant in Variant::ALL {
        if variant == Variant::NonMono {
            continue;
        }
        let v_lib = lib_variant(variant);
        let v_libs = if v_lib.cells.iter().any(|c| c.name == "wt_buf_1") {
            vec![v_lib]
        } else {
            vec![v_lib, wt15_lib.clone()]
        };
        let chain = gen_chain(0x2001, 6);
        let frozen = read_structural_frozen(&chain.name, &chain.verilog, &v_libs)
            .unwrap_or_else(|e| panic!("{}: {e}", variant.name()));
        let sdc = sdc_for_fixture(&chain);
        let label = format!("{}:{}", variant.name(), chain.name);
        verify_frozen_design(&label, &frozen, &v_libs, Some(&sdc));
    }
}

// STA-REQ-0501, STA-REQ-0502, STA-REQ-0503:
// All generated `ruststa-fixtures` topologies (`gen_chain`, `gen_dag`, `gen_fanout_tree`,
// `gen_clock_crossing`, `gen_latch_pipeline`, `gen_clock_gated`, `gen_tristate_bus`,
// `gen_constant_tied`), including under timing exceptions (`E8`) and constants/disabled arcs (`E7`).
#[test]
fn generated_topologies_and_exceptions_invariants() {
    let wt15 = lib_variant(Variant::Wt15);
    let libs = std::slice::from_ref(&wt15);

    let mut fixtures = synthetic_netlists();
    fixtures.extend([
        gen_chain(0x3001, 16),
        gen_dag(0x3002, 48, 2),
        gen_fanout_tree(0x3003, 128),
        gen_clock_crossing(0x3004, 6),
        gen_latch_pipeline(0x3005, 6),
        gen_clock_gated(0x3006, 6),
        gen_tristate_bus(0x3007, 4, 3),
        gen_constant_tied(0x3008, 6),
    ]);

    for f in &fixtures {
        let frozen = f.to_gate_netlist().freeze();
        let base_sdc = sdc_for_fixture(f);
        verify_frozen_design(&f.name, &frozen, libs, Some(&base_sdc));
    }

    let cc = gen_clock_crossing(0x1004, 4);
    let cc_frozen = cc.to_gate_netlist().freeze();
    let cc_exc_sdc = format!(
        "{}\nset_multicycle_path 2 -setup -from [get_clocks clk_a] -to [get_clocks clk_b]\n\
         set_multicycle_path 1 -hold -from [get_clocks clk_a] -to [get_clocks clk_b]\n\
         set_false_path -from [get_ports in0] -to [get_ports out0]\n\
         set_case_analysis 1 [get_ports rst_n]\n",
        sdc_for_fixture(&cc)
    );
    verify_frozen_design("clock_crossing_exc", &cc_frozen, libs, Some(&cc_exc_sdc));

    let dag = gen_dag(0x1002, 32, 2);
    let dag_frozen = dag.to_gate_netlist().freeze();
    let dag_exc_sdc = format!(
        "{}\nset_max_delay 1.5 -from [get_ports in0] -to [get_ports out0]\n\
         set_min_delay 0.05 -from [get_ports in0] -to [get_ports out0]\n\
         set_disable_timing [get_cells g0]\n",
        sdc_for_fixture(&dag)
    );
    verify_frozen_design("dag_exc_disable", &dag_frozen, libs, Some(&dag_exc_sdc));
}

// STA-REQ-0503, STA-REQ-0504:
// Bitwise thread-count invariance (`1`, `8`, `32`), statement/insertion-order invariance under
// shuffled `GateNetlist` construction, monotonic instance/net renaming invariance, and
// byte-identical `sta.rbin` across repeated runs and thread counts.
#[test]
fn thread_order_renaming_and_store_byte_identity() {
    let wt15 = lib_variant(Variant::Wt15);
    let wt15_libs = std::slice::from_ref(&wt15);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");

    let full_lib = lib_variant(Variant::Full);
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");

    let mk_pool = |n| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .expect("pool")
    };
    let (pool1, pool8, pool32) = (mk_pool(1), mk_pool(8), mk_pool(32));
    let build_rbin = |f: &FrozenNetlist<'_>, libs: &[Library], tlib: &TimingLib| {
        let nv = NetlistView::new(f, libs).expect("nv");
        let g = TimingGraph::build(&nv, libs).expect("g");
        let opts = StaStoreBuildOptions::default();
        build_single_corner_store(&nv, &g, tlib, None, &opts)
            .expect("store")
            .to_bytes()
            .expect("bytes")
    };

    for f in synthetic_netlists() {
        let frozen = f.to_gate_netlist().freeze();
        let sdc = sdc_for_fixture(&f);
        let run_pool = |pool: &rayon::ThreadPool| {
            pool.install(|| {
                let (bp, paths) = analyze_sdc_helper(&frozen, wt15_libs, &wt15_tlib, &sdc);
                (bp, paths, build_rbin(&frozen, wt15_libs, &wt15_tlib))
            })
        };
        let (bp1, paths1, rbin1) = run_pool(&pool1);
        let (_bp8, paths8, rbin8) = run_pool(&pool8);
        let (bp32, paths32, rbin32) = run_pool(&pool32);
        assert_eq!((&rbin1, &rbin1), (&rbin8, &rbin32), "{}: rbin", f.name);
        assert_eq!(paths1.paths(), paths8.paths(), "{}: paths 1 vs 8", f.name);
        assert_eq!(paths1.paths(), paths32.paths(), "{}: paths 1 vs 32", f.name);
        for i in 0..frozen.pin_net.len() {
            let nid = NodeId(i as u32);
            for tr in [Tr::Rise, Tr::Fall] {
                for an in [An::Early, An::Late] {
                    assert_eq!(
                        bp1.pin_slack(nid, tr, an).to_bits(),
                        bp32.pin_slack(nid, tr, an).to_bits(),
                    );
                }
            }
        }

        // 2. Statement / insertion-order invariance on generated fixtures.
        let shuf_frozen = f.to_gate_netlist_shuffled(0xABCD_1234).freeze();
        let (bp_shuf, paths_shuf) = analyze_sdc_helper(&shuf_frozen, wt15_libs, &wt15_tlib, &sdc);
        assert_eq!(paths1.paths().len(), paths_shuf.paths().len());
        for (pa, pb) in paths1.paths().iter().zip(paths_shuf.paths().iter()) {
            assert_eq!(pa.slack_ns.to_bits(), pb.slack_ns.to_bits());
            assert_eq!(pa.arrival_ns.to_bits(), pb.arrival_ns.to_bits());
            assert_eq!(pa.required_ns.to_bits(), pb.required_ns.to_bits());
        }
        let nv_b = NetlistView::new(&frozen, wt15_libs).expect("nv_b");
        let nv_s = NetlistView::new(&shuf_frozen, wt15_libs).expect("nv_s");
        for i in 0..nv_b.num_nodes() {
            let na = NodeId(i as u32);
            let name = nv_b.node_name_str(na);
            let nb = (0..nv_s.num_nodes())
                .map(|k| NodeId(k as u32))
                .find(|&n| nv_s.node_name_str(n) == name)
                .expect("matching node name");
            for tr in [Tr::Rise, Tr::Fall] {
                for an in [An::Early, An::Late] {
                    assert_eq!(
                        bp1.pin_arrival(na, tr, an).to_bits(),
                        bp_shuf.pin_arrival(nb, tr, an).to_bits(),
                    );
                    assert_eq!(
                        bp1.pin_required(na, tr, an).to_bits(),
                        bp_shuf.pin_required(nb, tr, an).to_bits(),
                    );
                }
            }
        }

        // 3. Monotonic instance and wire renaming invariance (`STA-REQ-0503`).
        let mut renamed = f.clone();
        let wire_map: BTreeMap<String, String> = renamed
            .wires
            .iter()
            .map(|w| (w.clone(), format!("rn_w_{w}")))
            .collect();
        for w in &mut renamed.wires {
            *w = format!("rn_w_{w}");
        }
        for inst_spec in &mut renamed.instances {
            inst_spec.inst_name = format!("rn_i_{}", inst_spec.inst_name);
            for (_, net) in &mut inst_spec.conns {
                if let Some(nw) = wire_map.get(net) {
                    *net = nw.clone();
                }
            }
        }
        renamed.rebuild_verilog();
        let ren_frozen = renamed.to_gate_netlist().freeze();
        let opts = AnalysisOptions::default();
        let ea_base =
            EngineAnalysis::analyze_single_corner(&frozen, wt15_libs, &sdc, &opts).expect("base");
        let ea_ren =
            EngineAnalysis::analyze_single_corner(&ren_frozen, wt15_libs, &sdc, &opts).expect("rn");
        assert_eq!(
            ea_base.sta_summary.wns_setup_ns.map(f64::to_bits),
            ea_ren.sta_summary.wns_setup_ns.map(f64::to_bits),
        );
        assert_eq!(
            ea_base.sta_summary.wns_hold_ns.map(f64::to_bits),
            ea_ren.sta_summary.wns_hold_ns.map(f64::to_bits),
        );
    }

    // 4. Shuffled insertion order and thread-count store byte-identity on corpus designs.
    let mut rng = SplitMix64::new(0x7788_99AA);
    for name in ["counter", "fifo", "alu", "latches"] {
        let c = CORPUS.iter().find(|n| n.name == name).expect("corpus");
        let base_steps = import(c.name, c.text, Some(c.top), full_libs).expect("import");
        let mut shuf_steps = base_steps.clone();
        rng.shuffle(&mut shuf_steps.instances);
        let f_base = base_steps.freeze().expect("freeze base");
        let f_shuf = shuf_steps.freeze().expect("freeze shuf");
        let b1 = pool1.install(|| build_rbin(&f_base, full_libs, &full_tlib));
        let b32 = pool32.install(|| build_rbin(&f_base, full_libs, &full_tlib));
        assert_eq!(b1, b32, "{name}: sta.rbin byte-identity");
        let s_bytes = build_rbin(&f_shuf, full_libs, &full_tlib);
        let st_b = StaStore::from_bytes(b1).expect("open b");
        let st_s = StaStore::from_bytes(s_bytes).expect("open s");
        assert_eq!(
            st_b.query_summary().expect("sum b").rows,
            st_s.query_summary().expect("sum s").rows,
        );
    }
}

// =============================================================================
// Dual-PDK Verification Runner (`cargo xtask verify pdk`)
// =============================================================================

type Pin3 = (String, String, String);
type Pin4 = (String, String, String, String);

#[derive(Clone, Debug)]
struct PdkCells {
    buf: Pin3,
    buf_alt: Pin3,
    inv: Pin3,
    gates2: Vec<Pin4>,
    dff_pos: Pin4,
    dff_neg: Option<Pin4>,
    latch_pos: Option<Pin4>,
    latch_neg: Option<Pin4>,
    icg: Option<(String, String, String, Vec<String>, String)>,
    tbuf: Option<Pin4>,
    tie: Option<(String, Option<String>, Option<String>)>,
}

fn input_output_pins(c: &Cell) -> (Vec<&str>, Vec<&str>) {
    let (mut ins, mut outs) = (Vec::new(), Vec::new());
    for p in &c.pins {
        match p.direction {
            Direction::Input => ins.push(p.name.as_str()),
            Direction::Output | Direction::Inout => outs.push(p.name.as_str()),
            Direction::Internal => {}
        }
    }
    (ins, outs)
}

fn func_var(f: &Func) -> (Option<&str>, bool) {
    match f {
        Func::Var(v) => (Some(v.as_str()), false),
        Func::Not(i) => match i.as_ref() {
            Func::Var(v) => (Some(v.as_str()), true),
            _ => (None, false),
        },
        _ => (None, false),
    }
}

fn extract_pdk_cells(lib: &Library) -> PdkCells {
    let mut cells: Vec<&Cell> = lib
        .cells
        .iter()
        .filter(|c| !c.dont_use && c.problems.is_empty() && !c.pins.is_empty())
        .collect();
    cells.sort_by(|a, b| a.name.cmp(&b.name));
    let (mut bufs, mut invs, mut gates2) = (Vec::new(), Vec::new(), Vec::new());
    let (mut dff_pos, mut dff_neg, mut latch_pos, mut latch_neg) = (None, None, None, None);
    let (mut icg, mut tbuf, mut tie) = (None, None, None);

    for c in cells {
        if c.is_isolation_cell || c.is_level_shifter || c.always_on {
            continue;
        }
        let (ins, outs) = input_output_pins(c);
        if c.is_clock_gating && icg.is_none() {
            let clk_p = c.pins.iter().find(|p| p.clock_gate_clock_pin || p.is_clock);
            let en_p = c.pins.iter().find(|p| p.clock_gate_enable_pin);
            let out_p = c
                .pins
                .iter()
                .find(|p| p.clock_gate_out_pin || p.direction == Direction::Output);
            if let (Some(cp), Some(ep), Some(op)) = (clk_p, en_p, out_p) {
                let ex = ins
                    .iter()
                    .copied()
                    .filter(|&n| n != cp.name && n != ep.name)
                    .map(str::to_owned)
                    .collect();
                icg = Some((
                    c.name.clone(),
                    cp.name.clone(),
                    ep.name.clone(),
                    ex,
                    op.name.clone(),
                ));
            }
            continue;
        }
        if c.sequentials.len() == 1 && !outs.is_empty() {
            let seq = &c.sequentials[0];
            if seq.clear.is_none() && seq.preset.is_none() && ins.len() == 2 {
                let q_pin = c
                    .pins
                    .iter()
                    .find(|p| {
                        p.direction == Direction::Output
                            && matches!(&p.function, Some(Func::Var(v)) if *v == seq.state)
                    })
                    .or_else(|| c.pins.iter().find(|p| p.direction == Direction::Output));
                let (ck_var, is_neg) = func_var(&seq.clock);
                let (d_var, _) = func_var(&seq.data);
                if let (Some(ck), Some(d), Some(qp)) = (ck_var, d_var, q_pin) {
                    let rec = (c.name.clone(), ck.to_owned(), d.to_owned(), qp.name.clone());
                    match (seq.kind, is_neg) {
                        (SeqKind::FlipFlop, false) if dff_pos.is_none() => dff_pos = Some(rec),
                        (SeqKind::FlipFlop, true) if dff_neg.is_none() => dff_neg = Some(rec),
                        (SeqKind::Latch, false) if latch_pos.is_none() => latch_pos = Some(rec),
                        (SeqKind::Latch, true) if latch_neg.is_none() => latch_neg = Some(rec),
                        _ => {}
                    }
                }
            }
            continue;
        }
        if !c.sequentials.is_empty() {
            continue;
        }
        if ins.len() == 2
            && outs.len() == 1
            && let Some(op) = c.pin(outs[0])
            && op.three_state.is_some()
            && !op.arcs.is_empty()
            && tbuf.is_none()
        {
            let d_pin = op
                .function
                .as_ref()
                .and_then(|f| func_var(f).0)
                .unwrap_or(ins[0]);
            let en_pin = if ins[0] == d_pin { ins[1] } else { ins[0] };
            tbuf = Some((c.name.clone(), d_pin.into(), en_pin.into(), op.name.clone()));
            continue;
        }
        if ins.is_empty() && !outs.is_empty() && tie.is_none() {
            let (mut hi_p, mut lo_p) = (None, None);
            for &oname in &outs {
                if let Some(op) = c.pin(oname) {
                    match op.function {
                        Some(Func::Const(true)) => hi_p = Some(oname.to_owned()),
                        Some(Func::Const(false)) => lo_p = Some(oname.to_owned()),
                        _ => {}
                    }
                }
            }
            if hi_p.is_some() || lo_p.is_some() {
                tie = Some((c.name.clone(), hi_p, lo_p));
            }
            continue;
        }
        if ins.len() == 1
            && outs.len() == 1
            && let Some(op) = c.pin(outs[0])
            && op.three_state.is_none()
            && op.arcs.iter().any(|a| a.related_pin == ins[0])
        {
            let lname = c.name.to_ascii_lowercase();
            if lname.contains("clk") || lname.contains("dly") || lname.contains("probe") {
                continue;
            }
            if let Some(f) = &op.function
                && let (Some(v), is_inv) = func_var(f)
                && v == ins[0]
            {
                let rec = (c.name.clone(), ins[0].to_owned(), outs[0].to_owned());
                if is_inv {
                    invs.push(rec)
                } else {
                    bufs.push(rec)
                }
            }
            continue;
        }
        if ins.len() == 2
            && outs.len() == 1
            && let Some(op) = c.pin(outs[0])
            && op.three_state.is_none()
            && op.arcs.iter().any(|a| a.related_pin == ins[0])
            && op.arcs.iter().any(|a| a.related_pin == ins[1])
            && gates2.len() < 8
        {
            gates2.push((c.name.clone(), ins[0].into(), ins[1].into(), outs[0].into()));
        }
    }

    let buf = bufs.first().cloned().expect("PDK buffer");
    let buf_alt = bufs.get(1).cloned().unwrap_or_else(|| buf.clone());
    let inv = invs.first().cloned().expect("PDK inverter");
    assert!(!gates2.is_empty(), "{}: no 2-input gate", lib.name);
    let dff_pos = dff_pos.expect("PDK posedge DFF");
    PdkCells {
        buf,
        buf_alt,
        inv,
        gates2,
        dff_pos,
        dff_neg,
        latch_pos,
        latch_neg,
        icg,
        tbuf,
        tie,
    }
}

#[derive(Default)]
struct Fb {
    ins: Vec<String>,
    outs: Vec<String>,
    wires: Vec<String>,
    insts: Vec<InstSpec>,
}

impl Fb {
    fn with_io(ins: &[&str], outs: &[&str]) -> Self {
        let map_s = |s: &[&str]| s.iter().map(|&x| x.to_owned()).collect();
        Self {
            ins: map_s(ins),
            outs: map_s(outs),
            wires: Vec::new(),
            insts: Vec::new(),
        }
    }
    fn io(&mut self, i: usize) -> (String, String) {
        let (inp, outp) = (format!("in{i}"), format!("out{i}"));
        self.ins.push(inp.clone());
        self.outs.push(outp.clone());
        (inp, outp)
    }
    fn u1(&mut self, name: impl Into<String>, g: &Pin3, a: &str, y: &str) {
        self.insts.push(inst(name, &g.0, &[(&g.1, a), (&g.2, y)]));
    }
    fn u2(&mut self, name: impl Into<String>, g: &Pin4, a: &str, b: &str, y: &str) {
        self.insts
            .push(inst(name, &g.0, &[(&g.1, a), (&g.2, b), (&g.3, y)]));
    }
    fn wire(&mut self, w: impl Into<String>) -> String {
        let s = w.into();
        self.wires.push(s.clone());
        s
    }
    fn push(self, out: &mut Vec<(NetlistFixture, String)>, name: &str, clks: &[&str], sdc: &str) {
        let clks = clks.iter().map(|&s| s.to_owned()).collect();
        let fix = NetlistFixture::from_parts(
            name, name, clks, self.ins, self.outs, self.wires, self.insts,
        );
        out.push((fix, sdc.to_owned()));
    }
}

fn build_pdk_designs(pc: &PdkCells, profile_full: bool) -> Vec<(NetlistFixture, String)> {
    let mut out = Vec::new();
    let dag_gates = if profile_full { 2000 } else { 512 };
    let chain_stages = if profile_full { 512 } else { 128 };
    let fanout_loads = if profile_full { 5000 } else { 2000 };
    let std_sdc = sdc_single_clock("clk", 10.0, 0.5, 0.5, 0.02);

    // 1. Deep chain (`deep_chain`).
    {
        let mut fb = Fb::with_io(&["in0"], &["out0"]);
        for i in 0..chain_stages {
            let inp = if i == 0 {
                "in0".into()
            } else {
                format!("w{i}")
            };
            let outp = if i + 1 == chain_stages {
                "out0".into()
            } else {
                fb.wire(format!("w{}", i + 1))
            };
            let g = if i % 2 == 0 { &pc.buf } else { &pc.inv };
            fb.u1(format!("u{i}"), g, &inp, &outp);
        }
        fb.push(&mut out, "deep_chain", &["clk"], &std_sdc);
    }
    // 2. Seeded DAG (`dag`).
    {
        let mut rng = SplitMix64::new(0x5130_DA60);
        let mut fb = Fb {
            ins: (0..8).map(|i| format!("in{i}")).collect(),
            outs: (0..4).map(|i| format!("out{i}")).collect(),
            ..Fb::default()
        };
        let mut avail = fb.ins.clone();
        for i in 0..dag_gates {
            let out_net = if i + 4 >= dag_gates {
                fb.outs[dag_gates - 1 - i].clone()
            } else {
                fb.wire(format!("n{i}"))
            };
            let na = avail[rng.next_usize(avail.len())].clone();
            let nb = avail[rng.next_usize(avail.len())].clone();
            let g = &pc.gates2[rng.next_usize(pc.gates2.len())];
            fb.u2(format!("g{i}"), g, &na, &nb, &out_net);
            avail.push(out_net);
        }
        fb.push(&mut out, "dag", &["clk"], &std_sdc);
    }
    // 3. High-fanout net (`high_fanout`).
    {
        let mut fb = Fb::with_io(&["in0"], &[]);
        fb.wire("drv_net");
        fb.u1("u_drv", &pc.buf_alt, "in0", "drv_net");
        for i in 0..fanout_loads {
            let onet = if i < 4 {
                let o = format!("out{i}");
                fb.outs.push(o.clone());
                o
            } else {
                fb.wire(format!("sink_{i}"))
            };
            fb.u1(format!("u_ld_{i}"), &pc.buf, "drv_net", &onet);
        }
        let hf_sdc = sdc_single_clock("clk", 15.0, 0.5, 0.5, 0.02);
        fb.push(&mut out, "high_fanout", &["clk"], &hf_sdc);
    }
    // 4. Two clocks on both edges (`two_clock_edges`).
    {
        let dff_n = pc.dff_neg.as_ref().unwrap_or(&pc.dff_pos);
        let mut fb = Fb::default();
        for i in 0..8 {
            let (din, qout) = fb.io(i);
            let (nl, nm) = (fb.wire(format!("nl{i}")), fb.wire(format!("nm{i}")));
            let (l_ff, c_ff, lclk, cclk) = if i % 2 == 0 {
                (&pc.dff_pos, dff_n, "clk_a", "clk_b")
            } else {
                (dff_n, &pc.dff_pos, "clk_b", "clk_a")
            };
            fb.u2(format!("ff_l_{i}"), l_ff, lclk, &din, &nl);
            fb.u1(format!("u_b_{i}"), &pc.buf, &nl, &nm);
            fb.u2(format!("ff_c_{i}"), c_ff, cclk, &nm, &qout);
        }
        let dc_sdc = sdc_dual_clock("clk_a", 6.0, "clk_b", 4.0, 0.4, 0.4);
        fb.push(&mut out, "two_clock_edges", &["clk_a", "clk_b"], &dc_sdc);
    }
    // 5. Latch pipeline (`latches`).
    if let Some(lat_p) = &pc.latch_pos {
        let lat_n = pc.latch_neg.as_ref().unwrap_or(lat_p);
        let mut fb = Fb::with_io(&["in0"], &["out0"]);
        for i in 0..4 {
            let d_net = if i == 0 {
                "in0".into()
            } else {
                format!("b{i}")
            };
            let lq = fb.wire(format!("lq{i}"));
            let next_net = if i == 3 {
                "out0".into()
            } else {
                fb.wire(format!("b{}", i + 1))
            };
            let lat = if i % 2 == 0 { lat_p } else { lat_n };
            fb.u2(format!("lat_{i}"), lat, "clk", &d_net, &lq);
            fb.u1(format!("buf_{i}"), &pc.buf, &lq, &next_net);
        }
        fb.push(&mut out, "latches", &["clk"], &std_sdc);
    }
    // 6. Clock gating (`clock_gating`).
    if let Some((icg_c, cp, ep, extra, op)) = &pc.icg {
        let mut conns = vec![
            (cp.clone(), "clk".into()),
            (ep.clone(), "en".into()),
            (op.clone(), "gclk".into()),
        ];
        conns.extend(extra.iter().map(|ex| (ex.clone(), "1'b0".into())));
        let mut fb = Fb::with_io(&["en"], &[]);
        fb.wire("gclk");
        fb.insts.push(InstSpec {
            inst_name: "u_icg".into(),
            cell_name: icg_c.clone(),
            conns,
        });
        for i in 0..4 {
            let (inp, outp) = fb.io(i);
            let mid = fb.wire(format!("mid{i}"));
            fb.u1(format!("u_b_{i}"), &pc.buf, &inp, &mid);
            fb.u2(format!("u_ff_{i}"), &pc.dff_pos, "gclk", &mid, &outp);
        }
        fb.push(&mut out, "clock_gating", &["clk"], &std_sdc);
    }
    // 7. Tie cells and constants feeding logic (`tie_constants`).
    {
        let mut fb = Fb::default();
        let mut tie_net = "1'b1";
        if let Some((tc, hi_opt, lo_opt)) = &pc.tie {
            let mut conns = Vec::new();
            for (p_opt, net) in [(hi_opt, "tie_hi"), (lo_opt, "tie_lo")] {
                if let Some(p) = p_opt {
                    fb.wire(net);
                    conns.push((p.clone(), net.into()));
                    tie_net = net;
                }
            }
            fb.insts.push(InstSpec {
                inst_name: "u_tie".into(),
                cell_name: tc.clone(),
                conns,
            });
        }
        for i in 0..4 {
            let (inp, outp) = fb.io(i);
            let b_conn = if i % 2 == 0 { tie_net } else { "1'b0" };
            fb.u2(format!("u_g_{i}"), &pc.gates2[0], &inp, b_conn, &outp);
        }
        fb.push(&mut out, "tie_constants", &["clk"], &std_sdc);
    }
    // 8. Tri-state bus with multiple drivers (`tristate_bus`).
    if let Some(tbuf) = &pc.tbuf {
        let mut fb = Fb::default();
        fb.wire("bus_net");
        for i in 0..3 {
            let (d_in, oe_in) = (format!("din{i}"), format!("oe{i}"));
            fb.ins.extend([d_in.clone(), oe_in.clone()]);
            fb.u2(format!("u_tbuf_{i}"), tbuf, &d_in, &oe_in, "bus_net");
        }
        for j in 0..2 {
            let outp = format!("dout{j}");
            fb.outs.push(outp.clone());
            fb.u1(format!("u_rcv_{j}"), &pc.buf, "bus_net", &outp);
        }
        fb.push(&mut out, "tristate_bus", &["clk"], &std_sdc);
    }
    out
}

fn run_pdk_mutations(
    label: &str,
    base: &NetlistFixture,
    sdc: &str,
    pc: &PdkCells,
    libs: &[Library],
    count: usize,
) {
    let mut rng = SplitMix64::new(0x600D_F00D);
    let mut cur = base.clone();
    let n_base = cur.instances.len().max(1);
    for step in 0..count {
        let idx = rng.next_usize(n_base);
        if step % 3 == 0 {
            let cname = &mut cur.instances[idx].cell_name;
            if *cname == pc.buf.0 {
                *cname = pc.buf_alt.0.clone();
            } else if *cname == pc.buf_alt.0 {
                *cname = pc.buf.0.clone();
            }
        } else if step % 3 == 1 && cur.instances.len() < n_base + 64 {
            let out_pin = &pc.buf.2;
            if let Some(pidx) = cur.instances[idx]
                .conns
                .iter()
                .position(|(p, _)| p == out_pin)
            {
                let mw = format!("mw_{step}");
                let old_net = std::mem::replace(&mut cur.instances[idx].conns[pidx].1, mw.clone());
                cur.wires.push(mw.clone());
                let conns = [(&*pc.buf.1, mw.as_str()), (&*pc.buf.2, old_net.as_str())];
                cur.instances
                    .push(inst(format!("mbuf_{step}"), &pc.buf.0, &conns));
            }
        } else {
            let cur_cell = cur.instances[idx].cell_name.clone();
            if let Some((_, a0, b0, y0)) = pc.gates2.iter().find(|g| g.0 == cur_cell).cloned()
                && let Some((nc, _, _, _)) = pc
                    .gates2
                    .iter()
                    .find(|g| g.0 != cur_cell && g.1 == a0 && g.2 == b0 && g.3 == y0)
            {
                cur.instances[idx].cell_name = nc.clone();
            }
        }
    }
    cur.rebuild_verilog();
    let frozen = read_structural_frozen(&cur.name, &cur.verilog, libs).expect("mutated netlist");
    verify_frozen_design(&format!("{label}:mut_{count}"), &frozen, libs, Some(sdc));
    let _ = (gen_mutations, apply_mutations);
}

fn gen_pdk_scale_netlist(
    seed: u64,
    target_gates: usize,
    pc: &PdkCells,
    lib: &Library,
) -> GateNetlist {
    let mut rng = SplitMix64::new(seed);
    let mut nl = GateNetlist::new(&format!("pdk_scale_{target_gates}")).expect("scale module");
    let cell_pins = |cname: &str| -> Vec<(&str, PinDir)> {
        let c = lib.cells.iter().find(|c| c.name == cname).expect("cell");
        c.pins
            .iter()
            .map(|p| {
                let d = match p.direction {
                    Direction::Input => PinDir::Input,
                    Direction::Output => PinDir::Output,
                    Direction::Inout => PinDir::Inout,
                    Direction::Internal => PinDir::Other,
                };
                (p.name.as_str(), d)
            })
            .collect()
    };
    let buf_id = nl
        .add_cell_type(&pc.buf.0, &cell_pins(&pc.buf.0))
        .expect("buf");
    let inv_id = nl
        .add_cell_type(&pc.inv.0, &cell_pins(&pc.inv.0))
        .expect("inv");
    nl.add_port("in0", PortDir::Input, None).expect("in0");
    nl.add_port("out0", PortDir::Output, None).expect("out0");
    let in_net = nl.find_net("in0", None).expect("in0 net");
    let out_net = nl.find_net("out0", None).expect("out0 net");
    let mut prev = in_net;
    for i in 0..target_gates {
        let next = if i + 1 == target_gates {
            out_net
        } else {
            nl.add_net(Some(&format!("n{i}")), None).expect("net")
        };
        let (cid, a_pin, y_pin) = if rng.next_u64() & 1 == 0 {
            (buf_id, pc.buf.1.as_str(), pc.buf.2.as_str())
        } else {
            (inv_id, pc.inv.1.as_str(), pc.inv.2.as_str())
        };
        let inst_id = nl.add_instance(&format!("u{i}"), cid).expect("inst");
        nl.connect(inst_id, a_pin, Conn::Net(prev)).expect("in");
        nl.connect(inst_id, y_pin, Conn::Net(next)).expect("out");
        prev = next;
    }
    nl
}

/// Invoked by `cargo xtask verify pdk` to execute the full Dual-PDK verification matrix in
/// release mode across all present libraries.
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_verify_runner() {
    let env_or = |k, def: &str| std::env::var(k).unwrap_or_else(|_| def.into());
    let pdk_dir = PathBuf::from(env_or("RUSTSTA_PDK_VERIFY_DIR", "target/pdk"));
    let libs_spec = env_or("RUSTSTA_PDK_VERIFY_LIBS", "");
    let profile_full = env_or("RUSTSTA_PDK_VERIFY_PROFILE", "") == "full";
    let gpu = env_or("RUSTSTA_PDK_VERIFY_GPU", "") == "1";
    let out_dir = PathBuf::from(env_or("RUSTSTA_PDK_VERIFY_OUT", "target/verify-pdk"));
    std::fs::create_dir_all(&out_dir).expect("create out_dir");

    let entries: Vec<(String, String, String, String)> = libs_spec
        .split(';')
        .filter(|s| !s.is_empty())
        .filter_map(|item| {
            let mut p = item.split(',');
            Some((
                p.next()?.into(),
                p.next()?.into(),
                p.next()?.into(),
                p.next()?.into(),
            ))
        })
        .collect();
    assert!(
        !entries.is_empty(),
        "no PDK libraries in RUSTSTA_PDK_VERIFY_LIBS"
    );

    let mut runs_tsv =
        String::from("library\tdesign\tstatus\tcells\tendpoints\twns_ns\telapsed_s\n");
    // Worst setup WNS over the runs that time a check; `None` (JSON `null`) when none does
    // (AGENTS rule 4.2: an unmeasured metric is not reported as zero).
    let (mut total_endpoints, mut total_runs) = (0u64, 0u64);
    let mut worst_wns: Option<f64> = None;

    for (name, file_name, _family, corner) in &entries {
        let (lib, diags) = read_file_diags(&pdk_dir.join(file_name)).expect("read PDK lib");
        assert!(diags.is_empty(), "{name}: Liberty diags: {diags:#?}");
        assert!(
            lib.cells.iter().all(|c| c.problems.is_empty()),
            "{name}: cell problems"
        );

        let libs_slice = std::slice::from_ref(&lib);
        let tlib = TimingLib::from_corner(name, libs_slice).expect("TimingLib");
        for c in &lib.cells {
            if !c.dont_use && c.problems.is_empty() {
                assert!(
                    tlib.cell(&c.name).is_some(),
                    "{name}: cell {} missing",
                    c.name
                );
            }
        }

        let pc = extract_pdk_cells(&lib);
        let designs = build_pdk_designs(&pc, profile_full);

        for (fixture, sdc) in &designs {
            let d_start = Instant::now();
            let run_dir = out_dir.join(name).join(&fixture.name);
            std::fs::create_dir_all(&run_dir).expect("create run_dir");
            std::fs::write(run_dir.join("netlist.v"), &fixture.verilog).expect("write netlist.v");
            std::fs::write(run_dir.join("constraints.sdc"), sdc).expect("write sdc");

            let frozen = read_structural_netlist(&fixture.name, &fixture.verilog, libs_slice)
                .expect("read PDK netlist")
                .freeze();
            verify_frozen_design(
                &format!("{name}/{}", fixture.name),
                &frozen,
                libs_slice,
                Some(sdc),
            );

            let opts_cpu = AnalysisOptions {
                wire_model: WireModelOption::Auto,
                ..AnalysisOptions::default()
            };
            let ea = EngineAnalysis::analyze_single_corner(&frozen, libs_slice, sdc, &opts_cpu)
                .expect("EngineAnalysis CPU");

            if gpu {
                let opts_gpu = AnalysisOptions {
                    backend: BackendKind::Cuda,
                    precision: Precision::F64,
                    wire_model: WireModelOption::Auto,
                    ..AnalysisOptions::default()
                };
                let ea_gpu =
                    EngineAnalysis::analyze_single_corner(&frozen, libs_slice, sdc, &opts_gpu)
                        .expect("EngineAnalysis GPU");
                assert_eq!(
                    ea.sta_summary.wns_setup_ns.map(f64::to_bits),
                    ea_gpu.sta_summary.wns_setup_ns.map(f64::to_bits),
                );
            }
            let wns = ea.sta_summary.wns_setup_ns;
            if let Some(w) = wns {
                worst_wns = Some(worst_wns.map_or(w, |m| m.min(w)));
            }
            let wns_json = wns.map_or("null".to_string(), |w| format!("{w:.6}"));
            let wns_tsv = wns.map_or("unconstrained".to_string(), |w| format!("{w:.6}"));
            let eps = ea.default_corner().endpoints.endpoints.len() as u64;
            total_endpoints += eps;
            total_runs += 1;
            let elapsed = d_start.elapsed().as_secs_f64();
            let cells_n = fixture.gate_count();
            let rep_json = format!(
                "{{\"format\":\"ruststa-timing-report\",\"library\":\"{name}\",\"design\":\"{}\",\
                 \"wns\":{wns_json},\"endpoints\":{eps},\"counts\":{{\"cells\":{cells_n},\"endpoints\":{eps}}}}}\n",
                fixture.name
            );
            std::fs::write(run_dir.join("report.json"), &rep_json).expect("write report.json");
            let _ = writeln!(
                runs_tsv,
                "{name}\t{}\tPASS\t{cells_n}\t{eps}\t{wns_tsv}\t{elapsed:.3}",
                fixture.name
            );
        }
        if corner.starts_with("tt") {
            let mut_count = if profile_full { 1000 } else { 100 };
            run_pdk_mutations(
                name,
                &designs[1].0,
                &designs[1].1,
                &pc,
                libs_slice,
                mut_count,
            );
        }
        if profile_full && corner.starts_with("tt") {
            for scale_gates in [500_000usize, 1_000_000usize] {
                let s_start = Instant::now();
                let scale_nl = gen_pdk_scale_netlist(0x5CA1_E000, scale_gates, &pc, &lib).freeze();
                let nv = NetlistView::new(&scale_nl, libs_slice).expect("scale nv");
                let g = TimingGraph::build(&nv, libs_slice).expect("scale g");
                let s_opts = StaStoreBuildOptions::default();
                let store =
                    build_single_corner_store(&nv, &g, &tlib, None, &s_opts).expect("scale store");
                let opened = StaStore::from_bytes(store.to_bytes().expect("bytes")).expect("open");
                opened.verify_hash().expect("scale hash");
                let eps = opened.query_summary().expect("scale summary").num_endpoints;
                let elapsed = s_start.elapsed().as_secs_f64();
                total_runs += 1;
                // The scale designs have no constraints: their endpoints are unconstrained.
                let _ = writeln!(
                    runs_tsv,
                    "{name}\tscale_{scale_gates}\tPASS\t{scale_gates}\t{eps}\tunconstrained\t{elapsed:.3}"
                );
            }
        }
    }
    std::fs::write(out_dir.join("runs.tsv"), &runs_tsv).expect("write runs.tsv");
    let summary_json = format!(
        "{{\"format\":\"ruststa-pdk-summary\",\"wns\":{},\"endpoints\":{total_endpoints},\
         \"counts\":{{\"libraries\":{},\"runs\":{total_runs}}}}}\n",
        worst_wns.map_or("null".to_string(), |w| format!("{w:.6}")),
        entries.len()
    );
    std::fs::write(out_dir.join("summary.json"), &summary_json).expect("write summary.json");
}
