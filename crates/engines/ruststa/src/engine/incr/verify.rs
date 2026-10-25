// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Bitwise comparison of a timing session with a full analysis of its netlist (`owner: E11`;
//! `STA-REQ-0455`, `STA-REQ-0602`).
//!
//! The reference is [`EngineAnalysis::analyze`], the analysis of `ruststa run`, never the
//! incremental code: the propagated values of every node and tag, the tags with their exception
//! states, every check record after the timing exceptions and clock groups, the endpoints, path
//! groups and summaries, the diagnostics, the backward state and the bytes of the timing table
//! must all be equal. Structured values are compared through their `Debug` text, which writes every
//! `f64` exactly (so that `-0.0` and `0.0` differ).

use std::collections::BTreeMap;
use std::fmt;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::Library;

use super::cone::propagate_backward_all;
use super::session::{TimingSession, backend_name};
use crate::engine::analysis::{CornerAnalysis, EngineAnalysis, Propagation};
use crate::engine::input::constr::{CompiledConstraints, CornerLibs};
use crate::engine::store::from_analysis::build_store_from_analysis;
use crate::engine::store::layout::{StoreMode, StorePrecision};
use crate::engine::types::NodeId;

/// Bitwise mismatch between incremental and full timing (`STA-REQ-0452` to `STA-REQ-0455`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitwiseMismatch {
    pub corner: usize,
    pub category: &'static str,
    pub node: Option<NodeId>,
    pub node_name: String,
    pub detail: String,
    pub incremental_bits: u64,
    pub full_bits: u64,
}

impl fmt::Display for BitwiseMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "corner {} {} mismatch at '{}': {} (incr={:#018x}, full={:#018x})",
            self.corner,
            self.category,
            self.node_name,
            self.detail,
            self.incremental_bits,
            self.full_bits
        )
    }
}

impl std::error::Error for BitwiseMismatch {}

fn mismatch(corner: usize, category: &'static str, detail: String) -> BitwiseMismatch {
    BitwiseMismatch {
        corner,
        category,
        node: None,
        node_name: String::new(),
        detail,
        incremental_bits: 1,
        full_bits: 0,
    }
}

/// Compares two values through their `Debug` text, which is exact for `f64`.
fn same_debug<T: fmt::Debug>(
    corner: usize,
    category: &'static str,
    inc: &T,
    full: &T,
) -> Result<(), BitwiseMismatch> {
    let (a, b) = (format!("{inc:?}"), format!("{full:?}"));
    if a == b {
        return Ok(());
    }
    let at = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let lo = at.saturating_sub(80);
    Err(mismatch(
        corner,
        category,
        format!(
            "differs at byte {at}: incremental …{}… full …{}…",
            a.get(lo..(at + 80).min(a.len())).unwrap_or(""),
            b.get(lo..(at + 80).min(b.len())).unwrap_or("")
        ),
    ))
}

fn check_f64_bits(
    corner: usize,
    category: &'static str,
    node: Option<NodeId>,
    node_name: &str,
    inc: f64,
    full: f64,
) -> Result<(), BitwiseMismatch> {
    if inc.to_bits() != full.to_bits() {
        return Err(BitwiseMismatch {
            corner,
            category,
            node,
            node_name: node_name.to_owned(),
            detail: format!("{inc} vs {full}"),
            incremental_bits: inc.to_bits(),
            full_bits: full.to_bits(),
        });
    }
    Ok(())
}

/// Compares the propagated values of every node and tag of two corner analyses.
fn compare_values(
    s: &TimingSession,
    c_idx: usize,
    inc: &CornerAnalysis,
    full: &CornerAnalysis,
) -> Result<(), BitwiseMismatch> {
    let (a, b) = (&inc.node_values, &full.node_values);
    if a.ntags != b.ntags || a.arr.len() != b.arr.len() || a.slew.len() != b.slew.len() {
        return Err(mismatch(
            c_idx,
            "values",
            format!(
                "{} tags and {} nodes vs {} tags and {} nodes",
                a.ntags,
                a.slew.len() / 4,
                b.ntags,
                b.slew.len() / 4
            ),
        ));
    }
    let ntags = a.ntags as usize;
    let n_nodes = a.slew.len() / 4;
    for v in 0..n_nodes {
        let node = Some(NodeId(v as u32));
        let name = || s.node_name(NodeId(v as u32));
        for tr_i in 0..2 {
            for an_i in 0..2 {
                let (x, y) = (a.transition(v, tr_i, an_i), b.transition(v, tr_i, an_i));
                if x.to_bits() != y.to_bits() {
                    check_f64_bits(c_idx, "slew", node, &name(), x, y)?;
                }
                for t in 0..ntags {
                    let (x, y) = (a.arrival(v, t, tr_i, an_i), b.arrival(v, t, tr_i, an_i));
                    if x.to_bits() != y.to_bits() {
                        check_f64_bits(c_idx, "arrival", node, &name(), x, y)?;
                    }
                    let (p, q) = (
                        a.predecessor(v, t, tr_i, an_i),
                        b.predecessor(v, t, tr_i, an_i),
                    );
                    if p != q {
                        return Err(BitwiseMismatch {
                            corner: c_idx,
                            category: "pred",
                            node,
                            node_name: name(),
                            detail: format!("tag={t} tr={tr_i} an={an_i}: {p:#x} vs {q:#x}"),
                            incremental_bits: u64::from(p),
                            full_bits: u64::from(q),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Compares everything else the analysis of a corner computes from its values.
fn compare_corner(
    c_idx: usize,
    inc: &CornerAnalysis,
    full: &CornerAnalysis,
) -> Result<(), BitwiseMismatch> {
    same_debug(c_idx, "tags", &inc.tags, &full.tags)?;
    same_debug(
        c_idx,
        "check_records",
        &inc.endpoints.records,
        &full.endpoints.records,
    )?;
    same_debug(
        c_idx,
        "endpoints",
        &inc.endpoints.endpoints,
        &full.endpoints.endpoints,
    )?;
    same_debug(
        c_idx,
        "path_groups",
        &inc.endpoints.path_groups,
        &full.endpoints.path_groups,
    )?;
    same_debug(
        c_idx,
        "unconstrained",
        &(
            inc.endpoints.unconstrained,
            inc.endpoints.legacy_unconstrained,
        ),
        &(
            full.endpoints.unconstrained,
            full.endpoints.legacy_unconstrained,
        ),
    )?;
    same_debug(
        c_idx,
        "wns",
        &inc.endpoints.summary,
        &full.endpoints.summary,
    )?;
    let sorted = |c: &CornerAnalysis| {
        c.launch_slews
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect::<BTreeMap<_, _>>()
    };
    same_debug(c_idx, "launch_slews", &sorted(inc), &sorted(full))?;
    same_debug(c_idx, "latch_maps", &inc.latch_maps, &full.latch_maps)?;
    same_debug(c_idx, "gating_sites", &inc.gating_sites, &full.gating_sites)?;
    same_debug(c_idx, "net_loads", &inc.net_loads, &full.net_loads)?;
    // The pin limit checks of the corner (`STA-REQ-0806`).
    same_debug(c_idx, "limits", &inc.limits, &full.limits)?;
    same_debug(
        c_idx,
        "diags",
        &(inc.unanalyzed_arcs, &inc.diags),
        &(full.unanalyzed_arcs, &full.diags),
    )?;
    same_debug(
        c_idx,
        "exceptions",
        &(inc.exceptions.is_active(), inc.exceptions.unused()),
        &(full.exceptions.is_active(), full.exceptions.unused()),
    )
}

impl TimingSession {
    /// Runs a fresh full analysis ([`EngineAnalysis::analyze`]) of `self.netlist().freeze()` and
    /// verifies that the session's state is bitwise identical to it (`STA-REQ-0455`,
    /// `STA-REQ-0602`): the values and predecessors of every node and tag, the tags with their
    /// exception states, the check records after the timing exceptions and clock groups, the
    /// endpoints, path groups, summaries and diagnostics, the backward state when the session
    /// keeps it, and the bytes of the timing table `sta.rbin`.
    pub fn verify_against_full(&self) -> Result<(), BitwiseMismatch> {
        let frozen = self.netlist.freeze();
        let corner_refs: Vec<CornerLibs<'_>> = self
            .corner_libs
            .iter()
            .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
            .collect();
        let failed = |what: &str, e: String| mismatch(0, "full_analysis", format!("{what}: {e}"));
        let compiled = CompiledConstraints::compile(&self.sdc, &frozen, &corner_refs)
            .map_err(|e| failed("constraints", format!("{:?}", e.diags)))?;
        let (full, record) = EngineAnalysis::analyze_with(
            &frozen,
            &corner_refs,
            None,
            compiled,
            &self.options.analysis,
            Ctx::NONE,
            Propagation::Full,
            true,
        )
        .map_err(|d| failed("analysis", format!("{d:?}")))?;

        let state = &self.state;
        if state.graph.num_nodes() != full.graph.num_nodes()
            || state.graph.order() != full.graph.order()
        {
            return Err(mismatch(
                0,
                "graph",
                format!(
                    "{} nodes vs {} nodes, or another topological order",
                    state.graph.num_nodes(),
                    full.graph.num_nodes()
                ),
            ));
        }
        if state.corners.len() != full.corners.len() {
            return Err(mismatch(0, "corners", "corner count".into()));
        }
        for (c_idx, (inc_c, full_c)) in state.corners.iter().zip(&full.corners).enumerate() {
            compare_values(self, c_idx, inc_c, full_c)?;
            compare_corner(c_idx, inc_c, full_c)?;
            if let Some(inc_b) = state.backward.get(c_idx) {
                let full_b = propagate_backward_all(
                    &full.graph,
                    &record.lowered[c_idx],
                    &full_c.propagated,
                    &full_c.node_values,
                    &full_c.endpoints.records,
                );
                same_debug(c_idx, "backward", inc_b, &full_b)?;
            }
        }
        same_debug(0, "sta_summary", &state.sta_summary, &full.sta_summary)?;
        same_debug(0, "scope", &state.scope, &full.scope)?;
        same_debug(0, "completeness", &state.completeness, &full.completeness)?;
        same_debug(0, "sta_status", &state.sta_status, &full.sta_status)?;
        same_debug(0, "diags", &state.diags, &full.diags)?;

        let libs: &[(String, Vec<Library>)] = &self.corner_libs;
        let backend = backend_name(&self.options.analysis);
        let inc_store = self
            .sta_store(StoreMode::Full, StorePrecision::F64, 100)
            .map_err(|e| failed("timing table", e.to_string()))?;
        let full_store = build_store_from_analysis(
            &full,
            libs,
            backend,
            StorePrecision::F64,
            StoreMode::Full,
            100,
        );
        if inc_store != full_store {
            return Err(mismatch(
                0,
                "sta_store",
                "StaStoreData tables differ from the table of the full analysis".into(),
            ));
        }
        let (inc_bytes, full_bytes) = (inc_store.to_bytes(), full_store.to_bytes());
        if inc_bytes.ok() != full_bytes.ok() {
            return Err(mismatch(0, "sta_store", "sta.rbin bytes differ".into()));
        }
        Ok(())
    }
}
