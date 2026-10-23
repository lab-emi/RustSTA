// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Compiled constraint data model for the RustSTA timing engine (owner: E2, then E12).
//!
//! All numeric values in [`CompiledConstraints`] are stored in canonical internal units:
//! time in nanoseconds (`ns`), capacitance in picofarads (`pF`), and resistance in kilo-ohms
//! (`kOhm`). Every symbolic [`Obj`](rusteda_formats::sdc::Obj) query from `CONSTR v1` is resolved
//! against the [`FrozenNetlist`](rusteda_core::netlist::FrozenNetlist) and corner libraries into
//! typed engine identifiers ([`NodeId`], [`NetRow`], [`ClockId`], [`CornerId`], instance indices,
//! and library cell/pin references).

use std::collections::BTreeMap;
use std::fmt;

use rusteda_core::diag::{Diag, Location, Severity};
use rusteda_core::liberty::Library;
use rusteda_formats::sdc::{
    AnalysisType, CaseValue, ClockGroupsKind, EarlyLateScope, EdgeScope, MinMaxScope,
    SetupHoldScope, SourceRecord, StartEndScope, Units as SdcUnits, WireLoadMode,
};

use crate::engine::types::{An, ClockId, CornerId, NetRow, NodeId, Tr};

/// Corner library slice passed to [`CompiledConstraints::compile`].
#[derive(Debug, Clone, Copy)]
pub struct CornerLibs<'a> {
    /// Corner name (e.g. `"default"`, `"fast"`, `"slow"`).
    pub name: &'a str,
    /// Ordered Liberty libraries for this corner (first definition of a cell wins).
    pub libs: &'a [Library],
}

impl<'a> CornerLibs<'a> {
    /// Creates a named corner library slice.
    #[must_use]
    pub const fn new(name: &'a str, libs: &'a [Library]) -> Self {
        Self { name, libs }
    }

    /// Creates a single `"default"` corner from `libs`.
    #[must_use]
    pub const fn default_corner(libs: &'a [Library]) -> Self {
        Self {
            name: "default",
            libs,
        }
    }
}

/// Resolved unit conversion scales for one analysis corner (`STA-REQ-0011`).
#[derive(Debug, Clone, PartialEq)]
pub struct CornerUnits {
    /// Corner index.
    pub corner: CornerId,
    /// Corner name.
    pub name: String,
    /// Scale factor applied to raw script time numbers to obtain `1 ns`.
    pub time_to_ns: f64,
    /// Scale factor applied to raw script capacitance numbers to obtain `1 pF`.
    pub cap_to_pf: f64,
    /// Scale factor applied to raw script resistance numbers to obtain `1 kOhm`.
    pub res_to_kohm: f64,
    /// Scale factor applied to raw script voltage numbers to obtain `1 V`.
    pub voltage_to_v: f64,
    /// Scale factor applied to raw script current numbers to obtain `1 mA`.
    pub current_to_ma: f64,
    /// Scale factor applied to raw script power numbers to obtain `1 nW`.
    pub power_to_nw: f64,
}

/// Reference to an instance pin in the [`FrozenNetlist`](rusteda_core::netlist::FrozenNetlist),
/// covering both connected pins (which also have a [`NodeId`]) and unconnected cell pins.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinRef {
    /// Instance index in `FrozenNetlist::instances`.
    pub inst: u32,
    /// Local pin index in `FrozenNetlist::cell_pins(cell)`.
    pub cell_pin: u16,
    /// Graph [`NodeId`] if this pin is connected in `FrozenNetlist::pin_net`, else `None`.
    pub node: Option<NodeId>,
    /// Canonical hierarchical pin name `<instance>/<pin>`.
    pub full_name: String,
}

/// Resolved reference to a Liberty library cell (`get_lib_cells`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibCellRef {
    /// Library name.
    pub library: String,
    /// Cell name.
    pub cell: String,
}

impl LibCellRef {
    /// Canonical `<library>/<cell>` name.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.library, self.cell)
    }
}

/// Resolved reference to a Liberty library pin (`get_lib_pins`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibPinRef {
    /// Library name.
    pub library: String,
    /// Cell name.
    pub cell: String,
    /// Pin name.
    pub pin: String,
}

impl LibPinRef {
    /// Canonical `<library>/<cell>/<pin>` name.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}/{}/{}", self.library, self.cell, self.pin)
    }
}

/// A resolved design or library object produced by evaluating an [`Obj`](rusteda_formats::sdc::Obj).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResolvedObj {
    /// Top-level port bit (`NodeId(0..n_port_bits)`).
    Port(NodeId),
    /// Instance pin (`PinRef`, with `node: Some(NodeId)` when connected).
    Pin(PinRef),
    /// Cell instance index in `FrozenNetlist::instances`.
    Cell(u32),
    /// Net row in `FrozenNetlist::nets`.
    Net(NetRow),
    /// Defined clock (`ClockId`).
    Clock(ClockId),
    /// Library cell (`get_lib_cells`).
    LibCell(LibCellRef),
    /// Library pin (`get_lib_pins`).
    LibPin(LibPinRef),
    /// Top-level design (`current_design`).
    Design(String),
}

/// Explicit unit overrides recorded from `set_units` commands (`SDC-REQ-0034`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExplicitUnits {
    pub time: Option<String>,
    pub capacitance: Option<String>,
    pub resistance: Option<String>,
    pub voltage: Option<String>,
    pub current: Option<String>,
    pub power: Option<String>,
}

/// Compiled clock definition (`create_clock`, `set_clock_latency`, `set_clock_transition`,
/// `set_clock_uncertainty`; `STA-REQ-0021`).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledClock {
    /// Dense clock identifier (`ClockId(0..)`).
    pub id: ClockId,
    /// Clock name.
    pub name: String,
    /// Clock period in `ns`.
    pub period_ns: f64,
    /// Rising and falling edge times `[rise_ns, fall_ns]` within the first period.
    pub waveform_ns: [f64; 2],
    /// Source port or pin [`NodeId`]s; empty for a virtual clock.
    pub sources: Vec<NodeId>,
    /// True when created with `-add`.
    pub add: bool,
    /// Optional `-comment` string.
    pub comment: Option<String>,
    /// Network latency `network_latency_ns[an][tr]` in `ns` (`[Early, Late]` x `[Rise, Fall]`).
    pub network_latency_ns: [[f64; 2]; 2],
    /// Source latency `source_latency_ns[early_late][min_max][tr]` in `ns`
    /// (`[Early, Late]` x `[Min, Max]` x `[Rise, Fall]`).
    pub source_latency_ns: [[[f64; 2]; 2]; 2],
    /// Ideal clock transition `transition_ns[an][tr]` in `ns` (`[Early, Late]` x `[Rise, Fall]`).
    pub transition_ns: [[f64; 2]; 2],
    /// Simple per-clock setup uncertainty in `ns` (per edge `[Rise, Fall]`).
    pub uncertainty_setup_ns: [f64; 2],
    /// Simple per-clock hold uncertainty in `ns` (per edge `[Rise, Fall]`).
    pub uncertainty_hold_ns: [f64; 2],
    /// Source location of the `create_clock` command.
    pub location: Option<Location>,
}

impl CompiledClock {
    /// True if this clock has no source ports or pins (virtual clock).
    #[must_use]
    pub fn is_virtual(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Compiled `set_clock_groups` relationship (`STA-REQ-0266`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledClockGroups {
    /// Group relationship kind (`Asynchronous`, `LogicallyExclusive`, `PhysicallyExclusive`).
    pub kind: ClockGroupsKind,
    /// Optional group set name (`-name`).
    pub name: Option<String>,
    /// `-allow_paths` flag (when `true`, emits `D0114` note and separates no clocks in STA).
    pub allow_paths: bool,
    /// Optional `-comment` string.
    pub comment: Option<String>,
    /// Resolved clock groups (`-group`), each containing zero or more [`ClockId`]s.
    pub groups: Vec<Vec<ClockId>>,
    /// Source location of the command.
    pub location: Option<Location>,
}

/// Inter-clock uncertainty (`set_clock_uncertainty -from ... -to ...`; `SDC-REQ-0021`).
#[derive(Debug, Clone, PartialEq)]
pub struct InterClockUncertainty {
    pub from_clock: ClockId,
    pub from_edge: EdgeScope,
    pub to_clock: ClockId,
    pub to_edge: EdgeScope,
    pub setup_hold: SetupHoldScope,
    pub uncertainty_ns: f64,
    pub location: Option<Location>,
}

/// Per-pin or per-port clock uncertainty (`set_clock_uncertainty <val> [get_ports/get_pins ...]`).
#[derive(Debug, Clone, PartialEq)]
pub struct PinClockUncertainty {
    pub node: NodeId,
    pub setup_ns: Option<f64>,
    pub hold_ns: Option<f64>,
}

/// Target of a `set_clock_latency` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClockLatencyTarget {
    Clock(ClockId),
    Node(NodeId),
}

/// Recorded `set_clock_latency` specification (`SDC-REQ-0022`, `STA-REQ-0217`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClockLatencySpec {
    pub target: ClockLatencyTarget,
    pub delay_ns: f64,
    pub source: bool,
    pub edge: EdgeScope,
    pub min_max: MinMaxScope,
    pub early_late: EarlyLateScope,
    pub clock: Option<ClockId>,
    pub location: Option<Location>,
}

/// Compiled `set_input_delay` or `set_output_delay` entry on a port or pin (`STA-REQ-0218`,
/// `STA-REQ-0219`).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledIoDelay {
    /// Target port or pin [`NodeId`].
    pub node: NodeId,
    /// Reference clock (`-clock`), if specified.
    pub clock: Option<ClockId>,
    /// Active edge of the reference clock (`Tr::Fall` when `-clock_fall`, else `Tr::Rise`).
    pub clock_edge: Tr,
    /// `-level_sensitive` flag.
    pub level_sensitive: bool,
    /// `-network_latency_included` flag.
    pub network_latency_included: bool,
    /// `-source_latency_included` flag.
    pub source_latency_included: bool,
    /// Delay values `delay_ns[an][tr]` (`[Early/Min, Late/Max]` x `[Rise, Fall]`) in `ns`.
    /// An unset side remains `None` (`STA-REQ-0219`).
    pub delay_ns: [[Option<f64>; 2]; 2],
    /// Source location of the most recent command updating this entry.
    pub location: Option<Location>,
}

impl CompiledIoDelay {
    /// Returns the delay for analysis bound `an` and transition `tr` in `ns`, if specified.
    #[must_use]
    pub fn get(&self, an: An, tr: Tr) -> Option<f64> {
        self.delay_ns[an as usize][tr as usize]
    }

    /// Returns a `[min_ns, max_ns]` pair in `ns` (falling back across edges/bounds).
    #[must_use]
    pub fn min_max_pair(&self) -> [f64; 2] {
        let early = self.delay_ns[An::Early as usize][Tr::Rise as usize]
            .or(self.delay_ns[An::Early as usize][Tr::Fall as usize]);
        let late = self.delay_ns[An::Late as usize][Tr::Rise as usize]
            .or(self.delay_ns[An::Late as usize][Tr::Fall as usize]);
        let e = early.or(late).unwrap_or(0.0);
        let l = late.or(early).unwrap_or(0.0);
        [e, l]
    }
}

/// Compiled `set_input_transition` on an input port (`SDC-REQ-0025`).
#[derive(Debug, Clone, PartialEq)]
pub struct InputTransitionSpec {
    /// Target input port [`NodeId`].
    pub port: NodeId,
    /// Transition times `slew_ns[an][tr]` (`[Early/Min, Late/Max]` x `[Rise, Fall]`) in `ns`.
    pub slew_ns: [[Option<f64>; 2]; 2],
    /// Optional `-clock` qualifier.
    pub clock: Option<ClockId>,
    /// `-clock_fall` flag.
    pub clock_fall: bool,
    /// Source location.
    pub location: Option<Location>,
}

impl InputTransitionSpec {
    /// Returns `[min, max]` transition values in `ns`, falling back across transitions/bounds.
    #[must_use]
    pub fn min_max_pair(&self) -> [f64; 2] {
        let early = self.slew_ns[An::Early as usize][Tr::Rise as usize]
            .or(self.slew_ns[An::Early as usize][Tr::Fall as usize]);
        let late = self.slew_ns[An::Late as usize][Tr::Rise as usize]
            .or(self.slew_ns[An::Late as usize][Tr::Fall as usize]);
        let e = early.or(late).unwrap_or(0.0);
        let l = late.or(early).unwrap_or(0.0);
        [e, l]
    }
}

/// Compiled `set_driving_cell` on an input port (`SDC-REQ-0026`, `STA-REQ-0218`).
#[derive(Debug, Clone, PartialEq)]
pub struct DrivingCellSpec {
    pub port: NodeId,
    pub lib_cell: String,
    pub library: Option<String>,
    pub pin: String,
    pub from_pin: Option<String>,
    pub edge: EdgeScope,
    pub min_max: MinMaxScope,
    pub input_transition_rise_ns: Option<f64>,
    pub input_transition_fall_ns: Option<f64>,
    pub multiply_by: Option<f64>,
    pub dont_scale: bool,
    pub no_design_rule: bool,
    pub clock: Option<ClockId>,
    pub clock_fall: bool,
    pub location: Option<Location>,
}

/// Compiled `set_load` on a top-level port (`SDC-REQ-0027`, `STA-REQ-0110`).
#[derive(Debug, Clone, PartialEq)]
pub struct PortLoadSpec {
    pub port: NodeId,
    /// `[early/min, late/max]` load capacitance in `pF`.
    pub load_pf: [f64; 2],
    pub pin_load: bool,
    pub wire_load: bool,
    pub subtract_pin_load: bool,
    pub location: Option<Location>,
}

/// Compiled `set_load` on an internal or port net (`SDC-REQ-0027`, `STA-REQ-0124`).
#[derive(Debug, Clone, PartialEq)]
pub struct NetLoadSpec {
    pub net: NetRow,
    /// `[early/min, late/max]` load capacitance in `pF`.
    pub load_pf: [f64; 2],
    pub pin_load: bool,
    pub wire_load: bool,
    pub subtract_pin_load: bool,
    pub location: Option<Location>,
}

/// Disabled timing arc or object from `set_disable_timing` (`STA-REQ-0157`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DisabledTiming {
    /// All arcs of a cell instance (`set_disable_timing [get_cells u]`).
    Cell { inst: u32 },
    /// Specific `from -> to` pin arc of a cell instance (`set_disable_timing -from A -to Y [get_cells u]`).
    CellArc {
        inst: u32,
        from_pin: String,
        to_pin: String,
    },
    /// All arcs touching a specific instance pin (`set_disable_timing [get_pins u/Y]`).
    Pin { pin: PinRef },
    /// Top-level port (`set_disable_timing [get_ports p]`).
    Port { port: NodeId },
    /// All instances of a library cell (`set_disable_timing [get_lib_cells lib/CELL]`).
    LibCell {
        lib_cell: LibCellRef,
        from_pin: Option<String>,
        to_pin: Option<String>,
    },
    /// Specific library pin across all instances of its cell (`set_disable_timing [get_lib_pins lib/CELL/A]`).
    LibPin { lib_pin: LibPinRef },
}

/// Target of a `set_max_time_borrow` constraint (`STA-REQ-0221`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaxTimeBorrowTarget {
    Clock(ClockId),
    Cell(u32),
    Pin(NodeId),
}

/// Compiled `set_max_time_borrow` entry (`SDC-REQ-0033`, `STA-REQ-0221`).
#[derive(Debug, Clone, PartialEq)]
pub struct MaxTimeBorrowSpec {
    pub target: MaxTimeBorrowTarget,
    pub max_borrow_ns: f64,
    pub location: Option<Location>,
}

/// Resolved `-from` or `-to` endpoint set of a timing exception or `group_path`
/// (`STA-REQ-0251`, `STA-REQ-0252`, `STA-REQ-0253`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExceptionEndpoints {
    /// Transition qualifier on the endpoint specifier (`-from` vs `-rise_from` vs `-fall_from`,
    /// or `-to` vs `-rise_to` vs `-fall_to`).
    pub edge: EdgeScope,
    /// Matched launch or capture clocks (`ClockId`).
    pub clocks: Vec<ClockId>,
    /// Matched startpoint or endpoint port/pin [`NodeId`]s (including pins expanded from cells),
    /// sorted and without duplicates.
    pub nodes: Vec<NodeId>,
    /// Cell instances (`FrozenNetlist::instances` indices) that contributed to `nodes`.
    pub cells: Vec<u32>,
}

impl ExceptionEndpoints {
    /// True when `node` is one of the matched ports or pins (`nodes` is sorted and without
    /// duplicates, so this is a binary search).
    #[must_use]
    pub fn has_node(&self, node: NodeId) -> bool {
        self.nodes.binary_search(&node).is_ok()
    }

    /// True when neither clocks nor port/pin nodes were matched.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clocks.is_empty() && self.nodes.is_empty()
    }
}

/// Kind and payload of a compiled timing exception (`STA-REQ-0250` to `STA-REQ-0269`).
#[derive(Debug, Clone, PartialEq)]
pub enum ExceptionKind {
    /// `set_false_path`.
    FalsePath { setup_hold: SetupHoldScope },
    /// `set_multicycle_path`.
    MulticyclePath {
        multiplier: u32,
        /// Effective setup multiplier (`Some(multiplier)` when `setup_hold` is `Setup` or `Both`).
        setup_multiplier: Option<u32>,
        /// Effective hold multiplier (`Some(multiplier)` when `setup_hold` is `Hold`, else `None`
        /// meaning default `0`; `STA-REQ-0250`).
        hold_multiplier: Option<u32>,
        setup_hold: SetupHoldScope,
        start_end: StartEndScope,
    },
    /// `set_max_delay` (delay in `ns`).
    MaxDelay {
        delay_ns: f64,
        ignore_clock_latency: bool,
    },
    /// `set_min_delay` (delay in `ns`).
    MinDelay {
        delay_ns: f64,
        ignore_clock_latency: bool,
    },
}

/// One compiled timing exception in command evaluation order (`STA-REQ-0250` to `STA-REQ-0270`).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledException {
    /// Monotonic command order index for tie-breaking (`STA-REQ-0258`).
    pub order: usize,
    /// Exception type and parameters.
    pub kind: ExceptionKind,
    /// End-transition filter (`-rise`, `-fall`, or `Both`).
    pub edge: EdgeScope,
    /// Resolved `-from` / `-rise_from` / `-fall_from` startpoints, if specified.
    pub from: Option<ExceptionEndpoints>,
    /// Resolved `-to` / `-rise_to` / `-fall_to` endpoints, if specified.
    pub to: Option<ExceptionEndpoints>,
    /// Optional `-comment` string.
    pub comment: Option<String>,
    /// Source location of the exception command.
    pub location: Option<Location>,
}

/// Compiled `group_path` specification (`SDC-REQ-0037`).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledPathGroup {
    pub name: Option<String>,
    pub default: bool,
    pub weight: Option<f64>,
    pub critical_range_ns: Option<f64>,
    pub from: Option<ExceptionEndpoints>,
    pub to: Option<ExceptionEndpoints>,
    pub comment: Option<String>,
    pub location: Option<Location>,
}

/// Compiled `set_operating_conditions` specification (`SDC-REQ-0035`, `STA-REQ-0011`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatingConditionsSpec {
    pub condition: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub library: Option<String>,
    pub min_library: Option<String>,
    pub max_library: Option<String>,
    pub analysis_type: Option<AnalysisType>,
    pub objects: Vec<ResolvedObj>,
    pub location: Option<Location>,
}

/// Compiled `set_wire_load_model` constraint (`SDC-REQ-0036`, `STA-REQ-0122`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireLoadModelSpec {
    pub name: String,
    pub library: Option<String>,
    pub min: bool,
    pub max: bool,
    pub objects: Vec<ResolvedObj>,
    pub location: Option<Location>,
}

/// Compiled `set_wire_load_selection_group` constraint (`SDC-REQ-0036`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireLoadSelectionSpec {
    pub group_name: String,
    pub library: Option<String>,
    pub min: bool,
    pub max: bool,
    pub objects: Vec<ResolvedObj>,
    pub location: Option<Location>,
}

/// Aggregated wire-load configuration (`set_wire_load_model`, `set_wire_load_mode`,
/// `set_wire_load_selection_group`; `STA-REQ-0122`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WireLoadConfig {
    /// Active wire-load mode (`top`, `enclosed`, `segmented`).
    pub mode: Option<WireLoadMode>,
    /// Top-level wire-load model name for early/min analysis.
    pub model_min: Option<String>,
    /// Top-level wire-load model name for late/max analysis.
    pub model_max: Option<String>,
    /// Top-level wire-load selection group for early/min analysis.
    pub selection_min: Option<String>,
    /// Top-level wire-load selection group for late/max analysis.
    pub selection_max: Option<String>,
    /// All `set_wire_load_model` commands in order.
    pub models: Vec<WireLoadModelSpec>,
    /// All `set_wire_load_selection_group` commands in order.
    pub selection_groups: Vec<WireLoadSelectionSpec>,
}

/// Kind of a recorded design-rule constraint (`SDC-REQ-0038`, `D0021`).
#[derive(Debug, Clone, PartialEq)]
pub enum DrcConstraintKind {
    MaxTransition {
        value_ns: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
    },
    MaxCapacitance {
        value_pf: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
    },
    MinCapacitance {
        value_pf: f64,
        clock_path: bool,
        data_path: bool,
        edge: EdgeScope,
    },
    MaxFanout {
        value: f64,
    },
    FanoutLoad {
        value: f64,
    },
    PortFanoutNumber {
        value: u32,
    },
}

/// Recorded design-rule check (DRC) constraint with resolved target objects (`SDC-REQ-0038`).
#[derive(Debug, Clone, PartialEq)]
pub struct DrcConstraint {
    pub kind: DrcConstraintKind,
    pub objects: Vec<ResolvedObj>,
    pub location: Option<Location>,
}

/// Recorded synthesis / optimization / power constraint with resolved target objects
/// (`SDC-REQ-0039`, `D0020`).
#[derive(Debug, Clone, PartialEq)]
pub enum SynthConstraint {
    DontUse {
        lib_cells: Vec<LibCellRef>,
        location: Option<Location>,
    },
    DontTouch {
        objects: Vec<ResolvedObj>,
        location: Option<Location>,
    },
    DontTouchNetwork {
        no_propagate: bool,
        objects: Vec<ResolvedObj>,
        location: Option<Location>,
    },
    SizeOnly {
        instances: Vec<u32>,
        no_instances: bool,
        location: Option<Location>,
    },
    MaxArea {
        value: f64,
        ignore_tns: bool,
        location: Option<Location>,
    },
    MaxDynamicPower {
        value: f64,
        unit: Option<String>,
        location: Option<Location>,
    },
    MaxLeakagePower {
        value: f64,
        unit: Option<String>,
        location: Option<Location>,
    },
}

/// Complete compiled constraint tables resolved against a [`FrozenNetlist`](rusteda_core::netlist::FrozenNetlist)
/// and corner libraries (`STA-REQ-0020`, `STA-REQ-0021`, `STA-REQ-0156`, `STA-REQ-0157`,
/// `STA-REQ-0217` to `STA-REQ-0221`, `STA-REQ-0250` to `STA-REQ-0270`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompiledConstraints {
    /// Raw `Sdc::units` header from the input constraint table.
    pub sdc_units: SdcUnits,
    /// Explicit unit strings from `set_units` commands.
    pub explicit_units: ExplicitUnits,
    /// Per-corner resolved unit conversion scales.
    pub corner_units: Vec<CornerUnits>,
    /// Validated `current_design` name, if specified.
    pub current_design: Option<String>,
    /// Source file provenance table from `Sdc::sources`.
    pub sources: Vec<SourceRecord>,
    /// Operating condition specifications (`set_operating_conditions`).
    pub operating_conditions: Vec<OperatingConditionsSpec>,
    /// Wire-load model, mode, and selection group configuration.
    pub wire_load: WireLoadConfig,
    /// Defined clocks in [`ClockId`] order.
    pub clocks: Vec<CompiledClock>,
    /// Clock group relationships (`set_clock_groups`).
    pub clock_groups: Vec<CompiledClockGroups>,
    /// Inter-clock uncertainties (`set_clock_uncertainty -from ... -to ...`).
    pub inter_clock_uncertainties: Vec<InterClockUncertainty>,
    /// Per-pin/port clock uncertainties (`set_clock_uncertainty <val> <ports/pins>`).
    pub pin_clock_uncertainties: Vec<PinClockUncertainty>,
    /// Recorded `set_clock_latency` commands on clocks, ports, or pins.
    pub clock_latencies: Vec<ClockLatencySpec>,
    /// Input delays (`set_input_delay`).
    pub input_delays: Vec<CompiledIoDelay>,
    /// Output delays (`set_output_delay`).
    pub output_delays: Vec<CompiledIoDelay>,
    /// Input port transitions (`set_input_transition`), keyed by port [`NodeId`].
    pub input_transitions: BTreeMap<NodeId, InputTransitionSpec>,
    /// Driving cell specifications (`set_driving_cell`), keyed by port [`NodeId`].
    pub driving_cells: BTreeMap<NodeId, Vec<DrivingCellSpec>>,
    /// Port capacitance loads (`set_load`), keyed by port [`NodeId`].
    pub port_loads: BTreeMap<NodeId, PortLoadSpec>,
    /// Net capacitance loads (`set_load`), keyed by [`NetRow`].
    pub net_loads: BTreeMap<NetRow, NetLoadSpec>,
    /// Case-analysis constants and transitions (`set_case_analysis`, `set_logic_zero`,
    /// `set_logic_one`), keyed by port or pin [`NodeId`].
    pub case_analysis: BTreeMap<NodeId, CaseValue>,
    /// Ports explicitly constrained via `set_logic_zero`.
    pub logic_zero_ports: Vec<NodeId>,
    /// Ports explicitly constrained via `set_logic_one`.
    pub logic_one_ports: Vec<NodeId>,
    /// Disabled timing arcs, pins, ports, cells, and library arcs (`set_disable_timing`).
    pub disabled_timing: Vec<DisabledTiming>,
    /// Maximum latch time-borrow limits (`set_max_time_borrow`).
    pub max_time_borrow: Vec<MaxTimeBorrowSpec>,
    /// Latch max time-borrow lookup by cell instance index (`FrozenNetlist::instances`).
    pub latch_max_borrow_ns: BTreeMap<u32, f64>,
    /// Compiled timing exceptions (`set_false_path`, `set_multicycle_path`, `set_max_delay`,
    /// `set_min_delay`) in command evaluation order.
    pub exceptions: Vec<CompiledException>,
    /// Compiled path groups (`group_path`) in command evaluation order.
    pub path_groups: Vec<CompiledPathGroup>,
    /// Recorded DRC constraints (`set_max_transition`, `set_max_capacitance`,
    /// `set_min_capacitance`, `set_max_fanout`, `set_fanout_load`, `set_port_fanout_number`).
    pub drc_constraints: Vec<DrcConstraint>,
    /// Recorded synthesis / optimization / power constraints (`set_dont_use`, `set_dont_touch`,
    /// `set_dont_touch_network`, `set_size_only`, `set_max_area`, `set_max_dynamic_power`,
    /// `set_max_leakage_power`).
    pub synth_constraints: Vec<SynthConstraint>,
    /// Replayed import diagnostics plus constraint-compilation diagnostics (`D0101`..`D0114`).
    pub diags: Vec<Diag>,
}

impl CompiledConstraints {
    /// Looks up a compiled clock by name.
    #[must_use]
    pub fn clock_by_name(&self, name: &str) -> Option<&CompiledClock> {
        self.clocks.iter().find(|c| c.name == name)
    }

    /// Returns true if any diagnostic in `self.diags` has code `code`.
    #[must_use]
    pub fn has_diag(&self, code: &str) -> bool {
        self.diags.iter().any(|d| d.code == code)
    }
}

/// Error returned when constraint compilation produces one or more [`Severity::Error`] diagnostics.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// All diagnostics emitted or replayed during compilation (errors, warnings, and notes).
    pub diags: Vec<Diag>,
    /// Partially compiled constraints up to the point of failure.
    pub partial: Box<CompiledConstraints>,
}

impl CompileError {
    /// Returns true if any diagnostic in `self.diags` has the given diagnostic `code`.
    #[must_use]
    pub fn has_code(&self, code: &str) -> bool {
        self.diags.iter().any(|d| d.code == code)
    }

    /// Returns all error diagnostics (`Severity::Error`).
    pub fn errors(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.severity == Severity::Error)
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for d in self.errors() {
            if !first {
                writeln!(f)?;
            }
            first = false;
            if let Some(loc) = &d.location {
                write!(
                    f,
                    "{}:{}:{}: error[{}]: {}",
                    loc.file, loc.line, loc.column, d.code, d.message
                )?;
            } else {
                write!(f, "error[{}]: {}", d.code, d.message)?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for CompileError {}
