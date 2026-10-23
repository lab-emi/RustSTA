// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The netlist view: zero-copy access to the revision's frozen netlist (owner: E1).
//!
//! Engine section A. The view reads the `FrozenNetlist` of the `netlist` table in place. Node ids
//! are the port bits, then the global pin rows ([`NodeId`](super::super::types::NodeId)). Names
//! come from the string table on demand; no node holds a `String`. The loads of a net are its
//! `NET_PINS` (instance pins only) plus a reverse map of the port bits; nets joined by `ASSIGNS`
//! are aliased by union-find, and the constant net rows of `PIN_NET` are the constants 0 and 1.
//! Drivers follow the driver rules: a corner's Liberty direction must match `CELL_PIN_DIR`,
//! several drivers are an error unless all are three-state outputs, inout ports are both driver
//! and load, and unconnected inputs get one aggregated warning.

use std::borrow::Cow;
use std::fmt;

use rusteda_core::liberty::{Cell, Direction, Library, Pin, TimingType};
use rusteda_core::netlist::{ANONYMOUS, CONST0, CONST1, FrozenNetlist, SCALAR, module_entry};

use crate::engine::types::{NetRow, NodeId};

/// What a net row or connected node resolves to after `ASSIGNS` union-find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetTarget {
    /// Canonical representative net row of the equivalence class.
    Net(NetRow),
    /// Constant logic value (`false` for `1'b0`, `true` for `1'b1`).
    Const(bool),
}

/// What a timing graph node stands for in the frozen netlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A top-level port bit (`node.0 < num_port_bits()`).
    PortBit {
        /// Port bit row `0..num_port_bits()`.
        port_bit: u32,
        /// Port row `0..num_ports()`.
        port: u32,
        /// Bit index of a bus port, or [`SCALAR`] (`i32::MIN`) for a scalar port.
        bit_index: i32,
        /// Direction of the top-level port (`Input`, `Output`, `Inout`).
        dir: Direction,
    },
    /// A connected instance pin (`node.0 >= num_port_bits()`).
    InstancePin {
        /// Pin row `0..num_pins()` (`node.0 - num_port_bits()`).
        pin_row: u32,
        /// Instance row `0..num_instances()`.
        inst: u32,
        /// Cell row `0..num_cells()`.
        cell: u32,
        /// 0-based pin index within the cell (`0..cell_pins(cell).len()`).
        cell_pin: u32,
        /// Pin direction (`Input`, `Output`, `Inout`, `Internal`).
        dir: Direction,
        /// True when the pin is a three-state output in the corner libraries.
        tristate: bool,
    },
}

const _: () = assert!(size_of::<NodeKind>() <= 24);

/// A non-fatal diagnostic emitted while constructing [`NetlistView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetlistDiagnostic {
    /// Diagnostic code (`T0010` for unconnected instance inputs).
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

/// Errors detected when building a [`NetlistView`] over a [`FrozenNetlist`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetlistViewError {
    /// A net is driven by multiple non-tristate drivers (STA-REQ-0002).
    #[error("net {net} has two drivers: {driver1} and {driver2} (STA-REQ-0002)")]
    MultipleDrivers {
        net: String,
        driver1: String,
        driver2: String,
    },
    /// A driver (input/inout port or output/inout/tristate pin) drives a constant net (STA-REQ-0002).
    #[error("{driver} drives a constant net (1'b{}) (STA-REQ-0002)", usize::from(*value))]
    DrivesConstant { driver: String, value: bool },
    /// A net equivalence class is assigned to both `1'b0` and `1'b1` (STA-REQ-0002).
    #[error("net {net} is tied to both 1'b0 and 1'b1 (STA-REQ-0002)")]
    ConflictingConstants { net: String },
    /// A cell in the netlist is missing from a corner's Liberty libraries (STA-REQ-0003).
    #[error("corner {corner}: unknown cell '{cell}' (STA-REQ-0003)")]
    UnknownCell { corner: usize, cell: String },
    /// A cell pin in the netlist is missing from a corner's Liberty cell (STA-REQ-0003).
    #[error("corner {corner}: cell '{cell}' has no pin '{pin}' (STA-REQ-0003)")]
    UnknownCellPin {
        corner: usize,
        cell: String,
        pin: String,
    },
    /// A cell pin's direction in a corner library disagrees with `CELL_PIN_DIR` (STA-REQ-0003).
    #[error(
        "corner {corner}: cell '{cell}' pin '{pin}' direction {found:?} does not match netlist direction {expected:?} (STA-REQ-0003)"
    )]
    PinDirectionMismatch {
        corner: usize,
        cell: String,
        pin: String,
        expected: Direction,
        found: Direction,
    },
    /// A cell pin's three-state attribute differs across corners (STA-REQ-0003).
    #[error(
        "corner {corner}: cell '{cell}' pin '{pin}' three-state attribute differs across corners (STA-REQ-0003)"
    )]
    TristateMismatch {
        corner: usize,
        cell: String,
        pin: String,
    },
}

impl NetlistViewError {
    /// Stable `T`-prefixed diagnostic code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            NetlistViewError::MultipleDrivers { .. } => "T0001",
            NetlistViewError::DrivesConstant { .. } => "T0002",
            NetlistViewError::ConflictingConstants { .. } => "T0003",
            NetlistViewError::UnknownCell { .. } => "T0004",
            NetlistViewError::UnknownCellPin { .. } => "T0005",
            NetlistViewError::PinDirectionMismatch { .. } => "T0006",
            NetlistViewError::TristateMismatch { .. } => "T0007",
        }
    }
}

/// On-demand formatter for a timing graph node's canonical name (`port`, `port[i]`, or `inst/pin`).
#[derive(Clone, Copy)]
pub struct NodeName<'a> {
    frozen: &'a FrozenNetlist<'a>,
    n_port_bits: u32,
    node: NodeId,
}

impl fmt::Display for NodeName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_node_name_raw(self.frozen, self.n_port_bits, self.node, f)
    }
}

impl fmt::Debug for NodeName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self)
    }
}

/// On-demand formatter for a net row's canonical name (`net`, `net[i]`, or `_net<row>`).
#[derive(Clone, Copy)]
pub struct NetName<'a> {
    frozen: &'a FrozenNetlist<'a>,
    net: NetRow,
}

impl fmt::Display for NetName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_net_name_raw(self.frozen, self.net, f)
    }
}

impl fmt::Debug for NetName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self)
    }
}

/// Zero-copy view over a [`FrozenNetlist`].
///
/// Stores only flat integer/CSR tables for `ASSIGNS` equivalence classes, port-bit mappings,
/// aliased drivers and loads, and three-state flags. No node or net holds an owned `String`.
#[derive(Debug, Clone)]
pub struct NetlistView<'a> {
    frozen: &'a FrozenNetlist<'a>,
    n_port_bits: u32,
    n_pins: u32,
    n_nodes: u32,
    n_nets: u32,
    /// For each port bit `0..n_port_bits`, its parent port index `0..ports.len()`.
    port_bit_port: Vec<u32>,
    /// For each raw net row `0..n_nets`, the port bit row `0..n_port_bits` if it is a port bit net,
    /// else `u32::MAX`.
    net_port_bit: Vec<u32>,
    /// For each cell pin slot `0..cell_pin_dir.len()`, true when the pin is a three-state output.
    cell_pin_tristate: Vec<bool>,
    /// For each raw net row `0..n_nets`, its resolved target (`0..n_nets`, `CONST0`, or `CONST1`).
    canon_target: Vec<u32>,
    /// Distinct canonical net rows after union-find, in increasing `NetRow` order.
    canonical_nets: Vec<NetRow>,
    /// Net rows that resolve to a constant (`false` = 0, `true` = 1).
    const_nets: Vec<(NetRow, bool)>,
    /// CSR offsets (`n_nets + 1`) and driver `NodeId`s for each canonical net row.
    net_driver_off: Vec<u32>,
    net_drivers: Vec<NodeId>,
    /// CSR offsets (`n_nets + 1`) and load `NodeId`s for each canonical net row.
    net_load_off: Vec<u32>,
    net_loads: Vec<NodeId>,
    /// CSR offsets (`n_nets + 1`) and port bit rows (`0..n_port_bits`) for each canonical net row.
    net_port_bit_off: Vec<u32>,
    net_port_bits: Vec<u32>,
    /// For each canonical net row, true if it is driven by any three-state or `inout` driver.
    net_has_tristate: Vec<bool>,
    /// Canonical net rows that have `>= 1` loads and `0` drivers.
    undriven_nets: Vec<NetRow>,
    /// Total count of unconnected instance input/inout pins across all instances.
    unconnected_input_count: usize,
    /// Aggregated non-fatal warnings (`T0010` for unconnected instance inputs).
    warnings: Vec<NetlistDiagnostic>,
}

impl<'a> NetlistView<'a> {
    /// Builds a [`NetlistView`] validating cell pin directions against a single corner's
    /// Liberty libraries.
    pub fn new(
        frozen: &'a FrozenNetlist<'a>,
        libs: &[Library],
    ) -> Result<NetlistView<'a>, NetlistViewError> {
        Self::with_corners(frozen, &[libs])
    }

    /// Builds a [`NetlistView`] without checking external Liberty libraries (using `CELL_PIN_DIR`
    /// from the frozen netlist directly).
    pub fn without_libraries(
        frozen: &'a FrozenNetlist<'a>,
    ) -> Result<NetlistView<'a>, NetlistViewError> {
        Self::with_corners(frozen, &[])
    }

    /// Builds a [`NetlistView`] validating cell pin directions and three-state attributes across
    /// all corners' Liberty libraries.
    pub fn with_corners(
        frozen: &'a FrozenNetlist<'a>,
        corners: &[&[Library]],
    ) -> Result<NetlistView<'a>, NetlistViewError> {
        let n_port_bits = frozen.port_bit_net.len() as u32;
        let n_pins = frozen.pin_net.len() as u32;
        let n_nodes = n_port_bits + n_pins;
        let n_nets = frozen.nets.len() as u32;

        // 1. Validate cell pins against each corner's libraries and record three-state pins.
        let mut cell_pin_tristate = vec![false; frozen.cell_pin_dir.len()];
        let mut cell_input_mask = vec![0u64; frozen.cells.len()];
        for (cell_row, &cell_name_id) in frozen.cells.iter().enumerate() {
            let pin_range = frozen.cell_pins(cell_row as u32);
            for (cp, slot) in pin_range.clone().enumerate() {
                let dir = decode_pin_dir(frozen.cell_pin_dir[slot]);
                if matches!(dir, Direction::Input | Direction::Inout) && cp < 64 {
                    cell_input_mask[cell_row] |= 1u64 << cp;
                }
            }
            if corners.is_empty() {
                continue;
            }
            let cell_name = str_of(frozen, cell_name_id);
            for (corner_idx, libs) in corners.iter().enumerate() {
                let lib_cell = find_lib_cell(libs, cell_name).ok_or_else(|| {
                    NetlistViewError::UnknownCell {
                        corner: corner_idx,
                        cell: cell_name.to_string(),
                    }
                })?;
                for slot in pin_range.clone() {
                    let pin_name = str_of(frozen, frozen.cell_pin_name[slot]);
                    let expected = decode_pin_dir(frozen.cell_pin_dir[slot]);
                    let lib_pin = lib_cell
                        .pins
                        .iter()
                        .find(|p| p.name == pin_name)
                        .ok_or_else(|| NetlistViewError::UnknownCellPin {
                            corner: corner_idx,
                            cell: cell_name.to_string(),
                            pin: pin_name.to_string(),
                        })?;
                    if lib_pin.direction != expected {
                        return Err(NetlistViewError::PinDirectionMismatch {
                            corner: corner_idx,
                            cell: cell_name.to_string(),
                            pin: pin_name.to_string(),
                            expected,
                            found: lib_pin.direction,
                        });
                    }
                    let is_ts = pin_is_tristate(lib_pin);
                    if corner_idx == 0 {
                        cell_pin_tristate[slot] = is_ts;
                    } else if cell_pin_tristate[slot] != is_ts {
                        return Err(NetlistViewError::TristateMismatch {
                            corner: corner_idx,
                            cell: cell_name.to_string(),
                            pin: pin_name.to_string(),
                        });
                    }
                }
            }
        }

        // 2. Build port_bit -> port map and net -> port_bit reverse map.
        let mut port_bit_port = vec![0u32; n_port_bits as usize];
        let mut net_port_bit = vec![u32::MAX; n_nets as usize];
        for port_row in 0..frozen.ports.len() {
            let start = frozen.port_bit_start[port_row] as usize;
            let end = frozen.port_bit_start[port_row + 1] as usize;
            for bit_row in start..end {
                port_bit_port[bit_row] = port_row as u32;
                let nr = frozen.port_bit_net[bit_row];
                if (nr as usize) < n_nets as usize {
                    net_port_bit[nr as usize] = bit_row as u32;
                }
            }
        }

        // 3. Union-find over ASSIGNS.
        // Priority rank for choosing the canonical representative of an equivalence class:
        //   0: Input or Inout port bit net
        //   1: Output port bit net
        //   2: Named internal net
        //   3: Anonymous internal net
        let net_rank = |nr: u32| -> u8 {
            let pb = net_port_bit[nr as usize];
            if pb != u32::MAX {
                let port_dir =
                    decode_port_dir(frozen.ports[port_bit_port[pb as usize] as usize].dir);
                if matches!(port_dir, Direction::Input | Direction::Inout) {
                    0
                } else {
                    1
                }
            } else if frozen.nets[nr as usize].name != ANONYMOUS {
                2
            } else {
                3
            }
        };

        let mut parent: Vec<u32> = (0..n_nets).collect();
        for assign in frozen.assigns.iter() {
            let (lhs, rhs) = (assign.lhs, assign.rhs);
            if lhs >= n_nets || rhs >= n_nets {
                continue;
            }
            let rl = uf_find(&mut parent, lhs);
            let rr = uf_find(&mut parent, rhs);
            if rl == rr {
                continue;
            }
            let (rank_l, rank_r) = (net_rank(rl), net_rank(rr));
            // Higher-priority rank (smaller number) stays representative; for equal rank on
            // port-bit nets, lower NetRow wins; for equal rank on internal nets, rhs wins.
            let keep_lhs = if rank_l != rank_r {
                rank_l < rank_r
            } else if rank_l <= 1 {
                rl < rr
            } else {
                false
            };
            if keep_lhs {
                parent[rr as usize] = rl;
            } else {
                parent[rl as usize] = rr;
            }
        }

        // Compress paths and apply constant assignments (CONST0 / CONST1).
        let mut canon_target = vec![0u32; n_nets as usize];
        for nr in 0..n_nets {
            canon_target[nr as usize] = uf_find(&mut parent, nr);
        }
        // Track constant assignments on canonical representatives.
        // 0 = none, 1 = CONST0, 2 = CONST1.
        let mut rep_const = vec![0u8; n_nets as usize];
        for assign in frozen.assigns.iter() {
            if assign.lhs >= n_nets {
                continue;
            }
            let c = match assign.rhs {
                CONST0 => 1u8,
                CONST1 => 2u8,
                _ => continue,
            };
            let rep = canon_target[assign.lhs as usize] as usize;
            if rep_const[rep] != 0 && rep_const[rep] != c {
                return Err(NetlistViewError::ConflictingConstants {
                    net: format_net_name_raw(frozen, NetRow(rep as u32)).into_owned(),
                });
            }
            rep_const[rep] = c;
        }
        let mut const_nets = Vec::new();
        let mut canonical_nets = Vec::new();
        for nr in 0..n_nets {
            let rep = canon_target[nr as usize] as usize;
            match rep_const[rep] {
                1 => {
                    canon_target[nr as usize] = CONST0;
                    const_nets.push((NetRow(nr), false));
                }
                2 => {
                    canon_target[nr as usize] = CONST1;
                    const_nets.push((NetRow(nr), true));
                }
                _ => {
                    if rep as u32 == nr {
                        canonical_nets.push(NetRow(nr));
                    }
                }
            }
        }

        // 4. Reverse map from canonical net row to all aliased port_bits on that net.
        let mut net_port_bit_off = vec![0u32; n_nets as usize + 1];
        for pb in 0..n_port_bits {
            let raw_net = frozen.port_bit_net[pb as usize];
            if (raw_net as usize) < n_nets as usize {
                let target = canon_target[raw_net as usize];
                if target < n_nets {
                    net_port_bit_off[target as usize + 1] += 1;
                }
            }
        }
        for i in 0..n_nets as usize {
            net_port_bit_off[i + 1] += net_port_bit_off[i];
        }
        let mut fill_pb = net_port_bit_off.clone();
        let mut net_port_bits = vec![0u32; net_port_bit_off[n_nets as usize] as usize];
        for pb in 0..n_port_bits {
            let raw_net = frozen.port_bit_net[pb as usize];
            if (raw_net as usize) < n_nets as usize {
                let target = canon_target[raw_net as usize];
                if target < n_nets {
                    let pos = fill_pb[target as usize] as usize;
                    net_port_bits[pos] = pb;
                    fill_pb[target as usize] += 1;
                }
            }
        }

        // 5. Two-pass CSR construction of drivers and loads per canonical net row, plus constant-drive
        // and multiple-driver checks.
        let mut net_driver_off = vec![0u32; n_nets as usize + 1];
        let mut net_load_off = vec![0u32; n_nets as usize + 1];
        let mut net_has_tristate = vec![false; n_nets as usize];

        let port_bit_props = |pb: u32| -> (u32, bool, bool, bool, bool) {
            let port_row = port_bit_port[pb as usize];
            let dir = decode_port_dir(frozen.ports[port_row as usize].dir);
            let raw_net = frozen.port_bit_net[pb as usize];
            let target = canon_target[raw_net as usize];
            let drives = matches!(dir, Direction::Input | Direction::Inout);
            let loads = matches!(dir, Direction::Output | Direction::Inout);
            let is_inout = dir == Direction::Inout;
            (target, drives, loads, false, is_inout)
        };

        let pin_row_props = |inst_row: usize, pin_row: usize| -> (u32, bool, bool, bool, bool) {
            let cell_row = frozen.instances[inst_row].cell as usize;
            let cp = frozen.pin_cellpin[pin_row] as usize;
            let slot = frozen.cell_pin_start[cell_row] as usize + cp;
            let dir = decode_pin_dir(frozen.cell_pin_dir[slot]);
            let is_ts = cell_pin_tristate[slot];
            let raw_net = frozen.pin_net[pin_row];
            let target = if raw_net < n_nets {
                canon_target[raw_net as usize]
            } else {
                raw_net
            };
            let drives = matches!(dir, Direction::Output | Direction::Inout);
            let loads = matches!(dir, Direction::Input | Direction::Inout);
            let is_inout = dir == Direction::Inout;
            (target, drives, loads, is_ts, is_inout)
        };

        for pb in 0..n_port_bits {
            let (target, drives, loads, is_ts, is_inout) = port_bit_props(pb);
            if target == CONST0 || target == CONST1 {
                if drives {
                    let mut driver = String::new();
                    let _ = write_node_name_raw(frozen, n_port_bits, NodeId(pb), &mut driver);
                    return Err(NetlistViewError::DrivesConstant {
                        driver,
                        value: target == CONST1,
                    });
                }
                continue;
            }
            if target < n_nets {
                let rep = target as usize;
                if drives {
                    net_driver_off[rep + 1] += 1;
                    if is_ts || is_inout {
                        net_has_tristate[rep] = true;
                    }
                }
                if loads {
                    net_load_off[rep + 1] += 1;
                }
            }
        }

        for inst_row in 0..frozen.instances.len() {
            for pin_row in frozen.instance_pins(inst_row as u32) {
                let v = n_port_bits + pin_row as u32;
                let (target, drives, loads, is_ts, is_inout) = pin_row_props(inst_row, pin_row);
                if target == CONST0 || target == CONST1 {
                    if drives {
                        let mut driver = String::new();
                        let _ = write_node_name_raw(frozen, n_port_bits, NodeId(v), &mut driver);
                        return Err(NetlistViewError::DrivesConstant {
                            driver,
                            value: target == CONST1,
                        });
                    }
                    continue;
                }
                if target < n_nets {
                    let rep = target as usize;
                    if drives {
                        net_driver_off[rep + 1] += 1;
                        if is_ts || is_inout {
                            net_has_tristate[rep] = true;
                        }
                    }
                    if loads {
                        net_load_off[rep + 1] += 1;
                    }
                }
            }
        }

        for i in 0..n_nets as usize {
            net_driver_off[i + 1] += net_driver_off[i];
            net_load_off[i + 1] += net_load_off[i];
        }

        let mut fill_drv = net_driver_off.clone();
        let mut fill_ld = net_load_off.clone();
        let mut net_drivers = vec![NodeId(0); net_driver_off[n_nets as usize] as usize];
        let mut net_loads = vec![NodeId(0); net_load_off[n_nets as usize] as usize];

        for pb in 0..n_port_bits {
            let (target, drives, loads, _, _) = port_bit_props(pb);
            if target < n_nets {
                let rep = target as usize;
                if drives {
                    let pos = fill_drv[rep] as usize;
                    net_drivers[pos] = NodeId(pb);
                    fill_drv[rep] += 1;
                }
                if loads {
                    let pos = fill_ld[rep] as usize;
                    net_loads[pos] = NodeId(pb);
                    fill_ld[rep] += 1;
                }
            }
        }

        for inst_row in 0..frozen.instances.len() {
            for pin_row in frozen.instance_pins(inst_row as u32) {
                let v = n_port_bits + pin_row as u32;
                let (target, drives, loads, _, _) = pin_row_props(inst_row, pin_row);
                if target < n_nets {
                    let rep = target as usize;
                    if drives {
                        let pos = fill_drv[rep] as usize;
                        net_drivers[pos] = NodeId(v);
                        fill_drv[rep] += 1;
                    }
                    if loads {
                        let pos = fill_ld[rep] as usize;
                        net_loads[pos] = NodeId(v);
                        fill_ld[rep] += 1;
                    }
                }
            }
        }

        let node_is_ts_or_inout = |v: u32| -> (bool, bool) {
            if v < n_port_bits {
                let (_, _, _, is_ts, is_inout) = port_bit_props(v);
                (is_ts, is_inout)
            } else {
                let pin_row = (v - n_port_bits) as usize;
                let inst_row = frozen.pin_instance(pin_row as u32) as usize;
                let (_, _, _, is_ts, is_inout) = pin_row_props(inst_row, pin_row);
                (is_ts, is_inout)
            }
        };

        // Check multiple-driver rule on each canonical net.
        let mut undriven_nets = Vec::new();
        for &net in &canonical_nets {
            let rep = net.index();
            let drvs = &net_drivers[net_driver_off[rep] as usize..net_driver_off[rep + 1] as usize];
            let lds = &net_loads[net_load_off[rep] as usize..net_load_off[rep + 1] as usize];
            if drvs.is_empty() {
                if !lds.is_empty() {
                    undriven_nets.push(net);
                }
                continue;
            }
            if drvs.len() > 1 {
                let mut hard_drivers = 0usize;
                let mut non_inout_drivers = 0usize;
                for &d in drvs {
                    let (is_ts, is_inout) = node_is_ts_or_inout(d.0);
                    if !is_inout {
                        non_inout_drivers += 1;
                        if !is_ts {
                            hard_drivers += 1;
                        }
                    }
                }
                if non_inout_drivers >= 2 && hard_drivers > 0 {
                    let mut conflict = drvs.iter().copied().filter(|d| !node_is_ts_or_inout(d.0).1);
                    let d1 = conflict.next().unwrap_or(drvs[0]);
                    let d2 = conflict.next().unwrap_or(drvs[1]);
                    let mut driver1 = String::new();
                    let mut driver2 = String::new();
                    let _ = write_node_name_raw(frozen, n_port_bits, d1, &mut driver1);
                    let _ = write_node_name_raw(frozen, n_port_bits, d2, &mut driver2);
                    return Err(NetlistViewError::MultipleDrivers {
                        net: format_net_name_raw(frozen, net).into_owned(),
                        driver1,
                        driver2,
                    });
                }
            }
        }

        // 6. Check for unconnected instance input/inout pins and emit at most one aggregated warning.
        let mut unconnected_input_count = 0usize;
        let mut unconnected_samples: Vec<String> = Vec::new();
        for (inst_row, inst_rec) in frozen.instances.iter().enumerate() {
            let cell_row = inst_rec.cell as usize;
            let cell_pins = frozen.cell_pins(cell_row as u32);
            let n_cell_pins = cell_pins.len();
            let inst_pins = frozen.instance_pins(inst_row as u32);

            if n_cell_pins <= 64 {
                let mut connected_mask = 0u64;
                for p in inst_pins {
                    let cp = frozen.pin_cellpin[p];
                    if (cp as usize) < 64 {
                        connected_mask |= 1u64 << cp;
                    }
                }
                let mut missing = cell_input_mask[cell_row] & !connected_mask;
                if missing != 0 {
                    while missing != 0 {
                        let cp = missing.trailing_zeros() as usize;
                        missing &= missing - 1;
                        unconnected_input_count += 1;
                        if unconnected_samples.len() < 5 {
                            let inst_name = str_of(frozen, inst_rec.name);
                            let pin_name =
                                str_of(frozen, frozen.cell_pin_name[cell_pins.start + cp]);
                            unconnected_samples.push(format!("{inst_name}/{pin_name}"));
                        }
                    }
                }
            } else {
                let mut connected = vec![false; n_cell_pins];
                for p in inst_pins {
                    let cp = frozen.pin_cellpin[p] as usize;
                    if cp < n_cell_pins {
                        connected[cp] = true;
                    }
                }
                for (cp, &is_conn) in connected.iter().enumerate() {
                    let dir = decode_pin_dir(frozen.cell_pin_dir[cell_pins.start + cp]);
                    if !is_conn && matches!(dir, Direction::Input | Direction::Inout) {
                        unconnected_input_count += 1;
                        if unconnected_samples.len() < 5 {
                            let inst_name = str_of(frozen, inst_rec.name);
                            let pin_name =
                                str_of(frozen, frozen.cell_pin_name[cell_pins.start + cp]);
                            unconnected_samples.push(format!("{inst_name}/{pin_name}"));
                        }
                    }
                }
            }
        }

        let mut warnings = Vec::new();
        if unconnected_input_count > 0 {
            let sample_list = unconnected_samples.join(", ");
            let suffix = if unconnected_input_count > unconnected_samples.len() {
                format!(
                    " (and {} more)",
                    unconnected_input_count - unconnected_samples.len()
                )
            } else {
                String::new()
            };
            warnings.push(NetlistDiagnostic {
                code: "T0010",
                message: format!(
                    "{unconnected_input_count} unconnected instance input pin(s): {sample_list}{suffix} (STA-REQ-0002)"
                ),
            });
        }

        Ok(NetlistView {
            frozen,
            n_port_bits,
            n_pins,
            n_nodes,
            n_nets,
            port_bit_port,
            net_port_bit,
            cell_pin_tristate,
            canon_target,
            canonical_nets,
            const_nets,
            net_driver_off,
            net_drivers,
            net_load_off,
            net_loads,
            net_port_bit_off,
            net_port_bits,
            net_has_tristate,
            undriven_nets,
            unconnected_input_count,
            warnings,
        })
    }

    /// Underlying [`FrozenNetlist`].
    #[inline]
    pub fn frozen(&self) -> &'a FrozenNetlist<'a> {
        self.frozen
    }

    /// Top module name.
    #[inline]
    pub fn module_name(&self) -> &'a str {
        str_of(self.frozen, self.frozen.module[module_entry::NAME])
    }

    /// Number of top-level port bits (`0..num_port_bits()`).
    #[inline]
    pub fn num_port_bits(&self) -> usize {
        self.n_port_bits as usize
    }

    /// Number of connected instance pin rows (`0..num_pins()`).
    #[inline]
    pub fn num_pins(&self) -> usize {
        self.n_pins as usize
    }

    /// Total number of timing graph nodes (`num_port_bits() + num_pins()`).
    #[inline]
    pub fn num_nodes(&self) -> usize {
        self.n_nodes as usize
    }

    /// Total number of raw net rows in `NETS`.
    #[inline]
    pub fn num_nets(&self) -> usize {
        self.n_nets as usize
    }

    /// Number of top-level ports.
    #[inline]
    pub fn num_ports(&self) -> usize {
        self.frozen.ports.len()
    }

    /// Number of cell instances.
    #[inline]
    pub fn num_instances(&self) -> usize {
        self.frozen.instances.len()
    }

    /// Number of cell types in `CELLS`.
    #[inline]
    pub fn num_cells(&self) -> usize {
        self.frozen.cells.len()
    }

    /// [`NodeId`] of top-level port bit `port_bit` (`0..num_port_bits()`).
    #[inline]
    pub fn port_bit_node(&self, port_bit: u32) -> NodeId {
        debug_assert!(port_bit < self.n_port_bits);
        NodeId(port_bit)
    }

    /// [`NodeId`] of connected instance pin row `pin_row` (`0..num_pins()`).
    #[inline]
    pub fn pin_row_node(&self, pin_row: u32) -> NodeId {
        debug_assert!(pin_row < self.n_pins);
        NodeId(self.n_port_bits + pin_row)
    }

    /// True if `node` is a top-level port bit.
    #[inline]
    pub fn is_port(&self, node: NodeId) -> bool {
        node.0 < self.n_port_bits
    }

    /// True if `node` is a connected instance pin.
    #[inline]
    pub fn is_pin(&self, node: NodeId) -> bool {
        node.0 >= self.n_port_bits && node.0 < self.n_nodes
    }

    /// Describes `node` in O(1) directly from the frozen netlist tables.
    #[inline]
    pub fn node_kind(&self, node: NodeId) -> NodeKind {
        if node.0 < self.n_port_bits {
            let pb = node.0;
            let port = self.port_bit_port[pb as usize];
            let nr = self.frozen.port_bit_net[pb as usize];
            let bit_index = self.frozen.nets[nr as usize].index;
            let dir = decode_port_dir(self.frozen.ports[port as usize].dir);
            NodeKind::PortBit {
                port_bit: pb,
                port,
                bit_index,
                dir,
            }
        } else {
            let pin_row = node.0 - self.n_port_bits;
            let inst = self.frozen.pin_instance(pin_row);
            let cell = self.frozen.instances[inst as usize].cell;
            let cell_pin = self.frozen.pin_cellpin[pin_row as usize];
            let slot = self.frozen.cell_pin_start[cell as usize] as usize + cell_pin as usize;
            let dir = decode_pin_dir(self.frozen.cell_pin_dir[slot]);
            let tristate = self.cell_pin_tristate[slot];
            NodeKind::InstancePin {
                pin_row,
                inst,
                cell,
                cell_pin,
                dir,
                tristate,
            }
        }
    }

    /// True if `node` can drive a net (`Input`/`Inout` port or `Output`/`Inout` instance pin).
    #[inline]
    pub fn is_driver(&self, node: NodeId) -> bool {
        match self.node_kind(node) {
            NodeKind::PortBit { dir, .. } => matches!(dir, Direction::Input | Direction::Inout),
            NodeKind::InstancePin { dir, .. } => {
                matches!(dir, Direction::Output | Direction::Inout)
            }
        }
    }

    /// True if `node` is a load on its net (`Output`/`Inout` port or `Input`/`Inout` instance pin).
    #[inline]
    pub fn is_load(&self, node: NodeId) -> bool {
        match self.node_kind(node) {
            NodeKind::PortBit { dir, .. } => matches!(dir, Direction::Output | Direction::Inout),
            NodeKind::InstancePin { dir, .. } => matches!(dir, Direction::Input | Direction::Inout),
        }
    }

    /// True if `node` is a three-state output instance pin.
    #[inline]
    pub fn is_tristate_pin(&self, node: NodeId) -> bool {
        matches!(
            self.node_kind(node),
            NodeKind::InstancePin { tristate: true, .. }
        )
    }

    /// Resolves raw `net` through `ASSIGNS` union-find to its canonical [`NetRow`] or constant.
    #[inline]
    pub fn resolve_net(&self, net: NetRow) -> NetTarget {
        let target = self.canon_target[net.index()];
        match target {
            CONST0 => NetTarget::Const(false),
            CONST1 => NetTarget::Const(true),
            rep => NetTarget::Net(NetRow(rep)),
        }
    }

    /// Resolved [`NetTarget`] connected to `node`.
    #[inline]
    pub fn node_net(&self, node: NodeId) -> NetTarget {
        let raw = if node.0 < self.n_port_bits {
            self.frozen.port_bit_net[node.0 as usize]
        } else {
            self.frozen.pin_net[(node.0 - self.n_port_bits) as usize]
        };
        match raw {
            CONST0 => NetTarget::Const(false),
            CONST1 => NetTarget::Const(true),
            r => self.resolve_net(NetRow(r)),
        }
    }

    /// If `node` is connected to a constant net (`1'b0` or `1'b1`), returns `Some(false)` or
    /// `Some(true)`; otherwise `None`.
    #[inline]
    pub fn is_const_node(&self, node: NodeId) -> Option<bool> {
        match self.node_net(node) {
            NetTarget::Const(v) => Some(v),
            NetTarget::Net(_) => None,
        }
    }

    /// Slice of all distinct canonical net rows (one representative per non-constant equivalence
    /// class).
    #[inline]
    pub fn canonical_nets(&self) -> &[NetRow] {
        &self.canonical_nets
    }

    /// Slice of all raw net rows that resolve to a constant `(NetRow, value)`.
    #[inline]
    pub fn const_nets(&self) -> &[(NetRow, bool)] {
        &self.const_nets
    }

    /// Driver [`NodeId`]s of `net`'s equivalence class (empty if undriven or constant, 1 for a
    /// single-driver net, `>= 1` for a tri-state or `inout` bus).
    #[inline]
    pub fn drivers(&self, net: NetRow) -> &[NodeId] {
        match self.resolve_net(net) {
            NetTarget::Net(rep) => {
                let r = rep.index();
                &self.net_drivers
                    [self.net_driver_off[r] as usize..self.net_driver_off[r + 1] as usize]
            }
            NetTarget::Const(_) => &[],
        }
    }

    /// First driver [`NodeId`] of `net`'s equivalence class, if any.
    #[inline]
    pub fn driver(&self, net: NetRow) -> Option<NodeId> {
        self.drivers(net).first().copied()
    }

    /// Load [`NodeId`]s of `net`'s equivalence class (both connected instance input/inout pins and
    /// top-level output/inout port bits).
    #[inline]
    pub fn loads(&self, net: NetRow) -> &[NodeId] {
        match self.resolve_net(net) {
            NetTarget::Net(rep) => {
                let r = rep.index();
                &self.net_loads[self.net_load_off[r] as usize..self.net_load_off[r + 1] as usize]
            }
            NetTarget::Const(_) => &[],
        }
    }

    /// True if `net`'s equivalence class has any three-state or `inout` driver.
    #[inline]
    pub fn has_tristate(&self, net: NetRow) -> bool {
        match self.resolve_net(net) {
            NetTarget::Net(rep) => self.net_has_tristate[rep.index()],
            NetTarget::Const(_) => false,
        }
    }

    /// If raw `net` is the dedicated net row of a top-level port bit, returns that port bit row
    /// (`0..num_port_bits()`).
    #[inline]
    pub fn port_bit_for_net(&self, net: NetRow) -> Option<u32> {
        let pb = self.net_port_bit.get(net.index()).copied()?;
        (pb != u32::MAX).then_some(pb)
    }

    /// All top-level port bit rows (`0..num_port_bits()`) in `net`'s equivalence class.
    #[inline]
    pub fn port_bits_on_net(&self, net: NetRow) -> &[u32] {
        match self.resolve_net(net) {
            NetTarget::Net(rep) => {
                let r = rep.index();
                &self.net_port_bits
                    [self.net_port_bit_off[r] as usize..self.net_port_bit_off[r + 1] as usize]
            }
            NetTarget::Const(_) => &[],
        }
    }

    /// Canonical net rows that have `>= 1` loads and `0` drivers.
    #[inline]
    pub fn undriven_nets(&self) -> &[NetRow] {
        &self.undriven_nets
    }

    /// Total count of unconnected instance input/inout pins across the netlist.
    #[inline]
    pub fn unconnected_input_count(&self) -> usize {
        self.unconnected_input_count
    }

    /// Non-fatal warnings recorded during construction (`T0010` for unconnected inputs).
    #[inline]
    pub fn warnings(&self) -> &[NetlistDiagnostic] {
        &self.warnings
    }

    /// Zero-allocation [`fmt::Display`] handle for `node`'s canonical name.
    #[inline]
    pub fn node_name(&self, node: NodeId) -> NodeName<'a> {
        NodeName {
            frozen: self.frozen,
            n_port_bits: self.n_port_bits,
            node,
        }
    }

    /// Returns `node`'s canonical name, borrowing directly from `STR_BYTES` for scalar ports and
    /// formatting `port[i]` or `inst/pin` on demand otherwise.
    pub fn node_name_str(&self, node: NodeId) -> Cow<'a, str> {
        if node.0 < self.n_port_bits {
            let nr = self.frozen.port_bit_net[node.0 as usize];
            let net_rec = &self.frozen.nets[nr as usize];
            let base = str_of(self.frozen, net_rec.name);
            if net_rec.index == SCALAR {
                Cow::Borrowed(base)
            } else {
                Cow::Owned(format!("{base}[{}]", net_rec.index))
            }
        } else {
            let mut s = String::new();
            let _ = self.write_node_name(node, &mut s);
            Cow::Owned(s)
        }
    }

    /// Writes `node`'s canonical name to `out` without intermediate allocation.
    #[inline]
    pub fn write_node_name(&self, node: NodeId, out: &mut impl fmt::Write) -> fmt::Result {
        write_node_name_raw(self.frozen, self.n_port_bits, node, out)
    }

    /// Zero-allocation [`fmt::Display`] handle for `net`'s canonical name.
    #[inline]
    pub fn net_name(&self, net: NetRow) -> NetName<'a> {
        NetName {
            frozen: self.frozen,
            net,
        }
    }

    /// Returns `net`'s name, borrowing directly from `STR_BYTES` for named scalar nets.
    pub fn net_name_str(&self, net: NetRow) -> Cow<'a, str> {
        format_net_name_raw(self.frozen, net)
    }

    /// Writes `net`'s name to `out` without intermediate allocation.
    #[inline]
    pub fn write_net_name(&self, net: NetRow, out: &mut impl fmt::Write) -> fmt::Result {
        write_net_name_raw(self.frozen, net, out)
    }

    /// Name of top-level port `port` (`0..num_ports()`).
    #[inline]
    pub fn port_name(&self, port: u32) -> &'a str {
        str_of(self.frozen, self.frozen.ports[port as usize].name)
    }

    /// Name of instance `inst` (`0..num_instances()`).
    #[inline]
    pub fn instance_name(&self, inst: u32) -> &'a str {
        str_of(self.frozen, self.frozen.instances[inst as usize].name)
    }

    /// Name of cell `cell` (`0..num_cells()`).
    #[inline]
    pub fn cell_name(&self, cell: u32) -> &'a str {
        str_of(self.frozen, self.frozen.cells[cell as usize])
    }

    /// Name of pin `cell_pin` (`0..cell_pins(cell).len()`) of `cell`.
    #[inline]
    pub fn cell_pin_name(&self, cell: u32, cell_pin: u32) -> &'a str {
        let slot = self.frozen.cell_pin_start[cell as usize] as usize + cell_pin as usize;
        str_of(self.frozen, self.frozen.cell_pin_name[slot])
    }
}

#[inline]
fn str_of<'a>(frozen: &'a FrozenNetlist<'a>, id: u32) -> &'a str {
    frozen.string(id).expect("validated frozen netlist string")
}

#[inline]
fn decode_pin_dir(code: u8) -> Direction {
    match code {
        0 => Direction::Input,
        1 => Direction::Output,
        2 => Direction::Inout,
        _ => Direction::Internal,
    }
}

#[inline]
fn decode_port_dir(code: u8) -> Direction {
    match code {
        0 => Direction::Input,
        1 => Direction::Output,
        _ => Direction::Inout,
    }
}

fn find_lib_cell<'a>(libs: &'a [Library], name: &str) -> Option<&'a Cell> {
    libs.iter().flat_map(|l| &l.cells).find(|c| c.name == name)
}

fn pin_is_tristate(pin: &Pin) -> bool {
    pin.three_state.is_some()
        || pin.arcs.iter().any(|a| {
            matches!(
                a.kind(),
                TimingType::ThreeStateEnable
                    | TimingType::ThreeStateEnableRise
                    | TimingType::ThreeStateEnableFall
                    | TimingType::ThreeStateDisable
                    | TimingType::ThreeStateDisableRise
                    | TimingType::ThreeStateDisableFall
            )
        })
}

#[inline]
fn uf_find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        let p = parent[x as usize];
        parent[x as usize] = parent[p as usize];
        x = p;
    }
    x
}

fn write_node_name_raw(
    frozen: &FrozenNetlist<'_>,
    n_port_bits: u32,
    node: NodeId,
    out: &mut impl fmt::Write,
) -> fmt::Result {
    if node.0 < n_port_bits {
        let nr = frozen.port_bit_net[node.0 as usize];
        write_net_name_raw(frozen, NetRow(nr), out)
    } else {
        let pin_row = node.0 - n_port_bits;
        let inst_row = frozen.pin_instance(pin_row) as usize;
        let inst_rec = &frozen.instances[inst_row];
        let inst_name = str_of(frozen, inst_rec.name);
        let cp = frozen.pin_cellpin[pin_row as usize] as usize;
        let slot = frozen.cell_pin_start[inst_rec.cell as usize] as usize + cp;
        let pin_name = str_of(frozen, frozen.cell_pin_name[slot]);
        write!(out, "{inst_name}/{pin_name}")
    }
}

fn write_net_name_raw(
    frozen: &FrozenNetlist<'_>,
    net: NetRow,
    out: &mut impl fmt::Write,
) -> fmt::Result {
    let rec = &frozen.nets[net.index()];
    if rec.name == ANONYMOUS {
        write!(out, "_net{}", net.0)
    } else {
        let base = str_of(frozen, rec.name);
        if rec.index == SCALAR {
            out.write_str(base)
        } else {
            write!(out, "{base}[{}]", rec.index)
        }
    }
}

fn format_net_name_raw<'a>(frozen: &'a FrozenNetlist<'a>, net: NetRow) -> Cow<'a, str> {
    let rec = &frozen.nets[net.index()];
    if rec.name == ANONYMOUS {
        Cow::Owned(format!("_net{}", net.0))
    } else {
        let base = str_of(frozen, rec.name);
        if rec.index == SCALAR {
            Cow::Borrowed(base)
        } else {
            Cow::Owned(format!("{base}[{}]", rec.index))
        }
    }
}
