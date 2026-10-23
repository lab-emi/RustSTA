// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Suite `sta-known-answer` (STA-REQ-0501): slacks derived by hand from linear cell models
//! (`verification/analytic/sta/*/DERIVATION.md`), compared with RustSTA and the independent
//! checker.

mod sta_common;

use sta_common::*;

const EPS: f64 = 1e-9;

fn close(got: Option<f64>, want: f64, what: &str) {
    let g = got.unwrap_or_else(|| panic!("{what}: no slack reported"));
    assert!((g - want).abs() < EPS, "{what}: got {g}, want {want}");
}

fn agree(n: &str, s: &str, r: &ruststa::Report) {
    let c = check(n, s, r);
    assert_eq!(
        c.verdict, "agree",
        "checker: {:?} {:?}",
        c.differences, c.errors
    );
}

/// Clock-to-Q arrival (rise, fall) and transition at load `c` with clock transition 0.
fn launch(c: f64) -> ((f64, f64), (f64, f64)) {
    (
        (CKQ_R.delay(0.0, c), CKQ_R.slew(c)),
        (CKQ_F.delay(0.0, c), CKQ_F.slew(c)),
    )
}

// STA-REQ-0110 to -0115, -0201, -0210 to -0212: a two-inverter chain between flip-flops, input
// and output delays.
#[test]
fn inverter_chain() {
    let n = "module m (input clk, input d, output q);\n wire n1, n2, n3;\n\
             DFF r1 (.D(d), .CK(clk), .Q(n1));\n INV u1 (.A(n1), .Y(n2));\n\
             INV u2 (.A(n2), .Y(n3));\n DFF r2 (.D(n3), .CK(clk), .Q(q));\nendmodule\n";
    let s = "create_clock -name clk -period 2 [get_ports clk]\n\
             set_input_delay -clock clk 0.5 [get_ports d]\n\
             set_output_delay -clock clk 0.3 [get_ports q]\nset_load 0.02 [get_ports q]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(CIN);
    // u1 inverts: output fall from input rise and output rise from input fall.
    let (n2f, n2f_s) = (qr + INV_F.delay(sr, CIN), INV_F.slew(CIN));
    let (n2r, n2r_s) = (qf + INV_R.delay(sf, CIN), INV_R.slew(CIN));
    let n3r = n2f + INV_R.delay(n2f_s, CIN);
    let n3f = n2r + INV_F.delay(n2r_s, CIN);
    let (setup, hold) = slack(&r, "r2/D");
    close(setup, 2.0 - SETUP - n3r.max(n3f), "r2/D setup");
    close(hold, n3r.min(n3f) - HOLD, "r2/D hold");
    let (setup, hold) = slack(&r, "r1/D");
    close(setup, 2.0 - SETUP - 0.5, "r1/D setup");
    close(hold, 0.5 - HOLD, "r1/D hold");
    let ((oqr, _), (oqf, _)) = launch(0.02);
    let (setup, hold) = slack(&r, "q");
    close(setup, 2.0 - 0.3 - oqr.max(oqf), "q setup");
    close(hold, oqr.min(oqf) - (0.0 - 0.3), "q hold");
    agree(n, s, &r);
}

// STA-REQ-0112, -0115: reconvergence through a buffer and a non-unate XOR.
#[test]
fn reconvergent_xor() {
    let n = "module m (input clk, input a, output q);\n wire n1, n2, n3;\n\
             DFF r1 (.D(a), .CK(clk), .Q(n1));\n BUF b1 (.A(n1), .Y(n2));\n\
             XOR2 x (.A(n1), .B(n2), .Y(n3));\n DFF r2 (.D(n3), .CK(clk), .Q(q));\nendmodule\n";
    let s = "create_clock -name clk -period 2 [get_ports clk]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(2.0 * CIN);
    let (br, br_s) = (qr + BUF_R.delay(sr, CIN), BUF_R.slew(CIN));
    let (bf, bf_s) = (qf + BUF_F.delay(sf, CIN), BUF_F.slew(CIN));
    // Every input transition of the XOR reaches both output transitions.
    let ins = [(qr, sr), (qf, sf), (br, br_s), (bf, bf_s)];
    let late = ins
        .iter()
        .flat_map(|(a, s)| [a + XOR_R.delay(*s, CIN), a + XOR_F.delay(*s, CIN)])
        .fold(f64::MIN, f64::max);
    let early = ins
        .iter()
        .flat_map(|(a, s)| [a + XOR_R.delay(*s, CIN), a + XOR_F.delay(*s, CIN)])
        .fold(f64::MAX, f64::min);
    let (setup, hold) = slack(&r, "r2/D");
    close(setup, 2.0 - SETUP - late, "r2/D setup");
    close(hold, early - HOLD, "r2/D hold");
    agree(n, s, &r);
}

// STA-REQ-0210, -0211: capture on the falling edge of a waveform with a 30 % duty cycle.
#[test]
fn falling_edge_capture() {
    let n = "module m (input clk, input a, output q);\n wire n1, n2;\n\
             DFF r1 (.D(a), .CK(clk), .Q(n1));\n INV u (.A(n1), .Y(n2));\n\
             DFFN r2 (.D(n2), .CKN(clk), .Q(q));\nendmodule\n";
    let s = "create_clock -name clk -period 2 -waveform {0 0.6} [get_ports clk]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(CIN);
    let late = (qr + INV_F.delay(sr, CIN)).max(qf + INV_R.delay(sf, CIN));
    let early = (qr + INV_F.delay(sr, CIN)).min(qf + INV_R.delay(sf, CIN));
    // Rise at 0 is captured by the fall at 0.6; hold is checked against the fall at -1.4.
    let (setup, hold) = slack(&r, "r2/D");
    close(setup, 0.6 - SETUP - late, "setup");
    close(hold, early - (0.6 - 2.0 + HOLD), "hold");
    agree(n, s, &r);
}

// STA-REQ-0213: a positive latch that borrows time and passes it on.
#[test]
fn latch_borrowing() {
    let n = "module m (input clk, input a, output q);\n wire n1, n2, n3, n4, n5;\n\
             DFF r1 (.D(a), .CK(clk), .Q(n1));\n BUF b1 (.A(n1), .Y(n2));\n\
             BUF b2 (.A(n2), .Y(n3));\n LAT l (.D(n3), .G(clk), .Q(n4));\n\
             INV u (.A(n4), .Y(n5));\n DFF r2 (.D(n5), .CK(clk), .Q(q));\nendmodule\n";
    let s = "create_clock -name clk -period 2 [get_ports clk]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(CIN);
    let (ar, as_r) = (qr + BUF_R.delay(sr, CIN), BUF_R.slew(CIN));
    let (af, as_f) = (qf + BUF_F.delay(sf, CIN), BUF_F.slew(CIN));
    let (dr, ds_r) = (ar + BUF_R.delay(as_r, CIN), BUF_R.slew(CIN));
    let (df, ds_f) = (af + BUF_F.delay(as_f, CIN), BUF_F.slew(CIN));
    // The latch is open from the rise at 0 to the fall at 1: the data arrives while it is open,
    // so its setup slack is 0 (time borrowed) as long as it meets the closing check.
    assert!(dr.max(df) > 0.0 && dr.max(df) < 1.0 - SETUP);
    let (setup, _) = slack(&r, "l/D");
    close(setup, 0.0, "latch D setup");
    // Q: the later of the latch's own launch at the opening edge and the borrowed data.
    let ((lr, lrs), (lf, lfs)) = launch(CIN);
    let q_r = (dr + DQ_R.delay(ds_r, CIN)).max(lr);
    let q_f = (df + DQ_F.delay(ds_f, CIN)).max(lf);
    let q_r_s = DQ_R.slew(CIN).max(lrs);
    let q_f_s = DQ_F.slew(CIN).max(lfs);
    let late = (q_r + INV_F.delay(q_r_s, CIN)).max(q_f + INV_R.delay(q_f_s, CIN));
    let (setup, _) = slack(&r, "r2/D");
    close(setup, 2.0 - SETUP - late, "r2/D setup");
    agree(n, s, &r);
}

// STA-REQ-0210: two clocks with periods 3 and 2 (common period 6).
#[test]
fn two_clocks() {
    let n = "module m (input ca, input cb, input a, output q);\n wire n1, n2;\n\
             DFF r1 (.D(a), .CK(ca), .Q(n1));\n BUF b (.A(n1), .Y(n2));\n\
             DFF r2 (.D(n2), .CK(cb), .Q(q));\nendmodule\n";
    let s = "create_clock -name A -period 3 [get_ports ca]\n\
             create_clock -name B -period 2 [get_ports cb]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(CIN);
    let late = (qr + BUF_R.delay(sr, CIN)).max(qf + BUF_F.delay(sf, CIN));
    let early = (qr + BUF_R.delay(sr, CIN)).min(qf + BUF_F.delay(sf, CIN));
    // Launches at 0 and 3 against captures every 2: the tightest setup relation is 4 - 3 = 1,
    // and hold is checked one capture period earlier (1 - 2 = -1).
    let (setup, hold) = slack(&r, "r2/D");
    close(setup, 1.0 - SETUP - late, "setup");
    close(hold, early - (-1.0 + HOLD), "hold");
    agree(n, s, &r);
}

// STA-REQ-0216: an AND gate gating the clock with an enable launched by a flip-flop on the
// same edge: setup against the next rise, hold against the fall that ends the current pulse.
#[test]
fn clock_gating() {
    let n = "module m (input clk, input a, input d, output q);\n wire e, e2, g;\n\
             DFF r1 (.D(a), .CK(clk), .Q(e));\n BUF b (.A(e), .Y(e2));\n\
             AND2 c (.A(clk), .B(e2), .Y(g));\n DFF r2 (.D(d), .CK(g), .Q(q));\nendmodule\n";
    let s = "create_clock -name clk -period 2 [get_ports clk]\n";
    let r = sta(n, s);
    let ((qr, sr), (qf, sf)) = launch(CIN);
    let late = (qr + BUF_R.delay(sr, CIN)).max(qf + BUF_F.delay(sf, CIN));
    let early = (qr + BUF_R.delay(sr, CIN)).min(qf + BUF_F.delay(sf, CIN));
    let (setup, hold) = slack(&r, "c/B");
    close(setup, 2.0 - late, "gating setup");
    close(hold, early - 1.0, "gating hold");
    agree(n, s, &r);
}
