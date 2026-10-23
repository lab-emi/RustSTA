// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suites `sta-metamorphic`, `sta-exhaustive` and `sta-defects` (STA-REQ-0502 to STA-REQ-0504)
//! on random circuits of the linear library.

mod sta_common;

use std::fmt::Write as _;

use sta_common::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A random acyclic circuit between flip-flops clocked by `clk`, with `prefix` on every
/// instance name and statements in a seed-dependent order when `shuffle` is set.
fn circuit(seed: u64, prefix: &str, shuffle: bool) -> String {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let nff = 2 + r.below(3);
    let ngates = 3 + r.below(9);
    let mut signals: Vec<String> = vec!["i0".into(), "i1".into()];
    let mut stmts = Vec::new();
    let mut wires = Vec::new();
    for k in 0..nff {
        signals.push(format!("q{k}"));
        wires.push(format!("q{k}"));
    }
    for g in 0..ngates {
        let y = format!("w{g}");
        let pick = |r: &mut Rng, s: &[String]| s[r.below(s.len())].clone();
        let stmt = match r.below(4) {
            0 => format!(
                "INV {prefix}g{g} (.A({}), .Y({y}));",
                pick(&mut r, &signals)
            ),
            1 => format!(
                "BUF {prefix}g{g} (.A({}), .Y({y}));",
                pick(&mut r, &signals)
            ),
            2 => format!(
                "AND2 {prefix}g{g} (.A({}), .B({}), .Y({y}));",
                pick(&mut r, &signals),
                pick(&mut r, &signals)
            ),
            _ => format!(
                "XOR2 {prefix}g{g} (.A({}), .B({}), .Y({y}));",
                pick(&mut r, &signals),
                pick(&mut r, &signals)
            ),
        };
        stmts.push(stmt);
        wires.push(y.clone());
        signals.push(y);
    }
    for k in 0..nff {
        let d = signals[2 + r.below(signals.len() - 2)].clone();
        stmts.push(format!("DFF {prefix}r{k} (.D({d}), .CK(clk), .Q(q{k}));"));
    }
    stmts.push(format!(
        "BUF {prefix}ob (.A({}), .Y(o0));",
        signals[signals.len() - 1]
    ));
    if shuffle {
        for i in (1..stmts.len()).rev() {
            let j = r.below(i + 1);
            stmts.swap(i, j);
        }
    }
    let mut out = String::from("module m (input clk, input i0, input i1, output o0);\n");
    let _ = writeln!(out, " wire {};", wires.join(", "));
    for s in stmts {
        let _ = writeln!(out, " {s}");
    }
    out.push_str("endmodule\n");
    out
}

fn sdc(period: f64, input: f64) -> String {
    format!(
        "create_clock -name clk -period {period} [get_ports clk]\n\
         set_input_delay -clock clk {input} [get_ports {{i0 i1}}]\n\
         set_output_delay -clock clk 0.2 [get_ports o0]\nset_load 0.03 [get_ports o0]\n"
    )
}

fn by_pin(r: &ruststa::Report) -> std::collections::BTreeMap<String, (Option<f64>, Option<f64>)> {
    r.endpoints
        .iter()
        .map(|e| (e.pin.clone(), (e.setup_slack, e.hold_slack)))
        .collect()
}

// STA-REQ-0503: a longer clock period moves every setup slack by exactly the difference (all
// paths launch and capture on the rising edge) and leaves hold slacks unchanged.
#[test]
fn period_shift() {
    for seed in 0..60 {
        let n = circuit(seed, "", false);
        let a = by_pin(&sta(&n, &sdc(3.0, 0.4)));
        let b = by_pin(&sta(&n, &sdc(3.75, 0.4)));
        assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        for (p, (s, h)) in &a {
            let (s2, h2) = b[p];
            if let (Some(x), Some(y)) = (s, s2) {
                assert!((y - x - 0.75).abs() < 1e-9, "seed {seed} {p}: {x} -> {y}");
            }
            if let (Some(x), Some(y)) = (h, h2) {
                assert!((y - x).abs() < 1e-9, "seed {seed} {p} hold: {x} -> {y}");
            }
        }
    }
}

// STA-REQ-0503: a later input delay never improves a setup slack, worsens none by more than the
// shift, and worsens by exactly the shift where the worst path starts at an input.
#[test]
fn input_delay_shift() {
    for seed in 0..60 {
        let n = circuit(seed, "", false);
        let a = sta(&n, &sdc(3.0, 0.4));
        let b = sta(&n, &sdc(3.0, 0.9));
        let (pa, pb) = (by_pin(&a), by_pin(&b));
        for (p, (s, _)) in &pa {
            if let (Some(x), Some(y)) = (s, pb[p].0) {
                assert!(
                    y <= x + 1e-9 && y >= x - 0.5 - 1e-9,
                    "seed {seed} {p}: {x} -> {y}"
                );
            }
        }
        // Where the worst path starts at an input in both runs, the shift is exact.
        let from_input = |r: &ruststa::Report, e: &str| {
            r.paths
                .iter()
                .find(|p| p.kind == "setup" && p.endpoint == e)
                .map(|p| p.startpoint == "i0" || p.startpoint == "i1")
        };
        for path in b.paths.iter().filter(|p| p.kind == "setup") {
            if from_input(&b, &path.endpoint) == Some(true)
                && from_input(&a, &path.endpoint) == Some(true)
            {
                let x = pa[&path.endpoint].0.unwrap_or(f64::NAN);
                assert!(
                    (path.slack - (x - 0.5)).abs() < 1e-9,
                    "seed {seed} {}",
                    path.endpoint
                );
            }
        }
    }
}

// STA-REQ-0503: renaming instances and permuting statements changes no slack.
#[test]
fn renaming_and_order() {
    for seed in 0..60 {
        let a = sta(&circuit(seed, "", false), &sdc(3.0, 0.4));
        let b = sta(&circuit(seed, "x_", true), &sdc(3.0, 0.4));
        let pb = by_pin(&b);
        for (p, v) in by_pin(&a) {
            let q = if p.contains('/') {
                format!("x_{p}")
            } else {
                p.clone()
            };
            let w = pb.get(&q).copied().unwrap_or((None, None));
            let same = |x: Option<f64>, y: Option<f64>| match (x, y) {
                (Some(x), Some(y)) => (x - y).abs() < 1e-12,
                (None, None) => true,
                _ => false,
            };
            assert!(
                same(v.0, w.0) && same(v.1, w.1),
                "seed {seed} {p}: {v:?} vs {w:?}"
            );
        }
    }
}

// STA-REQ-0502: every endpoint arrival equals the largest arrival over all paths enumerated one
// by one (delays evaluated with the propagated transitions), and the checker agrees.
#[test]
fn exhaustive_paths() {
    use ruststa::prop::{LATE, arc_eval, arc_outs};
    for seed in 0..80 {
        let n = circuit(seed, "", false);
        let s = sdc(3.0, 0.4);
        let opts = ruststa::StaOptions {
            netlist: ruststa::Source::Texts(vec![("n.v".into(), n.clone())]),
            top: None,
            sdc: s.clone(),
            paths: 0,
            path_to: vec![],
            backend: ruststa::Backend::Cpu,
            sdc_name: String::new(),
        };
        let a = ruststa::run(&opts, &[lib()]).expect("analysis");
        let (g, m, st, v) = (&a.graph, &a.model, &a.setup, &a.values);
        // Enumerate every path backwards from each endpoint of each tag and transition.
        fn walk(
            g: &ruststa::graph::Graph,
            m: &ruststa::model::Model,
            st: &ruststa::prop::Setup,
            v: &ruststa::prop::Values,
            node: u32,
            tag: usize,
            tr: usize,
            acc: f64,
            best: &mut f64,
        ) {
            let i = v.idx(node, tag, tr, LATE);
            if st.is_start[node as usize] && st.init.arr[i].is_finite() {
                *best = best.max(acc + st.init.arr[i]);
            }
            for &ai in g.incoming(node) {
                let u = g.arcs[ai as usize].from;
                for ti in 0..2 {
                    if !v.present(u, tag, ti) || !arc_outs(g, m, ai, ti)[tr] {
                        continue;
                    }
                    let (d, _) = arc_eval(
                        g,
                        m,
                        st,
                        ai,
                        node,
                        tr,
                        LATE,
                        v.slew[v.idx(u, tag, ti, LATE)],
                    );
                    walk(g, m, st, v, u, tag, ti, acc + d, best);
                }
            }
        }
        for c in &g.checks {
            for tag in 0..v.ntags {
                for tr in 0..2 {
                    if !v.present(c.data, tag, tr) {
                        continue;
                    }
                    let mut best = f64::NEG_INFINITY;
                    walk(g, m, st, v, c.data, tag, tr, 0.0, &mut best);
                    let got = v.arr[v.idx(c.data, tag, tr, LATE)];
                    assert!((best - got).abs() < 1e-9, "seed {seed}: {best} vs {got}");
                }
            }
        }
        let r = sta(&n, &s);
        let c = check(&n, &s, &r);
        assert_eq!(c.verdict, "agree", "seed {seed}: {:?}", c.differences);
    }
}

// STA-REQ-0504: the checker reports a difference for an altered report, an altered library,
// and a removed arc.
#[test]
fn planted_defects() {
    for seed in 0..20 {
        let n = circuit(seed, "", false);
        let s = sdc(3.0, 0.4);
        let r = sta(&n, &s);
        assert_eq!(check(&n, &s, &r).verdict, "agree");
        // Altered report.
        let mut bad = r.clone();
        if let Some(e) = bad.endpoints.iter_mut().find(|e| e.setup_slack.is_some()) {
            e.setup_slack = e.setup_slack.map(|x| x + 0.01);
        }
        assert_eq!(
            check(&n, &s, &bad).verdict,
            "disagree",
            "seed {seed}: altered slack"
        );
        // A removed arc: the output buffer bypassed in the analyzed netlist.
        let cut = n
            .replace("BUF ob (", "BUF ob_unused (")
            .replace(".Y(o0)", ".Y(o0_unused)");
        let cut = cut.replace("endmodule", " assign o0 = q0;\nendmodule");
        let r2 = sta(&cut, &s);
        assert_eq!(
            check(&n, &s, &r2).verdict,
            "disagree",
            "seed {seed}: removed arc"
        );
    }
    // Altered library: the same report checked against a slower inverter.
    let n = "module m (input clk, input a, output q);\n wire n1, n2;\n\
             DFF r1 (.D(a), .CK(clk), .Q(n1));\n INV u (.A(n1), .Y(n2));\n\
             DFF r2 (.D(n2), .CK(clk), .Q(q));\nendmodule\n";
    let s = sdc(3.0, 0.4).replace("i0 i1", "a").replace("o0", "q");
    let r = sta(n, &s);
    let slow = linear_library().replacen("cell (INV)", "cell (INV_OLD)", 1).replacen(
        "cell (BUF)",
        "cell (INV) {\n    area : 1;\n    pin (A) { direction : input; capacitance : 0.01; }\n    \
         pin (Y) {\n      direction : output;\n      function : \"!A\";\n      timing () {\n        \
         related_pin : \"A\";\n        timing_sense : negative_unate;\n        cell_rise (scalar) { values (\"0.9\"); }\n        \
         cell_fall (scalar) { values (\"0.9\"); }\n      }\n    }\n  }\n  cell (BUF)",
        1,
    );
    let slow = rusteda_formats::liberty::read_text(&slow, "slow.lib").expect("slow lib");
    let opts = ruststa_check::CheckStaOptions {
        netlist: ruststa_check::Netlist::Texts(vec![("n.v".into(), n.into())]),
        top: None,
        sdc: s.clone(),
        tolerance: 1e-9,
    };
    let c = ruststa_check::check(&opts, &[slow], &serde_json::to_value(&r).expect("json"));
    assert_eq!(c.verdict, "disagree", "altered library");
}
