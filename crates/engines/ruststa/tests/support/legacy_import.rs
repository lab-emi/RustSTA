// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Test adapter from the legacy flattener to the shared netlist model (owner: E1).
//!
//! Until the workspace imports structural Verilog, the tests of the new engine read the corpus
//! netlists with the Verilog reader and the legacy flattener of the crate root
//! ([`ruststa::netlist::flatten`]), build the shared `GateNetlist` of `rusteda-core` from the
//! flat netlist, and freeze it into the `FrozenNetlist` that the engine reads. [`import`] does the
//! first part and returns the flat netlist as the building steps of a `GateNetlist`, in call
//! order:
//!
//! 1. `GateNetlist::new(module)`;
//! 2. `add_cell_type(name, pins)` per entry of [`LegacyImport::cells`]: the cells in order of
//!    first use, each with its pins in library order and their Liberty directions;
//! 3. `add_port(name, dir, range)` per entry of [`LegacyImport::ports`], in declaration order,
//!    which creates the nets of the port bits;
//! 4. `add_net(Some(name), None)` per entry of [`LegacyImport::nets`]: the nets without a port
//!    bit, in the order of the flat netlist;
//! 5. `add_instance(name, cell)`, then `connect(instance, pin, conn)` per connection, per entry
//!    of [`LegacyImport::instances`], in elaboration order, connections in source order;
//! 6. `add_assign(lhs, rhs)` per entry of [`LegacyImport::assigns`].
//!
//! The legacy flattener has already joined the nets of each `assign` statement and module port
//! connection (STA-REQ-0001). The steps map its flat nets as follows:
//!
//! - a flat net with port bits is the net of its first input port bit, else of its first port
//!   bit (ports in declaration order, bits from the most significant declared bit); every other
//!   port bit on the net is assigned from that net;
//! - a port bit on a constant is assigned the constant, and an instance pin on a constant is
//!   connected to it;
//! - every other flat net is a scalar net with the flat netlist's name: hierarchical names joined
//!   by `/`, bus bits written `name[index]`.
//!
//! The names keep the flat netlist's spelling, so an escaped Verilog identifier keeps neither its
//! backslash nor an escape of a `/` in it; a name with a backslash or an empty segment is an
//! error. The legacy flattener supports input and output ports only. Work item E1 adds the
//! `GateNetlist` calls and the freeze once the shared netlist model is part of `rusteda-core`.

use std::collections::{HashMap, HashSet};

use rusteda_core::liberty::{Cell, Direction, Library};
use rusteda_core::netlist::{FrozenNetlist, GateNetlist};
use rusteda_formats::verilog::netlist::read_structural;
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};
use ruststa::netlist::{CONST0, CONST1, Flat, NetId, flatten};

/// The direction of a cell pin in the shared netlist model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDir {
    Input,
    Output,
    Inout,
    /// Any other pin, for example an internal one.
    Other,
}

/// The direction of a top-level port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDir {
    Input,
    Output,
}

/// A cell type: its name and its pins in library order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellType {
    pub name: String,
    pub pins: Vec<(String, PinDir)>,
}

/// A top-level port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: String,
    pub dir: PortDir,
    /// `(msb, lsb)` of a bus, `None` for a scalar port.
    pub range: Option<(i32, i32)>,
}

impl Port {
    /// The bit indices in declaration order, from `msb` to `lsb`; `[None]` for a scalar port.
    pub fn bits(&self) -> Vec<Option<i32>> {
        match self.range {
            None => vec![None],
            Some((msb, lsb)) if msb >= lsb => (lsb..=msb).rev().map(Some).collect(),
            Some((msb, lsb)) => (msb..=lsb).map(Some).collect(),
        }
    }

    /// The flat netlist's name of the bit at `index`: `name[index]`, or `name` when scalar.
    pub fn bit_name(&self, index: Option<i32>) -> String {
        match index {
            Some(i) => format!("{}[{i}]", self.name),
            None => self.name.clone(),
        }
    }
}

/// A net of the building steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetRef {
    /// The bit at position `bit` (in [`Port::bits`] order) of the port at position `port`.
    PortBit { port: usize, bit: usize },
    /// The net at this position of [`LegacyImport::nets`].
    Net(usize),
}

/// What a pin or the right-hand side of an assignment connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conn {
    Net(NetRef),
    Const(bool),
}

/// A cell instance and its connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub name: String,
    /// Position in [`LegacyImport::cells`].
    pub cell: usize,
    /// `(pin name, connection)` in source order; unconnected pins are not listed.
    pub conns: Vec<(String, Conn)>,
}

/// An assignment `lhs = rhs`; `lhs` is always a port bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assign {
    pub lhs: NetRef,
    pub rhs: Conn,
}

/// A flat netlist as the building steps of the shared netlist model (see the module
/// documentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyImport {
    pub module: String,
    pub cells: Vec<CellType>,
    pub ports: Vec<Port>,
    /// The names of the nets without a port bit.
    pub nets: Vec<String>,
    pub instances: Vec<Instance>,
    pub assigns: Vec<Assign>,
}

impl LegacyImport {
    /// Executes the building steps on a [`GateNetlist`].
    pub fn build(&self) -> Result<GateNetlist, String> {
        let mut g = GateNetlist::new(&self.module).map_err(|e| e.to_string())?;
        let mut cell_ids = Vec::with_capacity(self.cells.len());
        for c in &self.cells {
            let pins: Vec<(&str, rusteda_core::netlist::PinDir)> = c
                .pins
                .iter()
                .map(|(n, d)| {
                    let dir = match d {
                        PinDir::Input => rusteda_core::netlist::PinDir::Input,
                        PinDir::Output => rusteda_core::netlist::PinDir::Output,
                        PinDir::Inout => rusteda_core::netlist::PinDir::Inout,
                        PinDir::Other => rusteda_core::netlist::PinDir::Other,
                    };
                    (n.as_str(), dir)
                })
                .collect();
            let id = g.add_cell_type(&c.name, &pins).map_err(|e| e.to_string())?;
            cell_ids.push(id);
        }
        let mut port_bit_ids: Vec<Vec<rusteda_core::netlist::NetId>> =
            Vec::with_capacity(self.ports.len());
        for p in &self.ports {
            let dir = match p.dir {
                PortDir::Input => rusteda_core::netlist::PortDir::Input,
                PortDir::Output => rusteda_core::netlist::PortDir::Output,
            };
            let idx = g
                .add_port(&p.name, dir, p.range)
                .map_err(|e| e.to_string())?;
            port_bit_ids.push(g.ports()[idx].bits.clone());
        }
        let mut net_ids = Vec::with_capacity(self.nets.len());
        for n in &self.nets {
            let id = g
                .add_net(Some(n.as_str()), None)
                .map_err(|e| e.to_string())?;
            net_ids.push(id);
        }
        let resolve_ref = |r: NetRef| -> rusteda_core::netlist::NetId {
            match r {
                NetRef::PortBit { port, bit } => port_bit_ids[port][bit],
                NetRef::Net(k) => net_ids[k],
            }
        };
        let resolve_conn = |c: Conn| -> rusteda_core::netlist::Conn {
            match c {
                Conn::Net(r) => rusteda_core::netlist::Conn::Net(resolve_ref(r)),
                Conn::Const(v) => rusteda_core::netlist::Conn::Const(v),
            }
        };
        for inst in &self.instances {
            let id = g
                .add_instance(&inst.name, cell_ids[inst.cell])
                .map_err(|e| e.to_string())?;
            for (pin, conn) in &inst.conns {
                g.connect(id, pin.as_str(), resolve_conn(*conn))
                    .map_err(|e| e.to_string())?;
            }
        }
        for a in &self.assigns {
            g.add_assign(resolve_ref(a.lhs), resolve_conn(a.rhs))
                .map_err(|e| e.to_string())?;
        }
        Ok(g)
    }

    /// Builds and freezes the netlist into a [`FrozenNetlist`].
    pub fn freeze(&self) -> Result<FrozenNetlist<'static>, String> {
        Ok(self.build()?.freeze())
    }
}

/// Reads the netlist `text` (file name `name`) and returns its building steps; `top` names the
/// top module, else the netlist must have exactly one.
pub fn import(
    name: &str,
    text: &str,
    top: Option<&str>,
    libs: &[Library],
) -> Result<LegacyImport, String> {
    steps(&read_flat(name, text, top, libs)?, libs)
}

/// Reads the netlist `text` via [`import`] and builds a [`GateNetlist`].
pub fn import_netlist(
    name: &str,
    text: &str,
    top: Option<&str>,
    libs: &[Library],
) -> Result<GateNetlist, String> {
    import(name, text, top, libs)?.build()
}

/// Reads the netlist `text` via [`import`] and freezes it into a [`FrozenNetlist`].
pub fn import_frozen(
    name: &str,
    text: &str,
    top: Option<&str>,
    libs: &[Library],
) -> Result<FrozenNetlist<'static>, String> {
    import(name, text, top, libs)?.freeze()
}

/// Reads a single-module structural Verilog netlist directly via [`read_structural`] (`RS-VLOG`).
pub fn read_structural_netlist(
    name: &str,
    text: &str,
    libs: &[Library],
) -> Result<GateNetlist, String> {
    read_structural(text, name, &|cell_name| {
        let cell = find_cell(libs, cell_name)?;
        Some(
            cell.pins
                .iter()
                .map(|p| {
                    let dir = match p.direction {
                        Direction::Input => rusteda_core::netlist::PinDir::Input,
                        Direction::Output => rusteda_core::netlist::PinDir::Output,
                        Direction::Inout => rusteda_core::netlist::PinDir::Inout,
                        Direction::Internal => rusteda_core::netlist::PinDir::Other,
                    };
                    (p.name.clone(), dir)
                })
                .collect(),
        )
    })
    .map_err(|e| e.to_string())
}

/// Reads a single-module structural Verilog netlist via [`read_structural`] (`RS-VLOG`) and
/// freezes it into a [`FrozenNetlist`].
pub fn read_structural_frozen(
    name: &str,
    text: &str,
    libs: &[Library],
) -> Result<FrozenNetlist<'static>, String> {
    Ok(read_structural_netlist(name, text, libs)?.freeze())
}

/// Reads the netlist `text` (file name `name`) with the Verilog reader and flattens it with the
/// legacy flattener; the cells of `libs` are leaf cells.
pub fn read_flat(
    name: &str,
    text: &str,
    top: Option<&str>,
    libs: &[Library],
) -> Result<Flat, String> {
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, &[(name, text)], PpOptions::default());
    if let Some(d) = diags.iter().find(|d| d.is_error()) {
        return Err(d.render(&sm));
    }
    flatten(&units, top, &|cell: &str| find_cell(libs, cell).is_some())
}

/// The cell named `name`: its first definition in `libs`, as in the legacy engine.
fn find_cell<'a>(libs: &'a [Library], name: &str) -> Option<&'a Cell> {
    libs.iter().flat_map(|l| &l.cells).find(|c| c.name == name)
}

/// The building steps of a flat netlist (see the module documentation).
pub fn steps(flat: &Flat, libs: &[Library]) -> Result<LegacyImport, String> {
    check_verbatim("module", &flat.top)?;
    let ports = ports(flat)?;
    // The flat net of every port bit, in the steps' bit order.
    let mut legacy_bit = 0;
    let mut bit_nets: Vec<Vec<NetId>> = Vec::with_capacity(ports.len());
    for p in &ports {
        let n = p.bits().len();
        let lsb_first = &flat.ports[legacy_bit..legacy_bit + n];
        bit_nets.push(lsb_first.iter().rev().map(|b| b.net).collect());
        legacy_bit += n;
    }
    // Each non-constant flat net: the net of its first input port bit, else of its first port
    // bit, else a net of its own.
    let mut of_flat: HashMap<NetId, NetRef> = HashMap::new();
    for want in [PortDir::Input, PortDir::Output] {
        for port in (0..ports.len()).filter(|&k| ports[k].dir == want) {
            for (bit, &f) in bit_nets[port].iter().enumerate() {
                if f != CONST0 && f != CONST1 {
                    of_flat.entry(f).or_insert(NetRef::PortBit { port, bit });
                }
            }
        }
    }
    let mut nets = Vec::new();
    let mut names: HashSet<&str> = ports.iter().map(|p| p.name.as_str()).collect();
    for f in (CONST1 + 1)..flat.num_nets as NetId {
        if of_flat.contains_key(&f) {
            continue;
        }
        let name = &flat.net_names[f as usize];
        check_hierarchical("net", name)?;
        if !names.insert(name.as_str()) {
            return Err(format!(
                "net name '{name}' is taken by a port or another net"
            ));
        }
        of_flat.insert(f, NetRef::Net(nets.len()));
        nets.push(name.clone());
    }
    let conn = |f: NetId| match f {
        CONST0 => Conn::Const(false),
        CONST1 => Conn::Const(true),
        f => Conn::Net(of_flat[&f]),
    };
    let mut assigns = Vec::new();
    for (port, flat_bits) in bit_nets.iter().enumerate() {
        for (bit, &f) in flat_bits.iter().enumerate() {
            let lhs = NetRef::PortBit { port, bit };
            if conn(f) != Conn::Net(lhs) {
                assigns.push(Assign { lhs, rhs: conn(f) });
            }
        }
    }
    let mut cells: Vec<CellType> = Vec::new();
    let mut instances = Vec::with_capacity(flat.insts.len());
    let mut inst_names = HashSet::new();
    for inst in &flat.insts {
        check_hierarchical("instance", &inst.name)?;
        if !inst_names.insert(inst.name.as_str()) {
            return Err(format!("two instances are named '{}'", inst.name));
        }
        let cell = match cells.iter().position(|c| c.name == inst.cell) {
            Some(k) => k,
            None => {
                cells.push(cell_type(libs, &inst.cell)?);
                cells.len() - 1
            }
        };
        let mut conns = Vec::with_capacity(inst.conns.len());
        for (pin, f) in &inst.conns {
            if !cells[cell].pins.iter().any(|(p, _)| p == pin) {
                return Err(format!(
                    "instance {}: cell {} has no pin {pin}",
                    inst.name, inst.cell
                ));
            }
            if conns.iter().any(|(p, _)| p == pin) {
                return Err(format!("instance {}: pin {pin} connected twice", inst.name));
            }
            conns.push((pin.clone(), conn(*f)));
        }
        instances.push(Instance {
            name: inst.name.clone(),
            cell,
            conns,
        });
    }
    Ok(LegacyImport {
        module: flat.top.clone(),
        cells,
        ports,
        nets,
        instances,
        assigns,
    })
}

/// The ports of the flat netlist, whose port bits run port by port, each from the least
/// significant declared bit to the most significant one.
fn ports(flat: &Flat) -> Result<Vec<Port>, String> {
    let mut out: Vec<Port> = Vec::new();
    for b in &flat.ports {
        let dir = if b.input {
            PortDir::Input
        } else {
            PortDir::Output
        };
        let (name, index) = split_bit(&b.name)?;
        if let Some(p) = out.last_mut()
            && let (Some((msb, lsb)), Some(i)) = (p.range, index)
            && continues_bus(p, name, dir, (msb, lsb), i)
        {
            p.range = Some((i, lsb));
            continue;
        }
        check_hierarchical("port", name)?;
        if out.iter().any(|p| p.name == name) {
            return Err(format!("two ports are named '{name}'"));
        }
        out.push(Port {
            name: name.to_string(),
            dir,
            range: index.map(|i| (i, i)),
        });
    }
    Ok(out)
}

/// True when bit `index` of the port `name` with direction `dir` is the next bit of the bus `p`,
/// read so far from `lsb` to `msb`.
fn continues_bus(p: &Port, name: &str, dir: PortDir, (msb, lsb): (i32, i32), index: i32) -> bool {
    let step = i64::from(index) - i64::from(msb);
    p.name == name
        && p.dir == dir
        && step.abs() == 1
        && (msb == lsb || step == (i64::from(msb) - i64::from(lsb)).signum())
}

/// `name[index]` as `(name, Some(index))`; any other name as `(name, None)`.
fn split_bit(name: &str) -> Result<(&str, Option<i32>), String> {
    let Some(body) = name.strip_suffix(']') else {
        return Ok((name, None));
    };
    let Some((base, index)) = body.rsplit_once('[') else {
        return Ok((name, None));
    };
    match index.parse::<i32>() {
        Ok(i) if !base.is_empty() => Ok((base, Some(i))),
        _ => Err(format!("port bit '{name}': no bit index")),
    }
}

/// The cell type of the cell named `name` in `libs`.
fn cell_type(libs: &[Library], name: &str) -> Result<CellType, String> {
    let cell = find_cell(libs, name).ok_or_else(|| format!("unknown cell {name}"))?;
    check_verbatim("cell", name)?;
    let pins = cell
        .pins
        .iter()
        .map(|p| {
            check_verbatim("cell pin", &p.name)?;
            let dir = match p.direction {
                Direction::Input => PinDir::Input,
                Direction::Output => PinDir::Output,
                Direction::Inout => PinDir::Inout,
                Direction::Internal => PinDir::Other,
            };
            Ok((p.name.clone(), dir))
        })
        .collect::<Result<_, String>>()?;
    Ok(CellType {
        name: name.to_string(),
        pins,
    })
}

/// A module, cell or cell pin name: printable ASCII without blanks.
fn check_verbatim(kind: &str, name: &str) -> Result<(), String> {
    if !name.is_empty() && name.bytes().all(|b| (33..=126).contains(&b)) {
        Ok(())
    } else {
        Err(format!("{kind} name '{name}' is not printable ASCII"))
    }
}

/// A port, net or instance name: printable ASCII, segments joined by `/`, no empty segment and
/// no backslash (the flat netlist's names carry no escapes).
fn check_hierarchical(kind: &str, name: &str) -> Result<(), String> {
    check_verbatim(kind, name)?;
    if name.contains('\\') || name.split('/').any(str::is_empty) {
        return Err(format!(
            "{kind} name '{name}' has a backslash or an empty segment"
        ));
    }
    Ok(())
}
