// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Pin limit checks of a corner (owner: S1; `STA-REQ-0800` to `STA-REQ-0812`).
//!
//! After the propagation of a corner, every clock pin with `min_pulse_width` or `minimum_period`
//! timing groups is checked against the propagated clock waveform at the pin ([`pulse`]), and every
//! pin and net driver against the electrical design rules `max_transition`, `max_capacitance` and
//! `max_fanout` of the corner's libraries and of the constraints ([`drv`]). The checks visit each
//! node, net and clock pin once (`O(V + E)`); the rows they report are kept in a bounded heap per
//! kind (AGENTS rule 3.4), worst slack first, while the counts cover every check. The rows of all
//! corners form the `limits` table of a revision ([`table`]).

mod drv;
mod pulse;
pub mod table;

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rusteda_core::job::Diag;

use crate::engine::delay::load::NetLoads;
use crate::engine::delay::lut::RangeTracker;
use crate::engine::input::NetlistView;
use crate::engine::input::constr::CompiledConstraints;
use crate::engine::input::libview::TimingLib;
use crate::engine::prop::clocks::ideal::IdealClockModel;
use crate::engine::prop::lower::TimingValues;
use crate::engine::prop::tags::TagTable;
use crate::engine::types::{ClockId, NodeId, Tr};

/// Default number of rows kept per corner and check kind (`STA-REQ-0810`).
pub const DEFAULT_MAX_LIMIT_ROWS: usize = 100_000;

/// Largest accepted number of rows kept per corner and check kind (`STA-REQ-0810`).
pub const MAX_LIMIT_ROWS_CEILING: usize = 10_000_000;

/// Bytes of the rows that the limit checks of one corner keep at most: the row budget, or the
/// node count when smaller, per kind (`STA-REQ-0810`; part of the state budget of a corner).
#[must_use]
pub fn retained_bytes(num_nodes: usize, max_rows: usize) -> u64 {
    (max_rows.min(num_nodes) as u64)
        .saturating_mul(LimitKind::ALL.len() as u64)
        .saturating_mul(size_of::<LimitRow>() as u64)
}

/// The kind of a pin limit check (`STA-REQ-0800`, `STA-REQ-0803`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LimitKind {
    /// Transition time of a pin against its `max_transition` limit.
    MaxTransition = 0,
    /// Capacitance of the net of a driver against its `max_capacitance` limit.
    MaxCapacitance = 1,
    /// Fanout of the net of a driver against its `max_fanout` limit.
    MaxFanout = 2,
    /// Width of the high pulse of a clock at a pin against its `min_pulse_width` rise constraint.
    MinPulseWidthHigh = 3,
    /// Width of the low pulse of a clock at a pin against its `min_pulse_width` fall constraint.
    MinPulseWidthLow = 4,
    /// Period of a clock at a pin against its `minimum_period` constraint.
    MinPeriod = 5,
}

impl LimitKind {
    /// Every kind, in table order.
    pub const ALL: [LimitKind; 6] = [
        LimitKind::MaxTransition,
        LimitKind::MaxCapacitance,
        LimitKind::MaxFanout,
        LimitKind::MinPulseWidthHigh,
        LimitKind::MinPulseWidthLow,
        LimitKind::MinPeriod,
    ];

    /// The index of the kind in [`LimitKind::ALL`].
    #[inline]
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }

    /// The kind stored as `v`, if any.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        Self::ALL.get(v as usize).copied()
    }

    /// The name of the kind in reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::MaxTransition => "max_transition",
            Self::MaxCapacitance => "max_capacitance",
            Self::MaxFanout => "max_fanout",
            Self::MinPulseWidthHigh => "min_pulse_width_high",
            Self::MinPulseWidthLow => "min_pulse_width_low",
            Self::MinPeriod => "min_period",
        }
    }

    /// The kind named `name` in reports.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// True for the electrical design rules (`max_transition`, `max_capacitance`, `max_fanout`).
    #[must_use]
    pub fn is_drv(self) -> bool {
        matches!(
            self,
            Self::MaxTransition | Self::MaxCapacitance | Self::MaxFanout
        )
    }
}

/// Where the limit of a row comes from (`STA-REQ-0805`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LimitSource {
    /// The library: a pin attribute, a library default or a pulse-check table.
    Liberty = 0,
    /// A design-rule command on the current design.
    SdcDesign = 1,
    /// A design-rule command on a port.
    SdcPort = 2,
    /// A design-rule command on a clock.
    SdcClock = 3,
}

impl LimitSource {
    /// The source stored as `v`, if any.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        [
            Self::Liberty,
            Self::SdcDesign,
            Self::SdcPort,
            Self::SdcClock,
        ]
        .get(v as usize)
        .copied()
    }

    /// The name of the source in reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Liberty => "liberty",
            Self::SdcDesign => "sdc_design",
            Self::SdcPort => "sdc_port",
            Self::SdcClock => "sdc_clock",
        }
    }
}

/// One checked limit of a corner (`STA-REQ-0803`, `STA-REQ-0806`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimitRow {
    /// The checked pin or port (for the capacitance and the fanout: the driver of the net).
    pub node: NodeId,
    /// The check.
    pub kind: LimitKind,
    /// The transition of the worst slack: of the pin (transition), of the net (capacitance), of
    /// the edge that opens the pulse (pulse width) or of the period's edges (period); `None` for
    /// the fanout.
    pub tr: Option<Tr>,
    /// The clock of a pulse or period check.
    pub clock: Option<ClockId>,
    /// The checked value: ns, pF, fanout load, pulse width in ns or period in ns.
    pub value: f64,
    /// The limit: the maximum of a design rule, or the minimum width or period.
    pub limit: f64,
    /// `limit - value` for the design rules and `value - limit` for pulse width and period:
    /// negative when violated.
    pub slack: f64,
    /// Where the limit comes from.
    pub source: LimitSource,
}

/// The counts of one check kind in a corner (`STA-REQ-0806`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KindSummary {
    /// Checked rows (pins, drivers, or pin and clock pairs).
    pub checked: u64,
    /// Checked rows with a negative slack.
    pub violating: u64,
    /// The worst slack, `None` without a checked row.
    pub worst_slack: Option<f64>,
    /// Pins or drivers with a value but no limit (design rules only).
    pub unlimited: u64,
    /// Rows kept in [`CornerLimits::rows`] (at most the row budget).
    pub stored: u64,
}

impl KindSummary {
    /// Rows counted but not kept because of the row budget.
    #[must_use]
    pub fn truncated(&self) -> u64 {
        self.checked - self.stored
    }
}

/// The limit checks of one corner (`STA-REQ-0806`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CornerLimits {
    /// Kept rows, by kind, worst slack first, ties by node and clock.
    pub rows: Vec<LimitRow>,
    /// Counts per kind, indexed by [`LimitKind::index`].
    pub kinds: [KindSummary; 6],
    /// Instance pins with pulse checks that no clock reaches: their checks are not evaluated.
    pub unclocked_pulse_pins: u64,
    /// Conditional (`when`) pulse groups on the checked instance pins.
    pub conditional_pulse_groups: u64,
}

impl CornerLimits {
    /// The counts of `kind`.
    #[must_use]
    pub fn kind(&self, kind: LimitKind) -> &KindSummary {
        &self.kinds[kind.index()]
    }

    /// The kept rows of `kind`.
    pub fn rows_of(&self, kind: LimitKind) -> impl Iterator<Item = &LimitRow> + '_ {
        self.rows.iter().filter(move |r| r.kind == kind)
    }
}

/// The inputs of the limit checks of one corner.
pub struct LimitInputs<'a> {
    /// The netlist.
    pub netview: &'a NetlistView<'a>,
    /// The libraries of the corner.
    pub lib: &'a TimingLib,
    /// The compiled constraints.
    pub constraints: &'a CompiledConstraints,
    /// The clock model of the corner.
    pub clock_model: &'a IdealClockModel,
    /// The propagation tags of the corner.
    pub tags: &'a TagTable,
    /// The propagated values of the corner in node order.
    pub node_values: &'a TimingValues,
    /// The net loads of the corner.
    pub net_loads: &'a NetLoads,
}

impl std::fmt::Debug for LimitInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LimitInputs")
            .field("corner", &self.lib.corner)
            .finish_non_exhaustive()
    }
}

/// Checks the pulse widths, periods and electrical design rules of one corner
/// (`STA-REQ-0800` to `STA-REQ-0810`). At most `max_rows` rows are kept per kind; the
/// diagnostics of the constraints are added to `diags` when `constraint_diags` is set (once per
/// analysis), the aggregated notes of the corner always.
pub fn check_corner_limits(
    inputs: &LimitInputs<'_>,
    max_rows: usize,
    constraint_diags: bool,
    tracker: &mut RangeTracker,
    diags: &mut Vec<Diag>,
) -> CornerLimits {
    let mut rows = RowCollector::new(max_rows);
    let mut out = CornerLimits::default();
    pulse::check_pulses(inputs, tracker, &mut rows, &mut out);
    let mut sdc_diags = Vec::new();
    drv::check_design_rules(inputs, &mut rows, &mut sdc_diags);
    if constraint_diags {
        diags.extend(sdc_diags);
    }
    let corner = &inputs.lib.corner;
    if out.unclocked_pulse_pins > 0 {
        diags.push(Diag::warning(
            "T0133",
            format!(
                "corner '{corner}': {} pin(s) with min_pulse_width or minimum_period checks are \
                 reached by no clock; their pulse checks are unchecked (STA-REQ-0802)",
                out.unclocked_pulse_pins
            ),
        ));
    }
    if out.conditional_pulse_groups > 0 {
        diags.push(Diag::note(
            "T0132",
            format!(
                "corner '{corner}': {} conditional (when) pulse group(s) combined as the worst \
                 group of their pin (STA-REQ-0801)",
                out.conditional_pulse_groups
            ),
        ));
    }
    let (kept, kinds) = rows.finish();
    for kind in LimitKind::ALL {
        let k = &kinds[kind.index()];
        if k.truncated() > 0 {
            diags.push(Diag::warning(
                "T0134",
                format!(
                    "corner '{corner}': {} of {} {} row(s) kept (row budget {max_rows}); the counts \
                     cover every check (STA-REQ-0810)",
                    k.stored,
                    k.checked,
                    kind.name()
                ),
            ));
        }
    }
    out.rows = kept;
    out.kinds = kinds;
    out
}

/// A row in the bounded heap of its kind: the greatest row (largest slack, then node and clock)
/// is evicted first.
#[derive(Debug, Clone, Copy)]
struct HeapRow(LimitRow);

impl HeapRow {
    fn key(&self) -> (f64, u32, u32) {
        (
            self.0.slack,
            self.0.node.0,
            self.0.clock.map_or(u32::MAX, |c| c.0),
        )
    }
}

impl PartialEq for HeapRow {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for HeapRow {}
impl PartialOrd for HeapRow {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapRow {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.key(), other.key());
        a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
    }
}

/// Counts every row and keeps the `cap` worst rows of each kind (`STA-REQ-0810`).
struct RowCollector {
    cap: usize,
    heaps: [BinaryHeap<HeapRow>; 6],
    kinds: [KindSummary; 6],
}

impl RowCollector {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            heaps: Default::default(),
            kinds: [KindSummary::default(); 6],
        }
    }

    fn unlimited(&mut self, kind: LimitKind) {
        self.kinds[kind.index()].unlimited += 1;
    }

    fn push(&mut self, mut row: LimitRow) {
        if !(row.value.is_finite() && row.limit.is_finite() && row.slack.is_finite()) {
            return;
        }
        row.value = canon(row.value);
        row.limit = canon(row.limit);
        row.slack = canon(row.slack);
        let k = &mut self.kinds[row.kind.index()];
        k.checked += 1;
        if row.slack < 0.0 {
            k.violating += 1;
        }
        k.worst_slack = Some(k.worst_slack.map_or(row.slack, |w| w.min(row.slack)));
        let heap = &mut self.heaps[row.kind.index()];
        let item = HeapRow(row);
        if heap.len() < self.cap {
            heap.push(item);
        } else if self.cap > 0 && heap.peek().is_some_and(|top| item < *top) {
            heap.pop();
            heap.push(item);
        }
    }

    fn finish(self) -> (Vec<LimitRow>, [KindSummary; 6]) {
        let mut kinds = self.kinds;
        let mut rows = Vec::new();
        for (i, heap) in self.heaps.into_iter().enumerate() {
            let sorted = heap.into_sorted_vec();
            kinds[i].stored = sorted.len() as u64;
            rows.extend(sorted.into_iter().map(|h| h.0));
        }
        (rows, kinds)
    }
}

/// `-0.0` as `+0.0`.
#[inline]
fn canon(x: f64) -> f64 {
    if x == 0.0 { 0.0 } else { x }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: LimitKind, node: u32, slack: f64) -> LimitRow {
        LimitRow {
            node: NodeId(node),
            kind,
            tr: None,
            clock: None,
            value: 1.0,
            limit: 1.0 + slack,
            slack,
            source: LimitSource::Liberty,
        }
    }

    // STA-REQ-0810: the counts cover every row, the kept rows are the worst `cap` of each kind,
    // in slack order with ties by node.
    #[test]
    fn bounded_rows_keep_the_worst_and_count_all() {
        let mut c = RowCollector::new(2);
        for (node, slack) in [(5, 0.3), (1, -0.2), (4, 0.1), (2, -0.2), (3, 0.0)] {
            c.push(row(LimitKind::MaxTransition, node, slack));
        }
        c.push(row(LimitKind::MaxFanout, 9, 1.0));
        c.unlimited(LimitKind::MaxFanout);
        let (rows, kinds) = c.finish();
        let t = &kinds[LimitKind::MaxTransition.index()];
        assert_eq!(
            (t.checked, t.violating, t.stored, t.truncated()),
            (5, 2, 2, 3)
        );
        assert_eq!(t.worst_slack, Some(-0.2));
        let kept: Vec<(u32, f64)> = rows
            .iter()
            .filter(|r| r.kind == LimitKind::MaxTransition)
            .map(|r| (r.node.0, r.slack))
            .collect();
        assert_eq!(kept, vec![(1, -0.2), (2, -0.2)]);
        let f = &kinds[LimitKind::MaxFanout.index()];
        assert_eq!((f.checked, f.unlimited, f.stored), (1, 1, 1));
    }

    // STA-REQ-0810: a budget of zero keeps no row and still counts.
    #[test]
    fn zero_budget_counts_only() {
        let mut c = RowCollector::new(0);
        c.push(row(LimitKind::MinPeriod, 1, -1.0));
        c.push(row(LimitKind::MinPeriod, 2, f64::NAN));
        let (rows, kinds) = c.finish();
        assert!(rows.is_empty());
        let p = &kinds[LimitKind::MinPeriod.index()];
        assert_eq!((p.checked, p.violating, p.stored), (1, 1, 0));
    }

    // STA-REQ-0810: the kept rows of a corner are bounded by the budget or the node count.
    #[test]
    fn retained_rows_are_bounded() {
        assert!(size_of::<LimitRow>() <= 48);
        let row = size_of::<LimitRow>() as u64;
        assert_eq!(retained_bytes(10, DEFAULT_MAX_LIMIT_ROWS), 10 * 6 * row);
        assert_eq!(retained_bytes(usize::MAX, 100), 100 * 6 * row);
        assert_eq!(retained_bytes(5, 0), 0);
    }

    #[test]
    fn kind_and_source_names_round_trip() {
        for k in LimitKind::ALL {
            assert_eq!(LimitKind::from_name(k.name()), Some(k));
            assert_eq!(LimitKind::from_u8(k as u8), Some(k));
        }
        for v in 0..4u8 {
            let s = LimitSource::from_u8(v).unwrap();
            assert_eq!(s as u8, v);
            assert!(!s.name().is_empty());
        }
        assert_eq!(LimitSource::from_u8(4), None);
    }
}
