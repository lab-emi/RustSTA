// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tests of the support modules of the engine's tests (owner: E1).

#[path = "mod.rs"]
mod support;

use std::collections::HashMap;

use rusteda_formats::liberty::{Library, read_text};
use ruststa::netlist::{CONST0, CONST1, Flat, NetId};
use ruststa_fixtures::liberty::{Variant, library};
use ruststa_fixtures::netlists::{CORPUS, LANES_K1};
use support::legacy_import::{
    Assign, CellType, Conn, Instance, LegacyImport, NetRef, PinDir, Port, PortDir, import,
    import_frozen, read_flat, read_structural_frozen, steps,
};

fn wt_full() -> Library {
    read_text(&library(Variant::Full), Variant::Full.name()).expect("synthetic library")
}

/// The connection of a port bit after its assignment, if it has one.
fn resolve(s: &LegacyImport, r: NetRef) -> Conn {
    s.assigns
        .iter()
        .find(|a| a.lhs == r)
        .map_or(Conn::Net(r), |a| a.rhs)
}

/// Checks that the building steps connect exactly what the flat netlist connects: the same port
/// bits and instance pins share a net, and the same ones are constant.
fn check_against_flat(flat: &Flat, s: &LegacyImport) {
    assert_eq!(s.module, flat.top);
    let mut of_step: HashMap<NetRef, NetId> = HashMap::new();
    let mut of_flat: HashMap<NetId, NetRef> = HashMap::new();
    let mut bind = |c: Conn, f: NetId| match c {
        Conn::Const(v) => assert_eq!(f, if v { CONST1 } else { CONST0 }),
        Conn::Net(r) => {
            assert!(f > CONST1, "{r:?} is constant in the flat netlist");
            assert_eq!(
                *of_step.entry(r).or_insert(f),
                f,
                "{r:?} joins two flat nets"
            );
            assert_eq!(*of_flat.entry(f).or_insert(r), r, "flat net {f} is split");
        }
    };
    // The flat netlist lists the port bits port by port, least significant bit first.
    let mut flat_bits = flat.ports.iter();
    let mut port_bits = 0;
    for (port, p) in s.ports.iter().enumerate() {
        for (bit, index) in p.bits().into_iter().enumerate().rev() {
            let b = flat_bits.next().expect("a flat port bit");
            assert_eq!(b.name, p.bit_name(index));
            assert_eq!(b.input, p.dir == PortDir::Input);
            bind(resolve(s, NetRef::PortBit { port, bit }), b.net);
            port_bits += 1;
        }
    }
    assert!(flat_bits.next().is_none());
    assert_eq!(s.instances.len(), flat.insts.len());
    for (i, fi) in s.instances.iter().zip(&flat.insts) {
        assert_eq!(i.name, fi.name);
        assert_eq!(s.cells[i.cell].name, fi.cell);
        assert_eq!(i.conns.len(), fi.conns.len());
        for ((pin, c), (fpin, f)) in i.conns.iter().zip(&fi.conns) {
            assert_eq!(pin, fpin);
            if let Conn::Net(r) = c {
                assert!(s.assigns.iter().all(|a| a.lhs != *r), "{r:?} is assigned");
            }
            bind(*c, *f);
        }
    }
    for a in &s.assigns {
        assert!(matches!(a.lhs, NetRef::PortBit { .. }));
    }
    for (k, name) in s.nets.iter().enumerate() {
        if let Some(f) = of_step.get(&NetRef::Net(k)) {
            assert_eq!(&flat.net_names[*f as usize], name);
        }
    }
    // One net of the steps per non-constant flat net: the port bits without an assignment and
    // the other nets.
    assert_eq!(
        port_bits - s.assigns.len() + s.nets.len(),
        flat.num_nets - 2
    );
}

// STA-REQ-0001: every corpus netlist, flattened by the legacy flattener, becomes building steps
// that connect the same pins, deterministically, and freezes to a valid FrozenNetlist.
#[test]
fn corpus_netlists_map_one_to_one() {
    let libs = [wt_full()];
    let mut seen = 0;
    for n in CORPUS.iter().chain([&LANES_K1]) {
        let flat = read_flat(n.name, n.text, Some(n.top), &libs).expect(n.name);
        let s = steps(&flat, &libs).unwrap_or_else(|e| panic!("{}: {e}", n.name));
        check_against_flat(&flat, &s);
        assert_eq!(import(n.name, n.text, Some(n.top), &libs).as_ref(), Ok(&s));
        assert!(!s.instances.is_empty() && !s.ports.is_empty(), "{}", n.name);
        let frozen = import_frozen(n.name, n.text, Some(n.top), &libs).expect(n.name);
        frozen.validate().expect(n.name);
        let frozen_vlog = read_structural_frozen(n.name, n.text, &libs).expect(n.name);
        frozen_vlog.validate().expect(n.name);
        seen += 1;
    }
    assert_eq!(seen, 32);
}

const HIER: &str = "
module leaf(input i, output o);
  wtf_inv_1 u (.A(i), .Y(o));
endmodule

module top(input a, input [1:0] b, output y, output z, output [0:1] w, output k);
  wire n;
  wire spare;
  leaf u1 (.i(a), .o(n));
  wtf_and2_1 g (.A(n), .B(b[1]), .X(y));
  assign z = y;
  assign w[0] = b[0];
  assign w[1] = 1'b1;
  wtf_inv_1 c (.A(1'b0), .Y(k));
endmodule
";

// STA-REQ-0001: flattening joins instance names with `/` and nets with `assign`; constants are
// nets without transitions. The steps keep one net per joined set: the first input port bit's,
// then the first port bit's, then a named net; other port bits and constant port bits are
// assigned.
#[test]
fn hierarchy_assignments_and_constants() {
    let libs = [wt_full()];
    let s = import("hier.v", HIER, None, &libs).unwrap();
    check_against_flat(&read_flat("hier.v", HIER, None, &libs).unwrap(), &s);
    let (input, output) = (PinDir::Input, PinDir::Output);
    let bit = |port, bit| Conn::Net(NetRef::PortBit { port, bit });
    let port = |name: &str, dir, range| Port {
        name: name.into(),
        dir,
        range,
    };
    let want = LegacyImport {
        module: "top".into(),
        cells: vec![
            CellType {
                name: "wtf_inv_1".into(),
                pins: vec![("A".into(), input), ("Y".into(), output)],
            },
            CellType {
                name: "wtf_and2_1".into(),
                pins: vec![
                    ("A".into(), input),
                    ("B".into(), input),
                    ("X".into(), output),
                ],
            },
        ],
        ports: vec![
            port("a", PortDir::Input, None),
            port("b", PortDir::Input, Some((1, 0))),
            port("y", PortDir::Output, None),
            port("z", PortDir::Output, None),
            port("w", PortDir::Output, Some((0, 1))),
            port("k", PortDir::Output, None),
        ],
        // `n` joins the output `o` of `u1`, whose name the flattener keeps; `spare` has no pin.
        nets: vec!["u1/o".into(), "spare".into()],
        instances: vec![
            Instance {
                name: "u1/u".into(),
                cell: 0,
                conns: vec![
                    ("A".into(), bit(0, 0)),
                    ("Y".into(), Conn::Net(NetRef::Net(0))),
                ],
            },
            Instance {
                name: "g".into(),
                cell: 1,
                conns: vec![
                    ("A".into(), Conn::Net(NetRef::Net(0))),
                    ("B".into(), bit(1, 0)),
                    ("X".into(), bit(2, 0)),
                ],
            },
            Instance {
                name: "c".into(),
                cell: 0,
                conns: vec![("A".into(), Conn::Const(false)), ("Y".into(), bit(5, 0))],
            },
        ],
        assigns: vec![
            Assign {
                lhs: NetRef::PortBit { port: 3, bit: 0 },
                rhs: bit(2, 0),
            },
            Assign {
                lhs: NetRef::PortBit { port: 4, bit: 0 },
                rhs: bit(1, 1),
            },
            Assign {
                lhs: NetRef::PortBit { port: 4, bit: 1 },
                rhs: Conn::Const(true),
            },
        ],
    };
    assert_eq!(s, want);
    assert_eq!(s.ports[1].bits(), [Some(1), Some(0)]);
    assert_eq!(s.ports[4].bits(), [Some(0), Some(1)]);
    assert_eq!(s.ports[0].bits(), [None]);
}

// STA-REQ-0003: unknown pins are errors; names that the shared netlist model cannot hold are
// errors too.
#[test]
fn unknown_pins_and_invalid_names_are_errors() {
    let libs = [wt_full()];
    let pin = "module t(input a, output y);\n  wtf_inv_1 u (.A(a), .Q(y));\nendmodule\n";
    assert_eq!(
        import("pin.v", pin, None, &libs).unwrap_err(),
        "instance u: cell wtf_inv_1 has no pin Q"
    );
    let name = "module t(input a, output y);\n  wire \\p\\q ;\n  wtf_inv_1 u (.A(a), .Y(\\p\\q ));\n  wtf_inv_1 v (.A(\\p\\q ), .Y(y));\nendmodule\n";
    let e = import("name.v", name, None, &libs).unwrap_err();
    assert!(e.contains("backslash"), "{e}");
}
