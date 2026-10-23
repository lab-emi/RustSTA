// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Legacy-parity and capability-delta tests comparing [`CompiledConstraints`] against the legacy
//! [`ruststa::constraints::Constraints`] reader across all synthetic STA fixtures and all 32
//! corpus netlists (`STA-REQ-0010`, `STA-REQ-0020`, `STA-REQ-0021`).

mod support;

use std::path::{Path, PathBuf};

use rusteda_core::liberty::Library;
use rusteda_core::netlist::{
    Conn as CoreConn, FrozenNetlist, GateNetlist, NetId as CoreNetId, PinDir as CorePinDir,
    PortDir as CorePortDir,
};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::checks;
use ruststa::constraints::{Clock, Constraints, PortDelay};
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs, ExceptionKind};
use ruststa::engine::types::{An, Tr};
use ruststa::graph::Graph;
use ruststa::model::Model;
use ruststa::old_sdc;
use ruststa::prop;
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use support::legacy_import::{
    Conn as StepConn, LegacyImport, NetRef, PinDir as StepPinDir, PortDir as StepPortDir,
    read_flat, steps,
};

fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn freeze_from_steps(s: &LegacyImport) -> FrozenNetlist<'static> {
    let mut gn = GateNetlist::new(&s.module).expect("GateNetlist::new");
    let mut cell_ids = Vec::with_capacity(s.cells.len());
    for c in &s.cells {
        let pins: Vec<(&str, CorePinDir)> = c
            .pins
            .iter()
            .map(|(n, d)| {
                let dir = match d {
                    StepPinDir::Input => CorePinDir::Input,
                    StepPinDir::Output => CorePinDir::Output,
                    StepPinDir::Inout => CorePinDir::Inout,
                    StepPinDir::Other => CorePinDir::Other,
                };
                (n.as_str(), dir)
            })
            .collect();
        cell_ids.push(gn.add_cell_type(&c.name, &pins).expect("add_cell_type"));
    }

    for p in &s.ports {
        let dir = match p.dir {
            StepPortDir::Input => CorePortDir::Input,
            StepPortDir::Output => CorePortDir::Output,
        };
        gn.add_port(&p.name, dir, p.range).expect("add_port");
    }

    let mut extra_nets: Vec<CoreNetId> = Vec::with_capacity(s.nets.len());
    for name in &s.nets {
        extra_nets.push(gn.add_net(Some(name), None).expect("add_net"));
    }

    let resolve_net = |gn: &GateNetlist, r: NetRef| -> CoreNetId {
        match r {
            NetRef::PortBit { port, bit } => gn.ports()[port].bits[bit],
            NetRef::Net(k) => extra_nets[k],
        }
    };
    let resolve_conn = |gn: &GateNetlist, c: StepConn| -> CoreConn {
        match c {
            StepConn::Net(r) => CoreConn::Net(resolve_net(gn, r)),
            StepConn::Const(b) => CoreConn::Const(b),
        }
    };

    for inst in &s.instances {
        let id = gn
            .add_instance(&inst.name, cell_ids[inst.cell])
            .expect("add_instance");
        for (pin, conn) in &inst.conns {
            let c = resolve_conn(&gn, *conn);
            gn.connect(id, pin.as_str(), c).expect("connect");
        }
    }

    for a in &s.assigns {
        let lhs = resolve_net(&gn, a.lhs);
        let rhs = resolve_conn(&gn, a.rhs);
        gn.add_assign(lhs, rhs).expect("add_assign");
    }

    gn.freeze()
}

fn build_structural_netlist(verilog_src: &str, lib: &Library) -> FrozenNetlist<'static> {
    let cell_pins = |cell_name: &str| -> Option<Vec<(String, CorePinDir)>> {
        let cell = lib.cells.iter().find(|c| c.name == cell_name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        rusteda_core::liberty::Direction::Input => CorePinDir::Input,
                        rusteda_core::liberty::Direction::Output => CorePinDir::Output,
                        rusteda_core::liberty::Direction::Inout => CorePinDir::Inout,
                        rusteda_core::liberty::Direction::Internal => CorePinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    };
    read_structural(verilog_src, "<test.v>", &cell_pins)
        .expect("valid structural verilog")
        .freeze()
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-12
}

fn assert_clock_eq(ctx: &str, a: &Clock, b: &Clock) {
    assert_eq!(a.name, b.name, "{ctx}: clock name");
    assert!(
        approx_eq(a.period, b.period),
        "{ctx}: clock {} period {} vs {}",
        a.name,
        a.period,
        b.period
    );
    assert!(
        approx_eq(a.edges[0], b.edges[0]) && approx_eq(a.edges[1], b.edges[1]),
        "{ctx}: clock {} edges {:?} vs {:?}",
        a.name,
        a.edges,
        b.edges
    );
    let mut sa = a.sources.clone();
    let mut sb = b.sources.clone();
    sa.sort_unstable();
    sb.sort_unstable();
    assert_eq!(sa, sb, "{ctx}: clock {} sources", a.name);
    assert!(
        approx_eq(a.latency[0], b.latency[0]) && approx_eq(a.latency[1], b.latency[1]),
        "{ctx}: clock {} latency {:?} vs {:?}",
        a.name,
        a.latency,
        b.latency
    );
    assert!(
        approx_eq(a.transition, b.transition),
        "{ctx}: clock {} transition {} vs {}",
        a.name,
        a.transition,
        b.transition
    );
    assert!(
        approx_eq(a.uncertainty_setup, b.uncertainty_setup),
        "{ctx}: clock {} uncertainty_setup {} vs {}",
        a.name,
        a.uncertainty_setup,
        b.uncertainty_setup
    );
    assert!(
        approx_eq(a.uncertainty_hold, b.uncertainty_hold),
        "{ctx}: clock {} uncertainty_hold {} vs {}",
        a.name,
        a.uncertainty_hold,
        b.uncertainty_hold
    );
}

fn sorted_delays(delays: &[PortDelay]) -> Vec<PortDelay> {
    let mut v = delays.to_vec();
    v.sort_by_key(|d| (d.node, d.clock, d.edge));
    v
}

fn assert_delays_eq(ctx: &str, kind: &str, a: &[PortDelay], b: &[PortDelay]) {
    let sa = sorted_delays(a);
    let sb = sorted_delays(b);
    assert_eq!(
        sa.len(),
        sb.len(),
        "{ctx}: {kind} len {} vs {}",
        sa.len(),
        sb.len()
    );
    for (da, db) in sa.iter().zip(&sb) {
        assert_eq!(
            (da.node, da.clock, da.edge),
            (db.node, db.clock, db.edge),
            "{ctx}: {kind} key mismatch"
        );
        assert!(
            approx_eq(da.value[0], db.value[0]) && approx_eq(da.value[1], db.value[1]),
            "{ctx}: {kind} node {} value {:?} vs {:?}",
            da.node,
            da.value,
            db.value
        );
    }
}

fn assert_constraints_parity(ctx: &str, legacy: &Constraints, compiled_as_legacy: &Constraints) {
    assert_eq!(
        legacy.clocks.len(),
        compiled_as_legacy.clocks.len(),
        "{ctx}: clock count"
    );
    for (ca, cb) in legacy.clocks.iter().zip(&compiled_as_legacy.clocks) {
        assert_clock_eq(ctx, ca, cb);
    }
    assert_delays_eq(
        ctx,
        "input_delays",
        &legacy.input_delays,
        &compiled_as_legacy.input_delays,
    );
    assert_delays_eq(
        ctx,
        "output_delays",
        &legacy.output_delays,
        &compiled_as_legacy.output_delays,
    );
    assert_eq!(
        legacy.port_load.len(),
        compiled_as_legacy.port_load.len(),
        "{ctx}: port_load count"
    );
    for (&node, &va) in &legacy.port_load {
        let vb = compiled_as_legacy
            .port_load
            .get(&node)
            .unwrap_or_else(|| panic!("{ctx}: missing port_load for node {node}"));
        assert!(
            approx_eq(va[0], vb[0]) && approx_eq(va[1], vb[1]),
            "{ctx}: port_load node {node}: {va:?} vs {vb:?}"
        );
    }
    assert_eq!(
        legacy.input_transition.len(),
        compiled_as_legacy.input_transition.len(),
        "{ctx}: input_transition count"
    );
    for (&node, &va) in &legacy.input_transition {
        let vb = compiled_as_legacy
            .input_transition
            .get(&node)
            .unwrap_or_else(|| panic!("{ctx}: missing input_transition for node {node}"));
        assert!(
            approx_eq(va[0], vb[0]) && approx_eq(va[1], vb[1]),
            "{ctx}: input_transition node {node}: {va:?} vs {vb:?}"
        );
    }
}

// STA-REQ-0010, STA-REQ-0020, STA-REQ-0021: Exact parity with legacy Constraints::from_sdc and
// downstream timing propagation across all 8 synthetic STA fixtures.
#[test]
fn synthetic_sta_sdc_legacy_parity_and_propagation() {
    let root = repo_root();
    let sta_dir = root.join("verification/fixtures/synthetic/sta");
    let lib_text = std::fs::read_to_string(sta_dir.join("lib/wt_15.lib")).expect("wt_15.lib");
    let lib = read_text(&lib_text, "wt_15.lib").expect("parse wt_15.lib");
    let libs = [lib];
    let model = Model::build(&libs).expect("Model::build");

    let fixtures = [
        "chain_8",
        "clock_crossing",
        "clock_gated",
        "constant_tied",
        "dag_32",
        "fanout_64",
        "latch_pipeline",
    ];

    for name in fixtures {
        let v_text =
            std::fs::read_to_string(sta_dir.join(format!("net/{name}.v"))).expect("netlist");
        let sdc_text =
            std::fs::read_to_string(sta_dir.join(format!("sdc/{name}.sdc"))).expect("sdc");

        // Legacy pipeline
        let flat = read_flat(name, &v_text, None, &libs).expect("read_flat");
        let graph = Graph::build(&flat, &model).expect("Graph::build");
        let old_cmds = old_sdc::parse(&sdc_text).expect("old_sdc::parse");
        let legacy = Constraints::from_sdc(&old_cmds, &graph, model.time_unit, model.cap_unit)
            .expect("Constraints::from_sdc");

        // New constraint compiler on both LegacyImport netlist and read_structural netlist
        let step_netlist = freeze_from_steps(&steps(&flat, &libs).expect("steps"));
        let struct_netlist = build_structural_netlist(&v_text, &libs[0]);
        let corners = [CornerLibs::new("default", &libs)];

        for (tag, nl) in [("steps", &step_netlist), ("structural", &struct_netlist)] {
            let compiled = CompiledConstraints::from_sdc_str(&sdc_text, name, nl, &corners)
                .unwrap_or_else(|e| panic!("{name} ({tag}): {e}"));
            let converted = compiled.to_legacy_constraints(nl, &graph);
            assert_constraints_parity(&format!("{name} ({tag})"), &legacy, &converted);

            // Verify downstream STA setup, propagation, and endpoint checks are identical
            let setup_leg = prop::Setup::build(&graph, &model, &legacy).expect("setup legacy");
            let setup_new = prop::Setup::build(&graph, &model, &converted).expect("setup new");
            let vals_leg = prop::propagate_cpu(&graph, &model, &setup_leg);
            let vals_new = prop::propagate_cpu(&graph, &model, &setup_new);
            let ep_leg =
                checks::check_all(&graph, &model, &legacy, &setup_leg, &vals_leg).expect("ep leg");
            let ep_new = checks::check_all(&graph, &model, &converted, &setup_new, &vals_new)
                .expect("ep new");
            assert_eq!(ep_leg.setup.len(), ep_new.setup.len(), "{name} setup count");
            assert_eq!(ep_leg.hold.len(), ep_new.hold.len(), "{name} hold count");
            for (node, a) in &ep_leg.setup {
                let b = &ep_new.setup[node];
                assert!(
                    approx_eq(a.slack, b.slack),
                    "{name}: setup slack {} vs {}",
                    a.slack,
                    b.slack
                );
            }
            for (node, a) in &ep_leg.hold {
                let b = &ep_new.hold[node];
                assert!(
                    approx_eq(a.slack, b.slack),
                    "{name}: hold slack {} vs {}",
                    a.slack,
                    b.slack
                );
            }
        }
    }

    // `tristate_bus` is rejected by legacy `Graph::build` (multiple drivers on `bus_net`), while
    // `CompiledConstraints` compiles its constraints cleanly on the shared `FrozenNetlist`.
    {
        let v_text =
            std::fs::read_to_string(sta_dir.join("net/tristate_bus.v")).expect("tristate_bus.v");
        let sdc_text = std::fs::read_to_string(sta_dir.join("sdc/tristate_bus.sdc"))
            .expect("tristate_bus.sdc");
        let flat = read_flat("tristate_bus", &v_text, None, &libs).expect("read_flat");
        assert!(Graph::build(&flat, &model).is_err());
        let struct_netlist = build_structural_netlist(&v_text, &libs[0]);
        let corners = [CornerLibs::new("default", &libs)];
        let compiled =
            CompiledConstraints::from_sdc_str(&sdc_text, "tristate_bus", &struct_netlist, &corners)
                .expect("CompiledConstraints compiles tristate_bus");
        assert_eq!(compiled.clocks.len(), 1);
        assert_eq!(compiled.input_delays.len(), 6);
        assert_eq!(compiled.output_delays.len(), 2);
    }
}

// STA-REQ-0010, STA-REQ-0020: Legacy parity across all 32 corpus netlists (`CORPUS` + `LANES_K1`).
#[test]
fn corpus_32_netlists_legacy_parity() {
    let lib = read_text(&library(Variant::Full), Variant::Full.name()).expect("wt_full");
    let libs = [lib];
    let model = Model::build(&libs).expect("Model::build");
    let corners = [CornerLibs::new("default", &libs)];

    let mut count = 0usize;
    for n in CORPUS.iter().chain([&LANES_K1]) {
        let flat = read_flat(n.name, n.text, Some(n.top), &libs).expect(n.name);
        let graph = Graph::build(&flat, &model).expect("Graph::build");
        let imp = steps(&flat, &libs).expect("steps");
        let netlist = freeze_from_steps(&imp);

        // Pick clock port (prefer `clk` if present, otherwise first input port bit).
        let clk_port_name = flat
            .ports
            .iter()
            .find(|p| p.input && p.name == "clk")
            .or_else(|| flat.ports.iter().find(|p| p.input))
            .expect("at least one input port")
            .name
            .clone();

        // Pick one specific non-clock input port and one specific output port for targeted queries.
        let sample_in = flat
            .ports
            .iter()
            .find(|p| p.input && p.name != clk_port_name)
            .map(|p| p.name.clone());
        let sample_out = flat
            .ports
            .iter()
            .find(|p| !p.input)
            .map(|p| p.name.clone())
            .expect("at least one output port");

        let mut sdc_lines = vec![
            format!(
                "create_clock -name main_clk -period 2.5 -waveform {{0.0 1.25}} [get_ports {{{clk_port_name}}}]"
            ),
            "set_clock_uncertainty -setup 0.08 [get_clocks main_clk]".to_string(),
            "set_clock_uncertainty -hold 0.03 [get_clocks main_clk]".to_string(),
            "set_clock_latency -min 0.10 [get_clocks main_clk]".to_string(),
            "set_clock_latency -max 0.18 [get_clocks main_clk]".to_string(),
            "set_clock_transition 0.05 [all_clocks]".to_string(),
            "set_input_delay -clock main_clk -min 0.12 [all_inputs]".to_string(),
            "set_input_delay -clock main_clk -max 0.45 [all_inputs]".to_string(),
            "set_output_delay -clock main_clk -min 0.08 [all_outputs]".to_string(),
            "set_output_delay -clock main_clk -max 0.50 [all_outputs]".to_string(),
            "set_input_transition -min 0.02 [all_inputs]".to_string(),
            "set_input_transition -max 0.06 [all_inputs]".to_string(),
            "set_load -min 0.01 [all_outputs]".to_string(),
            "set_load -max 0.04 [all_outputs]".to_string(),
            format!("set_load -max 0.07 [get_ports {{{sample_out}}}]"),
        ];
        if let Some(in_port) = sample_in {
            sdc_lines.push(format!(
                "set_input_delay -clock main_clk -clock_fall -add_delay 0.25 [get_ports {{{in_port}}}]"
            ));
        }
        let sdc_text = sdc_lines.join("\n");

        let old_cmds = old_sdc::parse(&sdc_text).expect("old_sdc::parse");
        let legacy = Constraints::from_sdc(&old_cmds, &graph, model.time_unit, model.cap_unit)
            .unwrap_or_else(|e| panic!("{}: legacy from_sdc failed: {e}", n.name));

        let compiled = CompiledConstraints::from_sdc_str(&sdc_text, n.name, &netlist, &corners)
            .unwrap_or_else(|e| panic!("{}: compiled from_sdc_str failed: {e}", n.name));
        let converted = compiled.to_legacy_constraints(&netlist, &graph);

        assert_constraints_parity(n.name, &legacy, &converted);
        count += 1;
    }
    assert_eq!(count, 32);
}

// STA-REQ-0010, STA-REQ-0021: Verify unit conversion parity when Liberty library units are 1ps / 1fF.
#[test]
fn library_unit_scaling_ps_ff_legacy_parity() {
    let mut lib = read_text(&library(Variant::Full), Variant::Full.name()).expect("wt_full");
    lib.time_unit_ns = 1e-3; // 1 ps
    lib.cap_unit_pf = 1e-3; // 1 fF
    lib.units.time.scale = 1e-3;
    lib.units.capacitance.scale = 1e-3;
    let libs = [lib];
    let model = Model::build(&libs).expect("Model::build");
    let corners = [CornerLibs::new("default", &libs)];

    let n = &CORPUS[0]; // counter
    let flat = read_flat(n.name, n.text, Some(n.top), &libs).expect("read_flat");
    let graph = Graph::build(&flat, &model).expect("Graph::build");
    let netlist = freeze_from_steps(&steps(&flat, &libs).expect("steps"));
    let clk_port = &flat.ports.iter().find(|p| p.input).unwrap().name;

    // Values given in library units (2000 ps = 2.0 ns, 25 fF = 0.025 pF)
    let sdc_text = format!(
        r#"
        create_clock -name clk -period 2000 -waveform {{0 1000}} [get_ports {{{clk_port}}}]
        set_clock_uncertainty -setup 80 [get_clocks clk]
        set_clock_uncertainty -hold 30 [get_clocks clk]
        set_clock_latency -min 100 [get_clocks clk]
        set_clock_latency -max 150 [get_clocks clk]
        set_clock_transition 40 [get_clocks clk]
        set_input_delay -clock clk -min 120 [all_inputs]
        set_input_delay -clock clk -max 350 [all_inputs]
        set_output_delay -clock clk -min 90 [all_outputs]
        set_output_delay -clock clk -max 420 [all_outputs]
        set_input_transition -min 15 [all_inputs]
        set_input_transition -max 45 [all_inputs]
        set_load -min 10 [all_outputs]
        set_load -max 25 [all_outputs]
    "#
    );

    let old_cmds = old_sdc::parse(&sdc_text).expect("old_sdc::parse");
    let legacy = Constraints::from_sdc(&old_cmds, &graph, model.time_unit, model.cap_unit)
        .expect("legacy from_sdc");
    let compiled = CompiledConstraints::from_sdc_str(&sdc_text, "ps_ff.sdc", &netlist, &corners)
        .expect("compiled");
    let converted = compiled.to_legacy_constraints(&netlist, &graph);

    assert_constraints_parity("counter_ps_ff", &legacy, &converted);
    assert!(approx_eq(converted.clocks[0].period, 2.0));
}

// STA-REQ-0010, STA-REQ-0020: Documented improvements of CompiledConstraints over legacy Constraints.
#[test]
fn documented_improvements_over_legacy_constraints() {
    let lib = read_text(&library(Variant::Full), Variant::Full.name()).expect("wt_full");
    let libs = [lib];
    let corners = [CornerLibs::new("default", &libs)];

    let n = &CORPUS[0]; // counter
    let flat = read_flat(n.name, n.text, Some(n.top), &libs).expect("read_flat");
    let netlist = freeze_from_steps(&steps(&flat, &libs).expect("steps"));
    let clk_port = &flat.ports.iter().find(|p| p.input).unwrap().name;

    // Commands/options that legacy `old_sdc` / `Constraints::from_sdc` rejects or ignores,
    // which `CompiledConstraints` compiles with full fidelity:
    // 1. Virtual clocks (`create_clock` without source objects) and `-add` secondary clocks
    // 2. Per-edge (`-rise` / `-fall`) input/output delays, input transitions, and clock transitions
    // 3. `set_units` explicit unit overrides
    // 4. `all_registers`, `get_cells -filter`, `get_pins -of_objects`, timing exceptions
    let advanced_sdc = format!(
        r#"
        set_units -time ns -capacitance pF
        create_clock -name clk -period 2.0 [get_ports {{{clk_port}}}]
        create_clock -name clk_alt -period 4.0 -add [get_ports {{{clk_port}}}]
        create_clock -name vclk -period 2.0

        set_clock_transition -rise -max 0.04 [get_clocks clk]
        set_clock_transition -fall -max 0.06 [get_clocks clk]

        set_input_delay -clock vclk -rise -max 0.40 [all_inputs -no_clocks]
        set_input_delay -clock vclk -fall -max 0.35 [all_inputs -no_clocks]
        set_input_delay -clock vclk -rise -min 0.10 [all_inputs -no_clocks]
        set_input_delay -clock vclk -fall -min 0.08 [all_inputs -no_clocks]

        set_input_transition -rise -max 0.05 [all_inputs -no_clocks]
        set_input_transition -fall -max 0.07 [all_inputs -no_clocks]

        set_false_path -from [get_clocks clk_alt] -to [all_registers -data_pins]
        set_multicycle_path -setup 2 -from [all_registers -clock_pins] -to [all_outputs]
    "#
    );

    // Legacy `Constraints::from_sdc` fails on virtual clocks (`create_clock -name vclk -period 2.0`).
    let model = Model::build(&libs).expect("Model::build");
    let graph = Graph::build(&flat, &model).expect("Graph::build");
    let old_cmds = old_sdc::parse(&advanced_sdc).expect("old_sdc::parse");
    assert!(
        Constraints::from_sdc(&old_cmds, &graph, model.time_unit, model.cap_unit).is_err(),
        "legacy reader rejects virtual clocks"
    );

    // New compiler succeeds and preserves every per-edge value and exception.
    let compiled = CompiledConstraints::from_sdc_str(&advanced_sdc, "adv.sdc", &netlist, &corners)
        .expect("CompiledConstraints handles virtual clocks, -add, per-edge delays, and queries");
    assert_eq!(compiled.clocks.len(), 3);
    assert!(compiled.clocks[2].is_virtual());
    assert!(compiled.clocks[1].add);
    assert!(approx_eq(
        compiled.clocks[0].transition_ns[An::Late as usize][Tr::Rise as usize],
        0.04
    ));
    assert!(approx_eq(
        compiled.clocks[0].transition_ns[An::Late as usize][Tr::Fall as usize],
        0.06
    ));
    for d in &compiled.input_delays {
        assert_eq!(d.delay_ns[An::Late as usize][Tr::Rise as usize], Some(0.40));
        assert_eq!(d.delay_ns[An::Late as usize][Tr::Fall as usize], Some(0.35));
        assert_eq!(
            d.delay_ns[An::Early as usize][Tr::Rise as usize],
            Some(0.10)
        );
        assert_eq!(
            d.delay_ns[An::Early as usize][Tr::Fall as usize],
            Some(0.08)
        );
    }
    for it in compiled.input_transitions.values() {
        assert_eq!(it.slew_ns[An::Late as usize][Tr::Rise as usize], Some(0.05));
        assert_eq!(it.slew_ns[An::Late as usize][Tr::Fall as usize], Some(0.07));
    }
    assert_eq!(compiled.exceptions.len(), 2);
    assert!(matches!(
        compiled.exceptions[0].kind,
        ExceptionKind::FalsePath { .. }
    ));
    assert!(matches!(
        compiled.exceptions[1].kind,
        ExceptionKind::MulticyclePath { multiplier: 2, .. }
    ));
}
