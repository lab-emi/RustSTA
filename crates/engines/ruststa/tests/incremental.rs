// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for `ruststa::engine::incr` (`TimingSession`, cone propagation,
//! 1,000-mutation randomized bitwise verification, `ddmin` shrinking, and cross-revision
//! `update_revision`; `owner: E11`; `STA-REQ-0450` to `STA-REQ-0458`).

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusteda_core::liberty::Library;
use rusteda_core::netlist::{CellId, Conn, GateNetlist, InstId, NetId};
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc::Sdc;
use rusteda_workspace::netlist as ws_netlist;
use rusteda_workspace::{
    Invocation, LibraryEntry, RbinWriter, Rev, Upstream, Workspace, rlib as ws_rlib,
};
use ruststa::engine::analysis::AnalysisOptions;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::incr::{
    FullUpdateReason, SessionEdit, SessionError, TimingSession, UpdateMode, UpdateOptions,
    shrink_failing_edits, update_revision,
};
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::store::layout::StoreMode;
use ruststa::engine::store::{StaStore, StaStoreBuildOptions, build_single_corner_store};
use ruststa::engine::types::{An, Tr};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::LANES_K1;
use support::legacy_import::read_structural_netlist;
use support::mutate::{
    LibCatalog, Rng, Shape, base_sdc, build_netlist, cell_in_pin, net_sink_pins, parse_sdc,
    pick_random_swap, run_random_mutation_campaign,
};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("ruststa-incr-{label}-{}-{id}", std::process::id()));
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

fn wt_rich() -> Library {
    read_text(&library(Variant::Rich), Variant::Rich.name()).expect("wt_rich library")
}

fn new_session(netlist: GateNetlist, lib: Library, sdc: Sdc) -> TimingSession {
    TimingSession::from_sdc(netlist, vec![lib], sdc, AnalysisOptions::default()).expect("session")
}

fn check_full(s: &TimingSession) {
    s.verify_against_full().expect("bitwise match full STA");
}

fn upd_opts(verify_full: bool, force_full: bool, max_dirty_fraction: f64) -> UpdateOptions {
    UpdateOptions {
        verify_full,
        force_full,
        max_dirty_fraction,
        ..UpdateOptions::default()
    }
}

fn ed_conn(inst: InstId, pin: &str, net: NetId) -> SessionEdit {
    SessionEdit::ConnectPin {
        inst,
        pin_name: pin.to_owned(),
        net,
    }
}

fn ed_disc(inst: InstId, pin: &str) -> SessionEdit {
    SessionEdit::DisconnectPin {
        inst,
        pin_name: pin.to_owned(),
    }
}

fn ed_move(inst: InstId, pin: &str, new_net: NetId) -> SessionEdit {
    SessionEdit::MovePin {
        inst,
        pin_name: pin.to_owned(),
        new_net,
    }
}

fn ed_swap(inst: InstId, new_cell: CellId) -> SessionEdit {
    SessionEdit::SwapCell { inst, new_cell }
}

fn build_catalog_netlist(
    lib: &Library,
    seed: u64,
    dims: (usize, usize, usize, usize),
) -> (GateNetlist, Sdc) {
    let shape = Shape::of(dims);
    (
        build_netlist(lib, seed, &shape),
        parse_sdc(&base_sdc(&shape)),
    )
}

// STA-REQ-0450, STA-REQ-0451, STA-REQ-0453:
// Verify all 6 edit categories (`SwapCell`, `InsertBuffer`, `RemoveBuffer`, `ConnectPin`/`DisconnectPin`/`MovePin`,
// `AddInstance`/`RemoveInstance`/`AddNet`/`RemoveNet`, and batch `apply_edits`) match full STA bitwise (0 ULP).
#[test]
fn single_edit_categories_and_batch_bitwise() {
    let lib = wt_rich();
    let (netlist, sdc) = build_catalog_netlist(&lib, 42, (4, 4, 12, 4));
    let mut session = new_session(netlist, lib, sdc);
    check_full(&session);

    let inv_x1 = session.netlist().find_cell("wt_inv_x1").unwrap();
    let inv_x2 = session.netlist().find_cell("wt_inv_x2").unwrap();
    let buf_x1 = session.netlist().find_cell("wt_buf_x1").unwrap();
    let nand2_x1 = session.netlist().find_cell("wt_nand2_x1").unwrap();
    let nand2_x2 = session.netlist().find_cell("wt_nand2_x2").unwrap();

    let drv0 = session.netlist().find_instance("u_out_drv_0").unwrap();
    session.swap_cell(drv0, inv_x2).expect("swap 1");
    check_full(&session);
    session.swap_cell(drv0, buf_x1).expect("swap 2");
    check_full(&session);

    let in0_net = session.netlist().ports()[1].bits[0];
    let in0_sinks = net_sink_pins(session.netlist(), in0_net);
    let b_net_name = Some(String::from("n_buf_ins_0"));
    let (buf_inst, buf_net, _) = session
        .insert_buffer(
            in0_net,
            &in0_sinks[..1],
            "u_buf_ins_0",
            buf_x1,
            "A",
            "Y",
            b_net_name,
        )
        .expect("insert_buffer");
    check_full(&session);

    session
        .remove_buffer(buf_inst, "A", "Y")
        .expect("remove_buffer");
    assert!(session.netlist().instance(buf_inst).is_err());
    assert!(session.netlist().net(buf_net).is_err());
    check_full(&session);

    let in1_net = session.netlist().ports()[2].bits[0];
    let sink_inst = session.netlist().pin(in0_sinks[0]).unwrap().inst;
    let sink_pin_name = cell_in_pin(
        session.netlist(),
        session.netlist().instance(sink_inst).unwrap().cell,
    );

    session
        .move_pin(sink_inst, &sink_pin_name, in1_net)
        .expect("move_pin in1");
    check_full(&session);
    session
        .disconnect_pin(sink_inst, &sink_pin_name)
        .expect("disconnect_pin");
    check_full(&session);
    session
        .connect_pin(sink_inst, &sink_pin_name, in0_net)
        .expect("connect_pin in0");
    check_full(&session);

    let (new_net, _) = session
        .add_net(Some(String::from("n_added_gate")))
        .expect("add_net");
    let (new_inst, _) = session
        .add_instance("u_added_nand", nand2_x1)
        .expect("add_instance");
    session
        .apply_edits(&[
            ed_conn(new_inst, "A", in0_net),
            ed_conn(new_inst, "B", in1_net),
            ed_conn(new_inst, "Y", new_net),
            ed_move(sink_inst, &sink_pin_name, new_net),
        ])
        .expect("wire new gate batch");
    check_full(&session);

    session.swap_cell(new_inst, nand2_x2).expect("swap added");
    check_full(&session);

    session
        .apply_edits(&[
            ed_move(sink_inst, &sink_pin_name, in0_net),
            SessionEdit::RemoveInstance { inst: new_inst },
            SessionEdit::RemoveNet { net: new_net },
            ed_swap(drv0, inv_x1),
        ])
        .expect("remove gate batch");
    check_full(&session);
    assert_eq!(session.apply_edits(&[]).unwrap().dirty_instances, 0);
}

// STA-REQ-0452 (Hazard 1):
// Arena slot reuse: `remove_buffer` followed by `insert_buffer` reuses freed `InstId`, `NetId`,
// and `PinId` slots with incremented generations.
#[test]
fn hazard_1_arena_slot_reuse_pin_generation() {
    let lib = wt_rich();
    let (netlist, sdc) = build_catalog_netlist(&lib, 101, (4, 4, 10, 4));
    let mut session = new_session(netlist, lib, sdc);
    let buf_x1 = session.netlist().find_cell("wt_buf_x1").unwrap();
    let inv_x1 = session.netlist().find_cell("wt_inv_x1").unwrap();
    let inv_x2 = session.netlist().find_cell("wt_inv_x2").unwrap();
    let in0_net = session.netlist().ports()[1].bits[0];

    let mut prev_inst_idx = None;
    let mut prev_net_idx = None;
    for (cycle, &cell) in [buf_x1, inv_x1, inv_x2, buf_x1, inv_x2].iter().enumerate() {
        let sinks = net_sink_pins(session.netlist(), in0_net);
        let (b_name, n_name) = (
            format!("u_slot_reuse_buf_{cycle}"),
            Some(format!("n_slot_reuse_{cycle}")),
        );
        let (b_inst, b_net, _) = session
            .insert_buffer(in0_net, &sinks, b_name, cell, "A", "Y", n_name)
            .expect("insert_buffer");

        if let Some(p_idx) = prev_inst_idx {
            assert_eq!(b_inst.index(), p_idx, "expected InstId slot reuse");
        }
        if let Some(n_idx) = prev_net_idx {
            assert_eq!(b_net.index(), n_idx, "expected NetId slot reuse");
        }
        prev_inst_idx = Some(b_inst.index());
        prev_net_idx = Some(b_net.index());

        check_full(&session);
        session
            .remove_buffer(b_inst, "A", "Y")
            .expect("remove_buffer");
        check_full(&session);
    }
}

// STA-REQ-0452 (Hazard 2):
// Pin connection order independence: `disconnect_pin` + `connect_pin` changes the order of pins in
// `inst.connected`. Verify `TimingSession` matches full STA bitwise.
#[test]
fn hazard_2_pin_connection_order_independence() {
    let lib = wt_rich();
    let (mut netlist, sdc) = build_catalog_netlist(&lib, 202, (4, 4, 10, 4));

    let nand3_ct = netlist.find_cell("wt_nand3_x1").unwrap();
    let n_a = netlist.ports()[1].bits[0];
    let n_b = netlist.ports()[2].bits[0];
    let n_c = netlist.find_net("n_rq_0", None).unwrap();
    let n_y = netlist.add_net(Some("n_nand3_y"), None).unwrap();
    let u_nand3 = netlist.add_instance("u_order_nand3", nand3_ct).unwrap();
    for (p, n) in [("A", n_a), ("B", n_b), ("C", n_c), ("Y", n_y)] {
        netlist.connect(u_nand3, p, Conn::Net(n)).unwrap();
    }

    let drv0 = netlist.find_instance("u_out_drv_0").unwrap();
    let drv0_a = netlist.find_pin(drv0, "A").unwrap();
    netlist.disconnect(drv0_a).unwrap();
    netlist.connect(drv0, "A", Conn::Net(n_y)).unwrap();

    let mut session = new_session(netlist, lib, sdc);
    check_full(&session);

    session.disconnect_pin(u_nand3, "A").expect("disconnect A");
    session.connect_pin(u_nand3, "A", n_a).expect("reconnect A");
    check_full(&session);

    session
        .apply_edits(&[
            ed_disc(u_nand3, "B"),
            ed_conn(u_nand3, "B", n_c),
            ed_move(u_nand3, "A", n_b),
        ])
        .expect("reorder B and move A");
    check_full(&session);
}

// STA-REQ-0453 (Hazard 3):
// Load-change driver sensitivity: changing downstream load capacitance on the upstream net
// updates the upstream driver's delay and output slew bitwise.
#[test]
fn hazard_3_downstream_load_change_updates_upstream_driver() {
    let lib = wt_full();
    let (mut netlist, sdc) = build_catalog_netlist(&lib, 303, (4, 4, 8, 4));

    let inv1 = netlist.find_cell("wtf_inv_1").unwrap();
    let inv4 = netlist.find_cell("wtf_inv_4").unwrap();
    let n_in0 = netlist.ports()[1].bits[0];
    let n_shared = netlist.add_net(Some("n_shared_load"), None).unwrap();
    let n_down_y = netlist.add_net(Some("n_down_y"), None).unwrap();

    let u_up = netlist.add_instance("u_upstream_drv", inv1).unwrap();
    netlist.connect(u_up, "A", Conn::Net(n_in0)).unwrap();
    netlist.connect(u_up, "Y", Conn::Net(n_shared)).unwrap();

    let u_down = netlist.add_instance("u_downstream_load", inv1).unwrap();
    netlist.connect(u_down, "A", Conn::Net(n_shared)).unwrap();
    netlist.connect(u_down, "Y", Conn::Net(n_down_y)).unwrap();

    let drv0 = netlist.find_instance("u_out_drv_0").unwrap();
    let drv0_in = cell_in_pin(&netlist, netlist.instance(drv0).unwrap().cell);
    let drv0_in_pid = netlist.find_pin(drv0, drv0_in.as_str()).unwrap();
    netlist.disconnect(drv0_in_pid).unwrap();
    netlist
        .connect(drv0, drv0_in.as_str(), Conn::Net(n_down_y))
        .unwrap();

    let mut session = new_session(netlist, lib, sdc);
    let up_y_node = session.find_node("u_upstream_drv/Y").unwrap();
    let slew_before = session.pin_slew(up_y_node, Tr::Rise, An::Late);
    let arr_before = session.default_corner().node_values.arrival(
        up_y_node.index(),
        0,
        Tr::Rise.index(),
        An::Late.index(),
    );

    session.swap_cell(u_down, inv4).expect("swap downstream");
    check_full(&session);

    let (extra_load, _) = session.add_instance("u_extra_fanout_load", inv4).unwrap();
    let (extra_y, _) = session.add_net(Some(String::from("n_extra_y"))).unwrap();
    session
        .apply_edits(&[
            ed_conn(extra_load, "A", n_shared),
            ed_conn(extra_load, "Y", extra_y),
        ])
        .expect("connect extra load");

    assert_ne!(
        slew_before.to_bits(),
        session.pin_slew(up_y_node, Tr::Rise, An::Late).to_bits()
    );
    let arr_after = session.default_corner().node_values.arrival(
        up_y_node.index(),
        0,
        Tr::Rise.index(),
        An::Late.index(),
    );
    assert_ne!(arr_before.to_bits(), arr_after.to_bits());
    check_full(&session);
}

// STA-REQ-0451, STA-REQ-0458:
// Verify error handling on invalid edits and `shrink_failing_edits` (`ddmin` delta-debugging).
#[test]
fn error_handling_and_ddmin_shrink_unit() {
    let lib = wt_rich();
    let (netlist, sdc) = build_catalog_netlist(&lib, 404, (4, 4, 16, 4));

    let mut session = new_session(netlist.clone(), lib, sdc);
    let drv0 = session.netlist().find_instance("u_out_drv_0").unwrap();
    let nand2_x1 = session.netlist().find_cell("wt_nand2_x1").unwrap();
    let buf_x1 = session.netlist().find_cell("wt_buf_x1").unwrap();
    let in0_net = session.netlist().ports()[1].bits[0];

    let err = session
        .swap_cell(drv0, nand2_x1)
        .expect_err("incompatible swap");
    assert!(matches!(err, SessionError::Netlist(_)));
    check_full(&session);

    let err = session
        .insert_buffer(in0_net, &[], "u_bad_buf", buf_x1, "Y", "A", None)
        .expect_err("swapped pin directions");
    assert!(matches!(err, SessionError::Netlist(_)));
    check_full(&session);

    let inv_x2 = netlist.find_cell("wt_inv_x2").unwrap();
    let unary_insts: Vec<InstId> = netlist
        .instances()
        .filter_map(|(id, inst)| (netlist.cell(inst.cell).ok()?.pins.len() == 2).then_some(id))
        .collect();
    let mut edits: Vec<SessionEdit> = (0..20)
        .map(|i| {
            ed_swap(
                unary_insts[i % unary_insts.len()],
                if i % 2 == 0 { inv_x2 } else { buf_x1 },
            )
        })
        .collect();
    let target_a = SessionEdit::AddNet {
        name: Some(String::from("n_trigger_bug_a")),
    };
    let target_b = SessionEdit::AddNet {
        name: Some(String::from("n_trigger_bug_b")),
    };
    edits[3] = target_a.clone();
    edits[14] = target_b.clone();

    let minimal = shrink_failing_edits(&edits, |subset| {
        subset.contains(&target_a) && subset.contains(&target_b)
    });
    assert_eq!(minimal, vec![target_a, target_b]);
}

// STA-REQ-0450..0453, STA-REQ-0458:
// 1,000-step randomized mutation bitwise test on both `wt_full` and `wt_rich` synthetic libraries,
// plus `LANES_K1` corpus netlist.
#[test]
fn random_1000_mutations_wt_full_and_wt_rich_bitwise() {
    let full_lib = wt_full();
    let full_pair = build_catalog_netlist(&full_lib, 0x0E11_F011, (6, 8, 24, 6));
    run_random_mutation_campaign("wt_full", full_pair, &full_lib, (0x1000_F011, 1_000, 25));

    let rich_lib = wt_rich();
    let rich_pair = build_catalog_netlist(&rich_lib, 0x0E11_81C4, (6, 8, 24, 6));
    run_random_mutation_campaign("wt_rich", rich_pair, &rich_lib, (0x1000_81C4, 1_000, 25));

    let full_slice = std::slice::from_ref(&full_lib);
    let mut lanes = read_structural_netlist(LANES_K1.name, LANES_K1.text, full_slice).unwrap();
    LibCatalog::from_library(&full_lib).register_in_netlist(&mut lanes, &full_lib);
    let lanes_sdc = parse_sdc("create_clock -name clk -period 5.0 [get_ports clk]\n");
    run_random_mutation_campaign(
        "lanes",
        (lanes, lanes_sdc),
        &full_lib,
        (0x1A4E_5001, 200, 20),
    );
}

fn commit_netlist_rev(
    ws: &Workspace,
    base: Rev,
    netlist: &GateNetlist,
    libs: Option<Vec<LibraryEntry>>,
    sdc: Option<&Sdc>,
    cmd: &str,
) -> Rev {
    let frozen = netlist.clone().freeze();
    let base_rev = ws.resolve_base(&base.to_string()).expect("resolve_base");
    let mut txn = ws.begin(base_rev, &inv(cmd)).expect("ws.begin");
    ws_netlist::add_to_txn(&mut txn, &frozen, Upstream::new()).expect("stage netlist");
    if let Some(lib_entries) = libs {
        txn.set_libraries(lib_entries).expect("set_libraries");
    }
    if let Some(sdc_ast) = sdc {
        let secs = sdc_ast.to_sections();
        let mut w = RbinWriter::new("CONSTR", 1).expect("CONSTR writer");
        for sec in &secs {
            w.section_bytes(u32::from(sec.id), sec.elem_size, &sec.bytes)
                .expect("stage CONSTR section");
        }
        txn.add_rbin("constraints", "constraints.rbin", &w, Upstream::new())
            .expect("add constraints.rbin");
    }
    txn.commit(BTreeMap::new()).expect("txn.commit").rev
}

fn commit_netlist(ws: &Workspace, base: Rev, netlist: &GateNetlist, cmd: &str) -> Rev {
    commit_netlist_rev(ws, base, netlist, None, None, cmd)
}

fn commit_with_libs(
    ws: &Workspace,
    b: Rev,
    g: &GateNetlist,
    l: Vec<LibraryEntry>,
    s: Option<&Sdc>,
) -> Rev {
    commit_netlist_rev(ws, b, g, Some(l), s, "set libs")
}

fn init_ws_with_rich(label: &str) -> (PathBuf, Workspace, Library, LibraryEntry) {
    let dir = temp_dir(label);
    let ws = Workspace::init(&dir, &inv("rusteda init"))
        .expect("Workspace::init")
        .workspace;
    let rich_text = library(Variant::Rich);
    let lib = read_text(&rich_text, Variant::Rich.name()).unwrap();
    let cached_rich = ws_rlib::put(&ws, &lib, rich_text.as_bytes()).expect("rlib::put rich");
    let rich_entry = cached_rich.entry("default", "wt_rich.lib");
    (dir, ws, lib, rich_entry)
}

// STA-REQ-0454, STA-REQ-0455, STA-REQ-0456, STA-REQ-0457:
// Verify cross-revision `update_revision` modes (`UpToDate`, `Incremental`, and all `FullUpdateReason` fallbacks).
#[test]
fn cross_revision_update_modes_and_fallbacks() {
    use FullUpdateReason::*;
    use UpdateMode::*;

    let (dir, ws, lib, rich_entry) = init_ws_with_rich("ws-update");
    let full_text = library(Variant::Full);
    let full_lib = read_text(&full_text, Variant::Full.name()).unwrap();
    let cached_full = ws_rlib::put(&ws, &full_lib, full_text.as_bytes()).expect("rlib::put full");
    let full_entry = cached_full.entry("default", "wt_full.lib");

    let (mut netlist, sdc_v1) = build_catalog_netlist(&lib, 707, (4, 4, 16, 4));
    let sdc_v2 = parse_sdc("create_clock -name clk -period 8.0 [get_ports clk]\n");
    let libs = std::slice::from_ref(&lib);
    let opts_verify = upd_opts(true, false, 0.25);

    // 1. Commit rev1 -> Full(NoAncestorSta); 2. Re-run -> UpToDate; 3. Forced -> Full(Forced).
    let l1 = vec![rich_entry.clone()];
    let rev1 = commit_with_libs(&ws, Rev(0), &netlist, l1, Some(&sdc_v1));
    let out1 = update_revision(&ws, Some(rev1), &opts_verify).expect("update_revision rev1");
    assert_eq!(out1.mode, Full(NoAncestorSta));
    let rev1_sta = out1.committed_rev;

    let out_uptodate = update_revision(&ws, Some(rev1_sta), &opts_verify).expect("uptodate");
    assert_eq!(
        (out_uptodate.mode, out_uptodate.committed_rev),
        (UpToDate, rev1_sta)
    );

    let out_forced =
        update_revision(&ws, Some(rev1_sta), &upd_opts(true, true, 0.25)).expect("forced");
    assert_eq!(out_forced.mode, Full(Forced));

    // 4. Mutate netlist -> Incremental (0 ULP); 5. Low threshold -> Full(DirtyFractionExceeded).
    let inv_x2 = netlist.find_cell("wt_inv_x2").unwrap();
    let drv0 = netlist.find_instance("u_out_drv_0").unwrap();
    netlist.swap_cell(drv0, inv_x2).unwrap();
    let rev_mut = commit_netlist(&ws, out_forced.committed_rev, &netlist, "opt swap drv0");
    let out_incr = update_revision(&ws, Some(rev_mut), &opts_verify).expect("incremental update");
    assert_eq!(
        (out_incr.mode, out_incr.ancestor_sta_rev),
        (Incremental, Some(out_forced.committed_rev))
    );
    assert!(out_incr.stats.dirty_instances > 0);

    let buf_x1 = netlist.find_cell("wt_buf_x1").unwrap();
    netlist.swap_cell(drv0, buf_x1).unwrap();
    let rev_thresh = commit_netlist(&ws, out_incr.committed_rev, &netlist, "opt swap 2");
    let out_thresh = update_revision(&ws, Some(rev_thresh), &upd_opts(true, false, 0.0001))
        .expect("threshold fallback");
    assert_eq!(out_thresh.mode, Full(DirtyFractionExceeded));

    // 6. ConstraintsChanged; 7. LibrariesChanged.
    let r_t = out_thresh.committed_rev;
    let rev_sdc = commit_netlist_rev(&ws, r_t, &netlist, None, Some(&sdc_v2), "sdc");
    let out_sdc = update_revision(&ws, Some(rev_sdc), &opts_verify).expect("sdc fallback");
    assert_eq!(out_sdc.mode, Full(ConstraintsChanged));

    let inv_x1 = netlist.find_cell("wt_inv_x1").unwrap();
    netlist.swap_cell(drv0, inv_x1).unwrap();
    let both_libs = vec![rich_entry.clone(), full_entry];
    let rev_lib = commit_with_libs(&ws, out_sdc.committed_rev, &netlist, both_libs, None);
    let out_lib = update_revision(&ws, Some(rev_lib), &opts_verify).expect("lib fallback");
    assert_eq!(out_lib.mode, Full(LibrariesChanged));

    // 8. AncestorSummaryOnly -> Full(AncestorSummaryOnly).
    let rev_single_lib =
        commit_with_libs(&ws, out_lib.committed_rev, &netlist, vec![rich_entry], None);
    let frozen_sum = netlist.clone().freeze();
    let nv_sum = NetlistView::new(&frozen_sum, libs).unwrap();
    let graph_sum = TimingGraph::build(&nv_sum, libs).unwrap();
    let tlib_sum = TimingLib::from_corner("default", libs).unwrap();
    let sum_opts = StaStoreBuildOptions {
        store_mode: StoreMode::Summary,
        ..StaStoreBuildOptions::default()
    };
    let sum_data =
        build_single_corner_store(&nv_sum, &graph_sum, &tlib_sum, None, &sum_opts).unwrap();
    let base_sum = ws.resolve_base(&rev_single_lib.to_string()).unwrap();
    let mut txn_sum = ws.begin(base_sum, &inv("ruststa summary")).unwrap();
    let mut upstream = txn_sum.upstream(&["netlist", "constraints"]).unwrap();
    upstream.insert(
        String::from("libraries"),
        ws.revision(rev_single_lib).unwrap().libraries_hash(),
    );
    sum_data.stage_to_txn(&mut txn_sum, upstream).unwrap();
    let rev_sum_sta = txn_sum.commit(BTreeMap::new()).unwrap().rev;

    netlist.swap_cell(drv0, inv_x2).unwrap();
    let rev_after_sum = commit_netlist(&ws, rev_sum_sta, &netlist, "after summary");
    let out_sum_fb =
        update_revision(&ws, Some(rev_after_sum), &opts_verify).expect("summary ancestor fallback");
    assert_eq!(out_sum_fb.mode, Full(AncestorSummaryOnly));
    let _ = fs::remove_dir_all(&dir);
}

// STA-REQ-0454..0457:
// 50-revision linear chain in a workspace, applying netlist mutations at every revision and verifying
// `UpdateMode::Incremental` and 0-ULP bitwise identity (`verify_full: true`) across all 50 steps.
#[test]
#[ignore = "50-step cross-revision incremental verification; run with --release --test incremental -- --ignored cross_revision"]
fn cross_revision_50_step_bitwise() {
    let (dir, ws, lib, rich_entry) = init_ws_with_rich("ws-50-step");
    let (mut netlist, sdc) = build_catalog_netlist(&lib, 0x0505_0E11, (6, 8, 28, 6));
    let mut rng = Rng::new(0x5050_BEEF);

    let swap_map = LibCatalog::from_library(&lib).build_swap_map(&netlist);
    let buf_ct = netlist.find_cell("wt_buf_x1").unwrap();
    let opts = upd_opts(true, false, 0.95);

    let rev_init = commit_with_libs(&ws, Rev(0), &netlist, vec![rich_entry], Some(&sdc));
    let out_init = update_revision(&ws, Some(rev_init), &opts).expect("initial update_revision");
    assert_eq!(
        out_init.mode,
        UpdateMode::Full(FullUpdateReason::NoAncestorSta)
    );
    let mut cur_rev = out_init.committed_rev;
    let mut inserted_bufs: Vec<InstId> = Vec::new();

    for step in 1..=50 {
        let roll = rng.gen_range(100);
        if roll < 65 || (roll >= 85 && inserted_bufs.is_empty()) {
            let (inst_id, new_ct) = pick_random_swap(&netlist, &swap_map, &mut rng);
            netlist.swap_cell(inst_id, new_ct).expect("swap_cell");
        } else if roll < 85 {
            let in_net = netlist.ports()[1 + (step % 4)].bits[0];
            let sinks = net_sink_pins(&netlist, in_net);
            if !sinks.is_empty() {
                let (b_inst, _) = netlist
                    .insert_buffer(
                        in_net,
                        &sinks[..1],
                        buf_ct,
                        "A",
                        "Y",
                        &format!("u_rev_buf_{step}"),
                        Some(&format!("n_rev_buf_{step}")),
                    )
                    .expect("insert_buffer");
                inserted_bufs.push(b_inst);
            }
        } else {
            let b_inst = inserted_bufs.swap_remove(rng.gen_range(inserted_bufs.len()));
            if netlist.instance(b_inst).is_ok() {
                netlist.remove_buffer(b_inst).expect("remove_buffer");
            }
        }

        let net_rev = commit_netlist(&ws, cur_rev, &netlist, &format!("opt step {step}"));
        let out = update_revision(&ws, Some(net_rev), &opts)
            .unwrap_or_else(|e| panic!("step {step}: update_revision failed: {e}"));
        assert_eq!(out.mode, UpdateMode::Incremental);
        let rev_obj = ws.revision(out.committed_rev).expect("open committed rev");
        StaStore::open_rev(&rev_obj)
            .expect("open committed sta.rbin")
            .verify_hash()
            .expect("verify_hash");
        cur_rev = out.committed_rev;
    }

    let _ = fs::remove_dir_all(&dir);
}

// STA-REQ-0455: 1,000 random sizing and buffering mutations on the `tt` library of each PDK; after
// every step the incremental state equals a full analysis bit for bit. Invoked by
// `cargo xtask verify pdk` (about 65 s and 0.2 GB for three libraries in release mode).
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_1000_mutations_bitwise() {
    for (idx, (tag, path)) in support::pdk::campaign_libraries().iter().enumerate() {
        let text = fs::read_to_string(path).expect("read PDK liberty file");
        let name = path.file_name().expect("file name").to_string_lossy();
        let lib = read_text(&text, &name).expect("parse PDK liberty file");
        let pair = build_catalog_netlist(&lib, 0x9D40_0000 + idx as u64, (6, 8, 24, 6));
        run_random_mutation_campaign(tag, pair, &lib, (0x9D41_0000 + idx as u64, 1_000, 1));
    }
}
