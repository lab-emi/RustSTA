// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `max_transition` check of every instance pin after propagation (owner: E5;
//! `STA-REQ-0120`).

use rusteda_compute::timing::TimingValues;

use crate::engine::delay::lut::RangeTracker;
use crate::engine::input::NetlistView;
use crate::engine::input::libview::{TimingCell, TimingLib};
use crate::engine::types::{An, Tr};

pub(super) fn check_max_transitions(
    netview: &NetlistView<'_>,
    lib: &TimingLib,
    node_values: &TimingValues,
    tracker: &mut RangeTracker,
) {
    let frozen = netview.frozen();
    // The `max_transition` of every pin of every netlist cell, looked up once per cell pin.
    let limits: Vec<Vec<Option<(f64, &TimingCell, usize)>>> = (0..frozen.cells.len() as u32)
        .map(|cell_row| {
            let n_cp = frozen.cell_pins(cell_row).len() as u32;
            let tc = lib.cell(netview.cell_name(cell_row));
            (0..n_cp)
                .map(|cp| {
                    let tc = tc?;
                    let i = tc.pin(netview.cell_pin_name(cell_row, cp))?;
                    tc.pins[i].max_transition.map(|limit| (limit, tc, i))
                })
                .collect()
        })
        .collect();
    // Violations are counted per cell pin (`count`, worst transition) and recorded once per
    // construct: one name and one map update per violating cell pin instead of per violation.
    let mut found: Vec<Vec<(usize, f64)>> = limits
        .iter()
        .map(|pins| vec![(0, f64::NEG_INFINITY); pins.len()])
        .collect();
    for inst in 0..frozen.instances.len() {
        let cell_row = frozen.instances[inst].cell as usize;
        for pin_row in frozen.instance_pins(inst as u32) {
            let cp_idx = frozen.pin_cellpin[pin_row] as usize;
            let Some((limit, _, _)) = limits[cell_row].get(cp_idx).copied().flatten() else {
                continue;
            };
            let node = netview.pin_row_node(pin_row as u32);
            for tr in Tr::ALL {
                for an in An::ALL {
                    let s = node_values.transition(node.index(), tr.index(), an.index());
                    if an.is_present(s) && s > limit {
                        let (count, worst) = &mut found[cell_row][cp_idx];
                        *count += 1;
                        if s > *worst {
                            *worst = s;
                        }
                    }
                }
            }
        }
    }
    for (cell_row, pins) in found.iter().enumerate() {
        for (cp_idx, &(count, worst)) in pins.iter().enumerate() {
            if count > 0
                && let Some((limit, tc, lib_pin_idx)) = limits[cell_row][cp_idx]
            {
                let construct = format!("cell {} pin {}", tc.name, tc.pins[lib_pin_idx].name);
                tracker.record_max_transitions(&construct, count, worst, limit);
            }
        }
    }
}
