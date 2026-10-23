// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! `ruststa path` command (`STA-REQ-0530` to `STA-REQ-0539`).

use rusteda_workspace::text::next_line;
use rusteda_workspace::{
    DiagCode, Diagnostic, Envelope, Error, Handle, JsonObject, Revision, Severity, Workspace, exit,
};
use ruststa::engine::graph::CheckKind;
use ruststa::engine::store::{PathPointKind, QueriedPath, StaStore, StaStoreView, canon_f64};
use ruststa::engine::types::{NodeId, Tr};

use crate::args::{PathArgs, PathKind};
use crate::ws_io::{
    envelope_headers_from_store, map_store_error, node_handle_string, path_delay_breakdown,
    resolve_store_corner, strip_underscore_handle_suggestion,
};

fn format_path_json(p: &QueriedPath, v: &StaStoreView<'_>) -> Result<JsonObject, Error> {
    let num_port_bits = v.meta.num_port_bits;
    let handle = format!("path{}", p.rank_idx + 1);
    let startpoint = p
        .points
        .first()
        .map_or_else(|| p.endpoint_name.clone(), |pt| pt.name.clone());
    let (logic_levels, cell_delay_ns, wire_delay_ns, cell_pct, wire_pct) = path_delay_breakdown(p);

    let mut points_json = Vec::with_capacity(p.points.len());
    for pt in &p.points {
        let pt_handle = node_handle_string(pt.node, num_port_bits);
        let (inst_opt, cell_opt) = if pt.node.0 < num_port_bits {
            (None::<String>, None::<String>)
        } else if let Some((inst_name, _)) = pt.name.rsplit_once('/') {
            let cell_name = v
                .inst_hash
                .lookup(inst_name, &v.strings)
                .ok()
                .flatten()
                .and_then(|ie| v.strings.get(ie.cell_str).ok())
                .map(str::to_owned);
            (Some(inst_name.to_owned()), cell_name)
        } else {
            (None, None)
        };

        let arc_str = match pt.kind {
            PathPointKind::ClockLaunch => "launch",
            PathPointKind::Port => "input",
            PathPointKind::Cell => "cell",
            PathPointKind::Net => "net",
        };
        let tr_str = match pt.tr {
            Tr::Rise => "rise",
            Tr::Fall => "fall",
        };

        let mut pt_obj = JsonObject::new();
        pt_obj.insert("pin", &pt.name)?;
        pt_obj.insert("handle", &pt_handle)?;
        pt_obj.insert("inst", &inst_opt)?;
        pt_obj.insert("cell", &cell_opt)?;
        pt_obj.insert("arc", &arc_str)?;
        pt_obj.insert("transition", &tr_str)?;
        pt_obj.insert("incr_ns", &canon_f64(pt.incr_delay))?;
        pt_obj.insert("arrival_ns", &canon_f64(pt.arrival))?;
        pt_obj.insert("slew_ns", &canon_f64(pt.slew))?;
        points_json.push(pt_obj);
    }

    let mut obj = JsonObject::new();
    obj.insert("handle", &handle)?;
    obj.insert("rank", &(p.rank_idx as u32 + 1))?;
    obj.insert("kind", &p.check_kind)?;
    obj.insert("corner", &p.corner_name)?;
    obj.insert("path_group", &p.path_group)?;
    obj.insert("startpoint", &startpoint)?;
    obj.insert("launch_clock", &p.launch_clock)?;
    obj.insert("endpoint", &p.endpoint_name)?;
    obj.insert("capture_clock", &p.capture_clock)?;
    obj.insert("arrival_ns", &canon_f64(p.arrival))?;
    obj.insert("required_ns", &canon_f64(p.required))?;
    obj.insert("slack_ns", &canon_f64(p.slack))?;
    obj.insert("check_value_ns", &canon_f64(p.check_value))?;
    obj.insert("logic_levels", &logic_levels)?;
    obj.insert("cell_delay_ns", &cell_delay_ns)?;
    obj.insert("wire_delay_ns", &wire_delay_ns)?;
    obj.insert("cell_delay_pct", &cell_pct)?;
    obj.insert("wire_delay_pct", &wire_pct)?;
    obj.insert("points", &points_json)?;
    Ok(obj)
}

fn resolve_node_name_from_store(
    v: &StaStoreView<'_>,
    revision: &Revision,
    node: NodeId,
) -> Result<String, Error> {
    if let Ok(ep_idx) = v.endpoints.binary_search_by_key(&node.0, |e| e.data_node)
        && let Some(nb) = v.endpoints[ep_idx].name_bucket()
        && let Ok(name) = v.name_hash.format_bucket(nb, &v.strings)
    {
        return Ok(name);
    }
    if let Some(name) = crate::ws_io::resolve_node_name(v, node) {
        return Ok(name);
    }
    // Fallback to O(1) lookup in netlist.rbin sections without full validation.
    let rbin = revision.open_rbin("netlist", "NETLIST", 1..=1)?;
    let str_bytes = rbin.section_bytes(1)?;
    let str_offs: &[u32] = bytemuck::cast_slice(rbin.section_bytes(2)?);
    let get_str = |id: u32| -> Option<&str> {
        let i = id as usize;
        let start = *str_offs.get(i)? as usize;
        let end = *str_offs.get(i + 1)? as usize;
        std::str::from_utf8(str_bytes.get(start..end)?).ok()
    };

    let num_port_bits = v.meta.num_port_bits;
    if node.0 < num_port_bits {
        let ports: &[rusteda_core::netlist::PortRec] = bytemuck::cast_slice(rbin.section_bytes(8)?);
        let port_bit_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(9)?);
        let pb = node.0;
        let p_row = port_bit_start
            .partition_point(|&s| s <= pb)
            .saturating_sub(1);
        if let Some(prec) = ports.get(p_row)
            && let Some(pname) = get_str(prec.name)
        {
            if prec.is_bus == 0 {
                return Ok(pname.to_owned());
            }
            let offset = (pb - port_bit_start[p_row]) as i32;
            let bit = if prec.msb >= prec.lsb {
                prec.msb - offset
            } else {
                prec.msb + offset
            };
            return Ok(format!("{pname}[{bit}]"));
        }
    } else {
        let pin_row = (node.0 - num_port_bits) as usize;
        let cells: &[u32] = bytemuck::cast_slice(rbin.section_bytes(4)?);
        let cell_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(5)?);
        let cell_pin_name: &[u32] = bytemuck::cast_slice(rbin.section_bytes(6)?);
        let instances: &[rusteda_core::netlist::InstRec] =
            bytemuck::cast_slice(rbin.section_bytes(12)?);
        let inst_pin_start: &[u32] = bytemuck::cast_slice(rbin.section_bytes(13)?);
        let pin_cellpin: &[u32] = bytemuck::cast_slice(rbin.section_bytes(14)?);
        if pin_row < pin_cellpin.len() {
            let inst_row = inst_pin_start
                .partition_point(|&s| (s as usize) <= pin_row)
                .saturating_sub(1);
            if let Some(irec) = instances.get(inst_row) {
                let iname = get_str(irec.name).unwrap_or("");
                let c_row = irec.cell as usize;
                let cp_idx = cell_pin_start.get(c_row).copied().unwrap_or(0) as usize
                    + pin_cellpin[pin_row] as usize;
                let _ = cells;
                let pname = cell_pin_name
                    .get(cp_idx)
                    .and_then(|&sid| get_str(sid))
                    .unwrap_or("");
                return Ok(format!("{iname}/{pname}"));
            }
        }
    }
    Err(Error::UnknownHandle {
        handle: node_handle_string(node, num_port_bits),
        reason: "could not resolve node name".to_string(),
    })
}

/// Executes `ruststa path`.
pub fn exec_path(
    ws: &Workspace,
    ws_arg: &str,
    args: &PathArgs,
    diags: &mut Vec<Diagnostic>,
) -> Result<(Envelope, Vec<String>, i32), Error> {
    if args.targets.is_empty() && args.to.is_empty() {
        return Err(Error::usage(
            "ruststa path requires at least one --target <HANDLE> or --to <NAME>",
        ));
    }
    if !args.targets.is_empty() && !args.to.is_empty() {
        return Err(Error::usage(
            "--target and --to cannot be combined in the same ruststa path invocation",
        ));
    }

    let rev = ws.resolve_query(&args.rev)?;
    let revision = ws.revision(rev)?;
    let store = StaStore::open_rev(&revision).map_err(map_store_error)?;
    let v = store.view().map_err(map_store_error)?;
    let (corner_idx, _) = resolve_store_corner(&store, args.corner.as_deref(), diags)?;

    let check_filter: Option<CheckKind> = match args.kind {
        PathKind::Setup => Some(CheckKind::Setup),
        PathKind::Hold => Some(CheckKind::Hold),
        PathKind::Both => None,
    };

    let mut queried_paths: Vec<QueriedPath> = Vec::new();

    for raw_target in &args.targets {
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
                handle: raw_target.clone(),
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
                handle: raw_target.clone(),
                reason: e.reason,
            }
        })?;

        match h.prefix() {
            "path" => {
                let rank_idx = (h.index() - 1) as usize;
                let p = store.query_path_handle(rank_idx).map_err(|e| {
                    diags.push(Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("T0007"),
                        format!("unknown handle '{raw_target}': {e}"),
                    ));
                    map_store_error(e)
                })?;
                queried_paths.push(p);
            }
            "pin" => {
                let pin_row = h.index() - 1;
                let num_pins = v.meta.num_nodes.saturating_sub(v.meta.num_port_bits);
                if pin_row >= num_pins {
                    let msg = format!(
                        "handle '{raw_target}' is out of range (design has {num_pins} pins)"
                    );
                    diags.push(Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("T0007"),
                        msg.clone(),
                    ));
                    return Err(Error::UnknownHandle {
                        handle: raw_target.clone(),
                        reason: msg,
                    });
                }
                let node = NodeId(v.meta.num_port_bits + pin_row);
                let pin_name = resolve_node_name_from_store(&v, &revision, node)?;
                let mut paths = store
                    .query_path_to(&pin_name, Some(corner_idx), check_filter)
                    .map_err(map_store_error)?;
                queried_paths.append(&mut paths);
            }
            "port" => {
                let port_row = h.index() - 1;
                let num_ports = v.meta.num_port_bits;
                if port_row >= num_ports {
                    let msg = format!(
                        "handle '{raw_target}' is out of range (design has {num_ports} port bits)"
                    );
                    diags.push(Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("T0007"),
                        msg.clone(),
                    ));
                    return Err(Error::UnknownHandle {
                        handle: raw_target.clone(),
                        reason: msg,
                    });
                }
                let node = NodeId(port_row);
                let port_name = resolve_node_name_from_store(&v, &revision, node)?;
                let mut paths = store
                    .query_path_to(&port_name, Some(corner_idx), check_filter)
                    .map_err(map_store_error)?;
                queried_paths.append(&mut paths);
            }
            _ => {
                return Err(Error::usage(format!(
                    "ruststa path --target expects path<N>, pin<N>, or port<N>, got '{raw_target}'"
                )));
            }
        }
    }

    for endpoint_name in &args.to {
        let mut paths = store
            .query_path_to(endpoint_name, Some(corner_idx), check_filter)
            .map_err(|e| {
                if matches!(
                    e,
                    ruststa::engine::store::StaStoreError::UnknownPin { .. }
                        | ruststa::engine::store::StaStoreError::UnknownPort { .. }
                ) {
                    diags.push(Diagnostic::new(
                        Severity::Error,
                        DiagCode::of("T0008"),
                        format!("unknown endpoint '{endpoint_name}'"),
                    ));
                }
                map_store_error(e)
            })?;
        queried_paths.append(&mut paths);
    }

    let mut paths_json = Vec::with_capacity(queried_paths.len());
    for p in &queried_paths {
        paths_json.push(format_path_json(p, &v)?);
    }

    let (scope, comp, metrics) = envelope_headers_from_store(&store, args.corner.as_deref())?;
    let mut env = Envelope::new("ruststa path");
    env.workspace = Some(ws_arg.to_string());
    env.rev = Some(rev);
    env.set_scope(scope)?;
    env.set_completeness(comp)?;
    env.set_metrics(metrics)?;
    if let Some(first) = paths_json.first() {
        env.set("path", first)?;
    } else {
        env.set_raw("path", rusteda_workspace::RawJson::null())?;
    }
    env.set("paths", &paths_json)?;

    let rev_str = rev.to_string();
    if let Some(first_p) = queried_paths.first() {
        env.push_next_action(&[
            "ruststa",
            "inspect",
            "-w",
            ws_arg,
            "--rev",
            &rev_str,
            "--pin",
            &first_p.endpoint_name,
        ]);
    }

    let mut lines = Vec::new();
    for p in &queried_paths {
        let handle = format!("path{}", p.rank_idx + 1);
        let startpoint = p
            .points
            .first()
            .map_or_else(|| p.endpoint_name.as_str(), |pt| pt.name.as_str());
        if args.exact {
            lines.push(format!(
                "{handle}  kind={}  corner={}  startpoint={startpoint}  endpoint={}  slack={:?} ns  arrival={:?} ns  required={:?} ns",
                p.check_kind, p.corner_name, p.endpoint_name, p.slack, p.arrival, p.required
            ));
        } else {
            lines.push(format!(
                "{handle}  kind={}  corner={}  startpoint={startpoint}  endpoint={}  slack={:.4} ns  arrival={:.4} ns  required={:.4} ns",
                p.check_kind, p.corner_name, p.endpoint_name, p.slack, p.arrival, p.required
            ));
        }
        for pt in &p.points {
            let tr_ch = match pt.tr {
                Tr::Rise => 'R',
                Tr::Fall => 'F',
            };
            if args.exact {
                lines.push(format!(
                    "  {} ({tr_ch})  incr={:?} ns  arrival={:?} ns  slew={:?} ns",
                    pt.name, pt.incr_delay, pt.arrival, pt.slew
                ));
            } else {
                lines.push(format!(
                    "  {} ({tr_ch})  incr={:.4} ns  arrival={:.4} ns  slew={:.4} ns",
                    pt.name, pt.incr_delay, pt.arrival, pt.slew
                ));
            }
        }
    }
    for next in &env.next_actions {
        lines.push(next_line(next));
    }

    Ok((env, lines, exit::OK))
}
