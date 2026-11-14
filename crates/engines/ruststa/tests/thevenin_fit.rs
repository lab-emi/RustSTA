// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Arcs whose tables no finite-ramp Thevenin driver fits drive their extracted pi as a lumped
//! load (`STA-REQ-0735`): the clock-to-output arc of an OpenRAM SKY130 SRAM macro, whose delay
//! rises by 5.7 ns/pF while its transition stays between 2 and 16 ps, launches into a routed net
//! with the table values at the net's total capacitance instead of failing the analysis with
//! T0077.

use std::sync::Arc;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::PinDir;
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_formats::{liberty, spef};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::delay::spef::ParasiticInput;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::types::{An, NetRow, NodeId, Tr};

/// The library: a buffer and a macro with the clock-to-output tables of OpenRAM's
/// `sky130_sram_2kbyte_1rw1r_32x512_8` (TT 1.8 V 25 C), its 10/90 % slew trip points included.
const LIB: &str = r#"library (sram_like) {
  delay_model : table_lookup;
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  pulling_resistance_unit : "1kohm";
  leakage_power_unit : "1mW";
  voltage_unit : "1V";
  current_unit : "1mA";
  input_threshold_pct_rise : 50; input_threshold_pct_fall : 50;
  output_threshold_pct_rise : 50; output_threshold_pct_fall : 50;
  slew_lower_threshold_pct_rise : 10; slew_upper_threshold_pct_rise : 90;
  slew_lower_threshold_pct_fall : 10; slew_upper_threshold_pct_fall : 90;
  lu_table_template (CELL_TABLE) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1 ("0.00125, 0.005, 0.04");
    index_2 ("0.0017224999999999999, 0.006889999999999999, 0.027559999999999998");
  }
  cell (BUF) {
    pin (A) { direction : input; capacitance : 0.0017; }
    pin (Y) { direction : output; function : "A";
      timing () { related_pin : "A"; timing_sense : positive_unate;
        cell_rise (CELL_TABLE) { values ("0.05, 0.08, 0.2", "0.06, 0.09, 0.21", "0.08, 0.11, 0.23"); }
        cell_fall (CELL_TABLE) { values ("0.05, 0.08, 0.2", "0.06, 0.09, 0.21", "0.08, 0.11, 0.23"); }
        rise_transition (CELL_TABLE) { values ("0.03, 0.07, 0.25", "0.04, 0.08, 0.26", "0.06, 0.1, 0.28"); }
        fall_transition (CELL_TABLE) { values ("0.03, 0.07, 0.25", "0.04, 0.08, 0.26", "0.06, 0.1, 0.28"); }
      }
    }
  }
  cell (SRAM) {
    pin (clk0) { direction : input; clock : true; capacitance : 0.0024; }
    pin (dout0) { direction : output;
      timing () { related_pin : "clk0"; timing_type : rising_edge; timing_sense : non_unate;
        cell_rise (CELL_TABLE) { values ("0.383, 0.412, 0.529", "0.383, 0.412, 0.529", "0.383, 0.412, 0.529"); }
        cell_fall (CELL_TABLE) { values ("0.383, 0.412, 0.529", "0.383, 0.412, 0.529", "0.383, 0.412, 0.529"); }
        rise_transition (CELL_TABLE) { values ("0.002, 0.005, 0.016", "0.002, 0.005, 0.016", "0.002, 0.005, 0.016"); }
        fall_transition (CELL_TABLE) { values ("0.002, 0.005, 0.016", "0.002, 0.005, 0.016", "0.002, 0.005, 0.016"); }
      }
    }
  }
}
"#;

const VERILOG: &str = "module top (clk, y);
  input clk;
  output y;
  wire q;
  SRAM u_ram (.clk0(clk), .dout0(q));
  BUF u_buf (.A(q), .Y(y));
endmodule
";

/// The routed `dout0` net: 8.45 fF near the macro, 184 Ohm, 15.3 fF far (as in the reduced pi of
/// the layout, whose resistance is above the lumped ratio of STA-REQ-0727).
const SPEF: &str = "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n\
    *BUS_DELIMITER []\n*T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 OHM\n\
    *D_NET clk 0.002\n*CONN\n*P clk I\n*I u_ram:clk0 I\n*CAP\n1 clk 0.001\n2 u_ram:clk0 0.001\n\
    *RES\n1 clk u_ram:clk0 20\n*END\n\
    *D_NET q 0.02205\n*CONN\n*I u_ram:dout0 O\n*I u_buf:A I\n*CAP\n1 u_ram:dout0 0.00845\n\
    2 q:1 0.0136\n*RES\n1 u_ram:dout0 q:1 184\n2 q:1 u_buf:A 1\n*END\n\
    *D_NET y 0.001\n*CONN\n*I u_buf:Y O\n*P y O\n*CAP\n1 u_buf:Y 0.0005\n2 y 0.0005\n\
    *RES\n1 u_buf:Y y 10\n*END\n";

const SDC: &str = "create_clock -name clk -period 10 [get_ports clk]\n\
    set_clock_transition 0.19 [get_clocks clk]\n\
    set_output_delay -clock clk 1 [get_ports y]\n\
    set_load 0.002 [get_ports y]\n";

/// The macro's delay table at the load `c` (pF), linear between and beyond the index points.
fn sram_delay(c: f64) -> f64 {
    let (index, values) = (
        [
            0.0017224999999999999,
            0.006889999999999999,
            0.027559999999999998,
        ],
        [0.383, 0.412, 0.529],
    );
    let k = usize::from(c > index[1]);
    values[k] + (values[k + 1] - values[k]) * (c - index[k]) / (index[k + 1] - index[k])
}

// STA-REQ-0735: the macro launches into its routed net with the table delay at the net's total
// capacitance; the analysis completes with finite slacks.
#[test]
fn openram_launch_into_routed_net_is_lumped() {
    let lib: Library = liberty::read_text(LIB, "sram_like.lib").unwrap();
    let pins = |name: &str| -> Option<Vec<(String, PinDir)>> {
        let cell = lib.cells.iter().find(|c| c.name == name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => PinDir::Input,
                        _ => PinDir::Output,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    };
    let frozen = read_structural(VERILOG, "top.v", &pins).unwrap().freeze();
    let libs = [lib.clone()];
    let corners = [CornerLibs::new("tt", &libs)];
    let constraints = CompiledConstraints::from_sdc_str(SDC, "top.sdc", &frozen, &corners).unwrap();
    let extraction = Arc::new(spef::read(SPEF, spef::Limits::default()).unwrap());
    let options = AnalysisOptions {
        parasitics: vec![Arc::new(ParasiticInput::new("tt", extraction))],
        ..AnalysisOptions::default()
    };
    let analysis = EngineAnalysis::analyze(&frozen, &corners, constraints, &options, Ctx::NONE)
        .unwrap_or_else(|d| panic!("{d:?}"));
    let corner = &analysis.corners[0];
    let node = |name: &str| {
        (0..analysis.netview.num_nodes())
            .map(|i| NodeId(i as u32))
            .find(|&n| analysis.netview.node_name_str(n) == name)
            .unwrap()
    };
    let q = node("u_ram/dout0");
    let net = (0..frozen.nets.len())
        .map(|i| NetRow(i as u32))
        .find(|&n| analysis.netview.net_name_str(n) == "q")
        .unwrap();
    for tr in Tr::ALL {
        for an in An::ALL {
            let total = corner.net_cap(net, tr, an);
            assert!((total - (0.02205 + 0.0017)).abs() < 1e-12, "{total}");
            let (_, arrival) = corner.worst_arrival(q, tr, an).unwrap();
            assert!((arrival - sram_delay(total)).abs() < 1e-12, "{arrival}");
        }
    }
    let y = node("y");
    let slack = corner.setup_slack(y).unwrap();
    assert!(slack.is_finite() && slack > 8.0, "{slack}");
}
