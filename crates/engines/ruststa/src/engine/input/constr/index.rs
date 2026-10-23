// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Netlist and library object index for constraint compilation (owner: E2).
//!
//! Indexes a [`FrozenNetlist`] and the corner [`Library`] slices using the exact [`NodeId`] and
//! [`NetRow`] numbering of the timing engine (`0..n_port_bits` for top-level port bits,
//! `n_port_bits..n_port_bits + n_pins` for connected instance pins).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rayon::prelude::*;
use rusteda_core::liberty::{Cell, Direction, Library, Pin, SeqKind};
use rusteda_core::netlist::{ANONYMOUS, FrozenNetlist, SCALAR, module_entry};

use super::model::{CornerLibs, LibCellRef, LibPinRef, PinRef};
use crate::engine::types::{NetRow, NodeId};

/// Indexed metadata for a top-level port bit (`NodeId(0..n_port_bits)`).
#[derive(Debug, Clone)]
pub struct PortBitInfo {
    pub node: NodeId,
    pub port_idx: u32,
    pub base_name: String,
    pub full_name: String,
    /// `0` = Input, `1` = Output, `2` = Inout.
    pub dir: u8,
    pub net: NetRow,
}

impl PortBitInfo {
    #[must_use]
    pub fn is_input(&self) -> bool {
        self.dir == 0 || self.dir == 2
    }

    #[must_use]
    pub fn is_output(&self) -> bool {
        self.dir == 1 || self.dir == 2
    }

    #[must_use]
    pub fn dir_str(&self) -> &'static str {
        match self.dir {
            0 => "in",
            1 => "out",
            _ => "inout",
        }
    }
}

/// Indexed metadata for an instance pin (both connected and unconnected).
///
/// The instance, pin and cell names are shared with the owning [`CellInstInfo`] and with every
/// other pin of the same cell type rather than copied per pin: on designs with tens of millions of
/// pins (OpenC910) the copies dominated the memory of constraint compilation.
#[derive(Debug, Clone)]
pub struct PinInfo {
    pub pin_ref: PinRef,
    pub inst_name: Arc<str>,
    pub pin_name: Arc<str>,
    pub cell_name: Arc<str>,
    /// `0` = Input, `1` = Output, `2` = Inout, `3` = Other/Internal.
    pub dir: u8,
    pub net: Option<NetRow>,
    pub is_clock_pin: bool,
    pub is_data_pin: bool,
    pub is_async_pin: bool,
    pub is_tristate_pin: bool,
}

impl PinInfo {
    #[must_use]
    pub fn dir_str(&self) -> &'static str {
        match self.dir {
            0 => "in",
            1 => "out",
            2 => "inout",
            _ => "internal",
        }
    }

    #[must_use]
    pub fn is_input(&self) -> bool {
        self.dir == 0 || self.dir == 2
    }
}

/// Indexed metadata for a cell instance (`0..netlist.instances.len()`).
#[derive(Debug, Clone)]
pub struct CellInstInfo {
    pub inst: u32,
    pub full_name: Arc<str>,
    pub base_name: String,
    pub cell_type: u16,
    pub cell_name: Arc<str>,
    /// Indices into `DesignIndex::all_pins` for this instance's pins.
    pub pin_indices: Vec<usize>,
    pub is_sequential: bool,
    pub is_flip_flop: bool,
    pub is_latch: bool,
    pub is_clock_gating: bool,
    pub is_tristate: bool,
}

/// Indexed metadata for a net (`NetRow(0..netlist.nets.len())`).
#[derive(Debug, Clone)]
pub struct NetInfo {
    pub row: NetRow,
    pub full_name: String,
    pub base_name: String,
    /// Representative `NetRow` of this net's `assign`-equivalence class; the port bits and pins
    /// of the class are [`DesignIndex::net_port_bits`] and [`DesignIndex::net_pins`].
    pub equiv_root: NetRow,
}

/// Indexed metadata for a library cell (`get_lib_cells`).
#[derive(Debug, Clone)]
pub struct LibCellInfo {
    pub lib_cell: LibCellRef,
    pub dont_use: bool,
    pub dont_touch: bool,
    pub is_sequential: bool,
    pub is_flip_flop: bool,
    pub is_latch: bool,
    pub is_clock_gating: bool,
    pub is_tristate: bool,
    pub pin_names: Vec<String>,
    pub output_pins: Vec<String>,
    pub input_pins: Vec<String>,
}

/// Indexed metadata for a library pin (`get_lib_pins`).
#[derive(Debug, Clone)]
pub struct LibPinInfo {
    pub lib_pin: LibPinRef,
    pub dir: &'static str,
    pub is_clock: bool,
}

#[derive(Debug, Clone, Default)]
struct CellRoleSummary {
    is_sequential: bool,
    is_flip_flop: bool,
    is_latch: bool,
    is_clock_gating: bool,
    is_tristate: bool,
    clock_pins: BTreeSet<String>,
    data_pins: BTreeSet<String>,
    async_pins: BTreeSet<String>,
    tristate_pins: BTreeSet<String>,
    comb_arcs: Vec<(String, String)>,
}

/// Pre-indexed view of the [`FrozenNetlist`] and corner [`Library`] slices.
#[derive(Debug, Clone)]
pub struct DesignIndex {
    pub top_name: String,
    pub n_port_bits: u32,
    pub n_pins: u32,
    pub port_bits: Vec<PortBitInfo>,
    /// All instance pins (both connected and unconnected).
    pub all_pins: Vec<PinInfo>,
    /// Map from connected pin `NodeId` (`n_port_bits..n_port_bits + n_pins`) to index in `all_pins`.
    pub node_to_pin_idx: Vec<usize>,
    pub instances: Vec<CellInstInfo>,
    pub nets: Vec<NetInfo>,
    /// The port bits and the pins (indices into `all_pins`) of each `assign`-equivalence class,
    /// stored once per class by root net row (empty for a net that is not a root): a net of a
    /// class with many aliases shares the class lists instead of holding a copy (AGENTS rule 3.4;
    /// the Yosys OpenC910 netlist has 654,105 assigned bits).
    class_port_start: Vec<usize>,
    class_ports: Vec<NodeId>,
    class_pin_start: Vec<usize>,
    class_pins: Vec<usize>,
    pub lib_cells: Vec<LibCellInfo>,
    pub lib_pins: Vec<LibPinInfo>,
    /// Combinational pin-pair arcs `(from_pin_name, to_pin_name)` by cell name.
    cell_comb_arcs: BTreeMap<String, Vec<(String, String)>>,
    /// Inferred clock-gating enable pin indices in `all_pins` (recomputed when clocks change).
    pub inferred_cg_enable_pins: BTreeSet<usize>,
}

impl DesignIndex {
    /// Builds the design and library index from `netlist` and `corners`.
    #[must_use]
    pub fn build(netlist: &FrozenNetlist, corners: &[CornerLibs<'_>]) -> Self {
        let top_name = netlist
            .module
            .get(module_entry::NAME)
            .and_then(|&id| netlist.string(id).ok())
            .unwrap_or("")
            .to_string();
        let n_port_bits = netlist.port_bit_net.len() as u32;
        let n_pins = netlist.pin_net.len() as u32;

        // 1. Index library cells, pins, and sequential/check pin roles.
        let mut cell_roles: BTreeMap<String, CellRoleSummary> = BTreeMap::new();
        let mut lib_cells = Vec::new();
        let mut lib_pins = Vec::new();
        let mut seen_lib_cells: BTreeSet<(String, String)> = BTreeSet::new();

        for corner in corners {
            for lib in corner.libs {
                for cell in &lib.cells {
                    let summary = summarize_lib_cell(cell);
                    cell_roles
                        .entry(cell.name.clone())
                        .or_insert_with(|| summary.clone());
                    if seen_lib_cells.insert((lib.name.clone(), cell.name.clone())) {
                        let mut pin_names = Vec::new();
                        let mut output_pins = Vec::new();
                        let mut input_pins = Vec::new();
                        for p in &cell.pins {
                            pin_names.push(p.name.clone());
                            match p.direction {
                                Direction::Input => input_pins.push(p.name.clone()),
                                Direction::Output => output_pins.push(p.name.clone()),
                                Direction::Inout => {
                                    input_pins.push(p.name.clone());
                                    output_pins.push(p.name.clone());
                                }
                                Direction::Internal => {}
                            }
                            let dir_str = match p.direction {
                                Direction::Input => "in",
                                Direction::Output => "out",
                                Direction::Inout => "inout",
                                Direction::Internal => "internal",
                            };
                            lib_pins.push(LibPinInfo {
                                lib_pin: LibPinRef {
                                    library: lib.name.clone(),
                                    cell: cell.name.clone(),
                                    pin: p.name.clone(),
                                },
                                dir: dir_str,
                                is_clock: summary.clock_pins.contains(&p.name),
                            });
                        }
                        lib_cells.push(LibCellInfo {
                            lib_cell: LibCellRef {
                                library: lib.name.clone(),
                                cell: cell.name.clone(),
                            },
                            dont_use: cell.dont_use,
                            dont_touch: cell.dont_touch,
                            is_sequential: summary.is_sequential,
                            is_flip_flop: summary.is_flip_flop,
                            is_latch: summary.is_latch,
                            is_clock_gating: summary.is_clock_gating,
                            is_tristate: summary.is_tristate,
                            pin_names,
                            output_pins,
                            input_pins,
                        });
                    }
                }
            }
        }

        let cell_comb_arcs: BTreeMap<String, Vec<(String, String)>> = cell_roles
            .iter()
            .map(|(k, v)| (k.clone(), v.comb_arcs.clone()))
            .collect();

        // 2. Build assign equivalence classes over nets.
        let n_nets = netlist.nets.len();
        let mut parent: Vec<u32> = (0..n_nets as u32).collect();
        for a in netlist.assigns.iter() {
            if (a.lhs as usize) < n_nets && (a.rhs as usize) < n_nets {
                uf_union(&mut parent, a.lhs, a.rhs);
            }
        }
        let mut equiv_roots = Vec::with_capacity(n_nets);
        for r in 0..n_nets as u32 {
            equiv_roots.push(NetRow(uf_find(&mut parent, r)));
        }

        // 3. Index port bits (`NodeId(0..n_port_bits)`).
        let mut port_bits = Vec::with_capacity(n_port_bits as usize);
        for (port_idx, port_rec) in netlist.ports.iter().enumerate() {
            let base_name =
                rusteda_core::names::unescape(netlist.string(port_rec.name).unwrap_or(""));
            let start = netlist.port_bit_start[port_idx];
            let end = netlist.port_bit_start[port_idx + 1];
            for pb in start..end {
                let net_row = netlist.port_bit_net[pb as usize];
                let net_rec = &netlist.nets[net_row as usize];
                let full_name = if net_rec.index == SCALAR {
                    base_name.clone()
                } else {
                    format!("{base_name}[{}]", net_rec.index)
                };
                port_bits.push(PortBitInfo {
                    node: NodeId(pb),
                    port_idx: port_idx as u32,
                    base_name: base_name.clone(),
                    full_name,
                    dir: port_rec.dir,
                    net: NetRow(net_row),
                });
            }
        }

        // 4. Index instances and all instance pins (connected + unconnected). Names and pin roles
        // are resolved once per netlist cell type and shared by its instances.
        struct CellPinMeta {
            name: Arc<str>,
            dir: u8,
            is_clock_pin: bool,
            is_data_pin: bool,
            is_async_pin: bool,
            is_tristate_pin: bool,
        }
        let cell_meta: Vec<(Arc<str>, Vec<CellPinMeta>)> = (0..netlist.cells.len())
            .map(|c| {
                let cell_name = netlist.string(netlist.cells[c]).unwrap_or("");
                let role = cell_roles.get(cell_name);
                let slots =
                    netlist.cell_pin_start[c] as usize..netlist.cell_pin_start[c + 1] as usize;
                let pins = slots
                    .map(|slot| {
                        let name = netlist.string(netlist.cell_pin_name[slot]).unwrap_or("");
                        let has = |set: fn(&CellRoleSummary) -> &BTreeSet<String>| {
                            role.is_some_and(|r| set(r).contains(name))
                        };
                        CellPinMeta {
                            name: Arc::from(name),
                            dir: netlist.cell_pin_dir[slot],
                            is_clock_pin: has(|r| &r.clock_pins),
                            is_data_pin: has(|r| &r.data_pins),
                            is_async_pin: has(|r| &r.async_pins),
                            is_tristate_pin: has(|r| &r.tristate_pins),
                        }
                    })
                    .collect();
                (Arc::from(cell_name), pins)
            })
            .collect();

        // Instance `i` owns the pins `pin_base[i]..pin_base[i + 1]` of `all_pins`, one per pin of
        // its cell, so the pins are built in parallel at fixed positions; `conn` holds the
        // connected pin row of each of them, or `u32::MAX` (the last connection of a local pin
        // wins).
        let n_inst = netlist.instances.len();
        let mut pin_base = Vec::with_capacity(n_inst + 1);
        pin_base.push(0usize);
        for inst_rec in netlist.instances.iter() {
            let n = cell_meta[inst_rec.cell as usize].1.len();
            pin_base.push(pin_base[pin_base.len() - 1] + n);
        }
        let mut conn = vec![u32::MAX; pin_base[n_inst]];
        for inst_idx in 0..n_inst {
            let n_cp = pin_base[inst_idx + 1] - pin_base[inst_idx];
            let start = netlist.inst_pin_start[inst_idx];
            let end = netlist.inst_pin_start[inst_idx + 1];
            for pin_row in start..end {
                let cp = netlist.pin_cellpin[pin_row as usize] as u16 as usize;
                if cp < n_cp {
                    conn[pin_base[inst_idx] + cp] = pin_row;
                }
            }
        }
        let inst_names: Vec<Arc<str>> = netlist
            .instances
            .par_iter()
            .with_min_len(4096)
            .map(|r| {
                Arc::from(rusteda_core::names::unescape(
                    netlist.string(r.name).unwrap_or(""),
                ))
            })
            .collect();

        let all_pins: Vec<PinInfo> = (0..n_inst)
            .into_par_iter()
            .with_min_len(1024)
            .flat_map_iter(|inst_idx| {
                let (cell_name, cell_pins) = &cell_meta[netlist.instances[inst_idx].cell as usize];
                let full_name = &inst_names[inst_idx];
                let conn = &conn[pin_base[inst_idx]..pin_base[inst_idx + 1]];
                cell_pins
                    .iter()
                    .zip(conn)
                    .enumerate()
                    .map(move |(cp, (meta, &pin_row))| {
                        let (node, net) = if pin_row == u32::MAX {
                            (None, None)
                        } else {
                            let raw_net = netlist.pin_net[pin_row as usize];
                            let net = ((raw_net as usize) < n_nets).then_some(NetRow(raw_net));
                            (Some(NodeId(n_port_bits + pin_row)), net)
                        };
                        PinInfo {
                            pin_ref: PinRef {
                                inst: inst_idx as u32,
                                cell_pin: cp as u16,
                                node,
                                full_name: format!("{full_name}/{}", meta.name),
                            },
                            inst_name: Arc::clone(full_name),
                            pin_name: Arc::clone(&meta.name),
                            cell_name: Arc::clone(cell_name),
                            dir: meta.dir,
                            net,
                            is_clock_pin: meta.is_clock_pin,
                            is_data_pin: meta.is_data_pin,
                            is_async_pin: meta.is_async_pin,
                            is_tristate_pin: meta.is_tristate_pin,
                        }
                    })
            })
            .collect();
        drop(conn);
        let mut node_to_pin_idx = vec![usize::MAX; n_pins as usize];
        for (pin_idx, pin) in all_pins.iter().enumerate() {
            if let Some(nid) = pin.pin_ref.node {
                node_to_pin_idx[(nid.0 - n_port_bits) as usize] = pin_idx;
            }
        }

        let instances: Vec<CellInstInfo> = inst_names
            .into_par_iter()
            .enumerate()
            .with_min_len(1024)
            .map(|(inst_idx, full_name)| {
                let cell_u32 = netlist.instances[inst_idx].cell;
                let cell_name = &cell_meta[cell_u32 as usize].0;
                let role = cell_roles.get(&**cell_name);
                let base_name = full_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&full_name)
                    .to_string();
                CellInstInfo {
                    inst: inst_idx as u32,
                    full_name,
                    base_name,
                    cell_type: cell_u32 as u16,
                    cell_name: Arc::clone(cell_name),
                    pin_indices: (pin_base[inst_idx]..pin_base[inst_idx + 1]).collect(),
                    is_sequential: role.is_some_and(|r| r.is_sequential),
                    is_flip_flop: role.is_some_and(|r| r.is_flip_flop),
                    is_latch: role.is_some_and(|r| r.is_latch),
                    is_clock_gating: role.is_some_and(|r| r.is_clock_gating),
                    is_tristate: role.is_some_and(|r| r.is_tristate),
                }
            })
            .collect();
        drop(pin_base);
        drop(cell_meta);

        // 5. Index nets and group connected port bits & pins by equivalence root, in port-bit and
        // pin order (a counting sort into one table per kind).
        let group = |keys: &mut dyn Iterator<Item = usize>| -> (Vec<usize>, Vec<usize>) {
            let keys: Vec<usize> = keys.collect();
            let mut start = vec![0usize; n_nets + 1];
            for &k in &keys {
                start[k + 1] += 1;
            }
            for i in 0..n_nets {
                start[i + 1] += start[i];
            }
            let mut fill = start.clone();
            let mut order = vec![0usize; keys.len()];
            for (i, &k) in keys.iter().enumerate() {
                order[fill[k]] = i;
                fill[k] += 1;
            }
            (start, order)
        };
        let (class_port_start, port_order) = group(
            &mut port_bits
                .iter()
                .map(|pb| equiv_roots[pb.net.0 as usize].0 as usize),
        );
        let class_ports: Vec<NodeId> = port_order.iter().map(|&i| port_bits[i].node).collect();
        let connected: Vec<usize> = all_pins
            .iter()
            .enumerate()
            .filter_map(|(i, pin)| pin.net.map(|_| i))
            .collect();
        let (class_pin_start, pin_order) = group(&mut connected.iter().map(|&i| {
            let nr = all_pins[i].net.expect("a connected pin");
            equiv_roots[nr.0 as usize].0 as usize
        }));
        let class_pins: Vec<usize> = pin_order.iter().map(|&i| connected[i]).collect();
        drop(connected);

        let net_names: Vec<(String, String)> = netlist
            .nets
            .par_iter()
            .enumerate()
            .with_min_len(4096)
            .map(|(r, net_rec)| {
                let full_name = if net_rec.name == ANONYMOUS {
                    format!("_net{r}")
                } else {
                    let base =
                        rusteda_core::names::unescape(netlist.string(net_rec.name).unwrap_or(""));
                    if net_rec.index == SCALAR {
                        base
                    } else {
                        format!("{base}[{}]", net_rec.index)
                    }
                };
                let base_name = full_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&full_name)
                    .to_string();
                (full_name, base_name)
            })
            .collect();
        let mut nets = Vec::with_capacity(n_nets);
        for (r, (full_name, base_name)) in net_names.into_iter().enumerate() {
            let row = NetRow(r as u32);
            let equiv_root = equiv_roots[r];
            nets.push(NetInfo {
                row,
                full_name,
                base_name,
                equiv_root,
            });
        }

        Self {
            top_name,
            n_port_bits,
            n_pins,
            port_bits,
            all_pins,
            node_to_pin_idx,
            instances,
            nets,
            class_port_start,
            class_ports,
            class_pin_start,
            class_pins,
            lib_cells,
            lib_pins,
            cell_comb_arcs,
            inferred_cg_enable_pins: BTreeSet::new(),
        }
    }

    /// The port bits of the `assign`-equivalence class of net `row`, in port-bit order.
    #[must_use]
    pub fn net_port_bits(&self, row: NetRow) -> &[NodeId] {
        let r = self.nets[row.0 as usize].equiv_root.0 as usize;
        &self.class_ports[self.class_port_start[r]..self.class_port_start[r + 1]]
    }

    /// The pins (indices into `all_pins`) of the `assign`-equivalence class of net `row`, in pin
    /// order.
    #[must_use]
    pub fn net_pins(&self, row: NetRow) -> &[usize] {
        let r = self.nets[row.0 as usize].equiv_root.0 as usize;
        &self.class_pins[self.class_pin_start[r]..self.class_pin_start[r + 1]]
    }

    /// Recomputes inferred clock-gating enable pins (`STA-REQ-0212`, `STA-REQ-0268`) from the
    /// current set of clock source [`NodeId`]s.
    pub fn recompute_inferred_clock_gating(&mut self, clock_sources: &[NodeId]) {
        self.inferred_cg_enable_pins.clear();
        if clock_sources.is_empty() {
            return;
        }

        // Dense reached flags instead of ordered sets: the clock network of a large design
        // reaches millions of pins, and this runs again whenever the clock set changes.
        let mut reached_nets = vec![false; self.nets.len()];
        let mut reached_pins = vec![false; self.all_pins.len()];
        let mut queue: VecDeque<NetRow> = VecDeque::new();
        let reach_net =
            |reached_nets: &mut Vec<bool>, queue: &mut VecDeque<NetRow>, root: NetRow| {
                if !std::mem::replace(&mut reached_nets[root.0 as usize], true) {
                    queue.push_back(root);
                }
            };

        for &src in clock_sources {
            if src.0 < self.n_port_bits {
                let nr = self.port_bits[src.0 as usize].net;
                let root = self.nets[nr.0 as usize].equiv_root;
                reach_net(&mut reached_nets, &mut queue, root);
            } else {
                let pin_row = (src.0 - self.n_port_bits) as usize;
                if let Some(&pin_idx) = self.node_to_pin_idx.get(pin_row)
                    && pin_idx < self.all_pins.len()
                {
                    reached_pins[pin_idx] = true;
                    if let Some(nr) = self.all_pins[pin_idx].net {
                        let root = self.nets[nr.0 as usize].equiv_root;
                        reach_net(&mut reached_nets, &mut queue, root);
                    }
                }
            }
        }

        while let Some(root_net) = queue.pop_front() {
            for &pin_idx in self.net_pins(root_net) {
                let pin = &self.all_pins[pin_idx];
                if !pin.is_input() {
                    continue;
                }
                reached_pins[pin_idx] = true;
                let inst_idx = pin.pin_ref.inst as usize;
                let inst_info = &self.instances[inst_idx];
                if inst_info.is_sequential {
                    continue;
                }
                let Some(comb_arcs) = self.cell_comb_arcs.get(&*inst_info.cell_name) else {
                    continue;
                };
                let from_name = &*pin.pin_name;
                for &other_pin_idx in &inst_info.pin_indices {
                    let other_pin = &self.all_pins[other_pin_idx];
                    if comb_arcs
                        .iter()
                        .any(|(f, t)| f == from_name && **t == *other_pin.pin_name)
                        && let Some(out_nr) = other_pin.net
                    {
                        let out_root = self.nets[out_nr.0 as usize].equiv_root;
                        reach_net(&mut reached_nets, &mut queue, out_root);
                    }
                }
            }
        }

        // Any non-sequential instance with >= 2 input pins where at least one input pin is reached
        // by a clock and another input pin is not reached by a clock has its unreached input pin(s)
        // as inferred clock-gating enable endpoint pins.
        for inst_info in &self.instances {
            if inst_info.is_sequential {
                continue;
            }
            let input_pins = || {
                inst_info
                    .pin_indices
                    .iter()
                    .copied()
                    .filter(|&pi| self.all_pins[pi].dir == 0)
            };
            if input_pins().count() < 2 {
                continue;
            }
            if input_pins().any(|pi| reached_pins[pi]) {
                self.inferred_cg_enable_pins
                    .extend(input_pins().filter(|&pi| !reached_pins[pi]));
            }
        }
    }

    /// Returns true if `pin_idx` in `self.all_pins` is a valid startpoint pin (`-from`;
    /// `STA-REQ-0251`, `STA-REQ-0252`).
    #[must_use]
    pub fn is_startpoint_pin(&self, pin_idx: usize) -> bool {
        self.all_pins
            .get(pin_idx)
            .is_some_and(|p| p.is_clock_pin && p.pin_ref.node.is_some())
    }

    /// Returns true if `pin_idx` in `self.all_pins` is a valid endpoint check pin (`-to`;
    /// `STA-REQ-0251`, `STA-REQ-0253`, `STA-REQ-0268`).
    #[must_use]
    pub fn is_endpoint_pin(&self, pin_idx: usize) -> bool {
        self.all_pins.get(pin_idx).is_some_and(|p| {
            p.pin_ref.node.is_some()
                && (p.is_data_pin
                    || p.is_async_pin
                    || self.inferred_cg_enable_pins.contains(&pin_idx))
        })
    }

    /// Returns canonical name of a [`NodeId`] (port bit or connected instance pin).
    #[must_use]
    pub fn node_name(&self, node: NodeId) -> &str {
        if node.0 < self.n_port_bits {
            &self.port_bits[node.0 as usize].full_name
        } else {
            let pin_row = (node.0 - self.n_port_bits) as usize;
            let pin_idx = self.node_to_pin_idx[pin_row];
            &self.all_pins[pin_idx].pin_ref.full_name
        }
    }
}

fn uf_find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = p;
    }
    x
}

fn uf_union(parent: &mut [u32], a: u32, b: u32) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        if ra < rb {
            parent[rb as usize] = ra;
        } else {
            parent[ra as usize] = rb;
        }
    }
}

fn summarize_lib_cell(cell: &Cell) -> CellRoleSummary {
    let mut s = CellRoleSummary {
        is_clock_gating: cell.is_clock_gating
            || cell.clock_gating_integrated_cell.is_some()
            || cell.name.to_ascii_lowercase().contains("icg"),
        is_flip_flop: cell
            .sequentials
            .iter()
            .any(|sq| sq.kind == SeqKind::FlipFlop),
        is_latch: cell.sequentials.iter().any(|sq| sq.kind == SeqKind::Latch),
        ..CellRoleSummary::default()
    };

    for sq in &cell.sequentials {
        for p in &cell.pins {
            if expr_mentions_ident(&sq.clock_text, &p.name) {
                s.clock_pins.insert(p.name.clone());
            }
            if expr_mentions_ident(&sq.data_text, &p.name) {
                s.data_pins.insert(p.name.clone());
            }
            if sq
                .clear_text
                .as_deref()
                .is_some_and(|e| expr_mentions_ident(e, &p.name))
                || sq
                    .preset_text
                    .as_deref()
                    .is_some_and(|e| expr_mentions_ident(e, &p.name))
            {
                s.async_pins.insert(p.name.clone());
            }
        }
    }

    for p in &cell.pins {
        if p.is_clock || p.clock_gate_clock_pin {
            s.clock_pins.insert(p.name.clone());
        }
        if p.clock_gate_enable_pin {
            s.data_pins.insert(p.name.clone());
        }
        if p.three_state.is_some() {
            s.is_tristate = true;
            s.tristate_pins.insert(p.name.clone());
        }
        for a in &p.arcs {
            let tt = a.timing_type.trim();
            match tt {
                "rising_edge" | "falling_edge" => {
                    if !a.related_pin.is_empty() {
                        s.clock_pins.insert(a.related_pin.clone());
                    }
                    if !s.is_latch {
                        s.is_flip_flop = true;
                    }
                }
                "setup_rising"
                | "setup_falling"
                | "hold_rising"
                | "hold_falling"
                | "nochange_high_high"
                | "nochange_high_low"
                | "nochange_low_high"
                | "nochange_low_low"
                | "clock_gating_setup_rising"
                | "clock_gating_setup_falling"
                | "clock_gating_hold_rising"
                | "clock_gating_hold_falling" => {
                    s.data_pins.insert(p.name.clone());
                    if !a.related_pin.is_empty() {
                        s.clock_pins.insert(a.related_pin.clone());
                    }
                }
                "recovery_rising" | "recovery_falling" | "removal_rising" | "removal_falling" => {
                    s.async_pins.insert(p.name.clone());
                    if !a.related_pin.is_empty() {
                        s.clock_pins.insert(a.related_pin.clone());
                    }
                }
                "three_state_enable"
                | "three_state_disable"
                | "three_state_enable_rise"
                | "three_state_enable_fall"
                | "three_state_disable_rise"
                | "three_state_disable_fall" => {
                    s.is_tristate = true;
                    s.tristate_pins.insert(p.name.clone());
                }
                "combinational" | "combinational_rise" | "combinational_fall" | ""
                    if !a.related_pin.is_empty() =>
                {
                    s.comb_arcs.push((a.related_pin.clone(), p.name.clone()));
                }
                _ => {}
            }
        }
        // Also derive combinational arcs from boolean function when explicit timing arcs are absent
        if s.comb_arcs.iter().all(|(_, to)| to != &p.name)
            && matches!(p.direction, Direction::Output | Direction::Inout)
        {
            for inp in &cell.pins {
                if matches!(inp.direction, Direction::Input | Direction::Inout)
                    && inp.name != p.name
                {
                    s.comb_arcs.push((inp.name.clone(), p.name.clone()));
                }
            }
        }
    }

    s.is_sequential =
        s.is_flip_flop || s.is_latch || !cell.sequentials.is_empty() || !s.clock_pins.is_empty();

    // If a cell is sequential or clock-gating and has input pins that are not clock pins, treat
    // those non-clock input pins as data check pins even when explicit check tables were omitted.
    if s.is_sequential || s.is_clock_gating {
        for p in &cell.pins {
            if matches!(p.direction, Direction::Input)
                && !s.clock_pins.contains(&p.name)
                && !s.async_pins.contains(&p.name)
            {
                s.data_pins.insert(p.name.clone());
            }
        }
    }

    s
}

fn expr_mentions_ident(expr: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    expr.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|tok| tok == ident)
}

/// Finds a cell definition across a slice of libraries (first library containing `cell_name` wins,
/// or restricted to `library_name` when `Some`).
#[must_use]
pub fn find_lib_cell<'a>(
    libs: &'a [Library],
    library_name: Option<&str>,
    cell_name: &str,
) -> Option<(&'a Library, &'a Cell)> {
    for lib in libs {
        if let Some(want_lib) = library_name
            && lib.name != want_lib
        {
            continue;
        }
        if let Some(c) = lib.cells.iter().find(|c| c.name == cell_name) {
            return Some((lib, c));
        }
    }
    None
}

/// Finds a pin on a Liberty [`Cell`].
#[must_use]
pub fn find_cell_pin<'a>(cell: &'a Cell, pin_name: &str) -> Option<&'a Pin> {
    cell.pins.iter().find(|p| p.name == pin_name)
}
