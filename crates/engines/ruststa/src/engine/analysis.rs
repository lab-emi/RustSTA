// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The orchestrator: an analysis of every corner of a revision (owner: E5, from E0).
//!
//! An analysis reads the revision's netlist, libraries and constraints through the views of
//! [`input`](super::input), builds the timing graph of [`graph`](super::graph) once, and runs per
//! corner the delay calculation of [`delay`](super::delay), the forward propagation through the
//! compute timing task, the checks and the backward pass of [`prop`](super::prop), and the path
//! search of [`paths`](super::paths). Graph and constraints are shared by the corners; libraries,
//! loads and propagation state belong to one corner. The orchestrator wires every seam: the wire
//! model, the gate delay, derates, the clock model, CPPR and the exception state machine.

use crate::engine::delay::spef::{ParasiticInput, Spef};
use std::sync::Arc;
use std::time::Instant;

use rustc_hash::FxHashMap;
use rusteda_core::diag::Severity;
use rusteda_core::job::{Ctx, Diag, Event};
use rusteda_core::liberty::Library;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::AnalysisType;

use crate::engine::delay::derate::NoDerate;
use crate::engine::delay::load::{NetLoadOverrides, NetLoads};
use crate::engine::delay::lut::RangeTracker;
use crate::engine::delay::wire::{Ideal, ResolvedWireModel, WireloadConfig};
use crate::engine::graph::TimingGraph;
use crate::engine::graph::constants::ConstantPropagation;
use crate::engine::incr::{NodeMapping, PriorAnalysis, into_owned_lowered, propagate_forward_cone};
use crate::engine::input::constr::{CompiledConstraints, CornerLibs, ExceptionKind};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView};
use crate::engine::limits::{
    ClockLimitScope, CornerLimits, DEFAULT_MAX_LIMIT_ROWS, LimitInputs, PulseRules,
    check_corner_limits, static_transitions,
};
use crate::engine::prop::checks::evaluate_all_checks;
use crate::engine::prop::clocks::ClockModel;
use crate::engine::prop::clocks::ideal::IdealClockModel;
use crate::engine::prop::cppr::NoCppr;
use crate::engine::prop::endpoints::{CornerEndpoints, EndpointRecord};
use crate::engine::prop::exceptions::ExceptionContext;
use crate::engine::prop::gating::{ClockGatingSite, discover_gating_sites};
use crate::engine::prop::latch::{build_latch_tag_maps, lower_corner};
use crate::engine::prop::lower::{
    self, ArcTagMaps, BackendKind, LoweredCorner, LoweredResult, Precision, TimingValues,
};
use crate::engine::prop::relation::RelationTable;
use crate::engine::prop::starts::{DriveCellEffect, build_start_rows};
use crate::engine::prop::tags::{DEFAULT_TAG_BUDGET, TagTable};
use crate::engine::scope::{
    AnalysisScope, CornerSummary, ScopeCompleteness, ScopeExceptions, StaStatusValue, StaSummary,
    UnconstrainedCounts,
};
use crate::engine::types::{An, CornerId, NetRow, NodeId, TagId, Tr};

mod electrical;
mod loads;
use loads::{build_net_load_overrides, build_wireload_config};

use electrical::check_max_transitions;

/// Policy for selecting the interconnect wire model in [`AnalysisOptions`] (`STA-REQ-0122`,
/// `STA-REQ-0125`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WireModelOption {
    /// Resolve automatically per corner: use `WireloadLumped` when SDC or the corner library
    /// selects a `wire_load` group, otherwise `Ideal` (`STA-REQ-0125`).
    #[default]
    Auto,
    /// Force `Ideal` zero-capacitance, zero-delay wires regardless of library defaults.
    Ideal,
    /// Require `WireloadLumped`, emitting `T0010` and falling back to `Ideal` if no `wire_load`
    /// group can be resolved.
    WireloadLumped,
}

/// Options controlling [`EngineAnalysis::analyze`] (`STA-REQ-0015`, `STA-REQ-0125`,
/// `STA-REQ-0400`).
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisOptions {
    /// Compute propagation backend (`Cpu`, `Gpu`, `Cuda`).
    pub backend: BackendKind,
    /// Floating-point precision of the propagation kernel (`F64` or `F32`).
    pub precision: Precision,
    /// Wire-model selection policy.
    pub wire_model: WireModelOption,
    /// Immutable extracted parasitics, each bound to one named corner (STA-REQ-0700,
    /// `STA-REQ-0820`). When it is not empty, every analyzed corner must have its own.
    pub parasitics: Vec<Arc<ParasiticInput>>,
    /// Maximum number of propagation tags per corner (`DEFAULT_TAG_BUDGET` by default).
    pub tag_budget: usize,
    /// Maximum estimated bytes of the propagation state of one corner
    /// ([`DEFAULT_MAX_STATE_BYTES`] by default; `STA-REQ-0024`).
    pub max_state_bytes: u64,
    /// Rows kept per corner and kind by the pin limit checks
    /// ([`DEFAULT_MAX_LIMIT_ROWS`] by default; `STA-REQ-0810`).
    pub max_limit_rows: usize,
    /// The rules of the pulse checks (`STA-REQ-0816`, `STA-REQ-0817`).
    pub pulse_rules: PulseRules,
    /// How the design-rule limits of a clock map to pins (`STA-REQ-0808`).
    pub clock_limits: ClockLimitScope,
}

/// Default limit of the propagation state of one corner: 16 GiB (`STA-REQ-0024`).
pub const DEFAULT_MAX_STATE_BYTES: u64 = 16 << 30;

/// Estimated bytes per `(node, tag)` of a corner: arrivals and predecessors in the compute state
/// and in node order, and the required times and slacks of the backward pass (`STA-REQ-0024`).
pub const STATE_BYTES_PER_NODE_TAG: u64 = 160;

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            backend: BackendKind::Cpu,
            precision: Precision::F64,
            wire_model: WireModelOption::Auto,
            parasitics: Vec::new(),
            tag_budget: DEFAULT_TAG_BUDGET,
            max_state_bytes: DEFAULT_MAX_STATE_BYTES,
            max_limit_rows: DEFAULT_MAX_LIMIT_ROWS,
            pulse_rules: PulseRules::default(),
            clock_limits: ClockLimitScope::default(),
        }
    }
}

impl AnalysisOptions {
    /// The extracted parasitics of `corner`: `None` without parasitics, an error `T0070` when the
    /// parasitics are bound to other corners only (`STA-REQ-0820`).
    pub fn parasitics_for(&self, corner: &str) -> Result<Option<&Arc<ParasiticInput>>, Diag> {
        if self.parasitics.is_empty() {
            return Ok(None);
        }
        if let Some(p) = self.parasitics.iter().find(|p| p.corner == corner) {
            return Ok(Some(p));
        }
        let bound: Vec<&str> = self.parasitics.iter().map(|p| p.corner.as_str()).collect();
        Err(Diag::error(
            "T0070",
            format!(
                "SPEF corner {} does not annotate requested corner {corner}: every analyzed corner \
                 needs its own extracted parasitics (import --spef FILE --spef-corner {corner}), \
                 or select the annotated corners with --corner (STA-REQ-0820)",
                bound.join(",")
            ),
        ))
    }

    /// Returns default options with [`WireModelOption::Ideal`].
    #[must_use]
    pub fn ideal() -> Self {
        Self {
            wire_model: WireModelOption::Ideal,
            ..Self::default()
        }
    }
}

/// How the forward propagation of the corners of an analysis runs (`STA-REQ-0601`).
#[derive(Debug, Clone, Copy)]
pub enum Propagation<'p> {
    /// Every node is evaluated (`Scope::All`).
    Full,
    /// The values of an earlier analysis are carried over by `mapping`, and only the cone of the
    /// nodes whose inputs differ from that analysis is evaluated (`Scope::Cone`,
    /// `STA-REQ-0453`).
    Cone {
        /// The earlier analysis.
        prior: &'p PriorAnalysis<'p>,
        /// The nodes of the earlier analysis that correspond to the nodes of this one.
        mapping: &'p NodeMapping,
    },
}

/// What the forward propagation of each corner of an analysis did, in corner order
/// (`STA-REQ-0601`).
#[derive(Debug, Default)]
pub struct PropagationRecord {
    /// The lowered problem of each corner, when the analysis was asked to keep it.
    pub lowered: Vec<LoweredCorner<'static>>,
    /// The level positions whose values a cone propagation changed; `None` after a full one.
    pub changed: Vec<Option<Vec<u32>>>,
    /// The seeds of the propagation: every node for a full one.
    pub seeds: Vec<usize>,
    /// The node evaluations of the propagation.
    pub evaluated: Vec<u64>,
}

/// Compiles the library view of every corner (`STA-REQ-0017`).
pub fn compile_corner_libs(corners: &[CornerLibs<'_>]) -> Result<Vec<TimingLib>, Diag> {
    corners
        .iter()
        .map(|cl| TimingLib::from_corner(cl.name, cl.libs))
        .collect()
}

/// Complete timing analysis state of one operating corner (`STA-REQ-0015`, `STA-REQ-0226`).
#[derive(Debug, Clone)]
pub struct CornerAnalysis {
    /// Dense corner index (`0..corners.len()`).
    pub corner_id: CornerId,
    /// Corner name.
    pub corner_name: String,
    /// Compiled library view of this corner.
    pub lib: TimingLib,
    /// Active wire model resolved for this corner (`Ideal` or `WireloadLumped`).
    pub wire_model: ResolvedWireModel,
    /// Per-net capacitive loads (`pin_loads`, `driver_loads`, `totals`).
    pub net_loads: NetLoads,
    /// Ideal clock network model.
    pub clock_model: IdealClockModel,
    /// Dense propagation tag table.
    pub tags: TagTable,
    /// Timing exceptions of the corner: startpoint tags and check resolution (`STA-REQ-0271`).
    pub exceptions: ExceptionContext,
    /// Cached setup/hold relation table in integer femtoseconds.
    pub relations: RelationTable,
    /// Launch slews at sequential outputs (including latch `G -> Q` slews; `STA-REQ-0222`).
    pub launch_slews: FxHashMap<NodeId, [f64; 4]>,
    /// Per-port driving-cell effects (`STA-REQ-0218`).
    pub driving_cells: FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
    /// Tag-mapping records on latch `D -> Q` arcs.
    pub latch_maps: ArcTagMaps,
    /// Discovered combinational clock-gating check sites (`STA-REQ-0224`).
    pub gating_sites: Vec<ClockGatingSite>,
    /// Forward propagation results (`values` in level order, `stats`, `lower_s`).
    pub propagated: LoweredResult,
    /// Propagated arrivals, slews, and predecessors remapped into [`NodeId`] order
    /// (`0..graph.num_nodes()`).
    pub node_values: TimingValues,
    /// Aggregated endpoint records, path groups, unconstrained counts, and corner summary.
    pub endpoints: CornerEndpoints,
    /// Count of unanalyzed library arcs across instantiated cells in this corner (`STA-REQ-0017`,
    /// `STA-REQ-0015`).
    pub unanalyzed_arcs: usize,
    /// Pulse width, period and electrical design-rule checks of the corner (`STA-REQ-0806`).
    pub limits: CornerLimits,
    /// Non-fatal diagnostics (warnings and notes) emitted while analyzing this corner.
    pub diags: Vec<Diag>,
}

impl CornerAnalysis {
    /// Returns the propagated pin transition time `slew[node][tr][an]` in `ns` (`STA-REQ-0114`).
    #[inline]
    #[must_use]
    pub fn slew(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.node_values
            .transition(node.index(), tr.index(), an.index())
    }

    /// Returns the propagated arrival time `arr[node][tag][tr][an]` in `ns`, or `An::absent(an)`
    /// when `tag` does not reach `node` with transition `tr`.
    #[inline]
    #[must_use]
    pub fn arrival(&self, node: NodeId, tag: TagId, tr: Tr, an: An) -> f64 {
        self.node_values
            .arrival(node.index(), tag.index(), tr.index(), an.index())
    }

    /// Returns the worst present `(tag, arrival_ns)` at `node` for transition `tr` and bound `an`
    /// (`min` for `An::Early`, `max` for `An::Late`), or `None` if no tag reaches `node` with `tr`.
    #[must_use]
    pub fn worst_arrival(&self, node: NodeId, tr: Tr, an: An) -> Option<(TagId, f64)> {
        let mut best: Option<(TagId, f64)> = None;
        for (tag_id, _) in self.tags.iter() {
            let a = self.arrival(node, tag_id, tr, an);
            if !an.is_present(a) {
                continue;
            }
            let replace = match best {
                None => true,
                Some((_, cur)) => an.better(a, cur),
            };
            if replace {
                best = Some((tag_id, a));
            }
        }
        best
    }

    /// Returns the total capacitive load `C_net` (in `pF`) on `net` for `(tr, an)`
    /// (`STA-REQ-0110`, `STA-REQ-0121`).
    #[inline]
    #[must_use]
    pub fn net_cap(&self, net: NetRow, tr: Tr, an: An) -> f64 {
        self.net_loads.total_cap(net, tr, an)
    }

    /// Looks up the [`EndpointRecord`] of `pin` in O(log E).
    #[inline]
    #[must_use]
    pub fn endpoint(&self, pin: NodeId) -> Option<&EndpointRecord> {
        self.endpoints.endpoints.get(&pin)
    }

    /// Returns the worst setup/recovery slack (in `ns`) at endpoint `pin`, if present.
    #[inline]
    #[must_use]
    pub fn setup_slack(&self, pin: NodeId) -> Option<f64> {
        self.endpoints.setup_slack(pin)
    }

    /// Returns the worst hold/removal slack (in `ns`) at endpoint `pin`, if present.
    #[inline]
    #[must_use]
    pub fn hold_slack(&self, pin: NodeId) -> Option<f64> {
        self.endpoints.hold_slack(pin)
    }

    /// Returns the [`CornerSummary`] of this corner.
    #[inline]
    #[must_use]
    pub fn summary(&self) -> &CornerSummary {
        &self.endpoints.summary
    }
}

/// Complete multi-corner static timing analysis of a revision (`STA-REQ-0015`, `STA-REQ-0226`).
#[derive(Debug, Clone)]
pub struct EngineAnalysis<'a> {
    /// Zero-copy netlist view shared by all corners.
    pub netview: NetlistView<'a>,
    /// Levelized timing graph shared by all corners.
    pub graph: TimingGraph,
    /// Compiled SDC constraints shared by all corners.
    pub constraints: CompiledConstraints,
    /// Per-corner timing analysis results in corner order.
    pub corners: Vec<CornerAnalysis>,
    /// Analysis scope metadata (`STA-REQ-0125`, `STA-REQ-0225`, `STA-REQ-0226`).
    pub scope: AnalysisScope,
    /// Scope completeness accounting (`STA-REQ-0225`).
    pub completeness: ScopeCompleteness,
    /// Overall revision timing summary across all corners (`STA-REQ-0226`).
    pub sta_summary: StaSummary,
    /// Revision `status.sta` record.
    pub sta_status: StaStatusValue,
    /// All non-fatal diagnostics collected across graph build, constraint compilation, and corners.
    pub diags: Vec<Diag>,
}

impl<'a> EngineAnalysis<'a> {
    /// Runs a multi-corner analysis from a [`FrozenNetlist`], corner library slices, and already
    /// compiled [`CompiledConstraints`] (`STA-REQ-0015`, `STA-REQ-0226`).
    pub fn analyze(
        netlist: &'a FrozenNetlist<'a>,
        corners: &[CornerLibs<'_>],
        constraints: CompiledConstraints,
        options: &AnalysisOptions,
        ctx: Ctx<'_>,
    ) -> Result<Self, Vec<Diag>> {
        Self::analyze_with(
            netlist,
            corners,
            None,
            constraints,
            options,
            ctx,
            Propagation::Full,
            false,
        )
        .map(|(analysis, _)| analysis)
    }

    /// [`Self::analyze`] with the corners' compiled libraries `timing_libs` (compiled from
    /// `corners` when `None`), the forward propagation `propagation`, and, with `keep_lowered`,
    /// the lowered problem of every corner kept in the returned [`PropagationRecord`]. Every value
    /// of the analysis is independent of `propagation` (`STA-REQ-0601`).
    #[allow(clippy::too_many_arguments)]
    pub fn analyze_with(
        netlist: &'a FrozenNetlist<'a>,
        corners: &[CornerLibs<'_>],
        timing_libs: Option<Vec<TimingLib>>,
        constraints: CompiledConstraints,
        options: &AnalysisOptions,
        ctx: Ctx<'_>,
        propagation: Propagation<'_>,
        keep_lowered: bool,
    ) -> Result<(Self, PropagationRecord), Vec<Diag>> {
        if corners.is_empty() {
            return Err(vec![Diag::error(
                "T0005",
                "at least one corner with a Liberty library is required",
            )]);
        }
        if constraints
            .diags
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            return Err(constraints.diags);
        }

        // Overlap GPU kernel pre-warming with shared netlist/graph/library setup (`STA-REQ-0408`).
        let (setup_res, prewarm_res) =
            lower::with_prewarm(options.backend, options.precision, || {
                build_shared_and_libs(netlist, corners, timing_libs, &constraints, ctx)
            });
        let (netview, graph, timing_libs, constant, mut all_diags) = setup_res?;
        if let Err(err) = prewarm_res {
            return Err(vec![lower::compute_error_diag(&err)]);
        }

        let mut record = PropagationRecord::default();
        let mut corner_results = Vec::with_capacity(corners.len());
        for (idx, (cl, tlib)) in corners.iter().zip(timing_libs).enumerate() {
            let cid = CornerId(idx as u32);
            let ca = analyze_one_corner(
                cid,
                cl.name,
                cl.libs,
                tlib,
                &netview,
                &graph,
                &constant,
                &constraints,
                options,
                ctx,
                propagation,
                keep_lowered,
                &mut record,
            )
            .map_err(|d| vec![d])?;
            all_diags.extend(ca.diags.iter().cloned());
            corner_results.push(ca);
        }

        Ok((
            assemble_analysis(netview, graph, constraints, corner_results, all_diags),
            record,
        ))
    }

    /// Parses and compiles `sdc_text` (named `sdc_name`) against `netlist` and `corners`, then
    /// runs [`Self::analyze`].
    pub fn analyze_sdc(
        netlist: &'a FrozenNetlist<'a>,
        corners: &[CornerLibs<'_>],
        sdc_text: &str,
        sdc_name: &str,
        options: &AnalysisOptions,
        ctx: Ctx<'_>,
    ) -> Result<Self, Vec<Diag>> {
        let compiled = CompiledConstraints::from_sdc_str(sdc_text, sdc_name, netlist, corners)
            .map_err(|e| e.diags)?;
        Self::analyze(netlist, corners, compiled, options, ctx)
    }

    /// Convenience constructor for single-corner (`"default"`) analysis from `libs` and `sdc_text`.
    pub fn analyze_single_corner(
        netlist: &'a FrozenNetlist<'a>,
        libs: &[Library],
        sdc_text: &str,
        options: &AnalysisOptions,
    ) -> Result<Self, Vec<Diag>> {
        let corners = [CornerLibs::default_corner(libs)];
        Self::analyze_sdc(netlist, &corners, sdc_text, "<sdc>", options, Ctx::NONE)
    }

    /// Returns the first (or `"default"`) corner analysis.
    #[inline]
    #[must_use]
    pub fn default_corner(&self) -> &CornerAnalysis {
        &self.corners[0]
    }

    /// Looks up a [`CornerAnalysis`] by corner name.
    #[must_use]
    pub fn corner(&self, name: &str) -> Option<&CornerAnalysis> {
        self.corners.iter().find(|c| c.corner_name == name)
    }

    /// Finds a [`NodeId`] by its canonical port or `<inst>/<pin>` name.
    #[must_use]
    pub fn find_node(&self, name: &str) -> Option<NodeId> {
        for i in 0..self.netview.num_nodes() {
            let nid = NodeId(i as u32);
            if self.netview.node_name_str(nid) == name {
                return Some(nid);
            }
        }
        // Fallback: strip `[0]` on 1-bit ports or match `<port>[0]`.
        if let Some(base) = name.strip_suffix("[0]") {
            for i in 0..self.netview.num_port_bits() {
                let nid = NodeId(i as u32);
                if self.netview.node_name_str(nid) == base {
                    return Some(nid);
                }
            }
        }
        None
    }

    /// Finds a canonical [`NetRow`] by net name (resolving any `ASSIGNS` alias to its canonical
    /// representative).
    #[must_use]
    pub fn find_net(&self, name: &str) -> Option<NetRow> {
        for i in 0..self.netview.num_nets() {
            let raw = NetRow(i as u32);
            if self.netview.net_name(raw).to_string() == name {
                return match self.netview.resolve_net(raw) {
                    NetTarget::Net(canon) => Some(canon),
                    NetTarget::Const(_) => Some(raw),
                };
            }
        }
        None
    }

    /// Returns the worst setup/recovery slack (in `ns`) across all corners for endpoint `name`.
    #[must_use]
    pub fn setup_slack_for(&self, name: &str) -> Option<f64> {
        let node = self.find_node(name)?;
        self.corners
            .iter()
            .filter_map(|c| c.setup_slack(node))
            .reduce(f64::min)
    }

    /// Returns the worst hold/removal slack (in `ns`) across all corners for endpoint `name`.
    #[must_use]
    pub fn hold_slack_for(&self, name: &str) -> Option<f64> {
        let node = self.find_node(name)?;
        self.corners
            .iter()
            .filter_map(|c| c.hold_slack(node))
            .reduce(f64::min)
    }
}

/// Warnings for constraint options that the analysis accepts but does not model, so that none is
/// dropped silently (STA-REQ-0022): `-level_sensitive` input and output delays (`T0041`),
/// `set_load -subtract_pin_load` (`T0042`) and minimum/maximum operating conditions (`T0043`).
fn unmodelled_constraint_diags(
    netview: &NetlistView<'_>,
    constraints: &CompiledConstraints,
) -> Vec<Diag> {
    let mut diags = Vec::new();
    for (cmd, delays) in [
        ("set_input_delay", &constraints.input_delays),
        ("set_output_delay", &constraints.output_delays),
    ] {
        for d in delays.iter().filter(|d| d.level_sensitive) {
            diags.push(Diag::warning(
                "T0041",
                format!(
                    "{cmd} -level_sensitive on '{}' is timed as an edge-triggered delay; \
                     level-sensitive launch and capture are not analysed (STA-REQ-0022)",
                    netview.node_name_str(d.node)
                ),
            ));
        }
    }
    let port_loads = constraints
        .port_loads
        .values()
        .filter(|l| l.subtract_pin_load)
        .map(|l| netview.node_name_str(l.port).into_owned());
    let net_loads = constraints
        .net_loads
        .values()
        .filter(|l| l.subtract_pin_load)
        .map(|l| netview.net_name_str(l.net).into_owned());
    for name in port_loads.chain(net_loads) {
        diags.push(Diag::warning(
            "T0042",
            format!(
                "set_load -subtract_pin_load on '{name}' is applied as a plain load: the pin \
                 loads of the net are not subtracted (STA-REQ-0022)"
            ),
        ));
    }
    for oc in &constraints.operating_conditions {
        let min_max = oc.min.is_some()
            || oc.max.is_some()
            || oc.min_library.is_some()
            || oc.max_library.is_some()
            || matches!(
                oc.analysis_type,
                Some(AnalysisType::BcWc | AnalysisType::OnChipVariation)
            );
        if min_max {
            diags.push(Diag::warning(
                "T0043",
                "set_operating_conditions with minimum and maximum conditions or libraries is \
                 not applied: each corner is analysed with its own libraries (STA-REQ-0022)",
            ));
        }
    }
    diags
}

#[allow(clippy::type_complexity)]
/// The wall time of one analysis phase, reported to the observer of `ctx` as `PhaseStarted` and
/// `PhaseFinished` events (JOB-REQ-0003); without an observer it costs one clock read.
struct PhaseClock<'c> {
    ctx: Ctx<'c>,
    phase: &'static str,
    t0: Instant,
}

impl<'c> PhaseClock<'c> {
    fn start(ctx: Ctx<'c>, phase: &'static str) -> Self {
        ctx.emit(|| Event::PhaseStarted { phase });
        Self {
            ctx,
            phase,
            t0: Instant::now(),
        }
    }

    fn finish(self) {
        let seconds = self.t0.elapsed().as_secs_f64();
        let phase = self.phase;
        self.ctx.emit(|| Event::PhaseFinished { phase, seconds });
    }
}

fn build_shared_and_libs<'a>(
    netlist: &'a FrozenNetlist<'a>,
    corners: &[CornerLibs<'_>],
    timing_libs: Option<Vec<TimingLib>>,
    constraints: &CompiledConstraints,
    ctx: Ctx<'_>,
) -> Result<
    (
        NetlistView<'a>,
        TimingGraph,
        Vec<TimingLib>,
        Vec<bool>,
        Vec<Diag>,
    ),
    Vec<Diag>,
> {
    let clock = PhaseClock::start(ctx, "compile_libraries");
    let timing_libs = match timing_libs {
        Some(libs) if libs.len() == corners.len() => libs,
        Some(_) => {
            return Err(vec![Diag::error(
                "T0005",
                "one compiled library view per corner is required",
            )]);
        }
        None => compile_corner_libs(corners).map_err(|d| vec![d])?,
    };
    clock.finish();

    let clock = PhaseClock::start(ctx, "netlist_view");
    let corner_lib_slices: Vec<&[Library]> = corners.iter().map(|c| c.libs).collect();
    let netview = NetlistView::with_corners(netlist, &corner_lib_slices)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    clock.finish();

    let clock = PhaseClock::start(ctx, "timing_graph");
    let mut graph = TimingGraph::build(&netview, corners[0].libs)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    clock.finish();
    // `set_disable_timing`, netlist constants and case analysis (`STA-REQ-0150` to
    // `STA-REQ-0157`): the same preparation as every incremental update.
    let clock = PhaseClock::start(ctx, "constants");
    let consts =
        ConstantPropagation::apply(&netview, corners[0].libs, Some(constraints), &mut graph);
    let constant = consts.constant_nodes();
    clock.finish();

    let mut diags = constraints.diags.clone();
    diags.extend(consts.diagnostics(&netview));
    diags.extend(unmodelled_constraint_diags(&netview, constraints));
    for gd in graph.diagnostics() {
        diags.push(Diag::warning(gd.code, gd.message.clone()));
    }
    for d in &constraints.input_delays {
        let has_max = d.delay_ns[An::Late.index()].iter().any(Option::is_some);
        let has_min = d.delay_ns[An::Early.index()].iter().any(Option::is_some);
        if has_max != has_min {
            let bound = if has_max { "-max" } else { "-min" };
            diags.push(Diag::note(
                "T0031",
                format!(
                    "one-sided set_input_delay ({bound} only) on '{}' (STA-REQ-0219)",
                    netview.node_name_str(d.node)
                ),
            ));
        }
    }
    for d in &constraints.output_delays {
        let has_max = d.delay_ns[An::Late.index()].iter().any(Option::is_some);
        let has_min = d.delay_ns[An::Early.index()].iter().any(Option::is_some);
        if has_max != has_min {
            let bound = if has_max { "-max" } else { "-min" };
            diags.push(Diag::note(
                "T0031",
                format!(
                    "one-sided set_output_delay ({bound} only) on '{}' (STA-REQ-0219)",
                    netview.node_name_str(d.node)
                ),
            ));
        }
    }

    Ok((netview, graph, timing_libs, constant, diags))
}

/// The analysis of one corner up to its lowered problem: steps 1 to 7 of [`analyze_one_corner`].
struct CornerSetup<'l> {
    wire_model: ResolvedWireModel,
    net_loads: NetLoads,
    clock_model: IdealClockModel,
    tags: TagTable,
    exceptions: ExceptionContext,
    launch_slews: FxHashMap<NodeId, [f64; 4]>,
    driving_cells: FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
    latch_maps: ArcTagMaps,
    lowered: LoweredCorner<'l>,
    diags: Vec<Diag>,
}

#[allow(clippy::too_many_arguments)]
fn setup_corner<'l>(
    corner_id: CornerId,
    corner_name: &str,
    corner_libs: &[Library],
    lib: &'l TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
) -> Result<CornerSetup<'l>, Diag> {
    let netlist = netview.frozen();
    let mut diags = Vec::new();

    // 1. Resolve wire model (`STA-REQ-0122`..`0125`).
    let wl_cfg = build_wireload_config(netview, constraints);
    let (mut wire_model, wl_diags) = match options.wire_model {
        WireModelOption::Ideal => (ResolvedWireModel::Ideal(Ideal), Vec::new()),
        WireModelOption::Auto => ResolvedWireModel::resolve(netlist, corner_libs, &wl_cfg),
        WireModelOption::WireloadLumped => {
            let req_cfg = wl_cfg.with_require_wireload(true);
            ResolvedWireModel::resolve(netlist, corner_libs, &req_cfg)
        }
    };
    diags.extend(wl_diags);

    // 2. Compute per-net capacitive loads (`STA-REQ-0110`, `STA-REQ-0121`).
    let corner_rc = options.parasitics_for(corner_name)?;
    let load_overrides = build_net_load_overrides(
        netview,
        constraints,
        wire_model.is_ideal() || corner_rc.is_some(),
    );
    if let Some(input) = corner_rc {
        if !constraints.driving_cells.is_empty() {
            return Err(Diag::error(
                "T0077",
                "set_driving_cell is unsupported in extracted waveform mode; use port transition annotations",
            ));
        }
        if options.wire_model != WireModelOption::Auto {
            return Err(Diag::error(
                "T0070",
                "an explicit wire model cannot override extracted parasitics",
            ));
        }
        let max = options.max_state_bytes;
        let bound = Spef::bind(&input.data, netview, lib, &load_overrides, max)?;
        diags.clear();
        diags.extend(bound.topology().diagnostics(corner_name));
        wire_model = ResolvedWireModel::Extracted(bound);
        if input.data.coupling_records > 0 {
            diags.push(Diag::warning(
                "T0074",
                "inter-net coupling grounded at factor 1; crosstalk not analyzed",
            ));
        }
    }
    let net_loads = NetLoads::compute(netlist, lib, &wire_model, &load_overrides);

    // 3. Build ideal clock network (`STA-REQ-0200`).
    let clock_model = crate::engine::prop::clocks::propagated::build_model(
        netview,
        graph,
        lib,
        &net_loads,
        &wire_model,
        constraints,
        options,
        &mut diags,
    )?;

    // 4. Initialize the tag table and the timing exceptions (`STA-REQ-0217`, `STA-REQ-0220`,
    // `STA-REQ-0271`).
    let mut tags = TagTable::for_clocks_with_budget(constraints.clocks.len(), options.tag_budget)?;
    let (mut exceptions, exc_diags) =
        ExceptionContext::build(netview, graph, constraints, &clock_model, &mut tags)?;
    if corner_id.index() == 0 {
        diags.extend(exc_diags);
    }
    // Memory budget of the propagation state, checked before any of it is allocated (AGENTS
    // rule 9.2, `STA-REQ-0024`).
    let state_bytes =
        (graph.num_nodes() as u64 * u64::from(tags.ntags_for_compute()) * STATE_BYTES_PER_NODE_TAG)
            .saturating_add(
                graph.num_nodes() as u64
                    * constraints
                        .clocks
                        .iter()
                        .filter(|c| c.propagated && !c.is_virtual())
                        .count() as u64
                    * 128,
            )
            .saturating_add(crate::engine::limits::retained_bytes(
                graph.num_nodes(),
                options.max_limit_rows,
            ))
            // The parsed parasitics of every analyzed corner stay resident while any corner is
            // analyzed, so each corner counts all of them (`STA-REQ-0820`).
            .saturating_add(options.parasitics.iter().fold(0u64, |n, p| {
                n.saturating_add(p.data.estimated_state_bytes())
            }))
            .saturating_add(if corner_rc.is_some() {
                graph.arcs().len() as u64 * 192
            } else {
                0
            });
    if state_bytes > options.max_state_bytes {
        let gib = |b: u64| b as f64 / f64::from(1u32 << 30);
        return Err(Diag::error(
            "T0023",
            format!(
                "the propagation state of corner '{corner_name}' needs about {:.2} GiB ({} nodes x \
                 {} tags), above the limit of {:.2} GiB; raise --max-state-gib or reduce the \
                 clocks and timing exceptions",
                gib(state_bytes),
                graph.num_nodes(),
                tags.ntags_for_compute(),
                gib(options.max_state_bytes)
            ),
        ));
    }

    // 5. Build start rows (`STA-REQ-0217`, `STA-REQ-0218`, `STA-REQ-0219`).
    let start_build = build_start_rows(
        netview,
        graph,
        lib,
        constraints,
        &clock_model,
        &tags,
        &exceptions,
        &net_loads,
        &NoDerate,
    )?;

    // 6. Build latch `D -> Q` tag-mapping records (`STA-REQ-0221`, `STA-REQ-0222`).
    let latch_maps = build_latch_tag_maps(
        graph,
        constraints,
        &clock_model,
        &NoCppr,
        &tags,
        &mut exceptions,
    )?;

    // 7. Lower onto `rusteda_compute::timing` and pin latch `G -> Q` slews (`STA-REQ-0222`,
    // `STA-REQ-0401`).
    let ntags = tags.len().max(1) as u32;
    let lowered = lower_corner(
        netview,
        graph,
        lib,
        &net_loads,
        ntags,
        start_build.rows,
        &start_build.launch_slews,
        &start_build.launch_drives,
        &latch_maps,
        &wire_model,
        options.max_state_bytes,
    )?;

    Ok(CornerSetup {
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        launch_slews: start_build.launch_slews,
        driving_cells: start_build.driving_cells,
        latch_maps,
        lowered,
        diags,
    })
}

/// The timing graph of `netlist` and the lowered problem of every corner, as an analysis with
/// `timing_libs` and `constraints` builds them before it propagates: the state an incremental
/// analysis of a later netlist starts from (`STA-REQ-0601`, `STA-REQ-0610`).
pub fn lower_corners(
    netlist: &FrozenNetlist<'_>,
    corners: &[CornerLibs<'_>],
    timing_libs: &[TimingLib],
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
) -> Result<(TimingGraph, Vec<LoweredCorner<'static>>), Vec<Diag>> {
    if corners.is_empty() || timing_libs.len() != corners.len() {
        return Err(vec![Diag::error(
            "T0005",
            "one corner with a compiled library view per corner is required",
        )]);
    }
    let corner_lib_slices: Vec<&[Library]> = corners.iter().map(|c| c.libs).collect();
    let netview = NetlistView::with_corners(netlist, &corner_lib_slices)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    let mut graph = TimingGraph::build(&netview, corners[0].libs)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    ConstantPropagation::apply(&netview, corners[0].libs, Some(constraints), &mut graph);
    let mut lowered = Vec::with_capacity(corners.len());
    for (idx, (cl, tlib)) in corners.iter().zip(timing_libs).enumerate() {
        let setup = setup_corner(
            CornerId(idx as u32),
            cl.name,
            cl.libs,
            tlib,
            &netview,
            &graph,
            constraints,
            options,
        )
        .map_err(|d| vec![d])?;
        lowered.push(into_owned_lowered(setup.lowered));
    }
    Ok((graph, lowered))
}

#[allow(clippy::too_many_arguments)]
fn analyze_one_corner(
    corner_id: CornerId,
    corner_name: &str,
    corner_libs: &[Library],
    lib: TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constant: &[bool],
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    ctx: Ctx<'_>,
    propagation: Propagation<'_>,
    keep_lowered: bool,
    record: &mut PropagationRecord,
) -> Result<CornerAnalysis, Diag> {
    let netlist = netview.frozen();
    // 1. to 7. Wire model, loads, clocks, tags, exceptions, start rows, latch maps and lowering.
    let clock = PhaseClock::start(ctx, "corner_setup");
    let CornerSetup {
        wire_model,
        net_loads,
        clock_model,
        tags,
        mut exceptions,
        launch_slews,
        driving_cells,
        latch_maps,
        lowered,
        mut diags,
    } = setup_corner(
        corner_id,
        corner_name,
        corner_libs,
        &lib,
        netview,
        graph,
        constraints,
        options,
    )?;
    clock.finish();
    let relations = RelationTable::new();
    let mut range_tracker = RangeTracker::new();

    // 8. Propagate arrivals and slews (`STA-REQ-0400`..`0407`): every node, or the cone of the
    // nodes whose inputs differ from an earlier analysis (`STA-REQ-0453`, `STA-REQ-0601`).
    let clock = PhaseClock::start(ctx, "propagation");
    let propagated = match propagation {
        Propagation::Full => {
            let res = lowered
                .propagate(options.backend, options.precision, ctx)
                .map_err(|e| lower::compute_error_diag(&e))?;
            record.changed.push(None);
            record.seeds.push(graph.num_nodes());
            res
        }
        Propagation::Cone { prior, mapping } => {
            let pc = prior.corners.get(corner_id.index()).ok_or_else(|| {
                Diag::error(
                    "T0100",
                    format!("the earlier analysis has no corner '{corner_name}'"),
                )
            })?;
            let (res, changed, seeds) = propagate_forward_cone(
                Some(prior.graph),
                Some(pc.lowered),
                pc.node_values,
                graph,
                &lowered,
                mapping,
                options.backend,
                options.precision,
                ctx,
            )
            .map_err(|e| lower::compute_error_diag(&e))?;
            record.changed.push(Some(changed));
            record.seeds.push(seeds);
            res
        }
    };
    record.evaluated.push(propagated.stats.nodes_evaluated);
    let node_values = propagated.to_node_order(graph);
    clock.finish();
    // Static transitions of the pins without a propagated one, for the design rules (STA-REQ-0809).
    let statics = static_transitions(
        graph,
        netview,
        &lib,
        &lowered,
        &wire_model,
        &net_loads,
        &node_values,
        constant,
    );
    if keep_lowered {
        record.lowered.push(into_owned_lowered(lowered));
    } else {
        drop(lowered);
    }

    // 9. Check unanalyzed arcs, max capacitance, and max transition (`STA-REQ-0017`,
    // `STA-REQ-0015`, `STA-REQ-0120`).
    let clock = PhaseClock::start(ctx, "electrical_checks");
    let (unanalyzed_arcs, unanalyzed_diags) = lib.unanalyzed_arc_warnings(netlist);
    diags.extend(unanalyzed_diags);
    net_loads.check_max_capacitance(netlist, &lib, &mut range_tracker);
    check_max_transitions(netview, &lib, &node_values, &mut range_tracker);
    // Pulse widths, periods and design rules of the pins (`STA-REQ-0800` to `STA-REQ-0810`).
    let limits = check_corner_limits(
        &LimitInputs {
            netview,
            lib: &lib,
            constraints,
            clock_model: &clock_model,
            tags: &tags,
            node_values: &node_values,
            net_loads: &net_loads,
            pulse_rules: options.pulse_rules,
            clock_limits: options.clock_limits,
            constant,
            statics: &statics,
        },
        options.max_limit_rows,
        corner_id.index() == 0,
        &mut range_tracker,
        &mut diags,
    );
    clock.finish();

    // 10. Discover clock-gating sites, evaluate all timing checks, and classify endpoints
    // (`STA-REQ-0229`..`0226`).
    let clock = PhaseClock::start(ctx, "timing_checks");
    let gating_sites = discover_gating_sites(netview, graph, &lib, &clock_model);
    let check_records = evaluate_all_checks(
        netview,
        graph,
        &lib,
        constraints,
        &clock_model,
        &NoCppr,
        &tags,
        &mut exceptions,
        &gating_sites,
        &propagated,
        &mut range_tracker,
    )?;
    diags.extend(range_tracker.warnings(&lib.lut_bank));
    if corner_id.index() == 0 {
        diags.extend(exceptions.diagnostics());
    }

    clock.finish();
    let clock = PhaseClock::start(ctx, "endpoints");
    let endpoints = CornerEndpoints::build(
        corner_name,
        netview,
        graph,
        constraints,
        &clock_model,
        &tags,
        &gating_sites,
        &propagated,
        check_records,
    );
    clock.finish();

    Ok(CornerAnalysis {
        corner_id,
        corner_name: corner_name.to_string(),
        lib,
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        relations,
        launch_slews,
        driving_cells,
        latch_maps,
        gating_sites,
        propagated,
        node_values,
        endpoints,
        unanalyzed_arcs,
        limits,
        diags,
    })
}

fn count_applied_exceptions(constraints: &CompiledConstraints) -> ScopeExceptions {
    let mut exc = ScopeExceptions {
        clock_groups: constraints.clock_groups.len() as u32,
        ..ScopeExceptions::default()
    };
    for e in &constraints.exceptions {
        match e.kind {
            ExceptionKind::FalsePath { .. } => exc.false_path += 1,
            ExceptionKind::MulticyclePath { .. } => exc.multicycle_path += 1,
            ExceptionKind::MaxDelay { .. } => exc.max_delay += 1,
            ExceptionKind::MinDelay { .. } => exc.min_delay += 1,
        }
    }
    exc
}

fn assemble_analysis<'a>(
    netview: NetlistView<'a>,
    graph: TimingGraph,
    constraints: CompiledConstraints,
    corners: Vec<CornerAnalysis>,
    diags: Vec<Diag>,
) -> EngineAnalysis<'a> {
    let first = &corners[0];
    let wire_scope = first.wire_model.scope();
    let corner_names: Vec<String> = corners.iter().map(|c| c.corner_name.clone()).collect();

    let max_unanalyzed = corners
        .iter()
        .map(|c| c.unanalyzed_arcs as u32)
        .max()
        .unwrap_or(0);
    let merged_unconstrained = corners
        .iter()
        .fold(UnconstrainedCounts::default(), |acc, c| {
            acc.max_with(c.endpoints.unconstrained)
        });
    let completeness = ScopeCompleteness {
        unanalyzed_arcs: max_unanalyzed,
        unconstrained_endpoints: merged_unconstrained,
    };

    let scope = AnalysisScope {
        corners: corner_names,
        wire_model: wire_scope.wire_model.to_string(),
        clock_model: first.clock_model.scope_name().to_string(),
        cppr: "none".to_string(),
        pocv: "none".to_string(),
        crosstalk: "none".to_string(),
        exceptions_applied: count_applied_exceptions(&constraints),
        completeness,
    };

    let corner_summaries: Vec<CornerSummary> = corners
        .iter()
        .map(|c| c.endpoints.summary.clone())
        .collect();
    let sta_summary = StaSummary::from_corners(&corner_summaries);
    let sta_status = StaStatusValue::compute(&sta_summary, &completeness);

    EngineAnalysis {
        netview,
        graph,
        constraints,
        corners,
        scope,
        completeness,
        sta_summary,
        sta_status,
        diags,
    }
}
