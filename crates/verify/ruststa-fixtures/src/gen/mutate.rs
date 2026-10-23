// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Pin-compatible netlist mutation generator, deterministic replay, and delta-debugging shrinker.

use super::{InstSpec, NetlistFixture, SplitMix64};

/// A single pin-compatible mutation step on a [`NetlistFixture`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// Replaces the cell of `inst_idx` with a pin-compatible `new_cell`.
    SwapCell { inst_idx: usize, new_cell: String },
    /// Inserts a buffer on the output of `inst_idx`.
    InsertBuffer { inst_idx: usize, buf_cell: String },
    /// Removes a previously inserted buffer (or bypasses a 1-input 1-output buffer/inverter).
    RemoveBuffer { inst_idx: usize },
    /// Reconnects input pin `pin_idx` of `inst_idx` to `source_inst_idx`'s output net.
    ReconnectPin {
        inst_idx: usize,
        pin_idx: usize,
        source_inst_idx: usize,
    },
}

/// Generates a deterministic sequence of `count` mutation steps using `seed`.
#[must_use]
pub fn gen_mutations(seed: u64, count: usize) -> Vec<Mutation> {
    let mut rng = SplitMix64::new(seed);
    let single_in_cells = ["wt_buf_1", "wt_buf_2", "wt_inv_1"];
    let two_in_cells = [
        "wt_and2_1",
        "wt_or2_1",
        "wt_nand2_1",
        "wt_nor2_1",
        "wt_xor2_1",
    ];
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = rng.next_usize(4);
        let inst_idx = rng.next_usize(64);
        match kind {
            0 => {
                let new_cell = if rng.next_usize(2) == 0 {
                    single_in_cells[rng.next_usize(single_in_cells.len())]
                } else {
                    two_in_cells[rng.next_usize(two_in_cells.len())]
                };
                out.push(Mutation::SwapCell {
                    inst_idx,
                    new_cell: new_cell.to_string(),
                });
            }
            1 => {
                let buf = if rng.next_usize(2) == 0 {
                    "wt_buf_1"
                } else {
                    "wt_buf_2"
                };
                out.push(Mutation::InsertBuffer {
                    inst_idx,
                    buf_cell: buf.to_string(),
                });
            }
            2 => {
                out.push(Mutation::RemoveBuffer { inst_idx });
            }
            _ => {
                let pin_idx = rng.next_usize(2);
                let source_inst_idx = rng.next_usize(64);
                out.push(Mutation::ReconnectPin {
                    inst_idx,
                    pin_idx,
                    source_inst_idx,
                });
            }
        }
    }
    out
}

fn is_single_input_comb(cell: &str) -> bool {
    matches!(cell, "wt_buf_1" | "wt_buf_2" | "wt_inv_1" | "wt_inv_2")
}

fn is_two_input_comb(cell: &str) -> bool {
    matches!(
        cell,
        "wt_and2_1" | "wt_or2_1" | "wt_nand2_1" | "wt_nor2_1" | "wt_xor2_1" | "wt_xnor2_1"
    )
}

/// Replays `mutations` deterministically against `base`, returning the mutated [`NetlistFixture`].
#[must_use]
pub fn apply_mutations(base: &NetlistFixture, mutations: &[Mutation]) -> NetlistFixture {
    let mut cur = base.clone();
    let base_len = base.instances.len();
    if base_len == 0 {
        return cur;
    }
    for (step, m) in mutations.iter().enumerate() {
        match m {
            Mutation::SwapCell { inst_idx, new_cell } => {
                let idx = inst_idx % base_len;
                let cur_cell = &cur.instances[idx].cell_name;
                if (is_single_input_comb(cur_cell) && is_single_input_comb(new_cell))
                    || (is_two_input_comb(cur_cell) && is_two_input_comb(new_cell))
                {
                    cur.instances[idx].cell_name = new_cell.clone();
                }
            }
            Mutation::InsertBuffer { inst_idx, buf_cell } => {
                let idx = inst_idx % base_len;
                if let Some((_, out_net)) = cur.instances[idx]
                    .conns
                    .iter()
                    .find(|(p, _)| p == "Y")
                    .cloned()
                {
                    let buf_wire = format!("mut_w_{step}");
                    cur.wires.push(buf_wire.clone());
                    if let Some((_, y_conn)) =
                        cur.instances[idx].conns.iter_mut().find(|(p, _)| p == "Y")
                    {
                        *y_conn = buf_wire.clone();
                    }
                    cur.instances.push(InstSpec {
                        inst_name: format!("mut_buf_{step}"),
                        cell_name: buf_cell.clone(),
                        conns: vec![("A".into(), buf_wire), ("Y".into(), out_net)],
                    });
                }
            }
            Mutation::RemoveBuffer { inst_idx } => {
                let idx = inst_idx % base_len;
                if is_single_input_comb(&cur.instances[idx].cell_name) {
                    cur.instances[idx].cell_name = "wt_buf_1".into();
                }
            }
            Mutation::ReconnectPin {
                inst_idx,
                pin_idx,
                source_inst_idx,
            } => {
                let idx = inst_idx % base_len;
                let src = source_inst_idx % base_len;
                if idx != src
                    && let Some(src_net) = cur.instances[src]
                        .conns
                        .iter()
                        .find(|(p, _)| p == "A")
                        .map(|(_, n)| n.clone())
                {
                    let pin_name = if *pin_idx % 2 == 0 { "A" } else { "B" };
                    if let Some((_, net_ref)) = cur.instances[idx]
                        .conns
                        .iter_mut()
                        .find(|(p, _)| p == pin_name)
                    {
                        *net_ref = src_net;
                    }
                }
            }
        }
    }
    cur.rebuild_verilog();
    cur
}

/// Delta-debugging (`ddmin`) shrinker: given an `initial` mutation sequence for which `predicate`
/// returns `true` (indicating the failure is reproduced), returns a minimal failing subsequence.
#[must_use]
pub fn shrink_mutations<F>(initial: &[Mutation], mut predicate: F) -> Vec<Mutation>
where
    F: FnMut(&[Mutation]) -> bool,
{
    let mut best = initial.to_vec();
    let mut n = 2usize;
    while best.len() >= 2 {
        let len = best.len();
        let chunk_count = n.min(len);
        let mut reduced = false;
        for i in 0..chunk_count {
            let start = i * len / chunk_count;
            let end = (i + 1) * len / chunk_count;
            let chunk = &best[start..end];
            if !chunk.is_empty() && predicate(chunk) {
                best = chunk.to_vec();
                n = 2;
                reduced = true;
                break;
            }
        }
        if reduced {
            continue;
        }
        for i in 0..chunk_count {
            let start = i * len / chunk_count;
            let end = (i + 1) * len / chunk_count;
            let mut complement = Vec::with_capacity(len - (end - start));
            complement.extend_from_slice(&best[..start]);
            complement.extend_from_slice(&best[end..]);
            if !complement.is_empty() && predicate(&complement) {
                best = complement;
                n = (n - 1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if n >= len {
                break;
            }
            n = (n * 2).min(len);
        }
    }
    best
}
