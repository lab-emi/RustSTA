// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Constant propagation, case analysis and disabled arcs (owner: E7).
//!
//! Engine section B (`STA-REQ-0150` to `STA-REQ-0157`): three-valued Kleene constant propagation,
//! case analysis, cofactor-based arc disabling and `non_unate` sense refinement, `when` filtering,
//! `set_disable_timing`, sequential constant check classification, and disabled arc accounting.

use std::collections::BTreeMap;
use std::fmt;

use rusteda_core::job::Diag;
use rusteda_core::liberty::{
    Cell, Direction, Func, Library, Pin, TimingArc, TimingSense, TimingType,
};
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::CaseValue;

use super::build::{ArcKind, ArcSense, DisableReason, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, DisabledTiming};
use crate::engine::input::{NetlistView, NodeKind};
use crate::engine::types::{NetRow, NodeId, Tr};

mod propagate;
mod sequential;

use sequential::{latch_enable, unclocked_launches};

/// Canonical completeness record label for a [`DisableReason`] (`STA-REQ-0157`, `STA-REQ-0304`).
pub const fn disable_reason_str(reason: DisableReason) -> &'static str {
    match reason {
        DisableReason::LoopBreak => "cycle_break",
        DisableReason::Constant => "constant",
        DisableReason::CaseAnalysis => "case_analysis",
        DisableReason::FalseWhen => "false_when",
        DisableReason::UserDisabled => "user_disabled",
    }
}

/// Three-valued Kleene logic value (`0`, `1`, `X`) (`STA-REQ-0150`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LogicVal {
    /// Logic zero (`0`).
    Zero,
    /// Logic one (`1`).
    One,
    /// Unknown or switching value (`X`).
    #[default]
    X,
}

impl LogicVal {
    /// Converts a boolean into [`LogicVal::Zero`] (`false`) or [`LogicVal::One`] (`true`).
    pub const fn from_bool(b: bool) -> Self {
        if b { Self::One } else { Self::Zero }
    }

    /// Returns `Some(false)` for `Zero`, `Some(true)` for `One`, and `None` for `X`.
    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Zero => Some(false),
            Self::One => Some(true),
            Self::X => None,
        }
    }

    /// True if `self` is a known constant (`Zero` or `One`).
    pub const fn is_const(self) -> bool {
        !matches!(self, Self::X)
    }

    /// Three-valued Kleene negation (`!0 = 1`, `!1 = 0`, `!X = X`).
    pub const fn not(self) -> Self {
        match self {
            Self::Zero => Self::One,
            Self::One => Self::Zero,
            Self::X => Self::X,
        }
    }

    /// Three-valued Kleene conjunction (`0 & X = 0`, `1 & X = X`).
    pub const fn and(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::Zero, _) | (_, Self::Zero) => Self::Zero,
            (Self::One, v) | (v, Self::One) => v,
            (Self::X, Self::X) => Self::X,
        }
    }

    /// Three-valued Kleene disjunction (`1 | X = 1`, `0 | X = X`).
    pub const fn or(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::One, _) | (_, Self::One) => Self::One,
            (Self::Zero, v) | (v, Self::Zero) => v,
            (Self::X, Self::X) => Self::X,
        }
    }

    /// Three-valued Kleene exclusive-OR (`0 ^ v = v`, `1 ^ v = !v`, `X ^ X = X`).
    pub const fn xor(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::Zero, v) | (v, Self::Zero) => v,
            (Self::One, v) | (v, Self::One) => v.not(),
            (Self::X, Self::X) => Self::X,
        }
    }
}

impl fmt::Display for LogicVal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Zero => "0",
            Self::One => "1",
            Self::X => "X",
        })
    }
}

/// Provenance of a constant logic value (`STA-REQ-0150`, `STA-REQ-0151`, `STA-REQ-0156`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstOrigin {
    /// Constant originates purely from netlist ties (`1'b0`, `1'b1`, `TIELO`, `TIEHI`, `assign`).
    Netlist,
    /// Constant depends on `set_case_analysis`, `set_logic_zero`, or `set_logic_one`.
    CaseAnalysis,
}

impl ConstOrigin {
    /// Maps this constant origin to its corresponding [`DisableReason`].
    pub const fn disable_reason(self) -> DisableReason {
        match self {
            Self::Netlist => DisableReason::Constant,
            Self::CaseAnalysis => DisableReason::CaseAnalysis,
        }
    }

    /// Combines two constant origins (`CaseAnalysis` dominates if either input is from case analysis).
    pub const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::CaseAnalysis, _) | (_, Self::CaseAnalysis) => Self::CaseAnalysis,
            (Self::Netlist, Self::Netlist) => Self::Netlist,
        }
    }
}

/// Three-valued logic assignment on a pin or net together with its [`ConstOrigin`] when constant
/// (`STA-REQ-0150`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PinConst {
    /// Three-valued logic level (`Zero`, `One`, or `X`).
    pub value: LogicVal,
    /// Origin of the constant (`Some` iff `value` is `Zero` or `One`).
    pub origin: Option<ConstOrigin>,
}

impl PinConst {
    /// Unknown / switching pin state (`LogicVal::X`).
    pub const X: Self = Self {
        value: LogicVal::X,
        origin: None,
    };

    /// Creates a [`PinConst`] with `value` and `origin` (clearing `origin` if `value == LogicVal::X`).
    pub const fn new(value: LogicVal, origin: ConstOrigin) -> Self {
        match value {
            LogicVal::X => Self::X,
            LogicVal::Zero | LogicVal::One => Self {
                value,
                origin: Some(origin),
            },
        }
    }

    /// Creates a netlist-origin constant (`0` or `1`).
    pub const fn netlist(b: bool) -> Self {
        Self::new(LogicVal::from_bool(b), ConstOrigin::Netlist)
    }

    /// Creates a case-analysis-origin constant (`0` or `1`).
    pub const fn case_analysis(b: bool) -> Self {
        Self::new(LogicVal::from_bool(b), ConstOrigin::CaseAnalysis)
    }

    /// True if `self.value` is `Zero` or `One`.
    pub const fn is_const(self) -> bool {
        self.value.is_const()
    }

    /// Returns the [`DisableReason`] corresponding to `self.origin` when constant.
    pub const fn disable_reason(self) -> Option<DisableReason> {
        match self.origin {
            Some(o) if self.value.is_const() => Some(o.disable_reason()),
            _ => None,
        }
    }

    const fn netlist_only(self) -> Self {
        if matches!(self.origin, Some(ConstOrigin::Netlist)) {
            self
        } else {
            Self::X
        }
    }
}

/// Evaluates a Liberty Boolean [`Func`] under a three-valued Kleene variable environment
/// (`STA-REQ-0150`, `STA-REQ-0153`).
pub fn eval_func(func: &Func, env: &impl Fn(&str) -> LogicVal) -> LogicVal {
    match func {
        Func::Const(b) => LogicVal::from_bool(*b),
        Func::Var(name) => env(name),
        Func::Not(a) => eval_func(a, env).not(),
        Func::And(a, b) => match eval_func(a, env) {
            LogicVal::Zero => LogicVal::Zero,
            va => va.and(eval_func(b, env)),
        },
        Func::Or(a, b) => match eval_func(a, env) {
            LogicVal::One => LogicVal::One,
            va => va.or(eval_func(b, env)),
        },
        Func::Xor(a, b) => eval_func(a, env).xor(eval_func(b, env)),
    }
}

/// Evaluates a Liberty Boolean [`Func`] under a [`PinConst`] environment, attributing the result
/// to [`ConstOrigin::Netlist`] if netlist constants alone determine the constant value and to
/// [`ConstOrigin::CaseAnalysis`] otherwise (`STA-REQ-0150`, `STA-REQ-0151`).
pub fn eval_func_with_origin(func: &Func, env: &impl Fn(&str) -> PinConst) -> PinConst {
    let (_, net_val) = cofactor_summary(func, "", &|n| env(n).netlist_only());
    if net_val.is_const() {
        return PinConst::new(net_val, ConstOrigin::Netlist);
    }
    let (_, all_val) = cofactor_summary(func, "", env);
    if all_val.is_const() {
        return PinConst::new(all_val, ConstOrigin::CaseAnalysis);
    }
    PinConst::X
}

/// Outcome of cofactor analysis for a combinational arc `p -> y` (`STA-REQ-0151`, `STA-REQ-0159`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CofactorEffect {
    /// `f|_{p=0}` and `f|_{p=1}` are identically equal over all assignments of remaining `X`
    /// inputs (including when both evaluate to the same constant `0` or `1`); the arc is disabled.
    Independent(ConstOrigin),
    /// `f|_{p=0} <= f|_{p=1}` over all remaining `X` inputs (`positive_unate`).
    PositiveUnate,
    /// `f|_{p=0} >= f|_{p=1}` over all remaining `X` inputs (`negative_unate`).
    NegativeUnate,
    /// Output depends on `p` with neither `f|_{p=0} <= f|_{p=1}` nor `f|_{p=0} >= f|_{p=1}`.
    NonUnate,
}

/// Evaluates the cofactors `f|_{p=0}` and `f|_{p=1}` of `func` with respect to `pin_name` under
/// `env` (`STA-REQ-0151` to `STA-REQ-0153`).
pub fn analyze_arc_cofactors(
    func: &Func,
    pin_name: &str,
    env: &impl Fn(&str) -> PinConst,
) -> CofactorEffect {
    match cofactor_summary(func, pin_name, env).0 {
        0 => {
            let (net_rel, _) = cofactor_summary(func, pin_name, &|n| env(n).netlist_only());
            CofactorEffect::Independent(if net_rel == 0 {
                ConstOrigin::Netlist
            } else {
                ConstOrigin::CaseAnalysis
            })
        }
        1 => CofactorEffect::PositiveUnate,
        2 => CofactorEffect::NegativeUnate,
        _ => CofactorEffect::NonUnate,
    }
}

fn cofactor_summary<'a>(
    func: &'a Func,
    pin_name: &str,
    env: &impl Fn(&str) -> PinConst,
) -> (u8, LogicVal) {
    let mut vars: Vec<&'a str> = Vec::new();
    collect_vars(func, &mut vars);
    let mut x_vars: Vec<&'a str> = vars
        .into_iter()
        .filter(|&v| v != pin_name && !env(v).is_const())
        .collect();
    x_vars.truncate(12);

    let (mut rel, mut s0) = (0u8, 0u8);
    for mask in 0..(1usize << x_vars.len()) {
        let eval_at = |p_val: bool| {
            eval_func(func, &|name| {
                if !pin_name.is_empty() && name == pin_name {
                    LogicVal::from_bool(p_val)
                } else if env(name).is_const() {
                    env(name).value
                } else {
                    let pos = x_vars.iter().position(|&x| x == name).unwrap_or(0);
                    LogicVal::from_bool(((mask >> pos) & 1) != 0)
                }
            }) == LogicVal::One
        };
        let v0 = eval_at(false);
        let v1 = if pin_name.is_empty() {
            v0
        } else {
            eval_at(true)
        };
        rel |= u8::from(!v0 && v1) | (u8::from(v0 && !v1) << 1);
        s0 |= 1 << u8::from(v0);
    }
    let val = match s0 {
        1 => LogicVal::Zero,
        2 => LogicVal::One,
        _ => LogicVal::X,
    };
    (rel, val)
}

fn collect_vars<'a>(func: &'a Func, out: &mut Vec<&'a str>) {
    match func {
        Func::Const(_) => {}
        Func::Var(name) => {
            if !out.contains(&name.as_str()) {
                out.push(name.as_str());
            }
        }
        Func::Not(a) => collect_vars(a, out),
        Func::And(a, b) | Func::Or(a, b) | Func::Xor(a, b) => {
            collect_vars(a, out);
            collect_vars(b, out);
        }
    }
}

/// Breakdown of disabled timing arcs by [`DisableReason`] (`STA-REQ-0156`, `STA-REQ-0304`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisabledArcCounts {
    /// Arcs disabled to break combinational cycles (`loop_break`).
    pub loop_break: usize,
    /// Arcs disabled by netlist constants (`constant`).
    pub constant: usize,
    /// Arcs disabled by `set_case_analysis`, `set_logic_zero`, or `set_logic_one` (`case_analysis`).
    pub case_analysis: usize,
    /// Arcs disabled because their Liberty `when` expression evaluated to `0` (`false_when`).
    pub false_when: usize,
    /// Arcs disabled by `set_disable_timing` (`user_disabled`).
    pub user_disabled: usize,
}

impl DisabledArcCounts {
    fn from_slices(slices: &[&[Option<DisableReason>]]) -> Self {
        let mut counts = Self::default();
        for &r in slices.iter().flat_map(|s| s.iter()).flatten() {
            counts.record(r);
        }
        counts
    }

    /// Increments the counter for `reason`.
    pub fn record(&mut self, reason: DisableReason) {
        match reason {
            DisableReason::LoopBreak => self.loop_break += 1,
            DisableReason::Constant => self.constant += 1,
            DisableReason::CaseAnalysis => self.case_analysis += 1,
            DisableReason::FalseWhen => self.false_when += 1,
            DisableReason::UserDisabled => self.user_disabled += 1,
        }
    }

    /// Total number of disabled arcs across all reasons.
    pub const fn total(&self) -> usize {
        self.loop_break + self.constant + self.case_analysis + self.false_when + self.user_disabled
    }
}

/// Record of a combinational arc whose timing sense was refined from `non_unate` (`STA-REQ-0159`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefinedArc {
    /// Index of the arc in [`TimingGraph::arcs`].
    pub arc_idx: usize,
    /// Original timing sense (`ArcSense::NonUnate`).
    pub old_sense: ArcSense,
    /// Refined timing sense (`ArcSense::PositiveUnate` or `ArcSense::NegativeUnate`).
    pub new_sense: ArcSense,
}

/// Status of a timing check arc after constant propagation and arc disabling (`STA-REQ-0161`,
/// `STA-REQ-0156`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckStatus {
    /// Both data and clock paths are active.
    Active,
    /// Check arc was explicitly disabled (`user_disabled` or `false_when`).
    Disabled(DisableReason),
    /// Constrained (data / async control) pin is held constant or has no active data arrival
    /// (`no_data_arrival`).
    NoDataArrival,
    /// Related (clock / enable) pin is held constant or has no active clock source (`unclocked`).
    Unclocked,
}

impl CheckStatus {
    /// Canonical completeness category string (`STA-REQ-0157`, `STA-REQ-0226`, `STA-REQ-0304`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "constrained",
            Self::Disabled(_) => "disabled",
            Self::NoDataArrival => "no_data_arrival",
            Self::Unclocked => "unclocked",
        }
    }
}

/// Result of constant propagation, case analysis, `when`-arc filtering, and `set_disable_timing`
/// on a [`TimingGraph`] (`STA-REQ-0150` to `STA-REQ-0157`).
#[derive(Debug, Clone)]
pub struct ConstantPropagation {
    node_consts: Vec<PinConst>,
    net_consts: Vec<PinConst>,
    transition_case: BTreeMap<NodeId, Tr>,
    refined_arcs: Vec<RefinedArc>,
    check_status: Vec<CheckStatus>,
    disabled_arc_counts: DisabledArcCounts,
    total_disabled_counts: DisabledArcCounts,
    /// Instances of latch cells whose latch group stores the inverted data pin
    /// (`data_in : "!D"`), in instance order (`STA-REQ-0230`).
    inverted_data_latches: Vec<u32>,
}

impl ConstantPropagation {
    /// Propagates netlist constants only (`1'b0`, `1'b1`, `TIELO`, `TIEHI`, `assign`s) without
    /// SDC constraints (`STA-REQ-0150` to `STA-REQ-0161`, `STA-REQ-0156`).
    pub fn apply_netlist_only(
        netview: &NetlistView<'_>,
        libs: &[Library],
        graph: &mut TimingGraph,
    ) -> Self {
        Self::apply(netview, libs, None, graph)
    }

    /// Propagates netlist and constraint constants, applies `set_disable_timing`, evaluates
    /// Liberty `when` expressions, disables controlled/constant arcs, refines `non_unate` arc
    /// senses, relevelizes `graph`, and classifies sequential checks and disabled arc counts
    /// (`STA-REQ-0150` to `STA-REQ-0156`).
    pub fn apply(
        netview: &NetlistView<'_>,
        libs: &[Library],
        constraints: Option<&CompiledConstraints>,
        graph: &mut TimingGraph,
    ) -> Self {
        if let Some(c) = constraints {
            apply_user_disabled(netview, &c.disabled_timing, graph);
        }

        let mut eng = PropEngine::new(netview, libs, graph.num_nodes());
        let transition_case = eng.seed_constants(constraints);
        eng.propagate_fixed_point(graph);
        let refined_arcs = eng.disable_and_refine_arcs(graph);
        graph.set_tie_outputs(std::mem::take(&mut eng.tie_outputs));
        graph.relevelize();
        // A launch arc whose clock pin no clock reaches launches no arrival; its transition still
        // enters its output pin, so the levels order it (`STA-REQ-0116`).
        let unclocked = unclocked_launches(graph, constraints);
        if unclocked.iter().any(|&u| u) {
            graph.set_unclocked_launches(unclocked);
            graph.relevelize();
        }
        let check_status = eng.classify_checks(graph);
        let inverted_data_latches = eng.inverted_data_latches(graph);

        Self {
            node_consts: eng.node_consts,
            net_consts: eng.net_consts,
            transition_case,
            refined_arcs,
            check_status,
            disabled_arc_counts: DisabledArcCounts::from_slices(&[graph.disabled()]),
            total_disabled_counts: DisabledArcCounts::from_slices(&[
                graph.disabled(),
                graph.disabled_launches(),
                graph.disabled_checks(),
            ]),
            inverted_data_latches,
        }
    }

    /// Constant value and origin of `node`.
    pub fn node_const(&self, node: NodeId) -> PinConst {
        self.node_consts
            .get(node.index())
            .copied()
            .unwrap_or_default()
    }

    /// Per node, true when its logic value is a constant (`STA-REQ-0150`).
    pub fn constant_nodes(&self) -> Vec<bool> {
        self.node_consts.iter().map(|c| c.is_const()).collect()
    }

    /// Three-valued logic value of `node` (`Zero`, `One`, or `X`).
    pub fn node_val(&self, node: NodeId) -> LogicVal {
        self.node_const(node).value
    }

    /// Constant value and origin of raw `net`.
    pub fn net_const(&self, net: NetRow) -> PinConst {
        self.net_consts
            .get(net.index())
            .copied()
            .unwrap_or_default()
    }

    /// Optional transition restriction on `node` from `set_case_analysis rising|falling`.
    pub fn transition_case(&self, node: NodeId) -> Option<Tr> {
        self.transition_case.get(&node).copied()
    }

    /// Combinational arcs whose sense was refined from `non_unate` (`STA-REQ-0159`).
    pub fn refined_arcs(&self) -> &[RefinedArc] {
        &self.refined_arcs
    }

    /// Status of timing check `check_idx` (`Active`, `Disabled`, `NoDataArrival`, `Unclocked`).
    pub fn check_status(&self, check_idx: usize) -> CheckStatus {
        self.check_status
            .get(check_idx)
            .copied()
            .unwrap_or(CheckStatus::Unclocked)
    }

    /// Refines `check_status(check_idx)` with propagated clock and data arrival availability
    /// (`STA-REQ-0161`, `STA-REQ-0156`).
    pub fn classify_check_with_arrivals(
        &self,
        check_idx: usize,
        has_clock_arrival: bool,
        has_data_arrival: bool,
    ) -> CheckStatus {
        match self.check_status(check_idx) {
            CheckStatus::Active if !has_clock_arrival => CheckStatus::Unclocked,
            CheckStatus::Active if !has_data_arrival => CheckStatus::NoDataArrival,
            other => other,
        }
    }

    /// Disabled propagation arc counts broken down by [`DisableReason`] (`STA-REQ-0156`).
    pub fn disabled_arc_counts(&self) -> &DisabledArcCounts {
        &self.disabled_arc_counts
    }

    /// Sum of disabled propagation, launch, and check arc counts across all [`DisableReason`]s.
    pub fn total_disabled_counts(&self) -> DisabledArcCounts {
        self.total_disabled_counts
    }

    /// Diagnostics of the propagation for the analysis (AGENTS rule 7.1): `set_case_analysis
    /// rising|falling` leaves the logic state at `X` (`STA-REQ-0150`) and does not restrict the
    /// transitions that propagate, so each such pin gets warning `T0040` instead of being dropped
    /// silently.
    pub fn diagnostics(&self, netview: &NetlistView<'_>) -> Vec<Diag> {
        let mut out: Vec<Diag> = Vec::new();
        if !self.inverted_data_latches.is_empty() {
            let mut cells: Vec<&str> = self
                .inverted_data_latches
                .iter()
                .map(|&i| netview.cell_name(netview.frozen().instances[i as usize].cell))
                .collect();
            cells.sort_unstable();
            cells.dedup();
            out.push(Diag::note(
                "T0150",
                format!(
                    "{} latch instances of {} cells ({}) store the inverted data pin \
                     (data_in \"!D\"); their D -> Q arcs are timed with the timing_sense of the \
                     library arc (STA-REQ-0230)",
                    self.inverted_data_latches.len(),
                    cells.len(),
                    cells.iter().take(5).copied().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        out.extend(self.transition_case.iter().map(|(&node, tr)| {
            let edge = match tr {
                Tr::Rise => "rising",
                Tr::Fall => "falling",
            };
            Diag::warning(
                "T0040",
                format!(
                    "set_case_analysis {edge} on '{}' is not applied: both transitions are \
                         timed (STA-REQ-0158)",
                    netview.node_name_str(node)
                ),
            )
        }));
        out
    }

    /// The number of latch instances whose latch group stores the inverted data pin
    /// (`STA-REQ-0230`).
    pub fn inverted_data_latch_count(&self) -> usize {
        self.inverted_data_latches.len()
    }
}

struct CellMeta<'a> {
    cell: &'a Cell,
    cp_to_pin: Vec<Option<&'a Pin>>,
    prop_arcs: Vec<&'a TimingArc>,
    check_arcs: Vec<&'a TimingArc>,
    async_arcs: Vec<(u32, u32, bool, bool)>,
    /// True when evaluating an instance whose pins are all `X` changes none of its outputs (see
    /// `propagate::cell_is_x_inert`), so such an instance need not be evaluated.
    x_inert: bool,
}

#[derive(Clone, Copy)]
struct InstCtx<'a> {
    meta: &'a CellMeta<'a>,
    frozen: &'a FrozenNetlist<'a>,
    n_port_bits: usize,
    inst: u32,
}

fn inst_ctx<'a>(
    netview: &'a NetlistView<'_>,
    cell_lookup: &'a [Option<CellMeta<'a>>],
    inst: u32,
) -> Option<InstCtx<'a>> {
    let frozen = netview.frozen();
    let cell_row = frozen.instances.get(inst as usize)?.cell as usize;
    let meta = cell_lookup.get(cell_row)?.as_ref()?;
    Some(InstCtx {
        meta,
        frozen,
        n_port_bits: netview.num_port_bits(),
        inst,
    })
}

impl<'a> InstCtx<'a> {
    fn pin(&self, node_consts: &[PinConst], name: &str) -> PinConst {
        for p in self.frozen.instance_pins(self.inst) {
            let cp = self.frozen.pin_cellpin[p] as usize;
            if let Some(&Some(lp)) = self.meta.cp_to_pin.get(cp)
                && lp.name == name
            {
                return node_consts[self.n_port_bits + p];
            }
        }
        PinConst::X
    }

    /// True if an input pin of the instance is at a constant.
    fn has_const_input(&self, node_consts: &[PinConst]) -> bool {
        self.frozen.instance_pins(self.inst).any(|p| {
            let cp = self.frozen.pin_cellpin[p] as usize;
            matches!(
                self.meta.cp_to_pin.get(cp),
                Some(Some(lp)) if matches!(lp.direction, Direction::Input | Direction::Inout)
            ) && node_consts[self.n_port_bits + p].is_const()
        })
    }

    fn cp_node(&self, target_cp: u32) -> Option<usize> {
        self.frozen
            .instance_pins(self.inst)
            .find(|&p| self.frozen.pin_cellpin[p] == target_cp)
            .map(|p| self.n_port_bits + p)
    }

    fn when_false(&self, node_consts: &[PinConst], arc: &TimingArc) -> bool {
        arc.when.as_ref().is_some_and(|when_fn| {
            eval_func(when_fn, &|name| self.pin(node_consts, name).value) == LogicVal::Zero
        })
    }
}

struct PropEngine<'a> {
    netview: &'a NetlistView<'a>,
    cell_lookup: Vec<Option<CellMeta<'a>>>,
    node_consts: Vec<PinConst>,
    net_consts: Vec<PinConst>,
    explicit_case_node: Vec<bool>,
    /// Outputs of tie cells, in node order (`STA-REQ-0116`).
    tie_outputs: Vec<NodeId>,
}

impl<'a> PropEngine<'a> {
    fn new(netview: &'a NetlistView<'a>, libs: &'a [Library], n_nodes: usize) -> Self {
        let cell_lookup = (0..netview.num_cells())
            .map(|cr| {
                let name = netview.cell_name(cr as u32);
                libs.iter()
                    .flat_map(|l| &l.cells)
                    .find(|c| c.name == name)
                    .map(|c| build_cell_meta(netview, cr as u32, c))
            })
            .collect();
        Self {
            netview,
            cell_lookup,
            node_consts: vec![PinConst::X; n_nodes],
            net_consts: vec![PinConst::X; netview.num_nets()],
            explicit_case_node: vec![false; n_nodes],
            tie_outputs: Vec::new(),
        }
    }

    fn seed_constants(
        &mut self,
        constraints: Option<&CompiledConstraints>,
    ) -> BTreeMap<NodeId, Tr> {
        let (n_nodes, n_nets) = (self.node_consts.len(), self.net_consts.len());
        let mut transition_case = BTreeMap::new();

        for &(nr, val) in self.netview.const_nets() {
            if nr.index() < n_nets {
                self.net_consts[nr.index()] = PinConst::netlist(val);
            }
        }
        for (v, item) in self.node_consts.iter_mut().enumerate() {
            if let Some(val) = self.netview.is_const_node(NodeId(v as u32)) {
                *item = PinConst::netlist(val);
            }
        }

        for inst_row in 0..self.netview.num_instances() {
            let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, inst_row as u32) else {
                continue;
            };
            if !ictx.meta.cell.sequentials.is_empty() {
                continue;
            }
            for pin_row in ictx.frozen.instance_pins(inst_row as u32) {
                let cp = ictx.frozen.pin_cellpin[pin_row] as usize;
                if let Some(&Some(lp)) = ictx.meta.cp_to_pin.get(cp)
                    && matches!(lp.direction, Direction::Output | Direction::Inout)
                    && let Some(Func::Const(val)) = lp.function
                {
                    self.node_consts[ictx.n_port_bits + pin_row] = PinConst::netlist(val);
                    self.tie_outputs
                        .push(NodeId((ictx.n_port_bits + pin_row) as u32));
                }
            }
        }

        if let Some(c) = constraints {
            let mut set_case = |nid: NodeId, val: bool| {
                if nid.index() < n_nodes {
                    self.node_consts[nid.index()] = PinConst::case_analysis(val);
                    self.explicit_case_node[nid.index()] = true;
                }
            };
            for &nid in &c.logic_zero_ports {
                set_case(nid, false);
            }
            for &nid in &c.logic_one_ports {
                set_case(nid, true);
            }
            for (&nid, &cv) in &c.case_analysis {
                match cv {
                    CaseValue::Zero => set_case(nid, false),
                    CaseValue::One => set_case(nid, true),
                    CaseValue::Rise if nid.index() < n_nodes => {
                        transition_case.insert(nid, Tr::Rise);
                    }
                    CaseValue::Fall if nid.index() < n_nodes => {
                        transition_case.insert(nid, Tr::Fall);
                    }
                    _ => {}
                }
            }
        }
        transition_case
    }

    fn disable_and_refine_arcs(&self, graph: &mut TimingGraph) -> Vec<RefinedArc> {
        let mut refined_arcs = Vec::new();
        for arc_idx in 0..graph.arcs().len() {
            if graph.is_arc_disabled(arc_idx) {
                continue;
            }
            let arc = graph.arcs()[arc_idx];
            let Some((inst, lib_arc)) = arc.kind.inst_and_lib_arc() else {
                continue;
            };
            let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, inst) else {
                continue;
            };
            let pin_env = |name: &str| ictx.pin(&self.node_consts, name);

            if let Some(&lib_timing_arc) = ictx.meta.prop_arcs.get(lib_arc.index())
                && ictx.when_false(&self.node_consts, lib_timing_arc)
            {
                graph.disable_arc(arc_idx, DisableReason::FalseWhen);
                continue;
            }
            if let Some(reason) = self.node_consts[arc.src.index()].disable_reason() {
                graph.disable_arc(arc_idx, reason);
                continue;
            }

            if let ArcKind::LatchData { setup_check, .. } = arc.kind {
                // A latch whose enable logic values hold active is transparent: `D -> Q` becomes
                // a combinational arc; held inactive it is opaque and the arc carries no arrival
                // (`STA-REQ-0161`).
                if let Some(reason) = self.node_consts[arc.dst.index()].disable_reason() {
                    graph.disable_arc(arc_idx, reason);
                } else if let Some(reason) = graph
                    .checks()
                    .get(setup_check as usize)
                    .and_then(|ch| self.node_consts[ch.clock_pin.index()].disable_reason())
                {
                    let from_name = pin_pair(self.netview, arc.src, arc.dst)
                        .map(|(_, cell, from_cp, _)| self.netview.cell_pin_name(cell, from_cp));
                    if latch_enable(ictx.meta.cell, from_name, &pin_env) == LogicVal::One {
                        graph.make_latch_transparent(arc_idx);
                    } else {
                        graph.disable_arc(arc_idx, reason);
                    }
                }
            } else if matches!(arc.kind, ArcKind::Comb { .. }) {
                let Some((_, cell, from_cp, to_cp)) = pin_pair(self.netview, arc.src, arc.dst)
                else {
                    continue;
                };
                let from_name = self.netview.cell_pin_name(cell, from_cp);
                let Some(&Some(to_pin)) = ictx.meta.cp_to_pin.get(to_cp as usize) else {
                    continue;
                };

                if let Some(ref ts_fn) = to_pin.three_state_function {
                    let ts_pc = eval_func_with_origin(ts_fn, &pin_env);
                    if ts_pc.value == LogicVal::One
                        && let Some(reason) = ts_pc.disable_reason()
                    {
                        graph.disable_arc(arc_idx, reason);
                        continue;
                    }
                }
                // Without a constant input the library's arcs and senses stand: refinement and
                // cofactor disabling apply to the residual function of constant inputs only
                // (`STA-REQ-0159`, `STA-REQ-0153`).
                if let Some(ref func) = to_pin.function
                    && ictx.has_const_input(&self.node_consts)
                {
                    let new_sense = match analyze_arc_cofactors(func, from_name, &pin_env) {
                        CofactorEffect::Independent(origin) => {
                            graph.disable_arc(arc_idx, origin.disable_reason());
                            continue;
                        }
                        CofactorEffect::PositiveUnate => Some(ArcSense::PositiveUnate),
                        CofactorEffect::NegativeUnate => Some(ArcSense::NegativeUnate),
                        CofactorEffect::NonUnate => None,
                    };
                    match new_sense {
                        Some(ns) if arc.sense == ArcSense::NonUnate => {
                            graph.set_arc_sense(arc_idx, ns);
                            refined_arcs.push(RefinedArc {
                                arc_idx,
                                old_sense: ArcSense::NonUnate,
                                new_sense: ns,
                            });
                        }
                        // A unate arc whose sense the residual function contradicts cannot
                        // switch the output: it is disabled (`STA-REQ-0159`).
                        Some(ns) if arc.sense != ns => {
                            let netlist_env = |name: &str| pin_env(name).netlist_only();
                            let netlist = analyze_arc_cofactors(func, from_name, &netlist_env);
                            let origin = if effect_sense(netlist).is_some_and(|s| s != arc.sense) {
                                ConstOrigin::Netlist
                            } else {
                                ConstOrigin::CaseAnalysis
                            };
                            graph.disable_arc(arc_idx, origin.disable_reason());
                        }
                        _ => {}
                    }
                }
            }
        }

        for li in 0..graph.launches().len() {
            if graph.is_launch_disabled(li) {
                continue;
            }
            let launch = graph.launches()[li];
            if let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, launch.inst)
                && let Some(&lib_timing_arc) = ictx.meta.prop_arcs.get(launch.lib_arc.index())
                && ictx.when_false(&self.node_consts, lib_timing_arc)
            {
                graph.disable_launch(li, DisableReason::FalseWhen);
                continue;
            }
            if let Some(reason) = self.node_consts[launch.clock_pin.index()]
                .disable_reason()
                .or_else(|| self.node_consts[launch.out_pin.index()].disable_reason())
            {
                graph.disable_launch(li, reason);
            }
        }
        refined_arcs
    }

    fn classify_checks(&self, graph: &mut TimingGraph) -> Vec<CheckStatus> {
        let n_port_bits = self.netview.num_port_bits();
        let n_nodes = graph.num_nodes();

        let mut port_reachable = vec![false; n_nodes];
        for (pb, slot) in port_reachable.iter_mut().enumerate().take(n_port_bits) {
            if self.netview.is_driver(NodeId(pb as u32)) && !self.node_consts[pb].is_const() {
                *slot = true;
            }
        }
        propagate_reachability(graph, &self.node_consts, &mut port_reachable);

        let mut data_reachable = port_reachable.clone();
        for (li, launch) in graph.launches().iter().enumerate() {
            if !graph.is_launch_disabled(li)
                && port_reachable[launch.clock_pin.index()]
                && !self.node_consts[launch.out_pin.index()].is_const()
            {
                data_reachable[launch.out_pin.index()] = true;
            }
        }
        propagate_reachability(graph, &self.node_consts, &mut data_reachable);

        let n_checks = graph.checks().len();
        let mut statuses = Vec::with_capacity(n_checks);

        for ci in 0..n_checks {
            if let Some(reason) = graph.check_disable_reason(ci) {
                statuses.push(CheckStatus::Disabled(reason));
                continue;
            }
            let check = graph.checks()[ci];
            if let Some(ictx) = inst_ctx(self.netview, &self.cell_lookup, check.inst)
                && let Some(&lib_timing_arc) = ictx.meta.check_arcs.get(check.lib_arc.index())
                && ictx.when_false(&self.node_consts, lib_timing_arc)
            {
                graph.disable_check(ci, DisableReason::FalseWhen);
                statuses.push(CheckStatus::Disabled(DisableReason::FalseWhen));
                continue;
            }
            if let Some(reason) = self.node_consts[check.clock_pin.index()].disable_reason() {
                graph.disable_check(ci, reason);
                statuses.push(CheckStatus::Unclocked);
                continue;
            }
            if !port_reachable[check.clock_pin.index()] {
                statuses.push(CheckStatus::Unclocked);
                continue;
            }
            if let Some(reason) = self.node_consts[check.data_pin.index()].disable_reason() {
                graph.disable_check(ci, reason);
                statuses.push(CheckStatus::NoDataArrival);
                continue;
            }
            if !data_reachable[check.data_pin.index()] {
                statuses.push(CheckStatus::NoDataArrival);
                continue;
            }
            statuses.push(CheckStatus::Active);
        }
        statuses
    }
}

fn build_cell_meta<'a>(
    netview: &NetlistView<'_>,
    cell_row: u32,
    lib_cell: &'a Cell,
) -> CellMeta<'a> {
    use TimingType::*;
    let n_pins = netview.frozen().cell_pins(cell_row).len();
    let cp_to_pin: Vec<Option<&'a Pin>> = (0..n_pins)
        .map(|cp| {
            let name = netview.cell_pin_name(cell_row, cp as u32);
            lib_cell.pins.iter().find(|p| p.name == name)
        })
        .collect();
    let find_cp = |name: &str| -> Option<u32> {
        cp_to_pin
            .iter()
            .position(|p| p.is_some_and(|lp| lp.name == name))
            .map(|i| i as u32)
    };

    let (mut prop_arcs, mut check_arcs, mut async_arcs) = (Vec::new(), Vec::new(), Vec::new());
    for pin in &lib_cell.pins {
        let Some(to_cp) = find_cp(&pin.name) else {
            continue;
        };
        for arc in &pin.arcs {
            let Some(from_cp) = find_cp(&arc.related_pin) else {
                continue;
            };
            match arc.kind() {
                SetupRising | SetupFalling | HoldRising | HoldFalling | RecoveryRising
                | RecoveryFalling | RemovalRising | RemovalFalling => check_arcs.push(arc),
                Combinational | CombinationalRise | CombinationalFall | RisingEdge
                | FallingEdge => prop_arcs.push(arc),
                ThreeStateEnable | ThreeStateEnableRise | ThreeStateEnableFall => {
                    prop_arcs.push(arc)
                }
                ThreeStateDisable | ThreeStateDisableRise | ThreeStateDisableFall => {
                    prop_arcs.push(arc)
                }
                tt @ (Preset | Clear) => {
                    let is_preset = tt == Preset;
                    let active_high = !matches!(
                        (is_preset, arc.sense()),
                        (true, Some(TimingSense::NegativeUnate))
                            | (false, Some(TimingSense::PositiveUnate))
                    );
                    async_arcs.push((from_cp, to_cp, is_preset, active_high));
                }
                _ => {}
            }
        }
    }
    let x_inert = propagate::cell_is_x_inert(lib_cell, &cp_to_pin, !async_arcs.is_empty());
    CellMeta {
        cell: lib_cell,
        cp_to_pin,
        prop_arcs,
        check_arcs,
        async_arcs,
        x_inert,
    }
}

fn apply_user_disabled(
    netview: &NetlistView<'_>,
    specs: &[DisabledTiming],
    graph: &mut TimingGraph,
) {
    if specs.is_empty() {
        return;
    }
    for ai in 0..graph.arcs().len() {
        let arc = graph.arcs()[ai];
        let matched = match arc.kind {
            ArcKind::Net { .. } => specs.iter().any(|s| {
                matches!(s, DisabledTiming::Port { port } if *port == arc.src || *port == arc.dst)
            }),
            _ => matches_disabled_pair(netview, arc.src, arc.dst, false, specs),
        };
        if matched {
            graph.disable_arc(ai, DisableReason::UserDisabled);
        }
    }
    for li in 0..graph.launches().len() {
        let l = graph.launches()[li];
        if matches_disabled_pair(netview, l.clock_pin, l.out_pin, false, specs) {
            graph.disable_launch(li, DisableReason::UserDisabled);
        }
    }
    for ci in 0..graph.checks().len() {
        let ch = graph.checks()[ci];
        if matches_disabled_pair(netview, ch.clock_pin, ch.data_pin, true, specs) {
            graph.disable_check(ci, DisableReason::UserDisabled);
        }
    }
}

/// The unate sense of a cofactor effect, `None` when it is independent or non-unate.
fn effect_sense(effect: CofactorEffect) -> Option<ArcSense> {
    match effect {
        CofactorEffect::PositiveUnate => Some(ArcSense::PositiveUnate),
        CofactorEffect::NegativeUnate => Some(ArcSense::NegativeUnate),
        CofactorEffect::Independent(_) | CofactorEffect::NonUnate => None,
    }
}

fn pin_pair(netview: &NetlistView<'_>, src: NodeId, dst: NodeId) -> Option<(u32, u32, u32, u32)> {
    let NodeKind::InstancePin {
        inst,
        cell,
        cell_pin: f,
        ..
    } = netview.node_kind(src)
    else {
        return None;
    };
    let NodeKind::InstancePin { cell_pin: t, .. } = netview.node_kind(dst) else {
        return None;
    };
    Some((inst, cell, f, t))
}

fn matches_disabled_pair(
    netview: &NetlistView<'_>,
    src_node: NodeId,
    dst_node: NodeId,
    bidirectional: bool,
    specs: &[DisabledTiming],
) -> bool {
    let Some((inst, cell, f_cp, t_cp)) = pin_pair(netview, src_node, dst_node) else {
        return false;
    };
    let (cell_name, f_name, t_name) = (
        netview.cell_name(cell),
        netview.cell_pin_name(cell, f_cp),
        netview.cell_pin_name(cell, t_cp),
    );
    let check_dir = |fnm: &str, tnm: &str, s: &DisabledTiming| match s {
        DisabledTiming::Cell { inst: t } => *t == inst,
        DisabledTiming::CellArc {
            inst: t,
            from_pin,
            to_pin,
        } => {
            *t == inst
                && (from_pin.is_empty() || from_pin == fnm)
                && (to_pin.is_empty() || to_pin == tnm)
        }
        DisabledTiming::Pin { pin } => pin.node.is_some_and(|n| n == src_node || n == dst_node),
        DisabledTiming::Port { port } => *port == src_node || *port == dst_node,
        DisabledTiming::LibCell {
            lib_cell,
            from_pin,
            to_pin,
        } => {
            lib_cell.cell == cell_name
                && from_pin.as_deref().is_none_or(|f| f == fnm)
                && to_pin.as_deref().is_none_or(|t| t == tnm)
        }
        DisabledTiming::LibPin { lib_pin } => {
            lib_pin.cell == cell_name && (lib_pin.pin == fnm || lib_pin.pin == tnm)
        }
    };
    specs
        .iter()
        .any(|s| check_dir(f_name, t_name, s) || (bidirectional && check_dir(t_name, f_name, s)))
}

fn propagate_reachability(graph: &TimingGraph, node_consts: &[PinConst], reach: &mut [bool]) {
    for &v in graph.order() {
        if !reach[v.index()] || node_consts[v.index()].is_const() {
            continue;
        }
        for &ai in graph.outgoing(v) {
            let dst = graph.arcs()[ai as usize].dst;
            if !node_consts[dst.index()].is_const() {
                reach[dst.index()] = true;
            }
        }
    }
}
