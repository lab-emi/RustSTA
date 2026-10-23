// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Cell timing model and flattened lookup tables (STA-REQ-0010 to STA-REQ-0012,
//! STA-REQ-0113, STA-REQ-0114).

use rustc_hash::FxHashMap;
use rusteda_core::liberty::{Direction, Library, Table};

/// Transition: 0 = rise, 1 = fall.
pub type Tr = usize;
pub const RISE: Tr = 0;
pub const FALL: Tr = 1;

/// Index into `Tables::refs`.
pub type TableId = u32;
pub const NO_TABLE: TableId = u32::MAX;

/// One flattened table. `arg[k]` names the canonical argument (0 or 1) that indexes dimension
/// `k`; a table with `n1 == 0` is scalar, with `n2 == 0` one-dimensional.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableRef {
    pub off1: u32,
    pub n1: u32,
    pub off2: u32,
    pub n2: u32,
    pub voff: u32,
    pub arg1: u8,
    pub arg2: u8,
}

/// All tables of the libraries in one index/value array, in nanoseconds and picofarads.
#[derive(Debug, Clone, Default)]
pub struct Tables {
    pub data: Vec<f64>,
    pub refs: Vec<TableRef>,
}

/// Interpolation weights along one axis of `n` points starting at `off` (STA-REQ-0114).
fn axis(data: &[f64], off: usize, n: usize, x: f64) -> (usize, usize, f64) {
    if n <= 1 {
        return (0, 0, 0.0);
    }
    let idx = &data[off..off + n];
    let mut k = 0;
    while k + 2 < n && x > idx[k + 1] {
        k += 1;
    }
    let (x0, x1) = (idx[k], idx[k + 1]);
    (k, k + 1, (x - x0) / (x1 - x0))
}

impl Tables {
    /// Value of table `t` at canonical arguments `a` (transition or related transition) and `b`
    /// (load or constrained transition).
    pub fn lookup(&self, t: TableId, a: f64, b: f64) -> f64 {
        let r = self.refs[t as usize];
        let args = [a, b];
        let d = &self.data;
        if r.n1 == 0 {
            return d[r.voff as usize];
        }
        let x = args[r.arg1 as usize];
        let (i0, i1, u) = axis(d, r.off1 as usize, r.n1 as usize, x);
        if r.n2 == 0 {
            let v0 = d[r.voff as usize + i0];
            let v1 = d[r.voff as usize + i1];
            return v0 + (v1 - v0) * u;
        }
        let y = args[r.arg2 as usize];
        let (j0, j1, w) = axis(d, r.off2 as usize, r.n2 as usize, y);
        let c = r.n2 as usize;
        let at = |i: usize, j: usize| d[r.voff as usize + i * c + j];
        let lo = at(i0, j0) + (at(i0, j1) - at(i0, j0)) * w;
        let hi = at(i1, j0) + (at(i1, j1) - at(i1, j0)) * w;
        lo + (hi - lo) * u
    }

    /// Adds a Liberty table. `second` lists the variable names that are the second canonical
    /// argument (load or constrained transition); `time` and `cap` scale transition and
    /// capacitance indices, `vscale` the values.
    fn add(
        &mut self,
        t: &Table,
        second: &[&str],
        time: f64,
        cap: f64,
        vscale: f64,
    ) -> Result<TableId, String> {
        if !t.is_consistent() {
            return Err(format!("table '{}' has inconsistent sizes", t.template));
        }
        let which = |v: &Option<String>| -> u8 {
            match v.as_deref() {
                Some(name) if second.contains(&name) => 1,
                _ => 0,
            }
        };
        let scale = |v: &Option<String>| -> f64 {
            match v.as_deref() {
                Some(n) if n.contains("capacitance") => cap,
                _ => time,
            }
        };
        let scalar = t.values.len() == 1;
        let mut r = TableRef {
            off1: 0,
            n1: 0,
            off2: 0,
            n2: 0,
            voff: 0,
            arg1: which(&t.variable_1),
            arg2: which(&t.variable_2),
        };
        if !scalar {
            for (idx, s, off, n) in [
                (&t.index_1, scale(&t.variable_1), &mut r.off1, &mut r.n1),
                (&t.index_2, scale(&t.variable_2), &mut r.off2, &mut r.n2),
            ] {
                if idx.windows(2).any(|w| w[1] <= w[0]) {
                    return Err(format!(
                        "table '{}' has indices that are not strictly increasing",
                        t.template
                    ));
                }
                *off = self.data.len() as u32;
                *n = idx.len() as u32;
                self.data.extend(idx.iter().map(|x| x * s));
            }
            if r.n1 == 1 && r.n2 == 0 {
                // A single point is a constant.
                r.n1 = 0;
            }
        }
        r.voff = self.data.len() as u32;
        self.data.extend(t.values.iter().map(|v| v * vscale));
        self.refs.push(r);
        Ok((self.refs.len() - 1) as TableId)
    }
}

/// Kind of a propagation arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcKind {
    /// Combinational; the output transitions it can produce (`[rise, fall]`).
    Comb { outs: [bool; 2] },
    /// Edge arc launching at the given clock-pin transition.
    Edge(Tr),
}

/// Timing sense of a combinational arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    Positive,
    Negative,
    Non,
}

/// A propagation arc of a cell.
#[derive(Debug, Clone)]
pub struct CellArc {
    pub from: usize,
    pub to: usize,
    pub kind: ArcKind,
    pub sense: Sense,
    /// Delay tables for an output rise and fall.
    pub delay: [TableId; 2],
    /// Output transition tables for rise and fall.
    pub slew: [TableId; 2],
    /// For the data-to-output arc of a latch: the setup check that gives its closing edge
    /// (STA-REQ-0213).
    pub latch_check: Option<usize>,
}

/// Kind of a timing check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// Setup or recovery: data must arrive before the edge.
    Setup,
    /// Hold or removal: data must stay after the edge.
    Hold,
}

/// A timing check of a cell.
#[derive(Debug, Clone)]
pub struct CellCheck {
    pub data: usize,
    pub clock: usize,
    pub kind: CheckKind,
    /// Clock-pin transition of the check edge.
    pub edge: Tr,
    /// Constraint tables for a data rise and fall.
    pub table: [TableId; 2],
}

/// A cell pin.
#[derive(Debug, Clone)]
pub struct CellPin {
    pub name: String,
    pub input: bool,
    pub output: bool,
    /// Input capacitance seen by a rising and a falling driver, early and late: `[tr][an]`
    /// (STA-REQ-0110).
    pub cap: [[f64; 2]; 2],
    /// Output function over the cell's input pins (bit k of a minterm = k-th entry), for
    /// combinational outputs of cells with at most six inputs.
    pub func: Option<(Vec<usize>, u64)>,
}

/// Timing view of a library cell.
#[derive(Debug, Clone)]
pub struct TimingCell {
    pub name: String,
    pub pins: Vec<CellPin>,
    pub arcs: Vec<CellArc>,
    pub checks: Vec<CellCheck>,
    pub notes: Vec<String>,
}

impl TimingCell {
    pub fn pin(&self, name: &str) -> Option<usize> {
        self.pins.iter().position(|p| p.name == name)
    }
}

/// Cells of all libraries (first definition wins) and their tables.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub cells: Vec<TimingCell>,
    pub by_name: FxHashMap<String, usize>,
    pub tables: Tables,
    /// Units of the first library, in ns and pF.
    pub time_unit: f64,
    pub cap_unit: f64,
}

const LOAD_VARS: &[&str] = &["total_output_net_capacitance"];
const CONSTRAINED_VARS: &[&str] = &["constrained_pin_transition"];

impl Model {
    pub fn build(libs: &[Library]) -> Result<Model, String> {
        let mut m = Model {
            time_unit: libs.first().map_or(1.0, |l| l.time_unit_ns),
            cap_unit: libs.first().map_or(1.0, |l| l.cap_unit_pf),
            ..Model::default()
        };
        for lib in libs {
            let (tu, cu) = (lib.time_unit_ns, lib.cap_unit_pf);
            for c in &lib.cells {
                if m.by_name.contains_key(&c.name) {
                    continue;
                }
                let mut tc = TimingCell {
                    name: c.name.clone(),
                    pins: c
                        .pins
                        .iter()
                        .map(|p| CellPin {
                            name: p.name.clone(),
                            input: matches!(p.direction, Direction::Input | Direction::Inout),
                            output: matches!(p.direction, Direction::Output | Direction::Inout),
                            cap: {
                                let c = |plain: Option<f64>, range: Option<(f64, f64)>| {
                                    let v = plain.unwrap_or(p.capacitance);
                                    let (lo, hi) = range.unwrap_or((v, v));
                                    [cu * lo, cu * hi]
                                };
                                [
                                    c(p.rise_capacitance, p.rise_capacitance_range),
                                    c(p.fall_capacitance, p.fall_capacitance_range),
                                ]
                            },
                            func: None,
                        })
                        .collect(),
                    arcs: Vec::new(),
                    checks: Vec::new(),
                    notes: Vec::new(),
                };
                for (to, p) in c.pins.iter().enumerate() {
                    for a in &p.arcs {
                        let Some(from) = tc.pin(&a.related_pin) else {
                            tc.notes.push(format!(
                                "pin {}: arc from unknown pin '{}' ignored",
                                p.name, a.related_pin
                            ));
                            continue;
                        };
                        let mut tab =
                            |t: &Option<Table>, second: &[&str]| -> Result<TableId, String> {
                                match t {
                                    Some(t) => m
                                        .tables
                                        .add(t, second, tu, cu, tu)
                                        .map_err(|e| format!("cell {}: {e}", c.name)),
                                    None => Ok(NO_TABLE),
                                }
                            };
                        let tt = a.timing_type.trim();
                        let check = match tt {
                            "setup_rising" | "recovery_rising" => Some((CheckKind::Setup, RISE)),
                            "setup_falling" | "recovery_falling" => Some((CheckKind::Setup, FALL)),
                            "hold_rising" | "removal_rising" => Some((CheckKind::Hold, RISE)),
                            "hold_falling" | "removal_falling" => Some((CheckKind::Hold, FALL)),
                            _ => None,
                        };
                        if let Some((kind, edge)) = check {
                            let table = [
                                tab(&a.rise_constraint, CONSTRAINED_VARS)?,
                                tab(&a.fall_constraint, CONSTRAINED_VARS)?,
                            ];
                            tc.checks.push(CellCheck {
                                data: to,
                                clock: from,
                                kind,
                                edge,
                                table,
                            });
                            continue;
                        }
                        let kind = match tt {
                            "combinational" | "" => ArcKind::Comb { outs: [true, true] },
                            "combinational_rise" => ArcKind::Comb {
                                outs: [true, false],
                            },
                            "combinational_fall" => ArcKind::Comb {
                                outs: [false, true],
                            },
                            "rising_edge" => ArcKind::Edge(RISE),
                            "falling_edge" => ArcKind::Edge(FALL),
                            "clear" | "preset" => continue,
                            other => {
                                tc.notes
                                    .push(format!("pin {}: timing type '{other}' ignored", p.name));
                                continue;
                            }
                        };
                        let sense = match a.timing_sense.as_deref().map(str::trim) {
                            Some("positive_unate") => Sense::Positive,
                            Some("negative_unate") => Sense::Negative,
                            Some("non_unate") => Sense::Non,
                            _ => derive_sense(p, &a.related_pin),
                        };
                        let delay = [tab(&a.cell_rise, LOAD_VARS)?, tab(&a.cell_fall, LOAD_VARS)?];
                        let slew = [
                            tab(&a.rise_transition, LOAD_VARS)?,
                            tab(&a.fall_transition, LOAD_VARS)?,
                        ];
                        for (k, t) in delay.iter().enumerate() {
                            if *t == NO_TABLE {
                                tc.notes.push(format!(
                                    "arc {} -> {}: no {} delay table, delay 0 (STA-REQ-0113)",
                                    a.related_pin,
                                    p.name,
                                    if k == RISE { "rise" } else { "fall" }
                                ));
                            }
                        }
                        tc.arcs.push(CellArc {
                            from,
                            to,
                            kind,
                            sense,
                            delay,
                            slew,
                            latch_check: None,
                        });
                    }
                }
                // Output functions over the input pins (STA-REQ-0216).
                if c.sequentials.is_empty() {
                    let ins: Vec<usize> = (0..tc.pins.len())
                        .filter(|k| tc.pins[*k].input && !tc.pins[*k].output)
                        .collect();
                    if ins.len() <= 6 {
                        let owned: Vec<String> =
                            ins.iter().map(|k| tc.pins[*k].name.clone()).collect();
                        let names: Vec<&str> = owned.iter().map(String::as_str).collect();
                        for (k, p) in c.pins.iter().enumerate() {
                            if let Some(tt) =
                                p.function.as_ref().and_then(|f| f.truth_table(&names))
                            {
                                tc.pins[k].func = Some((ins.clone(), tt));
                            }
                        }
                    }
                }
                // Latch data arcs: a combinational arc from a pin with a setup check against a
                // clock pin that has an edge arc to the same output.
                for ai in 0..tc.arcs.len() {
                    let a = &tc.arcs[ai];
                    if !matches!(a.kind, ArcKind::Comb { .. }) {
                        continue;
                    }
                    let found = tc.checks.iter().position(|ch| {
                        ch.kind == CheckKind::Setup
                            && ch.data == a.from
                            && tc.arcs.iter().any(|e| {
                                matches!(e.kind, ArcKind::Edge(_))
                                    && e.from == ch.clock
                                    && e.to == a.to
                            })
                    });
                    tc.arcs[ai].latch_check = found;
                }
                m.by_name.insert(c.name.clone(), m.cells.len());
                m.cells.push(tc);
            }
        }
        Ok(m)
    }

    /// Value of a table, 0 when absent.
    pub fn eval(&self, t: TableId, a: f64, b: f64) -> f64 {
        if t == NO_TABLE {
            0.0
        } else {
            self.tables.lookup(t, a, b)
        }
    }
}

/// Timing sense from the output function (STA-REQ-0012): positive when the function is monotone
/// increasing in the pin, negative when decreasing, else non-unate.
fn derive_sense(p: &rusteda_core::liberty::Pin, related: &str) -> Sense {
    let Some(f) = &p.function else {
        return Sense::Non;
    };
    let vars = f.vars();
    let Some(k) = vars.iter().position(|v| v == related) else {
        return Sense::Non;
    };
    let names: Vec<&str> = vars.iter().map(String::as_str).collect();
    if names.len() > 6 {
        return Sense::Non;
    }
    let Some(tt) = f.truth_table(&names) else {
        return Sense::Non;
    };
    let n = names.len();
    let (mut up, mut down) = (false, false);
    for m in 0..(1u64 << n) {
        if (m >> k) & 1 == 1 {
            continue;
        }
        let lo = (tt >> m) & 1;
        let hi = (tt >> (m | (1 << k))) & 1;
        if hi > lo {
            up = true;
        }
        if lo > hi {
            down = true;
        }
    }
    match (up, down) {
        (true, false) => Sense::Positive,
        (false, true) => Sense::Negative,
        _ => Sense::Non,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // STA-REQ-0114
    #[test]
    fn table_lookup_follows_template_order() {
        let mut t = Tables::default();
        let tab = Table {
            template: "t".into(),
            variable_1: Some("total_output_net_capacitance".into()),
            variable_2: Some("input_net_transition".into()),
            index_1: vec![1.0, 2.0],
            index_2: vec![10.0, 20.0, 40.0],
            values: vec![1.0, 2.0, 4.0, 3.0, 4.0, 6.0],
            ..Table::default()
        };
        let id = t.add(&tab, LOAD_VARS, 1.0, 1.0, 1.0).unwrap();
        // Canonical arguments: (transition, load) = (15, 1.5) -> interior point.
        assert!((t.lookup(id, 15.0, 1.5) - 2.5).abs() < 1e-12);
        // Extrapolation along both axes.
        assert!((t.lookup(id, 10.0, 3.0) - 5.0).abs() < 1e-12);
        assert!((t.lookup(id, 0.0, 1.0) - 0.0).abs() < 1e-12);
        let s = Table {
            values: vec![7.0],
            ..Table::default()
        };
        let sid = t.add(&s, LOAD_VARS, 1.0, 1.0, 2.0).unwrap();
        assert_eq!(t.lookup(sid, 3.0, 4.0), 14.0);
        let bad = Table {
            index_1: vec![2.0, 1.0],
            values: vec![1.0, 2.0],
            ..Table::default()
        };
        assert!(t.add(&bad, LOAD_VARS, 1.0, 1.0, 1.0).is_err());
    }
}
