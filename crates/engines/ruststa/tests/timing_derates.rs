// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Global OCV and mixed-library slew regressions, with independent OpenSTA evidence.

mod support;

use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{GateNetlist, PinDir};
use rusteda_formats::liberty::read_text;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::incr::{SessionEdit, TimingSession};
use ruststa::engine::types::{An, NodeId, Tr};

const NETLIST: &str =
    "module top(input a, output y); wire n; A u1(.I(a), .Z(n)); B u2(.I(n), .Z(y)); endmodule";
const SDC: &str = "create_clock -name clk -period 10\nset_input_delay 0 -clock clk [get_ports a]\nset_output_delay 0 -clock clk [get_ports y]\nset_input_transition 0.12 [get_ports a]\n";

fn library(cell: &str, low: f64, high: f64, derate: f64) -> Library {
    let text = r#"library (LIB) {
      time_unit : "1ns"; capacitive_load_unit (1,pf);
      input_threshold_pct_rise : 50; input_threshold_pct_fall : 50;
      output_threshold_pct_rise : 50; output_threshold_pct_fall : 50;
      slew_lower_threshold_pct_rise : LOW; slew_lower_threshold_pct_fall : LOW;
      slew_upper_threshold_pct_rise : HIGH; slew_upper_threshold_pct_fall : HIGH;
      slew_derate_from_library : DERATE;
      lu_table_template (t) { variable_1 : input_net_transition;
        variable_2 : total_output_net_capacitance; index_1 ("0,1"); index_2 ("0,1"); }
      cell (CELL) { area : 1;
        pin (I) { direction : input; capacitance : 0.01; }
        pin (Z) { direction : output; function : "I";
          timing () { related_pin : "I"; timing_sense : positive_unate;
            timing_type : combinational;
            cell_rise (t) { values ("0.1,0.1", "0.6,0.6"); }
            cell_fall (t) { values ("0.1,0.1", "0.6,0.6"); }
            rise_transition (t) { values ("0.3,0.3", "0.5,0.5"); }
            fall_transition (t) { values ("0.3,0.3", "0.5,0.5"); }
          }
        }
      }
    }"#
    .replace("LIB", &format!("lib_{cell}"))
    .replace("CELL", cell)
    .replace("LOW", &low.to_string())
    .replace("HIGH", &high.to_string())
    .replace("DERATE", &derate.to_string());
    read_text(&text, "mixed.lib").expect("valid synthetic library")
}

fn netlist(libs: &[Library]) -> GateNetlist {
    netlist_text(NETLIST, libs)
}

fn netlist_text(text: &str, libs: &[Library]) -> GateNetlist {
    read_structural(text, "mixed.v", &|name| {
        let cell = libs
            .iter()
            .flat_map(|l| &l.cells)
            .find(|c| c.name == name)?;
        Some(
            cell.pins
                .iter()
                .map(|pin| {
                    let direction = match pin.direction {
                        Direction::Input => PinDir::Input,
                        Direction::Output => PinDir::Output,
                        Direction::Inout => PinDir::Inout,
                        Direction::Internal => PinDir::Other,
                    };
                    (pin.name.clone(), direction)
                })
                .collect(),
        )
    })
    .expect("valid netlist")
}

#[test]
fn mixed_slew_and_global_ocv_match_reference_values() {
    let libs = [library("A", 20.0, 80.0, 1.0), library("B", 30.0, 70.0, 0.5)];
    let frozen = netlist(&libs).freeze();
    let sdc = format!("{SDC}set_timing_derate -early 0.95\nset_timing_derate -late 1.05\n");
    let analysis =
        EngineAnalysis::analyze_single_corner(&frozen, &libs, &sdc, &AnalysisOptions::default())
            .expect("mixed analysis");
    let corner = &analysis.corners[0];
    let node = |name: &str| {
        (0..analysis.netview.num_nodes())
            .map(|n| NodeId(n as u32))
            .find(|&n| analysis.netview.node_name_str(n) == name)
            .expect("named node")
    };
    // Independently observed OpenSTA 2.7.0 values, seconds converted to ns.
    for (name, slew) in [
        ("u1/Z", 0.324),
        ("u2/I", 0.432),
        ("u2/Z", 0.3864),
        ("y", 0.2898),
    ] {
        for tr in Tr::ALL {
            for an in An::ALL {
                let actual =
                    corner
                        .node_values
                        .transition(node(name).index(), tr.index(), an.index());
                assert!((actual - slew).abs() < 1e-12, "{name}: {actual} vs {slew}");
            }
        }
    }
    for (an, expected) in [(An::Early, 0.4522), (An::Late, 0.4998)] {
        for tr in Tr::ALL {
            let actual = (0..corner.node_values.ntags as usize)
                .map(|tag| {
                    corner
                        .node_values
                        .arrival(node("y").index(), tag, tr.index(), an.index())
                })
                .find(|v| v.is_finite())
                .expect("arrival");
            assert!(
                (actual - expected).abs() < 1e-12,
                "{an:?}: {actual} vs {expected}"
            );
        }
    }
}

#[test]
fn mixed_threshold_ocv_incremental_is_bitwise_full() {
    let libs = vec![
        library("A", 20.0, 80.0, 1.0),
        library("B", 30.0, 70.0, 0.5),
        library("C", 10.0, 90.0, 1.0),
    ];
    let sdc = format!("{SDC}set_timing_derate -early 0.95\nset_timing_derate -late 1.05\n");
    let mut session = TimingSession::new(netlist(&libs), libs, &sdc, AnalysisOptions::default())
        .expect("timing session");
    let inst = session.netlist().find_instance("u2").expect("u2");
    let replacement = session.netlist().find_cell("C").expect("C");
    session
        .apply_edits(&[SessionEdit::SwapCell {
            inst,
            new_cell: replacement,
        }])
        .expect("swap threshold convention");
    session
        .verify_against_full()
        .expect("mixed threshold incremental/full equality");
}

/// Each case is ten timing nodes and 128 bounded swaps; each PDK is parsed once.
#[test]
#[ignore = "requires the original Sky130 and GF180 PDK files"]
fn real_pdk_ocv_incremental_is_bitwise_full() {
    for (tag, path) in support::pdk::campaign_libraries() {
        let text = std::fs::read_to_string(&path).expect("original PDK Liberty");
        let lib = read_text(&text, &path.to_string_lossy()).expect("parse PDK");
        let cell = lib
            .cells
            .iter()
            .find(|c| c.name.ends_with("__buf_1"))
            .expect("buf_1");
        let cell_name = cell.name.clone();
        let larger = cell_name.replace("__buf_1", "__buf_2");
        let input = cell
            .pins
            .iter()
            .find(|p| p.direction == Direction::Input)
            .expect("input");
        let output = cell
            .pins
            .iter()
            .find(|p| p.direction == Direction::Output)
            .expect("output");
        let source = format!(
            "module top(input a, output y); wire n; {cell_name} u1(.{}(a), .{}(n)); {cell_name} u2(.{}(n), .{}(y)); endmodule",
            input.name, output.name, input.name, output.name
        );
        let netlist = netlist_text(&source, std::slice::from_ref(&lib));
        let sdc = format!("{SDC}set_timing_derate -early 0.95\nset_timing_derate -late 1.05\n");
        let mut session = TimingSession::new(netlist, vec![lib], &sdc, AnalysisOptions::default())
            .expect("PDK OCV session");
        let inst = session.netlist().find_instance("u1").expect("u1");
        let cells = [
            session.netlist().find_cell(&cell_name).expect("buf_1"),
            session.netlist().find_cell(&larger).expect("buf_2"),
        ];
        for step in 0..128 {
            session
                .apply_edits(&[SessionEdit::SwapCell {
                    inst,
                    new_cell: cells[step % 2],
                }])
                .expect("bounded resize");
            session
                .verify_against_full()
                .unwrap_or_else(|e| panic!("{tag} step {step}: {e}"));
        }
    }
}

/// Three original immutable libraries, ten timing nodes/case, 128 bounded resizes each.
/// The extracted 10 kOhm / 2+20 fF load exercises strong shielding under global OCV.
#[test]
#[ignore = "requires the original Sky130 and both GF180 PDK libraries; run with OS memory/time caps"]
fn real_pdk_strong_rc_ocv_incremental_is_bitwise_full() {
    use ruststa::engine::delay::spef::ParasiticInput;
    use std::sync::Arc;

    for (tag, path) in support::pdk::campaign_libraries() {
        assert!(std::fs::metadata(&path).unwrap().len() < 64 << 20);
        let text = std::fs::read_to_string(&path).expect("original PDK Liberty");
        let lib = read_text(&text, &path.to_string_lossy()).expect("parse original PDK once");
        let cell = lib
            .cells
            .iter()
            .find(|c| c.name.ends_with("__buf_1"))
            .expect("buf_1");
        let name = cell.name.clone();
        let input = cell
            .pins
            .iter()
            .find(|p| p.direction == Direction::Input)
            .unwrap()
            .name
            .clone();
        let output = cell
            .pins
            .iter()
            .find(|p| p.direction == Direction::Output)
            .unwrap()
            .name
            .clone();
        let design = netlist_text(
            &format!("module top(input a,output y); {name} d(.{input}(a),.{output}(y)); endmodule"),
            std::slice::from_ref(&lib),
        );
        let source = format!(
            r#"*SPEF "IEEE 1481-1998"
*DESIGN "top"
*DATE "2026"
*VENDOR "RustEDA original-library scalar control"
*PROGRAM "ruststa incremental test"
*VERSION "1"
*DESIGN_FLOW "PIN_CAP NONE"
*DIVIDER /
*DELIMITER :
*BUS_DELIMITER [ ]
*T_UNIT 1 NS
*C_UNIT 1 PF
*R_UNIT 1 OHM
*L_UNIT 1 HENRY
*PORTS
a I
y O
*D_NET a 0
*CONN
*P a I
*I d:{input} I
*RES
1 a d:{input} 0
*END
*D_NET y 0.022
*CONN
*I d:{output} O
*P y O
*CAP
1 d:{output} 0.002
2 y 0.02
*RES
1 d:{output} y 10000
*END
"#
        );
        let parasitic = Arc::new(ParasiticInput::new(
            "default",
            Arc::new(rusteda_formats::spef::read(&source, Default::default()).unwrap()),
        ));
        let sdc = format!("{SDC}set_timing_derate -early .95\nset_timing_derate -late 1.05\n");
        let mut session = TimingSession::new(
            design,
            vec![lib],
            &sdc,
            AnalysisOptions {
                parasitics: vec![parasitic],
                max_state_bytes: 1 << 28,
                ..Default::default()
            },
        )
        .expect("strong RC global OCV session");
        for step in 0..128 {
            let target = if step % 2 == 0 {
                name.replace("__buf_1", "__buf_2")
            } else {
                name.clone()
            };
            session
                .swap_cell_by_name("d", &target)
                .expect("bounded strong RC resize");
            session
                .verify_against_full()
                .unwrap_or_else(|e| panic!("{tag} strong RC step {step}: {e}"));
        }
    }
}
