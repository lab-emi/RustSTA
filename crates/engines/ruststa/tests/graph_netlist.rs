// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Unit and integration tests for [`NetlistView`] and [`TimingGraph`]: statement-order invariance,
//! multiple-driver rejection, tri-state buses, `inout` ports and pins, cross-corner pin direction
//! validation, aggregated unconnected-input warnings, latch data arcs, and the 500k-cell scale and
//! zero-per-node-allocation benchmark (owner: E1).

mod support;

use std::collections::HashMap;
use std::time::Instant;

use rusteda_core::liberty::Direction;
use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::{Library, read_text};
use ruststa::engine::graph::{ArcKind, ArcSense, CheckKind, DisableReason, GraphArc, TimingGraph};
use ruststa::engine::input::{NetTarget, NetlistView, NetlistViewError, NodeKind};
use ruststa::engine::types::NodeId;
use ruststa_fixtures::liberty::{Variant, library};
use support::legacy_import::read_structural_frozen;

fn wt_full() -> Library {
    read_text(&library(Variant::Full), Variant::Full.name()).expect("synthetic library")
}

#[derive(Debug, PartialEq, Eq)]
struct CanonicalGraphSnapshot {
    nodes: Vec<(String, Option<bool>, u32)>,
    arcs: Vec<(String, String, bool, Option<DisableReason>)>,
    launches: Vec<(String, String)>,
    checks: Vec<(String, String, CheckKind)>,
}

fn snapshot(nv: &NetlistView<'_>, g: &TimingGraph) -> CanonicalGraphSnapshot {
    let mut nodes: Vec<(String, Option<bool>, u32)> = (0..g.num_nodes())
        .map(|i| {
            let id = NodeId(i as u32);
            (
                nv.node_name_str(id).into_owned(),
                nv.is_const_node(id),
                g.level(id),
            )
        })
        .collect();
    nodes.sort();

    let mut arcs: Vec<(String, String, bool, Option<DisableReason>)> = g
        .arcs()
        .iter()
        .enumerate()
        .map(|(ai, a)| {
            (
                nv.node_name_str(a.src).into_owned(),
                nv.node_name_str(a.dst).into_owned(),
                a.kind.is_net(),
                g.arc_disable_reason(ai),
            )
        })
        .collect();
    arcs.sort();

    let mut launches: Vec<(String, String)> = g
        .launches()
        .iter()
        .map(|l| {
            (
                nv.node_name_str(l.clock_pin).into_owned(),
                nv.node_name_str(l.out_pin).into_owned(),
            )
        })
        .collect();
    launches.sort();

    let mut checks: Vec<(String, String, CheckKind)> = g
        .checks()
        .iter()
        .map(|c| {
            (
                nv.node_name_str(c.data_pin).into_owned(),
                nv.node_name_str(c.clock_pin).into_owned(),
                c.kind,
            )
        })
        .collect();
    checks.sort();

    CanonicalGraphSnapshot {
        nodes,
        arcs,
        launches,
        checks,
    }
}

// STA-REQ-0011, STA-REQ-0102: permuting wire declarations, assign statements, instance order,
// and pin connection order produces the exact same canonical graph, cycle breaks, and levels.
#[test]
fn statement_order_invariance() {
    let libs = [wt_full()];

    let v1 = "\
module inv_order(input clk, input a, input b, output y, output z);
  wire n1, n2, n3, n_alias, d_wire;
  wtf_nand2_1 u_loop1 (.A(a), .B(n2), .Y(n1));
  wtf_inv_1   u_loop2 (.A(n1), .Y(n2));
  wtf_and2_1  u_comb  (.A(n2), .B(b), .X(n3));
  wtf_dff_1 u_reg   (.CLK(clk), .D(n3), .Q(d_wire));
  assign n_alias = d_wire;
  assign y = n_alias;
  assign z = 1'b0;
endmodule
";

    // Permuted wire order, reversed instance order, reversed pin connections, and reordered assigns.
    let v2 = "\
module inv_order(input clk, input a, input b, output y, output z);
  wire d_wire, n_alias, n3, n2, n1;
  assign z = 1'b0;
  assign y = n_alias;
  assign n_alias = d_wire;
  wtf_dff_1 u_reg   (.Q(d_wire), .D(n3), .CLK(clk));
  wtf_and2_1  u_comb  (.X(n3), .B(b), .A(n2));
  wtf_inv_1   u_loop2 (.Y(n2), .A(n1));
  wtf_nand2_1 u_loop1 (.Y(n1), .B(n2), .A(a));
endmodule
";

    // Another permutation with interleaved wires, instances, and assigns.
    let v3 = "\
module inv_order(input clk, input a, input b, output y, output z);
  wire n2, n1;
  wtf_inv_1   u_loop2 (.A(n1), .Y(n2));
  wire n3, d_wire, n_alias;
  assign n_alias = d_wire;
  wtf_nand2_1 u_loop1 (.B(n2), .A(a), .Y(n1));
  wtf_dff_1 u_reg   (.D(n3), .CLK(clk), .Q(d_wire));
  wtf_and2_1  u_comb  (.B(b), .A(n2), .X(n3));
  assign y = n_alias;
  assign z = 1'b0;
endmodule
";

    let mut snapshots = Vec::new();
    for (idx, text) in [v1, v2, v3].into_iter().enumerate() {
        let frozen = read_structural_frozen(&format!("perm_{idx}.v"), text, &libs).unwrap();
        let nv = NetlistView::new(&frozen, &libs).unwrap();
        let g = TimingGraph::build(&nv, &libs).unwrap();
        snapshots.push(snapshot(&nv, &g));
    }

    assert_eq!(snapshots[0], snapshots[1]);
    assert_eq!(snapshots[1], snapshots[2]);

    // Also verify that renaming the external port `a` (outside the SCC `{u_loop1/B, u_loop1/Y,
    // u_loop2/A, u_loop2/Y}`) to `zz_a` does not change which arc inside the SCC is broken.
    let v_renamed = v1
        .replace("input a", "input zz_a")
        .replace(".A(a)", ".A(zz_a)");
    let frozen_ren = read_structural_frozen("renamed.v", &v_renamed, &libs).unwrap();
    let nv_ren = NetlistView::new(&frozen_ren, &libs).unwrap();
    let g_ren = TimingGraph::build(&nv_ren, &libs).unwrap();
    let snap_ren = snapshot(&nv_ren, &g_ren);

    let broken_orig: Vec<_> = snapshots[0]
        .arcs
        .iter()
        .filter(|a| a.3 == Some(DisableReason::LoopBreak))
        .collect();
    let broken_ren: Vec<_> = snap_ren
        .arcs
        .iter()
        .filter(|a| a.3 == Some(DisableReason::LoopBreak))
        .collect();
    assert_eq!(broken_orig.len(), 1);
    assert_eq!(broken_orig, broken_ren);
}

// STA-REQ-0002: multiple non-tristate drivers on a net and driving a constant net are rejected
// with stable T-codes.
#[test]
fn multiple_drivers_and_constant_drives_rejected() {
    let libs = [wt_full()];

    // 1. Two non-tristate instance outputs driving the same wire.
    let two_outputs = "\
module two_drv(input a, input b, output y);
  wtf_inv_1 u1 (.A(a), .Y(y));
  wtf_inv_1 u2 (.A(b), .Y(y));
endmodule
";
    let frozen = read_structural_frozen("two_drv.v", two_outputs, &libs).unwrap();
    let err = NetlistView::new(&frozen, &libs).unwrap_err();
    assert_eq!(err.code(), "T0001");
    assert!(
        matches!(err, NetlistViewError::MultipleDrivers { .. }),
        "{err}"
    );

    // 2. Input port and instance output driving the same wire through an assign alias.
    let port_and_inst = "\
module alias_drv(input a, input b, output y);
  wire w;
  wtf_inv_1 u1 (.A(b), .Y(w));
  assign w = a;
  assign y = w;
endmodule
";
    let frozen2 = read_structural_frozen("alias_drv.v", port_and_inst, &libs).unwrap();
    let err2 = NetlistView::new(&frozen2, &libs).unwrap_err();
    assert_eq!(err2.code(), "T0001");

    // 3. Instance output connected to constant 1'b0.
    let drives_const = "\
module const_drv(input a, output y);
  wtf_inv_1 u1 (.A(a), .Y(1'b0));
  assign y = a;
endmodule
";
    let frozen3 = read_structural_frozen("const_drv.v", drives_const, &libs).unwrap();
    let err3 = NetlistView::new(&frozen3, &libs).unwrap_err();
    assert_eq!(err3.code(), "T0002");
    assert!(
        matches!(err3, NetlistViewError::DrivesConstant { value: false, .. }),
        "{err3}"
    );

    // 4. Net tied to both 1'b0 and 1'b1 via assigns.
    let conflict_const = "\
module conflict_c(output y);
  wire w;
  assign w = 1'b0;
  assign y = w;
  assign y = 1'b1;
endmodule
";
    let frozen4 = read_structural_frozen("conflict_c.v", conflict_const, &libs).unwrap();
    let err4 = NetlistView::new(&frozen4, &libs).unwrap_err();
    assert_eq!(err4.code(), "T0003");
    assert!(matches!(
        err4,
        NetlistViewError::ConflictingConstants { .. }
    ));
}

// STA-REQ-0002, STA-REQ-0100: multiple three-state drivers on a shared bus are accepted, set
// `has_tristate(net) == true`, and emit net arcs from each driver to each load as well as
// `TristateEnable` / `TristateDisable` cell arcs.
#[test]
fn tristate_bus_multiple_drivers() {
    let lib_text = r#"
library (ts_lib) {
  delay_model : table_lookup;
  time_unit : "1ns";
  voltage_unit : "1V";
  capacitive_load_unit (1, pf);
  cell (ts_buf) {
    pin (A)  { direction : input; capacitance : 0.002; }
    pin (EN) { direction : input; capacitance : 0.002; }
    pin (Z)  {
      direction : output;
      function : "A";
      three_state : "!EN";
      timing () {
        related_pin : "A";
        timing_sense : positive_unate;
        timing_type : combinational;
      }
      timing () {
        related_pin : "EN";
        timing_sense : positive_unate;
        timing_type : three_state_enable;
      }
      timing () {
        related_pin : "EN";
        timing_sense : negative_unate;
        timing_type : three_state_disable;
      }
    }
  }
  cell (rcv_buf) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        timing_sense : positive_unate;
        timing_type : combinational;
      }
    }
  }
}
"#;
    let lib = read_text(lib_text, "ts_lib.lib").unwrap();
    let libs = [lib];

    let vlog = "\
module ts_bus(input a0, input en0, input a1, input en1, output bus_out, output rcv_out);
  wire bus;
  ts_buf drv0 (.A(a0), .EN(en0), .Z(bus));
  ts_buf drv1 (.A(a1), .EN(en1), .Z(bus));
  rcv_buf rcv (.A(bus), .Y(rcv_out));
  assign bus_out = bus;
endmodule
";
    let frozen = read_structural_frozen("ts_bus.v", vlog, &libs).unwrap();
    let nv = NetlistView::new(&frozen, &libs).unwrap();
    let g = TimingGraph::build(&nv, &libs).unwrap();

    // Find the bus net row via drv0/Z.
    let drv0_z = (0..nv.num_nodes())
        .map(|i| NodeId(i as u32))
        .find(|&n| nv.node_name_str(n) == "drv0/Z")
        .unwrap();
    assert!(nv.is_tristate_pin(drv0_z));
    let NetTarget::Net(bus_net) = nv.node_net(drv0_z) else {
        panic!("expected net");
    };
    assert!(nv.has_tristate(bus_net));
    assert_eq!(nv.drivers(bus_net).len(), 2);
    assert_eq!(nv.loads(bus_net).len(), 2); // rcv/A and bus_out

    // Both drv0/Z and drv1/Z must have net arcs to both rcv/A and bus_out.
    let net_arc_pairs: Vec<(String, String)> = g
        .arcs()
        .iter()
        .filter(|a| a.kind == ArcKind::Net { net: bus_net })
        .map(|a| {
            (
                nv.node_name_str(a.src).into_owned(),
                nv.node_name_str(a.dst).into_owned(),
            )
        })
        .collect();
    assert_eq!(net_arc_pairs.len(), 4);
    assert!(net_arc_pairs.contains(&("drv0/Z".into(), "rcv/A".into())));
    assert!(net_arc_pairs.contains(&("drv0/Z".into(), "bus_out".into())));
    assert!(net_arc_pairs.contains(&("drv1/Z".into(), "rcv/A".into())));
    assert!(net_arc_pairs.contains(&("drv1/Z".into(), "bus_out".into())));

    // Check TristateEnable and TristateDisable arcs.
    assert_eq!(
        g.arcs()
            .iter()
            .filter(|a| matches!(a.kind, ArcKind::TristateEnable { .. }))
            .count(),
        2
    );
    assert_eq!(
        g.arcs()
            .iter()
            .filter(|a| matches!(a.kind, ArcKind::TristateDisable { .. }))
            .count(),
        2
    );

    // Mixing a tri-state driver (`ts_buf`) with a non-tristate driver (`rcv_buf`) on the same net
    // must be rejected as MultipleDrivers!
    let bad_mix = "\
module ts_bad(input a0, input en0, input a1, output bus_out);
  wire bus;
  ts_buf drv0 (.A(a0), .EN(en0), .Z(bus));
  rcv_buf drv1 (.A(a1), .Y(bus));
  assign bus_out = bus;
endmodule
";
    let frozen_bad = read_structural_frozen("ts_bad.v", bad_mix, &libs).unwrap();
    let err = NetlistView::new(&frozen_bad, &libs).unwrap_err();
    assert_eq!(err.code(), "T0001");
}

// STA-REQ-0002, STA-REQ-0100: top-level `inout` ports and `inout` cell pins act as both driver
// and load on their net without self-loop arcs.
#[test]
fn inout_ports_and_pins() {
    let libs = [wt_full()];
    let vlog = "\
module bidir_top(inout pad, input tx_en, input tx_data, output rx_data);
  wtf_ebufn_1 u_tx (.A(tx_data), .TE_B(tx_en), .Z(pad));
  wtf_buf_1   u_rx (.A(pad), .X(rx_data));
  wtf_hold_1  u_hold (.Z(pad));
endmodule
";
    let frozen = read_structural_frozen("bidir_top.v", vlog, &libs).unwrap();
    let nv = NetlistView::new(&frozen, &libs).unwrap();
    let g = TimingGraph::build(&nv, &libs).unwrap();

    let pad_node = (0..nv.num_nodes())
        .map(|i| NodeId(i as u32))
        .find(|&n| nv.node_name_str(n) == "pad")
        .unwrap();
    assert!(nv.is_port(pad_node));
    assert!(nv.is_driver(pad_node));
    assert!(nv.is_load(pad_node));
    assert!(matches!(
        nv.node_kind(pad_node),
        NodeKind::PortBit {
            dir: Direction::Inout,
            ..
        }
    ));

    // Verify wire arcs on `pad`:
    // - `u_tx/Z -> pad` and `u_tx/Z -> u_rx/A`
    // - `pad -> u_rx/A`
    // - No self-loop `pad -> pad` or `u_hold/Z -> u_hold/Z`!
    for a in g.arcs() {
        assert_ne!(a.src, a.dst, "self-loop wire arc emitted");
    }
    let arc_names: Vec<(String, String)> = g
        .arcs()
        .iter()
        .filter(|a| a.kind.is_net())
        .map(|a| {
            (
                nv.node_name_str(a.src).into_owned(),
                nv.node_name_str(a.dst).into_owned(),
            )
        })
        .collect();
    assert!(arc_names.contains(&("u_tx/Z".into(), "pad".into())));
    assert!(arc_names.contains(&("pad".into(), "u_rx/A".into())));
}

// STA-REQ-0003: pin direction or three-state mismatch across corners is rejected with T-codes.
#[test]
fn cross_corner_pin_direction_and_tristate_validation() {
    let c0_text = r#"
library (c0) {
  delay_model : table_lookup;
  time_unit : "1ns";
  voltage_unit : "1V";
  capacitive_load_unit (1, pf);
  cell (my_cell) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Z) { direction : output; function : "A"; three_state : "!A"; }
  }
}
"#;
    // Corner 1 changes pin Z from output to input.
    let c1_bad_dir_text = r#"
library (c1_dir) {
  delay_model : table_lookup;
  time_unit : "1ns";
  voltage_unit : "1V";
  capacitive_load_unit (1, pf);
  cell (my_cell) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Z) { direction : input; capacitance : 0.002; }
  }
}
"#;
    // Corner 2 keeps pin Z as output but drops three_state.
    let c2_bad_ts_text = r#"
library (c2_ts) {
  delay_model : table_lookup;
  time_unit : "1ns";
  voltage_unit : "1V";
  capacitive_load_unit (1, pf);
  cell (my_cell) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (Z) { direction : output; function : "A"; }
  }
}
"#;
    let lib_c0 = [read_text(c0_text, "c0.lib").unwrap()];
    let lib_c1_dir = [read_text(c1_bad_dir_text, "c1_dir.lib").unwrap()];
    let lib_c2_ts = [read_text(c2_bad_ts_text, "c2_ts.lib").unwrap()];

    let vlog = "\
module m(input a, output z);
  my_cell u0 (.A(a), .Z(z));
endmodule
";
    let frozen = read_structural_frozen("m.v", vlog, &lib_c0).unwrap();

    // Direction mismatch in corner 1.
    let err_dir = NetlistView::with_corners(&frozen, &[&lib_c0, &lib_c1_dir]).unwrap_err();
    assert_eq!(err_dir.code(), "T0006");
    assert!(matches!(
        err_dir,
        NetlistViewError::PinDirectionMismatch {
            corner: 1,
            expected: Direction::Output,
            found: Direction::Input,
            ..
        }
    ));

    // Three-state mismatch in corner 1.
    let err_ts = NetlistView::with_corners(&frozen, &[&lib_c0, &lib_c2_ts]).unwrap_err();
    assert_eq!(err_ts.code(), "T0007");
    assert!(matches!(
        err_ts,
        NetlistViewError::TristateMismatch { corner: 1, .. }
    ));

    // Missing cell in corner 1.
    let empty_lib = [read_text(
        "library (empty) { delay_model : table_lookup; time_unit : \"1ns\"; voltage_unit : \"1V\"; capacitive_load_unit (1, pf); }",
        "empty.lib",
    )
    .unwrap()];
    let err_cell = NetlistView::with_corners(&frozen, &[&lib_c0, &empty_lib]).unwrap_err();
    assert_eq!(err_cell.code(), "T0004");
}

// STA-REQ-0002, STA-REQ-0213: unconnected instance input pins produce a single aggregated T0010
// warning; latch D -> Q combinational arcs are classified as LatchData; disable_arc + relevelize
// updates levels cleanly.
#[test]
fn aggregated_unconnected_inputs_warning_and_latch_data_arcs() {
    let lib_text = r#"
library (lat_lib) {
  delay_model : table_lookup;
  time_unit : "1ns";
  voltage_unit : "1V";
  capacitive_load_unit (1, pf);
  cell (and4_cell) {
    pin (A) { direction : input; capacitance : 0.002; }
    pin (B) { direction : input; capacitance : 0.002; }
    pin (C) { direction : input; capacitance : 0.002; }
    pin (D) { direction : input; capacitance : 0.002; }
    pin (Y) {
      direction : output;
      function : "A&B&C&D";
      timing () { related_pin : "A"; timing_type : combinational; }
      timing () { related_pin : "B"; timing_type : combinational; }
      timing () { related_pin : "C"; timing_type : combinational; }
      timing () { related_pin : "D"; timing_type : combinational; }
    }
  }
  cell (dlatch_cell) {
    latch (IQ, IQN) { data_in : "D"; enable : "G"; }
    pin (D) {
      direction : input;
      capacitance : 0.002;
      timing () {
        related_pin : "G";
        timing_type : setup_falling;
      }
      timing () {
        related_pin : "G";
        timing_type : hold_falling;
      }
    }
    pin (G) { direction : input; clock : true; capacitance : 0.002; }
    pin (Q) {
      direction : output;
      function : "IQ";
      timing () {
        related_pin : "D";
        timing_sense : positive_unate;
        timing_type : combinational;
      }
      timing () {
        related_pin : "G";
        timing_sense : positive_unate;
        timing_type : rising_edge;
      }
    }
  }
}
"#;
    let lib = read_text(lib_text, "lat_lib.lib").unwrap();
    let libs = [lib];

    // Build a netlist with 2 `and4_cell` instances where only pin A is connected (leaving 6
    // unconnected inputs B, C, D across u1 and u2) and 1 `dlatch_cell` instance.
    let mut g_nl = GateNetlist::new("unconn_test").unwrap();
    let and4 = g_nl
        .add_cell_type(
            "and4_cell",
            &[
                ("A", PinDir::Input),
                ("B", PinDir::Input),
                ("C", PinDir::Input),
                ("D", PinDir::Input),
                ("Y", PinDir::Output),
            ],
        )
        .unwrap();
    let dlatch = g_nl
        .add_cell_type(
            "dlatch_cell",
            &[
                ("D", PinDir::Input),
                ("G", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .unwrap();
    g_nl.add_port("in_a", PortDir::Input, None).unwrap();
    g_nl.add_port("clk_g", PortDir::Input, None).unwrap();
    g_nl.add_port("out_q", PortDir::Output, None).unwrap();
    let n_a = g_nl.find_net("in_a", None).unwrap();
    let n_g = g_nl.find_net("clk_g", None).unwrap();
    let n_q = g_nl.find_net("out_q", None).unwrap();
    let n_w1 = g_nl.add_net(Some("w1"), None).unwrap();
    let n_w2 = g_nl.add_net(Some("w2"), None).unwrap();

    let u1 = g_nl.add_instance("u1", and4).unwrap();
    g_nl.connect(u1, "A", Conn::Net(n_a)).unwrap();
    g_nl.connect(u1, "Y", Conn::Net(n_w1)).unwrap();

    let u2 = g_nl.add_instance("u2", and4).unwrap();
    g_nl.connect(u2, "A", Conn::Net(n_w1)).unwrap();
    g_nl.connect(u2, "Y", Conn::Net(n_w2)).unwrap();

    let u_lat = g_nl.add_instance("u_lat", dlatch).unwrap();
    g_nl.connect(u_lat, "D", Conn::Net(n_w2)).unwrap();
    g_nl.connect(u_lat, "G", Conn::Net(n_g)).unwrap();
    g_nl.connect(u_lat, "Q", Conn::Net(n_q)).unwrap();

    let frozen = g_nl.freeze();
    let nv = NetlistView::new(&frozen, &libs).unwrap();
    assert_eq!(nv.unconnected_input_count(), 6);
    assert_eq!(
        nv.warnings().len(),
        1,
        "must emit exactly one aggregated warning"
    );
    assert_eq!(nv.warnings()[0].code, "T0010");
    assert!(
        nv.warnings()[0]
            .message
            .contains("6 unconnected instance input pin(s)")
    );
    assert!(nv.warnings()[0].message.contains("(and 1 more)"));

    let mut tg = TimingGraph::build(&nv, &libs).unwrap();
    let latch_arc_idx = tg
        .arcs()
        .iter()
        .position(|a| matches!(a.kind, ArcKind::LatchData { .. }))
        .expect("dlatch_cell D -> Q must be classified as LatchData");
    assert_eq!(tg.arcs()[latch_arc_idx].sense, ArcSense::PositiveUnate);

    let out_q_node = (0..nv.num_nodes())
        .map(|i| NodeId(i as u32))
        .find(|&n| nv.node_name_str(n) == "out_q")
        .unwrap();
    let orig_level = tg.level(out_q_node);
    assert!(orig_level >= 5);

    // Disabling the latch D -> Q arc and relevelizing drops `u_lat/Q` to level 0 and `out_q` to
    // level 1.
    tg.disable_arc(latch_arc_idx, DisableReason::UserDisabled);
    tg.relevelize();
    assert_eq!(tg.level(out_q_node), 1);
}

// STA-REQ-0401: 500,000-cell graph construction completes in <= 0.35 s with zero per-node String
// or Vec allocations.
#[test]
#[ignore]
fn scale() {
    const NUM_CELLS: usize = 500_000;

    let libs = [wt_full()];

    // Build a 500,000-cell netlist of `wtf_buf_1` instances in a 500-lane x 1,000-stage pipeline
    // between scalar input `in_p` and output `out_p`.
    let mut g_nl = GateNetlist::new("scale_500k").unwrap();
    let buf = g_nl
        .add_cell_type("wtf_buf_1", &[("A", PinDir::Input), ("X", PinDir::Output)])
        .unwrap();
    g_nl.add_port("in_p", PortDir::Input, None).unwrap();
    g_nl.add_port("out_p", PortDir::Output, None).unwrap();
    let n_in = g_nl.find_net("in_p", None).unwrap();
    let n_out = g_nl.find_net("out_p", None).unwrap();

    let mut prev_net = n_in;
    for i in 0..NUM_CELLS {
        let inst = g_nl.add_instance(&format!("u{i}"), buf).unwrap();
        let out_net = if i + 1 == NUM_CELLS {
            n_out
        } else {
            g_nl.add_net(None, None).unwrap()
        };
        g_nl.connect(inst, "A", Conn::Net(prev_net)).unwrap();
        g_nl.connect(inst, "X", Conn::Net(out_net)).unwrap();
        prev_net = out_net;
    }

    let frozen = g_nl.freeze();
    assert_eq!(frozen.instances.len(), NUM_CELLS);
    assert_eq!(frozen.pin_net.len(), NUM_CELLS * 2);

    // Structural memory checks: node/arc descriptors are Copy and hold no heap pointers.
    assert!(size_of::<NodeKind>() <= 24);
    assert!(size_of::<GraphArc>() <= 28);
    assert!(!std::mem::needs_drop::<NodeKind>());
    assert!(!std::mem::needs_drop::<GraphArc>());

    // Measure wall-clock time of NetlistView + TimingGraph::build.
    let t0 = Instant::now();
    let nv = NetlistView::new(&frozen, &libs).expect("NetlistView 500k");
    let tg = TimingGraph::build(&nv, &libs).expect("TimingGraph 500k");
    let elapsed = t0.elapsed();

    eprintln!(
        "scale(500k cells, {} nodes, {} arcs): {:.4} s",
        tg.num_nodes(),
        tg.arcs().len(),
        elapsed.as_secs_f64(),
    );

    assert_eq!(tg.num_nodes(), NUM_CELLS * 2 + 2);
    assert_eq!(tg.arcs().len(), NUM_CELLS * 2 + 1);
    assert_eq!(tg.levels().max_level() as usize, NUM_CELLS * 2 + 1);
    assert!(
        elapsed.as_secs_f64() <= 0.35,
        "500k-cell TimingGraph build took {:.4} s (target <= 0.35 s)",
        elapsed.as_secs_f64(),
    );

    // Spot-check on-demand node naming at the first and last cell without storing Strings.
    let mut map = HashMap::new();
    map.insert(
        nv.node_name_str(NodeId(0)).into_owned(),
        tg.level(NodeId(0)),
    );
    assert_eq!(map.get("in_p"), Some(&0));
}
