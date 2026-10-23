// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions, clock groups and case analysis in timing sessions (`owner: E11`;
//! `STA-REQ-0600` to `STA-REQ-0602`, `STA-REQ-0605`).
//!
//! The oracle of every test is a full analysis of the session's netlist
//! ([`EngineAnalysis::analyze`], the analysis of `ruststa run`), compared with the session bit for
//! bit after every edit: values, tags with exception states, check records, endpoints, summaries,
//! diagnostics and the timing table.

mod support;

use std::fs;

use rusteda_core::liberty::Library;
use rusteda_formats::liberty::read_text;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::incr::{SessionError, TimingSession};
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::prop::endpoints::EndpointStatus;
use ruststa_fixtures::liberty::{Variant, library};
use support::mutate::{
    LibCatalog, Shape, base_sdc, build_netlist, exception_sdc, parse_sdc,
    run_random_mutation_campaign,
};

fn lib(v: Variant) -> Library {
    read_text(&library(v), v.name()).expect("synthetic library")
}

const SHAPE: Shape = Shape {
    inputs: 6,
    regs: 8,
    comb: 24,
    outputs: 6,
    clocks: 2,
    latches: 2,
};

// STA-REQ-0600, STA-REQ-0602: a session applies the exceptions, clock groups and case analysis
// of its constraints as the full analysis does: tags with exception states, endpoints that only
// false paths reach, moved relations; no refusal `T0074`.
#[test]
fn sessions_apply_exceptions_like_the_full_analysis() {
    let l = lib(Variant::Full);
    let netlist = build_netlist(&l, 0x0E0C_0001, &SHAPE);
    let sdc = exception_sdc(&SHAPE, &LibCatalog::from_library(&l));
    let session = TimingSession::from_sdc(
        netlist.clone(),
        vec![l.clone()],
        parse_sdc(&sdc),
        AnalysisOptions::default(),
    )
    .expect("a session takes exceptions and clock groups");
    session.verify_against_full().expect("bitwise equal");

    let c = session.default_corner();
    assert!(c.exceptions.is_active());
    assert!(
        c.tags.len() > 4,
        "the -from states make tags beyond the 4 base tags: {}",
        c.tags.len()
    );
    // The exceptions change the checks: compared with the same design without them, checks are
    // removed or their relations moved.
    let plain = TimingSession::from_sdc(
        netlist.clone(),
        vec![l.clone()],
        parse_sdc(&base_sdc(&SHAPE)),
        AnalysisOptions::default(),
    )
    .expect("plain session");
    let p = plain.default_corner();
    assert!(!p.exceptions.is_active() && p.tags.len() == 4);
    let relations = |c: &ruststa::engine::analysis::CornerAnalysis| {
        c.endpoints
            .records
            .iter()
            .map(|r| (r.endpoint, r.kind, r.relation.to_bits()))
            .collect::<Vec<_>>()
    };
    assert_ne!(relations(c), relations(p));
    let false_only = c
        .endpoints
        .endpoints
        .values()
        .filter(|e| e.status == EndpointStatus::FalsePathOnly)
        .count();
    assert!(
        false_only > 0 || c.endpoints.records.len() < p.endpoints.records.len(),
        "false paths and clock groups remove checks"
    );

    // A full analysis of the same netlist, independent of the session code, agrees.
    let frozen = session.netlist().freeze();
    let corners = [CornerLibs::default_corner(std::slice::from_ref(&l))];
    let full = EngineAnalysis::analyze_sdc(
        &frozen,
        &corners,
        &sdc,
        "<sdc>",
        &AnalysisOptions::default(),
        rusteda_core::job::Ctx::NONE,
    )
    .expect("full analysis");
    assert_eq!(
        format!("{:?}", full.corners[0].endpoints.records),
        format!("{:?}", c.endpoints.records)
    );

    // What the full analysis rejects, the session rejects with the same code.
    let bad = format!(
        "{sdc}set_max_delay 1.0 -ignore_clock_latency -from [get_ports in1] -to [get_ports out2]\n"
    );
    let compiled = CompiledConstraints::compile(&parse_sdc(&bad), &frozen, &corners).unwrap();
    let full_err = EngineAnalysis::analyze(
        &frozen,
        &corners,
        compiled,
        &AnalysisOptions::default(),
        rusteda_core::job::Ctx::NONE,
    )
    .expect_err("T0071");
    let SessionError::Diag(session_err) = TimingSession::from_sdc(
        netlist,
        vec![l],
        parse_sdc(&bad),
        AnalysisOptions::default(),
    )
    .expect_err("T0071") else {
        panic!("a coded rejection");
    };
    assert_eq!(full_err[0].code, "T0071");
    assert_eq!(session_err[0].code, "T0071");
}

// STA-REQ-0601, STA-REQ-0605: 1,000 random edits on designs with two clocks, transparent
// latches, false paths, multicycle paths, maximum and minimum delays, clock groups and case
// analysis, on three synthetic libraries (`wt_15` has latches with `D -> Q` arcs, so its designs
// relaunch latch data); after every edit the session equals a full analysis bit for bit, and the
// cones stay local (fewer node evaluations than full propagations).
#[test]
fn exception_campaigns_wt_full_wt_rich_and_wt_15_bitwise() {
    for (v, seed) in [
        (Variant::Full, 0x0E0C_F011u64),
        (Variant::Rich, 0x0E0C_81C4),
        (Variant::Wt15, 0x0E0C_0015),
    ] {
        let l = lib(v);
        let netlist = build_netlist(&l, seed, &SHAPE);
        let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(&l)));
        if v == Variant::Wt15 {
            let s = TimingSession::from_sdc(
                netlist.clone(),
                vec![l.clone()],
                sdc.clone(),
                AnalysisOptions::default(),
            )
            .expect("session");
            assert!(
                !s.default_corner().latch_maps.is_empty(),
                "the latches of the design relaunch their data"
            );
        }
        let (evaluated, full) =
            run_random_mutation_campaign(v.name(), (netlist, sdc), &l, (seed ^ 0x5EED, 1_000, 1));
        assert!(
            evaluated < full / 2,
            "{}: {evaluated} node evaluations for {full} of full propagations",
            v.name()
        );
    }
}

// STA-REQ-0605: 1,000 random edits with exceptions, clock groups and case analysis on the `tt`
// library of each PDK, compared with a full analysis after every edit. Invoked by
// `cargo xtask verify pdk`.
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_exception_campaigns_bitwise() {
    for (idx, (tag, path)) in support::pdk::campaign_libraries().iter().enumerate() {
        let text = fs::read_to_string(path).expect("read PDK liberty file");
        let name = path.file_name().expect("file name").to_string_lossy();
        let l = read_text(&text, &name).expect("parse PDK liberty file");
        let netlist = build_netlist(&l, 0x9E40_0000 + idx as u64, &SHAPE);
        let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(&l)));
        let s = TimingSession::from_sdc(
            netlist.clone(),
            vec![l.clone()],
            sdc.clone(),
            AnalysisOptions::default(),
        )
        .expect("session");
        assert!(
            !s.default_corner().latch_maps.is_empty(),
            "{tag}: the latches of the design relaunch their data"
        );
        let (evaluated, full) = run_random_mutation_campaign(
            tag,
            (netlist, sdc),
            &l,
            (0x9E41_0000 + idx as u64, 1_000, 1),
        );
        assert!(evaluated < full / 2, "{tag}: {evaluated} of {full}");
    }
}
