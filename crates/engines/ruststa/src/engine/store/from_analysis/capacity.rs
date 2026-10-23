// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The capacity check of a timing table before it is built (owner: E10; `STA-REQ-0302`).

use super::StoreInput;
use crate::engine::store::{INST_HASH_MAX_CELL_STR, INST_HASH_MAX_PIN_COUNT};
use crate::engine::types::NodeId;

/// Rejects, with error `T0022`, an analysis whose timing table would overflow a fixed-width field
/// of the table: the `u32` byte offsets and ids of the string table, or the cell-name id and pin
/// count that an `INST_HASH` bucket packs into 20 and 12 bits. The bounds count every string the
/// table builder can add, so a table that passes cannot be truncated (AGENTS rule 9.2,
/// `STA-REQ-0302`).
pub fn check_store_tables(analysis: &StoreInput<'_>, backend_name: &str) -> Result<(), String> {
    let nv = analysis.netview;
    let frozen = nv.frozen();
    let reject = |what: String| Err(format!("T0022: the timing table cannot hold {what}"));

    // Strings interned before the cell names: backend, `**`, `<unclocked>`, corners, clocks and
    // path groups.
    let groups = analysis
        .corners
        .iter()
        .map(|ca| ca.endpoints.path_groups.len());
    let leading_strings = 1
        + 3
        + analysis.corners.len()
        + analysis.constraints.clocks.len()
        + groups.clone().sum::<usize>();
    let cell_strings: usize = (0..nv.num_cells())
        .map(|c| 1 + frozen.cell_pins(c as u32).len())
        .sum();
    if leading_strings + cell_strings > INST_HASH_MAX_CELL_STR as usize {
        return reject(format!(
            "{} cell and pin names: an INST_HASH bucket names at most {INST_HASH_MAX_CELL_STR}",
            leading_strings + cell_strings
        ));
    }
    let max_pins = (0..frozen.instances.len())
        .map(|i| frozen.instance_pins(i as u32).len())
        .max()
        .unwrap_or(0);
    if max_pins > INST_HASH_MAX_PIN_COUNT as usize {
        return reject(format!(
            "an instance with {max_pins} pins: an INST_HASH bucket counts at most \
             {INST_HASH_MAX_PIN_COUNT}"
        ));
    }

    let mut bytes = backend_name.len() + "**".len() + "<unclocked>".len();
    bytes += analysis
        .corners
        .iter()
        .map(|ca| ca.corner_name.len())
        .sum::<usize>();
    bytes += analysis
        .constraints
        .clocks
        .iter()
        .map(|c| c.name.len())
        .sum::<usize>();
    for ca in analysis.corners {
        bytes += ca
            .endpoints
            .path_groups
            .keys()
            .map(String::len)
            .sum::<usize>();
        // The path group of every summarized endpoint, at most once per record.
        bytes += ca
            .endpoints
            .records
            .iter()
            .map(|r| r.path_group.len())
            .sum::<usize>();
    }
    bytes += (0..nv.num_cells())
        .map(|c| {
            let c = c as u32;
            nv.cell_name(c).len()
                + (0..frozen.cell_pins(c).len())
                    .map(|cp| nv.cell_pin_name(c, cp as u32).len())
                    .sum::<usize>()
        })
        .sum::<usize>();
    bytes += (0..nv.num_port_bits())
        .map(|pb| nv.node_name_str(NodeId(pb as u32)).len())
        .sum::<usize>();
    bytes += frozen
        .instances
        .iter()
        .map(|inst| frozen.string(inst.name).map_or(0, str::len))
        .sum::<usize>();
    bytes += analysis
        .diags
        .iter()
        .map(|d| d.code.len() + d.message.len())
        .sum::<usize>();
    let strings = leading_strings
        + cell_strings
        + nv.num_port_bits()
        + frozen.instances.len()
        + analysis
            .corners
            .iter()
            .map(|ca| ca.endpoints.records.len())
            .sum::<usize>()
        + 2 * analysis.diags.len();
    if bytes > u32::MAX as usize || strings >= u32::MAX as usize {
        return reject(format!(
            "{bytes} bytes in {strings} names: its string table addresses at most {} bytes",
            u32::MAX
        ));
    }
    Ok(())
}
