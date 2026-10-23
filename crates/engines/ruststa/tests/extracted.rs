// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Extracted RC hand derivation, bounded rejection and incremental/full consistency.
//! STA-REQ-0701: format semantics are validated by the shared SPEF reader before binding.
use rusteda_core::{
    liberty::Library,
    netlist::{Conn, GateNetlist, PinDir, PortDir},
};
use rusteda_formats::{liberty, spef};
use ruststa::engine::{
    analysis::AnalysisOptions,
    delay::{
        load::NetLoadOverrides,
        spef::{ParasiticInput, Spef},
        wire::WireModel,
    },
    incr::TimingSession,
    input::{NetlistView, libview::TimingLib},
    types::{An, NetRow, NodeId, Tr},
};
use ruststa_fixtures::liberty::{Variant, library};
use std::sync::{Arc, OnceLock};

fn lib() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(|| liberty::read_text(&library(Variant::Wt15), "synthetic.lib").unwrap())
}
fn design() -> GateNetlist {
    let mut g = GateNetlist::new("top").unwrap();
    let buf = g
        .add_cell_type("wt_buf_1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();
    g.add_port("a", PortDir::Input, None).unwrap();
    g.add_port("y", PortDir::Output, None).unwrap();
    let u = g.add_instance("u", buf).unwrap();
    g.connect(u, "A", Conn::Net(g.find_net("a", None).unwrap()))
        .unwrap();
    g.connect(u, "Y", Conn::Net(g.find_net("y", None).unwrap()))
        .unwrap();
    g
}
fn source() -> String {
    "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n*T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n*D_NET a 0.03\n*CONN\n*P a I\n*I u:A I\n*CAP\n1 a 0.01\n2 u:A 0.02\n*RES\n1 a u:A 2\n*END\n*D_NET y 0.04\n*CONN\n*P y O\n*I u:Y O\n*CAP\n1 u:Y 0.01\n2 y 0.03\n*RES\n1 u:Y y 3\n*END\n".into()
}
// STA-REQ-0703, STA-REQ-0704: exact R*C first moments, including Liberty and port caps.
#[test]
fn physical_pin_loads_and_port_loads_enter_elmore() {
    let frozen = design().freeze();
    let nv = NetlistView::new(&frozen, std::slice::from_ref(lib())).unwrap();
    let tl = TimingLib::from_corner("default", std::slice::from_ref(lib())).unwrap();
    let parsed = spef::read(&source(), spef::Limits::default()).unwrap();
    let mut ov = NetLoadOverrides::default();
    ov.add_port_bit_load(1, 0.05, 0.06);
    let model = Spef::bind(&parsed, &nv, &tl, &ov, 1 << 20).unwrap();
    let input_cap = tl
        .cell("wt_buf_1")
        .unwrap()
        .pins
        .iter()
        .find(|p| p.name == "A")
        .unwrap()
        .cap[0][0];
    let delay_a = model.load_delay(NetRow(0), NodeId(2), Tr::Rise, An::Early, 0.1);
    assert!((delay_a.delay - 2.0 * (0.02 + input_cap)).abs() < 1e-14);
    assert_eq!(delay_a.slew, 0.1);
    assert!(
        (model
            .load_delay(NetRow(1), NodeId(1), Tr::Rise, An::Early, 0.1)
            .delay
            - 3.0 * 0.08)
            .abs()
            < 1e-14
    );
    assert!(
        (model
            .load_delay(NetRow(1), NodeId(1), Tr::Rise, An::Late, 0.1)
            .delay
            - 3.0 * 0.09)
            .abs()
            < 1e-14
    );
    assert!(Spef::bind(&parsed, &nv, &tl, &ov, 1).is_err());
}
// STA-REQ-0703, STA-REQ-0705: missing pins/nets, cycles and disconnected nodes never become ideal.
#[test]
fn reject_incomplete_and_mesh_extractions() {
    let frozen = design().freeze();
    let nv = NetlistView::new(&frozen, std::slice::from_ref(lib())).unwrap();
    let tl = TimingLib::from_corner("default", std::slice::from_ref(lib())).unwrap();
    for text in [
        source().replace("*I u:A I\n", ""),
        source().replace("1 a u:A 2", "1 a u:A 2\n2 a u:A 3"),
        source().replace("1 a u:A 2", "1 a isolated:1 2"),
        source().replace("*I u:A I", "*I u:Y O"),
    ] {
        let p = spef::read(&text, spef::Limits::default()).unwrap();
        assert!(Spef::bind(&p, &nv, &tl, &NetLoadOverrides::default(), 1 << 20).is_err());
    }
    let p = spef::read(
        source().split("*D_NET y").next().unwrap(),
        spef::Limits::default(),
    )
    .unwrap();
    assert!(Spef::bind(&p, &nv, &tl, &NetLoadOverrides::default(), 1 << 20).is_err());
}

// STA-REQ-0708: reject forged counters/indices before allocation and indexing.
#[test]
fn invalid_typed_parasitics_are_errors_not_panics() {
    let frozen = design().freeze();
    let view = NetlistView::new(&frozen, std::slice::from_ref(lib())).unwrap();
    let library = TimingLib::from_corner("default", std::slice::from_ref(lib())).unwrap();
    let original = spef::read(&source(), spef::Limits::default()).unwrap();
    let mut bad = original.clone();
    bad.nets[0].connections[0].node = u32::MAX;
    assert!(Spef::bind(&bad, &view, &library, &NetLoadOverrides::default(), 1 << 20).is_err());
    let mut bad = original.clone();
    bad.node_count = 0;
    assert!(Spef::bind(&bad, &view, &library, &NetLoadOverrides::default(), 1 << 20).is_err());
    let mut bad = original;
    bad.nets[0].resistors[0].b = u32::MAX;
    assert!(Spef::bind(&bad, &view, &library, &NetLoadOverrides::default(), 1 << 20).is_err());
}
// STA-REQ-0706: a real cell resize changes pin loads and forward/backward timing; bitwise full match.
#[test]
fn extracted_incremental_resize_matches_full() {
    let input = Arc::new(ParasiticInput {
        corner: "default".into(),
        data: spef::read(&source(), spef::Limits::default()).unwrap(),
    });
    let sdc = "create_clock -name vclk -period 5\nset_input_delay -clock vclk -max 0.2 [get_ports a]\nset_input_delay -clock vclk -min 0.1 [get_ports a]\nset_output_delay -clock vclk -max 0.2 [get_ports y]\nset_output_delay -clock vclk -min 0.1 [get_ports y]\nset_input_transition 0.1 [get_ports a]\nset_load 0.01 [get_ports y]\n";
    let mut session = TimingSession::new(
        design(),
        vec![lib().clone()],
        sdc,
        AnalysisOptions {
            parasitics: Some(input),
            max_state_bytes: 1 << 24,
            ..Default::default()
        },
    )
    .unwrap();
    session.verify_against_full().unwrap();
    for cell in ["wt_buf_2", "wt_buf_1", "wt_buf_2"] {
        session.swap_cell_by_name("u", cell).unwrap();
        session.verify_against_full().unwrap();
    }
}

// STA-REQ-0706: real baseline libraries, fixed routed topology, resized pin capacitances.
// Acceptance harness emits this bounded two-case manifest; absence is a failed explicit run.
#[test]
#[ignore = "requires RUSTSTA_RC_CASES from the dual-PDK routed acceptance harness"]
fn dual_pdk_extracted_incremental_matches_full() {
    use rusteda_core::liberty::Direction;
    let path = std::env::var("RUSTSTA_RC_CASES").expect("RUSTSTA_RC_CASES");
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.len() < 16384);
    let cases: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let bounded = |key: &str| {
            let path = case[key].as_str().unwrap();
            assert!(std::fs::metadata(path).unwrap().len() <= 32 << 20);
            std::fs::read_to_string(path).unwrap()
        };
        let library = liberty::read_text(&bounded("library"), "baseline.lib").unwrap();
        let frozen = rusteda_formats::verilog::netlist::read_structural(
            &bounded("netlist"),
            "routed.v",
            &|name| {
                library.cells.iter().find(|c| c.name == name).map(|cell| {
                    cell.pins
                        .iter()
                        .filter_map(|pin| {
                            let dir = match pin.direction {
                                Direction::Input => PinDir::Input,
                                Direction::Output => PinDir::Output,
                                Direction::Inout => PinDir::Inout,
                                _ => return None,
                            };
                            Some((pin.name.clone(), dir))
                        })
                        .collect()
                })
            },
        )
        .unwrap()
        .freeze();
        let (sdc, _) = rusteda_formats::sdc::read_str(&bounded("sdc"));
        let corner = case["corner"].as_str().unwrap().to_owned();
        let input = Arc::new(ParasiticInput {
            corner: corner.clone(),
            data: spef::read(&bounded("spef"), spef::Limits::default()).unwrap(),
        });
        let original = case["original"].as_str().unwrap().to_owned();
        let mut session = TimingSession::new_multi_corner(
            frozen.thaw().unwrap(),
            vec![(corner, vec![library])],
            sdc,
            AnalysisOptions {
                parasitics: Some(input),
                max_state_bytes: 1 << 28,
                ..Default::default()
            },
        )
        .unwrap();
        session.verify_against_full().unwrap();
        for cell in [case["replacement"].as_str().unwrap(), original.as_str()] {
            let stats = session
                .swap_cell_by_name(case["target"].as_str().unwrap(), cell)
                .unwrap();
            assert!(stats.forward_nodes_evaluated > 0);
            session.verify_against_full().unwrap();
        }
    }
}

// STA-REQ-0704, STA-REQ-0706: native CUDA wire records agree bitwise with cpu_ref in f64.
#[cfg(feature = "cuda")]
#[test]
#[ignore = "requires a native CUDA driver and NVRTC; run without address-space ulimit"]
fn extracted_cpu_cuda_agree() {
    use ruststa::engine::{analysis::EngineAnalysis, prop::lower::BackendKind};
    let input = Arc::new(ParasiticInput {
        corner: "default".into(),
        data: spef::read(&source(), spef::Limits::default()).unwrap(),
    });
    let options = AnalysisOptions {
        parasitics: Some(input),
        max_state_bytes: 1 << 24,
        ..Default::default()
    };
    let frozen = design().freeze();
    let sdc = "create_clock -name vclk -period 5\nset_input_delay -clock vclk 0.2 [get_ports a]\nset_output_delay -clock vclk 0.2 [get_ports y]\nset_input_transition 0.1 [get_ports a]\nset_load 0.01 [get_ports y]\n";
    let cpu =
        EngineAnalysis::analyze_single_corner(&frozen, std::slice::from_ref(lib()), sdc, &options)
            .unwrap();
    let gpu = EngineAnalysis::analyze_single_corner(
        &frozen,
        std::slice::from_ref(lib()),
        sdc,
        &AnalysisOptions {
            backend: BackendKind::Cuda,
            ..options
        },
    )
    .unwrap();
    let (a, b) = (cpu.default_corner(), gpu.default_corner());
    for i in 0..cpu.graph.num_nodes() {
        for tr in Tr::ALL {
            for an in An::ALL {
                let node = NodeId(i as u32);
                assert_eq!(
                    a.slew(node, tr, an).to_bits(),
                    b.slew(node, tr, an).to_bits()
                );
                assert_eq!(
                    a.worst_arrival(node, tr, an)
                        .map(|(tag, v)| (tag, v.to_bits())),
                    b.worst_arrival(node, tr, an)
                        .map(|(tag, v)| (tag, v.to_bits()))
                );
            }
        }
    }
    assert_eq!(
        a.summary().wns_setup_ns.map(f64::to_bits),
        b.summary().wns_setup_ns.map(f64::to_bits)
    );
}

// STA-REQ-0710 to STA-REQ-0713: hand-clock RC latency and bitwise full/cone checks.
#[test]
fn propagated_clock_is_explicit_and_replaces_ideal_latency() {
    use ruststa::engine::{analysis::EngineAnalysis, prop::clocks::ClockModel, types::ClockId};
    let frozen = design().freeze();
    let input = Arc::new(ParasiticInput {
        corner: "default".into(),
        data: spef::read(&source(), spef::Limits::default()).unwrap(),
    });
    let options = AnalysisOptions {
        parasitics: Some(input),
        max_state_bytes: 1 << 24,
        ..Default::default()
    };
    let sdc = "create_clock -name clk -period 5 [get_ports a]\nset_input_transition 0.1 [get_ports a]\nset_clock_latency 9 [get_clocks clk]\nset_clock_latency -source 0.2 [get_clocks clk]\nset_propagated_clock [get_clocks clk]\nset_output_delay -clock clk 0.1 [get_ports y]\n";
    let analysis =
        EngineAnalysis::analyze_single_corner(&frozen, std::slice::from_ref(lib()), sdc, &options)
            .unwrap();
    let ca = analysis.default_corner();
    assert_eq!(ca.clock_model.scope_name(), "propagated");
    let a = ca
        .clock_model
        .arrival(ClockId(0), Tr::Rise, NodeId(2), Tr::Rise, An::Late)
        .unwrap();
    let cap = ca
        .lib
        .cell("wt_buf_1")
        .unwrap()
        .pins
        .iter()
        .find(|p| p.name == "A")
        .unwrap()
        .cap[0][1];
    assert!((a.latency - (0.2 + 2.0 * (0.02 + cap))).abs() < 1e-13);
    assert_eq!(a.slew, 0.1);
    assert!(analysis.diags.iter().any(|d| d.code == "T0076"));
    let mut session =
        TimingSession::new(design(), vec![lib().clone()], sdc, options.clone()).unwrap();
    session.swap_cell_by_name("u", "wt_buf_2").unwrap();
    session.verify_against_full().unwrap();
    let unset = format!("{sdc}\nunset_propagated_clock [get_clocks clk]\n");
    let ideal = EngineAnalysis::analyze_single_corner(
        &frozen,
        std::slice::from_ref(lib()),
        &unset,
        &options,
    )
    .unwrap();
    assert_eq!(ideal.default_corner().clock_model.scope_name(), "ideal");
    let tiny = AnalysisOptions {
        max_state_bytes: 1,
        ..options
    };
    assert!(
        EngineAnalysis::analyze_single_corner(&frozen, std::slice::from_ref(lib()), sdc, &tiny)
            .is_err()
    );
}

// STA-REQ-0713: propagated RC clock seeds reach both compute backends identically.
#[cfg(feature = "cuda")]
#[test]
#[ignore = "requires native CUDA and NVRTC; execute under a memory cgroup"]
fn propagated_extracted_cpu_cuda_agree() {
    use ruststa::engine::{analysis::EngineAnalysis, prop::lower::BackendKind};
    let frozen = design().freeze();
    let input = Arc::new(ParasiticInput {
        corner: "default".into(),
        data: spef::read(&source(), spef::Limits::default()).unwrap(),
    });
    let options = AnalysisOptions {
        parasitics: Some(input),
        max_state_bytes: 1 << 24,
        ..Default::default()
    };
    let sdc = "create_clock -name clk -period 5 [get_ports a]\nset_input_transition 0.1 [get_ports a]\nset_propagated_clock [get_clocks clk]\nset_output_delay -clock clk 0.1 [get_ports y]\n";
    let cpu =
        EngineAnalysis::analyze_single_corner(&frozen, std::slice::from_ref(lib()), sdc, &options)
            .unwrap();
    let gpu = EngineAnalysis::analyze_single_corner(
        &frozen,
        std::slice::from_ref(lib()),
        sdc,
        &AnalysisOptions {
            backend: BackendKind::Cuda,
            ..options
        },
    )
    .unwrap();
    for i in 0..cpu.graph.num_nodes() {
        for tr in Tr::ALL {
            for an in An::ALL {
                let node = NodeId(i as u32);
                assert_eq!(
                    cpu.default_corner().slew(node, tr, an).to_bits(),
                    gpu.default_corner().slew(node, tr, an).to_bits()
                );
                assert_eq!(
                    cpu.default_corner()
                        .worst_arrival(node, tr, an)
                        .map(|(tag, v)| (tag, v.to_bits())),
                    gpu.default_corner()
                        .worst_arrival(node, tr, an)
                        .map(|(tag, v)| (tag, v.to_bits()))
                );
            }
        }
    }
}
