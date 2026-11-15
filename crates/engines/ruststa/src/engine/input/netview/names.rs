// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Formatting of frozen netlist names.

use std::{borrow::Cow, fmt};

use rusteda_core::netlist::{ANONYMOUS, FrozenNetlist, SCALAR};

use super::str_of;
use crate::engine::types::{NetRow, NodeId};

pub(super) fn write_node_name_raw(
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
        write_pin_name_raw(frozen, pin_row, frozen.pin_instance(pin_row), out)
    }
}

/// Writes `inst/pin` for instance pin `pin_row` of instance `inst_row`.
pub(super) fn write_pin_name_raw(
    frozen: &FrozenNetlist<'_>,
    pin_row: u32,
    inst_row: u32,
    out: &mut impl fmt::Write,
) -> fmt::Result {
    let inst_rec = &frozen.instances[inst_row as usize];
    let inst_name = str_of(frozen, inst_rec.name);
    let cp = frozen.pin_cellpin[pin_row as usize] as usize;
    let slot = frozen.cell_pin_start[inst_rec.cell as usize] as usize + cp;
    let pin_name = str_of(frozen, frozen.cell_pin_name[slot]);
    write!(out, "{inst_name}/{pin_name}")
}

pub(super) fn write_net_name_raw(
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

pub(super) fn format_net_name_raw<'a>(frozen: &'a FrozenNetlist<'a>, net: NetRow) -> Cow<'a, str> {
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
