// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Evaluation of symbolic SDC object queries (`Obj`, `ObjQuery`, `-of_objects`, `-filter`,
//! `all_*`, `current_design`) against [`DesignIndex`] and defined [`CompiledClock`]s
//! (`SDC-REQ-0005`, `SDC-REQ-0006`, `STA-REQ-0020`).

use std::collections::BTreeSet;

use rusteda_core::diag::{Diag, Location};
use rusteda_formats::sdc::{Filter, FilterAtom, FilterOp, Obj, ObjQuery};

use super::index::{
    CellInstInfo, DesignIndex, LibCellInfo, LibPinInfo, NetInfo, PinInfo, PortBitInfo,
};
use super::model::{CompiledClock, ResolvedObj};
use super::pattern::{glob_match, matches_pattern, regex_match};
use crate::engine::types::NodeId;

/// Context hint for bare [`Obj::Names`] resolution when no explicit `get_*` command wraps a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameLookupHint {
    /// Resolve bare names against defined clocks (`-clock`, `set_clock_transition`, etc.).
    Clocks,
    /// Resolve bare names against top-level ports (`set_input_transition`, `set_driving_cell`, etc.).
    Ports,
    /// Resolve bare names against top-level ports or instance pins (`create_clock`, `set_input_delay`, etc.).
    PortsOrPins,
    /// Resolve bare names against top-level ports or nets (`set_load`).
    PortsOrNets,
    /// Resolve bare names against cell instances (`set_size_only`).
    Cells,
    /// Resolve bare names against library cells (`set_dont_use`).
    LibCells,
    /// Resolve bare names against clocks, ports, pins, or cells (`-from`, `-to`, `set_max_time_borrow`, etc.).
    AnyEndpoint,
    /// Resolve bare names against any design or library object.
    Any,
}

/// Query evaluation parameters for a single SDC command.
#[derive(Debug)]
pub struct QueryContext<'a> {
    pub index: &'a DesignIndex,
    pub clocks: &'a [CompiledClock],
    pub hint: NameLookupHint,
    /// When `true` (for `set_input_delay`, `set_input_transition`, and `set_driving_cell`),
    /// `[all_inputs]` excludes already-defined clock source ports even without `-no_clocks`.
    pub exclude_clocks_on_all_inputs: bool,
    pub location: Option<Location>,
}

impl<'a> QueryContext<'a> {
    fn sub(&self, hint: NameLookupHint) -> Self {
        Self {
            index: self.index,
            clocks: self.clocks,
            hint,
            exclude_clocks_on_all_inputs: false,
            location: self.location.clone(),
        }
    }
}

/// Evaluates `obj` in `ctx`, appending any `D0101` diagnostics to `diags`.
pub fn resolve_obj(obj: &Obj, ctx: &QueryContext<'_>, diags: &mut Vec<Diag>) -> Vec<ResolvedObj> {
    match obj {
        Obj::Ports(q) => resolve_ports_query(q, ctx, diags),
        Obj::Pins(q) => resolve_pins_query(q, ctx, diags),
        Obj::Cells(q) => resolve_cells_query(q, ctx, diags),
        Obj::Nets(q) => resolve_nets_query(q, ctx, diags),
        Obj::Clocks(q) => resolve_clocks_query(q, ctx, diags),
        Obj::LibCells(q) => resolve_lib_cells_query(q, ctx, diags),
        Obj::LibPins(q) => resolve_lib_pins_query(q, ctx, diags),
        Obj::AllInputs { no_clocks } => {
            let filter_clocks = *no_clocks || ctx.exclude_clocks_on_all_inputs;
            let clk_srcs: BTreeSet<NodeId> = if filter_clocks {
                ctx.clocks
                    .iter()
                    .flat_map(|c| c.sources.iter().copied())
                    .collect()
            } else {
                BTreeSet::new()
            };
            ctx.index
                .port_bits
                .iter()
                .filter(|pb| pb.is_input() && !clk_srcs.contains(&pb.node))
                .map(|pb| ResolvedObj::Port(pb.node))
                .collect()
        }
        Obj::AllOutputs => ctx
            .index
            .port_bits
            .iter()
            .filter(|pb| pb.is_output())
            .map(|pb| ResolvedObj::Port(pb.node))
            .collect(),
        Obj::AllClocks => ctx
            .clocks
            .iter()
            .map(|c| ResolvedObj::Clock(c.id))
            .collect(),
        Obj::AllRegisters => ctx
            .index
            .instances
            .iter()
            .filter(|inst| inst.is_sequential)
            .map(|inst| ResolvedObj::Cell(inst.inst))
            .collect(),
        Obj::CurrentDesign => vec![ResolvedObj::Design(ctx.index.top_name.clone())],
        Obj::Names(names) => resolve_bare_names(names, ctx, diags),
        Obj::Union(items) => {
            let mut out = Vec::new();
            let mut seen = BTreeSet::new();
            for item in items {
                for r in resolve_obj(item, ctx, diags) {
                    if seen.insert(r.clone()) {
                        out.push(r);
                    }
                }
            }
            out
        }
        Obj::Diff(lhs, rhs) => {
            let left = resolve_obj(lhs, ctx, diags);
            let right_ctx = QueryContext {
                index: ctx.index,
                clocks: ctx.clocks,
                hint: ctx.hint,
                exclude_clocks_on_all_inputs: false,
                location: ctx.location.clone(),
            };
            let right: BTreeSet<ResolvedObj> = resolve_obj_quiet_bare(rhs, &right_ctx, diags)
                .into_iter()
                .collect();
            left.into_iter().filter(|x| !right.contains(x)).collect()
        }
    }
}

/// Resolves `obj`, treating bare `Obj::Names` on the RHS of a `Diff` (such as `lreplace` /
/// `remove_from_collection` with a port/pin/clock name) against the LHS's universe without
/// emitting a spurious `D0101` if a subtracted name wasn't present.
fn resolve_obj_quiet_bare(
    obj: &Obj,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    if let Obj::Names(names) = obj {
        let mut local_diags = Vec::new();
        let any_ctx = QueryContext {
            index: ctx.index,
            clocks: ctx.clocks,
            hint: NameLookupHint::Any,
            exclude_clocks_on_all_inputs: false,
            location: ctx.location.clone(),
        };
        resolve_bare_names(names, &any_ctx, &mut local_diags)
    } else {
        resolve_obj(obj, ctx, diags)
    }
}

fn emit_d0101(
    diags: &mut Vec<Diag>,
    loc: &Option<Location>,
    quiet: bool,
    query_name: &str,
    detail: &str,
) {
    let msg = format!("{query_name}: no object matched {detail}");
    let mut d = if quiet {
        Diag::warning("D0101", msg)
    } else {
        Diag::error("D0101", msg)
    };
    if let Some(l) = loc {
        d = d.at(l.file.clone(), l.line, l.column);
    }
    diags.push(d);
}

fn resolve_ports_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let clk_srcs: BTreeSet<NodeId> = ctx
        .clocks
        .iter()
        .flat_map(|c| c.sources.iter().copied())
        .collect();

    let candidates: Vec<&PortBitInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::Any);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut allowed: BTreeSet<NodeId> = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Net(nr) => {
                    for &pb in ctx.index.net_port_bits(nr) {
                        allowed.insert(pb);
                    }
                }
                ResolvedObj::Clock(cid) => {
                    if let Some(ck) = ctx.clocks.get(cid.0 as usize) {
                        for &src in &ck.sources {
                            if src.0 < ctx.index.n_port_bits {
                                allowed.insert(src);
                            }
                        }
                    }
                }
                ResolvedObj::Port(nid) => {
                    allowed.insert(nid);
                }
                _ => {}
            }
        }
        ctx.index
            .port_bits
            .iter()
            .filter(|pb| allowed.contains(&pb.node))
            .collect()
    } else {
        ctx.index.port_bits.iter().collect()
    };

    let filtered: Vec<&PortBitInfo> = candidates
        .into_iter()
        .filter(|pb| {
            q.filter.as_ref().is_none_or(|f| {
                eval_filter(f, q.regexp, q.nocase, &|attr| {
                    port_attr(pb, clk_srcs.contains(&pb.node), attr)
                })
            })
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for pb in filtered {
            if seen.insert(pb.node) {
                out.push(ResolvedObj::Port(pb.node));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_ports", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for pb in &filtered {
                if matches_pattern(pat, &pb.full_name, q.regexp, q.nocase, true)
                    || matches_pattern(pat, &pb.base_name, q.regexp, q.nocase, true)
                {
                    matched_any = true;
                    if seen.insert(pb.node) {
                        out.push(ResolvedObj::Port(pb.node));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_ports",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_pins_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&PinInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::Any);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut allowed_pin_indices = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Cell(inst_id) => {
                    let inst_info = &ctx.index.instances[inst_id as usize];
                    for &pi in &inst_info.pin_indices {
                        allowed_pin_indices.insert(pi);
                    }
                    if q.hierarchical {
                        let prefix = format!("{}/", inst_info.full_name);
                        for other_inst in &ctx.index.instances {
                            if other_inst.full_name.starts_with(&prefix) {
                                for &pi in &other_inst.pin_indices {
                                    allowed_pin_indices.insert(pi);
                                }
                            }
                        }
                    }
                }
                ResolvedObj::Net(nr) => {
                    for &pi in ctx.index.net_pins(nr) {
                        allowed_pin_indices.insert(pi);
                    }
                }
                ResolvedObj::Pin(pr) => {
                    for (pi, pin_info) in ctx.index.all_pins.iter().enumerate() {
                        if pin_info.pin_ref == pr {
                            allowed_pin_indices.insert(pi);
                        }
                    }
                }
                _ => {}
            }
        }
        allowed_pin_indices
            .into_iter()
            .map(|pi| &ctx.index.all_pins[pi])
            .collect()
    } else {
        ctx.index.all_pins.iter().collect()
    };

    let filtered: Vec<&PinInfo> = candidates
        .into_iter()
        .filter(|pin| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| pin_attr(pin, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for pin in filtered {
            if seen.insert(pin.pin_ref.clone()) {
                out.push(ResolvedObj::Pin(pin.pin_ref.clone()));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_pins", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for pin in &filtered {
                if pin_matches_pattern(pat, pin, q.regexp, q.nocase, q.hierarchical) {
                    matched_any = true;
                    if seen.insert(pin.pin_ref.clone()) {
                        out.push(ResolvedObj::Pin(pin.pin_ref.clone()));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_pins",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn pin_matches_pattern(
    pat: &str,
    pin: &PinInfo,
    regexp: bool,
    nocase: bool,
    hierarchical: bool,
) -> bool {
    if regexp {
        return regex_match(pat, &pin.pin_ref.full_name, nocase)
            || (hierarchical && regex_match(pat, &pin.pin_name, nocase));
    }
    if hierarchical {
        if glob_match(pat, &pin.pin_ref.full_name, nocase, true)
            || glob_match(pat, &pin.pin_name, nocase, true)
        {
            return true;
        }
        let leaf_inst = pin.inst_name.rsplit('/').next().unwrap_or(&pin.inst_name);
        let leaf_pin = format!("{leaf_inst}/{}", pin.pin_name);
        glob_match(pat, &leaf_pin, nocase, true)
    } else if pat.contains('/') {
        // Each segment of `pat` matches without `*` crossing `/` unless the pattern has matching `/`
        glob_match(pat, &pin.pin_ref.full_name, nocase, false)
            || glob_match(pat, &pin.pin_ref.full_name, nocase, true)
    } else {
        // Pattern without `/` and without `-hierarchical`: matches only if full_name has no `/`
        // (which pins never do since they are `inst/pin`), OR if `pat` is `*` (matching top-level
        // instance pins where `inst_name` has no `/`).
        !pin.inst_name.contains('/') && glob_match(pat, &pin.pin_ref.full_name, nocase, true)
    }
}

fn resolve_cells_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&CellInstInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::Any);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut allowed_insts = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Pin(pr) => {
                    allowed_insts.insert(pr.inst);
                }
                ResolvedObj::Net(nr) => {
                    for &pi in ctx.index.net_pins(nr) {
                        allowed_insts.insert(ctx.index.all_pins[pi].pin_ref.inst);
                    }
                }
                ResolvedObj::Cell(inst_id) => {
                    allowed_insts.insert(inst_id);
                }
                _ => {}
            }
        }
        allowed_insts
            .into_iter()
            .map(|id| &ctx.index.instances[id as usize])
            .collect()
    } else {
        ctx.index.instances.iter().collect()
    };

    let filtered: Vec<&CellInstInfo> = candidates
        .into_iter()
        .filter(|inst| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| cell_attr(inst, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for inst in filtered {
            if seen.insert(inst.inst) {
                out.push(ResolvedObj::Cell(inst.inst));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_cells", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for inst in &filtered {
                let hit = if q.regexp {
                    regex_match(pat, &inst.full_name, q.nocase)
                        || (q.hierarchical && regex_match(pat, &inst.base_name, q.nocase))
                } else if q.hierarchical {
                    glob_match(pat, &inst.full_name, q.nocase, true)
                        || glob_match(pat, &inst.base_name, q.nocase, true)
                } else {
                    glob_match(pat, &inst.full_name, q.nocase, false)
                };
                if hit {
                    matched_any = true;
                    if seen.insert(inst.inst) {
                        out.push(ResolvedObj::Cell(inst.inst));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_cells",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_nets_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&NetInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::Any);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut allowed_nets = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Port(nid) => {
                    if nid.0 < ctx.index.n_port_bits {
                        allowed_nets.insert(ctx.index.port_bits[nid.0 as usize].net);
                    }
                }
                ResolvedObj::Pin(pr) => {
                    let inst_info = &ctx.index.instances[pr.inst as usize];
                    for &pi in &inst_info.pin_indices {
                        let pin = &ctx.index.all_pins[pi];
                        if pin.pin_ref == pr
                            && let Some(nr) = pin.net
                        {
                            allowed_nets.insert(nr);
                        }
                    }
                }
                ResolvedObj::Cell(inst_id) => {
                    let inst_info = &ctx.index.instances[inst_id as usize];
                    for &pi in &inst_info.pin_indices {
                        if let Some(nr) = ctx.index.all_pins[pi].net {
                            allowed_nets.insert(nr);
                        }
                    }
                }
                ResolvedObj::Net(nr) => {
                    allowed_nets.insert(nr);
                }
                _ => {}
            }
        }
        allowed_nets
            .into_iter()
            .map(|nr| &ctx.index.nets[nr.0 as usize])
            .collect()
    } else {
        ctx.index.nets.iter().collect()
    };

    let filtered: Vec<&NetInfo> = candidates
        .into_iter()
        .filter(|net| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| net_attr(net, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for net in filtered {
            if seen.insert(net.row) {
                out.push(ResolvedObj::Net(net.row));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_nets", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for net in &filtered {
                let net_bus_base = net.full_name.split('[').next().unwrap_or(&net.full_name);
                let hit = if q.regexp {
                    regex_match(pat, &net.full_name, q.nocase)
                        || (q.hierarchical && regex_match(pat, &net.base_name, q.nocase))
                } else if q.hierarchical {
                    glob_match(pat, &net.full_name, q.nocase, true)
                        || glob_match(pat, &net.base_name, q.nocase, true)
                        || glob_match(pat, net_bus_base, q.nocase, true)
                } else {
                    glob_match(pat, &net.full_name, q.nocase, false)
                        || glob_match(pat, net_bus_base, q.nocase, false)
                };
                if hit {
                    matched_any = true;
                    if seen.insert(net.row) {
                        out.push(ResolvedObj::Net(net.row));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_nets",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_clocks_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&CompiledClock> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::PortsOrPins);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut src_nodes = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Port(nid) => {
                    src_nodes.insert(nid);
                }
                ResolvedObj::Pin(pr) => {
                    if let Some(nid) = pr.node {
                        src_nodes.insert(nid);
                    }
                }
                _ => {}
            }
        }
        ctx.clocks
            .iter()
            .filter(|ck| ck.sources.iter().any(|s| src_nodes.contains(s)))
            .collect()
    } else {
        ctx.clocks.iter().collect()
    };

    let filtered: Vec<&CompiledClock> = candidates
        .into_iter()
        .filter(|ck| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| clock_attr(ck, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for ck in filtered {
            if seen.insert(ck.id) {
                out.push(ResolvedObj::Clock(ck.id));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_clocks", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for ck in &filtered {
                if matches_pattern(pat, &ck.name, q.regexp, q.nocase, true) {
                    matched_any = true;
                    if seen.insert(ck.id) {
                        out.push(ResolvedObj::Clock(ck.id));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_clocks",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_lib_cells_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&LibCellInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::Any);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut want_cells = BTreeSet::new();
        for p in parents {
            match p {
                ResolvedObj::Cell(inst_id) => {
                    want_cells.insert(ctx.index.instances[inst_id as usize].cell_name.to_string());
                }
                ResolvedObj::LibPin(lp) => {
                    want_cells.insert(lp.cell);
                }
                ResolvedObj::LibCell(lc) => {
                    want_cells.insert(lc.cell);
                }
                _ => {}
            }
        }
        ctx.index
            .lib_cells
            .iter()
            .filter(|lc| want_cells.contains(&lc.lib_cell.cell))
            .collect()
    } else {
        ctx.index.lib_cells.iter().collect()
    };

    let filtered: Vec<&LibCellInfo> = candidates
        .into_iter()
        .filter(|lc| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| lib_cell_attr(lc, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for lc in filtered {
            if seen.insert(lc.lib_cell.clone()) {
                out.push(ResolvedObj::LibCell(lc.lib_cell.clone()));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_lib_cells", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for lc in &filtered {
                let full = lc.lib_cell.full_name();
                if matches_pattern(pat, &full, q.regexp, q.nocase, true)
                    || matches_pattern(pat, &lc.lib_cell.cell, q.regexp, q.nocase, true)
                {
                    matched_any = true;
                    if seen.insert(lc.lib_cell.clone()) {
                        out.push(ResolvedObj::LibCell(lc.lib_cell.clone()));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_lib_cells",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_lib_pins_query(
    q: &ObjQuery,
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let candidates: Vec<&LibPinInfo> = if let Some(of_obj) = &q.of_objects {
        let sub_ctx = ctx.sub(NameLookupHint::LibCells);
        let parents = resolve_obj(of_obj, &sub_ctx, diags);
        let mut want_cells = BTreeSet::new();
        for p in parents {
            if let ResolvedObj::LibCell(lc) = p {
                want_cells.insert((lc.library, lc.cell));
            }
        }
        ctx.index
            .lib_pins
            .iter()
            .filter(|lp| {
                want_cells.contains(&(lp.lib_pin.library.clone(), lp.lib_pin.cell.clone()))
            })
            .collect()
    } else {
        ctx.index.lib_pins.iter().collect()
    };

    let filtered: Vec<&LibPinInfo> = candidates
        .into_iter()
        .filter(|lp| {
            q.filter
                .as_ref()
                .is_none_or(|f| eval_filter(f, q.regexp, q.nocase, &|attr| lib_pin_attr(lp, attr)))
        })
        .collect();

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if q.patterns.is_empty() {
        for lp in filtered {
            if seen.insert(lp.lib_pin.clone()) {
                out.push(ResolvedObj::LibPin(lp.lib_pin.clone()));
            }
        }
        if out.is_empty() {
            emit_d0101(diags, &ctx.location, q.quiet, "get_lib_pins", "query");
        }
    } else {
        for pat in &q.patterns {
            let mut matched_any = false;
            for lp in &filtered {
                let full = lp.lib_pin.full_name();
                let cell_pin = format!("{}/{}", lp.lib_pin.cell, lp.lib_pin.pin);
                if matches_pattern(pat, &full, q.regexp, q.nocase, true)
                    || matches_pattern(pat, &cell_pin, q.regexp, q.nocase, true)
                {
                    matched_any = true;
                    if seen.insert(lp.lib_pin.clone()) {
                        out.push(ResolvedObj::LibPin(lp.lib_pin.clone()));
                    }
                }
            }
            if !matched_any {
                emit_d0101(
                    diags,
                    &ctx.location,
                    q.quiet,
                    "get_lib_pins",
                    &format!("'{pat}'"),
                );
            }
        }
    }
    out
}

fn resolve_bare_names(
    names: &[String],
    ctx: &QueryContext<'_>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let q = ObjQuery::from_patterns(names.to_vec());
    match ctx.hint {
        NameLookupHint::Clocks => resolve_clocks_query(&q, ctx, diags),
        NameLookupHint::Ports => resolve_ports_query(&q, ctx, diags),
        NameLookupHint::Cells => resolve_cells_query(&q, ctx, diags),
        NameLookupHint::LibCells => resolve_lib_cells_query(&q, ctx, diags),
        NameLookupHint::PortsOrPins => {
            let mut out = Vec::new();
            let mut seen = BTreeSet::new();
            for pat in names {
                let mut local = Vec::new();
                for pb in &ctx.index.port_bits {
                    if glob_match(pat, &pb.full_name, false, true)
                        || glob_match(pat, &pb.base_name, false, true)
                    {
                        local.push(ResolvedObj::Port(pb.node));
                    }
                }
                for pin in &ctx.index.all_pins {
                    if glob_match(pat, &pin.pin_ref.full_name, false, true) {
                        local.push(ResolvedObj::Pin(pin.pin_ref.clone()));
                    }
                }
                if local.is_empty() {
                    emit_d0101(diags, &ctx.location, false, "names", &format!("'{pat}'"));
                } else {
                    for item in local {
                        if seen.insert(item.clone()) {
                            out.push(item);
                        }
                    }
                }
            }
            out
        }
        NameLookupHint::PortsOrNets => {
            let mut out = Vec::new();
            let mut seen = BTreeSet::new();
            for pat in names {
                let mut local = Vec::new();
                for pb in &ctx.index.port_bits {
                    if glob_match(pat, &pb.full_name, false, true)
                        || glob_match(pat, &pb.base_name, false, true)
                    {
                        local.push(ResolvedObj::Port(pb.node));
                    }
                }
                if local.is_empty() {
                    for net in &ctx.index.nets {
                        let base = net.full_name.split('[').next().unwrap_or(&net.full_name);
                        if glob_match(pat, &net.full_name, false, true)
                            || glob_match(pat, base, false, true)
                        {
                            local.push(ResolvedObj::Net(net.row));
                        }
                    }
                }
                if local.is_empty() {
                    emit_d0101(diags, &ctx.location, false, "names", &format!("'{pat}'"));
                } else {
                    for item in local {
                        if seen.insert(item.clone()) {
                            out.push(item);
                        }
                    }
                }
            }
            out
        }
        NameLookupHint::AnyEndpoint | NameLookupHint::Any => {
            let mut out = Vec::new();
            let mut seen = BTreeSet::new();
            for pat in names {
                let mut local = Vec::new();
                for ck in ctx.clocks {
                    if glob_match(pat, &ck.name, false, true) {
                        local.push(ResolvedObj::Clock(ck.id));
                    }
                }
                for pb in &ctx.index.port_bits {
                    if glob_match(pat, &pb.full_name, false, true)
                        || glob_match(pat, &pb.base_name, false, true)
                    {
                        local.push(ResolvedObj::Port(pb.node));
                    }
                }
                for pin in &ctx.index.all_pins {
                    if glob_match(pat, &pin.pin_ref.full_name, false, true) {
                        local.push(ResolvedObj::Pin(pin.pin_ref.clone()));
                    }
                }
                for inst in &ctx.index.instances {
                    if glob_match(pat, &inst.full_name, false, true) {
                        local.push(ResolvedObj::Cell(inst.inst));
                    }
                }
                if ctx.hint == NameLookupHint::Any && local.is_empty() {
                    for net in &ctx.index.nets {
                        if glob_match(pat, &net.full_name, false, true) {
                            local.push(ResolvedObj::Net(net.row));
                        }
                    }
                    for lc in &ctx.index.lib_cells {
                        if glob_match(pat, &lc.lib_cell.full_name(), false, true)
                            || glob_match(pat, &lc.lib_cell.cell, false, true)
                        {
                            local.push(ResolvedObj::LibCell(lc.lib_cell.clone()));
                        }
                    }
                }
                if local.is_empty() {
                    emit_d0101(diags, &ctx.location, false, "names", &format!("'{pat}'"));
                } else {
                    for item in local {
                        if seen.insert(item.clone()) {
                            out.push(item);
                        }
                    }
                }
            }
            out
        }
    }
}

fn eval_filter(
    filter: &Filter,
    regexp: bool,
    nocase: bool,
    get_attr: &dyn Fn(&str) -> Option<String>,
) -> bool {
    match filter {
        Filter::Atom(atom) => eval_filter_atom(atom, regexp, nocase, get_attr),
        Filter::And(a, b) => {
            eval_filter(a, regexp, nocase, get_attr) && eval_filter(b, regexp, nocase, get_attr)
        }
        Filter::Or(a, b) => {
            eval_filter(a, regexp, nocase, get_attr) || eval_filter(b, regexp, nocase, get_attr)
        }
    }
}

fn eval_filter_atom(
    atom: &FilterAtom,
    regexp: bool,
    nocase: bool,
    get_attr: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let Some(actual) = get_attr(&atom.attr) else {
        return false;
    };
    let want = normalize_attr_val(&atom.attr, &atom.value);
    let act = normalize_attr_val(&atom.attr, &actual);
    match atom.op {
        FilterOp::Eq => {
            if nocase {
                act.eq_ignore_ascii_case(&want)
            } else {
                act == want
            }
        }
        FilterOp::Ne => {
            if nocase {
                !act.eq_ignore_ascii_case(&want)
            } else {
                act != want
            }
        }
        FilterOp::Match => {
            if regexp {
                regex_match(&want, &act, nocase)
            } else {
                glob_match(&want, &act, nocase, true)
            }
        }
        FilterOp::NotMatch => {
            if regexp {
                !regex_match(&want, &act, nocase)
            } else {
                !glob_match(&want, &act, nocase, true)
            }
        }
    }
}

fn normalize_attr_val(attr: &str, val: &str) -> String {
    if matches!(attr, "direction" | "pin_direction" | "port_direction") {
        match val.to_ascii_lowercase().as_str() {
            "in" | "input" => "in".to_string(),
            "out" | "output" => "out".to_string(),
            "inout" => "inout".to_string(),
            "internal" => "internal".to_string(),
            other => other.to_string(),
        }
    } else if attr.starts_with("is_") || matches!(attr, "dont_use" | "dont_touch") {
        match val.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => "true".to_string(),
            "0" | "false" | "no" => "false".to_string(),
            other => other.to_string(),
        }
    } else {
        val.to_string()
    }
}

fn port_attr(pb: &PortBitInfo, is_clk: bool, attr: &str) -> Option<String> {
    match attr {
        "direction" | "port_direction" => Some(pb.dir_str().to_string()),
        "full_name" | "name" => Some(pb.full_name.clone()),
        "base_name" => Some(pb.base_name.clone()),
        "is_clock" | "is_clock_source" => Some(is_clk.to_string()),
        "is_hierarchical" => Some("false".to_string()),
        _ => None,
    }
}

fn pin_attr(pin: &PinInfo, attr: &str) -> Option<String> {
    match attr {
        "direction" | "pin_direction" => Some(pin.dir_str().to_string()),
        "full_name" => Some(pin.pin_ref.full_name.clone()),
        "name" | "lib_pin_name" => Some(pin.pin_name.to_string()),
        "ref_name" | "cell_name" | "lib_cell" => Some(pin.cell_name.to_string()),
        "is_hierarchical" => Some("false".to_string()),
        "is_clock" | "is_clock_pin" => Some(pin.is_clock_pin.to_string()),
        "is_data" | "is_data_pin" => Some(pin.is_data_pin.to_string()),
        "is_async" | "is_async_pin" => Some(pin.is_async_pin.to_string()),
        "is_three_state" | "is_tristate" => Some(pin.is_tristate_pin.to_string()),
        _ => None,
    }
}

fn cell_attr(inst: &CellInstInfo, attr: &str) -> Option<String> {
    match attr {
        "full_name" => Some(inst.full_name.to_string()),
        "name" | "base_name" => Some(inst.base_name.clone()),
        "ref_name" | "lib_cell" | "cell_name" => Some(inst.cell_name.to_string()),
        "is_hierarchical" => Some("false".to_string()),
        "is_sequential" => Some(inst.is_sequential.to_string()),
        "is_flip_flop" | "is_edge_triggered" => Some(inst.is_flip_flop.to_string()),
        "is_latch" | "is_level_sensitive" => Some(inst.is_latch.to_string()),
        "is_clock_gating" | "is_integrated_clock_gating_cell" => {
            Some(inst.is_clock_gating.to_string())
        }
        "is_combinational" => Some((!inst.is_sequential).to_string()),
        "is_tristate" | "is_three_state" => Some(inst.is_tristate.to_string()),
        _ => None,
    }
}

fn net_attr(net: &NetInfo, attr: &str) -> Option<String> {
    match attr {
        "full_name" => Some(net.full_name.clone()),
        "name" | "base_name" => Some(net.base_name.clone()),
        "is_hierarchical" => Some("false".to_string()),
        _ => None,
    }
}

fn clock_attr(ck: &CompiledClock, attr: &str) -> Option<String> {
    match attr {
        "full_name" | "name" => Some(ck.name.clone()),
        "period" => Some(format!("{}", ck.period_ns)),
        "is_virtual" => Some(ck.is_virtual().to_string()),
        "is_generated" => Some("false".to_string()),
        "is_propagated" => Some(ck.propagated.to_string()),
        _ => None,
    }
}

fn lib_cell_attr(lc: &LibCellInfo, attr: &str) -> Option<String> {
    match attr {
        "full_name" => Some(lc.lib_cell.full_name()),
        "name" | "base_name" | "ref_name" => Some(lc.lib_cell.cell.clone()),
        "library" | "lib_name" => Some(lc.lib_cell.library.clone()),
        "dont_use" => Some(lc.dont_use.to_string()),
        "dont_touch" => Some(lc.dont_touch.to_string()),
        "is_sequential" => Some(lc.is_sequential.to_string()),
        "is_flip_flop" => Some(lc.is_flip_flop.to_string()),
        "is_latch" => Some(lc.is_latch.to_string()),
        "is_clock_gating" => Some(lc.is_clock_gating.to_string()),
        "is_tristate" | "is_three_state" => Some(lc.is_tristate.to_string()),
        _ => None,
    }
}

fn lib_pin_attr(lp: &LibPinInfo, attr: &str) -> Option<String> {
    match attr {
        "full_name" => Some(lp.lib_pin.full_name()),
        "name" => Some(lp.lib_pin.pin.clone()),
        "ref_name" | "lib_cell" | "cell_name" => Some(lp.lib_pin.cell.clone()),
        "library" | "lib_name" => Some(lp.lib_pin.library.clone()),
        "direction" | "pin_direction" => Some(lp.dir.to_string()),
        "is_clock" | "is_clock_pin" => Some(lp.is_clock.to_string()),
        _ => None,
    }
}
