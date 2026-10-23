// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Structural netlist writer (VERILOG-REQ-0300, VERILOG-REQ-0302).

use std::fmt::Write as _;

use crate::verilog::token::keyword;

/// Port direction in a netlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NlDir {
    Input,
    Output,
}

/// A netlist port; `width == None` for scalars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlPort {
    pub name: String,
    pub dir: NlDir,
    pub width: Option<u32>,
}

/// A reference to a net bit or a constant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NlRef {
    /// A scalar net (or a whole port).
    Net(String),
    /// Bit `k` of a vector net.
    Bit(String, u32),
    Const(bool),
}

/// A cell instance with named connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NlInstance {
    pub cell: String,
    pub name: String,
    pub conns: Vec<(String, NlRef)>,
}

/// A flat structural module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NlModule {
    pub name: String,
    pub ports: Vec<NlPort>,
    /// Internal wires; `None` width for scalars.
    pub wires: Vec<(String, Option<u32>)>,
    pub instances: Vec<NlInstance>,
    /// `assign lhs = rhs;`
    pub assigns: Vec<(NlRef, NlRef)>,
}

/// True if `name` can be written without escaping.
pub fn is_simple_ident(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$')
        && keyword(name).is_none()
}

/// Writes a name, escaping it when necessary (backslash, name, one space).
pub fn ident(name: &str) -> String {
    if is_simple_ident(name) {
        name.to_string()
    } else {
        format!("\\{name} ")
    }
}

fn reference(r: &NlRef) -> String {
    match r {
        NlRef::Net(n) => ident(n),
        NlRef::Bit(n, k) => format!("{}[{k}]", ident(n)),
        NlRef::Const(false) => "1'b0".into(),
        NlRef::Const(true) => "1'b1".into(),
    }
}

/// Renders a module as Verilog text.
pub fn write_netlist(m: &NlModule) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "module {}(", ident(&m.name));
    for (i, p) in m.ports.iter().enumerate() {
        let dir = match p.dir {
            NlDir::Input => "input",
            NlDir::Output => "output",
        };
        let range = p
            .width
            .map(|w| format!(" [{}:0]", w.saturating_sub(1)))
            .unwrap_or_default();
        let sep = if i + 1 == m.ports.len() { "" } else { "," };
        let _ = writeln!(s, "  {dir} wire{range} {}{sep}", ident(&p.name));
    }
    let _ = writeln!(s, ");");
    for (w, width) in &m.wires {
        let range = width
            .map(|w| format!(" [{}:0]", w.saturating_sub(1)))
            .unwrap_or_default();
        let _ = writeln!(s, "  wire{range} {};", ident(w));
    }
    for inst in &m.instances {
        let _ = write!(s, "  {} {} (", ident(&inst.cell), ident(&inst.name));
        for (i, (pin, r)) in inst.conns.iter().enumerate() {
            let sep = if i + 1 == inst.conns.len() { "" } else { ", " };
            let _ = write!(s, ".{}({}){sep}", ident(pin), reference(r));
        }
        let _ = writeln!(s, ");");
    }
    for (l, r) in &m.assigns {
        let _ = writeln!(s, "  assign {} = {};", reference(l), reference(r));
    }
    let _ = writeln!(s, "endmodule");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verilog::ast::{Item, ModuleItem, PortConn, PortList};

    // VERILOG-REQ-0300 to VERILOG-REQ-0302
    #[test]
    fn escaping_and_round_trip() {
        assert_eq!(ident("a_1"), "a_1");
        assert_eq!(ident("u.q[3]"), "\\u.q[3] ");
        assert_eq!(ident("module"), "\\module ");
        let m = NlModule {
            name: "top".into(),
            ports: vec![
                NlPort {
                    name: "a".into(),
                    dir: NlDir::Input,
                    width: Some(2),
                },
                NlPort {
                    name: "y".into(),
                    dir: NlDir::Output,
                    width: None,
                },
            ],
            wires: vec![("u.q[0]".into(), None), ("n1".into(), None)],
            instances: vec![NlInstance {
                cell: "nd2".into(),
                name: "c0".into(),
                conns: vec![
                    ("A".into(), NlRef::Bit("a".into(), 0)),
                    ("B".into(), NlRef::Net("u.q[0]".into())),
                    ("Y".into(), NlRef::Net("n1".into())),
                ],
            }],
            assigns: vec![(NlRef::Net("y".into()), NlRef::Net("n1".into()))],
        };
        let text = write_netlist(&m);
        let unit = crate::verilog::test_util::parse_src(&text);
        let Item::Module(md) = &unit.items[0] else {
            panic!()
        };
        assert_eq!(md.name.name, "top");
        let PortList::Ansi(ports) = &md.ports else {
            panic!()
        };
        assert_eq!(ports.len(), 2);
        let inst = md.items.iter().find_map(|i| match i {
            ModuleItem::Instance(x) => Some(x),
            _ => None,
        });
        let inst = inst.unwrap();
        assert_eq!(inst.module.name, "nd2");
        let PortConn::Named(pin, Some(Some(e))) = &inst.instances[0].conns[1] else {
            panic!()
        };
        assert_eq!(pin.name, "B");
        assert_eq!(e.as_ident().unwrap().name, "u.q[0]");
        assert!(md.items.iter().any(|i| matches!(i, ModuleItem::Assign(_))));
    }
}
