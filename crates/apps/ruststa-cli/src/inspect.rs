// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa inspect` command (`STA-REQ-0540` to `STA-REQ-0549`).

use rusteda_core::netlist::{ANONYMOUS, InstRec, NetRec, PortRec, SCALAR};
use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, Handle, JsonObject, Revision, Severity, Workspace, exit,
};
use ruststa::engine::store::{InspectedInstance, InspectedNode, StaStore, StaStoreView, canon_f64};
use ruststa::engine::types::NodeId;

use crate::args::InspectArgs;
use crate::ws_io::{
    envelope_headers_from_store, map_store_error, node_handle_string, resolve_store_corner,
    strip_underscore_handle_suggestion,
};

fn finite_opt(v: f64) -> Option<f64> {
    v.is_finite().then(|| canon_f64(v))
}

fn worst_late(grid: [[f64; 2]; 2]) -> Option<f64> {
    match (finite_opt(grid[0][1]), finite_opt(grid[1][1])) {
        (Some(a), Some(b)) => Some(canon_f64(a.max(b))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn worst_early(grid: [[f64; 2]; 2]) -> Option<f64> {
    match (finite_opt(grid[0][0]), finite_opt(grid[1][0])) {
        (Some(a), Some(b)) => Some(canon_f64(a.min(b))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The required time of bound `an` (1: late, the setup required time; 0: early, the hold
/// required time) at the transition of the worst slack of that bound, which is the required
/// time of the worst path to the pin (`STA-REQ-0812`). Without a slack, the transition with a
/// required time is used: rise first.
pub(crate) fn required_at_worst(
    required: [[f64; 2]; 2],
    slack: [[f64; 2]; 2],
    an: usize,
) -> Option<f64> {
    let mut best: Option<(f64, f64)> = None;
    for tr in 0..2 {
        let (s, r) = (slack[tr][an], required[tr][an]);
        if !r.is_finite() {
            continue;
        }
        let key = if s.is_finite() { s } else { f64::INFINITY };
        if best.is_none_or(|(b, _)| key < b) {
            best = Some((key, r));
        }
    }
    best.map(|(_, r)| canon_f64(r))
}

fn format_inspected_node(n: &InspectedNode, num_port_bits: u32) -> Result<JsonObject, Error> {
    let mut worst_late_arr: Option<f64> = None;
    let mut worst_early_arr: Option<f64> = None;
    for t in &n.tags {
        if let Some(v) = worst_late(t.arrival) {
            worst_late_arr = Some(worst_late_arr.map_or(v, |p| p.max(v)));
        }
        if let Some(v) = worst_early(t.arrival) {
            worst_early_arr = Some(worst_early_arr.map_or(v, |p| p.min(v)));
        }
    }

    let setup_req = required_at_worst(n.required, n.slack, 1);
    let hold_req = required_at_worst(n.required, n.slack, 0);
    let setup_slack = n.endpoint_setup_slack.and_then(finite_opt).or_else(|| {
        worst_early([
            [n.slack[0][1], n.slack[0][1]],
            [n.slack[1][1], n.slack[1][1]],
        ])
    });
    let hold_slack = n.endpoint_hold_slack.and_then(finite_opt).or_else(|| {
        worst_early([
            [n.slack[0][0], n.slack[0][0]],
            [n.slack[1][0], n.slack[1][0]],
        ])
    });

    let mut obj = JsonObject::new();
    obj.insert("kind", &if n.is_port { "port" } else { "pin" })?;
    obj.insert("name", &n.name)?;
    obj.insert("handle", &node_handle_string(n.node, num_port_bits))?;
    obj.insert("corner", &n.corner_name)?;
    obj.insert("is_endpoint", &n.is_endpoint)?;
    obj.insert("arrival_late_ns", &worst_late_arr)?;
    obj.insert("arrival_early_ns", &worst_early_arr)?;
    obj.insert("slew_late_ns", &worst_late(n.slew))?;
    obj.insert("slew_early_ns", &worst_early(n.slew))?;
    obj.insert("required_ns", &setup_req)?;
    obj.insert("required_setup_ns", &setup_req)?;
    obj.insert("required_hold_ns", &hold_req)?;
    obj.insert("slack_ns", &setup_slack)?;
    obj.insert("hold_slack_ns", &hold_slack)?;
    Ok(obj)
}

fn format_inspected_instance(
    inst: &InspectedInstance,
    inst_handle: Option<&str>,
    num_port_bits: u32,
) -> Result<JsonObject, Error> {
    let mut pins_json = Vec::with_capacity(inst.pins.len());
    for p in &inst.pins {
        let mut p_obj = JsonObject::new();
        p_obj.insert("pin", &p.pin_name)?;
        p_obj.insert("full_name", &p.full_name)?;
        p_obj.insert("handle", &node_handle_string(p.node, num_port_bits))?;
        p_obj.insert("arrival_late_ns", &worst_late(p.arrival))?;
        p_obj.insert("arrival_early_ns", &worst_early(p.arrival))?;
        p_obj.insert("slew_late_ns", &worst_late(p.slew))?;
        p_obj.insert("slew_early_ns", &worst_early(p.slew))?;
        let setup_req = required_at_worst(p.required, p.slack, 1);
        p_obj.insert("required_ns", &setup_req)?;
        p_obj.insert("required_setup_ns", &setup_req)?;
        p_obj.insert(
            "required_hold_ns",
            &required_at_worst(p.required, p.slack, 0),
        )?;
        p_obj.insert(
            "slack_ns",
            &worst_early([
                [p.slack[0][1], p.slack[0][1]],
                [p.slack[1][1], p.slack[1][1]],
            ]),
        )?;
        p_obj.insert(
            "hold_slack_ns",
            &worst_early([
                [p.slack[0][0], p.slack[0][0]],
                [p.slack[1][0], p.slack[1][0]],
            ]),
        )?;
        pins_json.push(p_obj);
    }

    let mut obj = JsonObject::new();
    obj.insert("kind", &"instance")?;
    obj.insert("name", &inst.inst_name)?;
    obj.insert("handle", &inst_handle)?;
    obj.insert("cell", &inst.cell_name)?;
    obj.insert("corner", &inst.corner_name)?;
    obj.insert("pins", &pins_json)?;
    Ok(obj)
}

fn rbin_get_str(rbin: &Rbin, str_id: u32) -> Result<&str, Error> {
    let str_bytes = rbin.section_bytes(1)?;
    let str_offs: &[u32] = bytemuck::cast_slice(rbin.section_bytes(2)?);
    let idx = str_id as usize;
    let start = *str_offs
        .get(idx)
        .ok_or_else(|| Error::internal("string id out of bounds in netlist.rbin"))?
        as usize;
    let end = *str_offs
        .get(idx + 1)
        .ok_or_else(|| Error::internal("string offset out of bounds in netlist.rbin"))?
        as usize;
    std::str::from_utf8(
        str_bytes
            .get(start..end)
            .ok_or_else(|| Error::internal("string byte range out of bounds"))?,
    )
    .map_err(|_| Error::internal("invalid UTF-8 in netlist.rbin"))
}

fn resolve_inst_row_name(rbin: &Rbin, inst_row: usize) -> Result<Option<String>, Error> {
    let instances: &[InstRec] = bytemuck::cast_slice(rbin.section_bytes(12)?);
    let Some(rec) = instances.get(inst_row) else {
        return Ok(None);
    };
    Ok(Some(rbin_get_str(rbin, rec.name)?.to_owned()))
}

fn find_inst_handle_by_first_pin(
    rbin: &Rbin,
    first_pin_node: NodeId,
    num_port_bits: u32,
) -> Option<String> {
    if first_pin_node.0 < num_port_bits {
        return None;
    }
    let pin_row = first_pin_node.0 - num_port_bits;
    let inst_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(13).ok()?);
    let inst_row = inst_pin_start
        .partition_point(|&s| s <= pin_row)
        .checked_sub(1)?;
    Some(format!("inst{}", inst_row + 1))
}

fn resolve_pin_or_port_name(
    v: &StaStoreView<'_>,
    rbin: &Rbin,
    node: NodeId,
) -> Result<String, Error> {
    if let Some(name) = crate::ws_io::resolve_node_name(v, node) {
        return Ok(name);
    }
    let num_port_bits = v.meta.num_port_bits;
    if node.0 < num_port_bits {
        let ports: &[PortRec] = bytemuck::cast_slice(rbin.section_bytes(8)?);
        let port_bit_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(9)?);
        let pb = node.0;
        let p_row = port_bit_start
            .partition_point(|&s| s <= pb)
            .saturating_sub(1);
        let prec = ports
            .get(p_row)
            .ok_or_else(|| Error::internal("port bit out of bounds"))?;
        let pname = rbin_get_str(rbin, prec.name)?;
        if prec.is_bus == 0 {
            return Ok(pname.to_owned());
        }
        let offset = (pb - port_bit_start[p_row]) as i32;
        let bit = if prec.msb >= prec.lsb {
            prec.msb - offset
        } else {
            prec.msb + offset
        };
        Ok(format!("{pname}[{bit}]"))
    } else {
        let pin_row = (node.0 - num_port_bits) as usize;
        let cell_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(5)?);
        let cell_pin_name: &[u32] = bytemuck::cast_slice(rbin.section_bytes(6)?);
        let instances: &[InstRec] = bytemuck::cast_slice(rbin.section_bytes(12)?);
        let inst_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(13)?);
        let pin_cellpin: &[u32] = bytemuck::cast_slice(rbin.section_bytes(14)?);
        let inst_row = inst_pin_start
            .partition_point(|&s| (s as usize) <= pin_row)
            .saturating_sub(1);
        let irec = instances
            .get(inst_row)
            .ok_or_else(|| Error::internal("instance row out of bounds"))?;
        let iname = rbin_get_str(rbin, irec.name)?;
        let cp_idx = cell_pin_start[irec.cell as usize] as usize + pin_cellpin[pin_row] as usize;
        let pname = rbin_get_str(rbin, cell_pin_name[cp_idx])?;
        Ok(format!("{iname}/{pname}"))
    }
}

fn format_net_name(rbin: &Rbin, net_row: usize, rec: &NetRec) -> Result<String, Error> {
    if rec.name == ANONYMOUS {
        return Ok(format!("_net{net_row}"));
    }
    let base = rbin_get_str(rbin, rec.name)?;
    if rec.index == SCALAR {
        Ok(base.to_owned())
    } else {
        Ok(format!("{base}[{}]", rec.index))
    }
}

fn inspect_net_by_row(
    store: &StaStore,
    v: &StaStoreView<'_>,
    rbin: &Rbin,
    net_row: usize,
    corner_idx: usize,
    corner_name: &str,
) -> Result<JsonObject, Error> {
    let nets: &[NetRec] = bytemuck::cast_slice(rbin.section_bytes(11)?);
    let Some(nrec) = nets.get(net_row) else {
        return Err(Error::UnknownHandle {
            handle: format!("net{}", net_row + 1),
            reason: format!("out of range (design has {} nets)", nets.len()),
        });
    };
    let net_name = format_net_name(rbin, net_row, nrec)?;
    let net_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(17)?);
    let net_pins: &[u32] = bytemuck::cast_slice(rbin.section_bytes(18)?);
    let start = net_pin_start.get(net_row).copied().unwrap_or(0) as usize;
    let end = net_pin_start.get(net_row + 1).copied().unwrap_or(0) as usize;
    let connected_pin_rows = net_pins.get(start..end).unwrap_or(&[]);

    let num_port_bits = v.meta.num_port_bits;
    let mut pins_json = Vec::with_capacity(connected_pin_rows.len());
    for &pin_row in connected_pin_rows {
        let node = NodeId(num_port_bits + pin_row);
        let pname = resolve_pin_or_port_name(v, rbin, node)?;
        if let Ok(insp) = store.inspect_pin(&pname, corner_idx) {
            pins_json.push(format_inspected_node(&insp, num_port_bits)?);
        }
    }

    let mut obj = JsonObject::new();
    obj.insert("kind", &"net")?;
    obj.insert("name", &net_name)?;
    obj.insert("handle", &format!("net{}", net_row + 1))?;
    obj.insert("corner", &corner_name)?;
    obj.insert("connected_pins", &(connected_pin_rows.len() as u32))?;
    obj.insert("pins", &pins_json)?;
    Ok(obj)
}

fn ensure_netlist<'a>(slot: &'a mut Option<Rbin>, revision: &Revision) -> Result<&'a Rbin, Error> {
    if slot.is_none() {
        *slot = Some(revision.open_rbin("netlist", "NETLIST", 1..=1)?);
    }
    Ok(slot.as_ref().expect("just initialized"))
}

#[allow(clippy::too_many_arguments)]
fn inspect_single_handle(
    ws: &Workspace,
    revision: &Revision,
    store: &StaStore,
    v: &StaStoreView<'_>,
    netlist_rbin: &mut Option<Rbin>,
    raw_target: &str,
    corner_idx: usize,
    corner_name: &str,
    out_items: &mut Vec<JsonObject>,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    if let Some(sugg) = strip_underscore_handle_suggestion(raw_target) {
        diags.push(
            Diagnostic::new(
                Severity::Error,
                DiagCode::of("T0007"),
                format!("unknown handle '{raw_target}'"),
            )
            .with_help(format!("use '{sugg}' without an underscore")),
        );
        return Err(Error::UnknownHandle {
            handle: raw_target.to_string(),
            reason: format!("handles do not use underscores; did you mean '{sugg}'?"),
        });
    }

    let h = Handle::parse(raw_target).map_err(|e| {
        diags.push(Diagnostic::new(
            Severity::Error,
            DiagCode::of("T0007"),
            format!("invalid handle '{raw_target}': {}", e.reason),
        ));
        Error::UnknownHandle {
            handle: raw_target.to_string(),
            reason: e.reason,
        }
    })?;

    let num_port_bits = v.meta.num_port_bits;
    match h.prefix() {
        "sel" => {
            let state = ws.resolve_selection(raw_target, revision.rev())?;
            for inst_h in &state.selected_instances {
                inspect_single_handle(
                    ws,
                    revision,
                    store,
                    v,
                    netlist_rbin,
                    inst_h,
                    corner_idx,
                    corner_name,
                    out_items,
                    diags,
                )?;
            }
            for path_h in &state.selected_paths {
                inspect_single_handle(
                    ws,
                    revision,
                    store,
                    v,
                    netlist_rbin,
                    path_h,
                    corner_idx,
                    corner_name,
                    out_items,
                    diags,
                )?;
            }
        }
        "inst" => {
            let inst_row = (h.index() - 1) as usize;
            let rbin = ensure_netlist(netlist_rbin, revision)?;
            let Some(inst_name) = resolve_inst_row_name(rbin, inst_row)? else {
                let msg = format!("unknown instance handle '{raw_target}'");
                diags.push(Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("T0007"),
                    msg.clone(),
                ));
                return Err(Error::UnknownHandle {
                    handle: raw_target.to_string(),
                    reason: msg,
                });
            };
            let insp = store
                .inspect_instance(&inst_name, corner_idx)
                .map_err(map_store_error)?;
            out_items.push(format_inspected_instance(
                &insp,
                Some(raw_target),
                num_port_bits,
            )?);
        }
        "pin" => {
            let pin_row = h.index() - 1;
            let num_pins = v.meta.num_nodes.saturating_sub(num_port_bits);
            if pin_row >= num_pins {
                let msg = format!("unknown pin handle '{raw_target}'");
                diags.push(Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("T0007"),
                    msg.clone(),
                ));
                return Err(Error::UnknownHandle {
                    handle: raw_target.to_string(),
                    reason: msg,
                });
            }
            let rbin = ensure_netlist(netlist_rbin, revision)?;
            let node = NodeId(num_port_bits + pin_row);
            let pname = resolve_pin_or_port_name(v, rbin, node)?;
            let insp = store
                .inspect_pin(&pname, corner_idx)
                .map_err(map_store_error)?;
            out_items.push(format_inspected_node(&insp, num_port_bits)?);
        }
        "port" => {
            let port_row = h.index() - 1;
            if port_row >= num_port_bits {
                let msg = format!("unknown port handle '{raw_target}'");
                diags.push(Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("T0007"),
                    msg.clone(),
                ));
                return Err(Error::UnknownHandle {
                    handle: raw_target.to_string(),
                    reason: msg,
                });
            }
            let rbin = ensure_netlist(netlist_rbin, revision)?;
            let node = NodeId(port_row);
            let pname = resolve_pin_or_port_name(v, rbin, node)?;
            let insp = store
                .inspect_port(&pname, corner_idx)
                .map_err(map_store_error)?;
            out_items.push(format_inspected_node(&insp, num_port_bits)?);
        }
        "net" => {
            let net_row = (h.index() - 1) as usize;
            let rbin = ensure_netlist(netlist_rbin, revision)?;
            let obj = inspect_net_by_row(store, v, rbin, net_row, corner_idx, corner_name)
                .inspect_err(|_| {
                    diags.push(Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("T0007"),
                        format!("unknown net handle '{raw_target}'"),
                    ));
                })?;
            out_items.push(obj);
        }
        "path" => {
            let rank_idx = (h.index() - 1) as usize;
            let p = store.query_path_handle(rank_idx).map_err(|e| {
                diags.push(Diagnostic::new(
                    Severity::Error,
                    DiagCode::of("T0007"),
                    format!("unknown path handle '{raw_target}'"),
                ));
                map_store_error(e)
            })?;
            let startpoint = p
                .points
                .first()
                .map_or_else(|| p.endpoint_name.clone(), |pt| pt.name.clone());
            let mut obj = JsonObject::new();
            obj.insert("kind", &"path")?;
            obj.insert("handle", &format!("path{}", p.rank_idx + 1))?;
            obj.insert("check_kind", &p.check_kind)?;
            obj.insert("corner", &p.corner_name)?;
            obj.insert("startpoint", &startpoint)?;
            obj.insert("endpoint", &p.endpoint_name)?;
            obj.insert("slack_ns", &canon_f64(p.slack))?;
            obj.insert("arrival_ns", &canon_f64(p.arrival))?;
            obj.insert("required_ns", &canon_f64(p.required))?;
            out_items.push(obj);
        }
        _ => {
            return Err(Error::usage(format!(
                "unsupported handle '{raw_target}' for ruststa inspect"
            )));
        }
    }
    Ok(())
}

/// Executes `ruststa inspect`.
pub fn exec_inspect(
    ws: &Workspace,
    ws_arg: &str,
    args: &InspectArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    let by_name_count = usize::from(args.inst.is_some())
        + usize::from(args.net.is_some())
        + usize::from(args.pin.is_some());
    if args.targets.is_empty() && by_name_count == 0 {
        return Err(Error::usage(
            "ruststa inspect requires --target <HANDLE>, --inst <NAME>, --net <NAME>, or --pin <NAME>",
        ));
    }
    if !args.targets.is_empty() && by_name_count > 0 {
        return Err(Error::usage(
            "--target cannot be combined with --inst, --net, or --pin",
        ));
    }

    let rev = ws.resolve_query(&args.rev)?;
    let revision = ws.revision(rev)?;
    let store = StaStore::open_rev(&revision).map_err(map_store_error)?;
    let v = store.view().map_err(map_store_error)?;
    let (corner_idx, corner_name) = resolve_store_corner(&store, args.corner.as_deref(), diags)?;
    let num_port_bits = v.meta.num_port_bits;

    let mut items: Vec<JsonObject> = Vec::new();
    let mut netlist_rbin: Option<Rbin> = None;

    for raw_target in &args.targets {
        inspect_single_handle(
            ws,
            &revision,
            &store,
            &v,
            &mut netlist_rbin,
            raw_target,
            corner_idx,
            &corner_name,
            &mut items,
            diags,
        )?;
    }

    if let Some(inst_name) = &args.inst {
        let insp = store.inspect_instance(inst_name, corner_idx).map_err(|e| {
            diags.push(Diagnostic::new(
                Severity::Error,
                DiagCode::of("T0008"),
                format!("unknown instance '{inst_name}'"),
            ));
            map_store_error(e)
        })?;
        let rbin = ensure_netlist(&mut netlist_rbin, &revision)?;
        let inst_handle = insp
            .pins
            .first()
            .and_then(|p| find_inst_handle_by_first_pin(rbin, p.node, num_port_bits));
        items.push(format_inspected_instance(
            &insp,
            inst_handle.as_deref(),
            num_port_bits,
        )?);
    }

    if let Some(pin_name) = &args.pin {
        let insp = if pin_name.contains('/') {
            store.inspect_pin(pin_name, corner_idx)
        } else {
            store
                .inspect_port(pin_name, corner_idx)
                .or_else(|_| store.inspect_pin(pin_name, corner_idx))
        }
        .map_err(|e| {
            diags.push(Diagnostic::new(
                Severity::Error,
                DiagCode::of("T0008"),
                format!("unknown pin or port '{pin_name}'"),
            ));
            map_store_error(e)
        })?;
        items.push(format_inspected_node(&insp, num_port_bits)?);
    }

    if let Some(net_name) = &args.net {
        let rbin = ensure_netlist(&mut netlist_rbin, &revision)?;
        let nets: &[NetRec] = bytemuck::cast_slice(rbin.section_bytes(11)?);
        let mut found_row = None;
        for (nr, rec) in nets.iter().enumerate() {
            if let Ok(cand) = format_net_name(rbin, nr, rec)
                && cand == *net_name
            {
                found_row = Some(nr);
                break;
            }
        }
        let Some(net_row) = found_row else {
            let msg = format!("unknown net '{net_name}'");
            diags.push(Diagnostic::new(
                Severity::Error,
                DiagCode::of("T0008"),
                msg.clone(),
            ));
            return Err(Error::UnknownHandle {
                handle: net_name.clone(),
                reason: msg,
            });
        };
        items.push(inspect_net_by_row(
            &store,
            &v,
            rbin,
            net_row,
            corner_idx,
            &corner_name,
        )?);
    }

    let (scope, comp, metrics) = envelope_headers_from_store(&store, args.corner.as_deref())?;
    let mut env = Envelope::new("ruststa inspect");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(rev);
    env.set_scope(scope)?;
    env.set_completeness(comp)?;
    env.set_metrics(metrics)?;
    if let Some(first) = items.first() {
        env.set("object", first)?;
    } else {
        env.set_raw("object", rusteda_workspace::RawJson::null())?;
    }
    env.set("objects", &items)?;

    let rev_str = rev.to_string();
    env.push_next_action(&[
        "ruststa",
        "report",
        "-w",
        ws_arg,
        "--rev",
        &rev_str,
        "--section",
        "summary",
    ]);

    let mut lines = Vec::new();
    for item in &items {
        lines.push(format!("inspect: {}", item.to_raw().as_str()));
    }
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok((env, lines, exit::OK))
}
