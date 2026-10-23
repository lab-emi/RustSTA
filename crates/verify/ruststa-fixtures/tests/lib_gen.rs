// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests for closed-form Liberty library generators (`lib_gen` and `liberty`).

use rusteda_formats::liberty::read_text;
use ruststa_fixtures::lib_gen::{
    AffineLut2x2, Variant, eval_wireload_small_cap, eval_wireload_small_res, library, wt_3d, wt_15,
    wt_cond, wt_extrap, wt_gfconv, wt_nonmono, wt_outcap, wt_thresholds, wt_tristate, wt_units,
    wt_wireload,
};

// Closed-form 2x2 affine tables match bilinear interpolation and linear extrapolation to machine
// precision.
#[test]
fn closed_form_lut_matches_liberty_lookup_and_extrapolation() {
    let lib = read_text(&wt_15(), "wt_15").expect("wt_15 parses");
    let inv = lib.cell("wt_inv_1").expect("wt_inv_1");
    let y = inv.pin("Y").expect("pin Y");
    let arc = &y.arcs[0];
    let cr = arc.cell_rise.as_ref().expect("cell_rise");
    let rt = arc.rise_transition.as_ref().expect("rise_transition");

    for slew in [0.05, 0.10, 0.25, 0.50, 0.75] {
        for load in [0.005, 0.01, 0.03, 0.05, 0.08] {
            let want_d = AffineLut2x2::DEFAULT_DELAY.eval(slew, load);
            let got_d = cr.lookup(slew, load);
            assert!(
                (got_d - want_d).abs() < 1e-12,
                "delay at ({slew}, {load}): got {got_d}, want {want_d}"
            );
            let want_s = AffineLut2x2::DEFAULT_SLEW.eval(slew, load);
            let got_s = rt.lookup(slew, load);
            assert!(
                (got_s - want_s).abs() < 1e-12,
                "slew at ({slew}, {load}): got {got_s}, want {want_s}"
            );
        }
    }
}

// `wt_units` scales 100ps and 10fF to match `wt_15` physical values in ns and pF.
#[test]
fn units_variant_scales_to_physical_ns_and_pf() {
    let lib15 = read_text(&wt_15(), "wt_15").expect("wt_15");
    let libu = read_text(&wt_units(), "wt_units").expect("wt_units");
    assert!((libu.time_unit_ns - 0.1).abs() < 1e-12);
    assert!((libu.cap_unit_pf - 0.01).abs() < 1e-12);

    let inv15 = lib15.cell("wt_inv_1").unwrap();
    let invu = libu.cell("wt_inv_1").unwrap();
    let cap15_pf = inv15.pin("A").unwrap().capacitance * lib15.cap_unit_pf;
    let capu_pf = invu.pin("A").unwrap().capacitance * libu.cap_unit_pf;
    assert!((cap15_pf - capu_pf).abs() < 1e-12);

    let cr15 = inv15.pin("Y").unwrap().arcs[0].cell_rise.as_ref().unwrap();
    let cru = invu.pin("Y").unwrap().arcs[0].cell_rise.as_ref().unwrap();
    let d15_ns = cr15.lookup(0.25, 0.03) * lib15.time_unit_ns;
    let du_ns = cru.lookup(2.5, 3.0) * libu.time_unit_ns;
    assert!((d15_ns - du_ns).abs() < 1e-12, "{d15_ns} vs {du_ns}");
}

// All closed-form variants parse and expose their intended constructs (thresholds, wireload,
// conditional arcs, tri-state, outcap, 3D LUT, non-monotonic index, extrapolation, gf180mcu
// conventions).
#[test]
fn all_closed_form_variants_expose_expected_constructs() {
    for v in Variant::ALL {
        let text = library(v);
        let parsed = read_text(&text, v.name()).unwrap_or_else(|e| panic!("{}: {e}", v.name()));
        assert_eq!(parsed.name, v.name());
        if matches!(v, Variant::Minimal | Variant::ResetOnly) {
            assert!(!parsed.cells.is_empty(), "{}", v.name());
        } else {
            assert!(parsed.cells.len() >= 15, "{}", v.name());
        }
    }

    let thr = read_text(&wt_thresholds(), "wt_thresholds").unwrap();
    assert!(!thr.cells.is_empty());

    let wl = read_text(&wt_wireload(), "wt_wireload").unwrap();
    assert!(!wl.cells.is_empty());
    assert!((eval_wireload_small_cap(1) - 0.02).abs() < 1e-12);
    assert!((eval_wireload_small_res(2) - 0.125).abs() < 1e-12);
    assert!(eval_wireload_small_cap(8) > eval_wireload_small_cap(4));

    let cond = read_text(&wt_cond(), "wt_cond").unwrap();
    let xor_cond = cond.cell("wt_xor2_cond").expect("wt_xor2_cond");
    assert_eq!(xor_cond.pin("Y").unwrap().arcs.len(), 3);

    let tri = read_text(&wt_tristate(), "wt_tristate").unwrap();
    assert!(tri.cell("wt_tbuf_n").is_some());
    assert!(tri.cell("wt_keeper").is_some());

    let outcap = read_text(&wt_outcap(), "wt_outcap").unwrap();
    let inv_out = outcap.cell("wt_inv_1").unwrap().pin("Y").unwrap();
    assert!((inv_out.capacitance - 0.005).abs() < 1e-12);

    let l3d = read_text(&wt_3d(), "wt_3d").unwrap();
    let buf3d = l3d.cell("wt_buf_3d").expect("wt_buf_3d");
    let cr3 = buf3d.pin("Y").unwrap().arcs[0].cell_rise.as_ref().unwrap();
    assert_eq!(cr3.shape3(), (2, 2, 2));

    let nonmono = read_text(&wt_nonmono(), "wt_nonmono").unwrap();
    let bad = nonmono.cell("wt_inv_nonmono").expect("wt_inv_nonmono");
    assert!(
        bad.problems
            .iter()
            .any(|p| p.contains("not strictly increasing")),
        "{:?}",
        bad.problems
    );

    let extrap = read_text(&wt_extrap(), "wt_extrap").unwrap();
    assert!(extrap.cell("wt_buf_1").is_some());

    let gf = read_text(&wt_gfconv(), "wt_gfconv").unwrap();
    let fill = gf.cell("wt_fill_1").expect("wt_fill_1");
    assert!(fill.pins.is_empty() && fill.dont_use && !fill.pg_pins.is_empty());
    let aon = gf.cell("wt_aon_buf").expect("wt_aon_buf");
    assert!(aon.always_on && aon.is_isolation_cell);
}
