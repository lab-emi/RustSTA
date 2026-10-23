// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent Kleene ternary constant propagation, unateness refinement, and arc disabling
//! (`STA-REQ-0150` to `0156`).

use std::collections::BTreeMap;

use rusteda_core::liberty::{Direction, Func, TimingSense};

use super::canon_sdc::CanonSdc;
use super::lib_model::{CellDef, CombArc, CornerLib, collect_vars};
use super::verilog::{FlatDesign, NodeKind};

/// Three-valued logic state (`0`, `1`, or `X` unknown) (`STA-REQ-0151`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicVal {
    Zero,
    One,
    X,
}

impl LogicVal {
    #[must_use]
    pub const fn from_bool(b: bool) -> Self {
        if b { Self::One } else { Self::Zero }
    }

    #[must_use]
    pub const fn to_bool(self) -> Option<bool> {
        match self {
            Self::Zero => Some(false),
            Self::One => Some(true),
            Self::X => None,
        }
    }

    #[must_use]
    pub const fn not(self) -> Self {
        match self {
            Self::Zero => Self::One,
            Self::One => Self::Zero,
            Self::X => Self::X,
        }
    }

    #[must_use]
    pub const fn and(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::Zero, _) | (_, Self::Zero) => Self::Zero,
            (Self::One, Self::One) => Self::One,
            _ => Self::X,
        }
    }

    #[must_use]
    pub const fn or(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::One, _) | (_, Self::One) => Self::One,
            (Self::Zero, Self::Zero) => Self::Zero,
            _ => Self::X,
        }
    }

    #[must_use]
    pub const fn xor(self, rhs: Self) -> Self {
        match (self, rhs) {
            (Self::Zero, Self::Zero) | (Self::One, Self::One) => Self::Zero,
            (Self::Zero, Self::One) | (Self::One, Self::Zero) => Self::One,
            _ => Self::X,
        }
    }
}

/// Evaluates a Liberty boolean expression `f` under Kleene ternary logic (`STA-REQ-0151`).
#[must_use]
pub fn eval_kleene(f: &Func, env: &dyn Fn(&str) -> LogicVal) -> LogicVal {
    match f {
        Func::Const(b) => LogicVal::from_bool(*b),
        Func::Var(name) => env(name),
        Func::Not(inner) => eval_kleene(inner, env).not(),
        Func::And(a, b) => eval_kleene(a, env).and(eval_kleene(b, env)),
        Func::Or(a, b) => eval_kleene(a, env).or(eval_kleene(b, env)),
        Func::Xor(a, b) => eval_kleene(a, env).xor(eval_kleene(b, env)),
    }
}

/// Reason why a timing arc is disabled (`STA-REQ-0153`, `0154`, `0155`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableReason {
    UserDisabled,
    WhenFalse,
    CaseAnalysis,
}

/// Result of constant propagation and arc disabling on a design and corner.
#[derive(Debug, Clone)]
pub struct ConstPropResult {
    /// Constant logic state of each node `pin_vals[node_idx]`.
    pub pin_vals: Vec<LogicVal>,
    /// Disabled combinational arcs `(inst_idx, comb_arc_idx) -> DisableReason`.
    pub disabled_comb_arcs: BTreeMap<(usize, usize), DisableReason>,
    /// Refined `TimingSense` for active combinational arcs `(inst_idx, comb_arc_idx) -> TimingSense`.
    pub refined_comb_sense: BTreeMap<(usize, usize), TimingSense>,
    /// Disabled sequential launch arcs `(inst_idx, launch_arc_idx) -> DisableReason`.
    pub disabled_launch_arcs: BTreeMap<(usize, usize), DisableReason>,
    /// Disabled sequential check arcs `(inst_idx, check_arc_idx) -> DisableReason`.
    pub disabled_check_arcs: BTreeMap<(usize, usize), DisableReason>,
}

/// Runs Kleene ternary constant propagation, applies `set_disable_timing`, disables non-toggling
/// and `when = 0` arcs, and refines combinational unateness (`STA-REQ-0150` to `0156`).
#[must_use]
pub fn propagate_constants(
    design: &FlatDesign,
    lib: &CornerLib,
    sdc: &CanonSdc,
) -> ConstPropResult {
    let n_nodes = design.nodes.len();
    let mut pin_vals = vec![LogicVal::X; n_nodes];
    let mut pinned_by_sdc = vec![false; n_nodes];

    // 1. Seed constants from structural `1'b0` / `1'b1` ties and constant cells (`TIELO`/`TIEHI`).
    for (ni, node) in design.nodes.iter().enumerate() {
        if let Some(b) = node.const_tie {
            pin_vals[ni] = LogicVal::from_bool(b);
        }
    }
    for inst in &design.instances {
        let Some(cdef) = lib.cells.get(&inst.cell_type) else {
            continue;
        };
        if !is_pure_combinational(cdef) {
            continue;
        }
        for (pname, pdef) in &cdef.pins {
            if pdef.direction == Direction::Output
                && pdef.three_state.is_none()
                && let Some(f) = &pdef.function
            {
                let v = eval_kleene(f, &|_| LogicVal::X);
                if v != LogicVal::X
                    && let Some(&out_node) = inst.pin_nodes.get(pname)
                {
                    pin_vals[out_node] = v;
                }
            }
        }
    }

    // 2. Apply `set_case_analysis` overrides (`STA-REQ-0150`).
    for (name, &val) in &sdc.case_analysis {
        if let Some(&ni) = design.node_map.get(name) {
            pin_vals[ni] = LogicVal::from_bool(val);
            pinned_by_sdc[ni] = true;
        }
    }

    // 3. Apply `set_disable_timing` (`STA-REQ-0155`).
    let mut disabled_comb_arcs: BTreeMap<(usize, usize), DisableReason> = BTreeMap::new();
    let mut disabled_launch_arcs: BTreeMap<(usize, usize), DisableReason> = BTreeMap::new();
    let mut disabled_check_arcs: BTreeMap<(usize, usize), DisableReason> = BTreeMap::new();
    apply_user_disable_timing(
        design,
        lib,
        sdc,
        &mut disabled_comb_arcs,
        &mut disabled_launch_arcs,
        &mut disabled_check_arcs,
    );

    // 4. Fixed-point Kleene propagation across nets and combinational cells (`STA-REQ-0151`, `0152`).
    for _ in 0..n_nodes.max(4) {
        let mut changed = false;

        // Propagate from net drivers to net sinks.
        for net in &design.nets {
            let mut drv_val = LogicVal::X;
            for &drv in &net.drivers {
                let v = pin_vals[drv];
                if v == LogicVal::X {
                    continue;
                }
                // Check if driver is a three-state output pin that is currently disabled (hi-Z).
                if let NodeKind::InstancePin { inst_idx, ref pin } = design.nodes[drv].kind {
                    let inst = &design.instances[inst_idx];
                    if let Some(cdef) = lib.cells.get(&inst.cell_type)
                        && let Some(pdef) = cdef.pins.get(pin)
                        && let Some(ts) = &pdef.three_state
                    {
                        let env = |var: &str| {
                            inst.pin_nodes
                                .get(var)
                                .map_or(LogicVal::X, |&ni| pin_vals[ni])
                        };
                        if eval_kleene(ts, &env) == LogicVal::One {
                            continue;
                        }
                    }
                }
                drv_val = v;
                break;
            }
            if drv_val != LogicVal::X {
                for &snk in &net.sinks {
                    if !pinned_by_sdc[snk] && pin_vals[snk] != drv_val {
                        pin_vals[snk] = drv_val;
                        changed = true;
                    }
                }
            }
        }

        // Propagate across pure combinational cells (never across sequential cells, STA-REQ-0152).
        for (inst_idx, inst) in design.instances.iter().enumerate() {
            let Some(cdef) = lib.cells.get(&inst.cell_type) else {
                continue;
            };
            if !is_pure_combinational(cdef) {
                continue;
            }
            for (pname, pdef) in &cdef.pins {
                if pdef.direction != Direction::Output {
                    continue;
                }
                let Some(&out_node) = inst.pin_nodes.get(pname) else {
                    continue;
                };
                if pinned_by_sdc[out_node] {
                    continue;
                }
                if let Some(ts) = &pdef.three_state {
                    let env = |var: &str| {
                        inst.pin_nodes
                            .get(var)
                            .map_or(LogicVal::X, |&ni| pin_vals[ni])
                    };
                    if eval_kleene(ts, &env) == LogicVal::One {
                        continue;
                    }
                }
                let Some(func) = &pdef.function else {
                    continue;
                };
                let env = |var: &str| {
                    // If all combinational arcs `var -> pname` are user-disabled, treat `var` as X.
                    let any_arc = cdef
                        .comb_arcs
                        .iter()
                        .any(|ca| ca.from_pin == var && ca.to_pin == *pname);
                    if any_arc {
                        let all_user_dis = cdef
                            .comb_arcs
                            .iter()
                            .enumerate()
                            .filter(|(_, ca)| ca.from_pin == var && ca.to_pin == *pname)
                            .all(|(idx, _)| {
                                disabled_comb_arcs.get(&(inst_idx, idx))
                                    == Some(&DisableReason::UserDisabled)
                            });
                        if all_user_dis {
                            return LogicVal::X;
                        }
                    }
                    inst.pin_nodes
                        .get(var)
                        .map_or(LogicVal::X, |&ni| pin_vals[ni])
                };
                let out_v = eval_kleene(func, &env);
                if out_v != LogicVal::X && pin_vals[out_node] != out_v {
                    pin_vals[out_node] = out_v;
                    changed = true;
                }
            }
        }

        if !changed {
            break;
        }
    }

    // 5. Evaluate `when` conditions, disable non-toggling arcs, and refine unateness (`STA-REQ-0153`, `0154`).
    let mut refined_comb_sense: BTreeMap<(usize, usize), TimingSense> = BTreeMap::new();

    for (inst_idx, inst) in design.instances.iter().enumerate() {
        let Some(cdef) = lib.cells.get(&inst.cell_type) else {
            continue;
        };
        let env = |var: &str| {
            inst.pin_nodes
                .get(var)
                .map_or(LogicVal::X, |&ni| pin_vals[ni])
        };

        for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
            if disabled_comb_arcs.contains_key(&(inst_idx, ca_idx)) {
                continue;
            }
            if let Some(w) = &ca.when
                && eval_kleene(w, &env) == LogicVal::Zero
            {
                disabled_comb_arcs.insert((inst_idx, ca_idx), DisableReason::WhenFalse);
                continue;
            }
            let (Some(&u), Some(&v)) = (
                inst.pin_nodes.get(&ca.from_pin),
                inst.pin_nodes.get(&ca.to_pin),
            ) else {
                continue;
            };
            if pin_vals[u] != LogicVal::X || pin_vals[v] != LogicVal::X {
                disabled_comb_arcs.insert((inst_idx, ca_idx), DisableReason::CaseAnalysis);
                continue;
            }

            if let Some(pdef) = cdef.pins.get(&ca.to_pin) {
                if let Some((can_toggle, sense_opt)) = analyze_residual_arc(
                    pdef.function.as_ref(),
                    pdef.three_state.as_ref(),
                    ca,
                    &env,
                ) {
                    if !can_toggle {
                        disabled_comb_arcs.insert((inst_idx, ca_idx), DisableReason::CaseAnalysis);
                        continue;
                    }
                    if let Some(s) = sense_opt {
                        refined_comb_sense.insert((inst_idx, ca_idx), s);
                    }
                }
            }
        }

        for (la_idx, la) in cdef.launch_arcs.iter().enumerate() {
            if disabled_launch_arcs.contains_key(&(inst_idx, la_idx)) {
                continue;
            }
            if let Some(w) = &la.when
                && eval_kleene(w, &env) == LogicVal::Zero
            {
                disabled_launch_arcs.insert((inst_idx, la_idx), DisableReason::WhenFalse);
                continue;
            }
            let (Some(&ck_n), Some(&q_n)) = (
                inst.pin_nodes.get(&la.clock_pin),
                inst.pin_nodes.get(&la.to_pin),
            ) else {
                continue;
            };
            if pin_vals[ck_n] != LogicVal::X || pin_vals[q_n] != LogicVal::X {
                disabled_launch_arcs.insert((inst_idx, la_idx), DisableReason::CaseAnalysis);
            }
        }

        for (chk_idx, chk) in cdef.check_arcs.iter().enumerate() {
            if disabled_check_arcs.contains_key(&(inst_idx, chk_idx)) {
                continue;
            }
            if let Some(w) = &chk.when
                && eval_kleene(w, &env) == LogicVal::Zero
            {
                disabled_check_arcs.insert((inst_idx, chk_idx), DisableReason::WhenFalse);
            }
        }
    }

    ConstPropResult {
        pin_vals,
        disabled_comb_arcs,
        refined_comb_sense,
        disabled_launch_arcs,
        disabled_check_arcs,
    }
}

fn is_pure_combinational(cdef: &CellDef) -> bool {
    cdef.launch_arcs.is_empty() && cdef.check_arcs.is_empty() && cdef.latches.is_empty()
}

fn analyze_residual_arc(
    func: Option<&Func>,
    three_state: Option<&Func>,
    ca: &CombArc,
    env: &dyn Fn(&str) -> LogicVal,
) -> Option<(bool, Option<TimingSense>)> {
    if ca.kind.is_three_state() {
        let ts = three_state?;
        if eval_kleene(ts, env) == LogicVal::One {
            return Some((false, None));
        }
        return None;
    }
    if let Some(ts) = three_state
        && eval_kleene(ts, env) == LogicVal::One
    {
        return Some((false, None));
    }
    let f = func?;
    let mut vars = Vec::new();
    collect_vars(f, &mut vars);
    if !vars.contains(&ca.from_pin) {
        return None;
    }
    let free_others: Vec<String> = vars
        .into_iter()
        .filter(|v| v != &ca.from_pin && env(v) == LogicVal::X)
        .collect();
    if free_others.len() > 10 {
        return None;
    }

    let mut pos = false;
    let mut neg = false;
    for mask in 0..(1usize << free_others.len()) {
        let eval_at = |from_val: bool| -> Option<bool> {
            eval_kleene(f, &|name| {
                if name == ca.from_pin {
                    LogicVal::from_bool(from_val)
                } else if let Some(idx) = free_others.iter().position(|o| o == name) {
                    LogicVal::from_bool(((mask >> idx) & 1) != 0)
                } else {
                    env(name)
                }
            })
            .to_bool()
        };
        if let (Some(v0), Some(v1)) = (eval_at(false), eval_at(true)) {
            if !v0 && v1 {
                pos = true;
            } else if v0 && !v1 {
                neg = true;
            }
        }
    }

    match (pos, neg) {
        (false, false) => Some((false, None)),
        (true, false) => Some((true, Some(TimingSense::PositiveUnate))),
        (false, true) => Some((true, Some(TimingSense::NegativeUnate))),
        (true, true) => Some((true, Some(TimingSense::NonUnate))),
    }
}

fn apply_user_disable_timing(
    design: &FlatDesign,
    lib: &CornerLib,
    sdc: &CanonSdc,
    disabled_comb: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_launch: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_check: &mut BTreeMap<(usize, usize), DisableReason>,
) {
    for rule in &sdc.disable_timing {
        if rule.is_lib_cell {
            if let Some((cell_name, pin_name)) = rule.cell_or_inst_or_port.split_once('/') {
                for (inst_idx, inst) in design.instances.iter().enumerate() {
                    if inst.cell_type == cell_name
                        && let Some(cdef) = lib.cells.get(&inst.cell_type)
                    {
                        disable_inst_pin_arcs(
                            inst_idx,
                            cdef,
                            pin_name,
                            disabled_comb,
                            disabled_launch,
                            disabled_check,
                        );
                    }
                }
            } else {
                for (inst_idx, inst) in design.instances.iter().enumerate() {
                    if inst.cell_type == rule.cell_or_inst_or_port
                        && let Some(cdef) = lib.cells.get(&inst.cell_type)
                    {
                        disable_inst_from_to_arcs(
                            inst_idx,
                            cdef,
                            rule.from_pin.as_deref(),
                            rule.to_pin.as_deref(),
                            disabled_comb,
                            disabled_launch,
                            disabled_check,
                        );
                    }
                }
            }
        } else if let Some(&inst_idx) = design.inst_map.get(&rule.cell_or_inst_or_port) {
            let inst = &design.instances[inst_idx];
            if let Some(cdef) = lib.cells.get(&inst.cell_type) {
                disable_inst_from_to_arcs(
                    inst_idx,
                    cdef,
                    rule.from_pin.as_deref(),
                    rule.to_pin.as_deref(),
                    disabled_comb,
                    disabled_launch,
                    disabled_check,
                );
            }
        } else if let Some(&node_idx) = design.node_map.get(&rule.cell_or_inst_or_port)
            && let NodeKind::InstancePin { inst_idx, ref pin } = design.nodes[node_idx].kind
        {
            let inst = &design.instances[inst_idx];
            if let Some(cdef) = lib.cells.get(&inst.cell_type) {
                disable_inst_pin_arcs(
                    inst_idx,
                    cdef,
                    pin,
                    disabled_comb,
                    disabled_launch,
                    disabled_check,
                );
            }
        }
    }
}

fn disable_inst_pin_arcs(
    inst_idx: usize,
    cdef: &CellDef,
    pin: &str,
    disabled_comb: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_launch: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_check: &mut BTreeMap<(usize, usize), DisableReason>,
) {
    for (idx, ca) in cdef.comb_arcs.iter().enumerate() {
        if ca.from_pin == pin || ca.to_pin == pin {
            disabled_comb.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
    for (idx, la) in cdef.launch_arcs.iter().enumerate() {
        if la.clock_pin == pin || la.to_pin == pin {
            disabled_launch.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
    for (idx, chk) in cdef.check_arcs.iter().enumerate() {
        if chk.data_pin == pin || chk.clock_pin == pin {
            disabled_check.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
}

fn disable_inst_from_to_arcs(
    inst_idx: usize,
    cdef: &CellDef,
    from_pin: Option<&str>,
    to_pin: Option<&str>,
    disabled_comb: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_launch: &mut BTreeMap<(usize, usize), DisableReason>,
    disabled_check: &mut BTreeMap<(usize, usize), DisableReason>,
) {
    for (idx, ca) in cdef.comb_arcs.iter().enumerate() {
        if from_pin.is_none_or(|f| ca.from_pin == f) && to_pin.is_none_or(|t| ca.to_pin == t) {
            disabled_comb.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
    for (idx, la) in cdef.launch_arcs.iter().enumerate() {
        if from_pin.is_none_or(|f| la.clock_pin == f) && to_pin.is_none_or(|t| la.to_pin == t) {
            disabled_launch.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
    for (idx, chk) in cdef.check_arcs.iter().enumerate() {
        if from_pin.is_none_or(|f| chk.clock_pin == f) && to_pin.is_none_or(|t| chk.data_pin == t) {
            disabled_check.insert((inst_idx, idx), DisableReason::UserDisabled);
        }
    }
}
