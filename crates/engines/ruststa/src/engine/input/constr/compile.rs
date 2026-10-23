// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Command-stream compiler for `CONSTR v1` / [`Sdc`] against [`FrozenNetlist`] and corner
//! [`Library`] slices (owner: E2, then E12).

use rusteda_core::diag::{Diag, Location, Severity};
use rusteda_core::liberty::Direction;
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::{
    Cmd, CmdKind, CmdStatus, EdgeScope, MinMaxScope, Obj, Sdc, SetupHoldScope, find_schema,
};

use super::compile_ext::{
    apply_scale, cmd_location, compile_other_cmd, early_late_indices, edge_indices, emit_d0011,
    emit_d0102_error, has_diag_at, min_max_indices, resolve_clock_list, upsert_pin_uncertainty,
    with_loc,
};
use super::index::{DesignIndex, find_cell_pin, find_lib_cell};
use super::model::{
    ClockLatencySpec, ClockLatencyTarget, CompileError, CompiledClock, CompiledClockGroups,
    CompiledConstraints, CompiledIoDelay, CornerLibs, CornerUnits, DrivingCellSpec,
    InputTransitionSpec, InterClockUncertainty, NetLoadSpec, PortLoadSpec, ResolvedObj,
};
use super::query::{NameLookupHint, QueryContext, resolve_obj};
use crate::engine::types::{ClockId, CornerId, NodeId, Tr};

/// Compiles `sdc` against `netlist` and `corners`, returning `Ok(CompiledConstraints)` when no
/// [`Severity::Error`] diagnostic is present, or `Err(CompileError)` otherwise.
pub fn compile_sdc(
    sdc: &Sdc,
    netlist: &FrozenNetlist,
    corners: &[CornerLibs<'_>],
) -> Result<CompiledConstraints, CompileError> {
    let out = compile_sdc_lenient(sdc, netlist, corners);
    if out.diags.iter().any(|d| d.severity == Severity::Error) {
        Err(CompileError {
            diags: out.diags.clone(),
            partial: Box::new(out),
        })
    } else {
        Ok(out)
    }
}

/// Compiles `sdc` against `netlist` and `corners`, returning the [`CompiledConstraints`] together
/// with all diagnostics (`CompiledConstraints::diags`) regardless of whether errors occurred.
#[must_use]
pub fn compile_sdc_lenient(
    sdc: &Sdc,
    netlist: &FrozenNetlist,
    corners: &[CornerLibs<'_>],
) -> CompiledConstraints {
    let mut index = DesignIndex::build(netlist, corners);
    let mut out = CompiledConstraints {
        sdc_units: sdc.units.clone(),
        sources: sdc.sources.clone(),
        diags: sdc.diags.clone(),
        ..CompiledConstraints::default()
    };

    // 1. Resolve per-corner units and check unit consistency (`STA-REQ-0011`, `D0103`).
    let (time_scale, cap_scale, _res_scale) = resolve_units(sdc, corners, &mut out);

    // 2. Check top-level `sdc.current_design` if no `CurrentDesign` command is in `sdc.cmds`.
    if let Some(cd) = &sdc.current_design
        && !sdc
            .cmds
            .iter()
            .any(|c| matches!(c.kind, CmdKind::CurrentDesign { .. }))
    {
        if cd != &index.top_name {
            out.diags.push(Diag::error(
                "D0104",
                format!(
                    "current_design '{cd}' does not match top module '{}'",
                    index.top_name
                ),
            ));
        } else {
            out.current_design = Some(cd.clone());
        }
    }

    // 3. Process commands in evaluation order.
    let mut exc_order = 0usize;
    for cmd in &sdc.cmds {
        let loc = cmd_location(sdc, cmd);
        if let Some(schema) = find_schema(cmd.name()) {
            match schema.status {
                CmdStatus::Phase2b | CmdStatus::Unsupported => {
                    if !has_diag_at(&out.diags, "D0010", &loc) {
                        out.diags.push(with_loc(
                            Diag::error(
                                "D0010",
                                format!("command '{}' is not supported in Phase 2a", cmd.name()),
                            ),
                            &loc,
                        ));
                    }
                    continue;
                }
                CmdStatus::RecordedDrc => {
                    if !has_diag_at(&out.diags, "D0021", &loc) {
                        out.diags.push(with_loc(
                            Diag::warning(
                                "D0021",
                                format!(
                                    "design-rule command '{}' is recorded (drc_status: unchecked)",
                                    cmd.name()
                                ),
                            ),
                            &loc,
                        ));
                    }
                }
                CmdStatus::RecordedNoTimingEffect => {
                    if !has_diag_at(&out.diags, "D0020", &loc) {
                        out.diags.push(with_loc(
                            Diag::note(
                                "D0020",
                                format!(
                                    "synthesis command '{}' is recorded with no STA timing effect",
                                    cmd.name()
                                ),
                            ),
                            &loc,
                        ));
                    }
                }
                CmdStatus::Interpreted => {}
            }
        }

        compile_one_cmd(
            cmd,
            &loc,
            &mut index,
            corners,
            time_scale,
            cap_scale,
            &mut exc_order,
            &mut out,
        );
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn compile_one_cmd(
    cmd: &Cmd,
    loc: &Option<Location>,
    index: &mut DesignIndex,
    corners: &[CornerLibs<'_>],
    time_scale: f64,
    cap_scale: f64,
    exc_order: &mut usize,
    out: &mut CompiledConstraints,
) {
    match &cmd.kind {
        CmdKind::CreateClock {
            name,
            period,
            waveform,
            add,
            comment,
            sources,
        } => {
            let period_ns = apply_scale(*period, time_scale);
            let waveform_ns = match waveform {
                Some([r, f]) => [apply_scale(*r, time_scale), apply_scale(*f, time_scale)],
                None => [0.0, period_ns / 2.0],
            };
            let mut src_nodes = Vec::new();
            let mut first_src_name = None;
            if let Some(src_obj) = sources {
                let qctx = QueryContext {
                    index,
                    clocks: &out.clocks,
                    hint: NameLookupHint::PortsOrPins,
                    exclude_clocks_on_all_inputs: false,
                    location: loc.clone(),
                };
                let resolved = resolve_obj(src_obj, &qctx, &mut out.diags);
                for r in resolved {
                    match r {
                        ResolvedObj::Port(nid) => {
                            if first_src_name.is_none() {
                                first_src_name = Some(index.node_name(nid).to_string());
                            }
                            if !src_nodes.contains(&nid) {
                                src_nodes.push(nid);
                            }
                        }
                        ResolvedObj::Pin(pr) => {
                            if first_src_name.is_none() {
                                first_src_name = Some(pr.full_name.clone());
                            }
                            if let Some(nid) = pr.node
                                && !src_nodes.contains(&nid)
                            {
                                src_nodes.push(nid);
                            }
                        }
                        other => {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "create_clock",
                                "port or pin",
                                &other,
                            );
                        }
                    }
                }
                if src_nodes.is_empty() && first_src_name.is_none() && name.is_none() {
                    return;
                }
            }
            let clk_name = match name {
                Some(n) => n.clone(),
                None => match first_src_name {
                    Some(s) => s,
                    None => return,
                },
            };

            if !*add && !src_nodes.is_empty() {
                for existing in &mut out.clocks {
                    if existing.name != clk_name {
                        existing.sources.retain(|s| !src_nodes.contains(s));
                    }
                }
            }

            if let Some(existing) = out.clocks.iter_mut().find(|c| c.name == clk_name) {
                existing.period_ns = period_ns;
                existing.waveform_ns = waveform_ns;
                existing.add = *add;
                existing.comment = comment.clone();
                existing.location = loc.clone();
                if *add {
                    for s in src_nodes {
                        if !existing.sources.contains(&s) {
                            existing.sources.push(s);
                        }
                    }
                } else {
                    existing.sources = src_nodes;
                }
            } else {
                let id = ClockId(out.clocks.len() as u32);
                out.clocks.push(CompiledClock {
                    propagated: false,
                    id,
                    name: clk_name,
                    period_ns,
                    waveform_ns,
                    sources: src_nodes,
                    add: *add,
                    comment: comment.clone(),
                    network_latency_ns: [[0.0; 2]; 2],
                    source_latency_ns: [[[0.0; 2]; 2]; 2],
                    transition_ns: [[0.0; 2]; 2],
                    uncertainty_setup_ns: [0.0; 2],
                    uncertainty_hold_ns: [0.0; 2],
                    location: loc.clone(),
                });
            }
            let all_clk_srcs: Vec<NodeId> = out
                .clocks
                .iter()
                .flat_map(|c| c.sources.iter().copied())
                .collect();
            index.recompute_inferred_clock_gating(&all_clk_srcs);
        }
        CmdKind::SetClockGroups {
            kind,
            name,
            allow_paths,
            comment,
            groups,
        } => {
            let mut resolved_groups = Vec::with_capacity(groups.len());
            for g in groups {
                let qctx = QueryContext {
                    index,
                    clocks: &out.clocks,
                    hint: NameLookupHint::Clocks,
                    exclude_clocks_on_all_inputs: false,
                    location: loc.clone(),
                };
                let objs = resolve_obj(g, &qctx, &mut out.diags);
                let mut cids = Vec::new();
                for o in objs {
                    match o {
                        ResolvedObj::Clock(cid) => {
                            if !cids.contains(&cid) {
                                cids.push(cid);
                            }
                        }
                        other => {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "set_clock_groups",
                                "clock",
                                &other,
                            );
                        }
                    }
                }
                resolved_groups.push(cids);
            }
            if *allow_paths {
                out.diags.push(with_loc(
                    Diag::note(
                        "D0114",
                        "'set_clock_groups -allow_paths' separates no clocks in static timing analysis",
                    ),
                    loc,
                ));
            }
            out.clock_groups.push(CompiledClockGroups {
                kind: *kind,
                name: name.clone(),
                allow_paths: *allow_paths,
                comment: comment.clone(),
                groups: resolved_groups,
                location: loc.clone(),
            });
        }
        CmdKind::SetClockUncertainty {
            uncertainty,
            setup_hold,
            from,
            to,
            objects,
        } => {
            let unc_ns = apply_scale(*uncertainty, time_scale);
            if let (Some(from_end), Some(to_end)) = (from, to) {
                let from_clks = resolve_clock_list(
                    &from_end.obj,
                    index,
                    &out.clocks,
                    loc,
                    "set_clock_uncertainty",
                    &mut out.diags,
                );
                let to_clks = resolve_clock_list(
                    &to_end.obj,
                    index,
                    &out.clocks,
                    loc,
                    "set_clock_uncertainty",
                    &mut out.diags,
                );
                for &fc in &from_clks {
                    for &tc in &to_clks {
                        if let Some(existing) = out.inter_clock_uncertainties.iter_mut().find(|u| {
                            u.from_clock == fc
                                && u.from_edge == from_end.edge
                                && u.to_clock == tc
                                && u.to_edge == to_end.edge
                                && u.setup_hold == *setup_hold
                        }) {
                            existing.uncertainty_ns = unc_ns;
                            existing.location = loc.clone();
                        } else {
                            out.inter_clock_uncertainties.push(InterClockUncertainty {
                                from_clock: fc,
                                from_edge: from_end.edge,
                                to_clock: tc,
                                to_edge: to_end.edge,
                                setup_hold: *setup_hold,
                                uncertainty_ns: unc_ns,
                                location: loc.clone(),
                            });
                        }
                    }
                }
            } else if let Some(obj) = objects {
                let qctx = QueryContext {
                    index,
                    clocks: &out.clocks,
                    hint: NameLookupHint::Clocks,
                    exclude_clocks_on_all_inputs: false,
                    location: loc.clone(),
                };
                let resolved = resolve_obj(obj, &qctx, &mut out.diags);
                let (do_setup, do_hold) = match setup_hold {
                    SetupHoldScope::Setup => (true, false),
                    SetupHoldScope::Hold => (false, true),
                    SetupHoldScope::Both => (true, true),
                };
                for r in resolved {
                    match r {
                        ResolvedObj::Clock(cid) => {
                            if let Some(ck) = out.clocks.get_mut(cid.0 as usize) {
                                if do_setup {
                                    ck.uncertainty_setup_ns = [unc_ns, unc_ns];
                                }
                                if do_hold {
                                    ck.uncertainty_hold_ns = [unc_ns, unc_ns];
                                }
                            }
                        }
                        ResolvedObj::Port(nid) => {
                            upsert_pin_uncertainty(
                                &mut out.pin_clock_uncertainties,
                                nid,
                                do_setup.then_some(unc_ns),
                                do_hold.then_some(unc_ns),
                            );
                        }
                        ResolvedObj::Pin(pr) => {
                            if let Some(nid) = pr.node {
                                upsert_pin_uncertainty(
                                    &mut out.pin_clock_uncertainties,
                                    nid,
                                    do_setup.then_some(unc_ns),
                                    do_hold.then_some(unc_ns),
                                );
                            }
                        }
                        other => {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "set_clock_uncertainty",
                                "clock, port, or pin",
                                &other,
                            );
                        }
                    }
                }
            }
        }
        CmdKind::SetClockLatency {
            delay,
            source,
            edge,
            min_max,
            early_late,
            clock,
            objects,
        } => {
            let delay_ns = apply_scale(*delay, time_scale);
            let opt_clk = clock.as_ref().and_then(|cobj| {
                resolve_clock_list(
                    cobj,
                    index,
                    &out.clocks,
                    loc,
                    "set_clock_latency",
                    &mut out.diags,
                )
                .into_iter()
                .next()
            });
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Clocks,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(objects, &qctx, &mut out.diags);
            let ans = min_max_indices(*min_max);
            let trs = edge_indices(*edge);
            let els = early_late_indices(*early_late);
            for r in resolved {
                match r {
                    ResolvedObj::Clock(cid) => {
                        if let Some(ck) = out.clocks.get_mut(cid.0 as usize) {
                            if *source {
                                for &el in &els {
                                    for &an in &ans {
                                        for &tr in &trs {
                                            ck.source_latency_ns[el][an][tr] = delay_ns;
                                        }
                                    }
                                }
                            } else {
                                for &an in &ans {
                                    for &tr in &trs {
                                        ck.network_latency_ns[an][tr] = delay_ns;
                                    }
                                }
                            }
                        }
                        out.clock_latencies.push(ClockLatencySpec {
                            target: ClockLatencyTarget::Clock(cid),
                            delay_ns,
                            source: *source,
                            edge: *edge,
                            min_max: *min_max,
                            early_late: *early_late,
                            clock: opt_clk.or(Some(cid)),
                            location: loc.clone(),
                        });
                    }
                    ResolvedObj::Port(nid) => {
                        if *source
                            && let Some(cid) = opt_clk
                            && let Some(ck) = out.clocks.get_mut(cid.0 as usize)
                        {
                            for &el in &els {
                                for &an in &ans {
                                    for &tr in &trs {
                                        ck.source_latency_ns[el][an][tr] = delay_ns;
                                    }
                                }
                            }
                        }
                        out.clock_latencies.push(ClockLatencySpec {
                            target: ClockLatencyTarget::Node(nid),
                            delay_ns,
                            source: *source,
                            edge: *edge,
                            min_max: *min_max,
                            early_late: *early_late,
                            clock: opt_clk,
                            location: loc.clone(),
                        });
                    }
                    ResolvedObj::Pin(pr) => {
                        if let Some(nid) = pr.node {
                            out.clock_latencies.push(ClockLatencySpec {
                                target: ClockLatencyTarget::Node(nid),
                                delay_ns,
                                source: *source,
                                edge: *edge,
                                min_max: *min_max,
                                early_late: *early_late,
                                clock: opt_clk,
                                location: loc.clone(),
                            });
                        }
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_clock_latency",
                            "clock, port, or pin",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::SetPropagatedClock { objects } | CmdKind::UnsetPropagatedClock { objects } => {
            let ids =
                resolve_clock_list(objects, index, &out.clocks, loc, cmd.name(), &mut out.diags);
            for id in ids {
                out.clocks[id.index()].propagated =
                    matches!(cmd.kind, CmdKind::SetPropagatedClock { .. });
            }
        }
        CmdKind::SetClockTransition {
            transition,
            edge,
            min_max,
            clocks,
        } => {
            let tr_ns = apply_scale(*transition, time_scale);
            let cids = resolve_clock_list(
                clocks,
                index,
                &out.clocks,
                loc,
                "set_clock_transition",
                &mut out.diags,
            );
            let ans = min_max_indices(*min_max);
            let trs = edge_indices(*edge);
            for cid in cids {
                if let Some(ck) = out.clocks.get_mut(cid.0 as usize) {
                    for &an in &ans {
                        for &tr in &trs {
                            ck.transition_ns[an][tr] = tr_ns;
                        }
                    }
                }
            }
        }
        CmdKind::SetInputDelay {
            delay,
            clock,
            clock_fall,
            level_sensitive,
            edge,
            min_max,
            add_delay,
            network_latency_included,
            source_latency_included,
            reference_pin,
            ports_or_pins,
        } => {
            compile_io_delay(
                true,
                *delay,
                clock.as_ref(),
                *clock_fall,
                *level_sensitive,
                *edge,
                *min_max,
                *add_delay,
                *network_latency_included,
                *source_latency_included,
                reference_pin.as_ref(),
                ports_or_pins,
                loc,
                index,
                time_scale,
                out,
            );
        }
        CmdKind::SetOutputDelay {
            delay,
            clock,
            clock_fall,
            level_sensitive,
            edge,
            min_max,
            add_delay,
            network_latency_included,
            source_latency_included,
            reference_pin,
            ports_or_pins,
        } => {
            compile_io_delay(
                false,
                *delay,
                clock.as_ref(),
                *clock_fall,
                *level_sensitive,
                *edge,
                *min_max,
                *add_delay,
                *network_latency_included,
                *source_latency_included,
                reference_pin.as_ref(),
                ports_or_pins,
                loc,
                index,
                time_scale,
                out,
            );
        }
        CmdKind::SetInputTransition {
            transition,
            edge,
            min_max,
            clock,
            clock_fall,
            ports,
        } => {
            let slew_ns = apply_scale(*transition, time_scale);
            let opt_clk = clock.as_ref().and_then(|cobj| {
                resolve_clock_list(
                    cobj,
                    index,
                    &out.clocks,
                    loc,
                    "set_input_transition",
                    &mut out.diags,
                )
                .into_iter()
                .next()
            });
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: true,
                location: loc.clone(),
            };
            let resolved = resolve_obj(ports, &qctx, &mut out.diags);
            let ans = min_max_indices(*min_max);
            let trs = edge_indices(*edge);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        let pb = &index.port_bits[nid.0 as usize];
                        if !pb.is_input() {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "set_input_transition",
                                "input or inout port",
                                &ResolvedObj::Port(nid),
                            );
                            continue;
                        }
                        let entry = out.input_transitions.entry(nid).or_insert_with(|| {
                            InputTransitionSpec {
                                port: nid,
                                slew_ns: [[None; 2]; 2],
                                clock: opt_clk,
                                clock_fall: *clock_fall,
                                location: loc.clone(),
                            }
                        });
                        for &an in &ans {
                            for &tr in &trs {
                                entry.slew_ns[an][tr] = Some(slew_ns);
                            }
                        }
                        entry.clock = opt_clk;
                        entry.clock_fall = *clock_fall;
                        entry.location = loc.clone();
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_input_transition",
                            "input port",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::SetDrivingCell {
            lib_cell,
            library,
            pin,
            from_pin,
            edge,
            min_max,
            input_transition_rise,
            input_transition_fall,
            multiply_by,
            dont_scale,
            no_design_rule,
            clock,
            clock_fall,
            ports,
        } => {
            let opt_clk = clock.as_ref().and_then(|cobj| {
                resolve_clock_list(
                    cobj,
                    index,
                    &out.clocks,
                    loc,
                    "set_driving_cell",
                    &mut out.diags,
                )
                .into_iter()
                .next()
            });

            let has_libs = corners.iter().any(|c| !c.libs.is_empty());
            let mut resolved_out_pin = pin.clone().unwrap_or_default();
            if has_libs {
                let found = corners
                    .iter()
                    .flat_map(|c| c.libs.iter())
                    .find_map(|l| {
                        find_lib_cell(std::slice::from_ref(l), library.as_deref(), lib_cell)
                    })
                    .or_else(|| {
                        corners
                            .iter()
                            .flat_map(|c| c.libs.iter())
                            .find_map(|l| find_lib_cell(std::slice::from_ref(l), None, lib_cell))
                    });
                match found {
                    Some((_, cell_def)) => {
                        if let Some(pin_name) = pin {
                            if find_cell_pin(cell_def, pin_name).is_none() {
                                out.diags.push(with_loc(
                                    Diag::error(
                                        "D0101",
                                        format!(
                                            "set_driving_cell: cell '{lib_cell}' has no pin '{pin_name}'"
                                        ),
                                    ),
                                    loc,
                                ));
                            }
                        } else if let Some(out_p) = cell_def
                            .pins
                            .iter()
                            .find(|p| matches!(p.direction, Direction::Output | Direction::Inout))
                        {
                            resolved_out_pin = out_p.name.clone();
                        }
                        if let Some(fp) = from_pin
                            && find_cell_pin(cell_def, fp).is_none()
                        {
                            out.diags.push(with_loc(
                                Diag::error(
                                    "D0101",
                                    format!(
                                        "set_driving_cell: cell '{lib_cell}' has no -from_pin '{fp}'"
                                    ),
                                ),
                                loc,
                            ));
                        }
                    }
                    None => {
                        out.diags.push(with_loc(
                            Diag::error(
                                "D0101",
                                format!(
                                    "set_driving_cell: library cell '{lib_cell}' was not found"
                                ),
                            ),
                            loc,
                        ));
                    }
                }
            }

            let in_tr_rise_ns = input_transition_rise.map(|v| apply_scale(v, time_scale));
            let in_tr_fall_ns = input_transition_fall.map(|v| apply_scale(v, time_scale));
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: true,
                location: loc.clone(),
            };
            let resolved = resolve_obj(ports, &qctx, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        let pb = &index.port_bits[nid.0 as usize];
                        if !pb.is_input() {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "set_driving_cell",
                                "input or inout port",
                                &ResolvedObj::Port(nid),
                            );
                            continue;
                        }
                        out.driving_cells
                            .entry(nid)
                            .or_default()
                            .push(DrivingCellSpec {
                                port: nid,
                                lib_cell: lib_cell.clone(),
                                library: library.clone(),
                                pin: resolved_out_pin.clone(),
                                from_pin: from_pin.clone(),
                                edge: *edge,
                                min_max: *min_max,
                                input_transition_rise_ns: in_tr_rise_ns,
                                input_transition_fall_ns: in_tr_fall_ns,
                                multiply_by: *multiply_by,
                                dont_scale: *dont_scale,
                                no_design_rule: *no_design_rule,
                                clock: opt_clk,
                                clock_fall: *clock_fall,
                                location: loc.clone(),
                            });
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_driving_cell",
                            "input port",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::SetLoad {
            value,
            pin_load,
            wire_load,
            subtract_pin_load,
            min_max,
            objects,
        } => {
            let load_pf = apply_scale(*value, cap_scale);
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::PortsOrNets,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(objects, &qctx, &mut out.diags);
            let ans = min_max_indices(*min_max);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        let entry = out.port_loads.entry(nid).or_insert_with(|| PortLoadSpec {
                            port: nid,
                            load_pf: [load_pf, load_pf],
                            pin_load: *pin_load,
                            wire_load: *wire_load,
                            subtract_pin_load: *subtract_pin_load,
                            location: loc.clone(),
                        });
                        for &an in &ans {
                            entry.load_pf[an] = load_pf;
                        }
                        entry.pin_load = *pin_load;
                        entry.wire_load = *wire_load;
                        entry.subtract_pin_load = *subtract_pin_load;
                        entry.location = loc.clone();
                    }
                    ResolvedObj::Net(nr) => {
                        let entry = out.net_loads.entry(nr).or_insert_with(|| NetLoadSpec {
                            net: nr,
                            load_pf: [load_pf, load_pf],
                            pin_load: *pin_load,
                            wire_load: *wire_load,
                            subtract_pin_load: *subtract_pin_load,
                            location: loc.clone(),
                        });
                        for &an in &ans {
                            entry.load_pf[an] = load_pf;
                        }
                        entry.pin_load = *pin_load;
                        entry.wire_load = *wire_load;
                        entry.subtract_pin_load = *subtract_pin_load;
                        entry.location = loc.clone();
                    }
                    other => {
                        emit_d0102_error(&mut out.diags, loc, "set_load", "port or net", &other);
                    }
                }
            }
        }
        _ => compile_other_cmd(
            cmd, loc, index, corners, time_scale, cap_scale, exc_order, out,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_io_delay(
    is_input: bool,
    raw_delay: f64,
    clock_obj: Option<&Obj>,
    clock_fall: bool,
    level_sensitive: bool,
    edge: EdgeScope,
    min_max: MinMaxScope,
    add_delay: bool,
    network_latency_included: bool,
    source_latency_included: bool,
    reference_pin: Option<&Obj>,
    ports_or_pins: &Obj,
    loc: &Option<Location>,
    index: &DesignIndex,
    time_scale: f64,
    out: &mut CompiledConstraints,
) {
    let cmd_name = if is_input {
        "set_input_delay"
    } else {
        "set_output_delay"
    };
    if reference_pin.is_some() {
        emit_d0011(&mut out.diags, loc, cmd_name, "-reference_pin");
        return;
    }
    let delay_ns = apply_scale(raw_delay, time_scale);
    let opt_clk = clock_obj.and_then(|cobj| {
        let cids = resolve_clock_list(cobj, index, &out.clocks, loc, cmd_name, &mut out.diags);
        cids.into_iter().next()
    });
    if clock_obj.is_some() && opt_clk.is_none() {
        return;
    }
    let clock_edge = if clock_fall { Tr::Fall } else { Tr::Rise };
    let qctx = QueryContext {
        index,
        clocks: &out.clocks,
        hint: NameLookupHint::PortsOrPins,
        exclude_clocks_on_all_inputs: is_input,
        location: loc.clone(),
    };
    let resolved = resolve_obj(ports_or_pins, &qctx, &mut out.diags);
    let ans = min_max_indices(min_max);
    let trs = edge_indices(edge);

    let mut target_nodes = Vec::new();
    for r in resolved {
        match r {
            ResolvedObj::Port(nid) => {
                let pb = &index.port_bits[nid.0 as usize];
                let ok_dir = if is_input {
                    pb.is_input()
                } else {
                    pb.is_output()
                };
                if !ok_dir {
                    emit_d0102_error(
                        &mut out.diags,
                        loc,
                        cmd_name,
                        if is_input {
                            "input or inout port"
                        } else {
                            "output or inout port"
                        },
                        &ResolvedObj::Port(nid),
                    );
                    continue;
                }
                if !target_nodes.contains(&nid) {
                    target_nodes.push(nid);
                }
            }
            ResolvedObj::Pin(pr) => {
                if let Some(nid) = pr.node
                    && !target_nodes.contains(&nid)
                {
                    target_nodes.push(nid);
                }
            }
            other => {
                emit_d0102_error(&mut out.diags, loc, cmd_name, "port or pin", &other);
            }
        }
    }

    let list = if is_input {
        &mut out.input_delays
    } else {
        &mut out.output_delays
    };

    for nid in target_nodes {
        let pos = list
            .iter()
            .position(|d| d.node == nid && d.clock == opt_clk && d.clock_edge == clock_edge);
        let idx = match pos {
            Some(i) => i,
            None => {
                if !add_delay {
                    list.retain(|d| d.node != nid);
                }
                list.push(CompiledIoDelay {
                    node: nid,
                    clock: opt_clk,
                    clock_edge,
                    level_sensitive,
                    network_latency_included,
                    source_latency_included,
                    delay_ns: [[None; 2]; 2],
                    location: loc.clone(),
                });
                list.len() - 1
            }
        };
        let entry = &mut list[idx];
        entry.level_sensitive = level_sensitive;
        entry.network_latency_included = network_latency_included;
        entry.source_latency_included = source_latency_included;
        entry.location = loc.clone();
        for &an in &ans {
            for &tr in &trs {
                entry.delay_ns[an][tr] = Some(delay_ns);
            }
        }
    }
}

fn resolve_units(
    sdc: &Sdc,
    corners: &[CornerLibs<'_>],
    out: &mut CompiledConstraints,
) -> (f64, f64, f64) {
    let mut first_lib_units: Option<(String, f64, f64, f64)> = None;
    let mut d0103_emitted = false;

    for (ci, corner) in corners.iter().enumerate() {
        let mut corner_lib_units: Option<(&str, f64, f64, f64)> = None;
        for lib in corner.libs {
            let t = lib.units.time.scale;
            let c = lib.units.capacitance.scale;
            let r = lib.units.resistance.scale;
            if let Some((prev_name, pt, pc, pr)) = corner_lib_units {
                if (!scales_agree(pt, t) || !scales_agree(pc, c) || !scales_agree(pr, r))
                    && !d0103_emitted
                {
                    out.diags.push(Diag::error(
                        "D0103",
                        format!(
                            "corner '{}' libraries '{}' and '{}' disagree on units",
                            corner.name, prev_name, lib.name
                        ),
                    ));
                    d0103_emitted = true;
                }
            } else {
                corner_lib_units = Some((&lib.name, t, c, r));
            }

            if let Some((ref first_name, ft, fc, fr)) = first_lib_units {
                let time_mismatch = sdc.units.time_to_ns.is_none() && !scales_agree(ft, t);
                let cap_mismatch = sdc.units.cap_to_pf.is_none() && !scales_agree(fc, c);
                let res_mismatch = sdc.units.res_to_kohm.is_none() && !scales_agree(fr, r);
                if (time_mismatch || cap_mismatch || res_mismatch) && !d0103_emitted {
                    out.diags.push(Diag::error(
                        "D0103",
                        format!(
                            "libraries '{}' and '{}' disagree on units while library units are in effect",
                            first_name, lib.name
                        ),
                    ));
                    d0103_emitted = true;
                }
            } else {
                first_lib_units = Some((lib.name.clone(), t, c, r));
            }
        }

        let first_lib = corner.libs.first();
        let lib_t = first_lib.map_or(1.0, |l| l.units.time.scale);
        let lib_c = first_lib.map_or(1.0, |l| l.units.capacitance.scale);
        let lib_r = first_lib.map_or(1.0, |l| l.units.resistance.scale);
        let lib_v = first_lib.map_or(1.0, |l| l.units.voltage.scale);
        let lib_i = first_lib.map_or(1.0, |l| l.units.current.scale);
        let lib_p = first_lib.map_or(1.0, |l| l.units.leakage_power.scale);

        out.corner_units.push(CornerUnits {
            corner: CornerId(ci as u32),
            name: corner.name.to_string(),
            time_to_ns: sdc.units.time_to_ns.unwrap_or(lib_t),
            cap_to_pf: sdc.units.cap_to_pf.unwrap_or(lib_c),
            res_to_kohm: sdc.units.res_to_kohm.unwrap_or(lib_r),
            voltage_to_v: sdc.units.voltage_to_v.unwrap_or(lib_v),
            current_to_ma: sdc.units.current_to_ma.unwrap_or(lib_i),
            power_to_nw: sdc.units.power_to_nw.unwrap_or(lib_p),
        });
    }

    if out.corner_units.is_empty() {
        let cu = CornerUnits {
            corner: CornerId(0),
            name: "default".to_string(),
            time_to_ns: sdc.units.time_to_ns.unwrap_or(1.0),
            cap_to_pf: sdc.units.cap_to_pf.unwrap_or(1.0),
            res_to_kohm: sdc.units.res_to_kohm.unwrap_or(1.0),
            voltage_to_v: sdc.units.voltage_to_v.unwrap_or(1.0),
            current_to_ma: sdc.units.current_to_ma.unwrap_or(1.0),
            power_to_nw: sdc.units.power_to_nw.unwrap_or(1.0),
        };
        let scales = (cu.time_to_ns, cu.cap_to_pf, cu.res_to_kohm);
        out.corner_units.push(cu);
        scales
    } else {
        let cu = &out.corner_units[0];
        (cu.time_to_ns, cu.cap_to_pf, cu.res_to_kohm)
    }
}

fn scales_agree(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}
