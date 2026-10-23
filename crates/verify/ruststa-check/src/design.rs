// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Netlist reading for the checker: every net bit gets a textual key; `assign` statements and
//! port bindings join keys (STA-REQ-0001 to STA-REQ-0003).

use std::collections::HashMap;

use rusteda_formats::verilog::ast::{
    Dim, Direction, Expr, ExprKind, Item, Module, ModuleItem, PortConn, PortList, SourceUnit, UnOp,
};
use rusteda_formats::verilog::number::Bit4;

pub const ZERO: &str = "#0";
pub const ONE: &str = "#1";

/// A flat design with textual net keys.
#[derive(Debug, Default)]
pub struct Design {
    pub top: String,
    /// (name, is input, net key)
    pub ports: Vec<(String, bool, String)>,
    /// (instance name, cell, [(pin, net key)])
    pub cells: Vec<(String, String, Vec<(String, String)>)>,
    parent: HashMap<String, String>,
}

impl Design {
    fn root(&mut self, k: &str) -> String {
        let mut cur = k.to_string();
        let mut path = Vec::new();
        while let Some(p) = self.parent.get(&cur) {
            if *p == cur {
                break;
            }
            path.push(cur.clone());
            cur = p.clone();
        }
        for x in path {
            self.parent.insert(x, cur.clone());
        }
        cur
    }

    fn join(&mut self, a: &str, b: &str) -> Result<(), String> {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra == rb {
            return Ok(());
        }
        let konst = |x: &str| x == ZERO || x == ONE;
        if konst(&ra) && konst(&rb) {
            return Err(format!("nets {a} and {b} join both constants"));
        }
        // Constants stay representatives; otherwise the smaller key wins (any fixed rule works).
        let (keep, other) = if konst(&rb) || (!konst(&ra) && rb < ra) {
            (rb, ra)
        } else {
            (ra, rb)
        };
        self.parent.insert(other, keep);
        Ok(())
    }

    /// Final net key of a bit key.
    pub fn net(&mut self, k: &str) -> String {
        self.root(k)
    }
}

/// Declared names: `None` for scalars, `Some((msb, lsb))` for vectors.
type Scope = HashMap<String, Option<(i64, i64)>>;

fn number(e: &Expr) -> Result<i64, String> {
    match &e.kind {
        ExprKind::Number(l) => Ok(l
            .bits()?
            .iter()
            .enumerate()
            .take(62)
            .filter(|(_, b)| **b == Bit4::One)
            .map(|(i, _)| 1i64 << i)
            .sum()),
        ExprKind::Unary(UnOp::Minus, x) => Ok(-number(x)?),
        _ => Err("non-constant index in a netlist".into()),
    }
}

fn dims_range(d: &[Dim]) -> Result<Option<(i64, i64)>, String> {
    Ok(match d {
        [] => None,
        [Dim::Range(a, b)] => Some((number(a)?, number(b)?)),
        [Dim::Size(n)] => Some((number(n)? - 1, 0)),
        _ => return Err("multi-dimensional net in a netlist".into()),
    })
}

/// Bit keys of indices `i..=j` of vector `name` declared `[msb:lsb]`, least significant first.
fn vector_keys(path: &str, name: &str, (msb, lsb): (i64, i64), i: i64, j: i64) -> Vec<String> {
    let (lo, hi) = (i.min(j), i.max(j));
    let mut idx: Vec<i64> = (lo..=hi).collect();
    // Least significant first: toward lsb first.
    if msb < lsb {
        idx.reverse();
    }
    idx.into_iter()
        .map(|k| format!("{path}{name}[{k}]"))
        .collect()
}

fn all_keys(path: &str, name: &str, decl: Option<(i64, i64)>) -> Vec<String> {
    match decl {
        None => vec![format!("{path}{name}")],
        Some(r) => vector_keys(path, name, r, r.0, r.1),
    }
}

fn expr_keys(scope: &mut Scope, path: &str, e: &Expr) -> Result<Vec<String>, String> {
    Ok(match &e.kind {
        ExprKind::Ident(p) => {
            let n = &p.name.name;
            let decl = *scope.entry(n.clone()).or_insert(None);
            all_keys(path, n, decl)
        }
        ExprKind::Index(b, i) => {
            let ExprKind::Ident(p) = &b.kind else {
                return Err("bit-select of an expression".into());
            };
            vec![format!("{path}{}[{}]", p.name.name, number(i)?)]
        }
        ExprKind::Slice {
            base, left, right, ..
        } => {
            let ExprKind::Ident(p) = &base.kind else {
                return Err("part-select of an expression".into());
            };
            let decl = scope
                .get(&p.name.name)
                .copied()
                .flatten()
                .ok_or_else(|| format!("part-select of scalar {}", p.name.name))?;
            vector_keys(path, &p.name.name, decl, number(left)?, number(right)?)
        }
        ExprKind::Concat(items) => {
            let mut out = Vec::new();
            for it in items.iter().rev() {
                out.extend(expr_keys(scope, path, it)?);
            }
            out
        }
        ExprKind::Repl(n, items) => {
            let mut one = Vec::new();
            for it in items.iter().rev() {
                one.extend(expr_keys(scope, path, it)?);
            }
            let k = number(n)?.max(0) as usize;
            (0..k).flat_map(|_| one.clone()).collect()
        }
        ExprKind::Number(l) => l
            .bits()?
            .iter()
            .map(|b| if *b == Bit4::One { ONE } else { ZERO }.to_string())
            .collect(),
        ExprKind::UnbasedUnsized(c) => vec![if *c == '1' { ONE } else { ZERO }.to_string()],
        _ => return Err("unsupported netlist expression".into()),
    })
}

struct Reader<'a> {
    modules: HashMap<String, &'a Module>,
    is_cell: &'a dyn Fn(&str) -> bool,
}

impl Reader<'_> {
    /// Elaborates `m` under `path`; returns its ports as (name, direction, keys).
    fn module(
        &self,
        d: &mut Design,
        m: &Module,
        path: &str,
        depth: usize,
    ) -> Result<Vec<(String, Direction, Vec<String>)>, String> {
        if depth > 64 {
            return Err("hierarchy too deep".into());
        }
        let mut scope: Scope = HashMap::new();
        let mut dirs: Vec<(String, Direction)> = Vec::new();
        let declare = |scope: &mut Scope, n: &str, r: Option<(i64, i64)>| {
            let e = scope.entry(n.to_string()).or_insert(None);
            if r.is_some() {
                *e = r;
            }
        };
        if let PortList::Ansi(ps) = &m.ports {
            for p in ps {
                declare(&mut scope, &p.name.name, dims_range(&p.ty.packed)?);
                dirs.push((p.name.name.clone(), p.dir));
            }
        }
        for it in &m.items {
            match it {
                ModuleItem::PortDecl(pd) => {
                    for (n, dims, _) in &pd.names {
                        let r = dims_range(&pd.ty.packed)?.or(dims_range(dims)?);
                        declare(&mut scope, &n.name, r);
                        dirs.push((n.name.clone(), pd.dir));
                    }
                }
                ModuleItem::Data(dd) => {
                    for x in &dd.decls {
                        declare(&mut scope, &x.name.name, dims_range(&dd.ty.packed)?);
                    }
                }
                _ => {}
            }
        }
        let order: Vec<(String, Direction)> = match &m.ports {
            PortList::NonAnsi(list) => {
                let mut v = Vec::new();
                for p in list {
                    let Some(id) = p.expr.as_ref().and_then(|e| e.as_ident()) else {
                        continue;
                    };
                    let dir = dirs
                        .iter()
                        .find(|x| x.0 == id.name)
                        .map(|x| x.1)
                        .ok_or_else(|| format!("port {} without direction", id.name))?;
                    v.push((id.name.clone(), dir));
                }
                v
            }
            _ => dirs.clone(),
        };
        let mut ports = Vec::new();
        for (n, dir) in &order {
            let decl = scope.get(n).copied().flatten();
            ports.push((n.clone(), *dir, all_keys(path, n, decl)));
        }
        for it in &m.items {
            match it {
                ModuleItem::Assign(v) => {
                    for a in v {
                        let l = expr_keys(&mut scope, path, &a.lhs)?;
                        let r = expr_keys(&mut scope, path, &a.rhs)?;
                        for (i, k) in l.iter().enumerate() {
                            let rk = r.get(i).cloned().unwrap_or_else(|| ZERO.to_string());
                            d.join(k, &rk)?;
                        }
                    }
                }
                ModuleItem::Instance(ins) => {
                    for i in &ins.instances {
                        let name = format!("{path}{}", i.name.name);
                        let ty = &ins.module.name;
                        if (self.is_cell)(ty) {
                            let mut pins = Vec::new();
                            for c in &i.conns {
                                if let PortConn::Named(p, Some(Some(e))) = c {
                                    let k = expr_keys(&mut scope, path, e)?;
                                    if k.len() != 1 {
                                        return Err(format!("{name}.{}: not one bit", p.name));
                                    }
                                    pins.push((p.name.clone(), k[0].clone()));
                                }
                            }
                            d.cells.push((name, ty.clone(), pins));
                        } else {
                            let sub = self
                                .modules
                                .get(ty)
                                .ok_or_else(|| format!("unknown module or cell {ty}"))?;
                            let sub_ports = self.module(d, sub, &format!("{name}/"), depth + 1)?;
                            for (pos, c) in i.conns.iter().enumerate() {
                                let (pname, e) = match c {
                                    PortConn::Named(p, Some(Some(e))) => (p.name.clone(), e),
                                    PortConn::Ordered(Some(e)) => (
                                        sub_ports
                                            .get(pos)
                                            .map(|x| x.0.clone())
                                            .ok_or("too many connections")?,
                                        e,
                                    ),
                                    _ => continue,
                                };
                                let outer = expr_keys(&mut scope, path, e)?;
                                let inner = &sub_ports
                                    .iter()
                                    .find(|x| x.0 == pname)
                                    .ok_or_else(|| format!("{ty} has no port {pname}"))?
                                    .2;
                                for (k, key) in inner.iter().enumerate() {
                                    let o = outer.get(k).cloned().unwrap_or_else(|| ZERO.into());
                                    d.join(key, &o)?;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(ports)
    }
}

/// Reads the flat design.
pub fn read(
    units: &[SourceUnit],
    top: Option<&str>,
    is_cell: &dyn Fn(&str) -> bool,
) -> Result<Design, String> {
    let mut modules: HashMap<String, &Module> = HashMap::new();
    for u in units {
        for it in &u.items {
            if let Item::Module(m) = it {
                modules.insert(m.name.name.clone(), m);
            }
        }
    }
    let top = match top {
        Some(t) => t.to_string(),
        None => {
            let mut used = std::collections::HashSet::new();
            for m in modules.values() {
                for it in &m.items {
                    if let ModuleItem::Instance(i) = it {
                        used.insert(i.module.name.clone());
                    }
                }
            }
            let mut c: Vec<&String> = modules.keys().filter(|k| !used.contains(*k)).collect();
            c.sort();
            if c.len() != 1 {
                return Err("top module is ambiguous".into());
            }
            c[0].clone()
        }
    };
    let m = *modules
        .get(&top)
        .ok_or_else(|| format!("no module {top}"))?;
    let r = Reader { modules, is_cell };
    let mut d = Design {
        top: top.clone(),
        ..Design::default()
    };
    let ports = r.module(&mut d, m, "", 0)?;
    for (_, dir, ks) in ports {
        for k in ks {
            // At the top level the bit key is the port bit name (`a` or `a[3]`).
            d.ports.push((k.clone(), dir == Direction::Input, k));
        }
    }
    Ok(d)
}
