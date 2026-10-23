// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Endpoint slacks for the checker (STA-REQ-0210 to STA-REQ-0216).

use std::collections::HashMap;

use crate::sdc::Sdc;
use crate::timing::Timing;

/// Smallest difference from a launch edge to a capture edge after it (or at it when not
/// strict), found by listing every edge of both clocks over their common period.
pub fn gap(s: &Sdc, l: usize, el: usize, c: usize, ec: usize, strict: bool) -> Result<f64, String> {
    let (a, b) = (&s.clocks[l], &s.clocks[c]);
    let q = |x: f64| (x * 1e6).round() as i128;
    let (pa, pb) = (q(a.period), q(b.period));
    let mut h = pa.max(pb);
    while h % pa != 0 || h % pb != 0 {
        h += pa.max(pb);
        if h > 1000 * pa.max(pb) {
            return Err(format!(
                "clocks {} and {} have no common period",
                a.name, b.name
            ));
        }
    }
    let te = |ck: &crate::sdc::Clk, e: usize| q(if e == 0 { ck.rise } else { ck.fall });
    let launches: Vec<i128> = (0..h / pa).map(|i| te(a, el) + i * pa).collect();
    let captures: Vec<i128> = (-1..=(h / pb) + 1).map(|k| te(b, ec) + k * pb).collect();
    let mut best = i128::MAX;
    for &x in &launches {
        for &y in &captures {
            let d = y - x;
            if (strict && d > 0) || (!strict && d >= 0) {
                best = best.min(d);
            }
        }
    }
    Ok(best as f64 / 1e6)
}

/// Worst setup and hold slack per endpoint name.
#[derive(Debug, Default)]
pub struct Slacks {
    pub setup: HashMap<String, f64>,
    pub hold: HashMap<String, f64>,
}

fn worst(m: &mut HashMap<String, f64>, k: &str, v: f64) {
    let e = m.entry(k.to_string()).or_insert(f64::INFINITY);
    if v < *e {
        *e = v;
    }
}

pub fn compute(t: &mut Timing) -> Result<Slacks, String> {
    let mut out = Slacks::default();
    let sdc = t.sdc;
    let n = t.names.len();
    for p in 0..n {
        let Some((ii, k)) = t.owner[p] else { continue };
        let ct = t.cell_of(ii);
        let iname = t.insts[ii].0.clone();
        // Register and latch checks at data pin `p`.
        for (ci, chk) in ct.checks.iter().enumerate() {
            if chk.data != k {
                continue;
            }
            let Some(&ckp) = t.index.get(&format!("{iname}/{}", ct.pins[chk.clock])) else {
                continue;
            };
            let senses = t.senses(ckp);
            let vals = t.vals(p);
            let latch_open = ct.arcs.iter().any(|a| a.latch == Some(ci)).then(|| {
                ct.arcs
                    .iter()
                    .find(|e| e.edge.is_some() && e.from == chk.clock)
                    .and_then(|e| e.edge)
            });
            for (c, bits) in senses.iter().enumerate() {
                let ck = &sdc.clocks[c];
                for (bit, flip) in [(1u8, false), (2u8, true)] {
                    if bits & bit == 0 {
                        continue;
                    }
                    let map = |e: usize| if flip { 1 - e } else { e };
                    for (&(tag, x), v) in &vals {
                        let Some(tb) = &chk.table[x] else { continue };
                        let (l, el) = (tag / 2, tag % 2);
                        let ds = gap(sdc, l, el, c, map(chk.edge), true)?;
                        if chk.setup {
                            let (arr, sl) = v[1];
                            if arr == f64::NEG_INFINITY {
                                continue;
                            }
                            let req = ds + ck.latency[0] - tb.at(ck.transition, sl) - ck.setup_unc;
                            let mut slack = req - arr;
                            if let Some(Some(op)) = latch_open {
                                let mut o = gap(sdc, l, el, c, map(op), true)?;
                                if o > ds {
                                    o -= ck.period;
                                }
                                let open = o + ck.latency[0];
                                if arr <= open {
                                    slack = open - arr;
                                } else if arr <= req {
                                    slack = 0.0;
                                }
                            }
                            worst(&mut out.setup, &t.names[p], slack);
                        } else {
                            let (arr, sl) = v[0];
                            if arr == f64::INFINITY {
                                continue;
                            }
                            let req = ds - ck.period
                                + ck.latency[1]
                                + tb.at(ck.transition, sl)
                                + ck.hold_unc;
                            worst(&mut out.hold, &t.names[p], arr - req);
                        }
                    }
                }
            }
        }
        // Clock-gating checks where `p` is the enable of a gating cell (STA-REQ-0216).
        if !ct.is_input[k] || t.senses(p).iter().any(|b| *b != 0) {
            continue;
        }
        for (_, (ins, tt)) in ct.funcs.clone() {
            let Some(jpos) = ins.iter().position(|x| *x == k) else {
                continue;
            };
            for (kpos, &kpin) in ins.iter().enumerate() {
                if kpos == jpos {
                    continue;
                }
                let Some(&ckp) = t.index.get(&format!("{iname}/{}", ct.pins[kpin])) else {
                    continue;
                };
                let senses = t.senses(ckp);
                if senses.iter().all(|b| *b == 0) {
                    continue;
                }
                let cst = |v: u64| {
                    let mut seen = None;
                    for m in 0u64..(1 << ins.len()) {
                        if (m >> kpos) & 1 != v {
                            continue;
                        }
                        let o = (tt >> m) & 1;
                        if seen.is_some_and(|s| s != o) {
                            return false;
                        }
                        seen = Some(o);
                    }
                    true
                };
                let idle = match (cst(0), cst(1)) {
                    (true, false) => 0usize,
                    (false, true) => 1,
                    _ => continue,
                };
                let (lead, trail) = (1 - idle, idle);
                let vals = t.vals(p);
                for (c, bits) in senses.iter().enumerate() {
                    let ck = &sdc.clocks[c];
                    for (bit, flip) in [(1u8, false), (2u8, true)] {
                        if bits & bit == 0 {
                            continue;
                        }
                        // Pin transition `lead` is a rise when leaving 0.
                        let map = |e: usize| if flip { 1 - e } else { e };
                        let lead_tr = if lead == 1 { 0 } else { 1 };
                        let trail_tr = if trail == 0 { 1 } else { 0 };
                        for (&(tag, _), v) in &vals {
                            let (l, el) = (tag / 2, tag % 2);
                            let ds = gap(sdc, l, el, c, map(lead_tr), true)?;
                            let dh = gap(sdc, l, el, c, map(trail_tr), false)?;
                            if v[1].0 > f64::NEG_INFINITY {
                                worst(
                                    &mut out.setup,
                                    &t.names[p],
                                    ds + ck.latency[0] - ck.setup_unc - v[1].0,
                                );
                            }
                            if v[0].0 < f64::INFINITY {
                                worst(
                                    &mut out.hold,
                                    &t.names[p],
                                    v[0].0 - (dh + ck.latency[1] + ck.hold_unc),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    // Output delays (STA-REQ-0212).
    for d in sdc.outputs.clone() {
        let ck = &sdc.clocks[d.clock];
        let vals = t.vals(d.pin);
        for (&(tag, _), v) in &vals {
            let (l, el) = (tag / 2, tag % 2);
            let ds = gap(sdc, l, el, d.clock, d.edge, true)?;
            if v[1].0 > f64::NEG_INFINITY {
                worst(
                    &mut out.setup,
                    &t.names[d.pin],
                    ds + ck.latency[0] - d.value[1] - ck.setup_unc - v[1].0,
                );
            }
            if v[0].0 < f64::INFINITY {
                worst(
                    &mut out.hold,
                    &t.names[d.pin],
                    v[0].0 - (ds - ck.period + ck.latency[1] - d.value[0] + ck.hold_unc),
                );
            }
        }
    }
    Ok(out)
}
