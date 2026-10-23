// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Demand-driven arrival evaluation for the checker (STA-REQ-0100 to STA-REQ-0115,
//! STA-REQ-0200, STA-REQ-0201, STA-REQ-0213).

use std::collections::HashMap;

use crate::cells::{ArcT, CellTiming};
use crate::design::{Design, ONE, ZERO};
use crate::sdc::Sdc;

/// A fanin edge of a pin: from pin `src`, either a wire or arc `arc` of cell instance `inst`.
#[derive(Debug, Clone, Copy)]
pub struct Fanin {
    pub src: usize,
    pub inst: Option<(usize, usize)>,
}

/// Arrival data of one pin: per (tag, transition): (early, late) arrival and slew.
pub type Vals = HashMap<(usize, usize), [(f64, f64); 2]>;

#[derive(Debug)]
pub struct Timing<'a> {
    pub cells: &'a HashMap<String, CellTiming>,
    pub names: Vec<String>,
    pub index: HashMap<String, usize>,
    /// Per pin: Some((instance, cell pin)) or None for a port.
    pub owner: Vec<Option<(usize, usize)>>,
    pub insts: Vec<(String, String)>,
    pub fanin: Vec<Vec<Fanin>>,
    /// Loads per pin that drives a net: `[tr][early/late]`.
    pub load: Vec<[[f64; 2]; 2]>,
    pub sdc: &'a Sdc,
    pub port_in: Vec<bool>,
    memo: Vec<Option<Vals>>,
    sense_memo: Vec<Option<Vec<u8>>>,
    disabled: std::collections::HashSet<(usize, usize, Option<(usize, usize)>)>,
}

impl<'a> Timing<'a> {
    pub fn cell_of(&self, inst: usize) -> &'a CellTiming {
        &self.cells[&self.insts[inst].1]
    }

    pub fn new(
        d: &mut Design,
        cells: &'a HashMap<String, CellTiming>,
        sdc_text: &str,
        tu: f64,
        cu: f64,
        sdc_holder: &'a mut Option<Sdc>,
    ) -> Result<Timing<'a>, String> {
        let mut names = Vec::new();
        let mut owner = Vec::new();
        let mut net_of = Vec::new();
        let mut port_in = Vec::new();
        for (n, input, key) in d.ports.clone() {
            names.push(n);
            owner.push(None);
            net_of.push(d.net(&key));
            port_in.push(input);
        }
        let mut insts = Vec::new();
        for (ii, (iname, cell, pins)) in d.cells.clone().into_iter().enumerate() {
            let ct = cells
                .get(&cell)
                .ok_or_else(|| format!("unknown cell {cell}"))?;
            for (p, key) in pins {
                let k = ct.pin(&p).ok_or_else(|| format!("{iname}: no pin {p}"))?;
                names.push(format!("{iname}/{p}"));
                owner.push(Some((ii, k)));
                net_of.push(d.net(&key));
                port_in.push(false);
            }
            insts.push((iname, cell));
        }
        let index: HashMap<String, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        let is_driver = |i: usize| match owner[i] {
            None => port_in[i],
            Some((ii, k)) => {
                let ct = &cells[&insts[ii].1];
                ct.is_output[k] && !ct.is_input[k]
            }
        };
        let mut drivers: HashMap<&str, usize> = HashMap::new();
        for i in 0..names.len() {
            if is_driver(i) && net_of[i] != ZERO && net_of[i] != ONE {
                if drivers.insert(net_of[i].as_str(), i).is_some() {
                    return Err(format!("net {} has two drivers", net_of[i]));
                }
            }
        }
        let sdc = Sdc::read(sdc_text, &names, &port_in, owner.as_slice(), tu, cu)?;
        let sdc: &'a Sdc = sdc_holder.insert(sdc);
        let mut fanin: Vec<Vec<Fanin>> = vec![Vec::new(); names.len()];
        let mut load = vec![[[0.0; 2]; 2]; names.len()];
        for i in 0..names.len() {
            if is_driver(i) {
                if let Some(p) = sdc.port_load.get(&i) {
                    for row in load[i].iter_mut() {
                        row[0] += p[0];
                        row[1] += p[1];
                    }
                }
                continue;
            }
            let Some(&drv) = drivers.get(net_of[i].as_str()) else {
                continue;
            };
            fanin[i].push(Fanin {
                src: drv,
                inst: None,
            });
            let c = match owner[i] {
                Some((ii, k)) => cells[&insts[ii].1].cap[k],
                None => [[0.0; 2]; 2],
            };
            let pl = sdc.port_load.get(&i).copied().unwrap_or([0.0, 0.0]);
            for tr in 0..2 {
                load[drv][tr][0] += c[tr][0] + pl[0];
                load[drv][tr][1] += c[tr][1] + pl[1];
            }
        }
        // Cell arcs as fanins of their output pins.
        let mut pin_of: HashMap<(usize, usize), usize> = HashMap::new();
        for (i, o) in owner.iter().enumerate() {
            if let Some(x) = o {
                pin_of.insert(*x, i);
            }
        }
        for (ii, (_, cell)) in insts.iter().enumerate() {
            let ct = &cells[cell];
            for (ai, a) in ct.arcs.iter().enumerate() {
                if a.edge.is_some() {
                    continue;
                }
                if let (Some(&f), Some(&t)) = (pin_of.get(&(ii, a.from)), pin_of.get(&(ii, a.to))) {
                    fanin[t].push(Fanin {
                        src: f,
                        inst: Some((ii, ai)),
                    });
                }
            }
        }
        let n = names.len();
        let mut t = Timing {
            cells,
            names,
            index,
            owner,
            insts,
            fanin,
            load,
            sdc,
            port_in,
            memo: vec![None; n],
            sense_memo: vec![None; n],
            disabled: std::collections::HashSet::new(),
        };
        t.break_cycles();
        Ok(t)
    }

    /// STA-REQ-0102: depth-first from pins in name order, outgoing arcs in order of the name of
    /// their target; arcs that close a cycle are disabled.
    fn break_cycles(&mut self) {
        let n = self.names.len();
        let mut out: Vec<Vec<(usize, Fanin)>> = vec![Vec::new(); n];
        for (t, fs) in self.fanin.iter().enumerate() {
            for f in fs {
                out[f.src].push((t, *f));
            }
        }
        for o in &mut out {
            o.sort_by(|a, b| self.names[a.0].cmp(&self.names[b.0]));
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| self.names[*a].cmp(&self.names[*b]));
        let mut state = vec![0u8; n];
        for s in order {
            if state[s] != 0 {
                continue;
            }
            state[s] = 1;
            let mut stack = vec![(s, 0usize)];
            while let Some((v, i)) = stack.pop() {
                if i < out[v].len() {
                    stack.push((v, i + 1));
                    let (w, f) = out[v][i];
                    match state[w] {
                        0 => {
                            state[w] = 1;
                            stack.push((w, 0));
                        }
                        1 => {
                            self.disabled.insert((f.src, w, f.inst));
                        }
                        _ => {}
                    }
                } else {
                    state[v] = 2;
                }
            }
        }
    }

    fn enabled(&self, to: usize, f: &Fanin) -> bool {
        !self.disabled.contains(&(f.src, to, f.inst))
    }

    pub fn arc(&self, f: &Fanin) -> Option<&'a ArcT> {
        f.inst.map(|(ii, ai)| &self.cell_of(ii).arcs[ai])
    }

    /// Clock senses of a pin per clock: bit 0 positive, bit 1 negative (STA-REQ-0200), by a
    /// memoized backward search toward the clock sources.
    pub fn senses(&mut self, p: usize) -> Vec<u8> {
        if let Some(s) = &self.sense_memo[p] {
            return s.clone();
        }
        let nclk = self.sdc.clocks.len();
        // Iterative post-order over the fanin graph (cycles contribute nothing).
        self.sense_memo[p] = Some(vec![0; nclk]);
        let mut stack = vec![(p, false)];
        let mut busy = std::collections::HashSet::new();
        while let Some((v, done)) = stack.pop() {
            if done {
                let mut bits = vec![0u8; nclk];
                for (c, ck) in self.sdc.clocks.iter().enumerate() {
                    if ck.sources.contains(&v) {
                        bits[c] |= 1;
                    }
                }
                for f in self.fanin[v].clone() {
                    let src = self.sense_memo[f.src].clone().unwrap_or(vec![0; nclk]);
                    let flip = match self.arc(&f) {
                        None => Some(false),
                        Some(a) if a.latch.is_some() => None,
                        Some(a) => match a.sense {
                            1 => Some(false),
                            -1 => Some(true),
                            _ => {
                                for c in 0..nclk {
                                    if src[c] != 0 {
                                        bits[c] |= 3;
                                    }
                                }
                                None
                            }
                        },
                    };
                    if let Some(fl) = flip {
                        for c in 0..nclk {
                            let s = src[c];
                            bits[c] |= if fl {
                                ((s & 1) << 1) | ((s >> 1) & 1)
                            } else {
                                s
                            };
                        }
                    }
                }
                self.sense_memo[v] = Some(bits);
                busy.remove(&v);
                continue;
            }
            if self.sense_memo[v].is_some() && v != p {
                continue;
            }
            if !busy.insert(v) {
                continue;
            }
            stack.push((v, true));
            for f in &self.fanin[v] {
                if self.sense_memo[f.src].is_none() && !busy.contains(&f.src) {
                    stack.push((f.src, false));
                }
            }
        }
        self.sense_memo[p].clone().unwrap_or_default()
    }

    /// Output transitions from input transition `x` over a fanin.
    fn outs(&self, f: &Fanin, x: usize) -> [bool; 2] {
        match self.arc(f) {
            None => [x == 0, x == 1],
            Some(a) => {
                let mut o = match a.sense {
                    1 => [x == 0, x == 1],
                    -1 => [x == 1, x == 0],
                    _ => [true, true],
                };
                o[0] &= a.outs[0];
                o[1] &= a.outs[1];
                o
            }
        }
    }

    fn eval_tab(t: &Option<crate::cells::Tab>, a: f64, b: f64) -> f64 {
        t.as_ref().map_or(0.0, |t| t.at(a, b))
    }

    /// Startpoint values of a pin (STA-REQ-0201).
    fn start(&mut self, p: usize) -> Vals {
        let mut v: Vals = HashMap::new();
        let put = |v: &mut Vals, tag: usize, tr: usize, e: (f64, f64), l: (f64, f64)| {
            let cur = v.entry((tag, tr)).or_insert([
                (f64::INFINITY, f64::INFINITY),
                (f64::NEG_INFINITY, f64::NEG_INFINITY),
            ]);
            cur[0] = (cur[0].0.min(e.0), cur[0].1.min(e.1));
            cur[1] = (cur[1].0.max(l.0), cur[1].1.max(l.1));
        };
        let sdc = self.sdc;
        for (c, ck) in sdc.clocks.iter().enumerate() {
            if ck.sources.contains(&p) {
                for e in 0..2 {
                    put(
                        &mut v,
                        2 * c + e,
                        e,
                        (ck.latency[0], ck.transition),
                        (ck.latency[1], ck.transition),
                    );
                }
            }
        }
        for d in &sdc.inputs {
            if d.pin == p {
                let tr = sdc.input_tr.get(&p).copied().unwrap_or([0.0, 0.0]);
                for x in 0..2 {
                    put(
                        &mut v,
                        2 * d.clock + d.edge,
                        x,
                        (d.value[0], tr[0]),
                        (d.value[1], tr[1]),
                    );
                }
            }
        }
        if let Some((ii, k)) = self.owner[p] {
            let ct = self.cell_of(ii);
            for a in ct.arcs.iter().filter(|a| a.to == k && a.edge.is_some()) {
                let Some(ck_pin) = self
                    .index
                    .get(&format!("{}/{}", self.insts[ii].0, ct.pins[a.from]))
                    .copied()
                else {
                    continue;
                };
                let senses = self.senses(ck_pin);
                for (c, bits) in senses.iter().enumerate() {
                    let ck = &sdc.clocks[c];
                    let pe = a.edge.expect("edge");
                    for (bit, flip) in [(1u8, false), (2u8, true)] {
                        if bits & bit == 0 {
                            continue;
                        }
                        let src = if flip { 1 - pe } else { pe };
                        for o in 0..2 {
                            let l = &self.load[p][o];
                            let e = (
                                ck.latency[0] + Self::eval_tab(&a.delay[o], ck.transition, l[0]),
                                Self::eval_tab(&a.slew[o], ck.transition, l[0]),
                            );
                            let lt = (
                                ck.latency[1] + Self::eval_tab(&a.delay[o], ck.transition, l[1]),
                                Self::eval_tab(&a.slew[o], ck.transition, l[1]),
                            );
                            put(&mut v, 2 * c + src, o, e, lt);
                        }
                    }
                }
            }
        }
        v
    }

    /// Latch windows of a data arc for a tag: (latest late arrival before subtracting the
    /// setup time, earliest early arrival that passes).
    fn latch_window(&mut self, f: &Fanin, tag: usize) -> Option<(f64, f64, usize)> {
        let (ii, ai) = f.inst?;
        let ct = self.cell_of(ii);
        let ci = ct.arcs[ai].latch?;
        let chk = &ct.checks[ci];
        let open_pin = ct
            .arcs
            .iter()
            .find(|e| e.edge.is_some() && e.from == chk.clock)?
            .edge?;
        let ck_pin = *self
            .index
            .get(&format!("{}/{}", self.insts[ii].0, ct.pins[chk.clock]))?;
        let senses = self.senses(ck_pin);
        let (l, el) = (tag / 2, tag % 2);
        let mut close = f64::NEG_INFINITY;
        let mut open = f64::INFINITY;
        let mut clk = 0;
        for (c, bits) in senses.iter().enumerate() {
            for (bit, flip) in [(1u8, false), (2u8, true)] {
                if bits & bit == 0 {
                    continue;
                }
                clk = c;
                let map = |e: usize| if flip { 1 - e } else { e };
                let ck = &self.sdc.clocks[c];
                let d = crate::checks::gap(self.sdc, l, el, c, map(chk.edge), true).ok()?;
                close = close.max(d + ck.latency[1]);
                let o = crate::checks::gap(self.sdc, l, el, c, map(open_pin), false).ok()?;
                open = open.min(o + ck.latency[0]);
            }
        }
        if close == f64::NEG_INFINITY {
            return None;
        }
        Some((close, open, clk))
    }

    /// Values of pin `p`, evaluated on demand (memoized, iterative).
    pub fn vals(&mut self, p: usize) -> Vals {
        if let Some(v) = &self.memo[p] {
            return v.clone();
        }
        let mut stack = vec![(p, false)];
        while let Some((v, ready)) = stack.pop() {
            if self.memo[v].is_some() {
                continue;
            }
            if !ready {
                stack.push((v, true));
                for f in self.fanin[v].clone() {
                    if self.enabled(v, &f) && self.memo[f.src].is_none() {
                        stack.push((f.src, false));
                    }
                }
                continue;
            }
            let mut out = self.start(v);
            for f in self.fanin[v].clone() {
                if !self.enabled(v, &f) {
                    continue;
                }
                let src = self.memo[f.src].clone().unwrap_or_default();
                let mut keys: Vec<&(usize, usize)> = src.keys().collect();
                keys.sort();
                for &(tag, x) in keys {
                    let s = src[&(tag, x)];
                    let outs = self.outs(&f, x);
                    let win = self.latch_window(&f, tag);
                    for o in 0..2 {
                        if !outs[o] {
                            continue;
                        }
                        let mut res = [(0.0, 0.0); 2];
                        let mut use_early = true;
                        for (an, r) in res.iter_mut().enumerate() {
                            let (mut a_in, sl) = s[an];
                            if let Some((close, open, clk)) = win {
                                let a = self.arc(&f).expect("latch arc");
                                let ct = self.cell_of(f.inst.expect("cell").0);
                                let chk = &ct.checks[a.latch.expect("latch")];
                                if an == 1 {
                                    let st = Self::eval_tab(
                                        &chk.table[x],
                                        self.sdc.clocks[clk].transition,
                                        sl,
                                    );
                                    a_in = a_in.min(close - st);
                                } else if a_in < open {
                                    use_early = false;
                                }
                            }
                            *r = match self.arc(&f) {
                                None => (a_in, sl),
                                Some(a) => {
                                    let l = self.load[v][o][an];
                                    (
                                        a_in + Self::eval_tab(&a.delay[o], sl, l),
                                        Self::eval_tab(&a.slew[o], sl, l),
                                    )
                                }
                            };
                        }
                        let cur = out.entry((tag, o)).or_insert([
                            (f64::INFINITY, f64::INFINITY),
                            (f64::NEG_INFINITY, f64::NEG_INFINITY),
                        ]);
                        if use_early {
                            cur[0] = (cur[0].0.min(res[0].0), cur[0].1.min(res[0].1));
                        }
                        cur[1] = (cur[1].0.max(res[1].0), cur[1].1.max(res[1].1));
                    }
                }
            }
            self.memo[v] = Some(out);
        }
        self.memo[p].clone().unwrap_or_default()
    }
}
