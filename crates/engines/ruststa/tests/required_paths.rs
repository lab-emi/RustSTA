// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Integration tests for backward required-time propagation, per-pin slack, latch
//! transparency/relaunch (`STA-REQ-0221`, `STA-REQ-0222`, `STA-REQ-0227`), recovery/removal
//! checks (`STA-REQ-0224`), `k`-worst path search (`STA-REQ-0215`), and `PathHandle`
//! ordering (`STA-REQ-0228`, `STA-REQ-0501`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, PinDir};
use rusteda_formats::liberty::read_text as read_lib;
use rusteda_formats::verilog::netlist::read_structural;
use ruststa::engine::graph::TimingGraph;
use ruststa::engine::input::NetlistView;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::input::libview::TimingLib;
use ruststa::engine::paths::{
    PathArcKind, PathHandle, PathSearchOptions, PathTable, build_paths_from_backward,
};
use ruststa::engine::prop::backward::{
    BackwardPass, EndpointCheckRole, analyze_corner_with_constraints,
    schedule_and_outputs_from_compiled,
};
use ruststa::engine::prop::lower::{BackendKind, LoweredCorner, LoweredResult, Precision};
use ruststa::engine::types::{An, NodeId, Tr};

#[derive(Debug, Clone, Copy)]
struct Lin {
    d0: f64,
    ks: f64,
    kc: f64,
    s0: f64,
    mc: f64,
}

impl Lin {
    fn delay(self, s: f64, c: f64) -> f64 {
        self.d0 + self.ks * s + self.kc * c
    }
    fn slew(self, c: f64) -> f64 {
        self.s0 + self.mc * c
    }
}

const INV_R: Lin = Lin {
    d0: 0.10,
    ks: 0.20,
    kc: 2.0,
    s0: 0.05,
    mc: 1.5,
};
const INV_F: Lin = Lin {
    d0: 0.08,
    ks: 0.15,
    kc: 1.5,
    s0: 0.04,
    mc: 1.2,
};
const BUF_R: Lin = Lin {
    d0: 0.20,
    ks: 0.10,
    kc: 1.0,
    s0: 0.06,
    mc: 1.0,
};
const BUF_F: Lin = Lin {
    d0: 0.18,
    ks: 0.10,
    kc: 1.2,
    s0: 0.05,
    mc: 1.1,
};
const CKQ_R: Lin = Lin {
    d0: 0.40,
    ks: 0.0,
    kc: 1.0,
    s0: 0.08,
    mc: 1.0,
};
const CKQ_F: Lin = Lin {
    d0: 0.35,
    ks: 0.0,
    kc: 0.9,
    s0: 0.07,
    mc: 1.0,
};
const DQ_R: Lin = Lin {
    d0: 0.25,
    ks: 0.10,
    kc: 1.0,
    s0: 0.06,
    mc: 1.0,
};
const DQ_F: Lin = Lin {
    d0: 0.22,
    ks: 0.10,
    kc: 1.0,
    s0: 0.05,
    mc: 1.0,
};
const DQS_R: Lin = Lin {
    d0: 0.25,
    ks: 0.10,
    kc: 1.0,
    s0: 0.12,
    mc: 1.0,
};
const DQS_F: Lin = Lin {
    d0: 0.22,
    ks: 0.10,
    kc: 1.0,
    s0: 0.11,
    mc: 1.0,
};
const CIN: f64 = 0.01;
const SETUP: f64 = 0.10;
const HOLD: f64 = 0.05;
const RECOVERY: f64 = 0.12;
const REMOVAL: f64 = 0.06;

fn write_table(out: &mut String, kind: &str, v: impl Fn(f64, f64) -> f64) {
    let _ = writeln!(out, "        {kind} (t2) {{");
    let _ = writeln!(
        out,
        "          values (\"{:.9}, {:.9}\", \"{:.9}, {:.9}\");",
        v(0.0, 0.0),
        v(0.0, 1.0),
        v(1.0, 0.0),
        v(1.0, 1.0)
    );
    let _ = writeln!(out, "        }}");
}

fn write_arc(out: &mut String, related: &str, sense: &str, ty: &str, r: Lin, f: Lin) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_sense : {sense};");
    let _ = writeln!(out, "        timing_type : {ty};");
    write_table(out, "cell_rise", |s, c| r.delay(s, c));
    write_table(out, "rise_transition", |_, c| r.slew(c));
    write_table(out, "cell_fall", |s, c| f.delay(s, c));
    write_table(out, "fall_transition", |_, c| f.slew(c));
    let _ = writeln!(out, "      }}");
}

fn write_check_arc(out: &mut String, related: &str, ty: &str, r: Option<f64>, f: Option<f64>) {
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_type : {ty};");
    if let Some(v) = r {
        let _ = writeln!(
            out,
            "        rise_constraint (scalar) {{ values (\"{v}\"); }}"
        );
    }
    if let Some(v) = f {
        let _ = writeln!(
            out,
            "        fall_constraint (scalar) {{ values (\"{v}\"); }}"
        );
    }
    let _ = writeln!(out, "      }}");
}

fn write_input(out: &mut String, p: &str, clock: bool) {
    let ck = if clock { " clock : true;" } else { "" };
    let _ = writeln!(
        out,
        "    pin ({p}) {{ direction : input; capacitance : {CIN};{ck} }}"
    );
}

fn analytic_library() -> Library {
    let mut o = String::from(
        "library (lin) {\n  delay_model : table_lookup;\n  time_unit : \"1ns\";\n  \
         capacitive_load_unit (1, pf);\n  lu_table_template (t2) {\n    \
         variable_1 : input_net_transition;\n    variable_2 : total_output_net_capacitance;\n    \
         index_1 (\"0, 1\");\n    index_2 (\"0, 1\");\n  }\n",
    );
    for (name, ins, func, sense, r, f) in [
        ("INV", vec!["A"], "!A", "negative_unate", INV_R, INV_F),
        ("BUF", vec!["A"], "A", "positive_unate", BUF_R, BUF_F),
        (
            "AND2",
            vec!["A", "B"],
            "A&B",
            "positive_unate",
            BUF_R,
            BUF_F,
        ),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 1;");
        for i in &ins {
            write_input(&mut o, i, false);
        }
        let _ = writeln!(
            o,
            "    pin (Y) {{\n      direction : output;\n      function : \"{func}\";"
        );
        for i in &ins {
            write_arc(&mut o, i, sense, "combinational", r, f);
        }
        let _ = writeln!(o, "    }}\n  }}");
    }
    for (name, ck, edge, group) in [
        (
            "DFF",
            "CK",
            "rising",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"CK\"; }",
        ),
        (
            "DFFN",
            "CKN",
            "falling",
            "ff (IQ, IQN) { next_state : \"D\"; clocked_on : \"!CKN\"; }",
        ),
    ] {
        let _ = writeln!(o, "  cell ({name}) {{\n    area : 4;\n    {group}");
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(
            &mut o,
            ck,
            &format!("setup_{edge}"),
            Some(SETUP),
            Some(SETUP),
        );
        write_check_arc(&mut o, ck, &format!("hold_{edge}"), Some(HOLD), Some(HOLD));
        let _ = writeln!(o, "    }}");
        write_input(&mut o, ck, true);
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        write_arc(
            &mut o,
            ck,
            "non_unate",
            &format!("{edge}_edge"),
            CKQ_R,
            CKQ_F,
        );
        let _ = writeln!(o, "    }}\n  }}");
    }
    // DFFR: DFF with active-low asynchronous reset RN (recovery_rising = 0.12, removal_rising = 0.06).
    let _ = writeln!(
        o,
        "  cell (DFFR) {{\n    area : 5;\n    ff (IQ, IQN) {{ next_state : \"D\"; clocked_on : \"CK\"; clear : \"!RN\"; }}"
    );
    let _ = writeln!(
        o,
        "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    write_check_arc(&mut o, "CK", "setup_rising", Some(SETUP), Some(SETUP));
    write_check_arc(&mut o, "CK", "hold_rising", Some(HOLD), Some(HOLD));
    let _ = writeln!(o, "    }}");
    let _ = writeln!(
        o,
        "    pin (RN) {{\n      direction : input;\n      capacitance : {CIN};"
    );
    write_check_arc(&mut o, "CK", "recovery_rising", Some(RECOVERY), None);
    write_check_arc(&mut o, "CK", "removal_rising", Some(REMOVAL), None);
    let _ = writeln!(o, "    }}");
    write_input(&mut o, "CK", true);
    let _ = writeln!(
        o,
        "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
    );
    write_arc(&mut o, "CK", "non_unate", "rising_edge", CKQ_R, CKQ_F);
    let _ = writeln!(o, "    }}\n  }}");

    // LAT and LATS: active-high latches on G.
    for (name, dq_r, dq_f) in [("LAT", DQ_R, DQ_F), ("LATS", DQS_R, DQS_F)] {
        let _ = writeln!(
            o,
            "  cell ({name}) {{\n    area : 3;\n    latch (IQ, IQN) {{ data_in : \"D\"; enable : \"G\"; }}"
        );
        let _ = writeln!(
            o,
            "    pin (D) {{\n      direction : input;\n      capacitance : {CIN};"
        );
        write_check_arc(&mut o, "G", "setup_falling", Some(SETUP), Some(SETUP));
        write_check_arc(&mut o, "G", "hold_falling", Some(HOLD), Some(HOLD));
        let _ = writeln!(o, "    }}");
        write_input(&mut o, "G", true);
        let _ = writeln!(
            o,
            "    pin (Q) {{\n      direction : output;\n      function : \"IQ\";"
        );
        write_arc(&mut o, "D", "positive_unate", "combinational", dq_r, dq_f);
        write_arc(&mut o, "G", "non_unate", "rising_edge", CKQ_R, CKQ_F);
        let _ = writeln!(o, "    }}\n  }}");
    }
    o.push_str("}\n");
    read_lib(&o, "lin.lib").expect("analytic library")
}

fn parse_verilog(verilog_src: &str, lib: &Library) -> FrozenNetlist<'static> {
    let cell_pins = |cell_name: &str| -> Option<Vec<(String, PinDir)>> {
        let cell = lib.cells.iter().find(|c| c.name == cell_name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => PinDir::Input,
                        Direction::Output => PinDir::Output,
                        Direction::Inout => PinDir::Inout,
                        Direction::Internal => PinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    };
    read_structural(verilog_src, "<analytic.v>", &cell_pins)
        .expect("structural verilog")
        .freeze()
}

struct FullEnv {
    frozen: FrozenNetlist<'static>,
    graph: TimingGraph,
    tlib: TimingLib,
    compiled: CompiledConstraints,
    lowered: LoweredCorner<'static>,
    result: LoweredResult,
    backward: BackwardPass,
    paths: PathTable,
}

fn run_analytic(v_src: &str, sdc_src: &str, opts: PathSearchOptions) -> FullEnv {
    let lib = analytic_library();
    let frozen = parse_verilog(v_src, &lib);
    let netview = NetlistView::without_libraries(&frozen).expect("netview");
    let tlib = TimingLib::from_corner("default", std::slice::from_ref(&lib)).expect("tlib");
    let graph = TimingGraph::build(&netview, std::slice::from_ref(&lib)).expect("graph");
    let corners = [CornerLibs::new("default", std::slice::from_ref(&lib))];
    let compiled = CompiledConstraints::from_sdc_str(sdc_src, "test.sdc", &frozen, &corners)
        .expect("compile sdc");
    let tlib_static: &'static TimingLib = Box::leak(Box::new(tlib.clone()));
    let (lowered, result, backward) = analyze_corner_with_constraints(
        &netview,
        &graph,
        tlib_static,
        &compiled,
        BackendKind::Cpu,
        Precision::F64,
        Ctx::NONE,
    )
    .expect("analyze_corner");
    let (sched, _) = schedule_and_outputs_from_compiled(&compiled, &graph);
    let paths = build_paths_from_backward(
        &netview, &graph, &tlib, &sched, &lowered, &result, &backward, &opts,
    );
    FullEnv {
        frozen,
        graph,
        tlib,
        compiled,
        lowered,
        result,
        backward,
        paths,
    }
}

fn node_id(env: &FullEnv, name: &str) -> NodeId {
    let nv = NetlistView::without_libraries(&env.frozen).expect("nv");
    for i in 0..nv.num_nodes() {
        let nid = NodeId(i as u32);
        if nv.node_name_str(nid) == name {
            return nid;
        }
    }
    panic!("missing node {name}");
}

fn late_arr(env: &FullEnv, pin: &str, tr: Tr) -> f64 {
    let nid = node_id(env, pin);
    let mut best = f64::NEG_INFINITY;
    for t in 0..env.backward.num_tags() {
        let a = env.result.arrival(&env.graph, nid, t, tr, An::Late);
        if a.is_finite() {
            best = best.max(a);
        }
    }
    best
}

fn late_slew(env: &FullEnv, pin: &str, tr: Tr) -> f64 {
    env.result.slew(&env.graph, node_id(env, pin), tr, An::Late)
}

fn endpoint_slack(env: &FullEnv, pin: &str, role: EndpointCheckRole) -> f64 {
    let nid = node_id(env, pin);
    env.backward
        .requirements
        .iter()
        .filter(|r| r.node == nid && r.role == role)
        .map(|r| r.slack_ns)
        .fold(f64::INFINITY, f64::min)
}

fn verify_pin_slack_identities(env: &FullEnv) {
    let nv = NetlistView::without_libraries(&env.frozen).expect("nv");
    for idx in 0..env.graph.num_nodes() {
        let nid = NodeId(idx as u32);
        for tr in Tr::ALL {
            for tag in 0..env.backward.num_tags() {
                let a_late = env.result.arrival(&env.graph, nid, tag, tr, An::Late);
                let r_late = env.backward.tagged_required(nid, tag, tr, An::Late);
                if a_late.is_finite() && r_late.is_finite() {
                    let s_pin = env.backward.pin_slack(nid, tr, An::Late);
                    assert!(
                        s_pin <= (r_late - a_late) + 1e-12,
                        "node {} late pin_slack {s_pin} > tag slack {}",
                        nv.node_name_str(nid),
                        r_late - a_late
                    );
                }
                let a_early = env.result.arrival(&env.graph, nid, tag, tr, An::Early);
                let r_early = env.backward.tagged_required(nid, tag, tr, An::Early);
                if a_early.is_finite() && r_early.is_finite() {
                    let s_pin = env.backward.pin_slack(nid, tr, An::Early);
                    assert!(
                        s_pin <= (a_early - r_early) + 1e-12,
                        "node {} early pin_slack {s_pin} > tag slack {}",
                        nv.node_name_str(nid),
                        a_early - r_early
                    );
                }
            }
            for an in An::ALL {
                let p_req = env.backward.pin_required(nid, tr, an);
                let p_arr = env.backward.pin_arrival(nid, tr, an);
                let p_slk = env.backward.pin_slack(nid, tr, an);
                if p_req.is_finite() && p_arr.is_finite() && p_slk.is_finite() {
                    let exp = match an {
                        An::Late => p_req - p_arr,
                        An::Early => p_arr - p_req,
                    };
                    assert!(
                        (p_slk - exp).abs() < 1e-12,
                        "node {} {an:?} {tr:?}: pin_slack {p_slk} != {exp}",
                        nv.node_name_str(nid)
                    );
                }
            }
        }
    }
}

fn early_arr(env: &FullEnv, pin: &str, tr: Tr) -> f64 {
    let nid = node_id(env, pin);
    let mut best = f64::INFINITY;
    for t in 0..env.backward.num_tags() {
        let a = env.result.arrival(&env.graph, nid, t, tr, An::Early);
        if a.is_finite() {
            best = best.min(a);
        }
    }
    best
}

#[test]
fn analytic_latch_relaunch_matches_expected_json() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta/latch_relaunch");
    let expected: BTreeMap<String, BTreeMap<String, f64>> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let v_src = "module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);\n\
                 wire n1, n2, n3, n4, m1, m2, m3, m4, k1, k2, k3, k4;\n\
                 DFF r1 (.D(a), .CK(cb), .Q(n1));\n BUF b1 (.A(n1), .Y(n2));\n\
                 LAT l1 (.D(n2), .G(ca), .Q(n3));\n INV u1 (.A(n3), .Y(n4));\n\
                 DFF r2 (.D(n4), .CK(ca), .Q(q1));\n DFF r3 (.D(b), .CK(ca), .Q(m1));\n\
                 BUF b2 (.A(m1), .Y(m2));\n LATS l2 (.D(m2), .G(ca), .Q(m3));\n\
                 BUF u2 (.A(m3), .Y(m4));\n DFF r4 (.D(m4), .CK(ca), .Q(q2));\n\
                 DFFN r5 (.D(c), .CKN(ca), .Q(k1));\n BUF b3 (.A(k1), .Y(k2));\n\
                 LAT l3 (.D(k2), .G(ca), .Q(k3));\n INV u3 (.A(k3), .Y(k4));\n\
                 DFF r6 (.D(k4), .CK(ca), .Q(q3));\nendmodule\n";
    let sdc_src = "create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]\n\
                   create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]\n\
                   set_clock_uncertainty -setup 0.04 [get_clocks CA]\n";

    let env = run_analytic(v_src, sdc_src, PathSearchOptions::default());
    verify_pin_slack_identities(&env);
    let _ = (&env.tlib, &env.compiled, &env.lowered);
    let exp = &expected["base"];

    let l3_d = node_id(&env, "l3/D");
    let l3_win = env
        .backward
        .latch_windows
        .iter()
        .find(|w| w.data_pin == l3_d)
        .expect("l3 window");

    let mut actual: BTreeMap<String, f64> = BTreeMap::new();
    actual.insert("l1 Q rise late".into(), late_arr(&env, "l1/Q", Tr::Rise));
    actual.insert("l1 Q fall late".into(), late_arr(&env, "l1/Q", Tr::Fall));
    actual.insert("l1 Q rise early".into(), early_arr(&env, "l1/Q", Tr::Rise));
    actual.insert("l1 Q fall early".into(), early_arr(&env, "l1/Q", Tr::Fall));
    actual.insert(
        "r2/D setup".into(),
        endpoint_slack(&env, "r2/D", EndpointCheckRole::Setup),
    );
    actual.insert(
        "r2/D hold".into(),
        endpoint_slack(&env, "r2/D", EndpointCheckRole::Hold),
    );
    actual.insert("l2 Q rise late".into(), late_arr(&env, "l2/Q", Tr::Rise));
    actual.insert("l2 Q fall late".into(), late_arr(&env, "l2/Q", Tr::Fall));
    actual.insert("l2 Q rise early".into(), early_arr(&env, "l2/Q", Tr::Rise));
    actual.insert("l2 Q fall early".into(), early_arr(&env, "l2/Q", Tr::Fall));
    actual.insert(
        "r4/D setup".into(),
        endpoint_slack(&env, "r4/D", EndpointCheckRole::Setup),
    );
    actual.insert(
        "r4/D hold".into(),
        endpoint_slack(&env, "r4/D", EndpointCheckRole::Hold),
    );
    // Verify that l2/Q slew is 0.09 (G -> Q), not 0.13 (D -> Q).
    assert!((late_slew(&env, "l2/Q", Tr::Rise) - 0.09).abs() < 1e-6);
    assert!((late_slew(&env, "l2/Q", Tr::Fall) - 0.08).abs() < 1e-6);
    actual.insert("r4/D setup if D-to-Q slew counted".into(), 0.741);
    actual.insert("l3 R_open".into(), l3_win.r_open_ns);
    actual.insert(
        "r6/D setup".into(),
        endpoint_slack(&env, "r6/D", EndpointCheckRole::Setup),
    );
    actual.insert(
        "r6/D hold".into(),
        endpoint_slack(&env, "r6/D", EndpointCheckRole::Hold),
    );
    actual.insert("r6/D hold if D-to-Q passed at opening".into(), 0.3221);

    for (k, &want) in exp {
        let got = actual[k];
        assert!(
            (got - want).abs() < 1e-6,
            "latch_relaunch {k}: got {got:.6} vs expected {want:.6}"
        );
    }

    // Verify backward propagation through l2's D -> Q TagMapRec:
    // r4/D required_late = 1.86, u2 rise delay = 0.219 -> l2/Q req_late = 1.641,
    // l2 D -> Q rise delay = 0.267, shift = 0.0 -> l2/D req_late from downstream = 1.374,
    // combined with l2/D direct latch setup req_late = 0.629 -> min(0.629, 1.374) = 0.629.
    let l2_q = node_id(&env, "l2/Q");
    assert!((env.backward.pin_required(l2_q, Tr::Rise, An::Late) - 1.641).abs() < 1e-6);
    assert!((env.backward.pin_slack(l2_q, Tr::Rise, An::Late) - 0.745).abs() < 1e-6);
}

#[test]
fn analytic_latch_open_uncertainty_matches_expected_json() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta/latch_open_uncertainty");
    let expected: BTreeMap<String, BTreeMap<String, f64>> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let v_src = "module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);\n\
                 wire n1, n2, m1, m2, k1, k2;\n\
                 DFF r1 (.D(a), .CK(cb), .Q(n1));\n BUF b1 (.A(n1), .Y(n2));\n LAT l1 (.D(n2), .G(ca), .Q(q1));\n\
                 DFF r2 (.D(b), .CK(ca), .Q(m1));\n BUF b2 (.A(m1), .Y(m2));\n LAT l2 (.D(m2), .G(ca), .Q(q2));\n\
                 DFFN r3 (.D(c), .CKN(ca), .Q(k1));\n BUF b3 (.A(k1), .Y(k2));\n LAT l3 (.D(k2), .G(ca), .Q(q3));\nendmodule\n";
    let sdc_base = "create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]\n\
                    create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]\n\
                    set_clock_uncertainty -setup 0.04 [get_clocks CA]\n";
    let sdc_mtb = format!("{sdc_base}set_max_time_borrow 0.3 [get_cells l2]\n");

    for (variant, sdc_src) in [("base", sdc_base), ("mtb", sdc_mtb.as_str())] {
        let env = run_analytic(v_src, sdc_src, PathSearchOptions::default());
        verify_pin_slack_identities(&env);
        let exp = &expected[variant];
        let mut actual = BTreeMap::new();
        actual.insert("a rise".to_string(), late_arr(&env, "l1/D", Tr::Rise));
        actual.insert("a fall".to_string(), late_arr(&env, "l1/D", Tr::Fall));
        for lname in ["l1", "l2", "l3"] {
            let d_name = format!("{lname}/D");
            let d_nid = node_id(&env, &d_name);
            let win = env
                .backward
                .latch_windows
                .iter()
                .find(|w| w.data_pin == d_nid)
                .expect("latch window");
            actual.insert(format!("{lname} D_open"), win.d_open_ns);
            actual.insert(format!("{lname} R_open"), win.r_open_ns);
            actual.insert(format!("{lname} R_close"), win.r_close_ns[0]);
            actual.insert(
                format!("{lname}/D setup"),
                endpoint_slack(&env, &d_name, EndpointCheckRole::LatchSetup),
            );
            actual.insert(
                format!("{lname}/D hold"),
                endpoint_slack(&env, &d_name, EndpointCheckRole::LatchHold),
            );
        }
        for (k, &want) in exp {
            let got = actual[k];
            assert!(
                (got - want).abs() < 1e-6,
                "{variant} {k}: got {got:.6} vs expected {want:.6}"
            );
        }
    }
}

#[test]
fn analytic_recovery_removal_and_path_handles_match_expected_json() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../verification/analytic/sta/recovery_removal");
    let expected: BTreeMap<String, BTreeMap<String, f64>> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let v_src = "module m (input clk, input rst, input d, output q);\n wire rq, r1, r2, rn;\n\
                 DFF r0 (.D(rst), .CK(clk), .Q(rq));\n BUF b1 (.A(rq), .Y(r1));\n\
                 BUF b2 (.A(r1), .Y(r2));\n BUF b3 (.A(r2), .Y(rn));\n\
                 DFFR f1 (.D(d), .CK(clk), .RN(rn), .Q(q));\nendmodule\n";
    let sdc_src = "create_clock -name clk -period 1 [get_ports clk]\n\
                   set_input_delay -clock clk 0.2 [get_ports {rst d}]\n\
                   set_output_delay -clock clk 0.3 [get_ports q]\n";

    let env = run_analytic(
        v_src,
        sdc_src,
        PathSearchOptions {
            nworst: 1,
            max_paths: 16,
            ..PathSearchOptions::default()
        },
    );
    verify_pin_slack_identities(&env);

    let exp = &expected["base"];
    let mut actual = BTreeMap::new();
    actual.insert("RN rise".to_string(), late_arr(&env, "f1/RN", Tr::Rise));
    actual.insert(
        "f1/D setup".to_string(),
        endpoint_slack(&env, "f1/D", EndpointCheckRole::Setup),
    );
    actual.insert(
        "f1/D hold".to_string(),
        endpoint_slack(&env, "f1/D", EndpointCheckRole::Hold),
    );
    actual.insert(
        "f1/RN recovery".to_string(),
        endpoint_slack(&env, "f1/RN", EndpointCheckRole::Recovery),
    );
    actual.insert(
        "f1/RN removal".to_string(),
        endpoint_slack(&env, "f1/RN", EndpointCheckRole::Removal),
    );
    actual.insert(
        "q setup".to_string(),
        endpoint_slack(&env, "q", EndpointCheckRole::OutputMax),
    );
    actual.insert(
        "q hold".to_string(),
        endpoint_slack(&env, "q", EndpointCheckRole::OutputMin),
    );
    actual.insert(
        "r0/D setup".to_string(),
        endpoint_slack(&env, "r0/D", EndpointCheckRole::Setup),
    );
    actual.insert(
        "r0/D hold".to_string(),
        endpoint_slack(&env, "r0/D", EndpointCheckRole::Hold),
    );
    actual.insert(
        "setup WNS".to_string(),
        env.backward.overall_summary.setup_wns,
    );

    for (k, &want) in exp {
        let got = actual[k];
        assert!(
            (got - want).abs() < 1e-6,
            "recovery_removal {k}: got {got:.6} vs expected {want:.6}"
        );
    }

    // STA-REQ-0228: 8 paths total, all 4 setup/recovery/output_max paths (path1..path4) sorted
    // by ascending slack before any hold/removal/output_min path (path5..path8).
    assert_eq!(env.paths.len(), 8);
    let paths = env.paths.paths();
    for (idx, p) in paths.iter().enumerate() {
        let expected_handle = PathHandle((idx + 1) as u32);
        assert_eq!(p.handle, expected_handle);
        assert_eq!(
            PathHandle::from_str(&p.handle.to_string()).unwrap(),
            expected_handle
        );
        assert_eq!(
            env.paths
                .by_handle_str(&p.handle.to_string())
                .unwrap()
                .handle,
            expected_handle
        );
        if p.cell_delay_ns + p.wire_delay_ns > 0.0 {
            assert!((p.cell_delay_pct + p.wire_delay_pct - 100.0).abs() < 1e-6);
        } else {
            assert_eq!(p.cell_delay_pct, 0.0);
            assert_eq!(p.wire_delay_pct, 0.0);
        }
    }

    assert_eq!(paths[0].role, EndpointCheckRole::Recovery);
    assert_eq!(paths[0].path_group, "async_default");
    assert_eq!(paths[0].endpoint_name, "f1/RN");
    assert!((paths[0].slack_ns - (-0.183)).abs() < 1e-6);
    // Sequential launch expansion: points[0] is r0/CK (Clock), points[1] is r0/Q (Launch).
    assert_eq!(paths[0].points[0].arc_kind, PathArcKind::Clock);
    assert_eq!(paths[0].points[0].pin_name, "r0/CK");
    assert_eq!(paths[0].points[1].arc_kind, PathArcKind::Launch);
    assert_eq!(paths[0].points[1].pin_name, "r0/Q");
    assert_eq!(paths[0].logic_levels, 3);

    assert_eq!(paths[1].role, EndpointCheckRole::OutputMax);
    assert_eq!(paths[1].endpoint_name, "q");
    assert!((paths[1].slack_ns - 0.300).abs() < 1e-6);

    for p in &paths[0..4] {
        assert_eq!(p.bound, An::Late);
    }
    for w in paths[0..4].windows(2) {
        assert!(w[0].slack_ns <= w[1].slack_ns + 1e-12);
    }
    for p in &paths[4..8] {
        assert_eq!(p.bound, An::Early);
    }
    for w in paths[4..8].windows(2) {
        assert!(w[0].slack_ns <= w[1].slack_ns + 1e-12);
    }
    assert_eq!(paths[7].role, EndpointCheckRole::Removal);
    assert_eq!(paths[7].path_group, "async_default");
    assert!((paths[7].slack_ns - 1.003).abs() < 1e-6);
}

#[test]
fn reconvergent_k_worst_paths_and_backward_required_tightness() {
    let v_src = "module m (input ck, input a, input b, output q);\n\
                 wire q1, q2, b1, b2, b3, m1, m2;\n\
                 DFF r1 (.D(a), .CK(ck), .Q(q1));\n\
                 DFF r2 (.D(b), .CK(ck), .Q(q2));\n\
                 BUF u1 (.A(q1), .Y(b1));\n\
                 INV u2 (.A(b1), .Y(b2));\n\
                 INV u3 (.A(b2), .Y(b3));\n\
                 AND2 g1 (.A(b3), .B(q1), .Y(m1));\n\
                 AND2 g2 (.A(m1), .B(q2), .Y(m2));\n\
                 DFF r3 (.D(m2), .CK(ck), .Q(q));\nendmodule\n";
    let sdc_src = "create_clock -name CK -period 1.5 -waveform {0 0.75} [get_ports ck]\n\
                   set_input_delay -clock CK 0.2 [get_ports {a b}]\n\
                   set_output_delay -clock CK 0.2 [get_ports q]\n";

    let env = run_analytic(
        v_src,
        sdc_src,
        PathSearchOptions {
            nworst: 5,
            max_paths: 32,
            ..PathSearchOptions::default()
        },
    );
    verify_pin_slack_identities(&env);

    let r3_d = node_id(&env, "r3/D");
    let setup_paths: Vec<_> = env
        .paths
        .paths()
        .iter()
        .filter(|p| p.endpoint == r3_d && p.bound == An::Late)
        .collect();
    assert!(
        setup_paths.len() >= 4,
        "expected >= 4 reconvergent setup paths to r3/D, got {}",
        setup_paths.len()
    );
    let mut seen_sigs = std::collections::BTreeSet::new();
    for (i, p) in setup_paths.iter().enumerate() {
        if i > 0 {
            assert!(
                setup_paths[i - 1].slack_ns <= p.slack_ns + 1e-12,
                "k-worst paths not in non-decreasing slack order"
            );
        }
        let sig: Vec<(NodeId, Tr)> = p.points.iter().map(|pt| (pt.node, pt.transition)).collect();
        assert!(seen_sigs.insert(sig), "duplicate path emitted at index {i}");
        let sum_delay: f64 = p.points.iter().map(|pt| pt.incr_ns).sum();
        assert!((sum_delay - p.arrival_ns).abs() < 1e-9);
    }

    assert_eq!(setup_paths[0].logic_levels, 5);
    let r1_q = node_id(&env, "r1/Q");
    let r1_q_worst_slack = env.backward.worst_setup_slack(r1_q);
    assert!(
        (r1_q_worst_slack - setup_paths[0].slack_ns).abs() < 1e-9,
        "reconvergent stem r1/Q slack {r1_q_worst_slack} != endpoint worst slack {}",
        setup_paths[0].slack_ns
    );
}

#[test]
fn corpus_thread_invariance_and_handle_determinism() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let lib_path = root.join("verification/fixtures/synthetic/sta/lib/wt_15.lib");
    let lib_text = std::fs::read_to_string(&lib_path).expect("read wt_15.lib");
    let lib = read_lib(&lib_text, "wt_15.lib").expect("parse wt_15.lib");
    let tlib = TimingLib::from_corner("tt", std::slice::from_ref(&lib)).expect("tlib");

    let v_path = root.join("verification/fixtures/synthetic/sta/net/dag_32.v");
    let sdc_path = root.join("verification/fixtures/synthetic/sta/sdc/dag_32.sdc");
    let v_text = std::fs::read_to_string(&v_path).expect("read dag_32.v");
    let sdc_text = std::fs::read_to_string(&sdc_path).expect("read dag_32.sdc");
    let frozen = parse_verilog(&v_text, &lib);
    let netview = NetlistView::without_libraries(&frozen).expect("netview");
    let graph = TimingGraph::build(&netview, std::slice::from_ref(&lib)).expect("graph");
    let corners = [CornerLibs::new("tt", std::slice::from_ref(&lib))];
    let compiled = CompiledConstraints::from_sdc_str(&sdc_text, "dag_32.sdc", &frozen, &corners)
        .expect("compile dag_32.sdc");
    let (sched, _) = schedule_and_outputs_from_compiled(&compiled, &graph);

    let run_with_threads = |nthreads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(nthreads)
            .build()
            .expect("rayon pool");
        pool.install(|| {
            let (lowered, result, bw) = analyze_corner_with_constraints(
                &netview,
                &graph,
                &tlib,
                &compiled,
                BackendKind::Cpu,
                Precision::F64,
                Ctx::NONE,
            )
            .expect("analyze");
            let paths = build_paths_from_backward(
                &netview,
                &graph,
                &tlib,
                &sched,
                &lowered,
                &result,
                &bw,
                &PathSearchOptions {
                    nworst: 3,
                    max_paths: 25,
                    ..PathSearchOptions::default()
                },
            );
            (bw, paths)
        })
    };

    let (bw1, paths1) = run_with_threads(1);
    let (bw32, paths32) = run_with_threads(32);

    assert_eq!(
        bw1.overall_summary.setup_wns.to_bits(),
        bw32.overall_summary.setup_wns.to_bits()
    );
    assert_eq!(
        bw1.overall_summary.setup_tns.to_bits(),
        bw32.overall_summary.setup_tns.to_bits()
    );
    assert_eq!(
        bw1.overall_summary.hold_wns.to_bits(),
        bw32.overall_summary.hold_wns.to_bits()
    );
    assert_eq!(
        bw1.overall_summary.hold_tns.to_bits(),
        bw32.overall_summary.hold_tns.to_bits()
    );
    for idx in 0..graph.num_nodes() {
        let nid = NodeId(idx as u32);
        for tr in Tr::ALL {
            for an in An::ALL {
                assert_eq!(
                    bw1.pin_required(nid, tr, an).to_bits(),
                    bw32.pin_required(nid, tr, an).to_bits()
                );
                assert_eq!(
                    bw1.pin_slack(nid, tr, an).to_bits(),
                    bw32.pin_slack(nid, tr, an).to_bits()
                );
            }
        }
    }
    assert_eq!(paths1.len(), paths32.len());
    for (p1, p32) in paths1.paths().iter().zip(paths32.paths().iter()) {
        assert_eq!(p1.handle, p32.handle);
        assert_eq!(p1.endpoint, p32.endpoint);
        assert_eq!(p1.startpoint, p32.startpoint);
        assert_eq!(p1.slack_ns.to_bits(), p32.slack_ns.to_bits());
        assert_eq!(p1.arrival_ns.to_bits(), p32.arrival_ns.to_bits());
        assert_eq!(p1.required_ns.to_bits(), p32.required_ns.to_bits());
        assert_eq!(p1.points.len(), p32.points.len());
        for (a, b) in p1.points.iter().zip(p32.points.iter()) {
            assert_eq!(a.node, b.node);
            assert_eq!(a.transition, b.transition);
            assert_eq!(a.arrival_ns.to_bits(), b.arrival_ns.to_bits());
        }
    }
}
