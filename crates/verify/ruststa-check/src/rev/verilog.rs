// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent structural Verilog reader, hierarchical flattener, and `GateNetlist` converter
//! (`STA-REQ-0001` to `0003`, `0500`, `0501`).

use std::collections::{BTreeMap, HashMap, HashSet};

use rusteda_core::liberty::Direction;
use rusteda_core::netlist::GateNetlist;
use rusteda_formats::verilog::ast::{
    AnsiPort, ContAssign, DataDecl, Dim, Direction as AstDir, Expr, ExprKind, Instantiation, Item,
    Module, ModuleItem, NonAnsiPort, PortConn, PortDecl, PortList, SliceKind, UnOp,
};
use rusteda_formats::verilog::netlist::from_gate_netlist;
use rusteda_formats::verilog::number::{Bit4, Literal};
use rusteda_formats::verilog::write::write_netlist;
use rusteda_formats::verilog::{PpOptions, SourceMap, read_texts};

use super::lib_model::CornerLib;

/// Raw net identifier during hierarchy flattening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RawNetId(u32);

impl RawNetId {
    const ZERO: Self = Self(u32::MAX - 1);
    const ONE: Self = Self(u32::MAX);

    const fn is_const(self) -> bool {
        self.0 >= Self::ZERO.0
    }

    const fn const_bool(self) -> Option<bool> {
        match self {
            Self::ZERO => Some(false),
            Self::ONE => Some(true),
            _ => None,
        }
    }
}

/// Kind of timing-graph node in the flattened design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    InputPort { port_idx: usize },
    OutputPort { port_idx: usize },
    InoutPort { port_idx: usize },
    InstancePin { inst_idx: usize, pin: String },
}

/// A timing-graph node (top-level port or leaf instance pin).
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: NodeKind,
    pub net_idx: Option<usize>,
    pub const_tie: Option<bool>,
}

/// Flattened top-level port bit.
#[derive(Debug, Clone)]
pub struct PortDef {
    pub name: String,
    pub bus_name: Option<String>,
    pub dir: AstDir,
    pub node_idx: usize,
}

/// Flattened leaf cell instance.
#[derive(Debug, Clone)]
pub struct CellInst {
    pub name: String,
    pub cell_type: String,
    pub pin_nodes: BTreeMap<String, usize>,
}

/// Flattened canonical signal net.
#[derive(Debug, Clone)]
pub struct NetDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub connected_pins: Vec<usize>,
    pub connected_ports: Vec<String>,
    pub drivers: Vec<usize>,
    pub sinks: Vec<usize>,
    pub is_port_connected: bool,
}

/// Flattened structural design ready for timing analysis (`STA-REQ-0001` to `0003`).
#[derive(Debug, Clone)]
pub struct FlatDesign {
    pub top: String,
    pub nodes: Vec<Node>,
    pub node_map: BTreeMap<String, usize>,
    pub ports: Vec<PortDef>,
    pub input_ports: Vec<usize>,
    pub output_ports: Vec<usize>,
    pub instances: Vec<CellInst>,
    pub inst_map: BTreeMap<String, usize>,
    pub nets: Vec<NetDef>,
    pub net_map: BTreeMap<String, usize>,
}

/// Renders a workspace `GateNetlist` back into canonical structural Verilog text so the checker
/// exercises its own independent Verilog reader and flattener (`STA-REQ-0500`).
#[must_use]
pub fn gate_netlist_to_verilog(gn: &GateNetlist) -> String {
    let nl = from_gate_netlist(gn).expect("valid GateNetlist");
    write_netlist(&nl)
}

/// Parses and hierarchically flattens Verilog sources against the compiled corner libraries
/// (`STA-REQ-0001` to `0003`).
pub fn read_verilog_texts(
    texts: &[(&str, &str)],
    top: Option<&str>,
    corners: &[CornerLib],
) -> Result<FlatDesign, String> {
    let mut sm = SourceMap::new();
    let (units, diags) = read_texts(&mut sm, texts, PpOptions::default());
    if let Some(err) = diags.iter().find(|d| d.is_error()) {
        return Err(format!("C0502: {}", err.render(&sm)));
    }

    let mut modules: HashMap<String, &Module> = HashMap::new();
    let mut module_order: Vec<String> = Vec::new();
    for u in &units {
        for item in &u.items {
            match item {
                Item::Module(m) => {
                    if modules.contains_key(&m.name.name) {
                        return Err(format!("C0502: duplicate module '{}'", m.name.name));
                    }
                    module_order.push(m.name.name.clone());
                    modules.insert(m.name.name.clone(), m);
                }
                Item::Unsupported { what, .. } => {
                    return Err(format!("C0502: unsupported top-level construct '{what}'"));
                }
                _ => {}
            }
        }
    }

    if modules.is_empty() {
        return Err("C0502: no Verilog modules found".to_string());
    }

    let is_cell = |name: &str| corners.iter().any(|c| c.cells.contains_key(name));

    let top_name = if let Some(t) = top {
        if !modules.contains_key(t) {
            return Err(format!("C0502: top module '{t}' not found"));
        }
        t.to_string()
    } else {
        let mut instantiated = HashSet::new();
        for m in modules.values() {
            for item in &m.items {
                if let ModuleItem::Instance(inst_stmt) = item
                    && modules.contains_key(&inst_stmt.module.name)
                {
                    instantiated.insert(inst_stmt.module.name.clone());
                }
            }
        }
        module_order
            .iter()
            .find(|n| !instantiated.contains(*n))
            .or_else(|| module_order.last())
            .cloned()
            .unwrap()
    };

    let mut flat = Flattener {
        modules: &modules,
        is_cell: &is_cell,
        corners,
        raw_net_names: Vec::new(),
        uf_parent: Vec::new(),
        raw_ports: Vec::new(),
        raw_insts: Vec::new(),
        call_stack: Vec::new(),
    };
    let top_mod = modules[&top_name];
    let scope = flat.elab_module(top_mod, "")?;
    for (pname, bname, dir, rnet) in scope.ports {
        flat.raw_ports.push((pname, bname, dir, rnet));
    }
    flat.finish(&top_name)
}

struct ScopeInfo {
    /// Ordered port bits `(bit_name, bus_name, dir, raw_net)`.
    ports: Vec<(String, Option<String>, AstDir, RawNetId)>,
    /// Map from local identifier name to ordered `(bit_index_opt, raw_net)` from MSB to LSB.
    nets: HashMap<String, Vec<(Option<i32>, RawNetId)>>,
}

struct Flattener<'a> {
    modules: &'a HashMap<String, &'a Module>,
    is_cell: &'a dyn Fn(&str) -> bool,
    corners: &'a [CornerLib],
    raw_net_names: Vec<String>,
    uf_parent: Vec<RawNetId>,
    raw_ports: Vec<(String, Option<String>, AstDir, RawNetId)>,
    raw_insts: Vec<(String, String, Vec<(String, Option<RawNetId>)>)>,
    call_stack: Vec<String>,
}

impl Flattener<'_> {
    fn new_raw_net(&mut self, name: String) -> RawNetId {
        let id = RawNetId(self.raw_net_names.len() as u32);
        self.raw_net_names.push(name);
        self.uf_parent.push(id);
        id
    }

    fn find(&mut self, x: RawNetId) -> RawNetId {
        if x.is_const() {
            return x;
        }
        let idx = x.0 as usize;
        let p = self.uf_parent[idx];
        if p == x {
            x
        } else {
            let r = self.find(p);
            self.uf_parent[idx] = r;
            r
        }
    }

    fn union(&mut self, a: RawNetId, b: RawNetId) -> Result<(), String> {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return Ok(());
        }
        if ra.is_const() && rb.is_const() {
            return Err(
                "C0502: net is tied to both 1'b0 and 1'b1 simultaneously (STA-REQ-0002)".into(),
            );
        }
        if ra.is_const() {
            self.uf_parent[rb.0 as usize] = ra;
        } else if rb.is_const() {
            self.uf_parent[ra.0 as usize] = rb;
        } else {
            // Prefer shorter (less deeply nested) name as canonical representative.
            let na = &self.raw_net_names[ra.0 as usize];
            let nb = &self.raw_net_names[rb.0 as usize];
            if na.matches('/').count() <= nb.matches('/').count() {
                self.uf_parent[rb.0 as usize] = ra;
            } else {
                self.uf_parent[ra.0 as usize] = rb;
            }
        }
        Ok(())
    }

    fn declare_signal(
        &mut self,
        prefix: &str,
        local_name: &str,
        dims: &[Dim],
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
    ) -> Result<Vec<(Option<i32>, RawNetId)>, String> {
        if let Some(existing) = nets.get(local_name) {
            return Ok(existing.clone());
        }
        let bits = if dims.is_empty() {
            let full = format!("{prefix}{local_name}");
            vec![(None, self.new_raw_net(full))]
        } else if dims.len() == 1 {
            let (msb, lsb) = eval_dim(&dims[0])?;
            let indices: Vec<i32> = if msb >= lsb {
                (lsb..=msb).rev().collect()
            } else {
                (msb..=lsb).collect()
            };
            indices
                .into_iter()
                .map(|idx| {
                    let full = format!("{prefix}{local_name}[{idx}]");
                    (Some(idx), self.new_raw_net(full))
                })
                .collect()
        } else {
            return Err(format!(
                "C0502: unsupported multi-dimensional array '{local_name}'"
            ));
        };
        nets.insert(local_name.to_string(), bits.clone());
        Ok(bits)
    }

    fn elab_module(&mut self, m: &Module, prefix: &str) -> Result<ScopeInfo, String> {
        if self.call_stack.iter().any(|s| s == &m.name.name) {
            return Err(format!(
                "C0502: recursive module instantiation involving '{}'",
                m.name.name
            ));
        }
        self.call_stack.push(m.name.name.clone());

        let mut nets: HashMap<String, Vec<(Option<i32>, RawNetId)>> = HashMap::new();
        let mut ports: Vec<(String, Option<String>, AstDir, RawNetId)> = Vec::new();
        let mut non_ansi_order: Vec<String> = Vec::new();

        match &m.ports {
            PortList::None => {}
            PortList::Ansi(ansi_ports) => {
                for p in ansi_ports {
                    self.add_ansi_port(p, prefix, &mut nets, &mut ports)?;
                }
            }
            PortList::NonAnsi(na_ports) => {
                for np in na_ports {
                    if let Some(name) = non_ansi_port_name(np) {
                        non_ansi_order.push(name);
                    }
                }
            }
        }

        let mut body_ports: HashMap<String, Vec<(String, Option<String>, AstDir, RawNetId)>> =
            HashMap::new();

        // First pass: port and wire declarations.
        for item in &m.items {
            match item {
                ModuleItem::PortDecl(pd) => {
                    self.elab_port_decl(pd, prefix, &mut nets, &mut body_ports)?;
                }
                ModuleItem::Data(dd) => {
                    self.elab_data_decl(dd, prefix, &mut nets)?;
                }
                ModuleItem::Assign(_) | ModuleItem::Instance(_) | ModuleItem::Ignored { .. } => {}
                other => {
                    self.call_stack.pop();
                    return Err(format!(
                        "C0502: unsupported non-structural item in module '{}': {other:?}",
                        m.name.name
                    ));
                }
            }
        }

        if !non_ansi_order.is_empty() {
            for pname in &non_ansi_order {
                if let Some(pvec) = body_ports.remove(pname) {
                    ports.extend(pvec);
                }
            }
        }
        for (_, pvec) in body_ports {
            ports.extend(pvec);
        }

        // Second pass: continuous assignments and module/cell instances.
        for item in &m.items {
            match item {
                ModuleItem::Assign(assigns) => {
                    self.elab_assigns(assigns, prefix, &mut nets)?;
                }
                ModuleItem::Instance(inst_stmt) => {
                    self.elab_instantiation(inst_stmt, prefix, &mut nets)?;
                }
                _ => {}
            }
        }

        self.call_stack.pop();
        Ok(ScopeInfo { ports, nets })
    }

    fn add_ansi_port(
        &mut self,
        p: &AnsiPort,
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
        ports: &mut Vec<(String, Option<String>, AstDir, RawNetId)>,
    ) -> Result<(), String> {
        let mut dims = p.ty.packed.clone();
        dims.extend(p.unpacked.iter().cloned());
        let bits = self.declare_signal(prefix, &p.name.name, &dims, nets)?;
        let bus_name = if bits.len() > 1 || dims.len() == 1 {
            Some(format!("{prefix}{}", p.name.name))
        } else {
            None
        };
        for (idx_opt, rnet) in bits {
            let bit_name = match idx_opt {
                Some(idx) => format!("{prefix}{}[{idx}]", p.name.name),
                None => format!("{prefix}{}", p.name.name),
            };
            ports.push((bit_name, bus_name.clone(), p.dir, rnet));
        }
        Ok(())
    }

    fn elab_port_decl(
        &mut self,
        pd: &PortDecl,
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
        body_ports: &mut HashMap<String, Vec<(String, Option<String>, AstDir, RawNetId)>>,
    ) -> Result<(), String> {
        for (ident, unp_dims, _) in &pd.names {
            let mut dims = pd.ty.packed.clone();
            dims.extend(unp_dims.iter().cloned());
            let bits = self.declare_signal(prefix, &ident.name, &dims, nets)?;
            let bus_name = if bits.len() > 1 || dims.len() == 1 {
                Some(format!("{prefix}{}", ident.name))
            } else {
                None
            };
            let mut pvec = Vec::with_capacity(bits.len());
            for (idx_opt, rnet) in bits {
                let bit_name = match idx_opt {
                    Some(idx) => format!("{prefix}{}[{idx}]", ident.name),
                    None => format!("{prefix}{}", ident.name),
                };
                pvec.push((bit_name, bus_name.clone(), pd.dir, rnet));
            }
            body_ports.insert(ident.name.clone(), pvec);
        }
        Ok(())
    }

    fn elab_data_decl(
        &mut self,
        dd: &DataDecl,
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
    ) -> Result<(), String> {
        for decl in &dd.decls {
            let mut dims = dd.ty.packed.clone();
            dims.extend(decl.dims.iter().cloned());
            let lhs_bits = self.declare_signal(prefix, &decl.name.name, &dims, nets)?;
            if let Some(init_expr) = &decl.init {
                let rhs_bits = self.resolve_expr(init_expr, prefix, nets)?;
                for ((_, l), r) in lhs_bits.into_iter().zip(rhs_bits) {
                    self.union(l, r)?;
                }
            }
        }
        Ok(())
    }

    fn elab_assigns(
        &mut self,
        assigns: &[ContAssign],
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
    ) -> Result<(), String> {
        for ca in assigns {
            let lhs = self.resolve_expr(&ca.lhs, prefix, nets)?;
            let rhs = self.resolve_expr(&ca.rhs, prefix, nets)?;
            if lhs.len() != rhs.len() && rhs.len() == 1 && rhs[0].is_const() {
                for l in lhs {
                    self.union(l, rhs[0])?;
                }
            } else {
                for (l, r) in lhs.into_iter().zip(rhs) {
                    self.union(l, r)?;
                }
            }
        }
        Ok(())
    }

    fn elab_instantiation(
        &mut self,
        inst_stmt: &Instantiation,
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
    ) -> Result<(), String> {
        let mod_name = &inst_stmt.module.name;
        let is_submod = self.modules.contains_key(mod_name) && !(self.is_cell)(mod_name);

        for inst in &inst_stmt.instances {
            let full_inst = format!("{prefix}{}", inst.name.name);
            if is_submod {
                let sub_mod = self.modules[mod_name];
                let sub_prefix = format!("{full_inst}/");
                let sub_scope = self.elab_module(sub_mod, &sub_prefix)?;
                for (pos, conn) in inst.conns.iter().enumerate() {
                    match conn {
                        PortConn::Named(p, Some(Some(expr))) => {
                            let Some(formal) = sub_scope.nets.get(&p.name) else {
                                return Err(format!(
                                    "C0502: unknown port '{}' on submodule '{mod_name}'",
                                    p.name
                                ));
                            };
                            let actual = self.resolve_expr(expr, prefix, nets)?;
                            for ((_, fnet), anet) in formal.iter().zip(actual) {
                                self.union(*fnet, anet)?;
                            }
                        }
                        PortConn::Named(_, _) => {}
                        PortConn::Ordered(Some(expr)) => {
                            let actual = self.resolve_expr(expr, prefix, nets)?;
                            if let Some((_, _, _, fnet)) = sub_scope.ports.get(pos)
                                && let Some(&anet) = actual.first()
                            {
                                self.union(*fnet, anet)?;
                            }
                        }
                        PortConn::Ordered(None) => {}
                        PortConn::Wildcard(_) => {
                            return Err("C0502: wildcard .* port connection not supported".into());
                        }
                    }
                }
            } else {
                let mut conns: Vec<(String, Option<RawNetId>)> = Vec::new();
                for conn in &inst.conns {
                    match conn {
                        PortConn::Named(p, Some(Some(expr))) => {
                            let actual = self.resolve_expr(expr, prefix, nets)?;
                            if actual.is_empty() {
                                conns.push((p.name.clone(), None));
                            } else if actual.len() == 1 {
                                conns.push((p.name.clone(), Some(actual[0])));
                            } else {
                                // Expand multi-bit bus pin connection (MSB to LSB).
                                let n = actual.len();
                                for (k, rnet) in actual.into_iter().enumerate() {
                                    let bit_idx = (n - 1) - k;
                                    conns.push((format!("{}[{bit_idx}]", p.name), Some(rnet)));
                                }
                            }
                        }
                        PortConn::Named(p, _) => {
                            conns.push((p.name.clone(), None));
                        }
                        PortConn::Ordered(_) | PortConn::Wildcard(_) => {
                            return Err(format!(
                                "C0502: positional or wildcard connection on leaf cell '{full_inst}'"
                            ));
                        }
                    }
                }
                self.raw_insts.push((full_inst, mod_name.clone(), conns));
            }
        }
        Ok(())
    }

    fn resolve_expr(
        &mut self,
        expr: &Expr,
        prefix: &str,
        nets: &mut HashMap<String, Vec<(Option<i32>, RawNetId)>>,
    ) -> Result<Vec<RawNetId>, String> {
        match &expr.kind {
            ExprKind::Ident(path) => {
                let name = &path.name.name;
                if let Some(v) = nets.get(name) {
                    Ok(v.iter().map(|(_, n)| *n).collect())
                } else {
                    let v = self.declare_signal(prefix, name, &[], nets)?;
                    Ok(vec![v[0].1])
                }
            }
            ExprKind::Index(base, idx_expr) => {
                let Some(ident) = base.as_ident() else {
                    return Err("C0502: complex indexed expression in netlist".into());
                };
                let idx = eval_i32(idx_expr)?;
                if let Some(v) = nets.get(&ident.name) {
                    if let Some((_, rnet)) = v.iter().find(|(i, _)| *i == Some(idx)) {
                        return Ok(vec![*rnet]);
                    }
                    return Err(format!(
                        "C0502: bit index {idx} out of bounds on '{}'",
                        ident.name
                    ));
                }
                let bit_name = format!("{prefix}{}[{idx}]", ident.name);
                let rnet = self.new_raw_net(bit_name);
                nets.entry(ident.name.clone())
                    .or_default()
                    .push((Some(idx), rnet));
                Ok(vec![rnet])
            }
            ExprKind::Slice {
                base,
                kind: SliceKind::Const,
                left,
                right,
            } => {
                let Some(ident) = base.as_ident() else {
                    return Err("C0502: complex sliced expression in netlist".into());
                };
                let l = eval_i32(left)?;
                let r = eval_i32(right)?;
                let Some(v) = nets.get(&ident.name) else {
                    return Err(format!("C0502: slice on undeclared net '{}'", ident.name));
                };
                let indices: Vec<i32> = if l >= r {
                    (r..=l).rev().collect()
                } else {
                    (l..=r).collect()
                };
                let mut out = Vec::with_capacity(indices.len());
                for idx in indices {
                    let Some((_, rnet)) = v.iter().find(|(i, _)| *i == Some(idx)) else {
                        return Err(format!(
                            "C0502: slice index {idx} out of bounds on '{}'",
                            ident.name
                        ));
                    };
                    out.push(*rnet);
                }
                Ok(out)
            }
            ExprKind::Concat(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    out.extend(self.resolve_expr(p, prefix, nets)?);
                }
                Ok(out)
            }
            ExprKind::Repl(count_expr, parts) => {
                let count = eval_i32(count_expr)?.max(0) as usize;
                let mut inner = Vec::new();
                for p in parts {
                    inner.extend(self.resolve_expr(p, prefix, nets)?);
                }
                let mut out = Vec::with_capacity(count * inner.len());
                for _ in 0..count {
                    out.extend(inner.iter().copied());
                }
                Ok(out)
            }
            ExprKind::Number(lit) => literal_to_raw_nets(lit),
            ExprKind::UnbasedUnsized(c) => match c.to_ascii_lowercase() {
                '0' => Ok(vec![RawNetId::ZERO]),
                '1' => Ok(vec![RawNetId::ONE]),
                'z' => Ok(Vec::new()),
                other => Err(format!("C0502: unsupported literal '{other}'")),
            },
            _ => Err("C0502: unsupported expression in structural netlist".into()),
        }
    }

    fn finish(mut self, top: &str) -> Result<FlatDesign, String> {
        // 1. Canonicalize raw nets via union-find.
        let mut port_name_for_root: HashMap<RawNetId, String> = HashMap::new();
        let port_nets: Vec<(String, RawNetId)> = self
            .raw_ports
            .iter()
            .map(|(pname, _, _, rnet)| (pname.clone(), *rnet))
            .collect();
        for (pname, rnet) in port_nets {
            let root = self.find(rnet);
            if !root.is_const() {
                port_name_for_root.entry(root).or_insert(pname);
            }
        }

        let mut root_to_net: HashMap<RawNetId, usize> = HashMap::new();
        let mut nets: Vec<NetDef> = Vec::new();
        let mut net_map: BTreeMap<String, usize> = BTreeMap::new();

        for i in 0..self.raw_net_names.len() {
            let raw = RawNetId(i as u32);
            let root = self.find(raw);
            let alias = self.raw_net_names[i].clone();
            if root.is_const() {
                continue;
            }
            let net_idx = *root_to_net.entry(root).or_insert_with(|| {
                let idx = nets.len();
                let cname = port_name_for_root
                    .get(&root)
                    .cloned()
                    .unwrap_or_else(|| self.raw_net_names[root.0 as usize].clone());
                nets.push(NetDef {
                    name: cname.clone(),
                    aliases: Vec::new(),
                    connected_pins: Vec::new(),
                    connected_ports: Vec::new(),
                    drivers: Vec::new(),
                    sinks: Vec::new(),
                    is_port_connected: false,
                });
                net_map.insert(cname, idx);
                idx
            });
            nets[net_idx].aliases.push(alias.clone());
            net_map.insert(alias, net_idx);
        }

        let mut nodes: Vec<Node> = Vec::new();
        let mut node_map: BTreeMap<String, usize> = BTreeMap::new();
        let mut ports: Vec<PortDef> = Vec::new();
        let mut input_ports: Vec<usize> = Vec::new();
        let mut output_ports: Vec<usize> = Vec::new();

        // 2. Create port nodes.
        let raw_ports = std::mem::take(&mut self.raw_ports);
        for (pname, bname, dir, rnet) in raw_ports {
            let root = self.find(rnet);
            let const_tie = root.const_bool();
            let net_idx = if root.is_const() {
                None
            } else {
                Some(root_to_net[&root])
            };
            let port_idx = ports.len();
            let node_idx = nodes.len();
            let kind = match dir {
                AstDir::Input => {
                    input_ports.push(node_idx);
                    NodeKind::InputPort { port_idx }
                }
                AstDir::Output => {
                    output_ports.push(node_idx);
                    NodeKind::OutputPort { port_idx }
                }
                AstDir::Inout => {
                    input_ports.push(node_idx);
                    output_ports.push(node_idx);
                    NodeKind::InoutPort { port_idx }
                }
                AstDir::Ref => return Err(format!("C0502: unsupported ref port '{pname}'")),
            };
            nodes.push(Node {
                name: pname.clone(),
                kind,
                net_idx,
                const_tie,
            });
            node_map.insert(pname.clone(), node_idx);
            ports.push(PortDef {
                name: pname.clone(),
                bus_name: bname,
                dir,
                node_idx,
            });

            if let Some(ni) = net_idx {
                let net = &mut nets[ni];
                net.is_port_connected = true;
                net.connected_ports.push(pname);
                match dir {
                    AstDir::Input => net.drivers.push(node_idx),
                    AstDir::Output => net.sinks.push(node_idx),
                    AstDir::Inout => {
                        net.drivers.push(node_idx);
                        net.sinks.push(node_idx);
                    }
                    AstDir::Ref => {}
                }
            } else if matches!(dir, AstDir::Input | AstDir::Inout) && const_tie.is_some() {
                return Err(format!(
                    "C0502: input port '{pname}' is tied to a constant (STA-REQ-0002)"
                ));
            }
        }

        // 3. Create leaf instances and bind against corner libraries (`STA-REQ-0002`, `0003`).
        let mut instances: Vec<CellInst> = Vec::new();
        let mut inst_map: BTreeMap<String, usize> = BTreeMap::new();
        let raw_insts = std::mem::take(&mut self.raw_insts);

        for (iname, cell_type, conns) in raw_insts {
            for clib in self.corners {
                let Some(cdef) = clib.cells.get(&cell_type) else {
                    return Err(format!(
                        "C0502: instance '{iname}' references unknown cell '{cell_type}' in corner '{}' (STA-REQ-0003)",
                        clib.corner
                    ));
                };
                for (cpin, _) in &conns {
                    if !cdef.pins.contains_key(cpin) {
                        return Err(format!(
                            "C0502: instance '{iname}' connects unknown pin '{cpin}' of cell '{cell_type}' (STA-REQ-0003)"
                        ));
                    }
                }
            }

            let cdef = &self.corners[0].cells[&cell_type];
            let inst_idx = instances.len();
            let mut conn_map: BTreeMap<String, Option<RawNetId>> = BTreeMap::new();
            for pdef in cdef.pins.values() {
                if pdef.direction != Direction::Internal {
                    conn_map.insert(pdef.name.clone(), None);
                }
            }
            for (cpin, rnet_opt) in conns {
                conn_map.insert(cpin, rnet_opt);
            }

            let mut pin_nodes = BTreeMap::new();
            for (cpin, rnet_opt) in conn_map {
                let pdef = &cdef.pins[&cpin];
                let (net_idx, const_tie) = match rnet_opt {
                    None => (None, None),
                    Some(raw) => {
                        let root = self.find(raw);
                        if let Some(b) = root.const_bool() {
                            if pdef.direction == Direction::Output {
                                return Err(format!(
                                    "C0502: output pin '{iname}/{cpin}' is tied to a constant (STA-REQ-0002)"
                                ));
                            }
                            (None, Some(b))
                        } else {
                            (Some(root_to_net[&root]), None)
                        }
                    }
                };
                let full_pin = format!("{iname}/{cpin}");
                let node_idx = nodes.len();
                nodes.push(Node {
                    name: full_pin.clone(),
                    kind: NodeKind::InstancePin {
                        inst_idx,
                        pin: cpin.clone(),
                    },
                    net_idx,
                    const_tie,
                });
                node_map.insert(full_pin, node_idx);
                pin_nodes.insert(cpin, node_idx);

                if let Some(ni) = net_idx {
                    let net = &mut nets[ni];
                    net.connected_pins.push(node_idx);
                    match pdef.direction {
                        Direction::Input => net.sinks.push(node_idx),
                        Direction::Output => net.drivers.push(node_idx),
                        Direction::Inout => {
                            net.drivers.push(node_idx);
                            net.sinks.push(node_idx);
                        }
                        Direction::Internal => {}
                    }
                }
            }

            inst_map.insert(iname.clone(), inst_idx);
            instances.push(CellInst {
                name: iname,
                cell_type,
                pin_nodes,
            });
        }

        // 4. Validate STA-REQ-0002: multiple drivers on a net are allowed only if all drivers are
        // three-state cell output pins.
        let cdef_map = &self.corners[0].cells;
        for net in &nets {
            if net.drivers.len() > 1 {
                let all_tristate = net.drivers.iter().all(|&drv_node| {
                    if let NodeKind::InstancePin { inst_idx, ref pin } = nodes[drv_node].kind {
                        let inst = &instances[inst_idx];
                        cdef_map
                            .get(&inst.cell_type)
                            .and_then(|c| c.pins.get(pin))
                            .is_some_and(super::lib_model::PinDef::is_three_state)
                    } else {
                        false
                    }
                });
                if !all_tristate {
                    return Err(format!(
                        "C0502: net '{}' has multiple non-three-state drivers (STA-REQ-0002)",
                        net.name
                    ));
                }
            }
        }

        Ok(FlatDesign {
            top: top.to_string(),
            nodes,
            node_map,
            ports,
            input_ports,
            output_ports,
            instances,
            inst_map,
            nets,
            net_map,
        })
    }
}

fn non_ansi_port_name(np: &NonAnsiPort) -> Option<String> {
    if let Some(ext) = &np.external {
        return Some(ext.name.clone());
    }
    np.expr
        .as_ref()
        .and_then(|e| e.as_ident().map(|id| id.name.clone()))
}

fn eval_dim(d: &Dim) -> Result<(i32, i32), String> {
    match d {
        Dim::Range(l, r) => Ok((eval_i32(l)?, eval_i32(r)?)),
        Dim::Size(s) => {
            let n = eval_i32(s)?;
            Ok((n - 1, 0))
        }
        Dim::Unsupported(_) => Err("C0502: unsupported array dimension".into()),
    }
}

fn eval_i32(e: &Expr) -> Result<i32, String> {
    match &e.kind {
        ExprKind::Number(lit) => {
            if lit.base == 'd' && lit.digits.bytes().all(|b| b.is_ascii_digit()) {
                lit.digits
                    .parse::<i32>()
                    .map_err(|_| format!("C0502: integer overflow in '{}'", lit.digits))
            } else {
                let bits = lit.bits().map_err(|e| format!("C0502: {e}"))?;
                let mut val = 0i64;
                for (k, b) in bits.iter().enumerate().take(31) {
                    if *b == Bit4::One {
                        val |= 1i64 << k;
                    }
                }
                Ok(val as i32)
            }
        }
        ExprKind::Unary(UnOp::Minus, inner) => Ok(-eval_i32(inner)?),
        ExprKind::Unary(UnOp::Plus, inner) => eval_i32(inner),
        _ => Err("C0502: non-constant integer expression".into()),
    }
}

fn literal_to_raw_nets(lit: &Literal) -> Result<Vec<RawNetId>, String> {
    let bits_lsb = lit.bits().map_err(|e| format!("C0502: {e}"))?;
    let width = lit.size.unwrap_or(1) as usize;
    let slice = &bits_lsb[..width.min(bits_lsb.len())];
    if slice.iter().all(|b| *b == Bit4::Z) {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(slice.len());
    for b in slice.iter().rev() {
        match b {
            Bit4::Zero => out.push(RawNetId::ZERO),
            Bit4::One => out.push(RawNetId::ONE),
            Bit4::X | Bit4::Z => {
                return Err("C0502: unsupported X/Z bit in constant tie".into());
            }
        }
    }
    Ok(out)
}
