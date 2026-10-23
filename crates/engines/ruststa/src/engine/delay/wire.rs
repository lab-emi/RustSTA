// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The wire model seam (the RC hook): ideal wires and lumped wire-load estimation (owner: E4).
//!
//! Engine section B (`STA-REQ-0122` to `STA-REQ-0125`). A [`WireModel`] gives the load that each
//! net presents to its driver and the delay and slew from the driver to each load pin. Phase 2a
//! has [`Ideal`] wires and the lumped estimation from the library's wire-load models
//! ([`WireloadLumped`], work item E4): wire capacitance from the wire-load table by fanout (the
//! number of connected instance load pins), with piecewise-linear interpolation and slope
//! extrapolation (`STA-REQ-0123`), no table wire load on nets that touch a top-level port
//! (`STA-REQ-0124`), mode `top` (`enclosed` and `segmented` resolve to the top module on a flat
//! netlist), and the selection order SDC (`set_wire_load_model`, `set_wire_load_selection_group`),
//! then the library's `default_wire_load_selection`, then its `default_wire_load`
//! (`STA-REQ-0125`). Phase 2b adds the tree estimation ([`wire_tree`](super::wire_tree)) and the
//! parasitics of SPEF ([`spef`](super::spef)), which give pi loads.

use rustc_hash::FxHashMap;
use rusteda_core::diag::Diag;
use rusteda_core::liberty::{Library, WireLoad, WireLoadMode, WireLoadSelection};
use rusteda_core::netlist::{CONST0, CONST1, FrozenNetlist, PinDir};

use crate::engine::types::{An, DelaySlew, NetRow, NodeId, Tr};

/// Diagnostic code emitted when a requested `wire_load` model, `wire_load_selection`, or
/// `wire_load_mode` is missing or unsupported (`STA-REQ-0125`).
pub const DIAG_WIRELOAD_FALLBACK: &str = "T0010";

/// The load that a net presents to its driver (`STA-REQ-0122`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriverLoad {
    /// A lumped capacitance `c` (pF).
    Lumped { c: f64 },
    /// A pi model: the capacitance `c_near` (pF) at the driver, then the resistance `r` (kOhm)
    /// and the capacitance `c_far` (pF) behind it.
    Pi { c_near: f64, r: f64, c_far: f64 },
}

impl DriverLoad {
    /// The total capacitance of the load (pF).
    #[must_use]
    pub fn total(self) -> f64 {
        match self {
            DriverLoad::Lumped { c } => c,
            DriverLoad::Pi { c_near, c_far, .. } => c_near + c_far,
        }
    }
}

/// A wire model: the load of each net at its driver, and the delay and slew from the driver to
/// each load pin (`STA-REQ-0122`).
pub trait WireModel {
    /// The name of the model in the analysis scope (`wire_model`).
    fn scope_name(&self) -> &'static str;

    /// The load that `net` presents to its driver for the bound `an`, when the capacitances of
    /// the pins on the net sum to `pin_load` (pF).
    fn driver_load(&self, net: NetRow, an: An, pin_load: f64) -> DriverLoad;

    /// The delay (ns) from the driver of `net` to its load pin `load`, and the slew (ns) at that
    /// pin, for the bound `an` and a transition `tr` that leaves the driver with slew `slew` (ns).
    fn load_delay(&self, net: NetRow, load: NodeId, tr: Tr, an: An, slew: f64) -> DelaySlew;
}

/// Ideal wires: no wire capacitance, no wire delay, and the driver's slew at every load
/// (`STA-REQ-0111`, `STA-REQ-0122`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ideal;

impl WireModel for Ideal {
    fn scope_name(&self) -> &'static str {
        "ideal"
    }

    fn driver_load(&self, _net: NetRow, _an: An, pin_load: f64) -> DriverLoad {
        DriverLoad::Lumped { c: pin_load }
    }

    fn load_delay(&self, _net: NetRow, _load: NodeId, _tr: Tr, _an: An, slew: f64) -> DelaySlew {
        DelaySlew { delay: 0.0, slew }
    }
}

/// Scope and status fields describing the active wire model of a corner (`STA-REQ-0125`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireScope {
    /// `"ideal"` or `"wireload_lumped"`.
    pub wire_model: &'static str,
    /// Selected `wire_load` group name when `wire_model == "wireload_lumped"`.
    pub wire_load_model: Option<String>,
    /// Active wire-load mode (`"top"`, `"enclosed"`, `"segmented"`) when
    /// `wire_model == "wireload_lumped"`.
    pub wire_load_mode: Option<String>,
}

impl WireScope {
    /// Scope metadata for [`Ideal`] wires.
    #[must_use]
    pub const fn ideal() -> Self {
        Self {
            wire_model: "ideal",
            wire_load_model: None,
            wire_load_mode: None,
        }
    }

    /// Scope metadata for [`WireloadLumped`] with the given model and mode names.
    #[must_use]
    pub fn lumped(model: impl Into<String>, mode: impl Into<String>) -> Self {
        Self {
            wire_model: "wireload_lumped",
            wire_load_model: Some(model.into()),
            wire_load_mode: Some(mode.into()),
        }
    }
}

/// Configuration and SDC overrides for wire-load model selection (`STA-REQ-0123`, `STA-REQ-0125`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireloadConfig {
    /// Explicit SDC `set_wire_load_model -name <name>`.
    pub wire_load_model: Option<String>,
    /// Optional `-library <lib>` qualifier on `set_wire_load_model` or
    /// `set_wire_load_selection_group`.
    pub wire_load_library: Option<String>,
    /// Explicit SDC `set_wire_load_selection_group <group>`.
    pub wire_load_selection_group: Option<String>,
    /// Explicit SDC `set_wire_load_mode <mode>` (`top`, `enclosed`, `segmented`).
    pub wire_load_mode: Option<String>,
    /// Optional override for total instantiated cell area (when `None`, computed by summing the
    /// area of all instances in `FrozenNetlist` across the corner libraries).
    pub total_cell_area_override: Option<f64>,
    /// Explicit per-net wire-load capacitance overrides (`set_load -wire_load`), in pF, indexed
    /// `[early, late]` by [`An::index`]. When present on a net, replaces the `wire_load` table
    /// capacitance for that net (`STA-REQ-0123`, `STA-REQ-0124`).
    pub net_wire_load_overrides: FxHashMap<NetRow, [f64; 2]>,
    /// When `true`, emit a `T0010` warning if neither SDC nor the corner library selects a
    /// `wire_load` model (falling back to [`Ideal`]).
    pub require_wireload: bool,
}

impl WireloadConfig {
    /// Creates an empty [`WireloadConfig`] with no SDC overrides.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the explicit SDC `set_wire_load_model -name <name>`.
    #[must_use]
    pub fn with_model(mut self, name: impl Into<String>) -> Self {
        self.wire_load_model = Some(name.into());
        self
    }

    /// Sets the optional `-library <lib>` qualifier for SDC wire-load lookup.
    #[must_use]
    pub fn with_library(mut self, library: impl Into<String>) -> Self {
        self.wire_load_library = Some(library.into());
        self
    }

    /// Sets the explicit SDC `set_wire_load_selection_group <group>`.
    #[must_use]
    pub fn with_selection_group(mut self, group: impl Into<String>) -> Self {
        self.wire_load_selection_group = Some(group.into());
        self
    }

    /// Sets the explicit SDC `set_wire_load_mode <mode>`.
    #[must_use]
    pub fn with_mode(mut self, mode: impl Into<String>) -> Self {
        self.wire_load_mode = Some(mode.into());
        self
    }

    /// Overrides the total instantiated cell area used for `wire_load_selection` lookup.
    #[must_use]
    pub fn with_total_cell_area(mut self, area: f64) -> Self {
        self.total_cell_area_override = Some(area);
        self
    }

    /// Sets a uniform `[early, late]` wire-load capacitance override (`set_load -wire_load`, in
    /// pF) on `net`.
    #[must_use]
    pub fn with_net_wire_load(mut self, net: NetRow, cap_pf: f64) -> Self {
        self.net_wire_load_overrides.insert(net, [cap_pf, cap_pf]);
        self
    }

    /// Sets bound-specific `[early, late]` wire-load capacitance overrides (`set_load -wire_load`,
    /// in pF) on `net`.
    #[must_use]
    pub fn with_net_wire_load_bounds(mut self, net: NetRow, early_pf: f64, late_pf: f64) -> Self {
        self.net_wire_load_overrides
            .insert(net, [early_pf, late_pf]);
        self
    }

    /// Requires a wire-load model to be present, emitting `T0010` if none can be selected.
    #[must_use]
    pub fn with_require_wireload(mut self, require: bool) -> Self {
        self.require_wireload = require;
        self
    }
}

/// Lumped wire-load estimation from the active `wire_load` group of a corner (`STA-REQ-0122` to
/// `STA-REQ-0125`).
#[derive(Debug, Clone, PartialEq)]
pub struct WireloadLumped {
    model: WireLoad,
    mode: WireLoadMode,
    cap_unit_pf: f64,
    total_cell_area: f64,
    net_fanout: Vec<u32>,
    net_is_port: Vec<bool>,
    net_wire_cap: Vec<[f64; 2]>,
}

impl WireloadLumped {
    /// Resolves the active `wire_load` model and mode for a corner from `libs` and `config`, and
    /// precomputes per-net fanouts, port-net exclusions, and lumped wire capacitances
    /// (`STA-REQ-0122` to `STA-REQ-0125`).
    ///
    /// Returns `(Some(model), diags)` when a `wire_load` group is selected, or `(None, diags)`
    /// when no `wire_load` model is active (in which case callers use [`Ideal`]).
    #[must_use]
    pub fn from_corner(
        netlist: &FrozenNetlist<'_>,
        libs: &[Library],
        config: &WireloadConfig,
    ) -> (Option<Self>, Vec<Diag>) {
        let mut diags = Vec::new();
        let total_cell_area = config
            .total_cell_area_override
            .unwrap_or_else(|| compute_total_cell_area(netlist, libs));

        let selected = select_wire_load(libs, config, total_cell_area, &mut diags);
        let Some((lib, wl)) = selected else {
            if config.require_wireload && diags.is_empty() {
                diags.push(Diag::warning(
                    DIAG_WIRELOAD_FALLBACK,
                    "no wire_load model in constraints or corner libraries; using ideal wires",
                ));
            }
            return (None, diags);
        };

        let mode = resolve_wire_load_mode(lib, libs, config, &mut diags);
        let lumped = Self::with_model(
            netlist,
            wl.clone(),
            mode,
            lib.cap_unit_pf,
            total_cell_area,
            &config.net_wire_load_overrides,
        );
        (Some(lumped), diags)
    }

    /// Resolves a [`WireloadLumped`] model from a single [`Library`] and `config`.
    #[must_use]
    pub fn from_library(
        netlist: &FrozenNetlist<'_>,
        lib: &Library,
        config: &WireloadConfig,
    ) -> (Option<Self>, Vec<Diag>) {
        Self::from_corner(netlist, std::slice::from_ref(lib), config)
    }

    /// Builds a [`WireloadLumped`] model directly from a resolved [`WireLoad`] and
    /// [`WireLoadMode`].
    #[must_use]
    pub fn with_model(
        netlist: &FrozenNetlist<'_>,
        model: WireLoad,
        mode: WireLoadMode,
        cap_unit_pf: f64,
        total_cell_area: f64,
        overrides: &FxHashMap<NetRow, [f64; 2]>,
    ) -> Self {
        let (net_fanout, net_is_port, net_wire_cap) =
            compute_net_wire_caps(netlist, &model, cap_unit_pf, overrides);
        Self {
            model,
            mode,
            cap_unit_pf,
            total_cell_area,
            net_fanout,
            net_is_port,
            net_wire_cap,
        }
    }

    /// The selected Liberty [`WireLoad`] group.
    #[must_use]
    pub fn model(&self) -> &WireLoad {
        &self.model
    }

    /// The name of the selected `wire_load` group.
    #[must_use]
    pub fn model_name(&self) -> &str {
        &self.model.name
    }

    /// The active [`WireLoadMode`] (`top`, `enclosed`, `segmented`).
    #[must_use]
    pub fn mode(&self) -> &WireLoadMode {
        &self.mode
    }

    /// The library capacitance unit scale in pF (`1.0` when `capacitive_load_unit (1, pf)`).
    #[must_use]
    pub fn cap_unit_pf(&self) -> f64 {
        self.cap_unit_pf
    }

    /// The total instantiated cell area used during `wire_load_selection` resolution.
    #[must_use]
    pub fn total_cell_area(&self) -> f64 {
        self.total_cell_area
    }

    /// The [`WireScope`] metadata for this model (`STA-REQ-0125`).
    #[must_use]
    pub fn scope(&self) -> WireScope {
        WireScope::lumped(self.model.name.clone(), self.mode.name())
    }

    /// Evaluates the wire length `L(F)` for `fanout` connected instance load pins by
    /// piecewise-linear interpolation and slope extrapolation (`STA-REQ-0123`).
    #[must_use]
    pub fn length_for_fanout(&self, fanout: u32) -> f64 {
        self.model.length_for_fanout(fanout)
    }

    /// Evaluates the lumped wire capacitance `C_wire(F)` (in pF) for an internal net with
    /// `fanout` connected instance load pins (`STA-REQ-0123`).
    #[must_use]
    pub fn capacitance_for_fanout(&self, fanout: u32) -> f64 {
        self.model.capacitance_for_fanout(fanout) * self.cap_unit_pf
    }

    /// Returns the number of connected instance load pins (`Input` or `Inout`) on `net` (including
    /// nets aliased via `ASSIGNS`, `STA-REQ-0123`).
    #[must_use]
    pub fn net_fanout(&self, net: NetRow) -> u32 {
        self.net_fanout.get(net.index()).copied().unwrap_or(0)
    }

    /// Returns `true` when `net` (or a net aliased to `net` via `ASSIGNS`) is connected to a
    /// top-level port (`STA-REQ-0124`).
    #[must_use]
    pub fn is_port_net(&self, net: NetRow) -> bool {
        self.net_is_port.get(net.index()).copied().unwrap_or(false)
    }

    /// Returns the wire capacitance `C_wire` (in pF) added to `net` in analysis bound `an`
    /// (`STA-REQ-0123`, `STA-REQ-0124`).
    #[must_use]
    pub fn wire_capacitance(&self, net: NetRow, an: An) -> f64 {
        self.net_wire_cap
            .get(net.index())
            .map_or(0.0, |pair| pair[an.index()])
    }
}

impl WireModel for WireloadLumped {
    fn scope_name(&self) -> &'static str {
        "wireload_lumped"
    }

    fn driver_load(&self, net: NetRow, an: An, pin_load: f64) -> DriverLoad {
        DriverLoad::Lumped {
            c: pin_load + self.wire_capacitance(net, an),
        }
    }

    fn load_delay(&self, _net: NetRow, _load: NodeId, _tr: Tr, _an: An, slew: f64) -> DelaySlew {
        DelaySlew { delay: 0.0, slew }
    }
}

/// The resolved Phase 2a wire model of a corner (`Ideal` or `WireloadLumped`, `STA-REQ-0122`,
/// `STA-REQ-0125`).
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedWireModel {
    /// Detailed extracted RC with lumped driver load and first-moment delay.
    Extracted(super::spef::Spef),
    /// Ideal wires (`wire_model = "ideal"`).
    Ideal(Ideal),
    /// Lumped wire-load estimation (`wire_model = "wireload_lumped"`).
    Lumped(WireloadLumped),
}

impl ResolvedWireModel {
    /// Resolves the corner wire model from `libs` and `config` (`STA-REQ-0125`), falling back to
    /// [`Ideal`] when no `wire_load` model is active or when a requested model is missing.
    #[must_use]
    pub fn resolve(
        netlist: &FrozenNetlist<'_>,
        libs: &[Library],
        config: &WireloadConfig,
    ) -> (Self, Vec<Diag>) {
        let (lumped, diags) = WireloadLumped::from_corner(netlist, libs, config);
        match lumped {
            Some(w) => (Self::Lumped(w), diags),
            None => (Self::Ideal(Ideal), diags),
        }
    }

    /// Resolves the wire model from a single [`Library`] and `config`.
    #[must_use]
    pub fn resolve_library(
        netlist: &FrozenNetlist<'_>,
        lib: &Library,
        config: &WireloadConfig,
    ) -> (Self, Vec<Diag>) {
        Self::resolve(netlist, std::slice::from_ref(lib), config)
    }

    /// Returns the [`WireScope`] metadata for the resolved wire model (`STA-REQ-0125`).
    #[must_use]
    pub fn scope(&self) -> WireScope {
        match self {
            Self::Ideal(_) => WireScope::ideal(),
            Self::Extracted(_) => WireScope {
                wire_model: "spef_elmore",
                wire_load_model: None,
                wire_load_mode: None,
            },
            Self::Lumped(w) => w.scope(),
        }
    }

    /// Returns `Some(&WireloadLumped)` when lumped wire-load estimation is active.
    #[must_use]
    pub fn as_lumped(&self) -> Option<&WireloadLumped> {
        match self {
            Self::Ideal(_) | Self::Extracted(_) => None,
            Self::Lumped(w) => Some(w),
        }
    }

    /// Returns `true` when the resolved wire model is [`Ideal`].
    #[must_use]
    pub fn is_ideal(&self) -> bool {
        matches!(self, Self::Ideal(_))
    }
}

impl WireModel for ResolvedWireModel {
    fn scope_name(&self) -> &'static str {
        match self {
            Self::Ideal(w) => w.scope_name(),
            Self::Extracted(w) => w.scope_name(),
            Self::Lumped(w) => w.scope_name(),
        }
    }

    fn driver_load(&self, net: NetRow, an: An, pin_load: f64) -> DriverLoad {
        match self {
            Self::Ideal(w) => w.driver_load(net, an, pin_load),
            Self::Extracted(w) => w.driver_load(net, an, pin_load),
            Self::Lumped(w) => w.driver_load(net, an, pin_load),
        }
    }

    fn load_delay(&self, net: NetRow, load: NodeId, tr: Tr, an: An, slew: f64) -> DelaySlew {
        match self {
            Self::Ideal(w) => w.load_delay(net, load, tr, an, slew),
            Self::Extracted(w) => w.load_delay(net, load, tr, an, slew),
            Self::Lumped(w) => w.load_delay(net, load, tr, an, slew),
        }
    }
}

/// Computes the sum of instantiated cell areas across all instances in `netlist` using the cells
/// of `libs` (`STA-REQ-0125`).
#[must_use]
pub fn compute_total_cell_area(netlist: &FrozenNetlist<'_>, libs: &[Library]) -> f64 {
    let mut cell_areas = Vec::with_capacity(netlist.cells.len());
    for &name_id in netlist.cells.iter() {
        let area = netlist
            .string(name_id)
            .ok()
            .and_then(|cell_name| {
                libs.iter()
                    .find_map(|lib| lib.cells.iter().find(|c| c.name == cell_name))
            })
            .map_or(0.0, |c| c.area);
        cell_areas.push(area);
    }

    let mut total = 0.0;
    for inst in netlist.instances.iter() {
        if let Some(&a) = cell_areas.get(inst.cell as usize) {
            total += a;
        }
    }
    total
}

fn find_wire_load_in_libs<'a>(
    libs: &'a [Library],
    name: &str,
    lib_filter: Option<&str>,
) -> Option<(&'a Library, &'a WireLoad)> {
    for lib in libs {
        if lib_filter.is_some_and(|want| lib.name != want) {
            continue;
        }
        if let Some(wl) = lib.wire_load(name) {
            return Some((lib, wl));
        }
    }
    None
}

fn find_selection_in_libs<'a>(
    libs: &'a [Library],
    name: &str,
    lib_filter: Option<&str>,
) -> Option<(&'a Library, &'a WireLoadSelection)> {
    for lib in libs {
        if lib_filter.is_some_and(|want| lib.name != want) {
            continue;
        }
        if let Some(sel) = lib.wire_load_selection(name) {
            return Some((lib, sel));
        }
    }
    None
}

fn resolve_from_selection<'a>(
    owner_lib: &'a Library,
    libs: &'a [Library],
    sel: &'a WireLoadSelection,
    total_cell_area: f64,
    lib_filter: Option<&str>,
    diags: &mut Vec<Diag>,
) -> Option<(&'a Library, &'a WireLoad)> {
    let Some(wl_name) = sel.select(total_cell_area) else {
        diags.push(Diag::warning(
            DIAG_WIRELOAD_FALLBACK,
            format!(
                "wire_load_selection '{}' has no area rule matching total cell area {total_cell_area}",
                sel.name
            ),
        ));
        return None;
    };
    if let Some(wl) = owner_lib.wire_load(wl_name) {
        return Some((owner_lib, wl));
    }
    if let Some(found) = find_wire_load_in_libs(libs, wl_name, lib_filter) {
        return Some(found);
    }
    diags.push(Diag::warning(
        DIAG_WIRELOAD_FALLBACK,
        format!(
            "wire_load model '{wl_name}' selected by '{}' for area {total_cell_area} not found in corner libraries",
            sel.name
        ),
    ));
    None
}

/// Selects the active `(&Library, &WireLoad)` in priority order (`STA-REQ-0125`):
/// 1. Explicit SDC `set_wire_load_model -name <name>` (or `set_wire_load_selection_group <group>`);
/// 2. Corner library's `default_wire_load_selection` matched against `total_cell_area`;
/// 3. Corner library's `default_wire_load`.
fn select_wire_load<'a>(
    libs: &'a [Library],
    config: &WireloadConfig,
    total_cell_area: f64,
    diags: &mut Vec<Diag>,
) -> Option<(&'a Library, &'a WireLoad)> {
    let lib_filter = config.wire_load_library.as_deref();

    // 1a. Explicit SDC `set_wire_load_model -name <name>`.
    if let Some(model_name) = config.wire_load_model.as_deref() {
        if let Some(found) = find_wire_load_in_libs(libs, model_name, lib_filter) {
            return Some(found);
        }
        diags.push(Diag::warning(
            DIAG_WIRELOAD_FALLBACK,
            format!("wire_load model '{model_name}' not found in corner libraries"),
        ));
    }

    // 1b. Explicit SDC `set_wire_load_selection_group <group>`.
    if let Some(group_name) = config.wire_load_selection_group.as_deref() {
        if let Some((owner_lib, sel)) = find_selection_in_libs(libs, group_name, lib_filter) {
            if let Some(found) =
                resolve_from_selection(owner_lib, libs, sel, total_cell_area, lib_filter, diags)
            {
                return Some(found);
            }
        } else {
            diags.push(Diag::warning(
                DIAG_WIRELOAD_FALLBACK,
                format!("wire_load_selection '{group_name}' not found in corner libraries"),
            ));
        }
    }

    // 2. Corner library's `default_wire_load_selection` matched against `total_cell_area`.
    for lib in libs {
        if let Some(sel_name) = lib.default_wire_load_selection.as_deref() {
            if let Some((owner_lib, sel)) = lib
                .wire_load_selection(sel_name)
                .map(|s| (lib, s))
                .or_else(|| find_selection_in_libs(libs, sel_name, None))
            {
                if let Some(found) =
                    resolve_from_selection(owner_lib, libs, sel, total_cell_area, None, diags)
                {
                    return Some(found);
                }
            } else {
                diags.push(Diag::warning(
                    DIAG_WIRELOAD_FALLBACK,
                    format!(
                        "default_wire_load_selection '{sel_name}' in library '{}' not found",
                        lib.name
                    ),
                ));
            }
        }
    }

    // 3. Corner library's `default_wire_load`.
    for lib in libs {
        if let Some(wl_name) = lib.default_wire_load.as_deref() {
            if let Some(wl) = lib.wire_load(wl_name) {
                return Some((lib, wl));
            }
            if let Some(found) = find_wire_load_in_libs(libs, wl_name, None) {
                return Some(found);
            }
            diags.push(Diag::warning(
                DIAG_WIRELOAD_FALLBACK,
                format!(
                    "default_wire_load '{wl_name}' in library '{}' not found",
                    lib.name
                ),
            ));
        }
    }

    None
}

fn resolve_wire_load_mode(
    active_lib: &Library,
    libs: &[Library],
    config: &WireloadConfig,
    diags: &mut Vec<Diag>,
) -> WireLoadMode {
    if let Some(raw_mode) = config.wire_load_mode.as_deref() {
        return validate_mode(WireLoadMode::from_name(raw_mode), diags);
    }
    if let Some(mode) = active_lib.default_wire_load_mode.clone().or_else(|| {
        libs.iter()
            .find_map(|lib| lib.default_wire_load_mode.clone())
    }) {
        return validate_mode(mode, diags);
    }
    WireLoadMode::Top
}

fn validate_mode(mode: WireLoadMode, diags: &mut Vec<Diag>) -> WireLoadMode {
    match mode {
        WireLoadMode::Top | WireLoadMode::Enclosed | WireLoadMode::Segmented => mode,
        WireLoadMode::Other(other) => {
            diags.push(Diag::warning(
                DIAG_WIRELOAD_FALLBACK,
                format!("unsupported wire_load_mode '{other}'; defaulting to 'top'"),
            ));
            WireLoadMode::Top
        }
        _ => WireLoadMode::Top,
    }
}

fn find_root(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = p;
    }
    x
}

fn union_roots(parent: &mut [u32], a: u32, b: u32) {
    let ra = find_root(parent, a);
    let rb = find_root(parent, b);
    if ra != rb {
        if ra < rb {
            parent[rb as usize] = ra;
        } else {
            parent[ra as usize] = rb;
        }
    }
}

fn compute_net_wire_caps(
    netlist: &FrozenNetlist<'_>,
    model: &WireLoad,
    cap_unit_pf: f64,
    overrides: &FxHashMap<NetRow, [f64; 2]>,
) -> (Vec<u32>, Vec<bool>, Vec<[f64; 2]>) {
    let n_nets = netlist.nets.len();
    let mut parent: Vec<u32> = (0..n_nets as u32).collect();
    let mut is_const = vec![false; n_nets];

    for a in netlist.assigns.iter() {
        if (a.lhs as usize) < n_nets {
            if (a.rhs as usize) < n_nets {
                union_roots(&mut parent, a.lhs, a.rhs);
            } else if a.rhs == CONST0 || a.rhs == CONST1 {
                is_const[a.lhs as usize] = true;
            }
        }
    }

    let mut root_of = vec![0usize; n_nets];
    for i in 0..n_nets {
        let r = find_root(&mut parent, i as u32) as usize;
        root_of[i] = r;
        if is_const[i] {
            is_const[r] = true;
        }
    }

    // Mark nets connected to any top-level port bit (STA-REQ-0124).
    let mut root_is_port = vec![false; n_nets];
    for &pb_net in netlist.port_bit_net.iter() {
        if (pb_net as usize) < n_nets {
            root_is_port[root_of[pb_net as usize]] = true;
        }
    }

    // Count connected instance load pins (`Input` or `Inout`) per net root (STA-REQ-0123).
    let mut root_fanout = vec![0u32; n_nets];
    for (net_idx, &r) in root_of.iter().enumerate() {
        for &pin_row in netlist.net_pins_of(net_idx as u32) {
            if matches!(netlist.pin_dir(pin_row), PinDir::Input | PinDir::Inout) {
                root_fanout[r] += 1;
            }
        }
    }

    // Propagate explicit `set_load -wire_load` overrides across aliased nets.
    let mut root_override: FxHashMap<usize, [f64; 2]> = FxHashMap::default();
    for (&net_row, &ov) in overrides {
        if let Some(&r) = root_of.get(net_row.index()) {
            let entry = root_override.entry(r).or_insert([0.0, 0.0]);
            entry[0] += ov[0];
            entry[1] += ov[1];
        }
    }

    let mut net_fanout = vec![0u32; n_nets];
    let mut net_is_port = vec![false; n_nets];
    let mut net_wire_cap = vec![[0.0f64; 2]; n_nets];

    for net_idx in 0..n_nets {
        let r = root_of[net_idx];
        let fo = root_fanout[r];
        let is_port = root_is_port[r];
        net_fanout[net_idx] = fo;
        net_is_port[net_idx] = is_port;

        if let Some(&ov) = root_override.get(&r) {
            net_wire_cap[net_idx] = ov;
        } else if is_port || is_const[r] {
            net_wire_cap[net_idx] = [0.0, 0.0];
        } else {
            let c = model.capacitance_for_fanout(fo) * cap_unit_pf;
            net_wire_cap[net_idx] = [c, c];
        }
    }

    (net_fanout, net_is_port, net_wire_cap)
}

#[cfg(test)]
mod tests {
    use rusteda_core::netlist::{Conn, GateNetlist, PortDir};

    use super::*;

    // STA-REQ-0110, STA-REQ-0111, STA-REQ-0122: with ideal wires the load of a net is the sum of
    // its pin capacitances, and net arcs have zero delay and copy the driver's transition to the
    // loads.
    #[test]
    fn ideal_wires() {
        let w: &dyn WireModel = &Ideal;
        assert_eq!(w.scope_name(), "ideal");
        for an in An::ALL {
            let load = w.driver_load(NetRow(3), an, 0.0125);
            assert_eq!(load, DriverLoad::Lumped { c: 0.0125 });
            assert_eq!(load.total(), 0.0125);
            for tr in Tr::ALL {
                assert_eq!(
                    w.load_delay(NetRow(3), NodeId(9), tr, an, 0.07),
                    DelaySlew {
                        delay: 0.0,
                        slew: 0.07
                    }
                );
            }
        }
    }

    // STA-REQ-0122, STA-REQ-0123, STA-REQ-0124, STA-REQ-0125: pi load total and lumped wire-load
    // estimation on a simple netlist with an internal net and a port net.
    #[test]
    fn pi_load_total_and_lumped_estimation() {
        let pi = DriverLoad::Pi {
            c_near: 0.25,
            r: 0.1,
            c_far: 0.5,
        };
        assert_eq!(pi.total(), 0.75);

        let mut g = GateNetlist::new("top").unwrap();
        let buf = g
            .add_cell_type("BUF", &[("A", PinDir::Input), ("Y", PinDir::Output)])
            .unwrap();
        g.add_port("a", PortDir::Input, None).unwrap();
        g.add_port("y", PortDir::Output, None).unwrap();
        let n_a = g.find_net("a", None).unwrap();
        let n_y = g.find_net("y", None).unwrap();
        let n_int = g.add_net(Some("n_int"), None).unwrap();

        let u1 = g.add_instance("u1", buf).unwrap();
        g.connect(u1, "A", Conn::Net(n_a)).unwrap();
        g.connect(u1, "Y", Conn::Net(n_int)).unwrap();
        let u2 = g.add_instance("u2", buf).unwrap();
        g.connect(u2, "A", Conn::Net(n_int)).unwrap();
        g.connect(u2, "Y", Conn::Net(n_y)).unwrap();
        let frozen = g.freeze();

        let wl = WireLoad {
            name: "wl_test".to_string(),
            resistance: 0.005,
            capacitance: 0.002,
            area: 0.1,
            slope: 2.0,
            fanout_length: vec![(1, 10.0), (3, 20.0)],
        };
        let model = WireloadLumped::with_model(
            &frozen,
            wl,
            WireLoadMode::Top,
            1.0,
            3.0,
            &FxHashMap::default(),
        );
        assert_eq!(model.scope_name(), "wireload_lumped");
        assert_eq!(
            model.scope(),
            WireScope::lumped("wl_test".to_string(), "top".to_string())
        );
        // Port nets (0 and 1) get 0 wire load; internal net (2) has fanout 1 -> 0.002 * 10 = 0.02 pF.
        assert!(model.is_port_net(NetRow(0)));
        assert!(model.is_port_net(NetRow(1)));
        assert!(!model.is_port_net(NetRow(2)));
        assert_eq!(model.net_fanout(NetRow(2)), 1);
        assert_eq!(
            model.driver_load(NetRow(0), An::Late, 0.01),
            DriverLoad::Lumped { c: 0.01 }
        );
        assert_eq!(
            model.driver_load(NetRow(2), An::Late, 0.01),
            DriverLoad::Lumped { c: 0.03 }
        );
        assert_eq!(
            model.load_delay(NetRow(2), NodeId(0), Tr::Rise, An::Late, 0.08),
            DelaySlew {
                delay: 0.0,
                slew: 0.08
            }
        );
    }
}
