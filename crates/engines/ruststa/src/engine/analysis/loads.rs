// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

use super::*;

pub(super) fn build_wireload_config(
    netview: &NetlistView<'_>,
    constraints: &CompiledConstraints,
) -> WireloadConfig {
    let mut cfg = WireloadConfig::new();
    if let Some(m) = constraints
        .wire_load
        .model_max
        .as_ref()
        .or(constraints.wire_load.model_min.as_ref())
    {
        cfg.wire_load_model = Some(m.clone());
    }
    if let Some(lib) = constraints
        .wire_load
        .models
        .last()
        .and_then(|m| m.library.clone())
        .or_else(|| {
            constraints
                .wire_load
                .selection_groups
                .last()
                .and_then(|s| s.library.clone())
        })
    {
        cfg.wire_load_library = Some(lib);
    }
    if let Some(sel) = constraints
        .wire_load
        .selection_max
        .as_ref()
        .or(constraints.wire_load.selection_min.as_ref())
    {
        cfg.wire_load_selection_group = Some(sel.clone());
    }
    if let Some(mode) = constraints.wire_load.mode {
        cfg.wire_load_mode = Some(mode.as_str().to_string());
    }

    // Explicit `set_load -wire_load` on nets or ports.
    for (&net_row, nl) in &constraints.net_loads {
        if nl.wire_load {
            let target = match netview.resolve_net(net_row) {
                NetTarget::Net(canon) => canon,
                NetTarget::Const(_) => net_row,
            };
            let entry = cfg
                .net_wire_load_overrides
                .entry(target)
                .or_insert([0.0, 0.0]);
            entry[0] += nl.load_pf[0];
            entry[1] += nl.load_pf[1];
        }
    }
    for (&port_node, pl) in &constraints.port_loads {
        if pl.wire_load
            && !pl.pin_load
            && let NetTarget::Net(canon) = netview.node_net(port_node)
        {
            let entry = cfg
                .net_wire_load_overrides
                .entry(canon)
                .or_insert([0.0, 0.0]);
            entry[0] += pl.load_pf[0];
            entry[1] += pl.load_pf[1];
        }
    }
    cfg
}

pub(super) fn build_net_load_overrides(
    netview: &NetlistView<'_>,
    constraints: &CompiledConstraints,
    wire_is_ideal: bool,
) -> NetLoadOverrides {
    let mut ov = NetLoadOverrides::new();
    for (&port_node, pl) in &constraints.port_loads {
        if pl.wire_load && !pl.pin_load {
            if wire_is_ideal && let NetTarget::Net(canon) = netview.node_net(port_node) {
                ov.add_net_wire_load(canon, pl.load_pf[0], pl.load_pf[1]);
            }
        } else {
            ov.add_port_bit_load(port_node.index() as u32, pl.load_pf[0], pl.load_pf[1]);
        }
    }
    for (&net_row, nl) in &constraints.net_loads {
        let target = match netview.resolve_net(net_row) {
            NetTarget::Net(canon) => canon,
            NetTarget::Const(_) => net_row,
        };
        if nl.wire_load && !nl.pin_load {
            if wire_is_ideal {
                ov.add_net_wire_load(target, nl.load_pf[0], nl.load_pf[1]);
            }
        } else {
            ov.add_net_pin_load(target, nl.load_pf[0], nl.load_pf[1]);
        }
    }
    ov
}
