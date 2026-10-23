// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Net loads per transition and bound (owner: E3).
//!
//! Engine section B. The load of a net is the sum, in `NET_PINS` order, of the capacitances of
//! its load pins (rise or fall, early or late), the output-pin capacitance of every driver, the
//! `set_load -pin_load` and `-wire_load` values, and the wire capacitance that the wire model
//! ([`WireModel`](super::wire::WireModel)) adds.

use rustc_hash::FxHashMap;
use rusteda_core::netlist::FrozenNetlist;

use super::lut::RangeTracker;
use super::wire::{DriverLoad, WireModel};
use crate::engine::input::libview::{CellPin, TimingCell, TimingLib};
use crate::engine::types::{An, NetRow, Tr};

/// Explicit capacitive load additions (`set_load -pin_load` and `-wire_load`) on one net, in pF,
/// indexed `[tr][an]`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetLoadSpec {
    /// Additional pin load capacitance `[tr][an]` in pF (`set_load -pin_load`).
    pub pin_load: [[f64; 2]; 2],
    /// Additional wire load capacitance `[tr][an]` in pF (`set_load -wire_load`).
    pub wire_load: [[f64; 2]; 2],
}

impl NetLoadSpec {
    /// Creates a [`NetLoadSpec`] with uniform `pin_load` and `wire_load` across transitions and
    /// bounds.
    #[must_use]
    pub fn uniform(pin_load: f64, wire_load: f64) -> Self {
        Self {
            pin_load: [[pin_load; 2]; 2],
            wire_load: [[wire_load; 2]; 2],
        }
    }

    /// Adds `[early, late]` pin load (in pF) for both transitions.
    pub fn add_pin_load(&mut self, early: f64, late: f64) {
        for tr in 0..2 {
            self.pin_load[tr][An::Early.index()] += early;
            self.pin_load[tr][An::Late.index()] += late;
        }
    }

    /// Adds `[early, late]` wire load (in pF) for both transitions.
    pub fn add_wire_load(&mut self, early: f64, late: f64) {
        for tr in 0..2 {
            self.wire_load[tr][An::Early.index()] += early;
            self.wire_load[tr][An::Late.index()] += late;
        }
    }
}

/// Collection of `set_load` overrides on nets and top-level port bits (`STA-REQ-0110`).
#[derive(Debug, Clone, Default)]
pub struct NetLoadOverrides {
    /// Per-net load overrides indexed by [`NetRow`].
    pub by_net: FxHashMap<NetRow, NetLoadSpec>,
    /// Per-port-bit pin load overrides `[early, late]` in pF, indexed by port-bit position in
    /// `FrozenNetlist::port_bit_net`.
    pub by_port_bit: FxHashMap<u32, [f64; 2]>,
}

impl NetLoadOverrides {
    /// Creates an empty override set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `set_load -pin_load` `[early, late]` (in pF) on `net`.
    pub fn add_net_pin_load(&mut self, net: NetRow, early: f64, late: f64) {
        self.by_net
            .entry(net)
            .or_default()
            .add_pin_load(early, late);
    }

    /// Adds `set_load -wire_load` `[early, late]` (in pF) on `net`.
    pub fn add_net_wire_load(&mut self, net: NetRow, early: f64, late: f64) {
        self.by_net
            .entry(net)
            .or_default()
            .add_wire_load(early, late);
    }

    /// Adds `set_load` `[early, late]` (in pF) on top-level port bit `port_bit`.
    pub fn add_port_bit_load(&mut self, port_bit: u32, early: f64, late: f64) {
        let entry = self.by_port_bit.entry(port_bit).or_insert([0.0, 0.0]);
        entry[An::Early.index()] += early;
        entry[An::Late.index()] += late;
    }
}

/// Computed loads for every net of a [`FrozenNetlist`] in one corner (`STA-REQ-0110`,
/// `STA-REQ-0121`).
#[derive(Debug, Clone, Default)]
pub struct NetLoads {
    /// Pin capacitance sum (load pins + driver output pins in `NET_PINS` order + `set_load
    /// -pin_load` + port `set_load`) per net `[tr][an]`, in pF.
    pub pin_loads: Vec<[[f64; 2]; 2]>,
    /// Driver load model per net `[tr][an]` after [`WireModel::driver_load`] and `set_load
    /// -wire_load`.
    pub driver_loads: Vec<[[DriverLoad; 2]; 2]>,
    /// Total lumped capacitance `driver_load.total()` per net `[tr][an]`, in pF.
    pub totals: Vec<[[f64; 2]; 2]>,
}

impl NetLoads {
    /// Computes net loads for all nets in `netlist` using cell pin capacitances from `lib`, the
    /// active `wire` model, and `overrides` (`STA-REQ-0110`, `STA-REQ-0121`).
    ///
    /// For each net, connected pins are visited in canonical `NET_PINS` order, summing both load
    /// pin capacitances and every driver's output-pin capacitance.
    #[must_use]
    pub fn compute(
        netlist: &FrozenNetlist<'_>,
        lib: &TimingLib,
        wire: &dyn WireModel,
        overrides: &NetLoadOverrides,
    ) -> Self {
        let cell_pin_map = map_netlist_cell_pins(netlist, lib);
        let n_nets = netlist.nets.len();

        // Accumulate port-bit loads onto their nets in port-bit order.
        let mut port_net_loads = vec![[0.0f64; 2]; n_nets];
        if !overrides.by_port_bit.is_empty() {
            for (pb_idx, &net_id) in netlist.port_bit_net.iter().enumerate() {
                if (net_id as usize) < n_nets
                    && let Some(&pl) = overrides.by_port_bit.get(&(pb_idx as u32))
                {
                    port_net_loads[net_id as usize][An::Early.index()] += pl[An::Early.index()];
                    port_net_loads[net_id as usize][An::Late.index()] += pl[An::Late.index()];
                }
            }
        }

        let mut pin_loads = vec![[[0.0f64; 2]; 2]; n_nets];
        let mut driver_loads = vec![[[DriverLoad::Lumped { c: 0.0 }; 2]; 2]; n_nets];
        let mut totals = vec![[[0.0f64; 2]; 2]; n_nets];

        for net_idx in 0..n_nets {
            let net_row = NetRow(net_idx as u32);
            let mut sum = [[0.0f64; 2]; 2];

            // Sum connected instance pin capacitances in `NET_PINS` order (load pins and driver
            // output pins alike, STA-REQ-0110, STA-REQ-0121).
            for &pin_row in netlist.net_pins_of(net_idx as u32) {
                let inst_idx = netlist.pin_instance(pin_row) as usize;
                let nl_cell = netlist.instances[inst_idx].cell as usize;
                let nl_cp = netlist.pin_cellpin[pin_row as usize] as usize;
                if let Some(cp) = cell_pin_map
                    .get(nl_cell)
                    .and_then(|v| v.get(nl_cp))
                    .and_then(|&opt| opt)
                {
                    for tr in Tr::ALL {
                        for an in An::ALL {
                            sum[tr.index()][an.index()] += cp.cap[tr.index()][an.index()];
                        }
                    }
                }
            }

            // Add port `set_load` and net `set_load -pin_load`.
            let pnl = port_net_loads[net_idx];
            let spec = overrides.by_net.get(&net_row).copied().unwrap_or_default();
            for tr in Tr::ALL {
                for an in An::ALL {
                    sum[tr.index()][an.index()] +=
                        pnl[an.index()] + spec.pin_load[tr.index()][an.index()];
                }
            }
            pin_loads[net_idx] = sum;

            for tr in Tr::ALL {
                for an in An::ALL {
                    let ti = tr.index();
                    let ai = an.index();
                    let wl = spec.wire_load[ti][ai];
                    let dl = add_wire_load_override(wire.driver_load(net_row, an, sum[ti][ai]), wl);
                    driver_loads[net_idx][ti][ai] = dl;
                    totals[net_idx][ti][ai] = dl.total();
                }
            }
        }

        Self {
            pin_loads,
            driver_loads,
            totals,
        }
    }

    /// Returns the [`DriverLoad`] on `net` for transition `tr` and bound `an`.
    #[inline]
    #[must_use]
    pub fn driver_load(&self, net: NetRow, tr: Tr, an: An) -> DriverLoad {
        self.driver_loads[net.index()][tr.index()][an.index()]
    }

    /// Returns the total capacitance (in pF) on `net` for transition `tr` and bound `an`.
    #[inline]
    #[must_use]
    pub fn total_cap(&self, net: NetRow, tr: Tr, an: An) -> f64 {
        self.totals[net.index()][tr.index()][an.index()]
    }

    /// Checks all driver pins in `netlist` against their `max_capacitance` limits and records
    /// aggregated violations in `tracker` (`STA-REQ-0120`).
    pub fn check_max_capacitance(
        &self,
        netlist: &FrozenNetlist<'_>,
        lib: &TimingLib,
        tracker: &mut RangeTracker,
    ) {
        let cell_map: Vec<Option<&TimingCell>> = netlist
            .cells
            .iter()
            .map(|&s| netlist.string(s).ok().and_then(|n| lib.cell(n)))
            .collect();
        let cell_pin_map = map_netlist_cell_pins(netlist, lib);
        let n_nets = netlist.nets.len();

        for (pin_row, &net_id) in netlist.pin_net.iter().enumerate() {
            if (net_id as usize) >= n_nets {
                continue;
            }
            let inst_idx = netlist.pin_instance(pin_row as u32) as usize;
            let nl_cell = netlist.instances[inst_idx].cell as usize;
            let nl_cp = netlist.pin_cellpin[pin_row] as usize;
            let Some(tc) = cell_map.get(nl_cell).copied().flatten() else {
                continue;
            };
            let Some(cp) = cell_pin_map
                .get(nl_cell)
                .and_then(|v| v.get(nl_cp))
                .copied()
                .flatten()
            else {
                continue;
            };
            if !cp.output {
                continue;
            }
            if let Some(limit) = cp.max_capacitance {
                // The construct is named only when the net exceeds the limit.
                let mut construct: Option<String> = None;
                for tr in Tr::ALL {
                    for an in An::ALL {
                        let c = self.totals[net_id as usize][tr.index()][an.index()];
                        if c > limit {
                            let k = construct
                                .get_or_insert_with(|| format!("cell {} pin {}", tc.name, cp.name));
                            tracker.check_max_capacitance(k, c, limit);
                        }
                    }
                }
            }
        }
    }
}

fn add_wire_load_override(dl: DriverLoad, extra_wire_pf: f64) -> DriverLoad {
    if extra_wire_pf == 0.0 {
        return dl;
    }
    match dl {
        DriverLoad::Lumped { c } => DriverLoad::Lumped {
            c: c + extra_wire_pf,
        },
        DriverLoad::Pi { c_near, r, c_far } => DriverLoad::Pi {
            c_near: c_near + extra_wire_pf,
            r,
            c_far,
        },
    }
}

/// Maps each `(netlist_cell_idx, netlist_cell_pin_idx)` to its corresponding `&CellPin` in `lib`.
pub(crate) fn map_netlist_cell_pins<'a>(
    netlist: &FrozenNetlist<'_>,
    lib: &'a TimingLib,
) -> Vec<Vec<Option<&'a CellPin>>> {
    let mut out = Vec::with_capacity(netlist.cells.len());
    for (c_idx, &cell_name_id) in netlist.cells.iter().enumerate() {
        let tc = netlist
            .string(cell_name_id)
            .ok()
            .and_then(|name| lib.cell(name));
        let pin_range = netlist.cell_pins(c_idx as u32);
        let mut pins = Vec::with_capacity(pin_range.len());
        for p_row in pin_range {
            let cp = tc.and_then(|cell| {
                let pname = netlist.string(netlist.cell_pin_name[p_row]).ok()?;
                cell.pin(pname).map(|i| &cell.pins[i])
            });
            pins.push(cp);
        }
        out.push(pins);
    }
    out
}
