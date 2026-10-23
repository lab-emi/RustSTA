// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Incremental timing (`owner: E11`; `STA-REQ-0450` to `STA-REQ-0458`, `STA-REQ-0600` to
//! `STA-REQ-0606`).
//!
//! An incremental analysis is the full analysis of the analysis module
//! ([`EngineAnalysis::analyze_with`](crate::engine::analysis::EngineAnalysis::analyze_with)) whose
//! forward propagation starts from the values of an earlier analysis ([`PriorAnalysis`]) and
//! evaluates only the cone of the nodes whose inputs changed ([`cone`]). Everything else, from the
//! timing graph to the exceptions, the checks and the endpoints, is computed by the code of the
//! full analysis, so that every value, the timing exceptions and clock groups included, is
//! bitwise identical to a full analysis of the same netlist. Three surfaces use it:
//!
//! - [`session::TimingSession`]: live timing over a mutable [`GateNetlist`] with low-level edits
//!   ([`SessionEdit`]) and the named edits of the suite's edit files ([`named`]).
//! - [`update::update_revision`] and the helpers of `ruststa update`: a revision analyzed from the
//!   timing table of an ancestor revision ([`update::AncestorTable`]).
//! - `ruststa whatif`: candidate edits evaluated on copies of one session.

pub mod cone;
pub mod diff;
pub mod edit;
pub mod named;
pub mod session;
pub mod update;
mod verify;

use std::borrow::Cow;

use rusteda_compute::timing::{CompiledLutBank, TimingValues};
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::{Conn, GateNetlist, InstId, NetId, PinDir};

pub use cone::{
    CornerBackwardState, IncrementalStats, NodeMapping, SessionNodeKey, propagate_backward_all,
    propagate_backward_cone, propagate_forward_cone, session_node_keys,
};
pub use diff::{NetlistDiff, NetlistDiffSummary, diff_frozen_netlists};
pub use edit::{SessionEdit, SessionError};
pub use session::{SessionOptions, TimingSession};
pub use update::{
    AncestorTable, DEFAULT_MAX_DIRTY_FRACTION, FullUpdateReason, UpdateError, UpdateMode,
    UpdateOptions, UpdateResult, analyze_from_ancestor, reconstruct_timing_values_from_store,
    select_ancestor, update_revision,
};
pub use verify::BitwiseMismatch;

use crate::engine::graph::TimingGraph;
use crate::engine::input::libview::TimingLib;
use crate::engine::prop::lower::LoweredCorner;

/// The state of an earlier analysis that an incremental analysis starts from (`STA-REQ-0601`):
/// its timing graph and, per corner, its lowered problem and its propagated values in node order.
#[derive(Debug, Clone)]
pub struct PriorAnalysis<'p> {
    /// The timing graph of the earlier analysis.
    pub graph: &'p TimingGraph,
    /// The corners of the earlier analysis, in corner order.
    pub corners: Vec<PriorCorner<'p>>,
}

/// One corner of a [`PriorAnalysis`].
#[derive(Debug, Clone, Copy)]
pub struct PriorCorner<'p> {
    /// The lowered problem the values were propagated on.
    pub lowered: &'p LoweredCorner<'p>,
    /// The propagated values, in [`NodeId`](crate::engine::types::NodeId) order.
    pub node_values: &'p TimingValues,
}

/// Converts a [`LoweredCorner`] with borrowed LUT slices into an owned `'static` [`LoweredCorner`].
#[must_use]
pub fn into_owned_lowered(lowered: LoweredCorner<'_>) -> LoweredCorner<'static> {
    LoweredCorner {
        csr: lowered.csr,
        csr_arc_to_graph_arc: lowered.csr_arc_to_graph_arc,
        graph_arc_to_csr_arc: lowered.graph_arc_to_csr_arc,
        luts: CompiledLutBank {
            classes: Cow::Owned(lowered.luts.classes.into_owned()),
            luts: Cow::Owned(lowered.luts.luts.into_owned()),
            data: Cow::Owned(lowered.luts.data.into_owned()),
        },
        ntags: lowered.ntags,
        load: lowered.load,
        starts: lowered.starts,
        wires: lowered.wires,
        lower_s: lowered.lower_s,
    }
}

/// Zeller's `ddmin` delta-debugging algorithm over a sequence of [`SessionEdit`]s (`STA-REQ-0458`).
///
/// Given a sequence `edits` and a predicate `fails` that returns `true` when applying a candidate
/// subsequence triggers a failure (such as a [`BitwiseMismatch`]), iteratively shrinks `edits` to a
/// 1-minimal failing subsequence.
pub fn shrink_failing_edits<F>(edits: &[SessionEdit], mut fails: F) -> Vec<SessionEdit>
where
    F: FnMut(&[SessionEdit]) -> bool,
{
    let mut current: Vec<SessionEdit> = edits.to_vec();
    if current.len() <= 1 {
        return current;
    }
    let mut n = 2usize;
    while current.len() >= 2 {
        let len = current.len();
        let chunks = n;
        let mut reduced = false;

        // 1. Try reducing to a single chunk of size ~len/chunks.
        if chunks <= len {
            for i in 0..chunks {
                let start = (i * len) / chunks;
                let end = ((i + 1) * len) / chunks;
                let chunk = &current[start..end];
                if !chunk.is_empty() && chunk.len() < len && fails(chunk) {
                    current = chunk.to_vec();
                    n = 2;
                    reduced = true;
                    break;
                }
            }
        }
        if reduced {
            continue;
        }

        // 2. Try removing each chunk (testing its complement).
        for i in 0..chunks {
            let start = (i * len) / chunks;
            let end = ((i + 1) * len) / chunks;
            if start == end {
                continue;
            }
            let mut complement = Vec::with_capacity(len - (end - start));
            complement.extend_from_slice(&current[..start]);
            complement.extend_from_slice(&current[end..]);
            if !complement.is_empty() && fails(&complement) {
                current = complement;
                n = (chunks - 1).max(2);
                reduced = true;
                break;
            }
        }
        if reduced {
            continue;
        }

        if chunks >= len {
            break;
        }
        n = (chunks * 2).min(len);
    }
    current
}

pub(crate) fn to_pin_dir(dir: Direction) -> PinDir {
    match dir {
        Direction::Input => PinDir::Input,
        Direction::Output => PinDir::Output,
        Direction::Inout => PinDir::Inout,
        Direction::Internal => PinDir::Other,
    }
}

/// The pins of cell `name` as `rusteda import --netlist` adds a cell to a netlist, and as edits
/// add one (`WS-REQ-0151`): the pins of the library cell in library order, internal pins left
/// out, merged over the libraries of every corner in their order; `None` when no library has
/// the cell.
pub fn revision_cell_pins(
    corners: &[(String, Vec<Library>)],
    name: &str,
) -> Option<Vec<(String, PinDir)>> {
    let mut out: Option<Vec<(String, PinDir)>> = None;
    for (_, libs) in corners {
        for lib in libs {
            let Some(cell) = lib.cells.iter().find(|c| c.name == name) else {
                continue;
            };
            let pins = out.get_or_insert_with(Vec::new);
            for p in &cell.pins {
                if p.direction == Direction::Internal || pins.iter().any(|(n, _)| *n == p.name) {
                    continue;
                }
                pins.push((p.name.clone(), to_pin_dir(p.direction)));
            }
        }
    }
    out
}

pub(crate) fn intern_all_lib_cells(netlist: &mut GateNetlist, lib: &TimingLib) {
    for tc in &lib.cells {
        if netlist.find_cell(&tc.name).is_none() {
            let pins: Vec<(&str, PinDir)> = tc
                .pins
                .iter()
                .map(|p| (p.name.as_str(), to_pin_dir(p.direction)))
                .collect();
            let _ = netlist.add_cell_type(&tc.name, &pins);
        }
    }
}

pub(crate) fn collect_instance_nets(netlist: &GateNetlist, inst_id: InstId) -> Vec<NetId> {
    let mut nets = Vec::new();
    if let Ok(inst) = netlist.instance(inst_id) {
        for &pin_id in &inst.connected {
            if let Ok(pin) = netlist.pin(pin_id)
                && let Some(Conn::Net(net_id)) = pin.conn
            {
                nets.push(net_id);
            }
        }
    }
    nets
}

/// The first library of `libs` that has cell `name`, as the corner's library view looks it up.
pub(crate) fn find_lib_cell<'l>(
    libs: &'l [Library],
    name: &str,
) -> Option<&'l rusteda_core::liberty::Cell> {
    libs.iter()
        .find_map(|l| l.cells.iter().find(|c| c.name == name))
}
