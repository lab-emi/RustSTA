// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Flattening of structural Verilog netlists (STA-REQ-0001 to STA-REQ-0003).

use rustc_hash::FxHashMap;
use rusteda_formats::verilog::ast::{
    Dim, Direction, Expr, ExprKind, Item, Module, ModuleItem, PortConn, PortList, SourceUnit,
};
use rusteda_formats::verilog::number::Bit4;

/// Index of a net bit after joining.
pub type NetId = u32;

/// Constant nets.
pub const CONST0: NetId = 0;
pub const CONST1: NetId = 1;

/// A top-level port bit.
#[derive(Debug, Clone)]
pub struct PortBit {
    pub name: String,
    pub input: bool,
    pub net: NetId,
}

/// A library cell instance.
#[derive(Debug, Clone)]
pub struct Inst {
    pub name: String,
    pub cell: String,
    /// (pin name, net).
    pub conns: Vec<(String, NetId)>,
}

/// A flat netlist.
#[derive(Debug, Clone)]
pub struct Flat {
    pub top: String,
    pub ports: Vec<PortBit>,
    pub insts: Vec<Inst>,
    pub num_nets: usize,
    /// One name per net (the first name seen).
    pub net_names: Vec<String>,
}

struct Builder<'a> {
    modules: FxHashMap<&'a str, &'a Module>,
    is_cell: &'a dyn Fn(&str) -> bool,
    parent: Vec<NetId>,
    names: Vec<String>,
    insts: Vec<Inst>,
    depth: usize,
}

/// A declared name: range and bits, index 0 = least significant.
#[derive(Debug, Clone)]
struct Decl {
    msb: i64,
    lsb: i64,
    bits: Vec<NetId>,
}

impl Decl {
    fn bit(&self, i: i64) -> Option<NetId> {
        let off = if self.msb >= self.lsb {
            i - self.lsb
        } else {
            self.lsb - i
        };
        usize::try_from(off)
            .ok()
            .and_then(|o| self.bits.get(o).copied())
    }
}

fn const_int(e: &Expr) -> Result<i64, String> {
    match &e.kind {
        ExprKind::Number(l) => {
            let bits = l.bits()?;
            let mut v = 0i64;
            for (i, b) in bits.iter().enumerate().take(62) {
                if *b == Bit4::One {
                    v |= 1 << i;
                }
            }
            Ok(v)
        }
        ExprKind::Unary(rusteda_formats::verilog::ast::UnOp::Minus, a) => Ok(-const_int(a)?),
        _ => Err("netlist indices must be integer constants".into()),
    }
}

fn range_of(dims: &[Dim]) -> Result<Option<(i64, i64)>, String> {
    match dims {
        [] => Ok(None),
        [Dim::Range(a, b)] => Ok(Some((const_int(a)?, const_int(b)?))),
        [Dim::Size(n)] => Ok(Some((const_int(n)? - 1, 0))),
        _ => Err("multi-dimensional nets are not supported in netlists".into()),
    }
}

impl Builder<'_> {
    fn new_net(&mut self, name: String) -> NetId {
        let id = self.parent.len() as NetId;
        self.parent.push(id);
        self.names.push(name);
        id
    }

    fn find(&mut self, mut x: NetId) -> NetId {
        while self.parent[x as usize] != x {
            let p = self.parent[x as usize];
            self.parent[x as usize] = self.parent[p as usize];
            x = p;
        }
        x
    }

    fn union(&mut self, a: NetId, b: NetId) -> Result<(), String> {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return Ok(());
        }
        if ra <= CONST1 && rb <= CONST1 {
            return Err("a net is tied to both constants".into());
        }
        // Constants stay representatives.
        let (keep, drop) = if rb <= CONST1 { (rb, ra) } else { (ra, rb) };
        self.parent[drop as usize] = keep;
        Ok(())
    }

    fn declare(
        &mut self,
        scope: &mut FxHashMap<String, Decl>,
        path: &str,
        name: &str,
        range: Option<(i64, i64)>,
    ) {
        if scope.contains_key(name) {
            return;
        }
        let d = match range {
            None => Decl {
                msb: 0,
                lsb: 0,
                bits: vec![self.new_net(format!("{path}{name}"))],
            },
            Some((msb, lsb)) => {
                let n = (msb - lsb).unsigned_abs() + 1;
                let bits = (0..n as i64)
                    .map(|k| {
                        let idx = if msb >= lsb { lsb + k } else { lsb - k };
                        self.new_net(format!("{path}{name}[{idx}]"))
                    })
                    .collect();
                Decl { msb, lsb, bits }
            }
        };
        scope.insert(name.to_string(), d);
    }

    /// Bits of an expression, least significant first.
    fn bits(
        &mut self,
        scope: &mut FxHashMap<String, Decl>,
        path: &str,
        e: &Expr,
    ) -> Result<Vec<NetId>, String> {
        Ok(match &e.kind {
            ExprKind::Ident(p) => {
                let n = &p.name.name;
                if !scope.contains_key(n) {
                    self.declare(scope, path, n, None);
                }
                scope[n].bits.clone()
            }
            ExprKind::Index(b, i) => {
                let ExprKind::Ident(p) = &b.kind else {
                    return Err("bit-select of a non-name".into());
                };
                let d = scope
                    .get(&p.name.name)
                    .ok_or_else(|| format!("'{}' is not declared", p.name.name))?;
                let k = const_int(i)?;
                vec![
                    d.bit(k)
                        .ok_or_else(|| format!("index {k} out of range of '{}'", p.name.name))?,
                ]
            }
            ExprKind::Slice {
                base, left, right, ..
            } => {
                let ExprKind::Ident(p) = &base.kind else {
                    return Err("part-select of a non-name".into());
                };
                let d = scope
                    .get(&p.name.name)
                    .ok_or_else(|| format!("'{}' is not declared", p.name.name))?
                    .clone();
                let (l, r) = (const_int(left)?, const_int(right)?);
                let (hi, lo) = if (l >= r) == (d.msb >= d.lsb) {
                    (l, r)
                } else {
                    (r, l)
                };
                let step: i64 = if hi >= lo { 1 } else { -1 };
                let mut out = Vec::new();
                let mut k = lo;
                loop {
                    out.push(
                        d.bit(k).ok_or_else(|| {
                            format!("index {k} out of range of '{}'", p.name.name)
                        })?,
                    );
                    if k == hi {
                        break;
                    }
                    k += step;
                }
                out
            }
            ExprKind::Concat(items) => {
                let mut out = Vec::new();
                for it in items.iter().rev() {
                    out.extend(self.bits(scope, path, it)?);
                }
                out
            }
            ExprKind::Repl(n, items) => {
                let k = const_int(n)?.max(0) as usize;
                let mut one = Vec::new();
                for it in items.iter().rev() {
                    one.extend(self.bits(scope, path, it)?);
                }
                let mut out = Vec::new();
                for _ in 0..k {
                    out.extend_from_slice(&one);
                }
                out
            }
            ExprKind::Number(l) => l
                .bits()?
                .iter()
                .map(|b| if *b == Bit4::One { CONST1 } else { CONST0 })
                .collect(),
            ExprKind::UnbasedUnsized(c) => vec![if *c == '1' { CONST1 } else { CONST0 }],
            _ => return Err("unsupported expression in a netlist".into()),
        })
    }

    fn connect(&mut self, a: &[NetId], b: &[NetId]) -> Result<(), String> {
        for (k, x) in a.iter().enumerate() {
            let y = b.get(k).copied().unwrap_or(CONST0);
            self.union(*x, y)?;
        }
        Ok(())
    }

    /// Elaborates `m` with the given port bindings (port name -> bits of the parent).
    fn module(
        &mut self,
        m: &Module,
        path: &str,
        bind: Option<&FxHashMap<String, Vec<NetId>>>,
        ports_out: &mut Vec<(String, Direction, Decl)>,
    ) -> Result<(), String> {
        if self.depth > 64 {
            return Err("module hierarchy too deep".into());
        }
        let mut scope: FxHashMap<String, Decl> = FxHashMap::default();
        let mut dirs: Vec<(String, Direction)> = Vec::new();
        match &m.ports {
            PortList::Ansi(ps) => {
                for p in ps {
                    let r = range_of(&p.ty.packed)?;
                    self.declare(&mut scope, path, &p.name.name, r);
                    dirs.push((p.name.name.clone(), p.dir));
                }
            }
            PortList::NonAnsi(_) | PortList::None => {}
        }
        for it in &m.items {
            match it {
                ModuleItem::PortDecl(pd) => {
                    for (n, dims, _) in &pd.names {
                        let r = range_of(&pd.ty.packed)?.or(range_of(dims)?);
                        self.declare(&mut scope, path, &n.name, r);
                        dirs.push((n.name.clone(), pd.dir));
                    }
                }
                ModuleItem::Data(d) => {
                    for dc in &d.decls {
                        let r = range_of(&d.ty.packed)?;
                        self.declare(&mut scope, path, &dc.name.name, r);
                    }
                }
                _ => {}
            }
        }
        if let PortList::NonAnsi(list) = &m.ports {
            // Port order is the header order; directions come from the declarations.
            let mut ordered = Vec::new();
            for p in list {
                let Some(e) = &p.expr else { continue };
                let Some(id) = e.as_ident() else {
                    return Err("port expressions other than names are not supported".into());
                };
                let dir = dirs
                    .iter()
                    .find(|(n, _)| *n == id.name)
                    .map(|x| x.1)
                    .ok_or_else(|| format!("port '{}' has no direction", id.name))?;
                ordered.push((id.name.clone(), dir));
            }
            dirs = ordered;
        }
        for (n, dir) in &dirs {
            let d = scope[n].clone();
            if let Some(b) = bind {
                if let Some(bits) = b.get(n) {
                    self.connect(&d.bits, bits)?;
                }
            }
            ports_out.push((n.clone(), *dir, d));
        }
        for it in &m.items {
            match it {
                ModuleItem::Assign(v) => {
                    for a in v {
                        let l = self.bits(&mut scope, path, &a.lhs)?;
                        let r = self.bits(&mut scope, path, &a.rhs)?;
                        self.connect(&l, &r)?;
                    }
                }
                ModuleItem::Instance(ins) => {
                    for i in &ins.instances {
                        if !i.dims.is_empty() {
                            return Err("arrays of instances are not supported".into());
                        }
                        let name = format!("{path}{}", i.name.name);
                        let cell = ins.module.name.as_str();
                        if (self.is_cell)(cell) {
                            let mut conns = Vec::new();
                            for c in &i.conns {
                                let PortConn::Named(pin, Some(Some(e))) = c else {
                                    if matches!(c, PortConn::Named(_, _)) {
                                        continue;
                                    }
                                    return Err(format!(
                                        "instance {name}: library cells need named connections"
                                    ));
                                };
                                let b = self.bits(&mut scope, path, e)?;
                                if b.len() != 1 {
                                    return Err(format!(
                                        "instance {name} pin {}: {} bits connected to a scalar pin",
                                        pin.name,
                                        b.len()
                                    ));
                                }
                                conns.push((pin.name.clone(), b[0]));
                            }
                            self.insts.push(Inst {
                                name,
                                cell: cell.to_string(),
                                conns,
                            });
                        } else {
                            let sub = *self.modules.get(cell).ok_or_else(|| {
                                format!("instance {name}: unknown cell or module '{cell}'")
                            })?;
                            let mut bind: FxHashMap<String, Vec<NetId>> = FxHashMap::default();
                            let mut pos = 0usize;
                            let sub_names = port_names(sub);
                            for c in &i.conns {
                                match c {
                                    PortConn::Named(p, Some(Some(e))) => {
                                        let b = self.bits(&mut scope, path, e)?;
                                        bind.insert(p.name.clone(), b);
                                    }
                                    PortConn::Ordered(Some(e)) => {
                                        let n = sub_names.get(pos).ok_or_else(|| {
                                            format!("instance {name}: too many connections")
                                        })?;
                                        let b = self.bits(&mut scope, path, e)?;
                                        bind.insert(n.clone(), b);
                                        pos += 1;
                                    }
                                    PortConn::Ordered(None) => pos += 1,
                                    _ => {}
                                }
                            }
                            self.depth += 1;
                            let mut sub_ports = Vec::new();
                            self.module(sub, &format!("{name}/"), Some(&bind), &mut sub_ports)?;
                            self.depth -= 1;
                        }
                    }
                }
                ModuleItem::Always(_) | ModuleItem::Initial(_) => {
                    return Err("behavioral code in a netlist".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn port_names(m: &Module) -> Vec<String> {
    match &m.ports {
        PortList::Ansi(ps) => ps.iter().map(|p| p.name.name.clone()).collect(),
        PortList::NonAnsi(list) => list
            .iter()
            .filter_map(|p| {
                p.expr
                    .as_ref()
                    .and_then(|e| e.as_ident())
                    .map(|i| i.name.clone())
            })
            .collect(),
        PortList::None => Vec::new(),
    }
}

/// Flattens the netlist. `is_cell` tells library cells apart from netlist modules.
pub fn flatten(
    units: &[SourceUnit],
    top: Option<&str>,
    is_cell: &dyn Fn(&str) -> bool,
) -> Result<Flat, String> {
    let mut modules: FxHashMap<&str, &Module> = FxHashMap::default();
    for u in units {
        for it in &u.items {
            if let Item::Module(m) = it {
                modules.insert(m.name.name.as_str(), m);
            }
        }
    }
    let top_name = match top {
        Some(t) => t.to_string(),
        None => {
            let mut used = rustc_hash::FxHashSet::default();
            for m in modules.values() {
                for it in &m.items {
                    if let ModuleItem::Instance(i) = it {
                        used.insert(i.module.name.clone());
                    }
                }
            }
            let mut c: Vec<&&str> = modules.keys().filter(|k| !used.contains(**k)).collect();
            c.sort();
            match c.as_slice() {
                [one] => one.to_string(),
                _ => return Err("cannot choose the top module; use --top".into()),
            }
        }
    };
    let m = *modules
        .get(top_name.as_str())
        .ok_or_else(|| format!("top module '{top_name}' not found"))?;
    let mut b = Builder {
        modules,
        is_cell,
        parent: vec![CONST0, CONST1],
        names: vec!["1'b0".into(), "1'b1".into()],
        insts: Vec::new(),
        depth: 0,
    };
    let mut ports = Vec::new();
    b.module(m, "", None, &mut ports)?;
    // Canonical net numbering: constants first, then representatives in order of creation.
    let n = b.parent.len();
    let mut canon = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut names = Vec::new();
    for x in 0..n as NetId {
        let r = b.find(x);
        if canon[r as usize] == u32::MAX {
            canon[r as usize] = next;
            names.push(b.names[r as usize].clone());
            next += 1;
        }
    }
    let map = |x: NetId, b: &mut Builder| canon[b.find(x) as usize];
    let mut out_ports = Vec::new();
    for (name, dir, d) in &ports {
        if !matches!(dir, Direction::Input | Direction::Output) {
            return Err(format!(
                "port '{name}': only input and output ports are supported"
            ));
        }
        let scalar = d.bits.len() == 1 && d.msb == 0 && d.lsb == 0;
        for (k, bit) in d.bits.iter().enumerate() {
            let idx = if d.msb >= d.lsb {
                d.lsb + k as i64
            } else {
                d.lsb - k as i64
            };
            out_ports.push(PortBit {
                name: if scalar {
                    name.clone()
                } else {
                    format!("{name}[{idx}]")
                },
                input: *dir == Direction::Input,
                net: map(*bit, &mut b),
            });
        }
    }
    let mut insts = std::mem::take(&mut b.insts);
    for i in &mut insts {
        for c in &mut i.conns {
            c.1 = map(c.1, &mut b);
        }
    }
    Ok(Flat {
        top: top_name,
        ports: out_ports,
        insts,
        num_nets: next as usize,
        net_names: names,
    })
}
