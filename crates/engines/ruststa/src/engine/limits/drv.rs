// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Electrical design rules: `max_transition`, `max_capacitance` and `max_fanout`
//! (`STA-REQ-0803` to `STA-REQ-0807`).
//!
//! The limit of a check is the smallest of the limits that apply to it: the library limit of an
//! instance pin (its own attribute, or the library default), and the constraint limits of the
//! current design, of the port, and of each clock whose domain contains the pin. A constraint
//! object holds one value per slot, and a later command on the same slot replaces an earlier one;
//! a clock has one slot per path (clock network or data) and transition. The transition check
//! covers every pin and port with a transition time; the capacitance and fanout checks cover
//! every driver of a net (an input port or an output pin), with the capacitance of its net
//! (pins, wire and port loads, the largest over transitions and bounds) and the sum of the
//! `fanout_load` of its instance load pins and of `set_fanout_load` on its output ports.

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;
use rusteda_core::liberty::Direction;
use rusteda_formats::sdc::EdgeScope;

use crate::engine::delay::load::map_netlist_cell_pins;
use crate::engine::input::constr::{DrcConstraint, DrcConstraintKind, ResolvedObj};
use crate::engine::input::libview::CellPin;
use crate::engine::input::{NetTarget, NodeKind};
use crate::engine::prop::clocks::ClockModel;
use crate::engine::prop::starts::canonical_net_totals;
use crate::engine::types::{An, ClockId, NodeId, Tr};

use super::{LimitInputs, LimitKind, LimitRow, LimitSource, RowCollector};

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
            let slots = &mut rules.kinds[kind.index()];
            for o in &dc.objects {
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
                        let paths: &[usize] = match (clock_path, data_path) {
                            (true, false) => &[0],
                            (false, true) => &[1],
                            _ => &[0, 1],
                        };
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

/// The constraint limits of `kind` at `node` for transition `tr`.
fn sdc_candidates(
    rules: &SdcRules,
    kind: LimitKind,
    node: NodeId,
    tr: Tr,
    clock_net: &[(ClockId, u8)],
    data: &[ClockId],
    out: &mut Vec<(f64, LimitSource)>,
) {
    let slots = &rules.kinds[kind.index()];
    if let Some(v) = slots.design {
        out.push((v, LimitSource::SdcDesign));
    }
    if let Some(&v) = slots.ports.get(&node) {
        out.push((v, LimitSource::SdcPort));
    }
    for (clock, s) in &slots.clocks {
        let on_clock = clock_net.iter().any(|(c, _)| c == clock);
        let path = if on_clock {
            Some(0)
        } else if data.contains(clock) {
            Some(1)
        } else {
            None
        };
        if let Some(v) = path.and_then(|p| s[p][tr.index()]) {
            out.push((v, LimitSource::SdcClock));
        }
    }
}

pub(super) fn check_design_rules(
    inputs: &LimitInputs<'_>,
    rows: &mut RowCollector,
    diags: &mut Vec<Diag>,
) {
    let rules = SdcRules::build(inputs, diags);
    let nv = inputs.netview;
    let frozen = nv.frozen();
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
        if any_clock_rule {
            data_clocks(inputs, node, &mut data);
        }
        let clock_net = inputs.clock_model.pin_clocks(node);
        let lp = lib_pin(node);

        // max_transition (`STA-REQ-0803`).
        let mut best: Option<LimitRow> = None;
        let mut has_value = false;
        for tr in Tr::ALL {
            let s = v.transition(i, tr.index(), An::Late.index());
            if !An::Late.is_present(s) {
                continue;
            }
            has_value = true;
            cands.clear();
            if let Some(l) = lp.and_then(|p| p.max_transition) {
                cands.push((l, LimitSource::Liberty));
            }
            sdc_candidates(
                &rules,
                LimitKind::MaxTransition,
                node,
                tr,
                clock_net,
                &data,
                &mut cands,
            );
            if let Some((limit, source)) = tightest(cands.iter().copied()) {
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
            None => {}
        }

        // max_capacitance and max_fanout of the net of a driver (`STA-REQ-0804`).
        if !nv.is_driver(node) {
            continue;
        }
        let NetTarget::Net(net) = nv.node_net(node) else {
            continue;
        };
        let cap = totals.get(net.index()).copied().unwrap_or_default();
        let mut best: Option<LimitRow> = None;
        for tr in Tr::ALL {
            let c = cap[tr.index()][0].max(cap[tr.index()][1]);
            cands.clear();
            if let Some(l) = lp.and_then(|p| p.max_capacitance) {
                cands.push((l, LimitSource::Liberty));
            }
            sdc_candidates(
                &rules,
                LimitKind::MaxCapacitance,
                node,
                tr,
                clock_net,
                &data,
                &mut cands,
            );
            if let Some((limit, source)) = tightest(cands.iter().copied()) {
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
            LimitKind::MaxFanout,
            node,
            Tr::Rise,
            clock_net,
            &data,
            &mut cands,
        );
        match tightest(cands.iter().copied()) {
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
}
