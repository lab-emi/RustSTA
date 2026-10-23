// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Backend benchmark (owner: C2; `STA-REQ-0400` to `STA-REQ-0408`): lowers and propagates a
//! datapath of `K` multiply-accumulate lanes (built from `K` instances of the one-lane netlist of
//! `ruststa-fixtures` on the new engine tree) across every propagation backend of the build,
//! checks that the backends agree (`STA-REQ-0403`: bitwise equal in `f64`, within envelope in
//! `f32`) and prints the median time of each phase (`lower`, `upload`, `kernel`, `readback`).
//!
//! `cargo bench -p ruststa-cli --features cuda --bench backends`
//! (`RUSTSTA_BENCH_K` sets the number of lanes, default 32; `RUSTSTA_BENCH_REPS` the repetitions,
//! default 3, of which the median is shown).

// The benchmark reports on standard output.
#![allow(clippy::print_stdout)]

use std::collections::HashMap;
use std::time::Instant;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{
    CONST0, CONST1, Conn, FrozenNetlist, GateNetlist, NetId, PinDir, PortDir, SCALAR,
};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::delay::load::{NetLoadOverrides, NetLoads};
use ruststa::engine::delay::wire::Ideal;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::input::{NetlistView, NodeKind};
use ruststa::engine::prop::lower::{
    BackendKind, IdealClockSpec, IdealSchedule, InputDelaySpec, LoweredResult, Precision,
    prewarm_backend,
};
use ruststa::engine::types::{An, NodeId, Tr};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::LANES_K1;

/// Parses the one-lane structural Verilog netlist [`LANES_K1`] via `read_structural`.
fn read_one_lane(lib: &Library) -> FrozenNetlist<'static> {
    read_structural(LANES_K1.text, "lanes_k1.v", &|cell_name| {
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
    .expect("read_structural lanes_k1")
    .freeze()
}

/// Builds a frozen [`FrozenNetlist`] for `k` parallel lanes of `one` (`lanes_k<k>`), connecting
/// lane `i` to `clk`, `a[16*i+15:16*i]`, `b[16*i+15:16*i]`, `c[16*i+15:16*i]`,
/// `y[32*i+31:32*i]`, and `gt[i]`.
fn build_k_lanes(one: &FrozenNetlist<'_>, k: usize) -> FrozenNetlist<'static> {
    let mut nl = GateNetlist::new(&format!("lanes_k{k}")).expect("module name");
    let mut cell_ids = Vec::with_capacity(one.cells.len());
    for (ci, &cname_id) in one.cells.iter().enumerate() {
        let cname_str = one.string(cname_id).expect("cell name");
        let pin_range = one.cell_pins(ci as u32);
        let pins: Vec<(&str, PinDir)> = pin_range
            .map(|p| {
                let pstr = one.string(one.cell_pin_name[p]).expect("pin name");
                let dir = match one.cell_pin_dir[p] {
                    0 => PinDir::Input,
                    1 => PinDir::Output,
                    2 => PinDir::Inout,
                    _ => PinDir::Other,
                };
                (pstr, dir)
            })
            .collect();
        cell_ids.push(nl.add_cell_type(cname_str, &pins).expect("add_cell_type"));
    }

    nl.add_port("clk", PortDir::Input, None).expect("port clk");
    for p in ["a", "b", "c"] {
        nl.add_port(p, PortDir::Input, Some((16 * k as i32 - 1, 0)))
            .expect("input bus port");
    }
    nl.add_port("y", PortDir::Output, Some((32 * k as i32 - 1, 0)))
        .expect("output bus port y");
    nl.add_port("gt", PortDir::Output, Some((k as i32 - 1, 0)))
        .expect("output bus port gt");

    let clk_net = nl.find_net("clk", None).expect("clk net");

    // Map each net row of `one` that corresponds to a port bit to `(port_name, bit_offset)`.
    let mut one_port_by_net: HashMap<u32, (&str, i32)> = HashMap::new();
    for &nr in one.port_bit_net.iter() {
        let net_rec = &one.nets[nr as usize];
        let pname = one.string(net_rec.name).expect("port name");
        let bit_idx = match net_rec.index {
            SCALAR => 0,
            idx => idx,
        };
        one_port_by_net.insert(nr, (pname, bit_idx));
    }

    for lane in 0..k {
        let mut lane_nets: Vec<Option<NetId>> = vec![None; one.nets.len()];
        for nr in 0..one.nets.len() as u32 {
            let net_id = if let Some(&(pname, bit_idx)) = one_port_by_net.get(&nr) {
                match pname {
                    "clk" => clk_net,
                    "a" | "b" | "c" => nl
                        .find_net(pname, Some(16 * lane as i32 + bit_idx))
                        .expect("lane input bit"),
                    "y" => nl
                        .find_net("y", Some(32 * lane as i32 + bit_idx))
                        .expect("lane y bit"),
                    "gt" => nl
                        .find_net("gt", Some(lane as i32 + bit_idx))
                        .expect("lane gt bit"),
                    other => panic!("unexpected port {other}"),
                }
            } else {
                let net_rec = &one.nets[nr as usize];
                let base = one.string(net_rec.name).unwrap_or("n");
                let nname = if net_rec.index == SCALAR {
                    format!("l{lane}/{base}_{nr}")
                } else {
                    format!("l{lane}/{base}_{}_{nr}", net_rec.index)
                };
                nl.add_net(Some(&nname), None).expect("internal lane net")
            };
            lane_nets[nr as usize] = Some(net_id);
        }

        let map_conn = |raw: u32| -> Conn {
            match raw {
                CONST0 => Conn::Const(false),
                CONST1 => Conn::Const(true),
                n => Conn::Net(lane_nets[n as usize].expect("mapped net")),
            }
        };

        for a in one.assigns.iter() {
            nl.add_assign(lane_nets[a.lhs as usize].expect("lhs net"), map_conn(a.rhs))
                .expect("add_assign");
        }

        for (inst_row, inst) in one.instances.iter().enumerate() {
            let ibase = one.string(inst.name).expect("inst name");
            let iname = format!("l{lane}/{ibase}");
            let cell_row = inst.cell as usize;
            let iid = nl
                .add_instance(&iname, cell_ids[cell_row])
                .expect("add_instance");
            let cell_pin_start = one.cell_pin_start[cell_row] as usize;
            for pin_row in one.instance_pins(inst_row as u32) {
                let raw_net = one.pin_net[pin_row];
                let cp = one.pin_cellpin[pin_row] as usize;
                let pname = one
                    .string(one.cell_pin_name[cell_pin_start + cp])
                    .expect("pin name");
                nl.connect(iid, pname, map_conn(raw_net)).expect("connect");
            }
        }
    }

    nl.freeze()
}

/// Builds the [`IdealSchedule`] matching `create_clock -name clk -period 4 [get_ports clk]` and
/// `set_input_delay -clock clk 0.5 [all_inputs]`.
fn lanes_schedule(nv: &NetlistView<'_>) -> IdealSchedule {
    let mut clk_sources = Vec::new();
    let mut input_delays = Vec::new();
    for i in 0..nv.num_port_bits() {
        let node = NodeId(i as u32);
        if let NodeKind::PortBit { dir, .. } = nv.node_kind(node)
            && dir == Direction::Input
        {
            if nv.node_name_str(node) == "clk" {
                clk_sources.push(node);
            } else {
                input_delays.push(InputDelaySpec {
                    node,
                    clock: 0,
                    edge: Tr::Rise,
                    delay: [0.5, 0.5],
                    transition: [0.02, 0.02],
                });
            }
        }
    }
    let mut ck = IdealClockSpec::new("clk", 4.0, clk_sources);
    ck.transition = [0.02, 0.02];
    IdealSchedule {
        clocks: vec![ck],
        input_delays,
        untagged_transitions: Vec::new(),
        max_time_borrow: HashMap::new(),
    }
}

/// Worst late arrival across all nodes of the graph (`max_arrival`).
fn worst_late_arrival(graph: &TimingGraph, res: &LoweredResult) -> f64 {
    let mut worst = f64::NEG_INFINITY;
    let ntags = res.values.ntags as usize;
    for &node in graph.order() {
        for tag in 0..ntags {
            for tr in [Tr::Rise, Tr::Fall] {
                let a = res.arrival(graph, node, tag, tr, An::Late);
                if a.is_finite() {
                    worst = worst.max(a);
                }
            }
        }
    }
    worst
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn in_pool<T: Send>(threads: usize, f: impl FnOnce() -> T + Send) -> T {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("thread pool")
        .install(f)
}

#[derive(Clone, Copy)]
struct PhaseSample {
    lower_s: f64,
    upload_s: f64,
    kernel_s: f64,
    readback_s: f64,
    total_s: f64,
}

fn run_once(
    nv: &NetlistView<'_>,
    graph: &TimingGraph,
    tlib: &TimingLib,
    loads: &NetLoads,
    sched: &IdealSchedule,
    kind: BackendKind,
    precision: Precision,
) -> (LoweredResult, PhaseSample) {
    let t0 = Instant::now();
    let lowered = sched.lower(nv, graph, tlib, loads).expect("lower corner");
    let lower_s = lowered.lower_s;
    let res = lowered
        .propagate(kind, precision, Ctx::new(None, None))
        .unwrap_or_else(|e| panic!("{kind:?}/{precision:?}: {e}"));
    let total_s = t0.elapsed().as_secs_f64();
    let sample = PhaseSample {
        lower_s,
        upload_s: res.stats.upload_s,
        kernel_s: res.stats.kernel_s,
        readback_s: res.stats.readback_s,
        total_s,
    };
    (res, sample)
}

fn main() {
    let k = env("RUSTSTA_BENCH_K", 32).max(1);
    let reps = env("RUSTSTA_BENCH_REPS", 3).max(1);
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let lib = read_text(&library(Variant::Full), "full").expect("library");
    let libs = std::slice::from_ref(&lib);
    let tlib = TimingLib::from_corner("default", libs).expect("TimingLib");

    let one_frozen = read_one_lane(&lib);
    let one_nv = NetlistView::new(&one_frozen, libs).expect("one lane NetlistView");
    let one_graph = TimingGraph::build(&one_nv, libs).expect("one lane TimingGraph");
    let one_loads = NetLoads::compute(&one_frozen, &tlib, &Ideal, &NetLoadOverrides::default());
    let one_sched = lanes_schedule(&one_nv);
    let (one_res, _) = run_once(
        &one_nv,
        &one_graph,
        &tlib,
        &one_loads,
        &one_sched,
        BackendKind::Cpu,
        Precision::F64,
    );

    let all_frozen = build_k_lanes(&one_frozen, k);
    let all_nv = NetlistView::new(&all_frozen, libs).expect("k lanes NetlistView");
    let all_graph = TimingGraph::build(&all_nv, libs).expect("k lanes TimingGraph");
    let all_loads = NetLoads::compute(&all_frozen, &tlib, &Ideal, &NetLoadOverrides::default());
    let all_sched = lanes_schedule(&all_nv);
    assert_eq!(
        all_nv.num_instances(),
        k * one_nv.num_instances(),
        "lanes_k{k}: instance count"
    );

    let mut runs: Vec<(&str, usize, BackendKind, Precision)> = vec![
        ("cpu", 1, BackendKind::Cpu, Precision::F64),
        ("cpu", cpus, BackendKind::Cpu, Precision::F64),
    ];
    if cfg!(feature = "cuda")
        && prewarm_backend(BackendKind::Cuda, Precision::F64).is_ok()
        && prewarm_backend(BackendKind::Cuda, Precision::F32).is_ok()
    {
        runs.push(("cuda f64", cpus, BackendKind::Cuda, Precision::F64));
        runs.push(("cuda f32", cpus, BackendKind::Cuda, Precision::F32));
    }

    println!(
        "lanes {k} ({} cells, {} nodes, {} arcs), {reps} repetitions (median), {cpus} CPUs",
        all_nv.num_instances(),
        all_graph.num_nodes(),
        all_graph.arcs().len(),
    );
    println!("backend          threads     lower    upload    kernel  readback     total");

    let one_worst = worst_late_arrival(&one_graph, &one_res);
    let mut cpu_ref: Option<LoweredResult> = None;

    for &(name, threads, kind, precision) in &runs {
        let mut lowers = Vec::with_capacity(reps);
        let mut uploads = Vec::with_capacity(reps);
        let mut kernels = Vec::with_capacity(reps);
        let mut readbacks = Vec::with_capacity(reps);
        let mut totals = Vec::with_capacity(reps);

        for _ in 0..reps {
            let (res, sample) = in_pool(threads, || {
                run_once(
                    &all_nv, &all_graph, &tlib, &all_loads, &all_sched, kind, precision,
                )
            });
            lowers.push(sample.lower_s);
            uploads.push(sample.upload_s);
            kernels.push(sample.kernel_s);
            readbacks.push(sample.readback_s);
            totals.push(sample.total_s);

            let w = worst_late_arrival(&all_graph, &res);
            assert!(
                (w - one_worst).abs() <= 1e-4 + 2e-5 * one_worst.abs(),
                "{name}: worst arrival {w} differs from one-lane {one_worst}"
            );

            match (&cpu_ref, precision) {
                (None, Precision::F64) => cpu_ref = Some(res),
                (Some(reference), Precision::F64) => {
                    // STA-REQ-0403, STA-REQ-0405: f64 backends (parallel CPU and CUDA f64) are
                    // bitwise identical to the 1-thread CPU reference.
                    assert!(
                        reference
                            .values
                            .arr
                            .iter()
                            .zip(&res.values.arr)
                            .all(|(a, b)| a.to_bits() == b.to_bits())
                            && reference
                                .values
                                .slew
                                .iter()
                                .zip(&res.values.slew)
                                .all(|(a, b)| a.to_bits() == b.to_bits())
                            && reference.values.pred == res.values.pred,
                        "{name} ({threads} threads): f64 values differ from CPU reference"
                    );
                }
                (Some(reference), Precision::F32) => {
                    // STA-REQ-0403: f32 values lie within `1e-4 + 2e-5 * |cpu|`.
                    for (c, g) in reference.values.arr.iter().zip(&res.values.arr) {
                        if c.is_infinite() || g.is_infinite() {
                            assert_eq!(c, g, "{name}: absent arrival mismatch");
                        } else {
                            assert!(
                                (c - g).abs() <= 1e-4 + 2e-5 * c.abs(),
                                "{name}: arrival cpu {c} vs f32 {g}"
                            );
                        }
                    }
                }
                (None, Precision::F32) => unreachable!("cpu f64 runs first"),
            }
        }

        println!(
            "  {name:<14} {threads:>7} {:>8.4}s {:>8.4}s {:>8.4}s {:>8.4}s {:>8.4}s",
            median(lowers),
            median(uploads),
            median(kernels),
            median(readbacks),
            median(totals),
        );
    }
}
