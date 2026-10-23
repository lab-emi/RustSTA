// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Secondary command handlers (environment, exceptions, mode/disable/latch, DRC, synthesis)
//! and diagnostic helpers.

use rusteda_core::diag::{Diag, Location};
use rusteda_formats::sdc::{
    CaseValue, Cmd, CmdKind, EarlyLateScope, EdgeScope, ExceptionEnd, MinMaxScope, Obj, Sdc,
    SetupHoldScope,
};

use super::index::DesignIndex;
use super::model::{
    CompiledClock, CompiledConstraints, CompiledException, CompiledPathGroup, CornerLibs,
    DisabledTiming, DrcConstraint, DrcConstraintKind, ExceptionEndpoints, ExceptionKind,
    MaxTimeBorrowSpec, MaxTimeBorrowTarget, OperatingConditionsSpec, PinClockUncertainty,
    ResolvedObj, SynthConstraint, WireLoadModelSpec, WireLoadSelectionSpec,
};
use super::query::{NameLookupHint, QueryContext, resolve_obj};
use crate::engine::types::{An, ClockId, NodeId, Tr};

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_other_cmd(
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
        CmdKind::SetUnits {
            time,
            capacitance,
            resistance,
            voltage,
            current,
            power,
        } => {
            if let Some(t) = time {
                out.explicit_units.time = Some(t.clone());
            }
            if let Some(c) = capacitance {
                out.explicit_units.capacitance = Some(c.clone());
            }
            if let Some(r) = resistance {
                out.explicit_units.resistance = Some(r.clone());
            }
            if let Some(v) = voltage {
                out.explicit_units.voltage = Some(v.clone());
            }
            if let Some(i) = current {
                out.explicit_units.current = Some(i.clone());
            }
            if let Some(p) = power {
                out.explicit_units.power = Some(p.clone());
            }
        }
        CmdKind::CurrentDesign { name } => {
            if name != &index.top_name {
                out.diags.push(with_loc(
                    Diag::error(
                        "D0104",
                        format!(
                            "current_design '{name}' does not match top module '{}'",
                            index.top_name
                        ),
                    ),
                    loc,
                ));
            } else {
                out.current_design = Some(name.clone());
            }
        }
        CmdKind::SetOperatingConditions {
            condition,
            min,
            max,
            library,
            min_library,
            max_library,
            analysis_type,
            objects,
        } => {
            for (cond_opt, lib_opt) in [
                (condition.as_deref(), library.as_deref()),
                (
                    min.as_deref(),
                    min_library.as_deref().or(library.as_deref()),
                ),
                (
                    max.as_deref(),
                    max_library.as_deref().or(library.as_deref()),
                ),
            ] {
                if let Some(cond_name) = cond_opt
                    && !operating_condition_exists(corners, cond_name, lib_opt)
                {
                    out.diags.push(with_loc(
                        Diag::error(
                            "D0105",
                            format!(
                                "operating condition '{cond_name}' was not found in corner libraries"
                            ),
                        ),
                        loc,
                    ));
                }
            }
            let resolved_objs = match objects {
                Some(obj) => resolve_any_objs(obj, index, &out.clocks, loc, &mut out.diags),
                None => Vec::new(),
            };
            out.operating_conditions.push(OperatingConditionsSpec {
                condition: condition.clone(),
                min: min.clone(),
                max: max.clone(),
                library: library.clone(),
                min_library: min_library.clone(),
                max_library: max_library.clone(),
                analysis_type: *analysis_type,
                objects: resolved_objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetWireLoadModel {
            name,
            library,
            min,
            max,
            objects,
        } => {
            let resolved_objs = match objects {
                Some(obj) => resolve_any_objs(obj, index, &out.clocks, loc, &mut out.diags),
                None => Vec::new(),
            };
            if resolved_objs.is_empty()
                || resolved_objs
                    .iter()
                    .any(|o| matches!(o, ResolvedObj::Design(_)))
            {
                let (apply_min, apply_max) = match (*min, *max) {
                    (true, false) => (true, false),
                    (false, true) => (false, true),
                    _ => (true, true),
                };
                if apply_min {
                    out.wire_load.model_min = Some(name.clone());
                }
                if apply_max {
                    out.wire_load.model_max = Some(name.clone());
                }
            }
            out.wire_load.models.push(WireLoadModelSpec {
                name: name.clone(),
                library: library.clone(),
                min: *min,
                max: *max,
                objects: resolved_objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetWireLoadMode { mode } => {
            out.wire_load.mode = Some(*mode);
        }
        CmdKind::SetWireLoadSelectionGroup {
            group_name,
            library,
            min,
            max,
            objects,
        } => {
            let resolved_objs = match objects {
                Some(obj) => resolve_any_objs(obj, index, &out.clocks, loc, &mut out.diags),
                None => Vec::new(),
            };
            if resolved_objs.is_empty()
                || resolved_objs
                    .iter()
                    .any(|o| matches!(o, ResolvedObj::Design(_)))
            {
                let (apply_min, apply_max) = match (*min, *max) {
                    (true, false) => (true, false),
                    (false, true) => (false, true),
                    _ => (true, true),
                };
                if apply_min {
                    out.wire_load.selection_min = Some(group_name.clone());
                }
                if apply_max {
                    out.wire_load.selection_max = Some(group_name.clone());
                }
            }
            out.wire_load.selection_groups.push(WireLoadSelectionSpec {
                group_name: group_name.clone(),
                library: library.clone(),
                min: *min,
                max: *max,
                objects: resolved_objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetFalsePath {
            setup_hold,
            edge,
            from,
            through,
            to,
            comment,
        } => {
            if !through.is_empty() {
                emit_d0011(&mut out.diags, loc, "set_false_path", "-through");
                return;
            }
            compile_exception(
                ExceptionKind::FalsePath {
                    setup_hold: *setup_hold,
                },
                *edge,
                from.as_ref(),
                to.as_ref(),
                comment.clone(),
                loc,
                index,
                exc_order,
                out,
            );
        }
        CmdKind::SetMulticyclePath {
            multiplier,
            setup_hold,
            start_end,
            edge,
            from,
            through,
            to,
            comment,
        } => {
            if !through.is_empty() {
                emit_d0011(&mut out.diags, loc, "set_multicycle_path", "-through");
                return;
            }
            if *setup_hold == SetupHoldScope::Both {
                out.diags.push(with_loc(
                    Diag::warning(
                        "D0113",
                        "set_multicycle_path with both '-setup' and '-hold' leaves the hold multiplier at 0",
                    ),
                    loc,
                ));
            }
            let (setup_mult, hold_mult) = match setup_hold {
                SetupHoldScope::Setup | SetupHoldScope::Both => (Some(*multiplier), None),
                SetupHoldScope::Hold => (None, Some(*multiplier)),
            };
            compile_exception(
                ExceptionKind::MulticyclePath {
                    multiplier: *multiplier,
                    setup_multiplier: setup_mult,
                    hold_multiplier: hold_mult,
                    setup_hold: *setup_hold,
                    start_end: *start_end,
                },
                *edge,
                from.as_ref(),
                to.as_ref(),
                comment.clone(),
                loc,
                index,
                exc_order,
                out,
            );
        }
        CmdKind::SetMaxDelay {
            delay,
            edge,
            from,
            through,
            to,
            ignore_clock_latency,
            comment,
        } => {
            if !through.is_empty() {
                emit_d0011(&mut out.diags, loc, "set_max_delay", "-through");
                return;
            }
            let delay_ns = apply_scale(*delay, time_scale);
            compile_exception(
                ExceptionKind::MaxDelay {
                    delay_ns,
                    ignore_clock_latency: *ignore_clock_latency,
                },
                *edge,
                from.as_ref(),
                to.as_ref(),
                comment.clone(),
                loc,
                index,
                exc_order,
                out,
            );
        }
        CmdKind::SetMinDelay {
            delay,
            edge,
            from,
            through,
            to,
            ignore_clock_latency,
            comment,
        } => {
            if !through.is_empty() {
                emit_d0011(&mut out.diags, loc, "set_min_delay", "-through");
                return;
            }
            let delay_ns = apply_scale(*delay, time_scale);
            compile_exception(
                ExceptionKind::MinDelay {
                    delay_ns,
                    ignore_clock_latency: *ignore_clock_latency,
                },
                *edge,
                from.as_ref(),
                to.as_ref(),
                comment.clone(),
                loc,
                index,
                exc_order,
                out,
            );
        }
        CmdKind::SetCaseAnalysis {
            value,
            ports_or_pins,
        } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::PortsOrPins,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(ports_or_pins, &qctx, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        out.case_analysis.insert(nid, *value);
                    }
                    ResolvedObj::Pin(pr) => {
                        if let Some(nid) = pr.node {
                            out.case_analysis.insert(nid, *value);
                        }
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_case_analysis",
                            "port or pin",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::SetLogicZero { ports } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(ports, &qctx, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        out.case_analysis.insert(nid, CaseValue::Zero);
                        if !out.logic_zero_ports.contains(&nid) {
                            out.logic_zero_ports.push(nid);
                        }
                    }
                    other => {
                        emit_d0102_error(&mut out.diags, loc, "set_logic_zero", "port", &other);
                    }
                }
            }
        }
        CmdKind::SetLogicOne { ports } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(ports, &qctx, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Port(nid) => {
                        out.case_analysis.insert(nid, CaseValue::One);
                        if !out.logic_one_ports.contains(&nid) {
                            out.logic_one_ports.push(nid);
                        }
                    }
                    other => {
                        emit_d0102_error(&mut out.diags, loc, "set_logic_one", "port", &other);
                    }
                }
            }
        }
        CmdKind::SetDisableTiming { from, to, objects } => {
            let resolved = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Cell(inst_id) => {
                        if let (Some(f), Some(t)) = (from, to) {
                            let inst_info = &index.instances[inst_id as usize];
                            let has_from = inst_info
                                .pin_indices
                                .iter()
                                .any(|&pi| *index.all_pins[pi].pin_name == **f);
                            let has_to = inst_info
                                .pin_indices
                                .iter()
                                .any(|&pi| *index.all_pins[pi].pin_name == **t);
                            if !has_from || !has_to {
                                out.diags.push(with_loc(
                                    Diag::error(
                                        "D0101",
                                        format!(
                                            "set_disable_timing: cell '{}' has no arc '{} -> {}'",
                                            inst_info.full_name, f, t
                                        ),
                                    ),
                                    loc,
                                ));
                                continue;
                            }
                            out.disabled_timing.push(DisabledTiming::CellArc {
                                inst: inst_id,
                                from_pin: f.clone(),
                                to_pin: t.clone(),
                            });
                        } else {
                            out.disabled_timing
                                .push(DisabledTiming::Cell { inst: inst_id });
                        }
                    }
                    ResolvedObj::Pin(pr) => {
                        if from.is_some() || to.is_some() {
                            emit_d0102_error(
                                &mut out.diags,
                                loc,
                                "set_disable_timing -from/-to",
                                "cell or lib_cell",
                                &ResolvedObj::Pin(pr),
                            );
                        } else {
                            out.disabled_timing.push(DisabledTiming::Pin { pin: pr });
                        }
                    }
                    ResolvedObj::Port(nid) => {
                        out.disabled_timing.push(DisabledTiming::Port { port: nid });
                    }
                    ResolvedObj::LibCell(lc) => {
                        out.disabled_timing.push(DisabledTiming::LibCell {
                            lib_cell: lc,
                            from_pin: from.clone(),
                            to_pin: to.clone(),
                        });
                    }
                    ResolvedObj::LibPin(lp) => {
                        out.disabled_timing
                            .push(DisabledTiming::LibPin { lib_pin: lp });
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_disable_timing",
                            "cell, pin, port, lib_cell, or lib_pin",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::SetMaxTimeBorrow { value, objects } => {
            let max_borrow_ns = apply_scale(*value, time_scale);
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::AnyEndpoint,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(objects, &qctx, &mut out.diags);
            for r in resolved {
                match r {
                    ResolvedObj::Clock(cid) => {
                        out.max_time_borrow.push(MaxTimeBorrowSpec {
                            target: MaxTimeBorrowTarget::Clock(cid),
                            max_borrow_ns,
                            location: loc.clone(),
                        });
                    }
                    ResolvedObj::Cell(inst_id) => {
                        out.latch_max_borrow_ns.insert(inst_id, max_borrow_ns);
                        out.max_time_borrow.push(MaxTimeBorrowSpec {
                            target: MaxTimeBorrowTarget::Cell(inst_id),
                            max_borrow_ns,
                            location: loc.clone(),
                        });
                    }
                    ResolvedObj::Pin(pr) => {
                        out.latch_max_borrow_ns.insert(pr.inst, max_borrow_ns);
                        if let Some(nid) = pr.node {
                            out.max_time_borrow.push(MaxTimeBorrowSpec {
                                target: MaxTimeBorrowTarget::Pin(nid),
                                max_borrow_ns,
                                location: loc.clone(),
                            });
                        }
                    }
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_max_time_borrow",
                            "clock, latch cell, or latch pin",
                            &other,
                        );
                    }
                }
            }
        }
        CmdKind::GroupPath {
            name,
            default,
            weight,
            critical_range,
            from,
            through,
            to,
            comment,
        } => {
            if !through.is_empty() {
                emit_d0011(&mut out.diags, loc, "group_path", "-through");
                return;
            }
            let from_ep = from
                .as_ref()
                .map(|f| resolve_exception_end(f, true, index, &out.clocks, loc, &mut out.diags));
            let to_ep = to
                .as_ref()
                .map(|t| resolve_exception_end(t, false, index, &out.clocks, loc, &mut out.diags));
            out.path_groups.push(CompiledPathGroup {
                name: name.clone(),
                default: *default,
                weight: *weight,
                critical_range_ns: critical_range.map(|v| apply_scale(v, time_scale)),
                from: from_ep,
                to: to_ep,
                comment: comment.clone(),
                location: loc.clone(),
            });
        }
        CmdKind::SetMaxTransition {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            let val_ns = apply_scale(*value, time_scale);
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::MaxTransition {
                    value_ns: val_ns,
                    clock_path: *clock_path,
                    data_path: *data_path,
                    edge: *edge,
                },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetMaxCapacitance {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            let val_pf = apply_scale(*value, cap_scale);
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::MaxCapacitance {
                    value_pf: val_pf,
                    clock_path: *clock_path,
                    data_path: *data_path,
                    edge: *edge,
                },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetMinCapacitance {
            value,
            clock_path,
            data_path,
            edge,
            objects,
        } => {
            let val_pf = apply_scale(*value, cap_scale);
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::MinCapacitance {
                    value_pf: val_pf,
                    clock_path: *clock_path,
                    data_path: *data_path,
                    edge: *edge,
                },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetMaxFanout { value, objects } => {
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::MaxFanout { value: *value },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetFanoutLoad { value, ports } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let objs = resolve_obj(ports, &qctx, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::FanoutLoad { value: *value },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetPortFanoutNumber { value, ports } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Ports,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let objs = resolve_obj(ports, &qctx, &mut out.diags);
            out.drc_constraints.push(DrcConstraint {
                kind: DrcConstraintKind::PortFanoutNumber { value: *value },
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetDontUse { lib_cells } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::LibCells,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(lib_cells, &qctx, &mut out.diags);
            let mut lcs = Vec::new();
            for r in resolved {
                match r {
                    ResolvedObj::LibCell(lc) => lcs.push(lc),
                    other => {
                        emit_d0102_error(&mut out.diags, loc, "set_dont_use", "lib_cell", &other);
                    }
                }
            }
            out.synth_constraints.push(SynthConstraint::DontUse {
                lib_cells: lcs,
                location: loc.clone(),
            });
        }
        CmdKind::SetDontTouch { objects } => {
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.synth_constraints.push(SynthConstraint::DontTouch {
                objects: objs,
                location: loc.clone(),
            });
        }
        CmdKind::SetDontTouchNetwork {
            no_propagate,
            objects,
        } => {
            let objs = resolve_any_objs(objects, index, &out.clocks, loc, &mut out.diags);
            out.synth_constraints
                .push(SynthConstraint::DontTouchNetwork {
                    no_propagate: *no_propagate,
                    objects: objs,
                    location: loc.clone(),
                });
        }
        CmdKind::SetSizeOnly {
            instances,
            no_instances,
        } => {
            let qctx = QueryContext {
                index,
                clocks: &out.clocks,
                hint: NameLookupHint::Cells,
                exclude_clocks_on_all_inputs: false,
                location: loc.clone(),
            };
            let resolved = resolve_obj(instances, &qctx, &mut out.diags);
            let mut inst_ids = Vec::new();
            for r in resolved {
                match r {
                    ResolvedObj::Cell(id) => inst_ids.push(id),
                    other => {
                        emit_d0102_error(
                            &mut out.diags,
                            loc,
                            "set_size_only",
                            "cell instance",
                            &other,
                        );
                    }
                }
            }
            out.synth_constraints.push(SynthConstraint::SizeOnly {
                instances: inst_ids,
                no_instances: *no_instances,
                location: loc.clone(),
            });
        }
        CmdKind::SetMaxArea { value, ignore_tns } => {
            out.synth_constraints.push(SynthConstraint::MaxArea {
                value: *value,
                ignore_tns: *ignore_tns,
                location: loc.clone(),
            });
        }
        CmdKind::SetMaxDynamicPower { value, unit } => {
            out.synth_constraints
                .push(SynthConstraint::MaxDynamicPower {
                    value: *value,
                    unit: unit.clone(),
                    location: loc.clone(),
                });
        }
        CmdKind::SetMaxLeakagePower { value, unit } => {
            out.synth_constraints
                .push(SynthConstraint::MaxLeakagePower {
                    value: *value,
                    unit: unit.clone(),
                    location: loc.clone(),
                });
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_exception(
    kind: ExceptionKind,
    edge: EdgeScope,
    from: Option<&ExceptionEnd>,
    to: Option<&ExceptionEnd>,
    comment: Option<String>,
    loc: &Option<Location>,
    index: &DesignIndex,
    exc_order: &mut usize,
    out: &mut CompiledConstraints,
) {
    let from_ep =
        from.map(|f| resolve_exception_end(f, true, index, &out.clocks, loc, &mut out.diags));
    let to_ep =
        to.map(|t| resolve_exception_end(t, false, index, &out.clocks, loc, &mut out.diags));

    // An option whose objects are all ignored matches no path, so the exception matches none
    // (STA-REQ-0255): dropping only the option would widen the exception to every path of the
    // other option.
    let empty_option = from_ep.as_ref().is_some_and(ExceptionEndpoints::is_empty)
        || to_ep.as_ref().is_some_and(ExceptionEndpoints::is_empty);
    if empty_option || (from_ep.is_none() && to_ep.is_none()) {
        out.diags.push(with_loc(
            Diag::warning(
                "D0110",
                "timing exception has no usable '-from', '-through', or '-to' objects",
            ),
            loc,
        ));
        return;
    }

    let order = *exc_order;
    *exc_order += 1;
    out.exceptions.push(CompiledException {
        order,
        kind,
        edge,
        from: from_ep,
        to: to_ep,
        comment,
        location: loc.clone(),
    });
}

fn resolve_exception_end(
    end: &ExceptionEnd,
    is_from: bool,
    index: &DesignIndex,
    clocks: &[CompiledClock],
    loc: &Option<Location>,
    diags: &mut Vec<Diag>,
) -> ExceptionEndpoints {
    let qctx = QueryContext {
        index,
        clocks,
        hint: NameLookupHint::AnyEndpoint,
        exclude_clocks_on_all_inputs: false,
        location: loc.clone(),
    };
    let resolved = resolve_obj(&end.obj, &qctx, diags);
    let mut ep = ExceptionEndpoints {
        edge: end.edge,
        ..ExceptionEndpoints::default()
    };
    let flag_name = if is_from { "-from" } else { "-to" };

    for r in resolved {
        match r {
            ResolvedObj::Clock(cid) => {
                if !ep.clocks.contains(&cid) {
                    ep.clocks.push(cid);
                }
            }
            ResolvedObj::Port(nid) => {
                let pb = &index.port_bits[nid.0 as usize];
                let valid = if is_from {
                    pb.is_input()
                } else {
                    pb.is_output()
                };
                if valid {
                    ep.nodes.push(nid);
                } else {
                    emit_d0102_warning(diags, loc, flag_name, &pb.full_name);
                }
            }
            ResolvedObj::Pin(pr) => {
                let pin_idx = pr
                    .node
                    .and_then(|nid| {
                        index
                            .node_to_pin_idx
                            .get((nid.0 - index.n_port_bits) as usize)
                            .copied()
                    })
                    .filter(|&pi| pi < index.all_pins.len());
                let valid = match pin_idx {
                    Some(pi) if is_from => index.is_startpoint_pin(pi),
                    Some(pi) => index.is_endpoint_pin(pi),
                    None => false,
                };
                if valid && let Some(nid) = pr.node {
                    ep.nodes.push(nid);
                } else {
                    emit_d0102_warning(diags, loc, flag_name, &pr.full_name);
                }
            }
            ResolvedObj::Cell(inst_id) => {
                let inst_info = &index.instances[inst_id as usize];
                let mut matched_pins = Vec::new();
                for &pi in &inst_info.pin_indices {
                    let ok = if is_from {
                        index.is_startpoint_pin(pi)
                    } else {
                        index.is_endpoint_pin(pi)
                    };
                    if ok && let Some(nid) = index.all_pins[pi].pin_ref.node {
                        matched_pins.push(nid);
                    }
                }
                if matched_pins.is_empty() {
                    emit_d0102_warning(diags, loc, flag_name, &inst_info.full_name);
                } else {
                    ep.cells.push(inst_id);
                    ep.nodes.extend(matched_pins);
                }
            }
            other => {
                emit_d0102_warning(diags, loc, flag_name, &format!("{other:?}"));
            }
        }
    }
    // Sorted and without duplicates, so that matching is a binary search (AGENTS rule 3.4):
    // an option may name every register of a large design.
    ep.nodes.sort_unstable();
    ep.nodes.dedup();
    ep.cells.sort_unstable();
    ep.cells.dedup();
    ep
}

pub(super) fn resolve_clock_list(
    obj: &Obj,
    index: &DesignIndex,
    clocks: &[CompiledClock],
    loc: &Option<Location>,
    cmd_name: &str,
    diags: &mut Vec<Diag>,
) -> Vec<ClockId> {
    let qctx = QueryContext {
        index,
        clocks,
        hint: NameLookupHint::Clocks,
        exclude_clocks_on_all_inputs: false,
        location: loc.clone(),
    };
    let resolved = resolve_obj(obj, &qctx, diags);
    let mut out = Vec::new();
    for r in resolved {
        match r {
            ResolvedObj::Clock(cid) => {
                if !out.contains(&cid) {
                    out.push(cid);
                }
            }
            other => {
                emit_d0102_error(diags, loc, cmd_name, "clock", &other);
            }
        }
    }
    out
}

pub(super) fn resolve_any_objs(
    obj: &Obj,
    index: &DesignIndex,
    clocks: &[CompiledClock],
    loc: &Option<Location>,
    diags: &mut Vec<Diag>,
) -> Vec<ResolvedObj> {
    let qctx = QueryContext {
        index,
        clocks,
        hint: NameLookupHint::Any,
        exclude_clocks_on_all_inputs: false,
        location: loc.clone(),
    };
    resolve_obj(obj, &qctx, diags)
}

pub(super) fn upsert_pin_uncertainty(
    list: &mut Vec<PinClockUncertainty>,
    node: NodeId,
    setup_ns: Option<f64>,
    hold_ns: Option<f64>,
) {
    if let Some(entry) = list.iter_mut().find(|u| u.node == node) {
        if let Some(s) = setup_ns {
            entry.setup_ns = Some(s);
        }
        if let Some(h) = hold_ns {
            entry.hold_ns = Some(h);
        }
    } else {
        list.push(PinClockUncertainty {
            node,
            setup_ns,
            hold_ns,
        });
    }
}

pub(super) fn apply_scale(val: f64, scale: f64) -> f64 {
    let r = if (scale - 1.0).abs() <= 1e-15 {
        val
    } else if scale > 0.0 && scale < 1.0 {
        let inv = (1.0 / scale).round();
        if inv > 0.0 && (1.0 / inv - scale).abs() <= 1e-15 {
            val / inv
        } else {
            val * scale
        }
    } else {
        val * scale
    };
    if r == 0.0 { 0.0 } else { r }
}

fn operating_condition_exists(
    corners: &[CornerLibs<'_>],
    cond_name: &str,
    lib_filter: Option<&str>,
) -> bool {
    let all_libs: Vec<_> = corners.iter().flat_map(|c| c.libs.iter()).collect();
    if all_libs.is_empty() {
        return true;
    }
    for lib in &all_libs {
        if let Some(want_lib) = lib_filter
            && lib.name != want_lib
        {
            continue;
        }
        if lib.name == cond_name
            || lib.default_operating_conditions.as_deref() == Some(cond_name)
            || lib.operating_conditions.contains_key(cond_name)
        {
            return true;
        }
    }
    all_libs.iter().any(|lib| {
        lib.name == cond_name
            || lib.default_operating_conditions.as_deref() == Some(cond_name)
            || lib.operating_conditions.contains_key(cond_name)
    })
}

pub(super) fn cmd_location(sdc: &Sdc, cmd: &Cmd) -> Option<Location> {
    let file = sdc
        .sources
        .get(cmd.source as usize)
        .map(|s| s.path.clone())
        .unwrap_or_else(|| "<sdc>".to_string());
    if cmd.line == 0 && cmd.col == 0 && sdc.sources.is_empty() {
        None
    } else {
        Some(Location {
            file,
            line: cmd.line.max(1),
            column: cmd.col.max(1),
        })
    }
}

pub(super) fn with_loc(mut d: Diag, loc: &Option<Location>) -> Diag {
    if let Some(l) = loc {
        d = d.at(l.file.clone(), l.line, l.column);
    }
    d
}

pub(super) fn has_diag_at(diags: &[Diag], code: &str, loc: &Option<Location>) -> bool {
    diags.iter().any(|d| d.code == code && &d.location == loc)
}

pub(super) fn emit_d0011(
    diags: &mut Vec<Diag>,
    loc: &Option<Location>,
    cmd_name: &str,
    opt_name: &str,
) {
    diags.push(with_loc(
        Diag::error(
            "D0011",
            format!("option '{opt_name}' on '{cmd_name}' is scheduled for Phase 2b"),
        ),
        loc,
    ));
}

pub(super) fn emit_d0102_error(
    diags: &mut Vec<Diag>,
    loc: &Option<Location>,
    cmd_name: &str,
    expected: &str,
    got: &ResolvedObj,
) {
    diags.push(with_loc(
        Diag::error(
            "D0102",
            format!("'{cmd_name}' expects {expected}, got {got:?}"),
        ),
        loc,
    ));
}

pub(super) fn emit_d0102_warning(
    diags: &mut Vec<Diag>,
    loc: &Option<Location>,
    flag_name: &str,
    obj_name: &str,
) {
    diags.push(with_loc(
        Diag::warning(
            "D0102",
            format!(
                "ignoring '{obj_name}' in '{flag_name}': object is not a valid timing path endpoint/startpoint"
            ),
        ),
        loc,
    ));
}

pub(super) fn min_max_indices(scope: MinMaxScope) -> Vec<usize> {
    match scope {
        MinMaxScope::Min => vec![An::Early as usize],
        MinMaxScope::Max => vec![An::Late as usize],
        MinMaxScope::Both => vec![An::Early as usize, An::Late as usize],
    }
}

pub(super) fn edge_indices(scope: EdgeScope) -> Vec<usize> {
    match scope {
        EdgeScope::Rise => vec![Tr::Rise as usize],
        EdgeScope::Fall => vec![Tr::Fall as usize],
        EdgeScope::Both => vec![Tr::Rise as usize, Tr::Fall as usize],
    }
}

pub(super) fn early_late_indices(scope: EarlyLateScope) -> Vec<usize> {
    match scope {
        EarlyLateScope::Early => vec![An::Early as usize],
        EarlyLateScope::Late => vec![An::Late as usize],
        EarlyLateScope::Both => vec![An::Early as usize, An::Late as usize],
    }
}
