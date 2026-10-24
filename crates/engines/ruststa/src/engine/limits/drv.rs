// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Electrical design rules: `max_transition`, `max_capacitance` and `max_fanout`
//! (`STA-REQ-0803` to `STA-REQ-0809`, `STA-REQ-0818`, `STA-REQ-0819`).
//!
//! The limit of a check is the smallest of the limits that apply to it: the library limit of an
//! instance pin (its own attribute, or the library default), and the constraint limits of the
//! current design, of the port, and of each clock whose domain contains the pin. A constraint
//! object holds one value per slot, and a later command on the same slot replaces an earlier one;
//! a clock has one slot per path (clock network or data) and transition, mapped to the pins by
//! the clock limit scope (`STA-REQ-0808`). The transition check covers every pin and port: with
//! its propagated transition, or with the static transition of a pin on a constant net or in its
//! cone (`STA-REQ-0809`); a pin without either is untimed, counted and not evaluated. The
//! capacitance and fanout checks cover every driver of a net (an input port or an output pin)
//! whose net is not constant, with the capacitance of its net (pins, wire and port loads, the
//! largest over transitions and bounds) and the sum of the `fanout_load` of its instance load
//! pins and of `set_fanout_load` on its output ports. The pins of an ideal clock network are not
//! checked (`STA-REQ-0818`), nor are the library pins that the netlist leaves unconnected, which
//! are counted (`STA-REQ-0819`).

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;
use rusteda_core::liberty::Direction;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::EdgeScope;

use crate::engine::delay::load::map_netlist_cell_pins;
use crate::engine::input::constr::{DrcConstraint, DrcConstraintKind, ResolvedObj};
use crate::engine::input::libview::CellPin;
use crate::engine::input::{NetTarget, NodeKind};
use crate::engine::prop::clocks::ClockModel;
use crate::engine::prop::starts::canonical_net_totals;
use crate::engine::types::{An, ClockId, NodeId, Tr};

use super::statics::StaticTransition;
use super::{ClockLimitScope, LimitInputs, LimitKind, LimitRow, LimitSource, RowCollector};

/// The constraint limits of one design rule kind (`STA-REQ-0805`).
#[derive(Debug, Default)]
struct RuleSlots {
    design: Option<f64>,
    ports: FxHashMap<NodeId, f64>,
    /// `[path][tr]` slots of each clock, path 0 the clock network and 1 the data paths.
    clocks: FxHashMap<ClockId, [[Option<f64>; 2]; 2]>,
}

/// The design-rule constraints, slot by slot.
#[derive(Debug, Default)]
struct SdcRules {
    /// Indexed by [`LimitKind::index`] of the three design rules.
    kinds: [RuleSlots; 3],
    /// `set_fanout_load` of output ports.
    fanout_load: FxHashMap<NodeId, f64>,
    /// Clock commands with `-clock_path` only that the scope `domain` does not apply
    /// (`STA-REQ-0808`).
    clock_path_skipped: u64,
}

fn flag_diag(dc: &DrcConstraint, cmd: &str) -> Diag {
    let d = Diag::warning(
        "T0130",
        format!(
            "{cmd}: -clock_path, -data_path, -rise and -fall apply to clocks only; they are \
             ignored for the design and ports (STA-REQ-0805)"
        ),
    );
    with_loc(d, dc)
}

fn object_diag(dc: &DrcConstraint, cmd: &str, what: &str) -> Diag {
    let d = Diag::warning(
        "T0131",
        format!(
            "{cmd} on {what} is not a checked design rule; the object is ignored (STA-REQ-0805)"
        ),
    );
    with_loc(d, dc)
}

fn with_loc(mut d: Diag, dc: &DrcConstraint) -> Diag {
    d.location = dc.location.clone();
    d
}

fn object_kind(o: &ResolvedObj) -> &'static str {
    match o {
        ResolvedObj::Port(_) => "a port",
        ResolvedObj::Pin(_) => "a pin",
        ResolvedObj::Cell(_) => "a cell",
        ResolvedObj::Net(_) => "a net",
        ResolvedObj::Clock(_) => "a clock",
        ResolvedObj::LibCell(_) => "a library cell",
        ResolvedObj::LibPin(_) => "a library pin",
        ResolvedObj::Design(_) => "the design",
    }
}

impl SdcRules {
    fn build(inputs: &LimitInputs<'_>, diags: &mut Vec<Diag>) -> Self {
        let nv = inputs.netview;
        let port_dir = |n: NodeId| match nv.node_kind(n) {
            NodeKind::PortBit { dir, .. } => Some(dir),
            NodeKind::InstancePin { .. } => None,
        };
        let mut rules = SdcRules::default();
        for dc in &inputs.constraints.drc_constraints {
            let (kind, cmd, value, clock_path, data_path, edge) = match dc.kind {
                DrcConstraintKind::MaxTransition {
                    value_ns,
                    clock_path,
                    data_path,
                    edge,
                } => (
                    LimitKind::MaxTransition,
                    "set_max_transition",
                    value_ns,
                    clock_path,
                    data_path,
                    edge,
                ),
                DrcConstraintKind::MaxCapacitance {
                    value_pf,
                    clock_path,
                    data_path,
                    edge,
                } => (
                    LimitKind::MaxCapacitance,
                    "set_max_capacitance",
                    value_pf,
                    clock_path,
                    data_path,
                    edge,
                ),
                DrcConstraintKind::MaxFanout { value } => (
                    LimitKind::MaxFanout,
                    "set_max_fanout",
                    value,
                    false,
                    false,
                    EdgeScope::Both,
                ),
                DrcConstraintKind::FanoutLoad { value } => {
                    for o in &dc.objects {
                        match o {
                            ResolvedObj::Port(n)
                                if matches!(
                                    port_dir(*n),
                                    Some(Direction::Output | Direction::Inout)
                                ) =>
                            {
                                rules.fanout_load.insert(*n, value);
                            }
                            other => {
                                let what = match other {
                                    ResolvedObj::Port(_) => "an input port",
                                    o => object_kind(o),
                                };
                                diags.push(object_diag(dc, "set_fanout_load", what));
                            }
                        }
                    }
                    continue;
                }
                // `set_min_capacitance` and `set_port_fanout_number` stay recorded (`D0021`).
                DrcConstraintKind::MinCapacitance { .. }
                | DrcConstraintKind::PortFanoutNumber { .. } => continue,
            };
            let flags = clock_path || data_path || edge != EdgeScope::Both;
            let mut flag_warned = false;
            for o in &dc.objects {
                let slots = &mut rules.kinds[kind.index()];
                match o {
                    ResolvedObj::Design(_) | ResolvedObj::Port(_) => {
                        if flags && !flag_warned {
                            diags.push(flag_diag(dc, cmd));
                            flag_warned = true;
                        }
                        if let ResolvedObj::Port(n) = o {
                            let drives =
                                matches!(port_dir(*n), Some(Direction::Input | Direction::Inout));
                            if kind != LimitKind::MaxTransition && !drives {
                                diags.push(object_diag(
                                    dc,
                                    cmd,
                                    "an output port (it drives no net)",
                                ));
                                continue;
                            }
                            slots.ports.insert(*n, value);
                        } else {
                            slots.design = Some(value);
                        }
                    }
                    ResolvedObj::Clock(c) if kind != LimitKind::MaxFanout => {
                        // The slots of the definitions (`STA-REQ-0805`): path 0 the clock
                        // network, path 1 the data pins, both without either option. The scope
                        // `domain` reads the data-path slot on the whole domain, so a
                        // `-clock_path` limit alone applies nowhere there (`STA-REQ-0808`).
                        let paths: &[usize] = match (clock_path, data_path) {
                            (true, false) => &[0],
                            (false, true) => &[1],
                            _ => &[0, 1],
                        };
                        if paths == [0] && inputs.clock_limits == ClockLimitScope::Domain {
                            rules.clock_path_skipped += 1;
                        }
                        let trs: &[usize] = match edge {
                            EdgeScope::Rise => &[0],
                            EdgeScope::Fall => &[1],
                            EdgeScope::Both => &[0, 1],
                        };
                        let entry = slots.clocks.entry(*c).or_default();
                        for &p in paths {
                            for &t in trs {
                                entry[p][t] = Some(value);
                            }
                        }
                    }
                    other => diags.push(object_diag(dc, cmd, object_kind(other))),
                }
            }
        }
        rules
    }
}

/// The smallest limit and its source over `candidates`, ties to the earlier source.
fn tightest(
    candidates: impl IntoIterator<Item = (f64, LimitSource)>,
) -> Option<(f64, LimitSource)> {
    candidates
        .into_iter()
        .filter(|(v, _)| v.is_finite())
        .reduce(|a, b| {
            if b.0 < a.0 || (b.0 == a.0 && b.1 < a.1) {
                b
            } else {
                a
            }
        })
}

/// The clocks with a data arrival at `node` (its data-path domains, `STA-REQ-0805`).
fn data_clocks(inputs: &LimitInputs<'_>, node: NodeId, out: &mut Vec<ClockId>) {
    out.clear();
    let v = inputs.node_values;
    if node.index() >= v.nodes() {
        return;
    }
    for (tid, tag) in inputs.tags.iter() {
        if tid.index() >= v.ntags as usize || out.contains(&tag.clock) {
            continue;
        }
        let reached = Tr::ALL.iter().any(|&tr| {
            An::ALL.iter().any(|&an| {
                an.is_present(v.arrival(node.index(), tid.index(), tr.index(), an.index()))
            })
        });
        if reached {
            out.push(tag.clock);
        }
    }
}

/// The constraint limits of `kind` at `node` for transition `tr`. Under the clock limit scope
/// `domain` the data-path slot of a clock covers its clock network too (`STA-REQ-0808`).
/// Returns true when the limit depends on the scope: `node` is on the clock network of a clock
/// whose clock-path and data-path slots differ.
#[allow(clippy::too_many_arguments)]
fn sdc_candidates(
    rules: &SdcRules,
    scope: ClockLimitScope,
    kind: LimitKind,
    node: NodeId,
    tr: Tr,
    clock_net: &[(ClockId, u8)],
    data: &[ClockId],
    out: &mut Vec<(f64, LimitSource)>,
) -> bool {
    let slots = &rules.kinds[kind.index()];
    if let Some(v) = slots.design {
        out.push((v, LimitSource::SdcDesign));
    }
    if let Some(&v) = slots.ports.get(&node) {
        out.push((v, LimitSource::SdcPort));
    }
    let mut scope_dependent = false;
    for (clock, s) in &slots.clocks {
        let on_clock = clock_net.iter().any(|(c, _)| c == clock);
        scope_dependent |= on_clock && s[0][tr.index()] != s[1][tr.index()];
        let path = match scope {
            ClockLimitScope::Paths if on_clock => Some(0),
            _ if on_clock || data.contains(clock) => Some(1),
            _ => None,
        };
        if let Some(v) = path.and_then(|p| s[p][tr.index()]) {
            out.push((v, LimitSource::SdcClock));
        }
    }
    scope_dependent
}

/// The output and inout library pins of instances that the netlist leaves unconnected
/// (`STA-REQ-0819`): their number and the names (`instance/pin`) of the first `samples`.
fn unconnected_output_pins(frozen: &FrozenNetlist<'_>, samples: usize) -> (u64, Vec<String>) {
    let mut count = 0u64;
    let mut names = Vec::new();
    let mut connected = Vec::new();
    for (row, inst) in frozen.instances.iter().enumerate() {
        let cell_pins = frozen.cell_pins(inst.cell);
        connected.clear();
        connected.resize(cell_pins.len(), false);
        for p in frozen.instance_pins(row as u32) {
            if let Some(c) = connected.get_mut(frozen.pin_cellpin[p] as usize) {
                *c = true;
            }
        }
        for (cp, &is_connected) in connected.iter().enumerate() {
            let slot = cell_pins.start + cp;
            // Cell pin direction codes of the frozen netlist: 1 output, 2 inout.
            if is_connected || !matches!(frozen.cell_pin_dir.get(slot), Some(1 | 2)) {
                continue;
            }
            count += 1;
            if names.len() < samples {
                let name = |id: u32| frozen.string(id).unwrap_or("?");
                names.push(format!(
                    "{}/{}",
                    name(inst.name),
                    name(frozen.cell_pin_name[slot])
                ));
            }
        }
    }
    (count, names)
}

/// The pins of the design rules that are left out or checked by a special rule
/// (`STA-REQ-0809`, `STA-REQ-0818`, `STA-REQ-0808`, `STA-REQ-0819`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DrvCounts {
    /// Drivers of constant nets: not checked for capacitance and fanout.
    pub constant_drivers: u64,
    /// Pins checked for transition with a static transition.
    pub static_transition_pins: u64,
    /// Pins of an ideal clock network: not checked.
    pub ideal_clock_pins: u64,
    /// Pins without any transition: their transition check is not evaluated.
    pub untimed_pins: u64,
    /// Clock commands with `-clock_path` only that the scope `domain` does not apply.
    pub clock_path_skipped: u64,
    /// Checked pins of a clock network whose clock has different clock-path and data-path
    /// limits: the clock limit scope decides their limit.
    pub clock_scope_pins: u64,
    /// Output and inout pins of instances that the netlist leaves unconnected: not checked.
    pub unconnected_output_pins: u64,
    /// The names of the first of them.
    pub unconnected_samples: Vec<String>,
}

pub(super) fn check_design_rules(
    inputs: &LimitInputs<'_>,
    rows: &mut RowCollector,
    diags: &mut Vec<Diag>,
) -> DrvCounts {
    let rules = SdcRules::build(inputs, diags);
    let nv = inputs.netview;
    let frozen = nv.frozen();
    // Unconnected library pins are not pins of the design (`STA-REQ-0819`).
    let (unconnected_output_pins, unconnected_samples) = unconnected_output_pins(frozen, 5);
    let mut counts = DrvCounts {
        clock_path_skipped: rules.clock_path_skipped,
        unconnected_output_pins,
        unconnected_samples,
        ..DrvCounts::default()
    };
    let cell_pins = map_netlist_cell_pins(frozen, inputs.lib);
    let lib_pin = |node: NodeId| -> Option<&CellPin> {
        match nv.node_kind(node) {
            NodeKind::InstancePin { cell, cell_pin, .. } => cell_pins
                .get(cell as usize)
                .and_then(|v| v.get(cell_pin as usize))
                .copied()
                .flatten(),
            NodeKind::PortBit { .. } => None,
        }
    };
    let any_clock_rule = rules.kinds.iter().any(|k| !k.clocks.is_empty());
    let totals = canonical_net_totals(nv, inputs.net_loads);
    let v = inputs.node_values;
    let mut data = Vec::new();
    let mut cands = Vec::new();

    for i in 0..nv.num_nodes() {
        let node = NodeId(i as u32);
        let clock_net = inputs.clock_model.pin_clocks(node);
        // The design rules of an ideal clock network are not checked; its limited checks are
        // unevaluated (`STA-REQ-0818`).
        let ideal = !clock_net.is_empty()
            && clock_net
                .iter()
                .all(|(c, _)| !inputs.clock_model.is_propagated(*c));
        if ideal {
            counts.ideal_clock_pins += 1;
        }
        if any_clock_rule {
            data_clocks(inputs, node, &mut data);
        }
        let lp = lib_pin(node);

        // max_transition (`STA-REQ-0803`): the propagated transition, else the static one of a
        // pin on a constant net or in its cone (`STA-REQ-0809`).
        let propagated = [
            v.transition(i, Tr::Rise.index(), An::Late.index()),
            v.transition(i, Tr::Fall.index(), An::Late.index()),
        ];
        let tr_values = if ideal {
            None
        } else if propagated.iter().any(|&s| An::Late.is_present(s)) {
            Some(propagated)
        } else {
            match inputs.statics.get(node) {
                Some(StaticTransition::Computed(s)) => {
                    counts.static_transition_pins += 1;
                    Some(s)
                }
                Some(StaticTransition::Constant) => {
                    counts.static_transition_pins += 1;
                    Some([0.0, 0.0])
                }
                None => None,
            }
        };
        let mut best: Option<LimitRow> = None;
        let mut has_value = false;
        let mut has_limit = false;
        // An evaluated check of this pin whose limit depends on the clock limit scope
        // (`STA-REQ-0808`).
        let mut scope_dependent = false;
        for tr in Tr::ALL {
            cands.clear();
            if let Some(l) = lp.and_then(|p| p.max_transition) {
                cands.push((l, LimitSource::Liberty));
            }
            let dependent = sdc_candidates(
                &rules,
                inputs.clock_limits,
                LimitKind::MaxTransition,
                node,
                tr,
                clock_net,
                &data,
                &mut cands,
            );
            let limit = tightest(cands.iter().copied());
            has_limit |= limit.is_some();
            let Some(s) = tr_values
                .map(|x| x[tr.index()])
                .filter(|&s| An::Late.is_present(s))
            else {
                continue;
            };
            has_value = true;
            scope_dependent |= dependent;
            if let Some((limit, source)) = limit {
                let row = LimitRow {
                    node,
                    kind: LimitKind::MaxTransition,
                    tr: Some(tr),
                    clock: None,
                    value: s,
                    limit,
                    slack: limit - s,
                    source,
                };
                if best.is_none_or(|b| row.slack < b.slack) {
                    best = Some(row);
                }
            }
        }
        match best {
            Some(row) => rows.push(row),
            None if has_value => rows.unlimited(LimitKind::MaxTransition),
            None if tr_values.is_none() => {
                // A pin of an ideal clock network, or an untimed pin: counted, and not evaluated
                // where a limit applies.
                if !ideal {
                    counts.untimed_pins += 1;
                }
                if has_limit {
                    rows.unevaluated(LimitKind::MaxTransition);
                }
            }
            None => {}
        }
        if scope_dependent {
            counts.clock_scope_pins += 1;
        }

        // max_capacitance and max_fanout of the net of a driver (`STA-REQ-0804`), except on a
        // constant net (`STA-REQ-0809`).
        if !nv.is_driver(node) {
            continue;
        }
        let NetTarget::Net(net) = nv.node_net(node) else {
            continue;
        };
        if inputs.constant.get(i).copied().unwrap_or(false) {
            counts.constant_drivers += 1;
            continue;
        }
        let cap = totals.get(net.index()).copied().unwrap_or_default();
        let mut best: Option<LimitRow> = None;
        let mut cap_limited = false;
        for tr in Tr::ALL {
            let c = cap[tr.index()][0].max(cap[tr.index()][1]);
            cands.clear();
            if let Some(l) = lp.and_then(|p| p.max_capacitance) {
                cands.push((l, LimitSource::Liberty));
            }
            let dependent = sdc_candidates(
                &rules,
                inputs.clock_limits,
                LimitKind::MaxCapacitance,
                node,
                tr,
                clock_net,
                &data,
                &mut cands,
            );
            if dependent && !ideal && !scope_dependent {
                scope_dependent = true;
                counts.clock_scope_pins += 1;
            }
            if let Some((limit, source)) = tightest(cands.iter().copied()) {
                cap_limited = true;
                if ideal {
                    continue;
                }
                let row = LimitRow {
                    node,
                    kind: LimitKind::MaxCapacitance,
                    tr: Some(tr),
                    clock: None,
                    value: c,
                    limit,
                    slack: limit - c,
                    source,
                };
                if best.is_none_or(|b| row.slack < b.slack) {
                    best = Some(row);
                }
            }
        }
        match best {
            Some(row) => rows.push(row),
            None if ideal && cap_limited => rows.unevaluated(LimitKind::MaxCapacitance),
            None if ideal => {}
            None => rows.unlimited(LimitKind::MaxCapacitance),
        }

        let mut fanout = 0.0f64;
        for &load in nv.loads(net) {
            if load == node {
                continue;
            }
            fanout += match nv.node_kind(load) {
                NodeKind::InstancePin { .. } => lib_pin(load).map_or(0.0, |p| p.fanout_load),
                NodeKind::PortBit { .. } => rules.fanout_load.get(&load).copied().unwrap_or(0.0),
            };
        }
        cands.clear();
        if let Some(l) = lp.and_then(|p| p.max_fanout) {
            cands.push((l, LimitSource::Liberty));
        }
        sdc_candidates(
            &rules,
            inputs.clock_limits,
            LimitKind::MaxFanout,
            node,
            Tr::Rise,
            clock_net,
            &data,
            &mut cands,
        );
        match tightest(cands.iter().copied()) {
            Some(_) if ideal => rows.unevaluated(LimitKind::MaxFanout),
            None if ideal => {}
            Some((limit, source)) => rows.push(LimitRow {
                node,
                kind: LimitKind::MaxFanout,
                tr: None,
                clock: None,
                value: fanout,
                limit,
                slack: limit - fanout,
                source,
            }),
            None => rows.unlimited(LimitKind::MaxFanout),
        }
    }
    counts
}
