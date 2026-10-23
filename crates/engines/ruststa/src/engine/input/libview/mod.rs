// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The library view: the timing of the cells of each corner (owner: E3).
//!
//! Engine section B. Each corner resolves the netlist's cells by name in its own libraries; a cell
//! defined twice in one corner is an error. The view gives typed timing arcs
//! ([`LibArc`](super::super::types::LibArc)): combinational (also `_rise`/`_fall`), clock edges,
//! the setup, hold, recovery and removal checks, and the three-state enable and disable arcs.
//! Preset and clear arcs are kept but not propagated; minimum pulse width and minimum period arcs
//! become the pulse checks of their pin ([`PulseCheck`], `STA-REQ-0800`); non-sequential, skew,
//! no-change and clock-tree-path arcs are counted per instance as unanalyzed arcs. Table axes are
//! typed by role,
//! up to three, with per-table index overrides. Slew thresholds and the slew derate are recorded
//! per corner.

pub mod thresholds;

use std::collections::BTreeMap;

use rustc_hash::FxHashMap;
use rusteda_compute::timing::{ArcClass, transition_bit};
use rusteda_core::job::Diag;
use rusteda_core::liberty::{Direction, Func, Library, Pin, Table, TimingSense, TimingType};
use rusteda_core::netlist::FrozenNetlist;

use crate::engine::delay::lut::{LutBank, NO_TABLE, RangeTracker, TableId, TableKind};
use crate::engine::types::{LibArc, Tr};

pub use thresholds::CornerThresholds;

/// Unateness (timing sense) of a timing arc (`STA-REQ-0012`, `STA-REQ-0017`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    /// Output transitions in the same direction as the input (or active on rising enable).
    Positive,
    /// Output transitions in the opposite direction to the input (or active on falling enable).
    Negative,
    /// Output may transition in either direction for either input transition.
    Non,
}

impl Sense {
    /// Inverts positive and negative unateness, keeping `Non` unchanged.
    #[must_use]
    pub fn invert(self) -> Self {
        match self {
            Self::Positive => Self::Negative,
            Self::Negative => Self::Positive,
            Self::Non => Self::Non,
        }
    }
}

/// Kind of a propagation timing arc (`STA-REQ-0011`, `STA-REQ-0017`, `STA-REQ-0223`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropArcKind {
    /// Combinational data arc (`combinational`, `combinational_rise`, `combinational_fall`).
    /// `outs` indicates `[can_rise, can_fall]` at the output pin.
    Combinational { outs: [bool; 2] },
    /// Sequential clock-to-output launch arc (`rising_edge` or `falling_edge`).
    Edge(Tr),
    /// Three-state enable arc (`three_state_enable`, `three_state_enable_rise`,
    /// `three_state_enable_fall`). `outs` indicates `[Z -> 1 (cell_rise), Z -> 0 (cell_fall)]`.
    ThreeStateEnable { outs: [bool; 2] },
    /// Three-state disable arc (`three_state_disable`, `three_state_disable_rise`,
    /// `three_state_disable_fall`). `outs` indicates `[0 -> Z (cell_rise), 1 -> Z (cell_fall)]`.
    ThreeStateDisable { outs: [bool; 2] },
}

/// A compiled propagation arc in the library view of a corner, addressed by [`LibArc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropArc {
    /// Owning cell index in [`TimingLib::cells`].
    pub cell: usize,
    /// Source (`related_pin`) pin index in [`TimingCell::pins`].
    pub from: usize,
    /// Sink (output) pin index in [`TimingCell::pins`].
    pub to: usize,
    /// Propagation arc kind.
    pub kind: PropArcKind,
    /// Timing sense (`positive_unate`, `negative_unate`, or `non_unate`).
    pub sense: Sense,
    /// Delay lookup tables `[cell_rise, cell_fall]` indexed by output [`Tr`].
    pub delay: [TableId; 2],
    /// Output transition lookup tables `[rise_transition, fall_transition]` indexed by output [`Tr`].
    pub slew: [TableId; 2],
    /// For a transparent latch data-to-output arc (`D -> Q`), the index of the corresponding
    /// setup check in [`TimingCell::checks`] (`STA-REQ-0213`).
    pub latch_check: Option<usize>,
}

impl PropArc {
    /// Returns `[can_rise, can_fall]` at the output pin when `input` transitions on `from`
    /// during combinational / three-state level propagation (`STA-REQ-0017`, `STA-REQ-0112`,
    /// `STA-REQ-0223`).
    #[must_use]
    pub fn out_transitions(&self, input: Tr) -> [bool; 2] {
        match self.kind {
            PropArcKind::Combinational { outs } => {
                let mut o = match self.sense {
                    Sense::Positive => [input == Tr::Rise, input == Tr::Fall],
                    Sense::Negative => [input == Tr::Fall, input == Tr::Rise],
                    Sense::Non => [true, true],
                };
                o[0] &= outs[0];
                o[1] &= outs[1];
                o
            }
            PropArcKind::ThreeStateEnable { outs } | PropArcKind::ThreeStateDisable { outs } => {
                let active = match self.sense {
                    Sense::Positive => input == Tr::Rise,
                    Sense::Negative => input == Tr::Fall,
                    Sense::Non => true,
                };
                if active { outs } else { [false, false] }
            }
            PropArcKind::Edge(_) => [false, false],
        }
    }

    /// Returns `[can_rise, can_fall]` launched at the output pin when `clock_edge` occurs on the
    /// clock pin of a sequential `rising_edge` / `falling_edge` arc.
    #[must_use]
    pub fn edge_transitions(&self, clock_edge: Tr) -> [bool; 2] {
        match self.kind {
            PropArcKind::Edge(edge) if edge == clock_edge => [true, true],
            _ => [false, false],
        }
    }

    /// Encodes the enabled `(ti, to)` transition pairs into the 4-bit mask of [`ArcClass::flags`].
    #[must_use]
    pub fn transition_flags(&self) -> u32 {
        let mut flags = 0u32;
        for ti in Tr::ALL {
            let outs = self.out_transitions(ti);
            for to in Tr::ALL {
                if outs[to.index()] {
                    flags |= transition_bit(ti.index(), to.index());
                }
            }
        }
        flags
    }
}

/// Kind of a timing check arc (`STA-REQ-0011`, `STA-REQ-0202`, `STA-REQ-0203`, `STA-REQ-0204`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckArcKind {
    /// `setup_rising` or `setup_falling`.
    Setup,
    /// `hold_rising` or `hold_falling`.
    Hold,
    /// `recovery_rising` or `recovery_falling`.
    Recovery,
    /// `removal_rising` or `removal_falling`.
    Removal,
}

impl CheckArcKind {
    /// True for late-path checks (`Setup` and `Recovery`).
    #[must_use]
    pub fn is_setup_like(self) -> bool {
        matches!(self, Self::Setup | Self::Recovery)
    }

    /// True for early-path checks (`Hold` and `Removal`).
    #[must_use]
    pub fn is_hold_like(self) -> bool {
        matches!(self, Self::Hold | Self::Removal)
    }
}

/// A compiled timing check on a cell (`setup`, `hold`, `recovery`, or `removal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellCheck {
    /// Constrained (data / async control) pin index in [`TimingCell::pins`].
    pub data: usize,
    /// Related (clock) pin index in [`TimingCell::pins`].
    pub clock: usize,
    /// Check kind.
    pub kind: CheckArcKind,
    /// Active clock-pin transition of the check.
    pub edge: Tr,
    /// Constraint tables `[rise_constraint, fall_constraint]` indexed by data [`Tr`].
    pub table: [TableId; 2],
}

/// Kind of an asynchronous preset or clear arc (`STA-REQ-0011`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetClearKind {
    /// `preset` timing arc.
    Preset,
    /// `clear` timing arc.
    Clear,
}

/// A recorded `preset` or `clear` arc kept in the library view and excluded from propagation
/// unless enabled (`STA-REQ-0011`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetClearArc {
    /// Source (`related_pin`) pin index in [`TimingCell::pins`].
    pub from: usize,
    /// Sink (output) pin index in [`TimingCell::pins`].
    pub to: usize,
    /// Whether this arc is `preset` or `clear`.
    pub kind: PresetClearKind,
    /// Timing sense.
    pub sense: Sense,
    /// Delay tables `[cell_rise, cell_fall]`.
    pub delay: [TableId; 2],
    /// Output transition tables `[rise_transition, fall_transition]`.
    pub slew: [TableId; 2],
}

/// Kind of a pulse check of a clock pin (`STA-REQ-0800`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PulseCheckKind {
    /// `min_pulse_width`: `rise_constraint` is the minimum width of the high pulse and
    /// `fall_constraint` the minimum width of the low pulse.
    Width,
    /// `minimum_period`: `rise_constraint` and `fall_constraint` are minimum periods measured
    /// between rising and between falling edges.
    Period,
}

/// A `min_pulse_width` or `minimum_period` timing group of a cell pin (`STA-REQ-0800`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulseCheck {
    /// The constrained pin (the pin that owns the timing group) in [`TimingCell::pins`].
    pub pin: usize,
    /// Pulse width or period.
    pub kind: PulseCheckKind,
    /// Constraint tables `[rise_constraint, fall_constraint]`, indexed by the transition time of
    /// the pin at the edge that opens the pulse (or at the edge of the period).
    pub table: [TableId; 2],
    /// True when the group has a `when` condition: every condition is taken as possible and the
    /// worst group of a pin applies (`STA-REQ-0801`).
    pub conditional: bool,
}

/// A pin of a [`TimingCell`] with its capacitances, design-rule limits, and Boolean functions.
#[derive(Debug, Clone, PartialEq)]
pub struct CellPin {
    /// Pin name.
    pub name: String,
    /// Pin direction in Liberty.
    pub direction: Direction,
    /// True for `Input` and `Inout`.
    pub input: bool,
    /// True for `Output` and `Inout`.
    pub output: bool,
    /// True when `clock: true` is set on the pin.
    pub is_clock: bool,
    /// Pin capacitance in pF indexed `[tr][an]` (`[Rise/Fall][Early/Late]`, `STA-REQ-0110`).
    /// Computed for all pins (both load pins and driver output pins, `STA-REQ-0121`).
    pub cap: [[f64; 2]; 2],
    /// Maximum capacitance limit in pF, with library default applied (`STA-REQ-0120`).
    pub max_capacitance: Option<f64>,
    /// Maximum transition limit in ns, with library default applied (`STA-REQ-0120`).
    pub max_transition: Option<f64>,
    /// Maximum fanout limit, with library default applied.
    pub max_fanout: Option<f64>,
    /// Minimum capacitance in pF, when stated on the pin.
    pub min_capacitance: Option<f64>,
    /// Dimensionless fanout load, with library default applied.
    pub fanout_load: f64,
    /// Truth table of `function` over the cell's input pins (for combinational cells with `<= 6`
    /// inputs).
    pub func: Option<(Vec<usize>, u64)>,
    /// Truth table of `three_state` over the cell's input pins (for cells with `<= 6` inputs).
    pub three_state_func: Option<(Vec<usize>, u64)>,
}

/// Timing view of one cell within a corner (`STA-REQ-0010`..`STA-REQ-0015`).
#[derive(Debug, Clone, PartialEq)]
pub struct TimingCell {
    /// Cell name.
    pub name: String,
    /// Name of the library that defined this cell in the corner.
    pub library: String,
    /// Cell area from Liberty.
    pub area: f64,
    /// True when the cell has `ff` or `latch` groups or clock-edge / setup / hold arcs.
    pub is_sequential: bool,
    /// Cell pins in library order.
    pub pins: Vec<CellPin>,
    /// [`LibArc`] ids of the propagation arcs of this cell in [`TimingLib::arcs`].
    pub arcs: Vec<LibArc>,
    /// Timing check arcs (`setup`, `hold`, `recovery`, `removal`).
    pub checks: Vec<CellCheck>,
    /// Recorded `preset` and `clear` arcs (`STA-REQ-0011`).
    pub preset_clear_arcs: Vec<PresetClearArc>,
    /// Minimum pulse width and minimum period checks of the cell's pins (`STA-REQ-0800`).
    pub pulse_checks: Vec<PulseCheck>,
    /// Count of unanalyzed timing arcs on this cell grouped by `timing_type` (`STA-REQ-0015`).
    pub unanalyzed_by_type: BTreeMap<String, usize>,
    /// Total count of unanalyzed timing arcs on a single instance of this cell (`STA-REQ-0015`).
    pub unanalyzed_arcs: usize,
    /// Informational notes recorded while compiling the cell's timing arcs.
    pub notes: Vec<String>,
}

impl TimingCell {
    /// Looks up the index of a pin by name.
    #[must_use]
    pub fn pin(&self, name: &str) -> Option<usize> {
        self.pins.iter().position(|p| p.name == name)
    }
}

/// Per-corner compiled library view (`STA-REQ-0010`..`STA-REQ-0016`).
#[derive(Debug, Clone)]
pub struct TimingLib {
    /// Corner name.
    pub corner: String,
    /// Cells in definition order.
    pub cells: Vec<TimingCell>,
    /// Cell lookup by name.
    pub by_name: FxHashMap<String, usize>,
    /// All propagation arcs of the corner, indexed by [`LibArc`].
    pub arcs: Vec<PropArc>,
    /// Compiled lookup tables and [`ArcClass`] descriptors for the corner.
    pub lut_bank: LutBank,
    /// Trip-point thresholds and `slew_derate_from_library` of the corner (`STA-REQ-0016`).
    pub thresholds: CornerThresholds,
    /// Index in `lut_bank.classes` of the wire [`ArcClass`] (`flags = ArcClass::WIRE`).
    pub wire_class: u32,
    /// Time unit of the first library in ns (or `1.0` when empty).
    pub time_unit_ns: f64,
    /// Capacitive load unit of the first library in pF (or `1.0` when empty).
    pub cap_unit_pf: f64,
}

impl TimingLib {
    /// Builds the library view for `corner` from parsed [`Library`] slices, reading
    /// [`CornerThresholds`] from the parsed library metadata.
    pub fn from_corner(corner: impl Into<String>, libs: &[Library]) -> Result<Self, Diag> {
        let pairs: Vec<(&Library, CornerThresholds)> = libs
            .iter()
            .map(|l| (l, CornerThresholds::from_library(l)))
            .collect();
        Self::from_corner_with_thresholds(corner, &pairs)
    }

    /// Reads Liberty source texts `(filename, text)` and builds the library view for `corner`,
    /// extracting [`CornerThresholds`] directly from each library's header (`STA-REQ-0016`).
    pub fn from_liberty_texts(
        corner: impl Into<String>,
        sources: &[(&str, &str)],
    ) -> Result<Self, Diag> {
        let mut parsed = Vec::with_capacity(sources.len());
        for &(file, text) in sources {
            let lib = rusteda_formats::liberty::read_text(text, file)
                .map_err(|e| Diag::error("T0011", e.to_string()))?;
            let thr = CornerThresholds::from_library(&lib);
            parsed.push((lib, thr));
        }
        let refs: Vec<(&Library, CornerThresholds)> = parsed.iter().map(|(l, t)| (l, *t)).collect();
        Self::from_corner_with_thresholds(corner, &refs)
    }

    /// Builds the library view for `corner` from `(library, thresholds)` pairs.
    ///
    /// - Rejects libraries within the same corner that disagree on trip-point thresholds or
    ///   `slew_derate_from_library` with `T0013` (`STA-REQ-0016`).
    /// - Rejects duplicate cell definitions within the same corner with `T0012` (`STA-REQ-0010`).
    /// - Rejects malformed lookup tables with `T0014` (`STA-REQ-0014`, `STA-REQ-0114`).
    pub fn from_corner_with_thresholds(
        corner: impl Into<String>,
        libs: &[(&Library, CornerThresholds)],
    ) -> Result<Self, Diag> {
        let corner = corner.into();
        let thresholds = libs
            .first()
            .map_or_else(CornerThresholds::default, |(_, t)| *t);
        if let Some((first_lib, first_thr)) = libs.first() {
            for (lib, thr) in &libs[1..] {
                first_thr.check_compatible(&corner, &first_lib.name, thr, &lib.name)?;
            }
        }

        let mut view = Self {
            corner: corner.clone(),
            cells: Vec::new(),
            by_name: FxHashMap::default(),
            arcs: Vec::new(),
            lut_bank: LutBank::default(),
            thresholds,
            wire_class: 0,
            time_unit_ns: libs.first().map_or(1.0, |(l, _)| l.time_unit_ns),
            cap_unit_pf: libs.first().map_or(1.0, |(l, _)| l.cap_unit_pf),
        };

        for &(lib, _) in libs {
            let (tu, cu) = (lib.time_unit_ns, lib.cap_unit_pf);
            for c in &lib.cells {
                if let Some(&prev_idx) = view.by_name.get(&c.name) {
                    let prev_lib = &view.cells[prev_idx].library;
                    return Err(Diag::error(
                        "T0012",
                        format!(
                            "corner '{corner}': cell '{}' is defined more than once \
                             (in library '{prev_lib}' and library '{}')",
                            c.name, lib.name
                        ),
                    ));
                }
                let cell_idx = view.cells.len();
                let mut tc = TimingCell {
                    name: c.name.clone(),
                    library: lib.name.clone(),
                    area: c.area,
                    is_sequential: !c.sequentials.is_empty(),
                    pins: c.pins.iter().map(|p| build_pin(lib, p, tu, cu)).collect(),
                    arcs: Vec::new(),
                    checks: Vec::new(),
                    preset_clear_arcs: Vec::new(),
                    pulse_checks: Vec::new(),
                    unanalyzed_by_type: BTreeMap::new(),
                    unanalyzed_arcs: 0,
                    notes: Vec::new(),
                };

                let mut cell_arc_ranges = Vec::new();
                for (to, p) in c.pins.iter().enumerate() {
                    for a in &p.arcs {
                        compile_arc(
                            &mut view.lut_bank,
                            &mut view.arcs,
                            &mut tc,
                            &mut cell_arc_ranges,
                            lib,
                            cell_idx,
                            to,
                            p,
                            a,
                        )?;
                    }
                }

                // Populate truth tables for combinational and three-state functions (`<= 6` inputs).
                let ins: Vec<usize> = (0..tc.pins.len())
                    .filter(|&k| tc.pins[k].input && !tc.pins[k].output)
                    .collect();
                if ins.len() <= 6 {
                    let owned: Vec<String> = ins.iter().map(|&k| tc.pins[k].name.clone()).collect();
                    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
                    for (k, p) in c.pins.iter().enumerate() {
                        if c.sequentials.is_empty()
                            && let Some(tt) =
                                p.function.as_ref().and_then(|f| f.truth_table(&names))
                        {
                            tc.pins[k].func = Some((ins.clone(), tt));
                        }
                        if let Some(tt) = p
                            .three_state_function
                            .as_ref()
                            .and_then(|f| f.truth_table(&names))
                        {
                            tc.pins[k].three_state_func = Some((ins.clone(), tt));
                        }
                    }
                }

                // Link transparent latch data-to-Q arcs to their setup checks (STA-REQ-0213) and
                // finalize `ArcClass` descriptors in `lut_bank.classes`.
                for &lib_arc in &cell_arc_ranges {
                    let arc_idx = lib_arc.index();
                    let a_from = view.arcs[arc_idx].from;
                    let a_to = view.arcs[arc_idx].to;
                    let a_kind = view.arcs[arc_idx].kind;
                    let mut setup = [NO_TABLE; 2];
                    if matches!(a_kind, PropArcKind::Combinational { .. }) {
                        let found = tc.checks.iter().position(|ch| {
                            ch.kind == CheckArcKind::Setup
                                && ch.data == a_from
                                && tc.arcs.iter().any(|&ea| {
                                    let e = &view.arcs[ea.index()];
                                    matches!(e.kind, PropArcKind::Edge(_))
                                        && e.from == ch.clock
                                        && e.to == a_to
                                })
                        });
                        if let Some(ch_idx) = found {
                            view.arcs[arc_idx].latch_check = Some(ch_idx);
                            setup = tc.checks[ch_idx].table;
                        }
                    }
                    let arc_ref = &view.arcs[arc_idx];
                    view.lut_bank.classes[arc_idx] = ArcClass {
                        flags: arc_ref.transition_flags(),
                        delay: arc_ref.delay,
                        slew: arc_ref.slew,
                        setup,
                    };
                }

                view.by_name.insert(c.name.clone(), cell_idx);
                view.cells.push(tc);
            }
        }

        // Append one shared wire `ArcClass` after all cell `LibArc` classes.
        let wire_class = view.lut_bank.classes.len() as u32;
        view.lut_bank.classes.push(ArcClass::wire());
        view.wire_class = wire_class;

        Ok(view)
    }

    /// Looks up a [`TimingCell`] by name.
    #[must_use]
    pub fn cell(&self, name: &str) -> Option<&TimingCell> {
        self.by_name.get(name).map(|&i| &self.cells[i])
    }

    /// Looks up a cell index in [`Self::cells`] by name.
    #[must_use]
    pub fn cell_id(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// Returns the [`PropArc`] for `id`.
    #[must_use]
    pub fn arc(&self, id: LibArc) -> &PropArc {
        &self.arcs[id.index()]
    }

    /// Evaluates a timing check table (`setup`, `hold`, `recovery`, `removal`) for constrained
    /// pin transition `data_tr`, related (clock) pin slew `related_slew` (ns), constrained (data)
    /// pin slew `constrained_slew` (ns), and output load `load` (pF) (`STA-REQ-0108`,
    /// `STA-REQ-0109`).
    #[must_use]
    pub fn eval_check(
        &self,
        check: &CellCheck,
        data_tr: Tr,
        related_slew: f64,
        constrained_slew: f64,
        load: f64,
    ) -> f64 {
        self.lut_bank.eval(
            check.table[data_tr.index()],
            [related_slew, constrained_slew, load],
        )
    }

    /// Evaluates a timing check table and records any axis extrapolation in `tracker`
    /// (`STA-REQ-0120`).
    pub fn eval_check_tracked(
        &self,
        check: &CellCheck,
        data_tr: Tr,
        related_slew: f64,
        constrained_slew: f64,
        load: f64,
        tracker: &mut RangeTracker,
    ) -> f64 {
        self.lut_bank.eval_tracked(
            check.table[data_tr.index()],
            [related_slew, constrained_slew, load],
            tracker,
        )
    }

    /// Evaluates the constraint of pulse check `check` for transition `tr` (`rise_constraint` for
    /// [`Tr::Rise`]) at the pin's transition time `slew` (ns), recording any axis extrapolation
    /// in `tracker`; `None` when the group has no table for `tr` (`STA-REQ-0800`).
    pub fn eval_pulse(
        &self,
        check: &PulseCheck,
        tr: Tr,
        slew: f64,
        tracker: &mut RangeTracker,
    ) -> Option<f64> {
        let id = check.table[tr.index()];
        (id != NO_TABLE).then(|| self.lut_bank.eval_tracked(id, [slew, slew, 0.0], tracker))
    }

    /// Counts unanalyzed arcs across all instances in `netlist` and emits one aggregated `T0015`
    /// warning per unanalyzed `timing_type` (`STA-REQ-0015`).
    #[must_use]
    pub fn unanalyzed_arc_warnings(&self, netlist: &FrozenNetlist<'_>) -> (usize, Vec<Diag>) {
        let names = netlist.instances.iter().filter_map(|inst| {
            let cell_str_id = netlist.cells[inst.cell as usize];
            netlist.string(cell_str_id).ok()
        });
        self.unanalyzed_arc_warnings_for_cells(names)
    }

    /// Counts unanalyzed arcs across an iterator of instantiated cell names and emits one
    /// aggregated `T0015` warning per unanalyzed `timing_type` (`STA-REQ-0015`).
    #[must_use]
    pub fn unanalyzed_arc_warnings_for_cells<'a>(
        &self,
        inst_cells: impl IntoIterator<Item = &'a str>,
    ) -> (usize, Vec<Diag>) {
        let mut total = 0usize;
        let mut by_type: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for cell_name in inst_cells {
            let Some(tc) = self.cell(cell_name) else {
                continue;
            };
            total += tc.unanalyzed_arcs;
            for (tt, &cnt) in &tc.unanalyzed_by_type {
                let entry = by_type.entry(tt.clone()).or_insert((0, 0));
                entry.0 += cnt;
                entry.1 += 1;
            }
        }
        let diags = by_type
            .into_iter()
            .map(|(tt, (arc_cnt, inst_cnt))| {
                Diag::warning(
                    "T0015",
                    format!(
                        "corner '{}': {arc_cnt} unanalyzed '{tt}' arc(s) across {inst_cnt} instance(s)",
                        self.corner
                    ),
                )
            })
            .collect();
        (total, diags)
    }
}

fn build_pin(lib: &Library, p: &Pin, tu: f64, cu: f64) -> CellPin {
    let base = if p.capacitance_given || p.capacitance != 0.0 {
        p.capacitance
    } else {
        lib.pin_capacitance(p)
    };
    let cap_tr = |plain: Option<f64>, range: Option<(f64, f64)>| -> [f64; 2] {
        let v = plain.unwrap_or(base);
        let (lo, hi) = range.unwrap_or((v, v));
        [cu * lo, cu * hi]
    };
    CellPin {
        name: p.name.clone(),
        direction: p.direction,
        input: matches!(p.direction, Direction::Input | Direction::Inout),
        output: matches!(p.direction, Direction::Output | Direction::Inout),
        is_clock: p.is_clock,
        cap: [
            cap_tr(p.rise_capacitance, p.rise_capacitance_range),
            cap_tr(p.fall_capacitance, p.fall_capacitance_range),
        ],
        max_capacitance: lib.pin_max_capacitance(p).map(|v| v * cu),
        max_transition: lib.pin_max_transition(p).map(|v| v * tu),
        max_fanout: lib.pin_max_fanout(p),
        min_capacitance: p.min_capacitance.map(|v| v * cu),
        fanout_load: lib.pin_fanout_load(p),
        func: None,
        three_state_func: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_arc(
    lut_bank: &mut LutBank,
    all_arcs: &mut Vec<PropArc>,
    tc: &mut TimingCell,
    cell_arc_ranges: &mut Vec<LibArc>,
    lib: &Library,
    cell_idx: usize,
    to: usize,
    p: &Pin,
    a: &rusteda_core::liberty::TimingArc,
) -> Result<(), Diag> {
    let (tu, cu) = (lib.time_unit_ns, lib.cap_unit_pf);
    let tt_raw = a.timing_type.trim();
    let tt = if tt_raw.is_empty() {
        TimingType::Combinational
    } else {
        TimingType::from_name(tt_raw)
    };

    // Pulse checks (`STA-REQ-0800`) constrain the pin that owns the group; their `related_pin`
    // names the same pin or is omitted, so they are classified before `related_pin` is resolved.
    if matches!(tt, TimingType::MinPulseWidth | TimingType::MinimumPeriod) {
        let kind = if tt == TimingType::MinPulseWidth {
            PulseCheckKind::Width
        } else {
            PulseCheckKind::Period
        };
        let mut add_check = |t: &Option<Table>, attr: &str| -> Result<TableId, Diag> {
            match t {
                Some(tab) => lut_bank.add_table(
                    tab,
                    &lib.templates,
                    TableKind::Check,
                    tu,
                    cu,
                    format!("cell {} pin {} ({}) {attr}", tc.name, p.name, tt.name()),
                ),
                None => Ok(NO_TABLE),
            }
        };
        let table = [
            add_check(&a.rise_constraint, "rise_constraint")?,
            add_check(&a.fall_constraint, "fall_constraint")?,
        ];
        if table == [NO_TABLE; 2] {
            tc.notes.push(format!(
                "pin {}: '{}' group without a constraint table ignored (STA-REQ-0800)",
                p.name,
                tt.name()
            ));
        } else {
            tc.pulse_checks.push(PulseCheck {
                pin: to,
                kind,
                table,
                conditional: a.when_text.is_some(),
            });
        }
        return Ok(());
    }

    // Unanalyzed arc types (some may self-reference or omit `related_pin`, so classify them
    // before resolving `related_pin`).
    if is_unanalyzed_type(&tt) {
        let key = tt.name().to_string();
        *tc.unanalyzed_by_type.entry(key).or_insert(0) += 1;
        tc.unanalyzed_arcs += 1;
        return Ok(());
    }

    let Some(from) = tc.pin(&a.related_pin) else {
        tc.notes.push(format!(
            "pin {}: arc from unknown pin '{}' ignored",
            p.name, a.related_pin
        ));
        return Ok(());
    };

    let mut add_tab = |t: &Option<Table>, kind: TableKind, attr: &str| -> Result<TableId, Diag> {
        match t {
            Some(tab) => lut_bank.add_table(
                tab,
                &lib.templates,
                kind,
                tu,
                cu,
                format!(
                    "cell {} pin {} ({} -> {}) {attr}",
                    tc.name, p.name, a.related_pin, p.name
                ),
            ),
            None => Ok(NO_TABLE),
        }
    };

    let check = match tt {
        TimingType::SetupRising => Some((CheckArcKind::Setup, Tr::Rise)),
        TimingType::SetupFalling => Some((CheckArcKind::Setup, Tr::Fall)),
        TimingType::HoldRising => Some((CheckArcKind::Hold, Tr::Rise)),
        TimingType::HoldFalling => Some((CheckArcKind::Hold, Tr::Fall)),
        TimingType::RecoveryRising => Some((CheckArcKind::Recovery, Tr::Rise)),
        TimingType::RecoveryFalling => Some((CheckArcKind::Recovery, Tr::Fall)),
        TimingType::RemovalRising => Some((CheckArcKind::Removal, Tr::Rise)),
        TimingType::RemovalFalling => Some((CheckArcKind::Removal, Tr::Fall)),
        _ => None,
    };
    if let Some((kind, edge)) = check {
        tc.is_sequential = true;
        let table = [
            add_tab(&a.rise_constraint, TableKind::Check, "rise_constraint")?,
            add_tab(&a.fall_constraint, TableKind::Check, "fall_constraint")?,
        ];
        tc.checks.push(CellCheck {
            data: to,
            clock: from,
            kind,
            edge,
            table,
        });
        return Ok(());
    }

    if matches!(tt, TimingType::Preset | TimingType::Clear) {
        let kind = if tt == TimingType::Preset {
            PresetClearKind::Preset
        } else {
            PresetClearKind::Clear
        };
        let sense = resolve_sense(a, p, &tt);
        let delay = [
            add_tab(&a.cell_rise, TableKind::Delay, "cell_rise")?,
            add_tab(&a.cell_fall, TableKind::Delay, "cell_fall")?,
        ];
        let slew = [
            add_tab(&a.rise_transition, TableKind::Slew, "rise_transition")?,
            add_tab(&a.fall_transition, TableKind::Slew, "fall_transition")?,
        ];
        tc.preset_clear_arcs.push(PresetClearArc {
            from,
            to,
            kind,
            sense,
            delay,
            slew,
        });
        return Ok(());
    }

    let kind = match tt {
        TimingType::Combinational => PropArcKind::Combinational { outs: [true, true] },
        TimingType::CombinationalRise => PropArcKind::Combinational {
            outs: [true, false],
        },
        TimingType::CombinationalFall => PropArcKind::Combinational {
            outs: [false, true],
        },
        TimingType::RisingEdge => {
            tc.is_sequential = true;
            PropArcKind::Edge(Tr::Rise)
        }
        TimingType::FallingEdge => {
            tc.is_sequential = true;
            PropArcKind::Edge(Tr::Fall)
        }
        TimingType::ThreeStateEnable => PropArcKind::ThreeStateEnable { outs: [true, true] },
        TimingType::ThreeStateEnableRise => PropArcKind::ThreeStateEnable {
            outs: [true, false],
        },
        TimingType::ThreeStateEnableFall => PropArcKind::ThreeStateEnable {
            outs: [false, true],
        },
        TimingType::ThreeStateDisable => PropArcKind::ThreeStateDisable { outs: [true, true] },
        TimingType::ThreeStateDisableRise => PropArcKind::ThreeStateDisable {
            outs: [true, false],
        },
        TimingType::ThreeStateDisableFall => PropArcKind::ThreeStateDisable {
            outs: [false, true],
        },
        other => {
            let key = other.name().to_string();
            *tc.unanalyzed_by_type.entry(key).or_insert(0) += 1;
            tc.unanalyzed_arcs += 1;
            return Ok(());
        }
    };

    let sense = resolve_sense(a, p, &tt);
    let delay = [
        add_tab(&a.cell_rise, TableKind::Delay, "cell_rise")?,
        add_tab(&a.cell_fall, TableKind::Delay, "cell_fall")?,
    ];
    let slew = [
        add_tab(&a.rise_transition, TableKind::Slew, "rise_transition")?,
        add_tab(&a.fall_transition, TableKind::Slew, "fall_transition")?,
    ];
    let outs_mask = match kind {
        PropArcKind::Combinational { outs }
        | PropArcKind::ThreeStateEnable { outs }
        | PropArcKind::ThreeStateDisable { outs } => outs,
        PropArcKind::Edge(_) => [true, true],
    };
    for (k, &t) in delay.iter().enumerate() {
        if outs_mask[k] && t == NO_TABLE {
            tc.notes.push(format!(
                "arc {} -> {}: no {} delay table, delay 0 (STA-REQ-0113)",
                a.related_pin,
                p.name,
                if k == 0 { "rise" } else { "fall" }
            ));
        }
    }

    let lib_arc = LibArc(all_arcs.len() as u32);
    all_arcs.push(PropArc {
        cell: cell_idx,
        from,
        to,
        kind,
        sense,
        delay,
        slew,
        latch_check: None,
    });
    lut_bank.classes.push(ArcClass {
        flags: 0,
        delay,
        slew,
        setup: [NO_TABLE; 2],
    });
    tc.arcs.push(lib_arc);
    cell_arc_ranges.push(lib_arc);
    Ok(())
}

fn is_unanalyzed_type(tt: &TimingType) -> bool {
    matches!(
        tt,
        TimingType::NonSeqSetupRising
            | TimingType::NonSeqSetupFalling
            | TimingType::NonSeqHoldRising
            | TimingType::NonSeqHoldFalling
            | TimingType::SkewRising
            | TimingType::SkewFalling
            | TimingType::NochangeHighHigh
            | TimingType::NochangeHighLow
            | TimingType::NochangeLowHigh
            | TimingType::NochangeLowLow
            | TimingType::MaxClockTreePath
            | TimingType::MinClockTreePath
            | TimingType::Other(_)
    )
}

/// Resolves the [`Sense`] of `a` from its explicit `timing_sense` attribute or derives it from the
/// pin's `function` / `three_state` expression (`STA-REQ-0012`, `STA-REQ-0017`).
fn resolve_sense(a: &rusteda_core::liberty::TimingArc, p: &Pin, tt: &TimingType) -> Sense {
    match a.sense() {
        Some(TimingSense::PositiveUnate) => return Sense::Positive,
        Some(TimingSense::NegativeUnate) => return Sense::Negative,
        Some(TimingSense::NonUnate) => return Sense::Non,
        _ => {}
    }
    match tt {
        TimingType::ThreeStateDisable
        | TimingType::ThreeStateDisableRise
        | TimingType::ThreeStateDisableFall => {
            derive_sense_from_func(p.three_state_function.as_ref(), &a.related_pin)
        }
        TimingType::ThreeStateEnable
        | TimingType::ThreeStateEnableRise
        | TimingType::ThreeStateEnableFall => {
            derive_sense_from_func(p.three_state_function.as_ref(), &a.related_pin).invert()
        }
        TimingType::RisingEdge | TimingType::FallingEdge => Sense::Non,
        _ => derive_sense_from_func(p.function.as_ref(), &a.related_pin),
    }
}

/// Derives unateness of Boolean expression `func` with respect to input variable `related`
/// (`STA-REQ-0012`).
fn derive_sense_from_func(func: Option<&Func>, related: &str) -> Sense {
    let Some(f) = func else {
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
