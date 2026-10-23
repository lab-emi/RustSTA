// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Bounded first-moment extracted RC timing (STA-REQ-0702 to STA-REQ-0706).
//! Lumped NLDM driver load, Elmore delay, unchanged wire slew. No waveform/ceff claim.

mod tree;

use super::load::{NetLoadOverrides, map_netlist_cell_pins};
use super::wire::{DriverLoad, WireModel};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::types::{An, DelaySlew, NetRow, NodeId, Tr};
use rusteda_core::diag::Diag;
use rusteda_core::liberty::Direction;
use rusteda_core::names::escape_segment;
use rusteda_formats::spef as format;
use std::collections::{BTreeMap, BTreeSet};

/// An extraction associated with one explicitly named library corner.
#[derive(Debug, Clone, PartialEq)]
pub struct ParasiticInput {
    pub corner: String,
    pub data: format::Spef,
}

/// Bound per-net wire capacitance and per-load, transition/bound Elmore delay.
#[derive(Debug, Clone, PartialEq)]
pub struct Spef {
    wire_cap_pf: Vec<f64>,
    delay_ns: Vec<[f64; 4]>,
}

pub(super) fn error(message: impl Into<String>) -> Diag {
    Diag::error("T0070", message)
}

/// Convert format dividers/escapes to the database's canonical hierarchy syntax.
fn canonical(raw: &str, source: &format::Spef, pin: bool) -> String {
    let mut out = String::new();
    let mut segment = String::new();
    let mut escaped = false;
    for c in raw.chars() {
        if escaped {
            segment.push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == source.divider || pin && c == source.delimiter {
            out.push_str(&escape_segment(&segment));
            out.push('/');
            segment.clear();
        } else if c == source.bus_delimiter.0 {
            segment.push('[');
        } else if c == source.bus_delimiter.1 {
            segment.push(']');
        } else {
            segment.push(c);
        }
    }
    out.push_str(&escape_segment(&segment));
    out
}

impl Spef {
    /// Bind and validate the entire extraction; missing annotation is an error.
    pub fn bind(
        source: &format::Spef,
        view: &NetlistView<'_>,
        lib: &TimingLib,
        loads: &NetLoadOverrides,
        max_state_bytes: u64,
    ) -> Result<Self, Diag> {
        let estimate = source
            .estimated_state_bytes()
            .saturating_add(view.num_nodes() as u64 * 256)
            .saturating_add(view.num_nets() as u64 * 160);
        if estimate > max_state_bytes {
            return Err(error(format!(
                "RC state budget exceeded: estimated {estimate} bytes > {max_state_bytes}"
            )));
        }
        source
            .validate(format::Limits::absolute_max())
            .map_err(|e| error(e.to_string()))?;
        if source.design != view.module_name() {
            return Err(error(format!(
                "SPEF design {} differs from {}",
                source.design,
                view.module_name()
            )));
        }
        let mut nodes = BTreeMap::new();
        for i in 0..view.num_nodes() {
            let node = NodeId(i as u32);
            if nodes
                .insert(view.node_name(node).to_string(), node)
                .is_some()
            {
                return Err(error("ambiguous canonical connection name in netlist"));
            }
        }
        let mut nets = BTreeMap::new();
        for i in 0..view.num_nets() {
            let row = NetRow(i as u32);
            if let NetTarget::Net(rep) = view.resolve_net(row) {
                if let Some(old) = nets.insert(view.net_name(row).to_string(), rep)
                    && old != rep
                {
                    return Err(error("ambiguous canonical net name in netlist"));
                }
            }
        }
        let mut result = Self {
            wire_cap_pf: vec![0.0; view.num_nets()],
            delay_ns: vec![[0.0; 4]; view.num_nodes()],
        };
        let mut annotated = BTreeSet::new();
        let pin_map = map_netlist_cell_pins(view.frozen(), lib);
        for net in &source.nets {
            let name = canonical(&net.name, source, false);
            let row = *nets
                .get(&name)
                .ok_or_else(|| error(format!("SPEF net {} is absent from netlist", net.name)))?;
            if !annotated.insert(row) {
                return Err(error(format!(
                    "duplicate extraction for aliased net {name}"
                )));
            }
            let drivers = view.drivers(row);
            if drivers.len() != 1 {
                return Err(error(format!("SPEF net {name} needs exactly one driver")));
            }
            let mut caps: Vec<[f64; 4]> = net.nodes.iter().map(|n| [n.cap_pf; 4]).collect();
            let mut connections = Vec::with_capacity(net.connections.len());
            let mut connected = BTreeSet::new();
            let mut root = None;
            for conn in &net.connections {
                let rc_name = &net.nodes[conn.node as usize].name;
                let name = canonical(rc_name, source, !conn.port);
                let id = *nodes.get(&name).ok_or_else(|| {
                    error(format!("SPEF connection {rc_name} is absent from netlist"))
                })?;
                if !connected.insert(id) {
                    return Err(error(format!("duplicate physical connection {name}")));
                }
                if view.node_net(id) != NetTarget::Net(row) {
                    return Err(error(format!(
                        "SPEF connection {name} belongs to another net"
                    )));
                }
                let (port, direction) = match view.node_kind(id) {
                    NodeKind::PortBit { dir, .. } => (true, dir),
                    NodeKind::InstancePin { dir, .. } => (false, dir),
                };
                let expected = match direction {
                    Direction::Input => 'I',
                    Direction::Output => 'O',
                    _ => 'B',
                };
                if port != conn.port || expected != conn.direction || expected == 'B' {
                    return Err(error(format!(
                        "SPEF direction/kind mismatch or unsupported inout at {name}"
                    )));
                }
                if id == drivers[0] {
                    root = Some(conn.node);
                }
                let cap = match view.node_kind(id) {
                    NodeKind::PortBit { port_bit, .. } => {
                        let c = loads
                            .by_port_bit
                            .get(&port_bit)
                            .copied()
                            .unwrap_or([0.0; 2]);
                        [c[0], c[1], c[0], c[1]]
                    }
                    NodeKind::InstancePin { cell, cell_pin, .. } => {
                        pin_map[cell as usize][cell_pin as usize].map_or([0.0; 4], |p| {
                            [p.cap[0][0], p.cap[0][1], p.cap[1][0], p.cap[1][1]]
                        })
                    }
                };
                for (dst, c) in caps[conn.node as usize].iter_mut().zip(cap) {
                    *dst += c;
                }
                connections.push((id, conn.node));
            }
            let expected: BTreeSet<_> = drivers.iter().chain(view.loads(row)).copied().collect();
            if connected != expected {
                return Err(error(format!("incomplete SPEF connections for net {name}")));
            }
            let root = root.ok_or_else(|| error(format!("SPEF driver absent for {name}")))?;
            // Net load directives have no physical location: pin load is placed at the root.
            // A wire-load override would replace extracted semantics and is rejected.
            for (raw, spec) in &loads.by_net {
                if view.resolve_net(*raw) != NetTarget::Net(row) {
                    continue;
                }
                if spec.wire_load.iter().flatten().any(|&c| c != 0.0) {
                    return Err(error(
                        "set_load -wire_load cannot override extracted parasitics",
                    ));
                }
                for tr in 0..2 {
                    for an in 0..2 {
                        caps[root as usize][tr * 2 + an] += spec.pin_load[tr][an];
                    }
                }
            }
            let delays = tree::elmore(net, root, &caps)?;
            for (id, rc_node) in connections {
                result.delay_ns[id.index()] = delays[rc_node as usize];
            }
            // Add wire capacitance once on the canonical row, before alias aggregation.
            result.wire_cap_pf[row.index()] = net.nodes.iter().map(|n| n.cap_pf).sum();
        }
        for &net in view.canonical_nets() {
            if !view.drivers(net).is_empty()
                && !view.loads(net).is_empty()
                && !annotated.contains(&net)
            {
                return Err(error(format!(
                    "missing SPEF annotation for {}",
                    view.net_name(net)
                )));
            }
        }
        Ok(result)
    }
}

impl WireModel for Spef {
    fn scope_name(&self) -> &'static str {
        "spef_elmore"
    }
    fn driver_load(&self, net: NetRow, _an: An, pin_load: f64) -> DriverLoad {
        DriverLoad::Lumped {
            c: pin_load + self.wire_cap_pf[net.index()],
        }
    }
    fn load_delay(&self, _net: NetRow, load: NodeId, tr: Tr, an: An, slew: f64) -> DelaySlew {
        DelaySlew {
            delay: self.delay_ns[load.index()][tr.index() * 2 + an.index()],
            slew,
        }
    }
}

#[cfg(test)]
mod names_tests {
    use super::*;
    #[test]
    fn escaped_dividers_and_bus_names() {
        let s = format::Spef {
            design: "top".into(),
            divider: '.',
            delimiter: ':',
            bus_delimiter: ('<', '>'),
            nets: Vec::new(),
            coupling_records: 0,
            metadata_records: 0,
            node_count: 0,
            element_count: 0,
        };
        assert_eq!(canonical("u.data<3>", &s, false), "u/data[3]");
        assert_eq!(canonical(r"u\.v:A<2>", &s, true), "u.v/A[2]");
        assert_eq!(canonical(r"u\/v:A", &s, true), r"u\/v/A");
    }
}
