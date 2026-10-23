// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for `ruststa::engine::store` (`sta.rbin` / `STA v2` binary store and
//! zero-copy mapped queries; `owner: E10`; `STA-REQ-0302`, `STA-REQ-0305`, `STA-REQ-0312` to
//! `STA-REQ-0315`, `STA-REQ-0401`, `STA-REQ-0405`, `WS-REQ-0030` to `WS-REQ-0038`, `WS-REQ-0045`).

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::liberty::{Library, read_text};
use rusteda_workspace::netlist as ws_netlist;
use rusteda_workspace::{ErrorCode, Invocation, RbinWriter, Rev, Upstream, Workspace};
use ruststa::engine::graph::{CheckKind, TimingGraph};
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::store::hash::{
    InstHashBuilder, NameHashBuilder, StringTableBuilder, hash_node_name,
};
use ruststa::engine::store::layout::{
    ClockRec, CompletenessReport, CornerRec, EndpointKindTag, EndpointRec, EpSlackRec,
    FORMAT_VERSION, KIND, NO_INDEX, NO_PRED, NodeRec, NoteRec, PathHeaderRec, PathPointKind,
    PathPointRec, PathRankRec, ScopeReport, StaMeta, StoreMode, StorePrecision, SummaryRec,
    TABLE_NAME, TABLE_PATH, TagRec, pack_chain_step, pack_f64_words, section,
};
use ruststa::engine::store::{
    StaStore, StaStoreBuildOptions, StaStoreData, build_single_corner_store,
};
use ruststa::engine::types::{NodeId, Tr};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::net_gen::{
    gen_clock_crossing, gen_clock_gated, gen_constant_tied, gen_latch_pipeline, gen_tristate_bus,
};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use support::legacy_import::read_structural_frozen;

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("ruststa-store-{label}-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn inv(cmd: &str) -> Invocation {
    Invocation::new("ruststa", "0.1.0", cmd, cmd.split_whitespace())
}

fn wt_full() -> Library {
    read_text(&library(Variant::Full), Variant::Full.name()).expect("wt_full library")
}

fn wt_15() -> Library {
    read_text(&library(Variant::Wt15), Variant::Wt15.name()).expect("wt_15 library")
}

fn assert_no_neg_zero_f64(label: &str, v: f64) {
    assert_ne!(
        v.to_bits(),
        (-0.0f64).to_bits(),
        "{label}: found -0.0f64 in store"
    );
}

fn assert_no_neg_zero_f32(label: &str, v: f32) {
    assert_ne!(
        v.to_bits(),
        (-0.0f32).to_bits(),
        "{label}: found -0.0f32 in store"
    );
}

fn verify_store_no_neg_zero(label: &str, data: &StaStoreData) {
    for (i, c) in data.clocks.iter().enumerate() {
        assert_no_neg_zero_f64(&format!("{label} clock[{i}].period"), c.period_ns);
        assert_no_neg_zero_f64(&format!("{label} clock[{i}].rise"), c.rise_edge_ns);
        assert_no_neg_zero_f64(&format!("{label} clock[{i}].fall"), c.fall_edge_ns);
        assert_no_neg_zero_f32(&format!("{label} clock[{i}].u_s"), c.uncertainty_setup_ns);
        assert_no_neg_zero_f32(&format!("{label} clock[{i}].u_h"), c.uncertainty_hold_ns);
    }
    for (i, n) in data.node_rec.iter().enumerate() {
        for tr in 0..2 {
            for an in 0..2 {
                assert_no_neg_zero_f64(&format!("{label} node[{i}].slew"), n.slew[tr][an]);
                assert_no_neg_zero_f64(&format!("{label} node[{i}].req"), n.required[tr][an]);
                assert_no_neg_zero_f64(&format!("{label} node[{i}].slack"), n.slack[tr][an]);
                assert_no_neg_zero_f64(&format!("{label} node[{i}].arr"), n.arrival[tr][an]);
            }
        }
    }
    for (i, s) in data.ep_slack.iter().enumerate() {
        assert_no_neg_zero_f64(&format!("{label} ep_slack[{i}].setup"), s.setup_slack);
        assert_no_neg_zero_f64(&format!("{label} ep_slack[{i}].hold"), s.hold_slack);
    }
    for (i, r) in data.path_rank.iter().enumerate() {
        assert_no_neg_zero_f32(&format!("{label} path_rank[{i}]"), r.slack_ns);
    }
    for (i, p) in data.paths.iter().enumerate() {
        assert_no_neg_zero_f64(&format!("{label} path[{i}].slack"), p.slack);
        assert_no_neg_zero_f64(&format!("{label} path[{i}].arr"), p.arrival);
        assert_no_neg_zero_f64(&format!("{label} path[{i}].req"), p.required);
    }
    for (i, pt) in data.path_points.iter().enumerate() {
        assert_no_neg_zero_f64(&format!("{label} pt[{i}].arr"), pt.arrival);
        assert_no_neg_zero_f64(&format!("{label} pt[{i}].delay"), pt.incr_delay);
        assert_no_neg_zero_f64(&format!("{label} pt[{i}].slew"), pt.slew);
    }
    for (i, s) in data.summary.iter().enumerate() {
        assert_no_neg_zero_f64(&format!("{label} sum[{i}].setup_wns"), s.setup_wns);
        assert_no_neg_zero_f64(&format!("{label} sum[{i}].setup_tns"), s.setup_tns);
        assert_no_neg_zero_f64(&format!("{label} sum[{i}].hold_wns"), s.hold_wns);
        assert_no_neg_zero_f64(&format!("{label} sum[{i}].hold_tns"), s.hold_tns);
    }
}

fn build_store_for_frozen(
    frozen: &FrozenNetlist<'_>,
    libs: &[Library],
    tlib: &TimingLib,
    options: &StaStoreBuildOptions,
) -> StaStoreData {
    let nv = NetlistView::new(frozen, libs).expect("NetlistView");
    let graph = TimingGraph::build(&nv, libs).expect("TimingGraph");
    build_single_corner_store(&nv, &graph, tlib, None, options).expect("build_single_corner_store")
}

// STA-REQ-0302, STA-REQ-0401, STA-REQ-0405, WS-REQ-0030..0036:
// Write/read bitwise round-trip and `--threads 1` vs `--threads 32` byte-identity on all 32
// corpus netlists (`CORPUS` + `LANES_K1`) and generated netlists.
#[test]
fn corpus_round_trip_and_thread_determinism_1_vs_32() {
    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");
    let opts = StaStoreBuildOptions::default();

    let pool1 = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("pool 1");
    let pool32 = rayon::ThreadPoolBuilder::new()
        .num_threads(32)
        .build()
        .expect("pool 32");

    let dir = temp_dir("corpus-rt");
    let mut count = 0usize;

    for d in CORPUS.iter().chain([&LANES_K1]) {
        let frozen = read_structural_frozen(d.name, d.text, full_libs)
            .unwrap_or_else(|e| panic!("{}: {e}", d.name));

        let data_t1 =
            pool1.install(|| build_store_for_frozen(&frozen, full_libs, &full_tlib, &opts));
        let bytes_t1 = data_t1.to_bytes().expect("to_bytes t1");

        let data_t32 =
            pool32.install(|| build_store_for_frozen(&frozen, full_libs, &full_tlib, &opts));
        let bytes_t32 = data_t32.to_bytes().expect("to_bytes t32");

        assert_eq!(
            bytes_t1, bytes_t32,
            "{}: sta.rbin bytes differ between --threads 1 and --threads 32",
            d.name
        );
        verify_store_no_neg_zero(d.name, &data_t1);

        // Round-trip through in-memory Rbin and verify_hash.
        let store = StaStore::from_bytes(bytes_t1.clone()).expect("StaStore::from_bytes");
        store.verify_hash().expect("verify_hash");
        assert_eq!(store.store_mode().expect("store_mode"), StoreMode::Full);
        store
            .require_full_mode_for_incremental()
            .expect("full mode supports incremental");

        let round_tripped = store.to_store_data().expect("to_store_data");
        let bytes_rt = round_tripped.to_bytes().expect("round-trip to_bytes");
        assert_eq!(bytes_t1, bytes_rt, "{}: round-trip bytes mismatch", d.name);

        // File mmap round-trip on the first 4 netlists and LANES_K1.
        if count < 4 || d.name == LANES_K1.name {
            let path = dir.join(format!("{}.sta.rbin", d.name));
            data_t1.write_file(&path).expect("write_file");
            let mapped = StaStore::read_path(&path).expect("read_path");
            mapped.verify_hash().expect("mapped verify_hash");
            let verified = StaStore::open_verified_path(&path).expect("open_verified_path");
            assert_eq!(
                verified
                    .to_store_data()
                    .expect("verified to_store_data")
                    .to_bytes()
                    .expect("verified bytes"),
                bytes_t1
            );
        }

        // Mapped queries smoke check on every corpus netlist.
        let sum = store.query_summary().expect("query_summary");
        assert!(!sum.rows.is_empty(), "{}: summary rows empty", d.name);
        let scope = store.query_scope().expect("query_scope");
        assert_eq!(scope.corners, vec!["default".to_owned()]);
        let eps = store
            .query_endpoints(0, Some(CheckKind::Setup), 50)
            .expect("query_endpoints");
        assert!(eps.len() <= 50);

        count += 1;
    }
    assert_eq!(count, 32);

    // Also test generated netlists (tristate, latch, clock crossing, clock gating, constants).
    let wt15_lib = wt_15();
    let wt15_libs = std::slice::from_ref(&wt15_lib);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");
    let generated = [
        gen_tristate_bus(101, 4, 3),
        gen_latch_pipeline(303, 4),
        gen_clock_crossing(404, 4),
        gen_clock_gated(505, 4),
        gen_constant_tied(606, 4),
    ];
    for fix in &generated {
        let frozen = fix.to_gate_netlist().freeze();
        let b1 = pool1
            .install(|| build_store_for_frozen(&frozen, wt15_libs, &wt15_tlib, &opts))
            .to_bytes()
            .expect("b1");
        let b32 = pool32
            .install(|| build_store_for_frozen(&frozen, wt15_libs, &wt15_tlib, &opts))
            .to_bytes()
            .expect("b32");
        assert_eq!(b1, b32, "{}: thread determinism 1 vs 32", fix.name);
    }

    let _ = fs::remove_dir_all(&dir);
}

// STA-REQ-0312, STA-REQ-0313, STA-REQ-0314:
// Verify 100-path reconstruction from `PATH_CHAIN` + `NODE_REC` (with cache-line prefetch)
// matches materialized `PATHS` + `PATH_POINTS` to 1e-12 ns, and verify `query_path_to`,
// `inspect_pin`, `inspect_port`, and `inspect_instance`.
#[test]
fn path_reconstruction_100_paths_and_mapped_queries() {
    let wt15_lib = wt_15();
    let wt15_libs = std::slice::from_ref(&wt15_lib);
    let wt15_tlib = TimingLib::from_corner("default", wt15_libs).expect("TimingLib wt15");

    // Generate a clock-crossing design with > 100 ranked paths.
    let fix = gen_clock_crossing(777, 64);
    let frozen = fix.to_gate_netlist().freeze();
    let nv = NetlistView::new(&frozen, wt15_libs).expect("NetlistView");
    let graph = TimingGraph::build(&nv, wt15_libs).expect("TimingGraph");

    // Build store A with top_k_paths = 150 so ranks 0..100 are materialized in PATHS/PATH_POINTS.
    let opts_full_k150 = StaStoreBuildOptions {
        top_k_paths: 150,
        path_limit: 500,
        ..StaStoreBuildOptions::default()
    };
    let data_k150 =
        build_single_corner_store(&nv, &graph, &wt15_tlib, None, &opts_full_k150).expect("k150");
    let store_k150 =
        StaStore::from_bytes(data_k150.to_bytes().expect("bytes k150")).expect("store k150");

    // Build store B with top_k_paths = 5 so ranks 5..100 are reconstructed from PATH_CHAIN + NODE_REC.
    let opts_full_k5 = StaStoreBuildOptions {
        top_k_paths: 5,
        path_limit: 500,
        ..StaStoreBuildOptions::default()
    };
    let data_k5 =
        build_single_corner_store(&nv, &graph, &wt15_tlib, None, &opts_full_k5).expect("k5");
    let store_k5 = StaStore::from_bytes(data_k5.to_bytes().expect("bytes k5")).expect("store k5");

    let n_ranked = store_k150.view().expect("view").meta.num_ranked_paths as usize;
    assert!(
        n_ranked >= 100,
        "expected at least 100 ranked paths, got {n_ranked}"
    );

    for rank in 0..100 {
        let mat = store_k150.query_path_handle(rank).expect("k150 handle");
        let recon = store_k5
            .reconstruct_path_from_chain(rank)
            .expect("reconstruct_path_from_chain");
        let k5_via_handle = store_k5.query_path_handle(rank).expect("k5 handle");
        assert_eq!(k5_via_handle.points, recon.points);

        assert!(
            (mat.slack - recon.slack).abs() <= 1e-12,
            "rank {rank}: slack {} vs {}",
            mat.slack,
            recon.slack
        );
        assert!(
            (mat.arrival - recon.arrival).abs() <= 1e-12,
            "rank {rank}: arrival {} vs {}",
            mat.arrival,
            recon.arrival
        );
        assert!(
            (mat.required - recon.required).abs() <= 1e-12,
            "rank {rank}: required {} vs {}",
            mat.required,
            recon.required
        );
        assert_eq!(
            mat.points.len(),
            recon.points.len(),
            "rank {rank}: point count mismatch"
        );
        for (p_idx, (mp, rp)) in mat.points.iter().zip(&recon.points).enumerate() {
            assert_eq!(mp.node, rp.node, "rank {rank} pt {p_idx}: node");
            assert_eq!(mp.name, rp.name, "rank {rank} pt {p_idx}: name");
            assert_eq!(mp.tr, rp.tr, "rank {rank} pt {p_idx}: tr");
            assert_eq!(mp.kind, rp.kind, "rank {rank} pt {p_idx}: kind");
            assert!(
                (mp.arrival - rp.arrival).abs() <= 1e-12,
                "rank {rank} pt {p_idx}: arrival {} vs {}",
                mp.arrival,
                rp.arrival
            );
            assert!(
                (mp.incr_delay - rp.incr_delay).abs() <= 1e-12,
                "rank {rank} pt {p_idx}: incr_delay {} vs {}",
                mp.incr_delay,
                rp.incr_delay
            );
            assert!(
                (mp.slew - rp.slew).abs() <= 1e-12,
                "rank {rank} pt {p_idx}: slew {} vs {}",
                mp.slew,
                rp.slew
            );
        }
    }

    // Verify `endpoint_table`, `query_endpoints`, `query_path_to`, `inspect_pin`, `inspect_port`, and `inspect_instance`.
    let compact_eps = store_k150.endpoint_table(0).expect("endpoint_table");
    assert!(!compact_eps.is_empty());
    let eps = store_k150
        .query_endpoints(0, Some(CheckKind::Setup), 10)
        .expect("query_endpoints");
    assert!(!eps.is_empty());
    let first_ep_pin = &eps[0].name;
    let paths_to_ep = store_k150
        .query_path_to(first_ep_pin, Some(0), None)
        .expect("query_path_to");
    assert!(!paths_to_ep.is_empty());
    assert_eq!(paths_to_ep[0].endpoint_name, *first_ep_pin);

    let pin_res = store_k150
        .inspect_pin(first_ep_pin, 0)
        .expect("inspect_pin");
    assert_eq!(pin_res.name, *first_ep_pin);
    assert!(pin_res.is_endpoint);

    if let Some((inst_name, _)) = first_ep_pin.split_once('/') {
        let inst_res = store_k150
            .inspect_instance(inst_name, 0)
            .expect("inspect_instance");
        assert_eq!(inst_res.inst_name, inst_name);
        assert!(!inst_res.pins.is_empty());
    }

    let first_port = nv.node_name_str(NodeId(0)).into_owned();
    let port_res = store_k150
        .inspect_port(&first_port, 0)
        .expect("inspect_port");
    assert!(port_res.is_port);
    assert_eq!(port_res.name, first_port);
}

// WS-REQ-0034, WS-REQ-0035, STA-REQ-0315:
// Verify `StoreMode::Full` vs `StoreMode::Summary` behavior and `SummaryModeUnavailable` (`INPUT_REJECTED`).
#[test]
fn store_mode_full_vs_summary_and_incremental_rejection() {
    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");
    let frozen = read_structural_frozen(LANES_K1.name, LANES_K1.text, full_libs).expect("lanes_k1");

    let full_opts = StaStoreBuildOptions {
        store_mode: StoreMode::Full,
        top_k_paths: 10,
        path_limit: 200,
        ..StaStoreBuildOptions::default()
    };
    let sum_opts = StaStoreBuildOptions {
        store_mode: StoreMode::Summary,
        top_k_paths: 10,
        path_limit: 200,
        ..StaStoreBuildOptions::default()
    };

    let full_data = build_store_for_frozen(&frozen, full_libs, &full_tlib, &full_opts);
    let sum_data = build_store_for_frozen(&frozen, full_libs, &full_tlib, &sum_opts);

    assert!(sum_data.node_rec.is_empty());
    assert!(sum_data.tag_extra_ptr.is_empty());
    assert!(sum_data.tag_extra.is_empty());
    assert_eq!(full_data.clone().into_summary_mode(), sum_data);

    let full_bytes = full_data.to_bytes().expect("full bytes");
    let sum_bytes = sum_data.to_bytes().expect("sum bytes");
    assert!(
        sum_bytes.len() < full_bytes.len(),
        "summary bytes ({}) should be smaller than full bytes ({})",
        sum_bytes.len(),
        full_bytes.len()
    );

    let full_store = StaStore::from_bytes(full_bytes).expect("full store");
    let sum_store = StaStore::from_bytes(sum_bytes).expect("sum store");
    assert_eq!(sum_store.store_mode().expect("mode"), StoreMode::Summary);

    // Summary, endpoints, scope, and top-K paths are identical between Full and Summary modes.
    assert_eq!(
        full_store.query_summary().expect("full sum").rows,
        sum_store.query_summary().expect("sum sum").rows
    );
    assert_eq!(
        full_store.endpoint_table(0).expect("full ep"),
        sum_store.endpoint_table(0).expect("sum ep")
    );
    assert_eq!(
        full_store.query_scope().expect("full scope").completeness,
        sum_store.query_scope().expect("sum scope").completeness
    );

    let k_mat = sum_store.view().expect("view").meta.num_top_k_paths as usize;
    for r in 0..k_mat {
        assert_eq!(
            full_store.query_path_handle(r).expect("full path"),
            sum_store.query_path_handle(r).expect("sum path")
        );
    }

    // Incremental seeding and `inspect_instance` on Summary mode return SummaryModeUnavailable (InputRejected).
    let inc_err = sum_store
        .require_full_mode_for_incremental()
        .expect_err("summary mode must reject incremental seeding");
    assert_eq!(inc_err.error_code(), ErrorCode::InputRejected);

    let eps = sum_store
        .query_endpoints(0, Some(CheckKind::Setup), 1)
        .expect("eps");
    if let Some(ep) = eps.first()
        && let Some((inst_name, _)) = ep.name.split_once('/')
    {
        let inst_err = sum_store
            .inspect_instance(inst_name, 0)
            .expect_err("summary mode must reject inspect_instance");
        assert_eq!(inst_err.error_code(), ErrorCode::InputRejected);
    }
}

// WS-REQ-0031, WS-REQ-0036:
// Verify `CORRUPT_TABLE` on truncated, bad-magic, bad-section, and bad-meta files,
// `SCHEMA_TOO_NEW` on newer schema versions, and O(1) structure-only open vs `verify_hash()`
// on same-size payload byte flips in `NODE_REC` and `SUMMARY`.
#[test]
fn corrupt_table_and_verify_hash_detection() {
    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");
    let frozen =
        read_structural_frozen(CORPUS[0].name, CORPUS[0].text, full_libs).expect("corpus[0]");
    let valid_data = build_store_for_frozen(
        &frozen,
        full_libs,
        &full_tlib,
        &StaStoreBuildOptions::default(),
    );
    let valid_bytes = valid_data.to_bytes().expect("valid bytes");

    // 1. Truncated files -> CORRUPT_TABLE.
    for len in [
        0usize,
        1,
        31,
        63,
        64,
        128,
        valid_bytes.len() / 2,
        valid_bytes.len() - 1,
    ] {
        let err = StaStore::from_bytes(valid_bytes[..len].to_vec()).expect_err("truncated");
        assert_eq!(
            err.error_code(),
            ErrorCode::CorruptTable,
            "len={len}: {err}"
        );
    }

    // 2. Bad magic -> CORRUPT_TABLE.
    {
        let mut bad = valid_bytes.clone();
        bad[0..8].copy_from_slice(b"BADMAGIC");
        let err = StaStore::from_bytes(bad).expect_err("bad magic");
        assert_eq!(err.error_code(), ErrorCode::CorruptTable);
    }

    // 3. Newer schema version -> SCHEMA_TOO_NEW.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION + 1).expect("writer");
        for id in 1..=section::COUNT as u32 {
            w.section_bytes(id, section::elem_size(id).unwrap(), &[])
                .expect("sec");
        }
        let err = StaStore::from_bytes(w.to_bytes()).expect_err("schema too new");
        assert_eq!(err.error_code(), ErrorCode::SchemaTooNew);
    }

    // 4. Missing section 24 -> CORRUPT_TABLE.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).expect("writer");
        for id in 1..section::COUNT as u32 {
            w.section_bytes(id, section::elem_size(id).unwrap(), &[])
                .expect("sec");
        }
        let err = StaStore::from_bytes(w.to_bytes()).expect_err("missing section");
        assert_eq!(err.error_code(), ErrorCode::CorruptTable);
    }

    // 5. Wrong section element size -> CORRUPT_TABLE.
    {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION).expect("writer");
        for id in 1..=section::COUNT as u32 {
            if id == section::META {
                w.section_bytes(id, 32, &[0u8; 64]).expect("sec");
            } else {
                w.section_bytes(id, section::elem_size(id).unwrap(), &[])
                    .expect("sec");
            }
        }
        let err = StaStore::from_bytes(w.to_bytes()).expect_err("bad elem_size");
        assert_eq!(err.error_code(), ErrorCode::CorruptTable);
    }

    // 6. Invalid store_mode in StaMeta -> CORRUPT_TABLE.
    {
        let mut bad_data = valid_data.clone();
        bad_data.meta[0].store_mode = 99;
        let bytes = bad_data.to_bytes().expect("encode bad store_mode");
        let err = StaStore::from_bytes(bytes).expect_err("bad store_mode");
        assert_eq!(err.error_code(), ErrorCode::CorruptTable);
    }

    // 7. Same-size payload byte flip in NODE_REC and SUMMARY:
    //    - O(1) structure-only open (`StaStore::from_bytes` / `StaStore::read_path`) succeeds
    //      without full-file hashing (`WS-REQ-0036`).
    //    - `verify_hash()` and `open_verified_path()` catch the flip with `CORRUPT_TABLE`.
    let dir = temp_dir("corrupt-flip");
    let store = StaStore::from_bytes(valid_bytes.clone()).expect("valid store");
    let node_rec_slice = store
        .rbin()
        .expect("a read table")
        .section_bytes(section::NODE_REC)
        .expect("NODE_REC bytes");
    let node_rec_offset = node_rec_slice.as_ptr() as usize
        - store.rbin().expect("a read table").as_bytes().as_ptr() as usize;
    let summary_slice = store
        .rbin()
        .expect("a read table")
        .section_bytes(section::SUMMARY)
        .expect("SUMMARY bytes");
    let summary_offset = summary_slice.as_ptr() as usize
        - store.rbin().expect("a read table").as_bytes().as_ptr() as usize;

    for (sec_name, offset) in [("NODE_REC", node_rec_offset), ("SUMMARY", summary_offset)] {
        let mut flipped = valid_bytes.clone();
        flipped[offset] ^= 0x01;
        let path = dir.join(format!("flipped_{sec_name}.sta.rbin"));
        fs::write(&path, &flipped).expect("write flipped");

        let unverified = StaStore::read_path(&path)
            .unwrap_or_else(|e| panic!("{sec_name}: structure-only open failed: {e}"));
        let hash_err = unverified
            .verify_hash()
            .expect_err("verify_hash must fail on payload byte flip");
        assert_eq!(hash_err.error_code(), ErrorCode::CorruptTable);

        let verified_err = StaStore::open_verified_path(&path)
            .expect_err("open_verified_path must fail on payload byte flip");
        assert_eq!(verified_err.error_code(), ErrorCode::CorruptTable);
    }

    let _ = fs::remove_dir_all(&dir);
}

// WS-REQ-0011, WS-REQ-0030, WS-REQ-0045:
// Verify workspace `Txn` staging (`stage_to_txn`), `summary.sta`, `status.sta`,
// `StaStore::open_rev`, `StaStore::open_head`, and `StaStore::open_for_mutation_input`.
#[test]
fn workspace_txn_staging_and_mapped_revision_open() {
    let dir = temp_dir("ws-sta");
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .expect("Workspace::init")
        .workspace;

    let full_lib = wt_full();
    let full_libs = std::slice::from_ref(&full_lib);
    let full_tlib = TimingLib::from_corner("default", full_libs).expect("TimingLib full");
    let frozen = read_structural_frozen(LANES_K1.name, LANES_K1.text, full_libs).expect("lanes_k1");
    let sta_data = build_store_for_frozen(
        &frozen,
        full_libs,
        &full_tlib,
        &StaStoreBuildOptions::default(),
    );

    let base0 = ws.resolve_base("rev0").expect("resolve rev0");
    let mut txn = ws.begin(base0, &inv("ruststa run")).expect("begin txn");
    let _netlist_hash =
        ws_netlist::add_to_txn(&mut txn, &frozen, Upstream::new()).expect("stage netlist");
    let upstream = txn.upstream(&["netlist"]).expect("upstream map");
    let sta_hash = sta_data
        .stage_to_txn(&mut txn, upstream.clone())
        .expect("stage_to_txn");
    let committed = txn.commit(BTreeMap::new()).expect("commit rev1");

    assert_eq!(committed.rev, Rev(1));
    assert_eq!(committed.meta.tables[TABLE_NAME].path, TABLE_PATH);
    assert_eq!(committed.meta.tables[TABLE_NAME].hash, sta_hash);

    let sta_sum = committed
        .meta
        .summary
        .get(TABLE_NAME)
        .expect("summary.sta populated");
    assert!(sta_sum.contains_key("setup_wns_ns"));
    assert!(sta_sum.contains_key("setup_tns_ns"));
    assert!(sta_sum.contains_key("hold_wns_ns"));
    assert!(sta_sum.contains_key("hold_tns_ns"));
    assert!(sta_sum.contains_key("total_endpoints"));

    let sta_status = committed
        .meta
        .status
        .get(TABLE_NAME)
        .expect("status.sta populated");
    assert!(sta_status.contains_key("timing"));

    // Open via `open_head` and `open_for_mutation_input`.
    let (head_rev, head_store) = StaStore::open_head(&ws).expect("open_head");
    assert_eq!(head_rev.rev(), Rev(1));
    assert_eq!(head_store.rbin().expect("a read table").upstream().len(), 1);
    assert_eq!(
        head_store.query_summary().expect("summary").rows.len(),
        sta_data.summary.len()
    );

    let verified_store =
        StaStore::open_for_mutation_input(&head_rev).expect("open_for_mutation_input");
    assert_eq!(
        verified_store
            .to_store_data()
            .expect("to_store_data")
            .to_bytes()
            .expect("bytes"),
        sta_data.to_bytes().expect("expected bytes")
    );

    let _ = fs::remove_dir_all(&dir);
}

fn build_synthetic_scale_store(n_cells: usize) -> StaStoreData {
    let n_ports = 2usize;
    let n_nodes = n_ports + 2 * n_cells;
    let n_endpoints = 10_000usize.min(n_cells);
    let top_k = 100usize;
    let path_limit = 10_000usize;

    let mut strings = StringTableBuilder::new();
    strings.reserve(n_ports + n_cells + 32, n_cells * 9 + 1024);
    let backend_str = strings.intern("cpu");
    let default_corner_str = strings.intern("default");
    let clk_str = strings.intern("clk");
    let overall_group_str = strings.intern("**");
    let inv_cell_str = strings.intern("INV_X1");
    let pin_a_str = strings.intern("A");
    let pin_y_str = strings.intern("Y");
    let port_in_str = strings.intern("in0");
    let port_out_str = strings.intern("out0");

    let mut name_hash = NameHashBuilder::with_capacity(n_nodes);
    let mut node_buckets = vec![0u32; n_nodes];
    node_buckets[0] = name_hash.insert(
        hash_node_name(None, "in0"),
        NodeId(0),
        NO_INDEX,
        port_in_str,
    );
    node_buckets[1] = name_hash.insert(
        hash_node_name(None, "out0"),
        NodeId(1),
        NO_INDEX,
        port_out_str,
    );

    let mut inst_hash = InstHashBuilder::with_capacity(n_cells);
    for k in 0..n_cells {
        let s = format!("u{k}");
        let inst_str = strings.append_raw(&s);
        let a_node = (n_ports + 2 * k) as u32;
        let y_node = a_node + 1;
        node_buckets[a_node as usize] = name_hash.insert(
            hash_node_name(Some(&s), "A"),
            NodeId(a_node),
            inst_str,
            pin_a_str,
        );
        node_buckets[y_node as usize] = name_hash.insert(
            hash_node_name(Some(&s), "Y"),
            NodeId(y_node),
            inst_str,
            pin_y_str,
        );
        inst_hash.insert(&s, inst_str, inv_cell_str, a_node, 2);
    }

    let node_rec: Vec<NodeRec> = (0..n_nodes)
        .map(|v| NodeRec {
            slew: [[0.02, 0.03]; 2],
            required: [[0.35, 0.90]; 2],
            slack: [[0.15, 0.10]; 2],
            tag: 0,
            pad: node_buckets[v],
            arrival: [[0.50, 0.80]; 2],
            pred: [[if v > 0 { (v - 1) as u32 } else { NO_PRED }; 2]; 2],
        })
        .collect();

    let tag_extra_ptr = vec![0u32; n_nodes + 1];

    let endpoints: Vec<EndpointRec> = (0..n_endpoints)
        .map(|i| {
            let y_node = (n_ports + 2 * i + 1) as u32;
            EndpointRec {
                data_node: y_node,
                clock_node: 0,
                capture_clock: 0,
                path_group_str: clk_str,
                check_arc_idx: i as u32,
                kind_flags: EndpointRec::pack_kind_flags(EndpointKindTag::FlipFlop, Some(Tr::Rise)),
                worst_setup_rank: i as u32,
                worst_hold_rank: NO_INDEX,
                name_bucket_plus1: EndpointRec::pack_name_bucket(Some(
                    node_buckets[y_node as usize],
                )),
            }
        })
        .collect();

    let ep_slack: Vec<EpSlackRec> = (0..n_endpoints)
        .map(|i| EpSlackRec {
            setup_slack: -0.50 + (i as f64) * 0.001,
            hold_slack: 0.10 + (i as f64) * 0.001,
        })
        .collect();

    let mut ep_order = Vec::with_capacity(2 * n_endpoints);
    for _ in 0..2 {
        for i in 0..n_endpoints {
            ep_order.push(i as u32);
        }
    }

    let mut path_rank = Vec::with_capacity(path_limit);
    let mut path_chain_ptr = Vec::with_capacity(path_limit + 1);
    let mut path_chain = Vec::with_capacity(path_limit * 10);
    path_chain_ptr.push(0u64);
    for r in 0..path_limit {
        let ep_i = r % n_endpoints;
        let y_node = (n_ports + 2 * ep_i + 1) as u32;
        let a_node = y_node - 1;
        let slack = -0.50 + (r as f64) * 0.0001;
        let req = 0.80 + slack;
        path_rank.push(PathRankRec::new(
            slack as f32,
            ep_i,
            0,
            CheckKind::Setup,
            Tr::Rise,
            r < top_k,
        ));
        path_chain.extend_from_slice(&pack_f64_words(req));
        path_chain.extend_from_slice(&pack_f64_words(0.05));
        path_chain.extend_from_slice(&pack_chain_step(
            NodeId(0),
            Tr::Rise,
            PathPointKind::Port,
            0,
            node_buckets[0],
        ));
        path_chain.extend_from_slice(&pack_chain_step(
            NodeId(a_node),
            Tr::Rise,
            PathPointKind::Net,
            0,
            node_buckets[a_node as usize],
        ));
        path_chain.extend_from_slice(&pack_chain_step(
            NodeId(y_node),
            Tr::Rise,
            PathPointKind::Cell,
            0,
            node_buckets[y_node as usize],
        ));
        path_chain_ptr.push(path_chain.len() as u64);
    }

    let mut paths = Vec::with_capacity(top_k);
    let mut path_points = Vec::with_capacity(top_k * 3);
    for r in 0..top_k {
        let ep_i = r % n_endpoints;
        let y_node = (n_ports + 2 * ep_i + 1) as u32;
        let a_node = y_node - 1;
        let slack = -0.50 + (r as f64) * 0.0001;
        let req = 0.80 + slack;
        let p_begin = path_points.len() as u32;
        path_points.push(PathPointRec {
            arrival: 0.50,
            incr_delay: 0.50,
            slew: 0.02,
            node: 0,
            tr: 0,
            kind: PathPointKind::Port as u8,
            pad: 0,
        });
        path_points.push(PathPointRec {
            arrival: 0.50,
            incr_delay: 0.0,
            slew: 0.02,
            node: a_node,
            tr: 0,
            kind: PathPointKind::Net as u8,
            pad: 0,
        });
        path_points.push(PathPointRec {
            arrival: 0.80,
            incr_delay: 0.30,
            slew: 0.03,
            node: y_node,
            tr: 0,
            kind: PathPointKind::Cell as u8,
            pad: 0,
        });
        let p_end = path_points.len() as u32;
        paths.push(PathHeaderRec {
            slack,
            arrival: 0.80,
            required: req,
            check_value: 0.05,
            rank_idx: r as u32,
            endpoint_idx: ep_i as u32,
            launch_clock: 0,
            capture_clock: 0,
            point_begin: p_begin,
            point_end: p_end,
        });
    }

    let summary = vec![SummaryRec {
        setup_wns: -0.50,
        setup_tns: -125.0,
        hold_wns: 0.10,
        hold_tns: 0.0,
        corner_idx: 0,
        group_str: overall_group_str,
        setup_violating: 500,
        setup_endpoints: n_endpoints as u32,
        hold_violating: 0,
        hold_endpoints: n_endpoints as u32,
        max_trans_violating: 0,
        max_cap_violating: 0,
        total_endpoints: n_endpoints as u32,
        unconstrained_endpoints: 0,
        reserved0: 0,
        reserved1: 0,
    }];

    let scope = ScopeReport {
        top: String::from("top"),
        corners: vec![String::from("default")],
        wire_model: String::from("ideal"),
        clock_model: String::from("ideal"),
        cppr: String::from("off"),
        backend: String::from("cpu"),
        precision: StorePrecision::F64,
        store_mode: StoreMode::Full,
        exceptions_applied: Vec::new(),
        completeness: CompletenessReport {
            analyzed_endpoints: n_endpoints,
            unconstrained_endpoints: 0,
            unconstrained_by_reason: BTreeMap::new(),
            disabled_arcs_by_reason: BTreeMap::new(),
            unanalyzed_arcs_by_type: BTreeMap::new(),
        },
    };
    let scope_bytes = serde_json::to_vec(&scope).expect("scope json");
    let (str_bytes, str_offs) = strings.finish();

    let mut data = StaStoreData {
        str_bytes,
        str_offs,
        meta: [StaMeta {
            num_nodes: n_nodes as u32,
            num_port_bits: n_ports as u32,
            num_arcs: (2 * n_cells) as u32,
            num_cells: n_cells as u32,
            num_startpoints: 1,
            num_disabled_arcs: 0,
            num_corners: 1,
            num_clocks: 1,
            num_tags: 1,
            num_endpoints: n_endpoints as u32,
            num_ranked_paths: path_limit as u32,
            num_top_k_paths: top_k as u32,
            num_path_groups: 1,
            store_mode: StoreMode::Full as u32,
            backend_str,
            precision: StorePrecision::F64 as u32,
        }],
        corners: vec![CornerRec {
            name_str: default_corner_str,
            lib_count: 1,
            flags: 0,
            reserved: 0,
        }],
        clocks: vec![ClockRec {
            name_str: clk_str,
            source_node: 0,
            master_clock: NO_INDEX,
            flags: 0,
            period_ns: 1.0,
            rise_edge_ns: 0.0,
            fall_edge_ns: 0.5,
            uncertainty_setup_ns: 0.0,
            uncertainty_hold_ns: 0.0,
        }],
        tags: vec![TagRec {
            clock_id: 0,
            launch_edge: 0,
            flags: 0,
            exc_state: 0,
            launch_source_node: 0,
            path_group_str: clk_str,
        }],
        disabled_arcs: Vec::new(),
        node_rec,
        tag_extra_ptr,
        tag_extra: Vec::new(),
        endpoints,
        ep_slack,
        ep_order,
        path_rank,
        path_chain_ptr,
        path_chain,
        paths,
        path_points,
        name_hash: name_hash.finish(),
        inst_hash: inst_hash.finish(),
        summary,
        unconstrained: Vec::new(),
        notes: Vec::<NoteRec>::new(),
        scope: scope_bytes,
    };
    data.canonicalize();
    data
}

// WS-REQ-0030..0038:
// Ignored scale benchmark at 500k and 1M cells verifying mapped query latencies are <= 25 ms warm.
#[test]
#[ignore = "500k and 1M-cell scale benchmark; run with --release --test store -- --ignored scale"]
fn scale_500k_and_1m_mapped_queries_under_25ms() {
    let dir = temp_dir("scale");

    for &n_cells in &[500_000usize, 1_000_000usize] {
        let t_build = Instant::now();
        let data = build_synthetic_scale_store(n_cells);
        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;

        let path = dir.join(format!("sta_{n_cells}.rbin"));
        let t_write = Instant::now();
        data.write_file(&path).expect("write scale sta.rbin");
        let write_ms = t_write.elapsed().as_secs_f64() * 1000.0;
        let file_mib = fs::metadata(&path).expect("metadata").len() as f64 / (1024.0 * 1024.0);

        // Open mapped revision / file once and measure warm mapped queries.
        let store = StaStore::read_path(&path).expect("read_path");

        // 1. query_summary.
        let t_sum = Instant::now();
        let sum = store.query_summary().expect("query_summary");
        let sum_ms = t_sum.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(sum.rows.len(), 1);
        assert!(
            sum_ms <= 25.0,
            "{n_cells} cells: query_summary took {sum_ms:.2} ms (> 25 ms)"
        );

        // 2. query_endpoints(limit=100).
        let t_ep = Instant::now();
        let eps = store
            .query_endpoints(0, Some(CheckKind::Setup), 100)
            .expect("query_endpoints");
        let ep_ms = t_ep.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(eps.len(), 100);
        assert!(
            ep_ms <= 25.0,
            "{n_cells} cells: query_endpoints(100) took {ep_ms:.2} ms (> 25 ms)"
        );

        // 3. Materialized path handle (rank 0) + chain reconstruction (rank 500).
        let t_path = Instant::now();
        let _p0 = store.query_path_handle(0).expect("rank 0");
        let p500 = store.reconstruct_path_from_chain(500).expect("rank 500");
        let path_ms = t_path.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(p500.points.len(), 3);
        assert!(
            path_ms <= 25.0,
            "{n_cells} cells: path query + chain reconstruct took {path_ms:.2} ms (> 25 ms)"
        );

        // 4. inspect_pin + inspect_instance via NAME_HASH and INST_HASH.
        let target_inst = format!("u{}", n_cells / 2);
        let target_pin = format!("{target_inst}/Y");
        let t_insp = Instant::now();
        let pin = store.inspect_pin(&target_pin, 0).expect("inspect_pin");
        let inst = store
            .inspect_instance(&target_inst, 0)
            .expect("inspect_instance");
        let insp_ms = t_insp.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(pin.name, target_pin);
        assert_eq!(inst.pins.len(), 2);
        assert!(
            insp_ms <= 25.0,
            "{n_cells} cells: inspect_pin + inspect_instance took {insp_ms:.2} ms (> 25 ms)"
        );

        eprintln!(
            "SCALE {n_cells} cells ({file_mib:.1} MiB): build={build_ms:.1}ms, write={write_ms:.1}ms, sum={sum_ms:.3}ms, ep100={ep_ms:.3}ms, path={path_ms:.3}ms, inspect={insp_ms:.3}ms"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}
