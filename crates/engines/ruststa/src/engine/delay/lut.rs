// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Table lookup for delay calculation (owner: E3).
//!
//! Engine section B. The library tables of a corner are compiled into the lookup-table bank of
//! the compute timing task, one per corner: up to three axes typed by role, linear interpolation
//! between and linear extrapolation beyond the index points. A transition below 0 is clamped to 0;
//! delays and constraint values are never clamped. A lookup outside an index range, or beyond a
//! pin's maximum capacitance or transition, gets one warning aggregated per construct.

use std::borrow::Cow;
use std::collections::BTreeMap;

use rusteda_compute::timing::lut::lookup;
use rusteda_compute::timing::{ArcClass, CompiledLutBank, LutDesc};
use rusteda_core::job::Diag;
use rusteda_core::liberty::{Table, TableVariable, Template};

/// Index of a compiled table in [`LutBank`], or [`NO_TABLE`] when absent.
pub type TableId = u32;

/// Sentinel [`TableId`] for an absent table (`STA-REQ-0113`).
pub const NO_TABLE: TableId = rusteda_compute::timing::NONE;

/// Role of a lookup table, governing axis variable defaults and the output clamp rule
/// (`STA-REQ-0109`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    /// Cell propagation delay (`cell_rise`, `cell_fall`). Never clamped.
    Delay,
    /// Output transition (`rise_transition`, `fall_transition`). Clamped to `>= 0.0`.
    Slew,
    /// Timing check constraint (`rise_constraint`, `fall_constraint`). Never clamped.
    Check,
}

/// Metadata kept per compiled table alongside its [`LutDesc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMeta {
    /// Kind of table (delay, slew, or constraint check).
    pub kind: TableKind,
    /// Construct label for aggregated out-of-range diagnostics (`STA-REQ-0120`).
    pub construct: String,
}

/// Compiled lookup-table bank for one corner (`STA-REQ-0108`, `STA-REQ-0109`, `STA-REQ-0113`,
/// `STA-REQ-0114`, `COMPUTE-REQ-0012`).
#[derive(Debug, Clone, Default)]
pub struct LutBank {
    /// Descriptors of compiled tables, indexed by [`TableId`].
    pub luts: Vec<LutDesc>,
    /// Shared index and value storage, in ns and pF.
    pub data: Vec<f64>,
    /// Compiled arc classes for the compute timing backend.
    pub classes: Vec<ArcClass>,
    /// Per-table metadata parallel to [`Self::luts`].
    pub meta: Vec<TableMeta>,
}

impl LutBank {
    /// Compiles a Liberty [`Table`] into the bank, resolving template variables and default
    /// indices from `templates` when not overridden on the table itself (`STA-REQ-0014`,
    /// `STA-REQ-0108`, `STA-REQ-0114`).
    ///
    /// Returns a `T0014` error diagnostic if the table dimensions are inconsistent with its value
    /// count or if any axis has non-increasing index points.
    pub fn add_table(
        &mut self,
        table: &Table,
        templates: &BTreeMap<String, Template>,
        kind: TableKind,
        time_unit_ns: f64,
        cap_unit_pf: f64,
        construct: impl Into<String>,
    ) -> Result<TableId, Diag> {
        let construct = construct.into();
        let resolved = resolve_with_template(table, templates);
        if !resolved.is_consistent3() {
            let (n1, n2, n3) = resolved.shape3();
            return Err(Diag::error(
                "T0014",
                format!(
                    "{construct}: table '{}' has {} values for {n1} x {n2} x {n3} index points",
                    resolved.template,
                    resolved.values.len()
                ),
            ));
        }
        let axes = [&resolved.index_1, &resolved.index_2, &resolved.index_3];
        for (k, idx) in axes.into_iter().enumerate() {
            if idx.windows(2).any(|w| w[1] <= w[0]) {
                return Err(Diag::error(
                    "T0014",
                    format!(
                        "{construct}: table '{}' has non-increasing index_{}",
                        resolved.template,
                        k + 1
                    ),
                ));
            }
        }

        let vars = [
            resolved.variable(1),
            resolved.variable(2),
            resolved.variable(3),
        ];
        let mut arg = [0u32; 3];
        let mut scales = [1.0f64; 3];
        for axis in 0..3 {
            let (slot, scale) =
                axis_role_and_scale(kind, axis, vars[axis].as_ref(), time_unit_ns, cap_unit_pf);
            arg[axis] = slot;
            scales[axis] = scale;
        }

        let mut n = [0u32; 3];
        let mut off = [0u32; 3];
        let scalar = resolved.values.len() == 1;
        if !scalar {
            let has_axis3 = !resolved.index_3.is_empty();
            let has_axis2 = !resolved.index_2.is_empty() || has_axis3;
            let active = if has_axis3 {
                3
            } else if has_axis2 {
                2
            } else {
                1
            };
            for axis in 0..active {
                let idx = axes[axis];
                off[axis] = self.data.len() as u32;
                if idx.is_empty() {
                    n[axis] = 1;
                    self.data.push(0.0);
                } else {
                    n[axis] = idx.len() as u32;
                    let s = scales[axis];
                    self.data.extend(idx.iter().map(|&x| x * s));
                }
            }
            if active == 1 && n[0] <= 1 {
                n[0] = 0;
            }
        }

        let voff = self.data.len() as u32;
        self.data
            .extend(resolved.values.iter().map(|&v| v * time_unit_ns));
        let id = self.luts.len() as TableId;
        self.luts.push(LutDesc { n, off, arg, voff });
        self.meta.push(TableMeta { kind, construct });
        Ok(id)
    }

    /// Evaluates table `id` at canonical arguments `x`, applying the [`TableKind`] clamp rule
    /// (`STA-REQ-0109`, `STA-REQ-0113`).
    ///
    /// Canonical arguments `x`:
    /// - For [`TableKind::Delay`] and [`TableKind::Slew`]: `[input_slew_ns, output_load_pf, axis3]`.
    /// - For [`TableKind::Check`]: `[related_slew_ns, constrained_slew_ns, output_load_pf]`.
    #[inline]
    #[must_use]
    pub fn eval(&self, id: TableId, x: [f64; 3]) -> f64 {
        if id == NO_TABLE {
            return 0.0;
        }
        let raw = lookup(&self.luts, &self.data, id, x);
        match self.meta[id as usize].kind {
            TableKind::Slew => raw.max(0.0),
            TableKind::Delay | TableKind::Check => raw,
        }
    }

    /// Evaluates table `id` at canonical arguments `x` and records any axis extrapolation in
    /// `tracker` (`STA-REQ-0120`).
    #[inline]
    pub fn eval_tracked(&self, id: TableId, x: [f64; 3], tracker: &mut RangeTracker) -> f64 {
        if id == NO_TABLE {
            return 0.0;
        }
        tracker.check_table(self, id, x);
        self.eval(id, x)
    }

    /// Borrows this bank as a [`CompiledLutBank`] for `rusteda_compute::timing`.
    #[must_use]
    pub fn as_compiled(&self) -> CompiledLutBank<'_> {
        CompiledLutBank {
            luts: Cow::Borrowed(&self.luts),
            data: Cow::Borrowed(&self.data),
            classes: Cow::Borrowed(&self.classes),
        }
    }
}

/// Resolves template variables and default indices from `templates` when `table` references a
/// named template without inline variables or indices.
fn resolve_with_template(table: &Table, templates: &BTreeMap<String, Template>) -> Table {
    let Some(tpl) = templates.get(&table.template) else {
        return table.clone();
    };
    let scalar = table.values.len() == 1
        && table.index_1.is_empty()
        && table.index_2.is_empty()
        && table.index_3.is_empty();
    Table {
        template: table.template.clone(),
        variable_1: table.variable_1.clone().or_else(|| tpl.variable_1.clone()),
        variable_2: table.variable_2.clone().or_else(|| tpl.variable_2.clone()),
        variable_3: table.variable_3.clone().or_else(|| tpl.variable_3.clone()),
        index_1: if !table.index_1.is_empty() || scalar {
            table.index_1.clone()
        } else {
            tpl.index_1.clone()
        },
        index_2: if !table.index_2.is_empty() || scalar {
            table.index_2.clone()
        } else {
            tpl.index_2.clone()
        },
        index_3: if !table.index_3.is_empty() || scalar {
            table.index_3.clone()
        } else {
            tpl.index_3.clone()
        },
        values: table.values.clone(),
    }
}

/// Maps a Liberty axis variable to its canonical argument slot (`0..=2`) and unit scale (`ns`,
/// `pF`, or `1.0`).
fn axis_role_and_scale(
    kind: TableKind,
    axis: usize,
    var: Option<&TableVariable>,
    time_unit_ns: f64,
    cap_unit_pf: f64,
) -> (u32, f64) {
    match kind {
        TableKind::Delay | TableKind::Slew => match var {
            Some(
                TableVariable::TotalOutputNetCapacitance
                | TableVariable::OutputNetWireCap
                | TableVariable::OutputNetPinCap
                | TableVariable::EqualOrOppositeOutputNetCapacitance,
            ) => (1, cap_unit_pf),
            Some(
                TableVariable::RelatedOutTotalOutputNetCapacitance
                | TableVariable::RelatedOutOutputNetWireCap
                | TableVariable::RelatedOutOutputNetPinCap,
            ) => (2, cap_unit_pf),
            Some(TableVariable::OutputNetLength | TableVariable::RelatedOutOutputNetLength) => {
                (2, 1.0)
            }
            Some(
                TableVariable::InputNetTransition
                | TableVariable::InputTransitionTime
                | TableVariable::RelatedPinTransition
                | TableVariable::Time,
            ) => (0, time_unit_ns),
            Some(other) => {
                let name = other.name();
                if name.starts_with("related_out") && name.contains("capacitance") {
                    (2, cap_unit_pf)
                } else if name.contains("capacitance") {
                    if axis == 2 {
                        (2, cap_unit_pf)
                    } else {
                        (1, cap_unit_pf)
                    }
                } else if axis == 2 {
                    (2, 1.0)
                } else {
                    (0, time_unit_ns)
                }
            }
            None => match axis {
                0 => (0, time_unit_ns),
                1 => (1, cap_unit_pf),
                _ => (2, cap_unit_pf),
            },
        },
        TableKind::Check => match var {
            Some(TableVariable::ConstrainedPinTransition) => (1, time_unit_ns),
            Some(
                TableVariable::RelatedPinTransition
                | TableVariable::InputNetTransition
                | TableVariable::InputTransitionTime
                | TableVariable::Time,
            ) => (0, time_unit_ns),
            Some(
                TableVariable::RelatedOutTotalOutputNetCapacitance
                | TableVariable::TotalOutputNetCapacitance
                | TableVariable::RelatedOutOutputNetWireCap
                | TableVariable::RelatedOutOutputNetPinCap
                | TableVariable::OutputNetWireCap
                | TableVariable::OutputNetPinCap,
            ) => (2, cap_unit_pf),
            Some(other) => {
                let name = other.name();
                if name.contains("capacitance") {
                    (2, cap_unit_pf)
                } else if name.contains("constrained") {
                    (1, time_unit_ns)
                } else {
                    (0, time_unit_ns)
                }
            }
            None => match axis {
                0 => (0, time_unit_ns),
                1 => (1, time_unit_ns),
                _ => (2, cap_unit_pf),
            },
        },
    }
}

/// Record of aggregated out-of-range table lookups for one construct.
#[derive(Debug, Clone)]
struct ExtrapEntry {
    count: usize,
    axis: usize,
    coord: f64,
    lo: f64,
    hi: f64,
}

/// Record of aggregated design-rule limit violations (`max_transition` or `max_capacitance`) for
/// one construct.
#[derive(Debug, Clone)]
struct LimitEntry {
    count: usize,
    worst: f64,
    limit: f64,
}

/// Aggregator for out-of-range table lookups and pin `max_transition` / `max_capacitance`
/// violations (`STA-REQ-0120`).
///
/// Emits at most one warning diagnostic per construct regardless of how many lookups or transitions
/// exceed the range.
#[derive(Debug, Clone, Default)]
pub struct RangeTracker {
    table_extrap: BTreeMap<TableId, ExtrapEntry>,
    max_transition: BTreeMap<String, LimitEntry>,
    max_capacitance: BTreeMap<String, LimitEntry>,
}

impl RangeTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks whether evaluating `id` at `x` extrapolates outside any axis range, aggregating by
    /// `id` (`STA-REQ-0120`).
    pub fn check_table(&mut self, bank: &LutBank, id: TableId, x: [f64; 3]) {
        let Some(desc) = bank.luts.get(id as usize) else {
            return;
        };
        for axis in 0..3 {
            let n = desc.n[axis] as usize;
            if n < 2 {
                continue;
            }
            let off = desc.off[axis] as usize;
            let lo = bank.data[off];
            let hi = bank.data[off + n - 1];
            let coord = x[desc.arg[axis] as usize];
            if coord < lo || coord > hi {
                self.table_extrap
                    .entry(id)
                    .and_modify(|e| e.count += 1)
                    .or_insert(ExtrapEntry {
                        count: 1,
                        axis: axis + 1,
                        coord,
                        lo,
                        hi,
                    });
                return;
            }
        }
    }

    /// Checks `slew` against `limit` (`max_transition`, in ns), aggregating violations by
    /// `construct` (`STA-REQ-0120`).
    pub fn check_max_transition(&mut self, construct: &str, slew: f64, limit: f64) {
        if slew > limit {
            self.max_transition
                .entry(construct.to_string())
                .and_modify(|e| {
                    e.count += 1;
                    if slew > e.worst {
                        e.worst = slew;
                    }
                })
                .or_insert(LimitEntry {
                    count: 1,
                    worst: slew,
                    limit,
                });
        }
    }

    /// Records `count` violations of `limit` at `construct` whose largest transition is `worst`, as
    /// `count` calls of [`Self::check_max_transition`] with transitions above `limit` would.
    pub fn record_max_transitions(
        &mut self,
        construct: &str,
        count: usize,
        worst: f64,
        limit: f64,
    ) {
        if count == 0 {
            return;
        }
        self.max_transition
            .entry(construct.to_string())
            .and_modify(|e| {
                e.count += count;
                if worst > e.worst {
                    e.worst = worst;
                }
            })
            .or_insert(LimitEntry {
                count,
                worst,
                limit,
            });
    }

    /// Checks `cap` against `limit` (`max_capacitance`, in pF), aggregating violations by
    /// `construct` (`STA-REQ-0120`).
    pub fn check_max_capacitance(&mut self, construct: &str, cap: f64, limit: f64) {
        if cap > limit {
            self.max_capacitance
                .entry(construct.to_string())
                .and_modify(|e| {
                    e.count += 1;
                    if cap > e.worst {
                        e.worst = cap;
                    }
                })
                .or_insert(LimitEntry {
                    count: 1,
                    worst: cap,
                    limit,
                });
        }
    }

    /// Renders one aggregated warning [`Diag`] per construct (`T0016` for table extrapolation,
    /// `T0017` for `max_transition`, `T0018` for `max_capacitance`).
    #[must_use]
    pub fn warnings(&self, bank: &LutBank) -> Vec<Diag> {
        let mut out = Vec::new();
        for (&id, entry) in &self.table_extrap {
            let construct = bank
                .meta
                .get(id as usize)
                .map_or("<table?>", |m| m.construct.as_str());
            out.push(Diag::warning(
                "T0016",
                format!(
                    "{construct}: table lookup extrapolated outside index_{} range [{:.6}, {:.6}] \
                     (first value {:.6}, {} lookup(s))",
                    entry.axis, entry.lo, entry.hi, entry.coord, entry.count
                ),
            ));
        }
        for (construct, entry) in &self.max_transition {
            out.push(Diag::warning(
                "T0017",
                format!(
                    "{construct}: transition {:.6} ns exceeds max_transition {:.6} ns \
                     ({} occurrence(s))",
                    entry.worst, entry.limit, entry.count
                ),
            ));
        }
        for (construct, entry) in &self.max_capacitance {
            out.push(Diag::warning(
                "T0018",
                format!(
                    "{construct}: capacitance {:.6} pF exceeds max_capacitance {:.6} pF \
                     ({} occurrence(s))",
                    entry.worst, entry.limit, entry.count
                ),
            ));
        }
        out
    }
}
