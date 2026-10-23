// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing checks, slacks and path reconstruction (STA-REQ-0210 to STA-REQ-0215).

use rustc_hash::FxHashMap;

use crate::constraints::Constraints;
use crate::graph::{Graph, NodeKind};
use crate::model::{CheckKind, Model, Tr};
use crate::prop::{EARLY, LATE, Setup, Values, arc_eval, arc_outs, source_edges};
use crate::report::{PathPoint, PathReport};

/// Setup relation `D_s` between a launch edge of clock `l` and a capture edge of clock `c`
/// (STA-REQ-0210).
pub fn setup_relation(
    k: &Constraints,
    l: u32,
    el: usize,
    c: u32,
    ec: usize,
) -> Result<f64, String> {
    edge_relation(k, l, el, c, ec, true)
}

/// Smallest difference from a launch edge to a later (`strict`) or not earlier capture edge.
pub fn edge_relation(
    k: &Constraints,
    l: u32,
    el: usize,
    c: u32,
    ec: usize,
    strict: bool,
) -> Result<f64, String> {
    let (lc, cc) = (&k.clocks[l as usize], &k.clocks[c as usize]);
    // Integer femtoseconds for an exact common period.
    let fs = |x: f64| (x * 1e6).round() as i128;
    let (tl, tc) = (fs(lc.period), fs(cc.period));
    let gcd = |mut a: i128, mut b: i128| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let lcm = tl / gcd(tl, tc) * tc;
    if lcm > 1000 * tl.max(tc) {
        return Err(format!(
            "clocks {} and {} have no common period within 1000 cycles (STA-REQ-0210)",
            lc.name, cc.name
        ));
    }
    let (el_t, ec_t) = (fs(lc.edges[el]), fs(cc.edges[ec]));
    let mut best = i128::MAX;
    let mut i = 0;
    while i * tl < lcm {
        let launch = el_t + i * tl;
        // Smallest capture edge after (or, when not strict, at) the launch.
        let mut k0 = (launch - ec_t).div_euclid(tc);
        if strict || ec_t + k0 * tc < launch {
            k0 += 1;
        }
        let cap = ec_t + k0 * tc;
        best = best.min(cap - launch);
        i += 1;
    }
    Ok(best as f64 / 1e6)
}

/// Worst slack of one endpoint and check kind, with what produced it.
#[derive(Debug, Clone, Copy)]
pub struct Slack {
    pub slack: f64,
    pub tag: usize,
    pub tr: Tr,
    pub capture: u32,
    pub arrival: f64,
    pub required: f64,
}

/// Endpoint results.
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    /// Per endpoint node: worst setup and hold slack.
    pub setup: FxHashMap<u32, Slack>,
    pub hold: FxHashMap<u32, Slack>,
    pub unconstrained: usize,
}

fn keep(map: &mut FxHashMap<u32, Slack>, v: u32, s: Slack) {
    match map.get(&v) {
        Some(o) if o.slack <= s.slack => {}
        _ => {
            map.insert(v, s);
        }
    }
}

/// Computes every check (STA-REQ-0211 to STA-REQ-0214).
pub fn check_all(
    g: &Graph,
    m: &Model,
    k: &Constraints,
    s: &Setup,
    val: &Values,
) -> Result<Endpoints, String> {
    let mut ep = Endpoints::default();
    let mut rel: FxHashMap<(u32, usize, u32, usize), f64> = FxHashMap::default();
    let mut relation = |l: u32, el: usize, c: u32, ec: usize| -> Result<f64, String> {
        if let Some(r) = rel.get(&(l, el, c, ec)) {
            return Ok(*r);
        }
        let r = setup_relation(k, l, el, c, ec)?;
        rel.insert((l, el, c, ec), r);
        Ok(r)
    };
    for ch in &g.checks {
        let cell = &m.cells[g.inst_cell[ch.inst as usize] as usize];
        let cc = &cell.checks[ch.check as usize];
        // For each check of the cell: the opening transition at the clock pin if it is the setup
        // check of a latch.
        let latch_open: Vec<Option<usize>> = (0..cell.checks.len())
            .map(|ci| {
                let chk = &cell.checks[ci];
                cell.arcs.iter().find_map(|a| match a.kind {
                    crate::model::ArcKind::Edge(e)
                        if a.from == chk.clock
                            && cell.arcs.iter().any(|d| d.latch_check == Some(ci)) =>
                    {
                        Some(e)
                    }
                    _ => None,
                })
            })
            .collect();
        let senses = &s.clock_sense[ch.clock as usize];
        if senses.is_empty() {
            ep.unconstrained += 1;
            continue;
        }
        for &(c, bits) in senses {
            let ck = &k.clocks[c as usize];
            for ec in source_edges(cc.edge, bits) {
                for t in 0..s.ntags {
                    let (l, el) = ((t / 2) as u32, t % 2);
                    for x in 0..2 {
                        if !val.present(ch.data, t, x) || cc.table[x] == crate::model::NO_TABLE {
                            continue;
                        }
                        let ds = relation(l, el, c, ec)?;
                        match cc.kind {
                            CheckKind::Setup => {
                                let arr = val.arr[val.idx(ch.data, t, x, LATE)];
                                let sl = val.slew[val.idx(ch.data, t, x, LATE)];
                                let st = m.eval(cc.table[x], ck.transition, sl);
                                let req = ds + ck.latency[EARLY] - st - ck.uncertainty_setup;
                                let (mut slack, mut required) = (req - arr, req);
                                // Latch data: time-borrowing convention (STA-REQ-0213).
                                if let Some(open_edge) = latch_open[ch.check as usize] {
                                    // Same sense as the capture edge: positive exactly when the
                                    // capture source edge equals the check's pin transition.
                                    let open_src = if ec == cc.edge {
                                        open_edge
                                    } else {
                                        1 - open_edge
                                    };
                                    let mut o = relation(l, el, c, open_src)?;
                                    if o > ds {
                                        o -= ck.period;
                                    }
                                    let open = o + ck.latency[EARLY];
                                    if arr <= open {
                                        slack = open - arr;
                                        required = open;
                                    } else if arr <= req {
                                        slack = 0.0;
                                        required = arr;
                                    }
                                }
                                keep(
                                    &mut ep.setup,
                                    ch.data,
                                    Slack {
                                        slack,
                                        tag: t,
                                        tr: x,
                                        capture: c,
                                        arrival: arr,
                                        required,
                                    },
                                );
                            }
                            CheckKind::Hold => {
                                let arr = val.arr[val.idx(ch.data, t, x, EARLY)];
                                let sl = val.slew[val.idx(ch.data, t, x, EARLY)];
                                let ht = m.eval(cc.table[x], ck.transition, sl);
                                let dh = ds - ck.period;
                                let req = dh + ck.latency[LATE] + ht + ck.uncertainty_hold;
                                keep(
                                    &mut ep.hold,
                                    ch.data,
                                    Slack {
                                        slack: arr - req,
                                        tag: t,
                                        tr: x,
                                        capture: c,
                                        arrival: arr,
                                        required: req,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    // Clock-gating checks (STA-REQ-0216).
    let mut pin_node: FxHashMap<(u32, u32), u32> = FxHashMap::default();
    for (v, n) in g.nodes.iter().enumerate() {
        if let NodeKind::Pin { inst, pin } = n.kind {
            pin_node.insert((inst, pin), v as u32);
        }
    }
    for (ii, &ci) in g.inst_cell.iter().enumerate() {
        let cell = &m.cells[ci as usize];
        for out in &cell.pins {
            let Some((ins, tt)) = &out.func else { continue };
            let nv = ins.len();
            let node = |p: usize| pin_node.get(&(ii as u32, p as u32)).copied();
            for (kpos, &kpin) in ins.iter().enumerate() {
                let Some(ck_node) = node(kpin) else { continue };
                let senses = &s.clock_sense[ck_node as usize];
                if senses.is_empty() {
                    continue;
                }
                // Value of the clock pin at which the output does not depend on the others.
                let constant_at = |v: u64| {
                    let vals: Vec<u64> = (0..1u64 << nv)
                        .filter(|mt| (mt >> kpos) & 1 == v)
                        .map(|mt| (tt >> mt) & 1)
                        .collect();
                    vals.iter().all(|x| *x == vals[0])
                };
                let inactive = match (constant_at(0), constant_at(1)) {
                    (true, false) => 0usize,
                    (false, true) => 1usize,
                    _ => continue,
                };
                // Pin transitions leaving and returning to the inactive value.
                let (lead_pin, trail_pin) = if inactive == 0 {
                    (0usize, 1usize)
                } else {
                    (1, 0)
                };
                for (jpos, &jpin) in ins.iter().enumerate() {
                    if jpos == kpos {
                        continue;
                    }
                    let Some(en) = node(jpin) else { continue };
                    if !s.clock_sense[en as usize].is_empty() {
                        continue;
                    }
                    for &(c, bits) in senses {
                        let ck = &k.clocks[c as usize];
                        for sense_neg in [false, true] {
                            if bits & (1 << u8::from(sense_neg)) == 0 {
                                continue;
                            }
                            let src = |pin_tr: usize| if sense_neg { 1 - pin_tr } else { pin_tr };
                            for t in 0..s.ntags {
                                let (l, el) = ((t / 2) as u32, t % 2);
                                for x in 0..2 {
                                    if !val.present(en, t, x) {
                                        continue;
                                    }
                                    let ds = edge_relation(k, l, el, c, src(lead_pin), true)?;
                                    let dh = edge_relation(k, l, el, c, src(trail_pin), false)?;
                                    let arr_l = val.arr[val.idx(en, t, x, LATE)];
                                    let req_s = ds + ck.latency[EARLY] - ck.uncertainty_setup;
                                    keep(
                                        &mut ep.setup,
                                        en,
                                        Slack {
                                            slack: req_s - arr_l,
                                            tag: t,
                                            tr: x,
                                            capture: c,
                                            arrival: arr_l,
                                            required: req_s,
                                        },
                                    );
                                    let arr_e = val.arr[val.idx(en, t, x, EARLY)];
                                    let req_h = dh + ck.latency[LATE] + ck.uncertainty_hold;
                                    keep(
                                        &mut ep.hold,
                                        en,
                                        Slack {
                                            slack: arr_e - req_h,
                                            tag: t,
                                            tr: x,
                                            capture: c,
                                            arrival: arr_e,
                                            required: req_h,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Output delays (STA-REQ-0212).
    let mut constrained_out = rustc_hash::FxHashSet::default();
    for d in &k.output_delays {
        constrained_out.insert(d.node);
        let ck = &k.clocks[d.clock as usize];
        for t in 0..s.ntags {
            let (l, el) = ((t / 2) as u32, t % 2);
            for x in 0..2 {
                if !val.present(d.node, t, x) {
                    continue;
                }
                let ds = relation(l, el, d.clock, d.edge)?;
                let arr_l = val.arr[val.idx(d.node, t, x, LATE)];
                let req_s = ds + ck.latency[EARLY] - d.value[LATE] - ck.uncertainty_setup;
                keep(
                    &mut ep.setup,
                    d.node,
                    Slack {
                        slack: req_s - arr_l,
                        tag: t,
                        tr: x,
                        capture: d.clock,
                        arrival: arr_l,
                        required: req_s,
                    },
                );
                let arr_e = val.arr[val.idx(d.node, t, x, EARLY)];
                let req_h =
                    ds - ck.period + ck.latency[LATE] - d.value[EARLY] + ck.uncertainty_hold;
                keep(
                    &mut ep.hold,
                    d.node,
                    Slack {
                        slack: arr_e - req_h,
                        tag: t,
                        tr: x,
                        capture: d.clock,
                        arrival: arr_e,
                        required: req_h,
                    },
                );
            }
        }
    }
    ep.unconstrained += g
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, n)| {
            matches!(n.kind, NodeKind::PortOut(_)) && !constrained_out.contains(&(*i as u32))
        })
        .count();
    Ok(ep)
}

/// Reconstructs the path behind an endpoint slack (STA-REQ-0215): at each node, the incoming arc
/// and source transition whose arrival is the worst (the first in arc order on ties), until the
/// startpoint's own value is at least as bad as every incoming one.
pub fn path(
    g: &Graph,
    m: &Model,
    k: &Constraints,
    s: &Setup,
    val: &Values,
    end: u32,
    sl: &Slack,
    an: usize,
) -> PathReport {
    // Worse means later for setup and earlier for hold.
    let worse = |a: f64, b: f64| if an == LATE { a > b } else { a < b };
    let mut points = Vec::new();
    let (mut v, mut x) = (end, sl.tr);
    let t = sl.tag;
    let mut guard = 0usize;
    loop {
        let i = val.idx(v, t, x, an);
        let (arr, slew) = (val.arr[i], val.slew[i]);
        let cell_of = |v: u32| match g.nodes[v as usize].kind {
            NodeKind::Pin { inst, .. } => {
                Some(m.cells[g.inst_cell[inst as usize] as usize].name.clone())
            }
            _ => None,
        };
        points.push(PathPoint {
            pin: g.nodes[v as usize].name.clone(),
            cell: cell_of(v),
            transition: if x == 0 { "rise" } else { "fall" }.into(),
            incr: 0.0,
            arrival: arr,
            transition_time: slew,
        });
        guard += 1;
        if guard > g.nodes.len() || matches!(g.nodes[v as usize].kind, NodeKind::PortIn(_)) {
            break;
        }
        let mut best: Option<(u32, usize, f64, f64)> = None;
        for &ai in g.incoming(v) {
            let u = g.arcs[ai as usize].from;
            for ti in 0..2 {
                if !val.present(u, t, ti) || !arc_outs(g, m, ai, ti)[x] {
                    continue;
                }
                let iu = val.idx(u, t, ti, an);
                let mut a_in = val.arr[iu];
                if an == LATE {
                    a_in = a_in.min(s.late_clip(m, ai, t, ti, val.slew[iu]));
                } else if a_in == f64::INFINITY || a_in < s.clip_early[ai as usize * s.ntags + t] {
                    continue;
                }
                let (d, _) = arc_eval(g, m, s, ai, v, x, an, val.slew[iu]);
                if best.is_none_or(|b| worse(a_in + d, b.3)) {
                    best = Some((u, ti, d, a_in + d));
                }
            }
        }
        // The startpoint's own launch value wins ties.
        let own = s.init.arr[i];
        let Some((u, ti, d, a)) = best else { break };
        if s.is_start[v as usize] && own.is_finite() && !worse(a, own) {
            break;
        }
        if let Some(p) = points.last_mut() {
            p.incr = d;
        }
        v = u;
        x = ti;
    }
    points.reverse();
    // The first point's increment is its own arrival (launch delay or input delay).
    if let Some(p) = points.first_mut() {
        p.incr = p.arrival;
    }
    let lc = &k.clocks[t / 2];
    let cc = &k.clocks[sl.capture as usize];
    PathReport {
        kind: if an == LATE { "setup" } else { "hold" }.into(),
        startpoint: points.first().map(|p| p.pin.clone()).unwrap_or_default(),
        endpoint: g.nodes[end as usize].name.clone(),
        launch_clock: format!(
            "{} {}",
            lc.name,
            if t.is_multiple_of(2) { "rise" } else { "fall" }
        ),
        capture_clock: cc.name.clone(),
        arrival: sl.arrival,
        required: sl.required,
        slack: sl.slack,
        points,
    }
}
