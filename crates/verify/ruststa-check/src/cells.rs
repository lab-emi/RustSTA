// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Cell timing for the checker, read directly from the Liberty model (STA-REQ-0010 to -0012,
//! STA-REQ-0110, STA-REQ-0113, STA-REQ-0114).

use std::collections::HashMap;

use rusteda_core::liberty::{Direction, Library, Table};

/// A table with its values in ns (or ns and pF on the indices) and the role of each index.
#[derive(Debug, Clone)]
pub struct Tab {
    /// Index vectors, already scaled; empty for missing dimensions.
    idx: [Vec<f64>; 2],
    /// For each dimension: true if it takes the second argument (load or constrained transition).
    second: [bool; 2],
    values: Vec<f64>,
}

/// Linear interpolation or extrapolation of `ys` over `xs` at `x`, using the two points of the
/// segment found by binary search (the first or last segment outside the range).
fn interp1(xs: &[f64], x: f64) -> (usize, f64) {
    if xs.len() < 2 {
        return (0, 0.0);
    }
    // Index of the first point strictly greater than x, clamped to a valid segment start.
    let p = xs.partition_point(|v| *v <= x);
    let k = p.saturating_sub(1).min(xs.len() - 2);
    (k, (x - xs[k]) / (xs[k + 1] - xs[k]))
}

impl Tab {
    fn new(t: &Table, second_names: &[&str], tu: f64, cu: f64) -> Tab {
        let role = |v: &Option<String>| v.as_deref().is_some_and(|n| second_names.contains(&n));
        let scale = |v: &Option<String>| {
            if v.as_deref().is_some_and(|n| n.contains("capacitance")) {
                cu
            } else {
                tu
            }
        };
        let single = t.values.len() == 1;
        let dim = |idx: &Vec<f64>, v: &Option<String>| -> Vec<f64> {
            if single {
                Vec::new()
            } else {
                idx.iter().map(|x| x * scale(v)).collect()
            }
        };
        Tab {
            idx: [
                dim(&t.index_1, &t.variable_1),
                dim(&t.index_2, &t.variable_2),
            ],
            second: [role(&t.variable_1), role(&t.variable_2)],
            values: t.values.iter().map(|v| v * tu).collect(),
        }
    }

    /// Value at (first, second) canonical arguments.
    pub fn at(&self, first: f64, second: f64) -> f64 {
        let arg = |d: usize| if self.second[d] { second } else { first };
        let (n1, n2) = (self.idx[0].len(), self.idx[1].len());
        if self.values.len() == 1 || n1 == 0 {
            return self.values[0];
        }
        let (i, u) = if n1 == 1 {
            (0, 0.0)
        } else {
            interp1(&self.idx[0], arg(0))
        };
        let i1 = if n1 == 1 { 0 } else { i + 1 };
        if n2 == 0 {
            return self.values[i] + (self.values[i1] - self.values[i]) * u;
        }
        let (j, w) = if n2 == 1 {
            (0, 0.0)
        } else {
            interp1(&self.idx[1], arg(1))
        };
        let j1 = if n2 == 1 { 0 } else { j + 1 };
        let v = |a: usize, b: usize| self.values[a * n2 + b];
        let top = v(i, j) * (1.0 - w) + v(i, j1) * w;
        let bot = v(i1, j) * (1.0 - w) + v(i1, j1) * w;
        top * (1.0 - u) + bot * u
    }
}

/// Timing of one cell.
#[derive(Debug, Clone, Default)]
pub struct CellTiming {
    pub pins: Vec<String>,
    pub is_input: Vec<bool>,
    pub is_output: Vec<bool>,
    /// Capacitance `[pin][transition][early/late]`.
    pub cap: Vec<[[f64; 2]; 2]>,
    /// Propagation arcs: (from, to, kind, sense, [delay rise, fall], [slew rise, fall]).
    pub arcs: Vec<ArcT>,
    /// Checks: (data, clock, is setup, clock-pin transition, [constraint rise, fall]).
    pub checks: Vec<CheckT>,
    /// Output functions over input pins: pin -> (inputs, truth table).
    pub funcs: HashMap<usize, (Vec<usize>, u64)>,
}

#[derive(Debug, Clone)]
pub struct ArcT {
    pub from: usize,
    pub to: usize,
    /// `None` for combinational arcs, `Some(t)` for edge arcs at clock-pin transition `t`.
    pub edge: Option<usize>,
    /// Output transitions allowed (combinational rise/fall variants).
    pub outs: [bool; 2],
    /// 1 positive, -1 negative, 0 non-unate.
    pub sense: i8,
    pub delay: [Option<Tab>; 2],
    pub slew: [Option<Tab>; 2],
    /// Index of the setup check if this is the data-to-output arc of a latch.
    pub latch: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CheckT {
    pub data: usize,
    pub clock: usize,
    pub setup: bool,
    pub edge: usize,
    pub table: [Option<Tab>; 2],
}

impl CellTiming {
    pub fn pin(&self, n: &str) -> Option<usize> {
        self.pins.iter().position(|p| p == n)
    }
}

/// Timing of every cell by name (first library wins) and the units of the first library.
pub fn build(libs: &[Library]) -> (HashMap<String, CellTiming>, f64, f64) {
    let mut out = HashMap::new();
    for lib in libs {
        let (tu, cu) = (lib.time_unit_ns, lib.cap_unit_pf);
        for c in &lib.cells {
            if out.contains_key(&c.name) {
                continue;
            }
            let mut t = CellTiming::default();
            for p in &c.pins {
                t.pins.push(p.name.clone());
                t.is_input
                    .push(matches!(p.direction, Direction::Input | Direction::Inout));
                t.is_output
                    .push(matches!(p.direction, Direction::Output | Direction::Inout));
                let pick = |plain: Option<f64>, range: Option<(f64, f64)>| {
                    let v = plain.unwrap_or(p.capacitance);
                    match range {
                        Some((a, b)) => [a * cu, b * cu],
                        None => [v * cu, v * cu],
                    }
                };
                t.cap.push([
                    pick(p.rise_capacitance, p.rise_capacitance_range),
                    pick(p.fall_capacitance, p.fall_capacitance_range),
                ]);
            }
            let load = ["total_output_net_capacitance"];
            let constrained = ["constrained_pin_transition"];
            for (to, p) in c.pins.iter().enumerate() {
                for a in &p.arcs {
                    let Some(from) = t.pin(&a.related_pin) else {
                        continue;
                    };
                    let tab = |x: &Option<Table>, sec: &[&str]| {
                        x.as_ref().map(|x| Tab::new(x, sec, tu, cu))
                    };
                    let ty = a.timing_type.trim();
                    let chk = match ty {
                        "setup_rising" | "recovery_rising" => Some((true, 0)),
                        "setup_falling" | "recovery_falling" => Some((true, 1)),
                        "hold_rising" | "removal_rising" => Some((false, 0)),
                        "hold_falling" | "removal_falling" => Some((false, 1)),
                        _ => None,
                    };
                    if let Some((setup, edge)) = chk {
                        t.checks.push(CheckT {
                            data: to,
                            clock: from,
                            setup,
                            edge,
                            table: [
                                tab(&a.rise_constraint, &constrained),
                                tab(&a.fall_constraint, &constrained),
                            ],
                        });
                        continue;
                    }
                    let (edge, outs) = match ty {
                        "" | "combinational" => (None, [true, true]),
                        "combinational_rise" => (None, [true, false]),
                        "combinational_fall" => (None, [false, true]),
                        "rising_edge" => (Some(0), [true, true]),
                        "falling_edge" => (Some(1), [true, true]),
                        _ => continue,
                    };
                    let sense = match a.timing_sense.as_deref().map(str::trim) {
                        Some("positive_unate") => 1,
                        Some("negative_unate") => -1,
                        Some("non_unate") => 0,
                        _ => monotone(p, &a.related_pin),
                    };
                    t.arcs.push(ArcT {
                        from,
                        to,
                        edge,
                        outs,
                        sense,
                        delay: [tab(&a.cell_rise, &load), tab(&a.cell_fall, &load)],
                        slew: [
                            tab(&a.rise_transition, &load),
                            tab(&a.fall_transition, &load),
                        ],
                        latch: None,
                    });
                }
            }
            // Latch data arcs: combinational, from the data pin of a setup check whose clock pin
            // has an edge arc to the same output.
            for i in 0..t.arcs.len() {
                if t.arcs[i].edge.is_some() {
                    continue;
                }
                let (f, o) = (t.arcs[i].from, t.arcs[i].to);
                t.arcs[i].latch = t.checks.iter().position(|ch| {
                    ch.setup
                        && ch.data == f
                        && t.arcs
                            .iter()
                            .any(|e| e.edge.is_some() && e.from == ch.clock && e.to == o)
                });
            }
            if c.sequentials.is_empty() {
                let ins: Vec<usize> = (0..t.pins.len())
                    .filter(|k| t.is_input[*k] && !t.is_output[*k])
                    .collect();
                if ins.len() <= 6 {
                    let names: Vec<&str> = ins.iter().map(|k| c.pins[*k].name.as_str()).collect();
                    for (k, p) in c.pins.iter().enumerate() {
                        if let Some(tt) = p.function.as_ref().and_then(|f| f.truth_table(&names)) {
                            t.funcs.insert(k, (ins.clone(), tt));
                        }
                    }
                }
            }
            out.insert(c.name.clone(), t);
        }
    }
    let (tu, cu) = libs
        .first()
        .map_or((1.0, 1.0), |l| (l.time_unit_ns, l.cap_unit_pf));
    (out, tu, cu)
}

/// Sense of the pin's function in `related`: 1 increasing, -1 decreasing, 0 otherwise.
fn monotone(p: &rusteda_core::liberty::Pin, related: &str) -> i8 {
    let Some(f) = &p.function else { return 0 };
    let vars = f.vars();
    let Some(k) = vars.iter().position(|v| v == related) else {
        return 0;
    };
    if vars.len() > 6 {
        return 0;
    }
    let names: Vec<&str> = vars.iter().map(String::as_str).collect();
    let Some(tt) = f.truth_table(&names) else {
        return 0;
    };
    let mut rises = false;
    let mut falls = false;
    for m in 0u64..(1 << vars.len()) {
        if m & (1 << k) != 0 {
            continue;
        }
        let (a, b) = ((tt >> m) & 1, (tt >> (m | (1 << k))) & 1);
        rises |= b > a;
        falls |= a > b;
    }
    match (rises, falls) {
        (true, false) => 1,
        (false, true) => -1,
        _ => 0,
    }
}
