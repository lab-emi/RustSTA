// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests for SplitMix64, sorted-cell catalog, seeded topology generators, shuffled insertion
//! canonicalization, 5k+ high-fanout nets, scale generators, and SDC generators.

use rusteda_core::netlist::Conn;
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc;
use ruststa_fixtures::lib_gen::wt_gfconv;
use ruststa_fixtures::net_gen::{
    CellCatalog, SplitMix64, gen_chain, gen_clock_crossing, gen_clock_gated, gen_constant_tied,
    gen_dag, gen_fanout_tree, gen_high_fanout_5k, gen_latch_pipeline, gen_scale_500k,
    gen_scale_netlist, gen_tristate_bus, synthetic_netlists,
};
use ruststa_fixtures::sdc_gen::synthetic_sdcs;

#[test]
fn splitmix64_is_deterministic() {
    let mut a = SplitMix64::new(0xDEAD_BEEF);
    let mut b = SplitMix64::new(0xDEAD_BEEF);
    let mut c = SplitMix64::new(0xDEAD_BEF0);
    for _ in 0..32 {
        let va = a.next_u64();
        let vb = b.next_u64();
        assert_eq!(va, vb);
        let f = a.next_f64();
        assert!((0.0..1.0).contains(&f));
        assert_eq!(f, b.next_f64());
    }
    assert_ne!(a.next_u64(), c.next_u64());
}

#[test]
fn cell_catalog_orders_by_sorted_cell_name_and_skips_dont_use() {
    let lib = read_text(&wt_gfconv(), "wt_gfconv").unwrap();
    let cat = CellCatalog::from_library(&lib);
    let names: Vec<&str> = cat.all_sorted.iter().map(|c| c.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(!names.contains(&"wt_fill_1"));
}

#[test]
fn topologies_parse_and_shuffle_to_same_canonical_multiset() {
    for fix in synthetic_netlists() {
        let nl = fix.to_gate_netlist();
        assert_eq!(nl.name(), fix.top);
        assert_eq!(nl.instance_count(), fix.gate_count());

        let shuf = fix.to_gate_netlist_shuffled(0xCAFE_1234);
        assert_eq!(nl.instance_count(), shuf.instance_count());
        assert_eq!(nl.net_count(), shuf.net_count());

        // Compare canonical sorted instance descriptors `(inst_name, cell_name, [(pin, net)])`.
        let describe = |g: &rusteda_core::netlist::GateNetlist| {
            let mut v = Vec::new();
            for (_id, inst) in g.instances() {
                let cell = g.cell(inst.cell).unwrap();
                let cell_name = g.resolve(cell.name).to_string();
                let mut conns: Vec<(String, String)> = inst
                    .connected
                    .iter()
                    .map(|&pid| {
                        let p = g.pin(pid).unwrap();
                        let pin = g.resolve(cell.pins[p.cell_pin as usize].name).to_string();
                        let net = match p.conn.unwrap() {
                            Conn::Net(nid) => g.net_label(nid),
                            Conn::Const(false) => "1'b0".to_string(),
                            Conn::Const(true) => "1'b1".to_string(),
                        };
                        (pin, net)
                    })
                    .collect();
                conns.sort();
                v.push((g.resolve(inst.name).to_string(), cell_name, conns));
            }
            v.sort();
            v
        };
        assert_eq!(describe(&nl), describe(&shuf), "fixture {}", fix.name);
    }
}

#[test]
fn high_fanout_5k_and_tristate_and_dual_clock_properties() {
    let hf = gen_high_fanout_5k(42);
    let nl = hf.to_gate_netlist();
    assert_eq!(nl.instance_count(), 5001);
    let drv_net_loads = nl.nets().map(|(_, net)| net.pins.len()).max().unwrap_or(0);
    assert!(drv_net_loads >= 5001, "loads+driver = {drv_net_loads}");

    let tri = gen_tristate_bus(7, 3, 2);
    let tri_nl = tri.to_gate_netlist();
    assert_eq!(tri_nl.instance_count(), 5);

    let cc = gen_clock_crossing(9, 4);
    assert_eq!(cc.clocks, ["clk_a", "clk_b"]);
    assert!(cc.verilog.contains("wt_dff_p") && cc.verilog.contains("wt_dffr_n"));

    let lp = gen_latch_pipeline(11, 4);
    assert!(lp.verilog.contains("wt_dlatch_p") && lp.verilog.contains("wt_dlatch_n"));

    let cg = gen_clock_gated(13, 4);
    assert!(cg.verilog.contains("wt_icg_p"));

    let ct = gen_constant_tied(15, 4);
    assert!(ct.verilog.contains("wt_tiehi") && ct.verilog.contains("wt_tielo"));

    let ch = gen_chain(17, 12);
    assert_eq!(ch.gate_count(), 12);

    let dg = gen_dag(19, 24, 2);
    assert_eq!(dg.gate_count(), 24);

    let ft = gen_fanout_tree(21, 16);
    assert_eq!(ft.gate_count(), 17);
}

#[test]
fn scale_generator_builds_frozen_500k_netlist() {
    let small = gen_scale_netlist(1, 1024);
    assert_eq!(small.instance_count(), 1024);
    let large = gen_scale_500k(1);
    assert_eq!(large.instance_count(), 500_000);
}

#[test]
fn synthetic_sdcs_parse_cleanly() {
    let sdcs = synthetic_sdcs();
    assert_eq!(sdcs.len(), synthetic_netlists().len());
    for (name, text) in sdcs {
        let (ast, diags) = sdc::read_str(&text);
        assert!(
            !diags.iter().any(|d| d.severity == sdc::Severity::Error),
            "{name}: {diags:?}"
        );
        assert!(!ast.cmds.is_empty(), "{name}");
    }
}
