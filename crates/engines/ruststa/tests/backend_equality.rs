// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-backends` (owner: C2; `STA-REQ-0400` to `STA-REQ-0408`, `CLI-REQ-0016`,
//! `COMPUTE-REQ-0010` to `COMPUTE-REQ-0029`):
//!
//! - level-parallel CPU propagation via `rusteda_compute::timing` is bitwise identical across
//!   thread counts (`1`, `3`, `8`) on the 32 corpus netlists and on generated three-state,
//!   latch-pipeline, clock-crossing, clock-gated and constant-tied designs;
//! - CUDA `f64` propagation is bitwise identical (`to_bits() == to_bits()`) to CPU `f64` on all
//!   32 corpus netlists, generated three-state netlists, latches and two clocks, and repeated GPU
//!   runs are bitwise identical;
//! - CUDA `f32` propagation agrees with CPU `f64` within the `STA-REQ-0403` envelope and is
//!   bitwise identical across repeated GPU runs;
//! - background GPU preparation (`with_prewarm`) overlaps with host graph construction;
//! - uncompiled/unavailable runtimes report `A0501` (`BACKEND_UNAVAILABLE`), and the retired
//!   legacy `ruststa::run` GPU path returns an input error naming `ruststa run`.

mod support;

use std::collections::HashMap;

use rusteda_core::job::Ctx;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::liberty::{Library, read_text};
use ruststa::engine::delay::load::{NetLoadOverrides, NetLoads};
use ruststa::engine::delay::wire::Ideal;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::prop::lower::{
    BackendKind, ComputeError, IdealClockSpec, IdealSchedule, InputDelaySpec, LoweredCorner,
    LoweredResult, NO_PRED, Precision, compute_error_code, compute_error_diag,
    is_backend_unavailable, prewarm_backend, with_prewarm,
};
use ruststa::engine::types::{An, NodeId, Tr};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::net_gen::{
    gen_clock_crossing, gen_clock_gated, gen_constant_tied, gen_latch_pipeline, gen_tristate_bus,
};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use support::legacy_import::read_structural_frozen;

fn wt_full() -> Library {
    read_text(&library(Variant::Full), Variant::Full.name()).expect("wt_full library")
}

fn wt_15() -> Library {
    read_text(&library(Variant::Wt15), Variant::Wt15.name()).expect("wt_15 library")
}

fn ctx() -> Ctx<'static> {
    Ctx::new(None, None)
}

fn lower_graph_and_corner<'a>(
    frozen: &FrozenNetlist<'_>,
    libs: &[Library],
    tlib: &'a TimingLib,
    sched_override: Option<IdealSchedule>,
) -> (TimingGraph, LoweredCorner<'a>) {
    let nv = NetlistView::new(frozen, libs).expect("netlist view");
    let graph = TimingGraph::build(&nv, libs).expect("timing graph");
    let loads = NetLoads::compute(frozen, tlib, &Ideal, &NetLoadOverrides::default());
    let sched = sched_override.unwrap_or_else(|| IdealSchedule::default_for_netlist(&nv));
    let lowered = sched
        .lower(&nv, &graph, tlib, &loads)
        .expect("lower corner");
    (graph, lowered)
}

/// Asserts that `a` and `b` have bitwise identical arrivals, transitions, and predecessors
/// (`STA-REQ-0403`, `STA-REQ-0405`, `COMPUTE-REQ-0019`).
fn assert_bitwise_equal(label: &str, a: &LoweredResult, b: &LoweredResult) {
    assert_eq!(a.values.arr.len(), b.values.arr.len(), "{label}: arr len");
    assert_eq!(
        a.values.slew.len(),
        b.values.slew.len(),
        "{label}: slew len"
    );
    assert_eq!(
        a.values.pred.len(),
        b.values.pred.len(),
        "{label}: pred len"
    );
    for (i, (&x, &y)) in a.values.arr.iter().zip(&b.values.arr).enumerate() {
        assert_eq!(
            x.to_bits(),
            y.to_bits(),
            "{label}: arr[{i}] differs: {x:.15} ({:#x}) vs {y:.15} ({:#x})",
            x.to_bits(),
            y.to_bits()
        );
    }
    for (i, (&x, &y)) in a.values.slew.iter().zip(&b.values.slew).enumerate() {
        assert_eq!(
            x.to_bits(),
            y.to_bits(),
            "{label}: slew[{i}] differs: {x:.15} ({:#x}) vs {y:.15} ({:#x})",
            x.to_bits(),
            y.to_bits()
        );
    }
    for (i, (&x, &y)) in a.values.pred.iter().zip(&b.values.pred).enumerate() {
        assert_eq!(x, y, "{label}: pred[{i}] differs: {x:#x} vs {y:#x}");
    }
}

/// Asserts that single-precision GPU values `f32_res` lie within the `STA-REQ-0403` /
/// `COMPUTE-REQ-0019` envelope of `cpu_res` (`1e-4 + 2e-5 * |cpu|`), allowing boundary-edge
/// discrepancies only when a latch tag-map window is present on the design.
fn assert_f32_envelope(
    label: &str,
    cpu_res: &LoweredResult,
    f32_res: &LoweredResult,
    has_latch_window: bool,
) {
    assert_eq!(cpu_res.values.arr.len(), f32_res.values.arr.len());
    assert_eq!(cpu_res.values.slew.len(), f32_res.values.slew.len());
    for (kind, c_slice, g_slice) in [
        ("arr", &cpu_res.values.arr, &f32_res.values.arr),
        ("slew", &cpu_res.values.slew, &f32_res.values.slew),
    ] {
        for (i, (&c, &g)) in c_slice.iter().zip(g_slice.iter()).enumerate() {
            if c.is_infinite() || g.is_infinite() {
                if !has_latch_window {
                    assert_eq!(
                        c, g,
                        "{label}: {kind}[{i}] absent in one precision only: cpu {c} vs f32 {g}"
                    );
                }
                continue;
            }
            let diff = (c - g).abs();
            let tol = 1e-4 + 2e-5 * c.abs();
            assert!(
                diff <= tol,
                "{label}: {kind}[{i}] cpu {c:.9} vs f32 {g:.9} (diff {diff:.3e} > tol {tol:.3e})"
            );
        }
    }
}

fn find_port_node(nv: &NetlistView<'_>, name: &str) -> NodeId {
    for i in 0..nv.num_port_bits() {
        let node = NodeId(i as u32);
        if nv.node_name_str(node) == name {
            return node;
        }
    }
    panic!("port {name} not found");
}

// STA-REQ-0400, STA-REQ-0401, STA-REQ-0405, CLI-REQ-0016: level-parallel CPU propagation is
// bit-identical to the 1-thread scalar reference across 1, 3 and 8 threads on all 32 corpus
// netlists and on generated three-state, latch-pipeline, clock-crossing, clock-gated and
// constant-tied netlists.
#[test]
fn parallel_cpu_matches_scalar_on_corpus_and_tristate() {
    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");

    let mut corpus_count = 0usize;
    for d in CORPUS.iter().chain([&LANES_K1]) {
        let frozen = read_structural_frozen(d.name, d.text, full_libs)
            .unwrap_or_else(|e| panic!("{}: {e}", d.name));
        let (graph, lowered) = lower_graph_and_corner(&frozen, full_libs, &full_tlib, None);
        let pool1 = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("pool 1");
        let ref_res = pool1
            .install(|| lowered.propagate(BackendKind::Cpu, Precision::F64, ctx()))
            .expect("cpu 1 thread");
        assert_eq!(ref_res.stats.backend, BackendKind::Cpu);

        // Verify NodeId accessors and to_node_order on the first few nodes.
        for &node in graph.order().iter().take(4) {
            let s_r = ref_res.slew(&graph, node, Tr::Rise, An::Late);
            let a_r = ref_res.arrival(&graph, node, 0, Tr::Rise, An::Late);
            assert!(s_r.is_finite() || s_r.is_infinite());
            assert!(a_r.is_finite() || a_r.is_infinite());
            if let Some((pred_node, _)) = ref_res.predecessor(&graph, node, 0, Tr::Rise, An::Late) {
                assert!(graph.level(pred_node) < graph.level(node));
            }
        }
        let node_ord = ref_res.to_node_order(&graph);
        assert_eq!(node_ord.arr.len(), ref_res.values.arr.len());

        for threads in [3, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let par_res = pool
                .install(|| lowered.propagate(BackendKind::Cpu, Precision::F64, ctx()))
                .expect("cpu parallel");
            assert_bitwise_equal(
                &format!("{} ({threads} threads)", d.name),
                &ref_res,
                &par_res,
            );
        }
        corpus_count += 1;
    }
    assert_eq!(corpus_count, 32);

    // Generated three-state and sequential topologies on `wt_15`.
    let wt15_lib = wt_15();
    let wt15_libs = std::slice::from_ref(&wt15_lib);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");
    let generated = [
        gen_tristate_bus(101, 4, 3),
        gen_tristate_bus(202, 8, 6),
        gen_latch_pipeline(303, 4),
        gen_clock_crossing(404, 4),
        gen_clock_gated(505, 4),
        gen_constant_tied(606, 4),
    ];
    for fix in &generated {
        let frozen = fix.to_gate_netlist().freeze();
        let (_graph, lowered) = lower_graph_and_corner(&frozen, wt15_libs, &wt15_tlib, None);
        let pool1 = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("pool 1");
        let ref_res = pool1
            .install(|| lowered.propagate(BackendKind::Cpu, Precision::F64, ctx()))
            .expect("cpu 1 thread");
        for threads in [3, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let par_res = pool
                .install(|| lowered.propagate(BackendKind::Cpu, Precision::F64, ctx()))
                .expect("cpu parallel");
            assert_bitwise_equal(
                &format!("{} ({threads} threads)", fix.name),
                &ref_res,
                &par_res,
            );
        }
    }
    let _ = NO_PRED;
}

// STA-REQ-0404, STA-REQ-0408: requesting an uncompiled GPU backend maps to A0501
// (`BACKEND_UNAVAILABLE`), error codes A0501..A0506 map deterministically, and the retired
// legacy `ruststa::run` GPU path fails with an input error naming `ruststa run`.
#[test]
fn missing_runtime_and_legacy_gpu_stub_are_errors() {
    assert_eq!(
        compute_error_code(&ComputeError::NotCompiled("cuda")),
        "A0501"
    );
    assert_eq!(
        compute_error_code(&ComputeError::Unavailable("no device".into())),
        "A0501"
    );
    assert_eq!(
        compute_error_code(&ComputeError::TooLarge("overflow".into())),
        "A0502"
    );
    assert_eq!(
        compute_error_code(&ComputeError::Compile("nvrtc".into())),
        "A0503"
    );
    assert_eq!(
        compute_error_code(&ComputeError::Device("launch".into())),
        "A0504"
    );
    assert_eq!(
        compute_error_code(&ComputeError::Invalid("bad csr".into())),
        "A0505"
    );
    assert_eq!(compute_error_code(&ComputeError::Cancelled), "A0506");

    let diag = compute_error_diag(&ComputeError::NotCompiled("cuda"));
    assert_eq!(diag.code, "A0501");
    assert!(is_backend_unavailable(&ComputeError::NotCompiled("cuda")));
    assert!(!is_backend_unavailable(&ComputeError::Invalid(
        "bad".into()
    )));

    // CPU prewarm always succeeds.
    let (v, prep) = with_prewarm(BackendKind::Cpu, Precision::F64, || 7usize);
    assert_eq!(v, 7);
    assert!(prep.is_ok());

    if !cfg!(feature = "cuda") {
        let wt15_lib = wt_15();
        let wt15_libs = std::slice::from_ref(&wt15_lib);
        let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");
        let fix = gen_tristate_bus(1, 2, 2);
        let frozen = fix.to_gate_netlist().freeze();
        let (_graph, lowered) = lower_graph_and_corner(&frozen, wt15_libs, &wt15_tlib, None);
        for kind in [BackendKind::Cuda, BackendKind::Gpu] {
            let err = lowered
                .propagate(kind, Precision::F64, ctx())
                .expect_err("uncompiled GPU backend must fail");
            assert!(is_backend_unavailable(&err), "unexpected error: {err}");
            assert_eq!(compute_error_code(&err), "A0501");
            let p_err = prewarm_backend(kind, Precision::F64).expect_err("prewarm must fail");
            assert!(is_backend_unavailable(&p_err));
            let (val, w_res) = with_prewarm(kind, Precision::F64, || 99u32);
            assert_eq!(val, 99);
            assert!(is_backend_unavailable(&w_res.unwrap_err()));
        }
    }

    // Legacy `ruststa::run` with `Backend::Cuda` or `Backend::Gpu` always returns an input error
    // naming `ruststa run` (safe stub replacing legacy `src/gpu/**`).
    let full_lib = wt_full();
    let vlog = "module m (input a, output y); wtf_inv_1 u (.A(a), .Y(y)); endmodule\n";
    for backend in [ruststa::Backend::Cuda, ruststa::Backend::Gpu] {
        let opts = ruststa::StaOptions {
            netlist: ruststa::Source::Texts(vec![("m.v".into(), vlog.into())]),
            top: None,
            sdc: String::new(),
            paths: 1,
            path_to: Vec::new(),
            backend,
            sdc_name: String::new(),
        };
        let err = ruststa::run(&opts, std::slice::from_ref(&full_lib))
            .expect_err("legacy GPU backend must be rejected");
        assert!(!err.internal, "legacy GPU error must be an input error");
        assert!(
            err.message.contains("ruststa run") || err.message.contains("runtime"),
            "unexpected message: {}",
            err.message
        );
    }
}

// STA-REQ-0400, STA-REQ-0401, STA-REQ-0402, STA-REQ-0403, STA-REQ-0406, STA-REQ-0407,
// STA-REQ-0408: CUDA f64 propagation is bitwise identical to CPU f64 across all 32 corpus
// netlists and generated three-state/sequential netlists, and CUDA f32 lies within the
// STA-REQ-0403 envelope with bitwise-identical reruns.
#[test]
#[ignore]
fn gpu_matches_cpu_on_corpus_and_tristate() {
    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");

    // Exercise background preparation overlap (`with_prewarm`, STA-REQ-0408).
    let ((), prep_f64) = with_prewarm(BackendKind::Cuda, Precision::F64, || {});
    prep_f64.expect("CUDA f64 prewarm");
    let ((), prep_gpu) = with_prewarm(BackendKind::Gpu, Precision::F32, || {});
    prep_gpu.expect("GPU f32 prewarm");

    let only = std::env::var("WT_ONLY").ok();
    for d in CORPUS.iter().chain([&LANES_K1]) {
        if only.as_deref().is_some_and(|o| o != d.name) {
            continue;
        }
        let frozen = read_structural_frozen(d.name, d.text, full_libs)
            .unwrap_or_else(|e| panic!("{}: {e}", d.name));
        let (_graph, lowered) = lower_graph_and_corner(&frozen, full_libs, &full_tlib, None);
        let cpu = lowered
            .propagate(BackendKind::Cpu, Precision::F64, ctx())
            .expect("cpu f64");
        let cuda_f64 = lowered
            .propagate(BackendKind::Cuda, Precision::F64, ctx())
            .expect("cuda f64");
        assert_eq!(cuda_f64.stats.backend, BackendKind::Cuda);
        assert_bitwise_equal(&format!("{} cuda f64", d.name), &cpu, &cuda_f64);

        // Second CUDA f64 run is bitwise identical (STA-REQ-0402).
        let cuda_f64_2 = lowered
            .propagate(BackendKind::Gpu, Precision::F64, ctx())
            .expect("gpu f64 rerun");
        assert_bitwise_equal(
            &format!("{} cuda f64 rerun", d.name),
            &cuda_f64,
            &cuda_f64_2,
        );

        // CUDA f32 agrees within the STA-REQ-0403 envelope and is deterministic on rerun.
        let cuda_f32 = lowered
            .propagate(BackendKind::Cuda, Precision::F32, ctx())
            .expect("cuda f32");
        let cuda_f32_2 = lowered
            .propagate(BackendKind::Cuda, Precision::F32, ctx())
            .expect("cuda f32 rerun");
        assert_bitwise_equal(
            &format!("{} cuda f32 rerun", d.name),
            &cuda_f32,
            &cuda_f32_2,
        );
        let has_latch = !lowered.csr.maps.is_empty();
        assert_f32_envelope(&format!("{} cuda f32", d.name), &cpu, &cuda_f32, has_latch);
    }

    // Generated three-state and sequential topologies on `wt_15`.
    let wt15_lib = wt_15();
    let wt15_libs = std::slice::from_ref(&wt15_lib);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");
    for fix in [
        gen_tristate_bus(101, 4, 3),
        gen_tristate_bus(202, 8, 6),
        gen_latch_pipeline(303, 4),
        gen_clock_crossing(404, 4),
        gen_clock_gated(505, 4),
        gen_constant_tied(606, 4),
    ] {
        let frozen = fix.to_gate_netlist().freeze();
        let (_graph, lowered) = lower_graph_and_corner(&frozen, wt15_libs, &wt15_tlib, None);
        let cpu = lowered
            .propagate(BackendKind::Cpu, Precision::F64, ctx())
            .expect("cpu f64");
        let cuda_f64 = lowered
            .propagate(BackendKind::Cuda, Precision::F64, ctx())
            .expect("cuda f64");
        assert_bitwise_equal(&format!("{} cuda f64", fix.name), &cpu, &cuda_f64);
        let cuda_f32 = lowered
            .propagate(BackendKind::Cuda, Precision::F32, ctx())
            .expect("cuda f32");
        let has_latch = !lowered.csr.maps.is_empty();
        assert_f32_envelope(
            &format!("{} cuda f32", fix.name),
            &cpu,
            &cuda_f32,
            has_latch,
        );
    }
}

// STA-REQ-0402, STA-REQ-0403, STA-REQ-0406, STA-REQ-0407: explicit latch transparency windows
// (normal and short clock periods exercising opening/closing clips) and two-clock domains match
// bitwise in f64 and within the STA-REQ-0403 envelope in f32.
#[test]
#[ignore]
fn gpu_matches_cpu_on_latches_and_clocks() {
    let wt15_lib = wt_15();
    let wt15_libs = std::slice::from_ref(&wt15_lib);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");

    let latch_v = "\
module m (input clk, input a, output q);
  wire n1, n2, n3, n4, n5;
  wt_dff_p r1 (.D(a), .CLK(clk), .Q(n1));
  wt_buf_1 b1 (.A(n1), .Y(n2));
  wt_buf_1 b2 (.A(n2), .Y(n3));
  wt_dlatch_p l (.D(n3), .G(clk), .Q(n4));
  wt_inv_1 u (.A(n4), .Y(n5));
  wt_dff_p r2 (.D(n5), .CLK(clk), .Q(q));
endmodule
";
    let two_clocks_v = "\
module m (input ca, input cb, input a, output q);
  wire n1, n2;
  wt_dff_p r1 (.D(a), .CLK(ca), .Q(n1));
  wt_buf_1 b (.A(n1), .Y(n2));
  wt_dff_p r2 (.D(n2), .CLK(cb), .Q(q));
endmodule
";

    for (name, period) in [("latch", 2.0), ("latch_short", 0.9)] {
        let frozen = read_structural_frozen(name, latch_v, wt15_libs).expect("latch verilog");
        let nv = NetlistView::new(&frozen, wt15_libs).expect("nv");
        let clk_node = find_port_node(&nv, "clk");
        let a_node = find_port_node(&nv, "a");
        let mut ck = IdealClockSpec::new("clk", period, vec![clk_node]);
        ck.transition = [0.02, 0.02];
        let sched = IdealSchedule {
            clocks: vec![ck],
            input_delays: vec![InputDelaySpec {
                node: a_node,
                clock: 0,
                edge: Tr::Rise,
                delay: [0.2, 0.2],
                transition: [0.03, 0.03],
            }],
            untagged_transitions: Vec::new(),
            max_time_borrow: HashMap::new(),
        };
        let (_graph, lowered) = lower_graph_and_corner(&frozen, wt15_libs, &wt15_tlib, Some(sched));
        assert!(
            !lowered.csr.maps.is_empty(),
            "{name}: expected latch TagMapRec records"
        );
        let cpu = lowered
            .propagate(BackendKind::Cpu, Precision::F64, ctx())
            .expect("cpu f64");
        let cuda_f64 = lowered
            .propagate(BackendKind::Cuda, Precision::F64, ctx())
            .expect("cuda f64");
        assert_bitwise_equal(&format!("{name} cuda f64"), &cpu, &cuda_f64);
        let cuda_f32 = lowered
            .propagate(BackendKind::Cuda, Precision::F32, ctx())
            .expect("cuda f32");
        assert_f32_envelope(&format!("{name} cuda f32"), &cpu, &cuda_f32, true);
    }

    // Two clocks (`ca` period 3.0, `cb` period 2.0, `a` on clock `cb` with delay 0.4).
    let frozen_two =
        read_structural_frozen("two_clocks", two_clocks_v, wt15_libs).expect("two_clocks verilog");
    let nv_two = NetlistView::new(&frozen_two, wt15_libs).expect("nv_two");
    let ca_node = find_port_node(&nv_two, "ca");
    let cb_node = find_port_node(&nv_two, "cb");
    let a_node = find_port_node(&nv_two, "a");
    let mut ck_a = IdealClockSpec::new("A", 3.0, vec![ca_node]);
    ck_a.transition = [0.02, 0.02];
    let mut ck_b = IdealClockSpec::new("B", 2.0, vec![cb_node]);
    ck_b.transition = [0.02, 0.02];
    let sched_two = IdealSchedule {
        clocks: vec![ck_a, ck_b],
        input_delays: vec![InputDelaySpec {
            node: a_node,
            clock: 1,
            edge: Tr::Rise,
            delay: [0.4, 0.4],
            transition: [0.03, 0.03],
        }],
        untagged_transitions: Vec::new(),
        max_time_borrow: HashMap::new(),
    };
    let (_graph_two, lowered_two) =
        lower_graph_and_corner(&frozen_two, wt15_libs, &wt15_tlib, Some(sched_two));
    assert_eq!(lowered_two.ntags, 4);
    let cpu_two = lowered_two
        .propagate(BackendKind::Cpu, Precision::F64, ctx())
        .expect("cpu f64 two_clocks");
    let cuda_two_f64 = lowered_two
        .propagate(BackendKind::Cuda, Precision::F64, ctx())
        .expect("cuda f64 two_clocks");
    assert_bitwise_equal("two_clocks cuda f64", &cpu_two, &cuda_two_f64);
    let cuda_two_f32 = lowered_two
        .propagate(BackendKind::Cuda, Precision::F32, ctx())
        .expect("cuda f32 two_clocks");
    assert_f32_envelope("two_clocks cuda f32", &cpu_two, &cuda_two_f32, false);
}
