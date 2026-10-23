// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for `WireModel`, `Ideal`, and `WireloadLumped` (`STA-REQ-0122` to
//! `STA-REQ-0125`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;
use rusteda_core::liberty::{Library, WireLoad, WireLoadMode};
use rusteda_core::netlist::{Conn, GateNetlist, PinDir, PortDir};
use rusteda_formats::liberty::read_text;
use ruststa::engine::delay::wire::{
    DIAG_WIRELOAD_FALLBACK, DriverLoad, ResolvedWireModel, WireModel, WireScope, WireloadConfig,
    WireloadLumped, compute_total_cell_area,
};
use ruststa::engine::types::{An, DelaySlew, NetRow, NodeId, Tr};
use ruststa_fixtures::lib_gen::{Variant, eval_wireload_small_cap, library};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("workspace root")
        .to_path_buf()
}

fn wt_wireload_lib() -> Library {
    let text = library(Variant::Wireload);
    read_text(&text, "wt_wireload.lib").expect("parse wt_wireload.lib")
}

/// Builds a synthetic netlist against `wt_wireload` cells with:
/// - input port `a`, output port `y`, output port `z`, inout port `io`;
/// - internal nets `n_f0` (0 loads), `n_f1` (1 load), `n_f2` (2 loads), `n_f3` (3 loads),
///   `n_f4` (4 loads), `n_f6` (6 loads);
/// - port-aliased net `n_alias_port` connected to `z` via `assign`.
fn build_wt_wireload_netlist() -> (
    rusteda_core::netlist::FrozenNetlist<'static>,
    FxHashMap<String, NetRow>,
) {
    let mut g = GateNetlist::new("top").unwrap();
    let buf = g
        .add_cell_type("wt_buf_1", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();
    let dff = g
        .add_cell_type(
            "wt_dff_p",
            &[
                ("CLK", PinDir::Input),
                ("D", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .unwrap();

    g.add_port("a", PortDir::Input, None).unwrap();
    g.add_port("y", PortDir::Output, None).unwrap();
    g.add_port("z", PortDir::Output, None).unwrap();
    g.add_port("io", PortDir::Inout, None).unwrap();

    let net_a = g.find_net("a", None).unwrap();
    let net_y = g.find_net("y", None).unwrap();
    let net_z = g.find_net("z", None).unwrap();
    let net_io = g.find_net("io", None).unwrap();

    let net_f0 = g.add_net(Some("n_f0"), None).unwrap();
    let net_f1 = g.add_net(Some("n_f1"), None).unwrap();
    let net_f2 = g.add_net(Some("n_f2"), None).unwrap();
    let net_f3 = g.add_net(Some("n_f3"), None).unwrap();
    let net_f4 = g.add_net(Some("n_f4"), None).unwrap();
    let net_f6 = g.add_net(Some("n_f6"), None).unwrap();
    let net_alias_port = g.add_net(Some("n_alias_port"), None).unwrap();

    // Alias `z` to `n_alias_port` via continuous assignment.
    g.add_assign(net_z, Conn::Net(net_alias_port)).unwrap();

    // Driver for n_f0 (0 loads).
    let drv0 = g.add_instance("drv0", buf).unwrap();
    g.connect(drv0, "A", Conn::Net(net_a)).unwrap();
    g.connect(drv0, "Y", Conn::Net(net_f0)).unwrap();

    // Helper to create a driver and `k` load buffers on `net`.
    let mut add_fanout_group = |tag: &str, net: rusteda_core::netlist::NetId, k: usize| {
        let drv = g.add_instance(&format!("drv_{tag}"), buf).unwrap();
        g.connect(drv, "A", Conn::Net(net_a)).unwrap();
        g.connect(drv, "Y", Conn::Net(net)).unwrap();
        for i in 0..k {
            let ld = g.add_instance(&format!("ld_{tag}_{i}"), buf).unwrap();
            g.connect(ld, "A", Conn::Net(net)).unwrap();
        }
    };

    add_fanout_group("f1", net_f1, 1);
    add_fanout_group("f2", net_f2, 2);
    add_fanout_group("f3", net_f3, 3);
    add_fanout_group("f4", net_f4, 4);
    add_fanout_group("f6", net_f6, 6);
    add_fanout_group("alias", net_alias_port, 3);

    // Also connect a load buffer on output port `y` and a DFF on `io` so port nets have nonzero
    // instance load pins while still being port-connected nets.
    let ld_y = g.add_instance("ld_y", buf).unwrap();
    g.connect(ld_y, "A", Conn::Net(net_y)).unwrap();
    let ff_io = g.add_instance("ff_io", dff).unwrap();
    g.connect(ff_io, "CLK", Conn::Net(net_a)).unwrap();
    g.connect(ff_io, "D", Conn::Net(net_io)).unwrap();
    g.connect(ff_io, "Q", Conn::Net(net_y)).unwrap();

    let frozen = g.freeze();
    let mut map = FxHashMap::default();
    for (idx, rec) in frozen.nets.iter().enumerate() {
        let name = frozen.string(rec.name).unwrap().to_string();
        map.insert(name, NetRow(idx as u32));
    }
    (frozen, map)
}

// STA-REQ-0122, STA-REQ-0123, STA-REQ-0124: verifies piecewise-linear interpolation, slope
// extrapolation above F_max and below F_min (clamped at 0.0), instance-pin fanout counting, and
// zero wire load on port-connected nets (including nets aliased via ASSIGNS).
#[test]
fn wt_wireload_interpolation_extrapolation_and_port_net_rule() {
    let lib = wt_wireload_lib();
    let (frozen, nets) = build_wt_wireload_netlist();

    let (resolved, diags) =
        ResolvedWireModel::resolve_library(&frozen, &lib, &WireloadConfig::new());
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let wl = resolved.as_lumped().expect("expected WireloadLumped");
    assert_eq!(wl.model_name(), "wl_small");
    assert_eq!(wl.mode(), &WireLoadMode::Top);
    assert_eq!(
        resolved.scope(),
        WireScope::lumped("wl_small".to_string(), "top".to_string())
    );

    // Check closed-form `wl_small` lengths and capacitances across fanouts 0..=10.
    assert!((wl.length_for_fanout(0) - 0.0).abs() <= 1e-12);
    assert!((wl.length_for_fanout(1) - 10.0).abs() <= 1e-12);
    assert!((wl.length_for_fanout(2) - 25.0).abs() <= 1e-12);
    assert!((wl.length_for_fanout(3) - 42.5).abs() <= 1e-12);
    assert!((wl.length_for_fanout(4) - 60.0).abs() <= 1e-12);
    assert!((wl.length_for_fanout(6) - 100.0).abs() <= 1e-12);

    for fo in 0..=10u32 {
        let got = wl.capacitance_for_fanout(fo);
        let want = eval_wireload_small_cap(fo as usize);
        assert!(
            (got - want).abs() <= 1e-12,
            "fanout {fo}: got {got}, expected {want}"
        );
    }

    // Internal nets: driver output pins do not count toward load fanout; only Input/Inout pins do.
    for (name, want_fo, want_cap) in [
        ("n_f0", 0u32, 0.0),
        ("n_f1", 1, 0.020),
        ("n_f2", 2, 0.050),
        ("n_f3", 3, 0.085),
        ("n_f4", 4, 0.120),
        ("n_f6", 6, 0.200),
    ] {
        let row = nets[name];
        assert!(!wl.is_port_net(row), "{name} should be an internal net");
        assert_eq!(wl.net_fanout(row), want_fo, "fanout mismatch on {name}");
        for an in An::ALL {
            let c_wire = wl.wire_capacitance(row, an);
            assert!(
                (c_wire - want_cap).abs() <= 1e-12,
                "{name} ({an:?}): got {c_wire}, expected {want_cap}"
            );
            let dl = wl.driver_load(row, an, 0.015);
            assert_eq!(dl, DriverLoad::Lumped { c: 0.015 + c_wire });
            assert_eq!(
                wl.load_delay(row, NodeId(0), Tr::Rise, an, 0.12),
                DelaySlew {
                    delay: 0.0,
                    slew: 0.12
                }
            );
        }
    }

    // Port-connected nets (`a`, `y`, `z`, `io`, and `n_alias_port` aliased to `z`) receive 0.0
    // wire capacitance from the table even though they have connected instance load pins.
    for port_net_name in ["a", "y", "z", "io", "n_alias_port"] {
        let row = nets[port_net_name];
        assert!(
            wl.is_port_net(row),
            "{port_net_name} must be classified as port-connected"
        );
        assert!(
            wl.net_fanout(row) >= 1,
            "{port_net_name} has connected load pins"
        );
        for an in An::ALL {
            assert_eq!(
                wl.wire_capacitance(row, an),
                0.0,
                "port net {port_net_name} must get 0 table wire load"
            );
            assert_eq!(
                wl.driver_load(row, an, 0.025),
                DriverLoad::Lumped { c: 0.025 }
            );
        }
    }

    // Explicit `set_load -wire_load` override on a port net or internal net replaces table C_wire.
    let cfg_override = WireloadConfig::new()
        .with_net_wire_load_bounds(nets["y"], 0.003, 0.007)
        .with_net_wire_load(nets["n_f2"], 0.099);
    let (wl_ov, diags_ov) = WireloadLumped::from_library(&frozen, &lib, &cfg_override);
    assert!(diags_ov.is_empty());
    let wl_ov = wl_ov.unwrap();
    assert!((wl_ov.wire_capacitance(nets["y"], An::Early) - 0.003).abs() <= 1e-12);
    assert!((wl_ov.wire_capacitance(nets["y"], An::Late) - 0.007).abs() <= 1e-12);
    assert!((wl_ov.wire_capacitance(nets["n_f2"], An::Early) - 0.099).abs() <= 1e-12);
    assert!((wl_ov.wire_capacitance(nets["n_f2"], An::Late) - 0.099).abs() <= 1e-12);

    // Downward slope extrapolation when F < F_min and F_min > 1 (STA-REQ-0123).
    let custom_wl = WireLoad {
        name: "wl_fmin2".to_string(),
        resistance: 0.0,
        capacitance: 0.001,
        area: 0.0,
        slope: 10.0,
        fanout_length: vec![(2, 25.0), (4, 45.0)],
    };
    let custom = WireloadLumped::with_model(
        &frozen,
        custom_wl,
        WireLoadMode::Top,
        1.0,
        10.0,
        &FxHashMap::default(),
    );
    // F = 1 < F_min = 2: L(1) = max(0, 25 - 10*(2 - 1)) = 15.0; F = 0: max(0, 25 - 20) = 5.0.
    assert!((custom.length_for_fanout(1) - 15.0).abs() <= 1e-12);
    assert!((custom.length_for_fanout(0) - 5.0).abs() <= 1e-12);
}

// STA-REQ-0125: verifies selection priority order:
// 1. SDC `set_wire_load_model -name <name>` (or `set_wire_load_selection_group <group>`)
// 2. Corner library's `default_wire_load_selection` matched against total cell area
// 3. Corner library's `default_wire_load`
// 4. Fallback to `Ideal` and `T0010` diagnostics when missing or unsupported.
#[test]
fn wt_wireload_selection_order_mode_diagnostics_and_scope_fields() {
    let lib = wt_wireload_lib();
    let (frozen, nets) = build_wt_wireload_netlist();

    // Computed total cell area: 27 `wt_buf_1` instances (area 1.5 each = 40.5) + 1 `wt_dff_p`
    // (area 5.0) = 45.5 (< 500.0 -> `sel_area` selects `wl_small`).
    let area = compute_total_cell_area(&frozen, std::slice::from_ref(&lib));
    assert!((area - 45.5).abs() <= 1e-12, "unexpected total area {area}");

    // 1. Library `default_wire_load_selection` ("sel_area") selects `wl_medium` when area = 800,
    // overriding `default_wire_load : "wl_small"`.
    let (res_med, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib,
        &WireloadConfig::new().with_total_cell_area(800.0),
    );
    assert!(diags.is_empty());
    assert_eq!(
        res_med.scope(),
        WireScope::lumped("wl_medium".to_string(), "top".to_string())
    );
    // For `wl_medium`, cap = 0.004, L(2) = 40.0 -> C_wire = 0.160 pF.
    assert!(
        (res_med
            .as_lumped()
            .unwrap()
            .wire_capacitance(nets["n_f2"], An::Late)
            - 0.160)
            .abs()
            <= 1e-12
    );

    // Area = 3000 -> `default_wire_load_selection` selects `wl_large` (cap = 0.006, L(2) = 60 -> 0.360 pF).
    let (res_large, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib,
        &WireloadConfig::new().with_total_cell_area(3000.0),
    );
    assert!(diags.is_empty());
    assert_eq!(
        res_large.scope(),
        WireScope::lumped("wl_large".to_string(), "top".to_string())
    );
    assert!(
        (res_large
            .as_lumped()
            .unwrap()
            .wire_capacitance(nets["n_f2"], An::Late)
            - 0.360)
            .abs()
            <= 1e-12
    );

    // 2. Explicit SDC `set_wire_load_model -name "wl_large"` overrides `default_wire_load_selection`
    // even at small area (41.0).
    let (res_sdc_model, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib,
        &WireloadConfig::new()
            .with_model("wl_large")
            .with_mode("enclosed"),
    );
    assert!(diags.is_empty());
    assert_eq!(
        res_sdc_model.scope(),
        WireScope::lumped("wl_large".to_string(), "enclosed".to_string())
    );

    // 3. Explicit SDC `set_wire_load_selection_group "sel_area"` with area = 1200 selects `wl_medium`
    // even if `default_wire_load_selection` is cleared on the library.
    let mut lib_no_def_sel = lib.clone();
    lib_no_def_sel.default_wire_load_selection = None;
    let (res_sdc_sel, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib_no_def_sel,
        &WireloadConfig::new()
            .with_selection_group("sel_area")
            .with_total_cell_area(1200.0)
            .with_mode("segmented"),
    );
    assert!(diags.is_empty());
    assert_eq!(
        res_sdc_sel.scope(),
        WireScope::lumped("wl_medium".to_string(), "segmented".to_string())
    );

    // 4. When `default_wire_load_selection` is absent and no SDC override is given, falls back to
    // `default_wire_load` ("wl_small").
    let (res_def_wl, diags) =
        ResolvedWireModel::resolve_library(&frozen, &lib_no_def_sel, &WireloadConfig::new());
    assert!(diags.is_empty());
    assert_eq!(
        res_def_wl.scope(),
        WireScope::lumped("wl_small".to_string(), "top".to_string())
    );

    // 5. Missing SDC `set_wire_load_model` emits `T0010` and falls back to library selection.
    let (res_missing_sdc, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib,
        &WireloadConfig::new().with_model("no_such_wl"),
    );
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, DIAG_WIRELOAD_FALLBACK);
    assert_eq!(
        res_missing_sdc.as_lumped().unwrap().model_name(),
        "wl_small"
    );

    // 6. Unsupported `wire_load_mode` emits `T0010` and defaults to `"top"`.
    let (res_bad_mode, diags) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib,
        &WireloadConfig::new().with_mode("bogus_mode"),
    );
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, DIAG_WIRELOAD_FALLBACK);
    assert_eq!(
        res_bad_mode.scope(),
        WireScope::lumped("wl_small".to_string(), "top".to_string())
    );

    // 7. Library with no wire_load groups (`wt_15`): resolves to `Ideal` (`wire_model = "ideal"`),
    // and emits `T0010` when wire-load estimation is required (`require_wireload = true`).
    let lib_plain = read_text(&library(Variant::Wt15), "wt_15.lib").unwrap();
    let (res_ideal, diags_plain) =
        ResolvedWireModel::resolve_library(&frozen, &lib_plain, &WireloadConfig::new());
    assert!(diags_plain.is_empty());
    assert!(res_ideal.is_ideal());
    assert_eq!(res_ideal.scope(), WireScope::ideal());
    assert_eq!(res_ideal.scope_name(), "ideal");

    let (res_req, diags_req) = ResolvedWireModel::resolve_library(
        &frozen,
        &lib_plain,
        &WireloadConfig::new().with_require_wireload(true),
    );
    assert!(res_req.is_ideal());
    assert_eq!(res_req.scope(), WireScope::ideal());
    assert_eq!(diags_req.len(), 1);
    assert_eq!(diags_req[0].code, DIAG_WIRELOAD_FALLBACK);
}

// STA-REQ-0122, STA-REQ-0123, STA-REQ-0124, STA-REQ-0125: verifies exact closed-form parity with
// `verification/analytic/sta/wireload_lumped/DERIVATION.md` and `expected.json`.
#[test]
fn analytic_derivation_wireload_lumped_matches_expected_json() {
    let expected_path = repo_root().join("verification/analytic/sta/wireload_lumped/expected.json");
    let expected_text = std::fs::read_to_string(&expected_path).expect("read expected.json");
    let parsed: BTreeMap<String, BTreeMap<String, f64>> =
        serde_json::from_str(&expected_text).expect("parse expected.json");
    let exp = &parsed["base"];

    // Build the netlist from `verification/analytic/sta/wireload_lumped/DERIVATION.md`:
    // module m (input clk, input a, output y, output z);
    //  wire n1, n2, n3, w3, w4, w5;
    //  DFF r1 (.D(a), .CK(clk), .Q(n1));
    //  BUF b1 (.A(n1), .Y(n2));
    //  BUF b2 (.A(n2), .Y(n3));
    //  BUF x1 (.A(n2), .Y(y));
    //  DFF r2 (.D(n3), .CK(clk), .Q());
    //  BUF x2 (.A(n3), .Y(z));
    //  BUF x3 (.A(n3), .Y(w3));
    //  BUF x4 (.A(n3), .Y(w4));
    //  BUF x5 (.A(n3), .Y(w5));
    let mut g = GateNetlist::new("m").unwrap();
    let dff = g
        .add_cell_type(
            "DFF",
            &[
                ("D", PinDir::Input),
                ("CK", PinDir::Input),
                ("Q", PinDir::Output),
            ],
        )
        .unwrap();
    let buf = g
        .add_cell_type("BUF", &[("A", PinDir::Input), ("Y", PinDir::Output)])
        .unwrap();

    g.add_port("clk", PortDir::Input, None).unwrap();
    g.add_port("a", PortDir::Input, None).unwrap();
    g.add_port("y", PortDir::Output, None).unwrap();
    g.add_port("z", PortDir::Output, None).unwrap();

    let n_clk = g.find_net("clk", None).unwrap();
    let n_a = g.find_net("a", None).unwrap();
    let n_y = g.find_net("y", None).unwrap();
    let n_z = g.find_net("z", None).unwrap();
    let n_n1 = g.add_net(Some("n1"), None).unwrap();
    let n_n2 = g.add_net(Some("n2"), None).unwrap();
    let n_n3 = g.add_net(Some("n3"), None).unwrap();
    let n_w3 = g.add_net(Some("w3"), None).unwrap();
    let n_w4 = g.add_net(Some("w4"), None).unwrap();
    let n_w5 = g.add_net(Some("w5"), None).unwrap();

    let r1 = g.add_instance("r1", dff).unwrap();
    g.connect(r1, "D", Conn::Net(n_a)).unwrap();
    g.connect(r1, "CK", Conn::Net(n_clk)).unwrap();
    g.connect(r1, "Q", Conn::Net(n_n1)).unwrap();

    let b1 = g.add_instance("b1", buf).unwrap();
    g.connect(b1, "A", Conn::Net(n_n1)).unwrap();
    g.connect(b1, "Y", Conn::Net(n_n2)).unwrap();

    let b2 = g.add_instance("b2", buf).unwrap();
    g.connect(b2, "A", Conn::Net(n_n2)).unwrap();
    g.connect(b2, "Y", Conn::Net(n_n3)).unwrap();

    let x1 = g.add_instance("x1", buf).unwrap();
    g.connect(x1, "A", Conn::Net(n_n2)).unwrap();
    g.connect(x1, "Y", Conn::Net(n_y)).unwrap();

    let r2 = g.add_instance("r2", dff).unwrap();
    g.connect(r2, "D", Conn::Net(n_n3)).unwrap();
    g.connect(r2, "CK", Conn::Net(n_clk)).unwrap();

    let x2 = g.add_instance("x2", buf).unwrap();
    g.connect(x2, "A", Conn::Net(n_n3)).unwrap();
    g.connect(x2, "Y", Conn::Net(n_z)).unwrap();

    for (inst_name, out_net) in [("x3", n_w3), ("x4", n_w4), ("x5", n_w5)] {
        let inst = g.add_instance(inst_name, buf).unwrap();
        g.connect(inst, "A", Conn::Net(n_n3)).unwrap();
        g.connect(inst, "Y", Conn::Net(out_net)).unwrap();
    }

    let frozen = g.freeze();
    let mut net_rows = FxHashMap::default();
    for (idx, rec) in frozen.nets.iter().enumerate() {
        net_rows.insert(
            frozen.string(rec.name).unwrap().to_string(),
            NetRow(idx as u32),
        );
    }

    // `lin_wl` wire_load model from `DERIVATION.md`:
    // capacitance = 0.001 pF, slope = 2.0, fanout_length = [(1, 2.0), (3, 5.0)].
    let wl_lin = WireLoad {
        name: "wl_lin".to_string(),
        resistance: 0.0,
        capacitance: 0.001,
        area: 0.0,
        slope: 2.0,
        fanout_length: vec![(1, 2.0), (3, 5.0)],
    };
    let model = WireloadLumped::with_model(
        &frozen,
        wl_lin.clone(),
        WireLoadMode::Top,
        1.0,
        0.0,
        &FxHashMap::default(),
    );

    // 1. Check L(F) values against `expected.json`.
    assert!((model.length_for_fanout(0) - exp["L(0)"]).abs() <= 1e-12);
    assert!((model.length_for_fanout(1) - exp["L(1)"]).abs() <= 1e-12);
    assert!((model.length_for_fanout(2) - exp["L(2)"]).abs() <= 1e-12);
    assert!((model.length_for_fanout(5) - exp["L(5)"]).abs() <= 1e-12);

    // 2. Check net capacitances (`pin_load = F * 0.01` on internal nets, `0.004` port load on `y`,
    // `0.0` on unloaded port `z` under STA-REQ-0124).
    let c_n1 = model
        .driver_load(net_rows["n1"], An::Late, 1.0 * 0.01)
        .total();
    let c_n2 = model
        .driver_load(net_rows["n2"], An::Late, 2.0 * 0.01)
        .total();
    let c_n3 = model
        .driver_load(net_rows["n3"], An::Late, 5.0 * 0.01)
        .total();
    let c_y = model.driver_load(net_rows["y"], An::Late, 0.004).total();
    let c_z_port_excluded = model.driver_load(net_rows["z"], An::Late, 0.0).total();

    assert!((c_n1 - exp["C n1"]).abs() <= 1e-12);
    assert!((c_n2 - exp["C n2"]).abs() <= 1e-12);
    assert!((c_n3 - exp["C n3"]).abs() <= 1e-12);
    assert!((c_y - exp["C y"]).abs() <= 1e-12);
    assert!((c_z_port_excluded - 0.0).abs() <= 1e-12);

    // Stage equations from `lin_wl`:
    // r1 CK->Q: delay_rise = 0.40 + 1.0 * C, slew_rise = 0.08 + 1.0 * C
    //           delay_fall = 0.35 + 0.9 * C, slew_fall = 0.07 + 1.0 * C
    // BUF A->Y: delay_rise = 0.20 + 0.1 * slew_in + 1.0 * C, slew_rise = 0.06 + 1.0 * C
    //           delay_fall = 0.18 + 0.1 * slew_in + 1.2 * C, slew_fall = 0.05 + 1.1 * C
    let n1_rise = 0.40 + 1.0 * c_n1;
    let n1_slew_rise = 0.08 + 1.0 * c_n1;
    let n1_fall = 0.35 + 0.9 * c_n1;
    let n1_slew_fall = 0.07 + 1.0 * c_n1;

    let n2_rise = n1_rise + (0.20 + 0.1 * n1_slew_rise + 1.0 * c_n2);
    let n2_slew_rise = 0.06 + 1.0 * c_n2;
    let n2_fall = n1_fall + (0.18 + 0.1 * n1_slew_fall + 1.2 * c_n2);
    let n2_slew_fall = 0.05 + 1.1 * c_n2;

    let n3_rise = n2_rise + (0.20 + 0.1 * n2_slew_rise + 1.0 * c_n3);
    let n3_slew_rise = 0.06 + 1.0 * c_n3;
    let n3_fall = n2_fall + (0.18 + 0.1 * n2_slew_fall + 1.2 * c_n3);
    let n3_slew_fall = 0.05 + 1.1 * c_n3;

    assert!((n1_rise - exp["n1 rise"]).abs() <= 1e-12);
    assert!((n1_fall - exp["n1 fall"]).abs() <= 1e-12);
    assert!((n2_rise - exp["n2 rise"]).abs() <= 1e-12);
    assert!((n2_fall - exp["n2 fall"]).abs() <= 1e-12);
    assert!((n3_rise - exp["n3 rise"]).abs() <= 1e-12);
    assert!((n3_fall - exp["n3 fall"]).abs() <= 1e-12);

    let r2_d_setup = 2.0 - 0.10 - n3_rise;
    let r2_d_hold = n3_fall - 0.05;
    let y_rise = n2_rise + (0.20 + 0.1 * n2_slew_rise + 1.0 * c_y);
    let y_fall = n2_fall + (0.18 + 0.1 * n2_slew_fall + 1.2 * c_y);
    let y_setup = 2.0 - 0.2 - y_rise;
    let y_hold = y_fall + 0.2;

    assert!((r2_d_setup - exp["r2/D setup"]).abs() <= 1e-12);
    assert!((r2_d_hold - exp["r2/D hold"]).abs() <= 1e-12);
    assert!((y_setup - exp["y setup"]).abs() <= 1e-12);
    assert!((y_hold - exp["y hold"]).abs() <= 1e-12);

    // And when `z` carries an explicit `set_load -wire_load 0.002`, C(z) and `z` slacks match
    // `expected.json` as well.
    let mut ov = FxHashMap::default();
    ov.insert(net_rows["z"], [0.002, 0.002]);
    let model_with_z =
        WireloadLumped::with_model(&frozen, wl_lin, WireLoadMode::Top, 1.0, 0.0, &ov);
    let c_z = model_with_z
        .driver_load(net_rows["z"], An::Late, 0.0)
        .total();
    let z_rise = n3_rise + (0.20 + 0.1 * n3_slew_rise + 1.0 * c_z);
    let z_fall = n3_fall + (0.18 + 0.1 * n3_slew_fall + 1.2 * c_z);
    let z_setup = 2.0 - 0.2 - z_rise;
    let z_hold = z_fall + 0.2;
    assert!((c_z - exp["C z"]).abs() <= 1e-12);
    assert!((z_setup - exp["z setup"]).abs() <= 1e-12);
    assert!((z_hold - exp["z hold"]).abs() <= 1e-12);
}

fn pdk_dir() -> PathBuf {
    if let Ok(val) = std::env::var("RUSTEDA_PDK_DIR") {
        let p = PathBuf::from(val);
        if p.is_dir() {
            return p;
        }
    }
    repo_root().join("target/pdk")
}

// STA-REQ-0122, STA-REQ-0123, STA-REQ-0124, STA-REQ-0125: verifies `sky130_fd_sc_hd` (tt/ss/ff)
// and `gf180mcu` (7t/9t tt/ss/ff) PDK wire-load resolution, fanout capacitance calculation, and
// alignment against the reference lumped-capacitance delay calculator (`<= 2.4e-6 ns`) and the
// default RC tree calculator (`10-45 ps` gap).
#[test]
#[ignore]
fn pdk_sky130_and_gf180_wireload_lumped_and_reference_parity() {
    let dir = pdk_dir();
    assert!(
        dir.is_dir(),
        "PDK directory {} not found; set RUSTEDA_PDK_DIR",
        dir.display()
    );

    // 1. Check all three sky130 corners (`tt`, `ss`, `ff`).
    for file in [
        "sky130_fd_sc_hd__tt_025C_1v80.lib",
        "sky130_fd_sc_hd__ss_100C_1v60.lib",
        "sky130_fd_sc_hd__ff_n40C_1v95.lib",
    ] {
        let path = dir.join(file);
        let lib = rusteda_formats::liberty::read_file(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        // Build `fanN` netlists (N = 1, 2, 3, 6, 8, 10): `inv_1 drv (.A(a), .Y(w))` driving `N`
        // `inv_2` loads on internal net `w`.
        let inv1 = lib
            .cells
            .iter()
            .find(|c| c.name == "sky130_fd_sc_hd__inv_1")
            .expect("sky130_fd_sc_hd__inv_1");
        let inv2 = lib
            .cells
            .iter()
            .find(|c| c.name == "sky130_fd_sc_hd__inv_2")
            .expect("sky130_fd_sc_hd__inv_2");
        let inv2_a = inv2.pins.iter().find(|p| p.name == "A").unwrap();
        let inv1_y = inv1.pins.iter().find(|p| p.name == "Y").unwrap();
        let arc = inv1_y
            .arcs
            .iter()
            .find(|a| a.related_pin == "A")
            .expect("inv_1 A->Y arc");

        let cases = [
            // (fanout, ref_c_fall, ref_c_rise, ref_d_fall, ref_s_fall, ref_d_rise, ref_s_rise, tree_arr_ld_rise)
            (
                1usize,
                0.004606499_f64,
                0.004972499_f64,
                0.057124805_f64,
                0.039376911_f64,
                0.084002085_f64,
                0.059388239_f64,
                0.095399618_f64,
            ),
            (
                2,
                0.009008013,
                0.009740013,
                0.076659948,
                0.054432280,
                0.112383313,
                0.092170767,
                0.121347435,
            ),
            (
                3,
                0.013516503,
                0.014614503,
                0.092485510,
                0.072214901,
                0.140062168,
                0.130722851,
                0.148982957,
            ),
            (
                6,
                0.026852561,
                0.029048562,
                0.137607828,
                0.126201153,
                0.221832857,
                0.245536014,
                0.230093762,
            ),
            (
                8,
                0.035642073,
                0.038570073,
                0.166788742,
                0.164469361,
                0.275399059,
                0.322561413,
                0.283248186,
            ),
            (
                10,
                0.044431586,
                0.048091583,
                0.195969641,
                0.202737570,
                0.328965276,
                0.399586797,
                0.336569011,
            ),
        ];

        for &(
            n_loads,
            ref_c_fall,
            ref_c_rise,
            ref_d_fall,
            ref_s_fall,
            ref_d_rise,
            ref_s_rise,
            tree_arr_ld_rise,
        ) in &cases
        {
            let mut g = GateNetlist::new(&format!("fan{n_loads}")).unwrap();
            let c_inv1 = g
                .add_cell_type(
                    "sky130_fd_sc_hd__inv_1",
                    &[("A", PinDir::Input), ("Y", PinDir::Output)],
                )
                .unwrap();
            let c_inv2 = g
                .add_cell_type(
                    "sky130_fd_sc_hd__inv_2",
                    &[("A", PinDir::Input), ("Y", PinDir::Output)],
                )
                .unwrap();
            g.add_port("a", PortDir::Input, None).unwrap();
            let net_a = g.find_net("a", None).unwrap();
            let net_w = g.add_net(Some("w"), None).unwrap();

            let drv = g.add_instance("drv", c_inv1).unwrap();
            g.connect(drv, "A", Conn::Net(net_a)).unwrap();
            g.connect(drv, "Y", Conn::Net(net_w)).unwrap();

            for i in 0..n_loads {
                let p_name = format!("y{i}");
                g.add_port(&p_name, PortDir::Output, None).unwrap();
                let net_yi = g.find_net(&p_name, None).unwrap();
                let ld = g.add_instance(&format!("ld{i}"), c_inv2).unwrap();
                g.connect(ld, "A", Conn::Net(net_w)).unwrap();
                g.connect(ld, "Y", Conn::Net(net_yi)).unwrap();
            }

            let frozen = g.freeze();
            let w_idx = frozen
                .nets
                .iter()
                .position(|r| frozen.string(r.name).unwrap() == "w")
                .unwrap();
            let w_row = NetRow(w_idx as u32);

            let (resolved, diags) =
                ResolvedWireModel::resolve_library(&frozen, &lib, &WireloadConfig::new());
            assert!(diags.is_empty());
            assert_eq!(
                resolved.scope(),
                WireScope::lumped("Small".to_string(), "top".to_string())
            );
            let wl = resolved.as_lumped().unwrap();
            assert_eq!(wl.net_fanout(w_row), n_loads as u32);
            assert!(!wl.is_port_net(w_row));

            if file == "sky130_fd_sc_hd__tt_025C_1v80.lib" {
                let pin_fall =
                    (n_loads as f64) * inv2_a.fall_capacitance.unwrap_or(inv2_a.capacitance);
                let pin_rise =
                    (n_loads as f64) * inv2_a.rise_capacitance.unwrap_or(inv2_a.capacitance);
                let c_fall = wl.driver_load(w_row, An::Late, pin_fall).total();
                let c_rise = wl.driver_load(w_row, An::Late, pin_rise).total();
                assert!(
                    (c_fall - ref_c_fall).abs() <= 1e-8,
                    "fan{n_loads} c_fall: got {c_fall}, ref {ref_c_fall}"
                );
                assert!(
                    (c_rise - ref_c_rise).abs() <= 1e-8,
                    "fan{n_loads} c_rise: got {c_rise}, ref {ref_c_rise}"
                );

                // Evaluate NLDM tables at input slew 0.1 ns and compare against the reference
                // lumped-capacitance delay/slew within the decision-1 gate (<= 2.4e-6 ns).
                let in_slew = 0.1_f64;
                let d_fall = arc
                    .cell_fall
                    .as_ref()
                    .unwrap()
                    .lookup_slew_load(in_slew, c_fall);
                let s_fall = arc
                    .fall_transition
                    .as_ref()
                    .unwrap()
                    .lookup_slew_load(in_slew, c_fall);
                let d_rise = arc
                    .cell_rise
                    .as_ref()
                    .unwrap()
                    .lookup_slew_load(in_slew, c_rise);
                let s_rise = arc
                    .rise_transition
                    .as_ref()
                    .unwrap()
                    .lookup_slew_load(in_slew, c_rise);

                assert!((d_fall - ref_d_fall).abs() <= 2.4e-6);
                assert!((s_fall - ref_s_fall).abs() <= 2.4e-6);
                assert!((d_rise - ref_d_rise).abs() <= 2.4e-6);
                assert!((s_rise - ref_s_rise).abs() <= 2.4e-6);

                // Single-stage gap to the default RC tree calculator at load pin ld0/A is ~7-11 ps.
                let tree_gap_ps = (tree_arr_ld_rise - d_rise) * 1e3;
                assert!(
                    (5.0..=15.0).contains(&tree_gap_ps),
                    "fan{n_loads} tree gap {tree_gap_ps:.3} ps outside expected range"
                );
            }
        }
    }

    // 2. Check gf180mcu 7t/9t libraries (no wire_load groups -> resolves to `ideal`).
    let (frozen_small, _) = build_wt_wireload_netlist();
    for file in [
        "gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib",
        "gf180mcu_fd_sc_mcu9t5v0__tt_025C_3v30.lib",
    ] {
        let path = dir.join(file);
        let lib = rusteda_formats::liberty::read_file(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let (res, diags) =
            ResolvedWireModel::resolve_library(&frozen_small, &lib, &WireloadConfig::new());
        assert!(diags.is_empty());
        assert!(res.is_ideal());
        assert_eq!(res.scope(), WireScope::ideal());
    }
}
