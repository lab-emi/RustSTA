// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Physical RC binding, Elmore moments and passive pi loads (STA-REQ-0702 to STA-REQ-0706).
//! Bounded waveform lowering adds charge-matched loads and threshold/slew response.
//! Grounded coupling only; no CCS or crosstalk claim. Resistor loops are solved on the nodal
//! conductance system (STA-REQ-0730); components without a resistive path to the driver join it
//! (STA-REQ-0731).

mod mesh;
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
    pi_loads: Vec<[DriverLoad; 4]>,
    primary: Vec<bool>,
    thresholds: crate::engine::input::libview::CornerThresholds,
    topology: RcTopology,
}

/// The extracted networks of one binding that are not plain trees (STA-REQ-0730,
/// STA-REQ-0731).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RcTopology {
    /// Nets with at least one resistor loop, and their independent loops.
    pub mesh_nets: usize,
    pub loops: usize,
    /// Resistors whose two ends are one zero-resistance node; they carry no current.
    pub shorted_resistors: usize,
    /// The first net with a loop, in source order.
    pub first_mesh: Option<String>,
    /// Nets with components joined to their driver, the components, their RC nodes and the
    /// netlist pins among them.
    pub detached_nets: usize,
    pub detached_components: usize,
    pub detached_nodes: usize,
    pub detached_pins: usize,
    /// Extracted (ground and grounded coupling) capacitance of the joined components, pF.
    pub detached_wire_pf: f64,
    /// The first net with a joined component and the members of its first component.
    pub first_detached: Option<String>,
}

/// Members of joined components named in [`RcTopology::first_detached`].
const NAMED_MEMBERS: usize = 8;

impl RcTopology {
    /// The diagnostics of this binding at `corner`: warning `T0151` for joined components and
    /// note `T0152` for solved loops and shorted resistors, each aggregated with a count and
    /// the first net (Rule 7.1).
    pub fn diagnostics(&self, corner: &str) -> Vec<Diag> {
        let mut out = Vec::new();
        if self.detached_components > 0 {
            out.push(Diag::warning(
                "T0151",
                format!(
                    "corner {corner}: {} RC component(s) ({} node(s), {} pin(s), {:.6e} pF \
                     extracted capacitance) in {} net(s) have no resistor path to their driver; \
                     joined to the driver with zero resistance: their capacitance loads the \
                     driver and their sinks get zero wire delay (a lower bound); first: {}",
                    self.detached_components,
                    self.detached_nodes,
                    self.detached_pins,
                    self.detached_wire_pf,
                    self.detached_nets,
                    self.first_detached.as_deref().unwrap_or("-"),
                ),
            ));
        }
        if self.mesh_nets > 0 || self.shorted_resistors > 0 {
            out.push(Diag::note(
                "T0152",
                format!(
                    "corner {corner}: {} independent resistor loop(s) in {} net(s) solved on \
                     the nodal conductance system (first: {}); {} resistor(s) shorted by zero \
                     resistance carry no current",
                    self.loops,
                    self.mesh_nets,
                    self.first_mesh.as_deref().unwrap_or("-"),
                    self.shorted_resistors,
                ),
            ));
        }
        out
    }
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
    /// Bind and validate the entire extraction; missing annotation is an error (STA-REQ-0732).
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
            pi_loads: vec![[DriverLoad::Lumped { c: 0.0 }; 4]; view.num_nets()],
            primary: vec![false; view.num_nets()],
            thresholds: lib.thresholds,
            topology: RcTopology::default(),
        };
        // Scratch of one net's loop solve comes out of the state budget left after the
        // estimate; its work is bounded like the waveform preparation (STA-REQ-0730).
        let budget = tree::Budget {
            bytes: max_state_bytes - estimate,
            work: max_state_bytes.saturating_mul(8),
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
            let reduced = tree::reduce(net, root, &caps, budget)?;
            let topology = &mut result.topology;
            if reduced.loops > 0 {
                topology.mesh_nets += 1;
                topology.loops += reduced.loops;
                topology.first_mesh.get_or_insert_with(|| name.clone());
            }
            topology.shorted_resistors += reduced.shorted;
            if !reduced.detached.is_empty() {
                let pins: BTreeMap<u32, NodeId> =
                    connections.iter().map(|&(id, node)| (node, id)).collect();
                topology.detached_nets += 1;
                topology.detached_components += reduced.detached.len();
                for component in &reduced.detached {
                    topology.detached_nodes += component.len();
                    for &node in component {
                        topology.detached_wire_pf += net.nodes[node as usize].cap_pf;
                        topology.detached_pins += usize::from(pins.contains_key(&node));
                    }
                }
                if topology.first_detached.is_none() {
                    let mut named = 0;
                    let mut groups = Vec::new();
                    for component in &reduced.detached {
                        let mut members = Vec::new();
                        for &node in component {
                            if named == NAMED_MEMBERS {
                                break;
                            }
                            named += 1;
                            members.push(match pins.get(&node) {
                                Some(&id) => format!("pin {}", view.node_name(id)),
                                None => format!("node {}", net.nodes[node as usize].name),
                            });
                        }
                        if !members.is_empty() {
                            groups.push(format!("{{{}}}", members.join(", ")));
                        }
                    }
                    let total: usize = reduced.detached.iter().map(Vec::len).sum();
                    if total > named {
                        groups.push(format!("{} more node(s)", total - named));
                    }
                    topology.first_detached = Some(format!(
                        "net {name} ({} component(s)): {}",
                        reduced.detached.len(),
                        groups.join(" ")
                    ));
                }
            }
            let delays = reduced.delays;
            result.pi_loads[row.index()] = reduced.pi.map(|p| DriverLoad::Pi {
                c_near: p.near,
                r: p.resistance,
                c_far: p.far,
            });
            result.primary[row.index()] =
                matches!(view.node_kind(drivers[0]), NodeKind::PortBit { .. });
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

impl Spef {
    /// The non-tree networks of this binding (STA-REQ-0730, STA-REQ-0731).
    pub fn topology(&self) -> &RcTopology {
        &self.topology
    }
}

impl WireModel for Spef {
    fn elmore(&self, load: NodeId, tr: Tr, an: An) -> Option<f64> {
        Some(self.delay_ns[load.index()][tr.index() * 2 + an.index()])
    }

    fn scope_name(&self) -> &'static str {
        "spef_rc"
    }
    fn physical_load(&self, net: NetRow, tr: Tr, an: An) -> Option<DriverLoad> {
        Some(self.pi_loads[net.index()][tr.index() * 2 + an.index()])
    }
    fn primary_input(&self, net: NetRow) -> bool {
        self.primary[net.index()]
    }
    fn driver_load(&self, net: NetRow, _an: An, pin_load: f64) -> DriverLoad {
        DriverLoad::Lumped {
            c: pin_load + self.wire_cap_pf[net.index()],
        }
    }
    fn load_delay(&self, net: NetRow, load: NodeId, tr: Tr, an: An, slew: f64) -> DelaySlew {
        let tau = self.delay_ns[load.index()][tr.index() * 2 + an.index()];
        match rusteda_compute::rc::wire(
            tau,
            slew,
            self.thresholds.rc(tr == Tr::Fall),
            self.primary[net.index()],
        ) {
            Ok((delay, slew)) => DelaySlew { delay, slew },
            Err(_) => DelaySlew {
                delay: f64::NAN,
                slew: f64::NAN,
            },
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
