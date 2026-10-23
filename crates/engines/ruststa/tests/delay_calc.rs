// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Verification tests for the library view and delay calculation (owner: E3; `STA-REQ-0010` to
//! `STA-REQ-0016`, `STA-REQ-0108` to `STA-REQ-0121`, `STA-REQ-0223`).

mod support;

use std::fmt::Write as _;
use std::path::PathBuf;

use rusteda_core::liberty::{Library, TimingArc};
use rusteda_core::netlist::{
    Conn as GateConn, FrozenNetlist, GateNetlist, PinDir as GatePinDir, PortDir as GatePortDir,
};
use rusteda_formats::liberty::read_text;
use ruststa::engine::delay::gate::{GateDelay, NetlistTiming, NldmGateDelay, StartBuilder};
use ruststa::engine::delay::load::{NetLoadOverrides, NetLoadSpec, NetLoads};
use ruststa::engine::delay::lut::{NO_TABLE, RangeTracker};
use ruststa::engine::delay::wire::{DriverLoad, Ideal};
use ruststa::engine::input::libview::{
    CornerThresholds, PresetClearKind, PropArcKind, Sense, TimingLib,
};
use ruststa::engine::types::{An, NetRow, Tr};
use ruststa_fixtures::lib_gen::AffineLut2x2;
use ruststa_fixtures::liberty::{Variant, library};
use support::legacy_import::{Conn as StepConn, LegacyImport, NetRef, PinDir, PortDir, import};

const TOL: f64 = 1e-12;

fn assert_close(actual: f64, expected: f64, label: &str) {
    let diff = (actual - expected).abs();
    assert!(
        diff <= TOL,
        "{label}: actual {actual:.15} != expected {expected:.15} (diff {diff:.3e} > {TOL:.1e})"
    );
}

fn check_exp(actual: f64, exp: &serde_json::Value, key: &str) {
    assert_close(actual, exp[key].as_f64().unwrap(), key);
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn analytic_expected(case: &str) -> serde_json::Value {
    let p = repo_root()
        .join("verification/analytic/sta")
        .join(case)
        .join("expected.json");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&text).expect("valid expected.json")
}

/// Linear arc model from `verification/analytic/sta/README.md`:
/// `delay(s, c) = k * (d0 + ks * s + kc * c)`, `slew(s, c) = k * (s0 + ms * s + mc * c)`.
#[derive(Clone, Copy)]
struct LinArc {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    ms: f64,
    mc: f64,
}

const fn lin(d0: f64, ks: f64, kc: f64, s0: f64, ms: f64, mc: f64) -> LinArc {
    LinArc {
        d0,
        ks,
        kc,
        s0,
        ms,
        mc,
    }
}

impl LinArc {
    fn delay(self, s: f64, c: f64, scale: f64) -> f64 {
        scale * (self.d0 + self.ks * s + self.kc * c)
    }

    fn slew(self, s: f64, c: f64, scale: f64) -> f64 {
        scale * (self.s0 + self.ms * s + self.mc * c)
    }
}

const INV_R: LinArc = lin(0.10, 0.20, 2.0, 0.05, 0.0, 1.5);
const INV_F: LinArc = lin(0.08, 0.15, 1.5, 0.04, 0.0, 1.2);
const BUF_R: LinArc = lin(0.20, 0.10, 1.0, 0.06, 0.0, 1.0);
const BUF_F: LinArc = lin(0.18, 0.10, 1.2, 0.05, 0.0, 1.1);
const XOR_R: LinArc = lin(0.30, 0.25, 2.5, 0.07, 0.0, 1.8);
const XOR_F: LinArc = lin(0.28, 0.20, 2.2, 0.06, 0.0, 1.6);
const CKQ_R: LinArc = lin(0.40, 0.00, 1.0, 0.08, 0.0, 1.0);
const CKQ_F: LinArc = lin(0.35, 0.00, 0.9, 0.07, 0.0, 1.0);
const TSE_R: LinArc = lin(0.15, 0.10, 1.0, 0.06, 0.0, 1.0);
const TSE_F: LinArc = lin(0.13, 0.10, 1.0, 0.05, 0.0, 1.0);
const TSD_R: LinArc = lin(0.09, 0.05, 0.0, 0.00, 0.0, 0.0);
const TSD_F: LinArc = lin(0.07, 0.05, 0.0, 0.00, 0.0, 0.0);
const XTR_R: LinArc = lin(0.05, -0.04, 1.0, 0.10, -0.08, 0.5);
const XTR_F: LinArc = lin(0.05, -0.04, 1.0, 0.10, -0.08, 0.5);

/// Builds the linear synthetic Liberty text specified in `verification/analytic/sta/README.md`,
/// scaled by `scale` (`1.0` for `fast`/base, `2.0` for `slow`), with optional `extra_header`.
fn analytic_linear_library(lib_name: &str, scale: f64, extra_header: &str) -> String {
    let mut o = format!(
        "library ({lib_name}) {{\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n{extra_header}  lu_table_template (t2) {{\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }}\n"
    );
    let push_table = |out: &mut String, kind: &str, f: &dyn Fn(f64, f64) -> f64| {
        let _ = writeln!(
            out,
            "        {kind} (t2) {{ values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\"); }}",
            f(0.0, 0.0),
            f(0.0, 1.0),
            f(1.0, 0.0),
            f(1.0, 1.0)
        );
    };
    let push_arc =
        |out: &mut String, related: &str, sense: &str, ty: &str, r: LinArc, f: LinArc| {
            let _ = writeln!(
                out,
                "      timing () {{\n        related_pin : \"{related}\";\n        \
             timing_sense : {sense};\n        timing_type : {ty};"
            );
            push_table(out, "cell_rise", &|s, c| r.delay(s, c, scale));
            push_table(out, "rise_transition", &|s, c| r.slew(s, c, scale));
            push_table(out, "cell_fall", &|s, c| f.delay(s, c, scale));
            push_table(out, "fall_transition", &|s, c| f.slew(s, c, scale));
            let _ = writeln!(out, "      }}");
        };

    for (name, cout, func, sense, r, f) in [
        ("INV", 0.0, "!A", "negative_unate", INV_R, INV_F),
        ("BUF", 0.0, "A", "positive_unate", BUF_R, BUF_F),
        ("BUFO", 0.004, "A", "positive_unate", BUF_R, BUF_F),
        ("XTR", 0.0, "A", "positive_unate", XTR_R, XTR_F),
    ] {
        let _ = writeln!(
            o,
            "  cell ({name}) {{\n    area : 1;\n    \
             pin (A) {{ direction : input; capacitance : 0.01; }}\n    \
             pin (Y) {{\n      direction : output;\n      capacitance : {cout};\n      \
             function : \"{func}\";"
        );
        push_arc(&mut o, "A", sense, "combinational", r, f);
        let _ = writeln!(o, "    }}\n  }}");
    }

    let _ = writeln!(
        o,
        "  cell (MUX2) {{\n    area : 2;\n    \
         pin (A0) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (A1) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (S) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (Y) {{\n      direction : output;\n      function : \"(A0 & !S) | (A1 & S)\";"
    );
    push_arc(
        &mut o,
        "A0",
        "positive_unate",
        "combinational",
        BUF_R,
        BUF_F,
    );
    push_arc(
        &mut o,
        "A1",
        "positive_unate",
        "combinational",
        BUF_R,
        BUF_F,
    );
    push_arc(&mut o, "S", "non_unate", "combinational", XOR_R, XOR_F);
    let _ = writeln!(o, "    }}\n  }}");

    let _ = writeln!(
        o,
        "  cell (BUFZ) {{\n    area : 2;\n    \
         pin (A) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (EN) {{ direction : input; capacitance : 0.01; }}\n    \
         pin (Z) {{\n      direction : output;\n      capacitance : 0.005;\n      \
         function : \"A\";\n      three_state : \"!EN\";"
    );
    push_arc(&mut o, "A", "positive_unate", "combinational", BUF_R, BUF_F);
    push_arc(
        &mut o,
        "EN",
        "positive_unate",
        "three_state_enable",
        TSE_R,
        TSE_F,
    );
    push_arc(
        &mut o,
        "EN",
        "negative_unate",
        "three_state_disable",
        TSD_R,
        TSD_F,
    );
    let _ = writeln!(o, "    }}\n  }}");

    let su = 0.10 * scale;
    let hd = 0.05 * scale;
    let _ = writeln!(
        o,
        "  cell (DFF) {{\n    area : 4;\n    \
         ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; }}\n    \
         pin (D) {{\n      direction : input;\n      capacitance : 0.01;\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : setup_rising;\n        \
         rise_constraint (scalar) {{ values (\"{su:.9}\"); }}\n        \
         fall_constraint (scalar) {{ values (\"{su:.9}\"); }}\n      }}\n      \
         timing () {{\n        related_pin : \"CK\";\n        timing_type : hold_rising;\n        \
         rise_constraint (scalar) {{ values (\"{hd:.9}\"); }}\n        \
         fall_constraint (scalar) {{ values (\"{hd:.9}\"); }}\n      }}\n    }}\n    \
         pin (CK) {{ direction : input; capacitance : 0.01; clock : true; }}\n    \
         pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    push_arc(&mut o, "CK", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}\n}}");
    o
}

/// Replays [`LegacyImport`] steps into [`GateNetlist`] and freezes it into a [`FrozenNetlist`].
fn freeze_verilog(
    name: &str,
    text: &str,
    top: Option<&str>,
    libs: &[Library],
) -> FrozenNetlist<'static> {
    let steps: LegacyImport = import(name, text, top, libs).expect("valid verilog import");
    let mut g = GateNetlist::new(&steps.module).expect("valid module name");
    let mut cell_ids = Vec::with_capacity(steps.cells.len());
    for c in &steps.cells {
        let pins: Vec<(&str, GatePinDir)> = c
            .pins
            .iter()
            .map(|(n, d)| {
                let dir = match d {
                    PinDir::Input => GatePinDir::Input,
                    PinDir::Output => GatePinDir::Output,
                    PinDir::Inout => GatePinDir::Inout,
                    PinDir::Other => GatePinDir::Other,
                };
                (n.as_str(), dir)
            })
            .collect();
        cell_ids.push(g.add_cell_type(&c.name, &pins).expect("valid cell type"));
    }
    for p in &steps.ports {
        let dir = match p.dir {
            PortDir::Input => GatePortDir::Input,
            PortDir::Output => GatePortDir::Output,
        };
        g.add_port(&p.name, dir, p.range).expect("valid port");
    }
    let mut extra_nets = Vec::with_capacity(steps.nets.len());
    for n in &steps.nets {
        extra_nets.push(g.add_net(Some(n), None).expect("valid net"));
    }
    let resolve_net = |g: &GateNetlist, r: NetRef| match r {
        NetRef::PortBit { port, bit } => g.ports()[port].bits[bit],
        NetRef::Net(k) => extra_nets[k],
    };
    let resolve_conn = |g: &GateNetlist, c: StepConn| match c {
        StepConn::Net(r) => GateConn::Net(resolve_net(g, r)),
        StepConn::Const(b) => GateConn::Const(b),
    };
    for inst in &steps.instances {
        let id = g
            .add_instance(&inst.name, cell_ids[inst.cell])
            .expect("valid instance");
        for (pin, conn) in &inst.conns {
            let gc = resolve_conn(&g, *conn);
            g.connect(id, pin.as_str(), gc).expect("valid connection");
        }
    }
    for a in &steps.assigns {
        let lhs = resolve_net(&g, a.lhs);
        let rhs = resolve_conn(&g, a.rhs);
        g.add_assign(lhs, rhs).expect("valid assign");
    }
    let frozen = g.freeze();
    frozen.validate().expect("valid frozen netlist");
    frozen
}

fn find_net(netlist: &FrozenNetlist<'_>, name: &str) -> NetRow {
    NetRow(
        (0..netlist.nets.len() as u32)
            .find(|&n| netlist.string(netlist.nets[n as usize].name).unwrap() == name)
            .unwrap(),
    )
}

fn seed_dff_launch(nt: &NetlistTiming, lib: &TimingLib, sb: &mut StartBuilder, pin: &str, c: f64) {
    let dff_arc = lib.cell("DFF").unwrap().arcs[0];
    let dl = DriverLoad::Lumped { c };
    let q_r = lib.arc_delay(dff_arc, Tr::Rise, An::Late, 0.0, dl);
    let q_f = lib.arc_delay(dff_arc, Tr::Fall, An::Early, 0.0, dl);
    let node = nt.node(pin).unwrap();
    sb.set_tagged(
        node,
        0,
        Tr::Rise,
        [q_r.delay, q_r.delay],
        [q_r.slew, q_r.slew],
    );
    sb.set_tagged(
        node,
        0,
        Tr::Fall,
        [q_f.delay, q_f.delay],
        [q_f.slew, q_f.slew],
    );
}

// STA-REQ-0010, STA-REQ-0011, STA-REQ-0012, STA-REQ-0017, STA-REQ-0014, STA-REQ-0108,
// STA-REQ-0112, STA-REQ-0114: closed-form `wt_15` (1ns / 1pF) and `wt_units` (100ps / 10fF)
// produce identical physical capacitances, delays, slews, and check constraints within 1e-12 ns.
#[test]
fn closed_form_wt_15_and_wt_units() {
    let text_15 = library(Variant::Wt15);
    let text_units = library(Variant::Units);
    let lib_15 = TimingLib::from_liberty_texts("nom", &[("wt_15.lib", &text_15)]).unwrap();
    let lib_u = TimingLib::from_liberty_texts("nom", &[("wt_units.lib", &text_units)]).unwrap();

    assert_close(lib_15.time_unit_ns, 1.0, "wt_15 time_unit_ns");
    assert_close(lib_15.cap_unit_pf, 1.0, "wt_15 cap_unit_pf");
    assert_close(lib_u.time_unit_ns, 0.1, "wt_units time_unit_ns");
    assert_close(lib_u.cap_unit_pf, 0.01, "wt_units cap_unit_pf");

    let d_lut = AffineLut2x2::DEFAULT_DELAY;
    let s_lut = AffineLut2x2::DEFAULT_SLEW;
    let fast_d = AffineLut2x2 {
        d0: 0.03,
        k_load: 1.0,
        ..d_lut
    };
    let fast_s = AffineLut2x2 {
        d0: 0.025,
        k_load: 0.75,
        ..s_lut
    };

    for (tag, lib) in [("wt_15", &lib_15), ("wt_units", &lib_u)] {
        assert_eq!(lib.cells.len(), 17, "{tag} cell count");
        let gate = NldmGateDelay::new(lib);

        let inv = lib.cell("wt_inv_1").unwrap();
        assert_close(inv.pins[inv.pin("A").unwrap()].cap[0][0], 0.01, tag);
        let buf2 = lib.cell("wt_buf_2").unwrap();
        assert_close(buf2.pins[buf2.pin("A").unwrap()].cap[0][0], 0.015, tag);

        let inv_arc = inv.arcs[0];
        assert_eq!(lib.arc(inv_arc).sense, Sense::Negative);
        assert_eq!(lib.arc(inv_arc).out_transitions(Tr::Rise), [false, true]);
        assert_eq!(lib.arc(inv_arc).out_transitions(Tr::Fall), [true, false]);

        let xor = lib.cell("wt_xor2_1").unwrap();
        assert_eq!(lib.arc(xor.arcs[0]).sense, Sense::Non);
        assert_eq!(lib.arc(xor.arcs[0]).out_transitions(Tr::Rise), [true, true]);

        for (slew, load) in [(0.1, 0.01), (0.25, 0.025), (0.37, 0.042), (0.5, 0.05)] {
            let dl = DriverLoad::Lumped { c: load };
            for tr in Tr::ALL {
                for an in An::ALL {
                    let ds = gate.arc_delay(inv_arc, tr, an, slew, dl);
                    assert_close(ds.delay, d_lut.eval(slew, load), "inv delay");
                    assert_close(ds.slew, s_lut.eval(slew, load), "inv slew");

                    let ds_fast = gate.arc_delay(buf2.arcs[0], tr, an, slew, dl);
                    assert_close(ds_fast.delay, fast_d.eval(slew, load), "buf2 delay");
                    assert_close(ds_fast.slew, fast_s.eval(slew, load), "buf2 slew");
                }
            }
        }

        let dffr = lib.cell("wt_dffr_n").unwrap();
        assert!(dffr.is_sequential);
        assert_eq!(dffr.checks.len(), 4);
        assert_eq!(dffr.preset_clear_arcs.len(), 1);
        assert_eq!(dffr.preset_clear_arcs[0].kind, PresetClearKind::Clear);
        assert_eq!(dffr.arcs.len(), 1);
        assert_eq!(lib.arc(dffr.arcs[0]).kind, PropArcKind::Edge(Tr::Fall));

        for chk in &dffr.checks {
            let val = lib.eval_check(chk, Tr::Rise, 0.2, 0.3, 0.0);
            let want = if chk.kind.is_setup_like() { 0.04 } else { 0.02 };
            assert_close(val, want, "dffr check");
        }

        let latch = lib.cell("wt_dlatch_p").unwrap();
        let d_to_q = latch
            .arcs
            .iter()
            .copied()
            .find(|&a| matches!(lib.arc(a).kind, PropArcKind::Combinational { .. }))
            .unwrap();
        assert_eq!(lib.arc(d_to_q).latch_check, Some(0));
    }
}

// STA-REQ-0110, STA-REQ-0121: `wt_outcap` driver output-pin capacitance (`0.005 pF`) is summed into
// the driven net's load in `NET_PINS` order.
#[test]
fn closed_form_wt_outcap() {
    let text = library(Variant::Outcap);
    let raw_lib = read_text(&text, "wt_outcap.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("wt_outcap.lib", &text)]).unwrap();

    let verilog = "
module top (input a, output y);
  wire n1;
  wt_buf_1 u1 (.A(a), .Y(n1));
  wt_buf_1 u2 (.A(n1), .Y(y));
endmodule
";
    let netlist = freeze_verilog("top.v", verilog, Some("top"), &[raw_lib]);
    let mut overrides = NetLoadOverrides::new();
    let y_net = NetRow(netlist.port_bit_net[1]);
    overrides.add_net_pin_load(y_net, 0.02, 0.02);

    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &overrides);
    let n1_row = find_net(&netlist, "n1");

    for tr in Tr::ALL {
        for an in An::ALL {
            assert_close(loads.total_cap(n1_row, tr, an), 0.015, "n1 total_cap");
            assert_close(loads.total_cap(y_net, tr, an), 0.025, "y total_cap");
        }
    }

    let d_lut = AffineLut2x2::DEFAULT_DELAY;
    let s_lut = AffineLut2x2::DEFAULT_SLEW;
    let u1_arc = lib.cell("wt_buf_1").unwrap().arcs[0];
    let dl = loads.driver_load(n1_row, Tr::Rise, An::Late);
    let ds = lib.arc_delay(u1_arc, Tr::Rise, An::Late, 0.2, dl);
    assert_close(ds.delay, d_lut.eval(0.2, 0.015), "u1 delay with outcap");
    assert_close(ds.slew, s_lut.eval(0.2, 0.015), "u1 slew with outcap");
}

// STA-REQ-0014, STA-REQ-0114: `wt_nonmono` non-increasing axis indices and inconsistent table
// dimensions are rejected with error `T0014`.
#[test]
fn closed_form_wt_nonmono_and_table_errors() {
    let text = library(Variant::NonMono);
    let raw_lib = read_text(&text, "wt_nonmono.lib").unwrap();
    let err = TimingLib::from_corner("nom", &[raw_lib]).unwrap_err();
    assert_eq!(err.code, "T0014");
    assert!(
        err.message.contains("non-increasing index_1"),
        "{}",
        err.message
    );

    let mut bad_lib = read_text(&library(Variant::Wt15), "wt_15.lib").unwrap();
    bad_lib.cells[0].pins[1].arcs[0]
        .cell_rise
        .as_mut()
        .unwrap()
        .values
        .pop();
    let err2 = TimingLib::from_corner("nom", &[bad_lib]).unwrap_err();
    assert_eq!(err2.code, "T0014");
    assert!(
        err2.message.contains("3 values for 2 x 2 x 1"),
        "{}",
        err2.message
    );
}

// STA-REQ-0108, STA-REQ-0109: `wt_extrap` linear extrapolation below and above table axes;
// negative slews are clamped to 0 while negative delays and check values remain negative.
#[test]
fn closed_form_wt_extrap_and_clamp_rule() {
    let text = library(Variant::Extrap);
    let lib = TimingLib::from_liberty_texts("nom", &[("wt_extrap.lib", &text)]).unwrap();
    let buf_arc = lib.cell("wt_buf_1").unwrap().arcs[0];

    let d_extrap = |s: f64, l: f64| 0.05 + 1.0 * (s - 0.10) + 8.0 * (l - 0.01);
    let s_extrap = |s: f64, l: f64| (0.04 + 2.0 * (s - 0.10) + 6.0 * (l - 0.01)).max(0.0);

    let ds_hi = lib.arc_delay(
        buf_arc,
        Tr::Rise,
        An::Late,
        0.40,
        DriverLoad::Lumped { c: 0.04 },
    );
    assert_close(ds_hi.delay, d_extrap(0.40, 0.04), "extrap high delay");
    assert_close(ds_hi.slew, s_extrap(0.40, 0.04), "extrap high slew");

    let ds_lo = lib.arc_delay(
        buf_arc,
        Tr::Rise,
        An::Early,
        0.0,
        DriverLoad::Lumped { c: 0.0 },
    );
    assert_close(ds_lo.delay, -0.13, "extrap negative delay unclamped");
    assert_close(ds_lo.slew, 0.0, "extrap negative slew clamped to 0");
}

// STA-REQ-0014, STA-REQ-0108, STA-REQ-0114: `wt_3d` 3-axis lookup table (`lut_3d`) trilinear
// interpolation and extrapolation.
#[test]
fn closed_form_wt_3d_trilinear_lookup() {
    let text = library(Variant::Lut3d);
    let lib = TimingLib::from_liberty_texts("nom", &[("wt_3d.lib", &text)]).unwrap();
    let arc = lib.cell("wt_buf_3d").unwrap().arcs[0];
    let exact =
        |s: f64, l: f64, c3: f64| 0.05 + 0.05 * (s - 0.1) + 2.5 * (l - 0.01) + 2.0 * (c3 - 0.01);

    for (s, l, c3) in [
        (0.1, 0.01, 0.01),
        (0.5, 0.05, 0.05),
        (0.25, 0.025, 0.035),
        (0.05, 0.005, 0.002),
        (0.75, 0.08, 0.09),
    ] {
        for tr in Tr::ALL {
            let ds = lib.arc_delay_3d(arc, tr, s, DriverLoad::Lumped { c: l }, c3);
            assert_close(ds.delay, exact(s, l, c3), "wt_3d delay");
            assert_close(ds.slew, 0.0, "wt_3d absent slew table");
        }
    }
}

// STA-REQ-0012, STA-REQ-0017, STA-REQ-0016, STA-REQ-0113, STA-REQ-0223: `wt_gfconv`,
// `wt_tristate`, and `wt_thresholds`.
#[test]
fn closed_form_wt_gfconv_wt_tristate_and_wt_thresholds() {
    let text_gf = library(Variant::GfConv);
    let lib_gf = TimingLib::from_liberty_texts("nom", &[("wt_gfconv.lib", &text_gf)]).unwrap();
    assert!(lib_gf.cell("wt_fill_1").is_some());
    assert!(lib_gf.cell("wt_aon_buf").is_some());

    let text_ts = library(Variant::Tristate);
    let lib_ts = TimingLib::from_liberty_texts("nom", &[("wt_tristate.lib", &text_ts)]).unwrap();
    let tbuf1 = lib_ts.cell("wt_tbuf_1").unwrap();
    assert_eq!(tbuf1.arcs.len(), 3);
    let en_arc = lib_ts.arc(tbuf1.arcs[1]);
    assert_eq!(
        en_arc.kind,
        PropArcKind::ThreeStateEnable { outs: [true, true] }
    );
    assert_eq!(en_arc.sense, Sense::Positive);
    assert_eq!(en_arc.out_transitions(Tr::Rise), [true, true]);
    assert_eq!(en_arc.out_transitions(Tr::Fall), [false, false]);

    let dis_arc = lib_ts.arc(tbuf1.arcs[2]);
    assert_eq!(
        dis_arc.kind,
        PropArcKind::ThreeStateDisable { outs: [true, true] }
    );
    assert_eq!(dis_arc.sense, Sense::Negative);
    assert_eq!(dis_arc.out_transitions(Tr::Rise), [false, false]);
    assert_eq!(dis_arc.out_transitions(Tr::Fall), [true, true]);

    let tbufn = lib_ts.cell("wt_tbuf_n").unwrap();
    let enn_arc = lib_ts.arc(tbufn.arcs[1]);
    assert_eq!(
        enn_arc.kind,
        PropArcKind::ThreeStateEnable { outs: [true, true] }
    );
    assert_eq!(enn_arc.sense, Sense::Negative);
    assert_eq!(enn_arc.out_transitions(Tr::Rise), [false, false]);
    assert_eq!(enn_arc.out_transitions(Tr::Fall), [true, true]);

    let text_thr = library(Variant::Thresholds);
    let lib_thr =
        TimingLib::from_liberty_texts("nom", &[("wt_thresholds.lib", &text_thr)]).unwrap();
    assert_close(
        lib_thr.thresholds.slew_derate_from_library,
        0.5,
        "slew_derate",
    );
    assert_close(
        lib_thr.thresholds.slew_lower_threshold_pct_rise,
        20.0,
        "lower_rise",
    );
    assert_close(
        lib_thr.thresholds.slew_upper_threshold_pct_rise,
        80.0,
        "upper_rise",
    );
    let inv_thr = lib_thr.cell("wt_inv_1").unwrap().arcs[0];
    let ds_thr = lib_thr.arc_delay(
        inv_thr,
        Tr::Rise,
        An::Late,
        0.2,
        DriverLoad::Lumped { c: 0.02 },
    );
    assert_close(
        ds_thr.delay,
        AffineLut2x2::DEFAULT_DELAY.eval(0.2, 0.02),
        "wt_thresholds delay",
    );
    assert_close(
        ds_thr.slew,
        AffineLut2x2::DEFAULT_SLEW.eval(0.2, 0.02),
        "wt_thresholds slew",
    );
}

// STA-REQ-0108, STA-REQ-0109, STA-REQ-0114: analytic case `slew_clamp`
// (`verification/analytic/sta/slew_clamp/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_slew_clamp() {
    let exp = &analytic_expected("slew_clamp")["base"];
    let lib_text = analytic_linear_library("lin", 1.0, "");
    let raw_lib = read_text(&lib_text, "lin.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("lin.lib", &lib_text)]).unwrap();

    let v_text = "
module m (input clk, input a, output q);
 wire n1, n2;
 XTR x (.A(a), .Y(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r1 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_lib]);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &NetLoadOverrides::new());
    let nt = NetlistTiming::build(&netlist, &lib);

    let xtr_arc = lib.cell("XTR").unwrap().arcs[0];
    let xtr_ds = lib.arc_delay(
        xtr_arc,
        Tr::Rise,
        An::Late,
        2.0,
        DriverLoad::Lumped { c: 0.01 },
    );
    check_exp(xtr_ds.delay, exp, "XTR delay");
    check_exp(xtr_ds.slew, exp, "XTR transition");

    let mut sb = StartBuilder::new(1);
    let a_node = nt.node("a").unwrap();
    sb.set_tagged(a_node, 0, Tr::Rise, [0.3, 0.3], [2.0, 2.0]);
    sb.set_tagged(a_node, 0, Tr::Fall, [0.3, 0.3], [2.0, 2.0]);
    let vals = nt.propagate(1, &loads, sb.build());

    let r1_d = nt.node("r1/D").unwrap() as usize;
    let n2_rise = vals.arrival(r1_d, 0, Tr::Rise.index(), An::Late.index());
    let n2_fall = vals.arrival(r1_d, 0, Tr::Fall.index(), An::Early.index());
    check_exp(n2_rise, exp, "n2 rise");
    check_exp(n2_fall, exp, "n2 fall");

    let dff = lib.cell("DFF").unwrap();
    let su = lib.eval_check(&dff.checks[0], Tr::Rise, 0.0, 0.0, 0.0);
    let hd = lib.eval_check(&dff.checks[1], Tr::Fall, 0.0, 0.0, 0.0);
    check_exp(2.0 - su - n2_rise, exp, "r1/D setup");
    check_exp(n2_fall - hd, exp, "r1/D hold");
}

// STA-REQ-0110, STA-REQ-0121: analytic case `driver_pin_cap`
// (`verification/analytic/sta/driver_pin_cap/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_driver_pin_cap() {
    let exp = &analytic_expected("driver_pin_cap")["base"];
    let lib_text = analytic_linear_library("lin", 1.0, "");
    let raw_lib = read_text(&lib_text, "lin.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("lin.lib", &lib_text)]).unwrap();

    let v_text = "
module m (input clk, input a, output q);
 wire n1, n2, n3, n4;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUFO b1 (.A(n1), .Y(n2));
 INV u1 (.A(n2), .Y(n3));
 INV u2 (.A(n2), .Y(n4));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_lib]);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &NetLoadOverrides::new());
    let n2_row = find_net(&netlist, "n2");
    check_exp(loads.total_cap(n2_row, Tr::Rise, An::Late), exp, "C n2");

    let nt = NetlistTiming::build(&netlist, &lib);
    let mut sb = StartBuilder::new(1);
    seed_dff_launch(&nt, &lib, &mut sb, "r1/Q", 0.01);
    let vals = nt.propagate(1, &loads, sb.build());

    let b1_y = nt.node("b1/Y").unwrap() as usize;
    let r2_d = nt.node("r2/D").unwrap() as usize;
    check_exp(
        vals.arrival(b1_y, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "n2 rise",
    );
    check_exp(
        vals.arrival(b1_y, 0, Tr::Fall.index(), An::Early.index()),
        exp,
        "n2 fall",
    );
    check_exp(
        vals.transition(b1_y, Tr::Rise.index(), An::Late.index()),
        exp,
        "n2 slew rise",
    );
    check_exp(
        vals.transition(b1_y, Tr::Fall.index(), An::Early.index()),
        exp,
        "n2 slew fall",
    );

    let n3_fall_late = vals.arrival(r2_d, 0, Tr::Fall.index(), An::Late.index());
    let n3_rise_early = vals.arrival(r2_d, 0, Tr::Rise.index(), An::Early.index());
    check_exp(2.0 - 0.10 - n3_fall_late, exp, "r2/D setup");
    check_exp(n3_rise_early - 0.05, exp, "r2/D hold");
}

// STA-REQ-0016, STA-REQ-0108: analytic case `thresholds`
// (`verification/analytic/sta/thresholds/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_thresholds() {
    let exp = &analytic_expected("thresholds")["base"];
    let thr_header = "  input_threshold_pct_rise : 40;\n  input_threshold_pct_fall : 60;\n  \
                      output_threshold_pct_rise : 50;\n  output_threshold_pct_fall : 50;\n  \
                      slew_lower_threshold_pct_rise : 30;\n  slew_lower_threshold_pct_fall : 30;\n  \
                      slew_upper_threshold_pct_rise : 70;\n  slew_upper_threshold_pct_fall : 70;\n  \
                      slew_derate_from_library : 0.5;\n";
    let lib_text = analytic_linear_library("lin_thr", 1.0, thr_header);
    let raw_lib = read_text(&lib_text, "lin_thr.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("lin_thr.lib", &lib_text)]).unwrap();

    assert_eq!(
        lib.thresholds,
        CornerThresholds {
            input_threshold_pct_rise: 40.0,
            input_threshold_pct_fall: 60.0,
            output_threshold_pct_rise: 50.0,
            output_threshold_pct_fall: 50.0,
            slew_lower_threshold_pct_rise: 30.0,
            slew_lower_threshold_pct_fall: 30.0,
            slew_upper_threshold_pct_rise: 70.0,
            slew_upper_threshold_pct_fall: 70.0,
            slew_derate_from_library: 0.5,
        }
    );

    let v_text = "
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_lib]);
    let mut overrides = NetLoadOverrides::new();
    let q_net = NetRow(netlist.port_bit_net[2]);
    overrides.add_net_pin_load(q_net, 0.02, 0.02);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &overrides);
    let nt = NetlistTiming::build(&netlist, &lib);

    let mut sb = StartBuilder::new(1);
    let d_node = nt.node("d").unwrap();
    sb.set_tagged(d_node, 0, Tr::Rise, [0.5, 0.5], [0.0, 0.0]);
    sb.set_tagged(d_node, 0, Tr::Fall, [0.5, 0.5], [0.0, 0.0]);
    seed_dff_launch(&nt, &lib, &mut sb, "r1/Q", 0.01);
    seed_dff_launch(&nt, &lib, &mut sb, "r2/Q", 0.02);

    let vals = nt.propagate(1, &loads, sb.build());
    let r1_d = nt.node("r1/D").unwrap() as usize;
    let r2_d = nt.node("r2/D").unwrap() as usize;
    let q_port = nt.node("q").unwrap() as usize;

    check_exp(
        2.0 - 0.10 - vals.arrival(r2_d, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "r2/D setup",
    );
    check_exp(
        vals.arrival(r2_d, 0, Tr::Fall.index(), An::Early.index()) - 0.05,
        exp,
        "r2/D hold",
    );
    check_exp(
        2.0 - 0.10 - vals.arrival(r1_d, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "r1/D setup",
    );
    check_exp(
        vals.arrival(r1_d, 0, Tr::Fall.index(), An::Early.index()) - 0.05,
        exp,
        "r1/D hold",
    );
    check_exp(
        2.0 - 0.30 - vals.arrival(q_port, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "q setup",
    );
    check_exp(
        vals.arrival(q_port, 0, Tr::Fall.index(), An::Early.index()) + 0.30,
        exp,
        "q hold",
    );
}

// STA-REQ-0115: analytic case `per_pin_slew`
// (`verification/analytic/sta/per_pin_slew/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_per_pin_slew() {
    let exp = &analytic_expected("per_pin_slew")["base"];
    let lib_text = analytic_linear_library("lin", 1.0, "");
    let raw_lib = read_text(&lib_text, "lin.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("lin.lib", &lib_text)]).unwrap();

    let v_text = "
module m (input clk, input a, input s, output q);
 wire n1, n2, n3;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 MUX2 x (.A0(n1), .A1(n1), .S(s), .Y(n2));
 BUF b (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_lib]);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &NetLoadOverrides::new());
    let nt = NetlistTiming::build(&netlist, &lib);

    let n1_row = find_net(&netlist, "n1");
    let dff_arc = lib.cell("DFF").unwrap().arcs[0];
    let q_r = lib.arc_delay(
        dff_arc,
        Tr::Rise,
        An::Late,
        0.0,
        loads.driver_load(n1_row, Tr::Rise, An::Late),
    );
    let q_f = lib.arc_delay(
        dff_arc,
        Tr::Fall,
        An::Early,
        0.0,
        loads.driver_load(n1_row, Tr::Fall, An::Early),
    );
    check_exp(q_r.delay, exp, "Q rise");
    check_exp(q_f.delay, exp, "Q fall");
    check_exp(q_r.slew, exp, "Q slew rise");
    check_exp(q_f.slew, exp, "Q slew fall");

    let mut sb = StartBuilder::new(1);
    let r1_q = nt.node("r1/Q").unwrap();
    sb.set_tagged(
        r1_q,
        0,
        Tr::Rise,
        [q_r.delay, q_r.delay],
        [q_r.slew, q_r.slew],
    );
    sb.set_tagged(
        r1_q,
        0,
        Tr::Fall,
        [q_f.delay, q_f.delay],
        [q_f.slew, q_f.slew],
    );
    let s_node = nt.node("s").unwrap();
    sb.set_untagged_slew(s_node, Tr::Rise, [0.4, 0.4]);
    sb.set_untagged_slew(s_node, Tr::Fall, [0.4, 0.4]);

    let vals = nt.propagate(1, &loads, sb.build());
    let x_y = nt.node("x/Y").unwrap() as usize;
    let r2_d = nt.node("r2/D").unwrap() as usize;

    check_exp(
        vals.arrival(x_y, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "Y rise",
    );
    check_exp(
        vals.arrival(x_y, 0, Tr::Fall.index(), An::Early.index()),
        exp,
        "Y fall",
    );
    check_exp(
        vals.transition(x_y, Tr::Rise.index(), An::Late.index()),
        exp,
        "Y slew rise late",
    );
    check_exp(
        vals.transition(x_y, Tr::Fall.index(), An::Late.index()),
        exp,
        "Y slew fall late",
    );
    check_exp(
        vals.transition(x_y, Tr::Rise.index(), An::Early.index()),
        exp,
        "Y slew rise early",
    );
    check_exp(
        vals.transition(x_y, Tr::Fall.index(), An::Early.index()),
        exp,
        "Y slew fall early",
    );
    check_exp(
        2.0 - 0.10 - vals.arrival(r2_d, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "r2/D setup",
    );
    check_exp(
        vals.arrival(r2_d, 0, Tr::Fall.index(), An::Early.index()) - 0.05,
        exp,
        "r2/D hold",
    );
}

// STA-REQ-0017, STA-REQ-0110, STA-REQ-0121, STA-REQ-0223: analytic case `tristate_bus`
// (`verification/analytic/sta/tristate_bus/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_tristate_bus() {
    let exp = &analytic_expected("tristate_bus")["base"];
    let lib_text = analytic_linear_library("lin", 1.0, "");
    let raw_lib = read_text(&lib_text, "lin.lib").unwrap();
    let lib = TimingLib::from_liberty_texts("nom", &[("lin.lib", &lib_text)]).unwrap();

    let v_text = "
module m (input clk, input a, input b, output y, output q);
 wire qa, qe, qe2, n1;
 DFF r1 (.D(a), .CK(clk), .Q(qa));
 DFF r2 (.D(b), .CK(clk), .Q(qe));
 INV i1 (.A(qe), .Y(qe2));
 BUFZ z1 (.A(qa), .EN(qe), .Z(y));
 BUFZ z2 (.A(qa), .EN(qe2), .Z(y));
 INV i2 (.A(y), .Y(n1));
 DFF r3 (.D(n1), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_lib]);
    let y_net = NetRow(netlist.port_bit_net[3]);
    let mut overrides = NetLoadOverrides::new();
    overrides.add_net_pin_load(y_net, 0.02, 0.02);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &overrides);
    check_exp(loads.total_cap(y_net, Tr::Rise, An::Late), exp, "C_bus");

    let nt = NetlistTiming::build(&netlist, &lib);
    let mut sb = StartBuilder::new(1);
    seed_dff_launch(&nt, &lib, &mut sb, "r1/Q", 0.02);
    seed_dff_launch(&nt, &lib, &mut sb, "r2/Q", 0.02);
    let vals = nt.propagate(1, &loads, sb.build());

    let y_node = nt.node("y").unwrap() as usize;
    let r3_d = nt.node("r3/D").unwrap() as usize;

    check_exp(
        vals.arrival(y_node, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "y arr rise late",
    );
    check_exp(
        vals.arrival(y_node, 0, Tr::Rise.index(), An::Early.index()),
        exp,
        "y arr rise early",
    );
    check_exp(
        vals.arrival(y_node, 0, Tr::Fall.index(), An::Late.index()),
        exp,
        "y arr fall late",
    );
    check_exp(
        vals.arrival(y_node, 0, Tr::Fall.index(), An::Early.index()),
        exp,
        "y arr fall early",
    );
    check_exp(
        vals.transition(y_node, Tr::Rise.index(), An::Late.index()),
        exp,
        "y slew rise late",
    );
    check_exp(
        vals.transition(y_node, Tr::Rise.index(), An::Early.index()),
        exp,
        "y slew rise early",
    );
    check_exp(
        vals.transition(y_node, Tr::Fall.index(), An::Late.index()),
        exp,
        "y slew fall late",
    );
    check_exp(
        vals.transition(y_node, Tr::Fall.index(), An::Early.index()),
        exp,
        "y slew fall early",
    );
    check_exp(
        2.0 - 0.30 - vals.arrival(y_node, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "y setup",
    );
    check_exp(
        vals.arrival(y_node, 0, Tr::Fall.index(), An::Early.index()) + 0.30,
        exp,
        "y hold",
    );
    check_exp(
        2.0 - 0.10 - vals.arrival(r3_d, 0, Tr::Rise.index(), An::Late.index()),
        exp,
        "r3/D setup",
    );
    check_exp(
        vals.arrival(r3_d, 0, Tr::Fall.index(), An::Early.index()) - 0.05,
        exp,
        "r3/D hold",
    );
}

// STA-REQ-0010, STA-REQ-0108: analytic case `two_corners`
// (`verification/analytic/sta/two_corners/DERIVATION.md` and `expected.json`).
#[test]
fn analytic_two_corners() {
    let exp = analytic_expected("two_corners");
    let fast_text = analytic_linear_library("fast_lib", 1.0, "");
    let slow_text = analytic_linear_library("slow_lib", 2.0, "");
    let raw_fast = read_text(&fast_text, "fast.lib").unwrap();
    let lib_fast = TimingLib::from_liberty_texts("fast", &[("fast.lib", &fast_text)]).unwrap();
    let lib_slow = TimingLib::from_liberty_texts("slow", &[("slow.lib", &slow_text)]).unwrap();

    let v_text = "
module m (input clk, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(q));
endmodule
";
    let netlist = freeze_verilog("net.v", v_text, Some("m"), &[raw_fast]);

    for (corner_name, lib) in [("fast", &lib_fast), ("slow", &lib_slow)] {
        let cexp = &exp[corner_name];
        let loads = NetLoads::compute(&netlist, lib, &Ideal, &NetLoadOverrides::new());
        let nt = NetlistTiming::build(&netlist, lib);
        let dff = lib.cell("DFF").unwrap();
        let dff_arc = dff.arcs[0];
        let dl = DriverLoad::Lumped { c: 0.01 };
        let q_r = lib.arc_delay(dff_arc, Tr::Rise, An::Late, 0.0, dl);
        let q_f = lib.arc_delay(dff_arc, Tr::Fall, An::Early, 0.0, dl);
        check_exp(q_r.delay, cexp, "Q rise");
        check_exp(q_f.delay, cexp, "Q fall");

        let mut sb = StartBuilder::new(1);
        seed_dff_launch(&nt, lib, &mut sb, "r1/Q", 0.01);
        let vals = nt.propagate(1, &loads, sb.build());

        let r2_d = nt.node("r2/D").unwrap() as usize;
        let d_late = vals.arrival(r2_d, 0, Tr::Rise.index(), An::Late.index());
        let d_early = vals.arrival(r2_d, 0, Tr::Fall.index(), An::Early.index());
        check_exp(d_late, cexp, "r2/D late");
        check_exp(d_early, cexp, "r2/D early");

        let su = lib.eval_check(&dff.checks[0], Tr::Rise, 0.0, 0.0, 0.0);
        let hd = lib.eval_check(&dff.checks[1], Tr::Fall, 0.0, 0.0, 0.0);
        check_exp(2.0 - su - d_late, cexp, "r2/D setup");
        check_exp(d_early - hd, cexp, "r2/D hold");
    }
}

// STA-REQ-0010, STA-REQ-0016: duplicate cell definition in a corner (`T0012`) and differing
// thresholds within one corner (`T0013`) are rejected with errors.
#[test]
fn duplicate_cell_and_threshold_mismatch_errors() {
    let text_15 = library(Variant::Wt15);
    let text_thr = library(Variant::Thresholds);

    let err_dup = TimingLib::from_liberty_texts("nom", &[("a.lib", &text_15), ("b.lib", &text_15)])
        .unwrap_err();
    assert_eq!(err_dup.code, "T0012");
    assert!(
        err_dup.message.contains("defined more than once"),
        "{}",
        err_dup.message
    );

    let err_thr = TimingLib::from_liberty_texts(
        "nom",
        &[("wt_15.lib", &text_15), ("wt_thresholds.lib", &text_thr)],
    )
    .unwrap_err();
    assert_eq!(err_thr.code, "T0013");
    assert!(
        err_thr.message.contains("slew_derate_from_library"),
        "{}",
        err_thr.message
    );
}

// STA-REQ-0015, STA-REQ-0120: aggregated warnings for unanalyzed arcs (`T0015`, once per type)
// and out-of-range table lookups / design-rule limits (`T0016`, `T0017`, `T0018`, once per
// construct).
#[test]
fn aggregated_unanalyzed_arc_and_out_of_range_warnings() {
    let mut raw_lib = read_text(&library(Variant::Wt15), "wt_15.lib").unwrap();
    raw_lib.default_max_transition = Some(0.12);
    raw_lib.default_max_capacitance = Some(0.015);
    let buf_idx = raw_lib
        .cells
        .iter()
        .position(|c| c.name == "wt_buf_1")
        .unwrap();
    for tt in ["min_pulse_width", "minimum_period", "non_seq_setup_rising"] {
        raw_lib.cells[buf_idx].pins[0].arcs.push(TimingArc {
            related_pin: "A".into(),
            timing_type: tt.into(),
            ..TimingArc::default()
        });
    }
    let lib = TimingLib::from_corner("nom", &[raw_lib.clone()]).unwrap();
    // STA-REQ-0800: pulse groups are pulse checks, never unanalyzed arcs; these two have no
    // constraint table and are noted instead.
    let buf = lib.cell("wt_buf_1").unwrap();
    assert_eq!(buf.unanalyzed_arcs, 1);
    assert!(buf.pulse_checks.is_empty());
    assert_eq!(
        buf.notes
            .iter()
            .filter(|n| n.contains("without a constraint table"))
            .count(),
        2
    );

    let verilog = "
module top (input a, output y);
  wire n1, n2;
  wt_buf_1 u1 (.A(a), .Y(n1));
  wt_buf_1 u2 (.A(n1), .Y(n2));
  wt_buf_1 u3 (.A(n2), .Y(y));
endmodule
";
    let netlist = freeze_verilog("top.v", verilog, Some("top"), &[raw_lib]);
    let (total_unanalyzed, unanalyzed_diags) = lib.unanalyzed_arc_warnings(&netlist);
    assert_eq!(total_unanalyzed, 3);
    assert_eq!(unanalyzed_diags.len(), 1);
    assert!(unanalyzed_diags.iter().all(|d| d.code == "T0015"));

    let mut overrides = NetLoadOverrides::new();
    let y_net = NetRow(netlist.port_bit_net[1]);
    overrides.add_net_pin_load(y_net, 0.08, 0.08);
    let loads = NetLoads::compute(&netlist, &lib, &Ideal, &overrides);
    let nt = NetlistTiming::build(&netlist, &lib);

    let mut sb = StartBuilder::new(1);
    let a_node = nt.node("a").unwrap();
    sb.set_tagged(a_node, 0, Tr::Rise, [0.0, 0.0], [0.80, 0.80]);
    sb.set_tagged(a_node, 0, Tr::Fall, [0.0, 0.0], [0.80, 0.80]);
    let vals = nt.propagate(1, &loads, sb.build());

    let mut tracker = RangeTracker::new();
    loads.check_max_capacitance(&netlist, &lib, &mut tracker);
    nt.track_ranges(&vals, &loads, &mut tracker);
    let warnings = tracker.warnings(&lib.lut_bank);

    let t16: Vec<_> = warnings.iter().filter(|d| d.code == "T0016").collect();
    let t17: Vec<_> = warnings.iter().filter(|d| d.code == "T0017").collect();
    let t18: Vec<_> = warnings.iter().filter(|d| d.code == "T0018").collect();

    assert_eq!(t16.len(), 4, "one T0016 per table construct: {t16:?}");
    assert_eq!(t17.len(), 2, "one T0017 per pin construct: {t17:?}");
    assert_eq!(t18.len(), 1, "one T0018 per driver pin construct: {t18:?}");

    let spec = NetLoadSpec::uniform(0.01, 0.02);
    assert_close(spec.pin_load[0][0], 0.01, "uniform pin_load");
    assert_close(spec.wire_load[1][1], 0.02, "uniform wire_load");
    assert_close(
        lib.lut_bank.eval(NO_TABLE, [0.1, 0.01, 0.0]),
        0.0,
        "NO_TABLE eval",
    );
}
