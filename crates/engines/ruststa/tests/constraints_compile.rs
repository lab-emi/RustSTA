// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for the Phase 2a constraint compiler (`crates/engines/ruststa/src/engine/input/constr/`).

use std::path::{Path, PathBuf};

use rusteda_core::diag::Severity;
use rusteda_core::liberty::{
    Cell, Direction, Library, OperatingConditions, Pin, SeqKind, Sequential, TimingArc, Units,
};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc::schema::SCHEMAS;
use rusteda_formats::sdc::{
    AnalysisType, CaseValue, ClockGroupsKind, CmdStatus, EarlyLateScope, EdgeScope, Env, Limits,
    MinMaxScope, SetupHoldScope, Source, SourceRecord, StartEndScope, WireLoadMode,
};
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::input::constr::{
    ClockLatencyTarget, CompiledConstraints, CornerLibs, DisabledTiming, DrcConstraintKind,
    ExceptionKind, MaxTimeBorrowTarget, SynthConstraint,
};
use ruststa::engine::types::{An, ClockId, Tr};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn build_netlist(verilog_src: &str, lib: &Library) -> FrozenNetlist<'static> {
    let cell_pins = |cell_name: &str| -> Option<Vec<(String, PinDir)>> {
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
    };
    read_structural(verilog_src, "<test.v>", &cell_pins)
        .expect("structural verilog")
        .freeze()
}

fn make_pin(name: &str, direction: Direction, is_clock: bool, arcs: Vec<TimingArc>) -> Pin {
    Pin {
        name: name.to_string(),
        direction,
        is_clock,
        arcs,
        ..Pin::default()
    }
}

fn make_arc(related_pin: &str, timing_type: &str) -> TimingArc {
    TimingArc {
        related_pin: related_pin.to_string(),
        timing_type: timing_type.to_string(),
        ..TimingArc::default()
    }
}

fn synthetic_test_lib(name: &str, time_scale: f64, cap_scale: f64) -> Library {
    let mut units = Units::default();
    units.time.scale = time_scale;
    units.capacitance.scale = cap_scale;
    let mut lib = Library {
        name: name.to_string(),
        units,
        default_operating_conditions: Some("typical".to_string()),
        ..Library::default()
    };
    for oc_name in ["typical", "fast_lib", "slow_lib"] {
        lib.operating_conditions.insert(
            oc_name.to_string(),
            OperatingConditions {
                name: oc_name.to_string(),
                process: Some(1.0),
                temperature: Some(25.0),
                voltage: Some(1.0),
                tree_type: None,
            },
        );
    }

    // BUF_X4 (combinational buffer A -> Z)
    lib.cells.push(Cell {
        name: "BUF_X4".to_string(),
        pins: vec![
            make_pin("A", Direction::Input, false, vec![]),
            make_pin(
                "Z",
                Direction::Output,
                false,
                vec![make_arc("A", "combinational")],
            ),
        ],
        ..Cell::default()
    });

    // HOLD_X1 (combinational buffer A -> Z, marked dont_use)
    lib.cells.push(Cell {
        name: "HOLD_X1".to_string(),
        dont_use: true,
        pins: vec![
            make_pin("A", Direction::Input, false, vec![]),
            make_pin(
                "Z",
                Direction::Output,
                false,
                vec![make_arc("A", "combinational")],
            ),
        ],
        ..Cell::default()
    });

    // MUX2_X1 (combinational A, B, S -> Z)
    lib.cells.push(Cell {
        name: "MUX2_X1".to_string(),
        pins: vec![
            make_pin("A", Direction::Input, false, vec![]),
            make_pin("B", Direction::Input, false, vec![]),
            make_pin("S", Direction::Input, false, vec![]),
            make_pin(
                "Z",
                Direction::Output,
                false,
                vec![
                    make_arc("A", "combinational"),
                    make_arc("B", "combinational"),
                    make_arc("S", "combinational"),
                ],
            ),
        ],
        ..Cell::default()
    });

    // AND2_X1 (combinational A, B -> Y)
    lib.cells.push(Cell {
        name: "AND2_X1".to_string(),
        pins: vec![
            make_pin("A", Direction::Input, false, vec![]),
            make_pin("B", Direction::Input, false, vec![]),
            make_pin(
                "Y",
                Direction::Output,
                false,
                vec![
                    make_arc("A", "combinational"),
                    make_arc("B", "combinational"),
                ],
            ),
        ],
        ..Cell::default()
    });

    // DFF_X1 (flip-flop CK, d, RN -> q)
    lib.cells.push(Cell {
        name: "DFF_X1".to_string(),
        sequentials: vec![Sequential {
            kind: SeqKind::FlipFlop,
            state: "IQ".to_string(),
            state_n: "IQN".to_string(),
            data_text: "d".to_string(),
            data: rusteda_core::liberty::Func::Var("d".to_string()),
            clock_text: "CK".to_string(),
            clock: rusteda_core::liberty::Func::Var("CK".to_string()),
            clocked_on_also: None,
            clear_text: Some("!RN".to_string()),
            clear: None,
            preset_text: None,
            preset: None,
            clear_preset_var1: None,
            clear_preset_var2: None,
        }],
        pins: vec![
            make_pin("CK", Direction::Input, true, vec![]),
            make_pin(
                "d",
                Direction::Input,
                false,
                vec![
                    make_arc("CK", "setup_rising"),
                    make_arc("CK", "hold_rising"),
                ],
            ),
            make_pin(
                "RN",
                Direction::Input,
                false,
                vec![
                    make_arc("CK", "recovery_rising"),
                    make_arc("CK", "removal_rising"),
                ],
            ),
            make_pin(
                "q",
                Direction::Output,
                false,
                vec![make_arc("CK", "rising_edge")],
            ),
        ],
        ..Cell::default()
    });

    // DLATCH_X1 (level-sensitive latch en, d -> q)
    lib.cells.push(Cell {
        name: "DLATCH_X1".to_string(),
        sequentials: vec![Sequential {
            kind: SeqKind::Latch,
            state: "IQ".to_string(),
            state_n: "IQN".to_string(),
            data_text: "d".to_string(),
            data: rusteda_core::liberty::Func::Var("d".to_string()),
            clock_text: "en".to_string(),
            clock: rusteda_core::liberty::Func::Var("en".to_string()),
            clocked_on_also: None,
            clear_text: None,
            clear: None,
            preset_text: None,
            preset: None,
            clear_preset_var1: None,
            clear_preset_var2: None,
        }],
        pins: vec![
            make_pin("en", Direction::Input, true, vec![]),
            make_pin(
                "d",
                Direction::Input,
                false,
                vec![
                    make_arc("en", "setup_falling"),
                    make_arc("en", "hold_falling"),
                ],
            ),
            make_pin(
                "q",
                Direction::Output,
                false,
                vec![
                    make_arc("en", "rising_edge"),
                    make_arc("d", "combinational"),
                ],
            ),
        ],
        ..Cell::default()
    });

    lib
}

// STA-REQ-0010, STA-REQ-0020: compile all 8 synthetic STA SDC fixtures against their netlists and wt_15.lib
#[test]
fn compile_all_synthetic_sta_sdc_fixtures() {
    let root = repo_root();
    let sta_dir = root.join("verification/fixtures/synthetic/sta");
    let lib_text = std::fs::read_to_string(sta_dir.join("lib/wt_15.lib")).expect("wt_15.lib");
    let lib = read_text(&lib_text, "wt_15.lib").expect("parse wt_15.lib");
    let libs = [lib];

    let fixtures = [
        ("chain_8", 1usize, 1usize, 1usize),
        ("clock_crossing", 2, 5, 4),
        ("clock_gated", 1, 5, 4),
        ("constant_tied", 1, 4, 4),
        ("dag_32", 1, 4, 2),
        ("fanout_64", 1, 1, 4),
        ("latch_pipeline", 1, 1, 1),
        ("tristate_bus", 1, 6, 2),
    ];

    for (name, exp_clocks, exp_in_delays, exp_out_delays) in fixtures {
        let v_text =
            std::fs::read_to_string(sta_dir.join(format!("net/{name}.v"))).expect("netlist");
        let sdc_path = sta_dir.join(format!("sdc/{name}.sdc"));
        let sdc_text = std::fs::read_to_string(&sdc_path).expect("sdc");

        let netlist = build_netlist(&v_text, &libs[0]);
        let (sdc, parse_diags) = rusteda_formats::sdc::read_str(&sdc_text);
        assert!(parse_diags.is_empty(), "{name}: {parse_diags:?}");

        let compiled = CompiledConstraints::compile_single_corner(&sdc, &netlist, &libs)
            .unwrap_or_else(|e| panic!("{name} compile failed: {e}"));
        assert!(
            compiled.diags.is_empty(),
            "{name} unexpected diags: {:?}",
            compiled.diags
        );
        assert_eq!(compiled.clocks.len(), exp_clocks, "{name} clocks");
        assert_eq!(
            compiled.input_delays.len(),
            exp_in_delays,
            "{name} input_delays"
        );
        assert_eq!(
            compiled.output_delays.len(),
            exp_out_delays,
            "{name} output_delays"
        );
        assert_eq!(
            compiled.input_transitions.len(),
            exp_in_delays,
            "{name} input_transitions"
        );
        assert_eq!(
            compiled.port_loads.len(),
            exp_out_delays,
            "{name} port_loads"
        );

        // Also verify CONSTR v1 binary section round-trip + compilation
        let sections = sdc.to_sections();
        let from_sections =
            CompiledConstraints::from_sections_single_corner(&sections, &netlist, &libs)
                .expect("from_sections_single_corner");
        assert_eq!(compiled.clocks, from_sections.clocks);
        assert_eq!(compiled.input_delays, from_sections.input_delays);
        assert_eq!(compiled.output_delays, from_sections.output_delays);
        assert_eq!(compiled.port_loads, from_sections.port_loads);
        assert_eq!(compiled.input_transitions, from_sections.input_transitions);
    }
}

// SDC-REQ-0001..0055, STA-REQ-0010: compile synthetic/sdc/{basic,tcl_subset,sourced_clocks,exceptions,drc_and_synth}.sdc
#[test]
fn compile_all_five_synthetic_sdc_fixtures() {
    let root = repo_root();
    let sdc_dir = root.join("verification/fixtures/synthetic/sdc");
    let slow_lib = synthetic_test_lib("slow_db", 1.0, 1.0);
    let fast_lib = synthetic_test_lib("fast_db", 1.0, 1.0);
    let libs = [slow_lib, fast_lib];
    let corners = [CornerLibs::new("default", &libs)];

    // 1. basic.sdc
    {
        let netlist = build_netlist(
            r#"
            module top_core (input clk_i, input din_a, input din_b, output dout_a, output dout_b);
              BUF_X4 u0 (.A(din_a), .Z(dout_a));
              BUF_X4 u1 (.A(din_b), .Z(dout_b));
            endmodule
            "#,
            &libs[0],
        );
        let text = std::fs::read_to_string(sdc_dir.join("basic.sdc")).expect("basic.sdc");
        let c = CompiledConstraints::from_sdc_str(&text, "basic.sdc", &netlist, &corners)
            .expect("basic.sdc compile");
        assert_eq!(c.current_design.as_deref(), Some("top_core"));
        assert_eq!(c.explicit_units.time.as_deref(), Some("ns"));
        assert_eq!(c.explicit_units.capacitance.as_deref(), Some("pF"));
        assert_eq!(c.explicit_units.resistance.as_deref(), Some("kOhm"));
        assert_eq!(c.explicit_units.voltage.as_deref(), Some("V"));
        assert_eq!(c.explicit_units.current.as_deref(), Some("mA"));
        assert_eq!(c.explicit_units.power.as_deref(), Some("nW"));
        assert_eq!(c.operating_conditions.len(), 1);
        assert_eq!(
            c.operating_conditions[0].analysis_type,
            Some(AnalysisType::OnChipVariation)
        );
        assert_eq!(c.operating_conditions[0].min.as_deref(), Some("fast_lib"));
        assert_eq!(c.operating_conditions[0].max.as_deref(), Some("slow_lib"));
        assert_eq!(c.wire_load.model_min.as_deref(), Some("wl_small"));
        assert_eq!(c.wire_load.mode, Some(WireLoadMode::Enclosed));
        assert_eq!(c.wire_load.selection_max.as_deref(), Some("area_table"));

        assert_eq!(c.clocks.len(), 2);
        let sys_clk = &c.clocks[0];
        assert_eq!(sys_clk.name, "sys_clk");
        assert_eq!(sys_clk.period_ns, 2.5);
        assert_eq!(sys_clk.waveform_ns, [0.0, 1.25]);
        assert_eq!(sys_clk.sources.len(), 1);
        assert_eq!(sys_clk.uncertainty_hold_ns, [0.04, 0.04]);
        assert_eq!(
            sys_clk.source_latency_ns[An::Early as usize][An::Early as usize][Tr::Rise as usize],
            0.15
        );
        assert_eq!(
            sys_clk.transition_ns[An::Late as usize][Tr::Fall as usize],
            0.08
        );
        let vir_clk = &c.clocks[1];
        assert_eq!(vir_clk.name, "vir_clk");
        assert_eq!(vir_clk.period_ns, 5.0);
        assert!(vir_clk.sources.is_empty());

        assert_eq!(c.inter_clock_uncertainties.len(), 1);
        assert_eq!(c.inter_clock_uncertainties[0].uncertainty_ns, 0.1);
        assert_eq!(
            c.inter_clock_uncertainties[0].setup_hold,
            SetupHoldScope::Setup
        );

        assert_eq!(c.input_delays.len(), 2);
        for d in &c.input_delays {
            assert_eq!(d.min_max_pair(), [0.15, 0.6]);
        }
        assert_eq!(c.output_delays.len(), 2);
        for d in &c.output_delays {
            assert_eq!(d.min_max_pair(), [0.1, 0.75]);
        }
        assert_eq!(c.input_transitions.len(), 2);
        assert_eq!(c.driving_cells.len(), 1);
        let dc = c.driving_cells.values().next().unwrap();
        assert_eq!(dc[0].lib_cell, "BUF_X4");
        assert_eq!(dc[0].pin, "Z");
        assert_eq!(dc[0].from_pin.as_deref(), Some("A"));
        assert_eq!(dc[0].input_transition_rise_ns, Some(0.05));
        assert_eq!(dc[0].input_transition_fall_ns, Some(0.06));
        assert_eq!(c.port_loads.len(), 2);
    }

    // 2. tcl_subset.sdc + sourced_clocks.sdc
    {
        let netlist = build_netlist(
            r#"
            module tcl_top (input clk_i, input din_0, input din_1, output dout_0);
              DFF_X1 r0 (.CK(clk_i), .d(din_0), .RN(din_1), .q(dout_0));
            endmodule
            "#,
            &libs[0],
        );
        let tcl_path = sdc_dir.join("tcl_subset.sdc");
        let tcl_text = std::fs::read_to_string(&tcl_path).expect("tcl_subset.sdc");
        let src = Source::new(tcl_path.to_string_lossy().to_string(), tcl_text);
        let env = Env::from_pairs([("SYNTH_CAP_LOAD", "50")]);
        let c = CompiledConstraints::from_sdc_source(
            &src,
            &env,
            &Limits::default(),
            &netlist,
            &corners,
        )
        .expect("tcl_subset.sdc compile");
        assert_eq!(c.sources.len(), 2);
        assert_eq!(c.sources[0].flags, 0);
        assert_eq!(c.sources[1].flags, SourceRecord::FLAG_SOURCED);
        assert_eq!(c.clocks.len(), 1);
        assert_eq!(c.clocks[0].name, "core_clk");
        assert_eq!(c.clocks[0].period_ns, 4.0);
        assert_eq!(c.input_delays.len(), 2);
        for d in &c.input_delays {
            assert!((d.min_max_pair()[1] - 0.8).abs() < 1e-12);
        }
        assert_eq!(c.output_delays.len(), 1);
        assert!((c.output_delays[0].min_max_pair()[1] - 1.2).abs() < 1e-12);
        let pl = c.port_loads.values().next().unwrap();
        assert!((pl.load_pf[0] - 0.05).abs() < 1e-12);
        assert!(pl.pin_load);
    }

    // 3. exceptions.sdc
    {
        let netlist = build_netlist(
            r#"
            module exc_top (
              input clk_a, input clk_b, input rst_n, input async_in,
              input scan_en, input tie_lo, input tie_hi,
              output out_a, output out_b
            );
              wire n_slow, n_mux, n_lat;
              DFF_X1 \u_slow/r0 (.CK(clk_a), .d(async_in), .RN(rst_n), .q(n_slow));
              DFF_X1 \u_dst/r0 (.CK(clk_b), .d(n_slow), .RN(rst_n), .q(out_a));
              DFF_X1 u_sync (.CK(clk_b), .d(async_in), .RN(rst_n), .q(n_mux));
              MUX2_X1 u_mux (.A(n_mux), .B(tie_lo), .S(scan_en), .Z(n_lat));
              DLATCH_X1 u_latch (.en(clk_a), .d(n_lat), .q(out_b));
            endmodule
            "#,
            &libs[0],
        );
        let text = std::fs::read_to_string(sdc_dir.join("exceptions.sdc")).expect("exceptions.sdc");
        let c = CompiledConstraints::from_sdc_str(&text, "exceptions.sdc", &netlist, &corners)
            .expect("exceptions.sdc compile");
        assert_eq!(c.clocks.len(), 2);
        assert_eq!(c.clock_groups.len(), 1);
        assert_eq!(c.clock_groups[0].kind, ClockGroupsKind::Asynchronous);
        assert_eq!(c.clock_groups[0].name.as_deref(), Some("async_ab"));
        assert_eq!(
            c.clock_groups[0].groups,
            vec![vec![ClockId(0)], vec![ClockId(1)]]
        );
        assert_eq!(c.exceptions.len(), 6);
        assert!(matches!(
            c.exceptions[0].kind,
            ExceptionKind::FalsePath {
                setup_hold: SetupHoldScope::Setup
            }
        ));
        assert!(matches!(
            c.exceptions[1].kind,
            ExceptionKind::FalsePath {
                setup_hold: SetupHoldScope::Hold
            }
        ));
        assert_eq!(c.exceptions[1].from.as_ref().unwrap().edge, EdgeScope::Rise);
        assert_eq!(c.exceptions[1].to.as_ref().unwrap().edge, EdgeScope::Fall);
        assert!(matches!(
            c.exceptions[2].kind,
            ExceptionKind::MulticyclePath {
                multiplier: 2,
                setup_multiplier: Some(2),
                hold_multiplier: None,
                setup_hold: SetupHoldScope::Setup,
                start_end: StartEndScope::End,
            }
        ));
        assert!(matches!(
            c.exceptions[3].kind,
            ExceptionKind::MulticyclePath {
                multiplier: 1,
                setup_multiplier: None,
                hold_multiplier: Some(1),
                setup_hold: SetupHoldScope::Hold,
                start_end: StartEndScope::End,
            }
        ));
        assert!(matches!(
            c.exceptions[4].kind,
            ExceptionKind::MaxDelay { delay_ns, .. } if (delay_ns - 1.5).abs() < 1e-12
        ));
        assert!(matches!(
            c.exceptions[5].kind,
            ExceptionKind::MinDelay { delay_ns, .. } if (delay_ns - 0.2).abs() < 1e-12
        ));
        assert_eq!(c.case_analysis.len(), 3);
        assert_eq!(c.logic_zero_ports.len(), 1);
        assert_eq!(c.logic_one_ports.len(), 1);
        assert_eq!(c.disabled_timing.len(), 1);
        assert!(matches!(
            &c.disabled_timing[0],
            DisabledTiming::CellArc { from_pin, to_pin, .. } if from_pin == "A" && to_pin == "Z"
        ));
        assert_eq!(c.max_time_borrow.len(), 1);
        assert!((c.max_time_borrow[0].max_borrow_ns - 0.4).abs() < 1e-12);
        assert_eq!(c.path_groups.len(), 1);
        assert_eq!(c.path_groups[0].name.as_deref(), Some("reg2reg"));
        assert_eq!(c.path_groups[0].weight, Some(2.0));
        assert_eq!(c.path_groups[0].critical_range_ns, Some(0.3));
    }

    // 4. drc_and_synth.sdc
    {
        let netlist = build_netlist(
            r#"
            module synth_top (input clk, input in_a, output out_a);
              wire mid;
              BUF_X4 u_keep (.A(in_a), .Z(mid));
              BUF_X4 u_size (.A(mid), .Z(out_a));
            endmodule
            "#,
            &libs[0],
        );
        let text =
            std::fs::read_to_string(sdc_dir.join("drc_and_synth.sdc")).expect("drc_and_synth.sdc");
        let c = CompiledConstraints::from_sdc_str(&text, "drc_and_synth.sdc", &netlist, &corners)
            .expect("drc_and_synth.sdc compile");
        assert_eq!(c.current_design.as_deref(), Some("synth_top"));
        assert_eq!(c.drc_constraints.len(), 6);
        assert_eq!(c.synth_constraints.len(), 7);
        // STA-REQ-0805: the limit checks analyze set_max_transition, set_max_capacitance,
        // set_max_fanout and set_fanout_load; set_min_capacitance and set_port_fanout_number stay
        // recorded with `D0021`.
        let d0021: Vec<_> = c.diags.iter().filter(|d| d.code == "D0021").collect();
        let d0020_count = c.diags.iter().filter(|d| d.code == "D0020").count();
        assert_eq!(d0021.len(), 2, "{d0021:?}");
        assert!(
            d0021
                .iter()
                .all(|d| d.message.contains("set_min_capacitance")
                    || d.message.contains("set_port_fanout_number"))
        );
        assert_eq!(d0020_count, 7);
    }
}

// STA-REQ-0010: Table-driven test over rusteda_formats::sdc::schema::SCHEMAS verifying that every
// Phase 2a command and every non-Phase-2b option is compiled and reaches a field in CompiledConstraints.
#[test]
fn schema_table_driven_phase2a_coverage() {
    let lib = synthetic_test_lib("slow_db", 1.0, 1.0);
    let libs = [lib];
    let corners = [CornerLibs::new("default", &libs)];
    let netlist = build_netlist(
        r#"
        module top (
          input clk_a, input clk_b, input [1:0] din, input rst_n, input sel,
          output [1:0] dout
        );
          wire n0, n1, gclk;
          AND2_X1 u_cg (.A(clk_a), .B(sel), .Y(gclk));
          DFF_X1 u_ff0 (.CK(clk_a), .d(din[0]), .RN(rst_n), .q(n0));
          DFF_X1 u_ff1 (.CK(clk_b), .d(din[1]), .RN(rst_n), .q(n1));
          MUX2_X1 u_mux (.A(n0), .B(n1), .S(sel), .Z(dout[0]));
          DLATCH_X1 u_lat (.en(gclk), .d(n0), .q(dout[1]));
        endmodule
        "#,
        &libs[0],
    );

    // Confirm the exact counts in SCHEMAS so no command or option can be silently added/missed.
    let mut interpreted_cmds = 0usize;
    let mut drc_cmds = 0usize;
    let mut synth_cmds = 0usize;
    let mut phase2b_cmds = 0usize;
    let mut phase2a_opts = 0usize;
    let mut phase2b_opts = 0usize;

    for schema in SCHEMAS {
        match schema.status {
            CmdStatus::Interpreted => interpreted_cmds += 1,
            CmdStatus::RecordedDrc => drc_cmds += 1,
            CmdStatus::RecordedNoTimingEffect => synth_cmds += 1,
            CmdStatus::Phase2b | CmdStatus::Unsupported => phase2b_cmds += 1,
        }
        if schema.status != CmdStatus::Phase2b && schema.status != CmdStatus::Unsupported {
            for opt in schema.options {
                if opt.phase2b {
                    phase2b_opts += 1;
                } else {
                    phase2a_opts += 1;
                }
            }
        }
    }
    assert_eq!(interpreted_cmds, 28);
    assert_eq!(drc_cmds, 6);
    assert_eq!(synth_cmds, 7);
    assert_eq!(phase2b_cmds, 10);
    assert_eq!(phase2b_opts, 22);
    assert!(phase2a_opts >= 130);

    // Exercise all 39 Phase 2a commands and all their non-Phase-2b options.
    let full_phase2a_script = r#"
        set_units -time ns -capacitance pF -resistance kOhm -voltage V -current mA -power mW
        current_design top
        set_operating_conditions -analysis_type bc_wc -library slow_db -min fast_lib -max slow_lib -min_library slow_db -max_library slow_db -object_list [current_design] typical
        set_wire_load_model -name wl_a -library slow_db -min [current_design]
        set_wire_load_model -name wl_b -library slow_db -max [current_design]
        set_wire_load_mode top
        set_wire_load_selection_group -library slow_db -min sel_min [current_design]
        set_wire_load_selection_group -library slow_db -max sel_max [current_design]

        create_clock -name clk_a -period 2.0 -waveform {0.0 1.0} -comment "main" [get_ports clk_a]
        create_clock -name clk_b -period 4.0 -waveform {0.5 2.5} -add -comment "sec" [get_ports clk_b]
        set_clock_groups -physically_exclusive -name cg_phys -comment "p" -group [get_clocks clk_a] -group [get_clocks clk_b]
        set_clock_groups -logically_exclusive -allow_paths -name cg_log -group [get_clocks clk_a] -group [get_clocks clk_b]
        set_clock_groups -asynchronous -name cg_async -group [get_clocks clk_a] -group [get_clocks clk_b]

        set_clock_uncertainty -setup -rise_from [get_clocks clk_a] -fall_to [get_clocks clk_b] 0.12
        set_clock_uncertainty -hold -fall_from [get_clocks clk_b] -rise_to [get_clocks clk_a] 0.05
        set_clock_uncertainty -from [get_clocks clk_a] -to [get_clocks clk_a] 0.08
        set_clock_uncertainty -setup 0.09 [get_ports clk_a]
        set_clock_uncertainty -hold 0.03 [get_pins u_ff0/CK]

        set_clock_latency -source -early -rise -min -clock [get_clocks clk_a] 0.11 [get_ports clk_a]
        set_clock_latency -source -late -fall -max -clock [get_clocks clk_a] 0.22 [get_clocks clk_a]
        set_clock_latency -rise -max 0.33 [get_clocks clk_b]
        set_clock_transition -rise -min 0.04 [get_clocks clk_a]
        set_clock_transition -fall -max 0.09 [get_clocks clk_a]

        set_input_delay -clock [get_clocks clk_a] -clock_fall -level_sensitive -rise -max -add_delay -network_latency_included -source_latency_included 0.45 [get_ports {din[0]}]
        set_input_delay -clock [get_clocks clk_a] -fall -min -add_delay 0.15 [get_ports {din[0]}]
        set_output_delay -clock [get_clocks clk_b] -clock_fall -level_sensitive -rise -max -add_delay -network_latency_included -source_latency_included 0.55 [get_ports {dout[0]}]
        set_output_delay -clock [get_clocks clk_b] -fall -min -add_delay 0.20 [get_ports {dout[0]}]

        set_input_transition -rise -min -clock [get_clocks clk_a] -clock_fall 0.03 [get_ports {din[0]}]
        set_input_transition -fall -max -clock [get_clocks clk_a] 0.07 [get_ports {din[0]}]
        set_driving_cell -lib_cell BUF_X4 -library slow_db -pin Z -from_pin A -rise -min -input_transition_rise 0.04 -input_transition_fall 0.05 -multiply_by 2.0 -dont_scale -no_design_rule -clock [get_clocks clk_a] -clock_fall [get_ports {din[1]}]
        set_driving_cell -lib_cell BUF_X4 -fall -max [get_ports {din[1]}]

        set_load -pin_load -min 0.01 [get_ports {dout[0]}]
        set_load -wire_load -subtract_pin_load -max 0.03 [get_nets n0]

        set_false_path -setup -rise -from [get_clocks clk_a] -to [get_clocks clk_b] -comment "fp1"
        set_false_path -hold -fall -rise_from [get_ports {din[0]}] -fall_to [get_pins u_ff0/d]
        set_false_path -fall_from [get_pins u_ff0/CK] -rise_to [get_ports {dout[0]}]
        set_multicycle_path -setup -start -rise -from [get_clocks clk_a] -to [get_clocks clk_b] -comment "mcp1" 3
        set_multicycle_path -hold -end -fall -rise_from [get_cells u_ff0] -fall_to [get_cells u_ff1] 2
        set_max_delay -rise -ignore_clock_latency -from [get_ports {din[0]}] -to [get_ports {dout[0]}] -comment "maxd" 1.8
        set_min_delay -fall -ignore_clock_latency -from [get_ports {din[0]}] -to [get_ports {dout[0]}] -comment "mind" 0.2

        set_case_analysis 1 [get_ports sel]
        set_logic_zero [get_ports rst_n]
        set_logic_one [get_ports {din[1]}]
        set_disable_timing -from A -to Z [get_cells u_mux]
        set_disable_timing [get_pins u_mux/B]
        set_disable_timing [get_lib_cells slow_db/HOLD_X1]
        set_disable_timing [get_lib_pins slow_db/HOLD_X1/Z]
        set_max_time_borrow 0.35 [get_cells u_lat]
        set_max_time_borrow 0.40 [get_pins u_cg/B]
        group_path -default -weight 1.0 -critical_range 0.1 -from [get_clocks clk_a] -comment "def"
        group_path -name gp_io -weight 2.5 -critical_range 0.2 -rise_from [get_ports {din[*]}] -fall_to [get_ports {dout[*]}]

        set_max_transition -clock_path -rise 0.25 [get_clocks clk_a]
        set_max_transition -data_path -fall 0.35 [current_design]
        set_max_capacitance -clock_path -rise 0.15 [current_design]
        set_max_capacitance -data_path -fall 0.20 [all_outputs]
        set_min_capacitance -clock_path -rise 0.002 [all_inputs -no_clocks]
        set_min_capacitance -data_path -fall 0.004 [all_inputs -no_clocks]
        set_max_fanout 12.0 [current_design]
        set_fanout_load 1.5 [all_outputs]
        set_port_fanout_number 3 [all_inputs -no_clocks]

        set_dont_use [get_lib_cells slow_db/HOLD_X1]
        set_dont_touch [get_cells u_mux]
        set_dont_touch_network -no_propagate [get_ports clk_a]
        set_size_only -no_instances [get_cells u_ff0]
        set_max_area -ignore_tns 500.0
        set_max_dynamic_power -unit mW 10.0
        set_max_leakage_power -unit uW 50.0
    "#;

    let c =
        CompiledConstraints::from_sdc_str(full_phase2a_script, "schema.sdc", &netlist, &corners)
            .expect("schema script compiles without error");
    assert_eq!(c.clocks.len(), 2);
    assert_eq!(c.clock_groups.len(), 3);
    assert_eq!(c.inter_clock_uncertainties.len(), 3);
    assert_eq!(c.pin_clock_uncertainties.len(), 2);
    assert_eq!(c.clock_latencies.len(), 3);
    assert!(matches!(
        c.clock_latencies[0].target,
        ClockLatencyTarget::Node(_)
    ));
    assert_eq!(c.clock_latencies[0].early_late, EarlyLateScope::Early);
    assert_eq!(c.clock_latencies[0].min_max, MinMaxScope::Min);
    assert_eq!(c.input_delays.len(), 2);
    assert!(c.input_delays[0].level_sensitive);
    assert!(c.input_delays[0].network_latency_included);
    assert!(c.input_delays[0].source_latency_included);
    assert_eq!(c.output_delays.len(), 2);
    assert_eq!(c.input_transitions.len(), 1);
    assert_eq!(c.driving_cells.len(), 1);
    assert_eq!(c.port_loads.len(), 1);
    assert_eq!(c.net_loads.len(), 1);
    assert_eq!(c.exceptions.len(), 7);
    assert_eq!(c.case_analysis.len(), 3);
    assert_eq!(c.disabled_timing.len(), 4);
    assert_eq!(c.max_time_borrow.len(), 2);
    assert!(matches!(
        c.max_time_borrow[0].target,
        MaxTimeBorrowTarget::Cell(_)
    ));
    assert_eq!(c.path_groups.len(), 2);
    assert_eq!(c.drc_constraints.len(), 9);
    assert!(matches!(
        c.drc_constraints[0].kind,
        DrcConstraintKind::MaxTransition {
            clock_path: true,
            ..
        }
    ));
    assert_eq!(c.synth_constraints.len(), 7);
    assert!(matches!(
        c.synth_constraints[3],
        SynthConstraint::SizeOnly {
            no_instances: true,
            ..
        }
    ));
}

// STA-REQ-0020, STA-REQ-0212, STA-REQ-0268: Object queries, -filter, -of_objects, -regexp,
// -hierarchical, -nocase, assign-bridged nets, and inferred clock-gating enable pins.
#[test]
fn object_queries_filters_assign_equivalence_and_inferred_clock_gating() {
    let lib = synthetic_test_lib("slow_db", 1.0, 1.0);
    let libs = [lib];
    let corners = [CornerLibs::new("default", &libs)];
    let netlist = build_netlist(
        r#"
        module top (
          input clk, input cg_en, input [1:0] data_bus, input rst_n,
          output [1:0] out_bus
        );
          wire alias_net, gclk, q0;
          assign alias_net = data_bus[0];
          AND2_X1 u_cg_gate (.A(clk), .B(cg_en), .Y(gclk));
          DFF_X1 \sub/ff_0 (.CK(gclk), .d(alias_net), .RN(rst_n), .q(q0));
          DLATCH_X1 \sub/lat_0 (.en(clk), .d(data_bus[1]), .q(out_bus[0]));
          BUF_X4 u_buf (.A(q0), .Z(out_bus[1]));
        endmodule
        "#,
        &libs[0],
    );

    let sdc_text = r#"
        create_clock -name clk -period 2.0 [get_ports clk]

        # Numeric bus subscript [0] must match literally, while character range [0-1] matches both bits
        set_input_delay -clock clk 0.2 [get_ports {data_bus[0]}]
        set_output_delay -clock clk 0.3 [get_ports {out_bus[0-1]}]

        # -hierarchical, -regexp, -nocase, and -filter on cells and pins
        set_dont_touch [get_cells -hierarchical -filter "is_sequential == true && ref_name =~ DFF*"]
        set_dont_touch [get_cells -regexp -nocase {^sub/lat_[0-9]+$}]
        set_case_analysis 0 [get_pins -of_objects [get_cells -hierarchical ff_0] -filter "is_async_pin == true"]

        # -of_objects across assign-aliased nets (alias_net == data_bus[0])
        set_disable_timing [get_pins -of_objects [get_nets alias_net] -filter "direction == in"]

        # all_registers variants
        set_false_path -from [all_registers -edge_triggered -output_pins] -to [all_registers -level_sensitive -data_pins]

        # Inferred clock-gating enable pin: u_cg_gate/B gates clk -> gclk, so it is a valid -to endpoint!
        set_max_delay -from [get_ports cg_en] -to [get_pins u_cg_gate/B] 0.8
        set_min_delay -from [get_ports cg_en] -to [get_cells u_cg_gate] 0.1
    "#;

    let c = CompiledConstraints::from_sdc_str(sdc_text, "queries.sdc", &netlist, &corners)
        .expect("queries.sdc compile");
    // No D0102 warning should be emitted for u_cg_gate/B or u_cg_gate!
    let d0102_diags: Vec<_> = c.diags.iter().filter(|d| d.code == "D0102").collect();
    assert!(
        d0102_diags.is_empty(),
        "unexpected D0102 diags: {d0102_diags:?}"
    );
    assert_eq!(c.input_delays.len(), 1);
    assert_eq!(c.output_delays.len(), 2);
    assert_eq!(c.case_analysis.len(), 1);
    assert_eq!(
        c.case_analysis.values().next().copied(),
        Some(CaseValue::Zero)
    );
    assert_eq!(c.disabled_timing.len(), 1);
    assert_eq!(c.exceptions.len(), 3);
}

// STA-REQ-0010, STA-REQ-0011, STA-REQ-0020: Diagnostic codes and locations
// (D0101, D0102, D0103, D0104, D0105, D0110, D0113, D0114, D0010, D0011)
#[test]
fn diagnostics_codes_severities_and_source_locations() {
    let lib_ns = synthetic_test_lib("slow_db", 1.0, 1.0);
    let lib_ps = synthetic_test_lib("ps_db", 1e-3, 1e-3);
    let netlist = build_netlist(
        r#"
        module top (input clk, input din, output dout);
          wire mid;
          DFF_X1 u_ff (.CK(clk), .d(din), .RN(din), .q(mid));
          BUF_X4 u_buf (.A(mid), .Z(dout));
        endmodule
        "#,
        &lib_ns,
    );

    // 1. D0101 error vs -quiet D0101 warning with exact file:line:col
    {
        let script = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                      set_input_delay -clock clk 0.2 [get_ports no_such_port]\n\
                      set_input_delay -clock clk 0.2 [get_ports -quiet quiet_missing]\n";
        let sources = [Source::new("test_d0101.sdc", script)];
        let (sdc, _) = rusteda_formats::sdc::read(&sources, &Env::default(), &Limits::default());
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let err = CompiledConstraints::compile(&sdc, &netlist, &corners).unwrap_err();
        let d_err = err
            .diags
            .iter()
            .find(|d| d.code == "D0101" && d.severity == Severity::Error)
            .expect("D0101 error");
        let loc = d_err.location.as_ref().expect("location");
        assert_eq!(loc.file, "test_d0101.sdc");
        assert_eq!(loc.line, 2);

        let d_warn = err
            .diags
            .iter()
            .find(|d| d.code == "D0101" && d.severity == Severity::Warning)
            .expect("D0101 warning for -quiet");
        assert_eq!(d_warn.location.as_ref().unwrap().line, 3);
    }

    // 2. D0102 error (set_input_delay on output port, set_output_delay on input port)
    // and D0102 warning + D0110 warning (non-startpoint in -from / non-endpoint in -to)
    {
        let script = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                      set_input_delay -clock clk 0.2 [get_ports dout]\n\
                      set_output_delay -clock clk 0.2 [get_ports din]\n\
                      set_false_path -from [get_ports dout] -to [get_ports din]\n";
        let (sdc, _) = rusteda_formats::sdc::read_str(script);
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let c = CompiledConstraints::compile_lenient(&sdc, &netlist, &corners);
        let d0102_errs: Vec<_> = c
            .diags
            .iter()
            .filter(|d| d.code == "D0102" && d.severity == Severity::Error)
            .collect();
        assert_eq!(d0102_errs.len(), 2);
        let d0102_warns: Vec<_> = c
            .diags
            .iter()
            .filter(|d| d.code == "D0102" && d.severity == Severity::Warning)
            .collect();
        assert_eq!(d0102_warns.len(), 2);
        assert!(
            c.diags
                .iter()
                .any(|d| d.code == "D0110" && d.severity == Severity::Warning)
        );
    }

    // 2b. STA-REQ-0255: an option whose objects are all ignored makes the exception match no
    // path even when the other option has usable objects; it is not widened to that option.
    {
        let script = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                      set_false_path -from [get_ports dout] -to [get_ports dout]\n";
        let (sdc, _) = rusteda_formats::sdc::read_str(script);
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let c = CompiledConstraints::compile_lenient(&sdc, &netlist, &corners);
        assert!(
            c.exceptions.is_empty(),
            "the exception must match no path: {:?}",
            c.exceptions
        );
        assert!(
            c.diags
                .iter()
                .any(|d| d.code == "D0110" && d.severity == Severity::Warning)
        );
    }

    // 3. D0103 library unit disagreement when set_units is omitted
    {
        let (sdc, _) =
            rusteda_formats::sdc::read_str("create_clock -name clk -period 2.0 [get_ports clk]");
        let mixed_libs = [lib_ns.clone(), lib_ps.clone()];
        let corners = [CornerLibs::new("default", &mixed_libs)];
        let err = CompiledConstraints::compile(&sdc, &netlist, &corners).unwrap_err();
        assert!(
            err.diags
                .iter()
                .any(|d| d.code == "D0103" && d.severity == Severity::Error)
        );
    }

    // 4. D0104 current_design mismatch & D0105 unknown operating condition
    {
        let script = "current_design wrong_top\n\
                      set_operating_conditions no_such_cond\n";
        let (sdc, _) = rusteda_formats::sdc::read_str(script);
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let err = CompiledConstraints::compile(&sdc, &netlist, &corners).unwrap_err();
        assert!(err.diags.iter().any(|d| d.code == "D0104"));
        assert!(err.diags.iter().any(|d| d.code == "D0105"));
    }

    // 5. D0113 (-setup and -hold together on set_multicycle_path) and D0114 (-allow_paths on set_clock_groups)
    {
        let script = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                      set_multicycle_path -setup -hold -from [get_ports din] -to [get_ports dout] 2\n\
                      set_clock_groups -asynchronous -allow_paths -group [get_clocks clk]\n";
        let (sdc, _) = rusteda_formats::sdc::read_str(script);
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let c =
            CompiledConstraints::compile(&sdc, &netlist, &corners).expect("warnings/notes only");
        assert!(
            c.diags
                .iter()
                .any(|d| d.code == "D0113" && d.severity == Severity::Warning)
        );
        assert!(
            c.diags
                .iter()
                .any(|d| d.code == "D0114" && d.severity == Severity::Note)
        );
        // Hold multiplier must remain None when both -setup and -hold are on the same command
        assert!(matches!(
            c.exceptions[0].kind,
            ExceptionKind::MulticyclePath {
                setup_multiplier: Some(2),
                hold_multiplier: None,
                ..
            }
        ));
    }

    // 6. D0010 (Phase 2b command) and D0011 (Phase 2b option)
    {
        let script = "create_clock -name clk -period 2.0 [get_ports clk]\n\
                      set_timing_derate -early 0.95\n\
                      set_false_path -through [get_pins u_buf/A]\n";
        let (sdc, _) = rusteda_formats::sdc::read_str(script);
        let corners = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
        let err = CompiledConstraints::compile(&sdc, &netlist, &corners).unwrap_err();
        assert!(err.diags.iter().any(|d| d.code == "D0010"));
        assert!(err.diags.iter().any(|d| d.code == "D0011"));
    }
}

// STA-REQ-0011, SDC-REQ-0004: Unit invariance across (1) explicit ps/fF, (2) explicit ns/pF,
// and (3) library default units on a 1ps/1fF library (within 1e-12 relative error).
#[test]
fn unit_scaling_invariance_ps_ff_vs_ns_pf_vs_library_units() {
    let lib_ns = synthetic_test_lib("ns_lib", 1.0, 1.0);
    let lib_ps = synthetic_test_lib("ps_lib", 1e-3, 1e-3);
    let netlist = build_netlist(
        r#"
        module top (input clk, input din, output dout);
          DFF_X1 u_ff (.CK(clk), .d(din), .RN(din), .q(dout));
        endmodule
        "#,
        &lib_ns,
    );

    let sdc_ns_pf = r#"
        set_units -time ns -capacitance pF
        create_clock -name clk -period 2.5 -waveform {0.0 1.25} [get_ports clk]
        set_clock_uncertainty -setup 0.12 [get_clocks clk]
        set_clock_latency 0.08 [get_clocks clk]
        set_clock_transition 0.05 [get_clocks clk]
        set_input_delay -clock clk 0.45 [get_ports din]
        set_output_delay -clock clk 0.55 [get_ports dout]
        set_input_transition 0.06 [get_ports din]
        set_load 0.025 [get_ports dout]
        set_max_delay -from [get_ports din] -to [get_ports dout] 1.75
    "#;

    let sdc_ps_ff = r#"
        set_units -time ps -capacitance fF
        create_clock -name clk -period 2500.0 -waveform {0.0 1250.0} [get_ports clk]
        set_clock_uncertainty -setup 120.0 [get_clocks clk]
        set_clock_latency 80.0 [get_clocks clk]
        set_clock_transition 50.0 [get_clocks clk]
        set_input_delay -clock clk 450.0 [get_ports din]
        set_output_delay -clock clk 550.0 [get_ports dout]
        set_input_transition 60.0 [get_ports din]
        set_load 25.0 [get_ports dout]
        set_max_delay -from [get_ports din] -to [get_ports dout] 1750.0
    "#;

    let sdc_lib_ps = r#"
        create_clock -name clk -period 2500.0 -waveform {0.0 1250.0} [get_ports clk]
        set_clock_uncertainty -setup 120.0 [get_clocks clk]
        set_clock_latency 80.0 [get_clocks clk]
        set_clock_transition 50.0 [get_clocks clk]
        set_input_delay -clock clk 450.0 [get_ports din]
        set_output_delay -clock clk 550.0 [get_ports dout]
        set_input_transition 60.0 [get_ports din]
        set_load 25.0 [get_ports dout]
        set_max_delay -from [get_ports din] -to [get_ports dout] 1750.0
    "#;

    let corners_ns = [CornerLibs::new("default", std::slice::from_ref(&lib_ns))];
    let corners_ps = [CornerLibs::new("default", std::slice::from_ref(&lib_ps))];

    let c1 = CompiledConstraints::from_sdc_str(sdc_ns_pf, "ns.sdc", &netlist, &corners_ps)
        .expect("c1 compile");
    let c2 = CompiledConstraints::from_sdc_str(sdc_ps_ff, "ps.sdc", &netlist, &corners_ns)
        .expect("c2 compile");
    let c3 = CompiledConstraints::from_sdc_str(sdc_lib_ps, "lib_ps.sdc", &netlist, &corners_ps)
        .expect("c3 compile");

    let assert_close = |a: f64, b: f64, label: &str| {
        let rel = (a - b).abs() / a.abs().max(b.abs()).max(1e-30);
        assert!(rel <= 1e-12, "{label}: {a} vs {b} (rel {rel})");
    };

    for (other, tag) in [(&c2, "ps_ff"), (&c3, "lib_ps")] {
        assert_close(c1.clocks[0].period_ns, other.clocks[0].period_ns, tag);
        assert_close(
            c1.clocks[0].waveform_ns[1],
            other.clocks[0].waveform_ns[1],
            tag,
        );
        assert_close(
            c1.clocks[0].uncertainty_setup_ns[0],
            other.clocks[0].uncertainty_setup_ns[0],
            tag,
        );
        assert_close(
            c1.clocks[0].network_latency_ns[1][0],
            other.clocks[0].network_latency_ns[1][0],
            tag,
        );
        assert_close(
            c1.clocks[0].transition_ns[1][0],
            other.clocks[0].transition_ns[1][0],
            tag,
        );
        assert_close(
            c1.input_delays[0].min_max_pair()[1],
            other.input_delays[0].min_max_pair()[1],
            tag,
        );
        assert_close(
            c1.output_delays[0].min_max_pair()[1],
            other.output_delays[0].min_max_pair()[1],
            tag,
        );
        for (&nid, it) in &c1.input_transitions {
            assert_close(
                it.min_max_pair()[1],
                other.input_transitions[&nid].min_max_pair()[1],
                tag,
            );
        }
        for (&nid, pl) in &c1.port_loads {
            assert_close(pl.load_pf[1], other.port_loads[&nid].load_pf[1], tag);
        }
        match (&c1.exceptions[0].kind, &other.exceptions[0].kind) {
            (
                ExceptionKind::MaxDelay { delay_ns: d1, .. },
                ExceptionKind::MaxDelay { delay_ns: d2, .. },
            ) => assert_close(*d1, *d2, tag),
            _ => panic!("expected MaxDelay"),
        }
    }
}
