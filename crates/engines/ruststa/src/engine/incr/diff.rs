// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Structural diff of two frozen netlists and their node correspondence (`owner: E11`;
//! `STA-REQ-0457`).
//!
//! Nets are matched by name, or for anonymous nets by their driver, so that the nets a buffer
//! insertion creates do not show as differences; instances and pins are matched by name. Names are
//! borrowed from the netlists, so a diff allocates no string per pin.

use rustc_hash::{FxHashMap, FxHashSet};
use rusteda_core::netlist::{ANONYMOUS, FrozenNetlist};

use super::cone::NodeMapping;
use crate::engine::types::NodeId;

/// Summary counts of a cross-revision [`FrozenNetlist`] diff (`STA-REQ-0457`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetlistDiffSummary {
    pub total_instances: usize,
    pub total_nets: usize,
    pub added_instances: usize,
    pub removed_instances: usize,
    pub resized_instances: usize,
    pub rewired_instances: usize,
    pub added_nets: usize,
    pub removed_nets: usize,
    pub modified_nets: usize,
    pub instance_dirty_fraction: f64,
    pub net_dirty_fraction: f64,
}

impl NetlistDiffSummary {
    /// Total number of dirty instances (`added + resized + rewired + removed`).
    #[inline]
    #[must_use]
    pub const fn dirty_instances(&self) -> usize {
        self.added_instances
            + self.resized_instances
            + self.rewired_instances
            + self.removed_instances
    }

    /// Total number of dirty nets (`added + modified + removed`).
    #[inline]
    #[must_use]
    pub const fn dirty_nets(&self) -> usize {
        self.added_nets + self.modified_nets + self.removed_nets
    }
}

/// Cross-revision structural diff and node correspondence between two [`FrozenNetlist`]s
/// (`STA-REQ-0457`).
#[derive(Debug, Clone)]
pub struct NetlistDiff {
    pub ports_changed: bool,
    pub summary: NetlistDiffSummary,
    pub mapping: NodeMapping,
}

/// Ids shared by the strings of two netlists: equal strings get equal ids, so that names compare
/// as integers.
struct SharedIds {
    old: Vec<u32>,
    new: Vec<u32>,
}

impl SharedIds {
    fn new<'x>(old: &'x FrozenNetlist<'x>, new: &'x FrozenNetlist<'x>) -> Self {
        let n_old = old.str_offs.len().saturating_sub(1);
        let n_new = new.str_offs.len().saturating_sub(1);
        let mut ids: FxHashMap<&'x [u8], u32> =
            FxHashMap::with_capacity_and_hasher(n_old + n_new / 8, Default::default());
        let mut intern = |f: &'x FrozenNetlist<'x>, k: usize| -> u32 {
            let (a, b) = (f.str_offs[k] as usize, f.str_offs[k + 1] as usize);
            let next = ids.len() as u32;
            *ids.entry(f.str_bytes.get(a..b).unwrap_or(&[]))
                .or_insert(next)
        };
        let old_ids = (0..n_old).map(|k| intern(old, k)).collect();
        let new_ids = (0..n_new).map(|k| intern(new, k)).collect();
        Self {
            old: old_ids,
            new: new_ids,
        }
    }
}

/// Identity of a net across two netlists (`STA-REQ-0457`): its name and bit index, or for an
/// anonymous net (one a buffer insertion made) its driver, so that such nets match by driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NetKey {
    /// A named net: shared id of its name, and its bit index.
    Named(u32, i32),
    /// An anonymous net driven by a pin: shared ids of the instance and pin names.
    DrivenByPin(u32, u32),
    /// An anonymous net driven by a port bit.
    DrivenByPortBit(u32),
    /// An anonymous net without a driver, or a constant: its row.
    Undriven(u32),
}

/// A member of a net: a port bit, or a pin by shared ids of instance, cell and pin names.
type Member = (u32, u32, u32, u32);

/// One netlist as the diff sees it: shared ids of names, the key of every net and the members of
/// every net, sorted, in compressed rows.
struct Side<'f> {
    f: &'f FrozenNetlist<'f>,
    ids: &'f [u32],
    keys: Vec<NetKey>,
    member_start: Vec<u32>,
    members: Vec<Member>,
}

impl<'f> Side<'f> {
    fn new(f: &'f FrozenNetlist<'f>, ids: &'f [u32]) -> Self {
        let sid = |k: u32| ids.get(k as usize).copied().unwrap_or(u32::MAX);
        let n_nets = f.nets.len();
        let mut driver: Vec<Option<NetKey>> = vec![None; n_nets];
        for (pb, &net) in f.port_bit_net.iter().enumerate() {
            if let Some(d) = driver.get_mut(net as usize)
                && d.is_none()
            {
                *d = Some(NetKey::DrivenByPortBit(pb as u32));
            }
        }
        let mut count = vec![0u32; n_nets + 1];
        for &net in f.port_bit_net.iter() {
            if (net as usize) < n_nets {
                count[net as usize + 1] += 1;
            }
        }
        for (inst_row, rec) in f.instances.iter().enumerate() {
            let cpins = f.cell_pins(rec.cell);
            for pin_row in f.instance_pins(inst_row as u32) {
                let cp = cpins.start + f.pin_cellpin[pin_row] as usize;
                let net = f.pin_net[pin_row] as usize;
                if net >= n_nets {
                    continue;
                }
                count[net + 1] += 1;
                if matches!(f.cell_pin_dir.get(cp).copied().unwrap_or(0), 1 | 2) {
                    let pin = f.cell_pin_name.get(cp).map_or(u32::MAX, |&s| sid(s));
                    driver[net] = Some(NetKey::DrivenByPin(sid(rec.name), pin));
                }
            }
        }
        for i in 0..n_nets {
            count[i + 1] += count[i];
        }
        let mut fill = count.clone();
        let mut members = vec![(0, 0, 0, 0); count[n_nets] as usize];
        for (pb, &net) in f.port_bit_net.iter().enumerate() {
            if (net as usize) < n_nets {
                members[fill[net as usize] as usize] = (0, pb as u32, 0, 0);
                fill[net as usize] += 1;
            }
        }
        for (inst_row, rec) in f.instances.iter().enumerate() {
            let cell = f.cells.get(rec.cell as usize).map_or(u32::MAX, |&s| sid(s));
            let cpins = f.cell_pins(rec.cell);
            for pin_row in f.instance_pins(inst_row as u32) {
                let net = f.pin_net[pin_row] as usize;
                if net >= n_nets {
                    continue;
                }
                let cp = cpins.start + f.pin_cellpin[pin_row] as usize;
                let pin = f.cell_pin_name.get(cp).map_or(u32::MAX, |&s| sid(s));
                members[fill[net] as usize] = (1, sid(rec.name), cell, pin);
                fill[net] += 1;
            }
        }
        for n in 0..n_nets {
            members[count[n] as usize..count[n + 1] as usize].sort_unstable();
        }
        let keys = f
            .nets
            .iter()
            .enumerate()
            .map(|(n, rec)| {
                if rec.name != ANONYMOUS && f.string(rec.name).is_ok_and(|s| !s.is_empty()) {
                    NetKey::Named(sid(rec.name), rec.index)
                } else {
                    driver[n].unwrap_or(NetKey::Undriven(n as u32))
                }
            })
            .collect();
        Self {
            f,
            ids,
            keys,
            member_start: count,
            members,
        }
    }

    fn sid(&self, k: u32) -> u32 {
        self.ids.get(k as usize).copied().unwrap_or(u32::MAX)
    }

    /// The key of the connection of pin row `pin_row`.
    fn pin_key(&self, pin_row: usize) -> NetKey {
        let net = self.f.pin_net[pin_row];
        self.keys
            .get(net as usize)
            .copied()
            .unwrap_or(NetKey::Undriven(net))
    }

    fn members_of(&self, net: usize) -> &[Member] {
        &self.members[self.member_start[net] as usize..self.member_start[net + 1] as usize]
    }

    /// The shared id of the cell pin name of pin row `pin_row` of instance record `rec`.
    fn pin_name(&self, cell: u32, pin_row: usize) -> u32 {
        let cp = self.f.cell_pins(cell).start + self.f.pin_cellpin[pin_row] as usize;
        self.f
            .cell_pin_name
            .get(cp)
            .map_or(u32::MAX, |&s| self.sid(s))
    }
}

/// Computes the structural diff and [`NodeMapping`] between two [`FrozenNetlist`]s
/// (`STA-REQ-0457`). Names are compared through ids shared by the two string tables, so the diff
/// is linear in the size of the netlists and allocates no string.
#[must_use]
pub fn diff_frozen_netlists(old: &FrozenNetlist<'_>, new: &FrozenNetlist<'_>) -> NetlistDiff {
    let ports_changed = !ports_equivalent(old, new);
    let ids = SharedIds::new(old, new);
    let o = Side::new(old, &ids.old);
    let n = Side::new(new, &ids.new);
    let new_pb = new.port_bit_net.len();
    let mut directly_dirty = vec![false; new_pb + new.pin_net.len()];

    let mut old_insts: FxHashMap<u32, u32> =
        FxHashMap::with_capacity_and_hasher(old.instances.len(), Default::default());
    for (row, rec) in old.instances.iter().enumerate() {
        old_insts.insert(o.sid(rec.name), row as u32);
    }

    let mut added_instances = 0usize;
    let mut resized_instances = 0usize;
    let mut rewired_instances = 0usize;
    let mut seen_old_insts = 0usize;
    let mut dirty_new_nets = vec![false; new.nets.len()];

    for (new_row, rec) in new.instances.iter().enumerate() {
        let new_pins = new.instance_pins(new_row as u32);
        let inst_dirty = match old_insts.get(&n.sid(rec.name)) {
            Some(&old_row) => {
                seen_old_insts += 1;
                let orec = &old.instances[old_row as usize];
                let old_pins = old.instance_pins(old_row);
                let cell_of = |s: &Side<'_>, f: &FrozenNetlist<'_>, c: u32| {
                    f.cells.get(c as usize).map_or(u32::MAX, |&x| s.sid(x))
                };
                if cell_of(&o, old, orec.cell) != cell_of(&n, new, rec.cell) {
                    resized_instances += 1;
                    true
                } else if old_pins.len() != new_pins.len()
                    || old_pins.clone().zip(new_pins.clone()).any(|(op, np)| {
                        o.pin_name(orec.cell, op) != n.pin_name(rec.cell, np)
                            || o.pin_key(op) != n.pin_key(np)
                    })
                {
                    rewired_instances += 1;
                    true
                } else {
                    false
                }
            }
            None => {
                added_instances += 1;
                true
            }
        };
        if inst_dirty {
            for pin_row in new_pins {
                directly_dirty[new_pb + pin_row] = true;
                if let Some(d) = dirty_new_nets.get_mut(new.pin_net[pin_row] as usize) {
                    *d = true;
                }
            }
        }
    }
    let removed_instances = old_insts.len().saturating_sub(seen_old_insts);

    let mut old_nets: FxHashMap<NetKey, u32> =
        FxHashMap::with_capacity_and_hasher(old.nets.len(), Default::default());
    for (row, &k) in o.keys.iter().enumerate() {
        old_nets.insert(k, row as u32);
    }
    let mut added_nets = 0usize;
    let mut modified_nets = 0usize;
    let mut seen_old_nets = FxHashSet::default();
    for (row, k) in n.keys.iter().enumerate() {
        if let Some(&old_row) = old_nets.get(k) {
            seen_old_nets.insert(old_row);
            if o.members_of(old_row as usize) != n.members_of(row) || dirty_new_nets[row] {
                modified_nets += 1;
                dirty_new_nets[row] = true;
            }
        } else {
            added_nets += 1;
            dirty_new_nets[row] = true;
        }
    }
    let removed_nets = old_nets.len().saturating_sub(seen_old_nets.len());

    for (pb, &net_id) in new.port_bit_net.iter().enumerate() {
        if dirty_new_nets
            .get(net_id as usize)
            .copied()
            .unwrap_or(false)
        {
            directly_dirty[pb] = true;
        }
    }
    for (pin_row, &net_id) in new.pin_net.iter().enumerate() {
        if dirty_new_nets
            .get(net_id as usize)
            .copied()
            .unwrap_or(false)
        {
            directly_dirty[new_pb + pin_row] = true;
        }
    }

    let total_instances = new.instances.len();
    let total_nets = new.nets.len();
    let mut summary = NetlistDiffSummary {
        total_instances,
        total_nets,
        added_instances,
        removed_instances,
        resized_instances,
        rewired_instances,
        added_nets,
        removed_nets,
        modified_nets,
        instance_dirty_fraction: 0.0,
        net_dirty_fraction: 0.0,
    };
    summary.instance_dirty_fraction =
        summary.dirty_instances() as f64 / total_instances.max(1) as f64;
    summary.net_dirty_fraction = summary.dirty_nets() as f64 / total_nets.max(1) as f64;

    let mapping = node_mapping(&o, &n, directly_dirty);
    NetlistDiff {
        ports_changed,
        summary,
        mapping,
    }
}

/// The node correspondence of the two sides: port bits by position, connected pins by instance
/// and pin name (`STA-REQ-0457`).
fn node_mapping(o: &Side<'_>, n: &Side<'_>, mut dirty: Vec<bool>) -> NodeMapping {
    let (old, new) = (o.f, n.f);
    let old_pb = old.port_bit_net.len();
    let new_pb = new.port_bit_net.len();
    let old_total = old_pb + old.pin_net.len();
    let new_total = new_pb + new.pin_net.len();
    let mut old_pins: FxHashMap<(u32, u32), u32> =
        FxHashMap::with_capacity_and_hasher(old.pin_net.len(), Default::default());
    for (row, rec) in old.instances.iter().enumerate() {
        for pin_row in old.instance_pins(row as u32) {
            old_pins.insert(
                (o.sid(rec.name), o.pin_name(rec.cell, pin_row)),
                (old_pb + pin_row) as u32,
            );
        }
    }
    let mut new_to_old = vec![None; new_total];
    let mut old_to_new = vec![None; old_total];
    dirty.resize(new_total, false);
    let common_pb = old_pb.min(new_pb);
    for pb in 0..common_pb {
        new_to_old[pb] = Some(NodeId(pb as u32));
        old_to_new[pb] = Some(NodeId(pb as u32));
    }
    for d in dirty.iter_mut().take(new_pb).skip(common_pb) {
        *d = true;
    }
    for (row, rec) in new.instances.iter().enumerate() {
        for pin_row in new.instance_pins(row as u32) {
            let new_idx = new_pb + pin_row;
            match old_pins.get(&(n.sid(rec.name), n.pin_name(rec.cell, pin_row))) {
                Some(&old_idx) => {
                    new_to_old[new_idx] = Some(NodeId(old_idx));
                    old_to_new[old_idx as usize] = Some(NodeId(new_idx as u32));
                }
                None => dirty[new_idx] = true,
            }
        }
    }
    NodeMapping {
        new_to_old,
        old_to_new,
        directly_dirty: dirty,
    }
}

fn ports_equivalent(old: &FrozenNetlist<'_>, new: &FrozenNetlist<'_>) -> bool {
    if old.ports.len() != new.ports.len() || old.port_bit_net.len() != new.port_bit_net.len() {
        return false;
    }
    for (op, np) in old.ports.iter().zip(new.ports.iter()) {
        if op.dir != np.dir
            || op.is_bus != np.is_bus
            || op.msb != np.msb
            || op.lsb != np.lsb
            || old.string(op.name).ok() != new.string(np.name).ok()
        {
            return false;
        }
    }
    true
}
