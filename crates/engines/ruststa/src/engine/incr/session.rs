// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Live incremental timing session (`TimingSession`) over a mutable [`GateNetlist`] (`owner: E11`;
//! `STA-REQ-0450` to `STA-REQ-0455`, `STA-REQ-0600` to `STA-REQ-0603`).
//!
//! A session holds an owned [`GateNetlist`], the libraries of every corner, the parsed SDC and the
//! analysis of the netlist ([`SessionState`]): the timing graph, the compiled constraints, the
//! [`CornerAnalysis`] of every corner (tags with exception states, propagated values, checks and
//! endpoints after the timing exceptions and clock groups), the lowered problem of every corner
//! and, on request, the backward state. After an edit, the analysis is recomputed by the code of
//! the full analysis ([`EngineAnalysis::analyze_with`]) with a cone propagation that starts from
//! the previous values, so that every value is bitwise identical to a full analysis of the edited
//! netlist (`STA-REQ-0601`).

use rustc_hash::FxHashMap;
use rusteda_core::diag::Severity;
use rusteda_core::job::{Ctx, Diag};
use rusteda_core::liberty::Library;
use rusteda_core::netlist::{CellId, GateNetlist, PinDir};
use rusteda_formats::sdc::{Env as SdcEnv, Limits as SdcLimits, Sdc, Source as SdcSource};

use super::cone::{
    CornerBackwardState, IncrementalStats, NodeMapping, SessionNodeKey, propagate_backward_all,
    propagate_backward_cone, session_node_keys,
};
use super::edit::{DirtyTracker, SessionError};
use super::{PriorAnalysis, PriorCorner, intern_all_lib_cells, revision_cell_pins};
use crate::engine::analysis::{
    AnalysisOptions, CornerAnalysis, EngineAnalysis, Propagation, PropagationRecord,
    compile_corner_libs,
};
use crate::engine::graph::TimingGraph;
use crate::engine::input::NetlistView;
use crate::engine::input::constr::{CompiledConstraints, CornerLibs};
use crate::engine::input::libview::TimingLib;
use crate::engine::prop::lower::LoweredCorner;
use crate::engine::scope::{AnalysisScope, ScopeCompleteness, StaStatusValue, StaSummary};
use crate::engine::store::from_analysis::{StoreInput, build_store};
use crate::engine::store::layout::{StoreMode, StorePrecision};
use crate::engine::store::writer::StaStoreData;
use crate::engine::types::{An, NodeId, Tr};

/// Options of a [`TimingSession`] (`STA-REQ-0450`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionOptions {
    /// Options of every analysis of the session: backend, precision, wire model and budgets.
    pub analysis: AnalysisOptions,
    /// Adds every cell of the first corner's libraries to the netlist when the session is made,
    /// so that edits by id can name any library cell. Without it, a cell is added when an edit
    /// first names it, as the shared edits add it (`WS-REQ-0151`), and the netlist stays the one
    /// a tool that applies the same edits commits.
    pub intern_library_cells: bool,
    /// Keeps the per-tag required times and per-pin slacks of every corner up to date after each
    /// edit (`STA-REQ-0454`).
    pub backward: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            analysis: AnalysisOptions::default(),
            intern_library_cells: true,
            backward: true,
        }
    }
}

/// The analysis of the netlist of a [`TimingSession`], without its netlist view
/// (`STA-REQ-0450`): the fields of an [`EngineAnalysis`], the lowered problem of every corner and
/// the backward state.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// The timing graph.
    pub graph: TimingGraph,
    /// The constraints compiled against the current netlist.
    pub constraints: CompiledConstraints,
    /// The analysis of every corner, in corner order.
    pub corners: Vec<CornerAnalysis>,
    /// The lowered problem of every corner, the start of the next incremental propagation.
    pub lowered: Vec<LoweredCorner<'static>>,
    /// The backward state of every corner; empty when the session does not keep it.
    pub backward: Vec<CornerBackwardState>,
    /// The scope of the analysis.
    pub scope: AnalysisScope,
    /// The completeness of the analysis.
    pub completeness: ScopeCompleteness,
    /// The summary over the corners.
    pub sta_summary: StaSummary,
    /// The status of the analysis.
    pub sta_status: StaStatusValue,
    /// The warnings and notes of the analysis.
    pub diags: Vec<Diag>,
}

/// Live incremental timing session over a mutable [`GateNetlist`] (`STA-REQ-0450` to
/// `STA-REQ-0455`).
#[derive(Debug, Clone)]
pub struct TimingSession {
    pub(super) netlist: GateNetlist,
    pub(super) corner_libs: Vec<(String, Vec<Library>)>,
    pub(super) sdc: Sdc,
    pub(super) options: SessionOptions,
    pub(super) node_keys: Vec<SessionNodeKey>,
    pub(super) key_index: FxHashMap<SessionNodeKey, NodeId>,
    pub(super) state: SessionState,
    pub(super) last_stats: IncrementalStats,
}

impl TimingSession {
    /// Creates a single-corner (`"default"`) session from a [`GateNetlist`], Liberty libraries and
    /// SDC text (`STA-REQ-0450`).
    pub fn new(
        netlist: GateNetlist,
        libs: Vec<Library>,
        sdc_text: &str,
        options: AnalysisOptions,
    ) -> Result<Self, SessionError> {
        Self::new_multi_corner_sdc_str(
            netlist,
            vec![(String::from("default"), libs)],
            sdc_text,
            options,
        )
    }

    /// Creates a single-corner (`"default"`) session from a [`GateNetlist`], Liberty libraries
    /// and a parsed [`Sdc`] (`STA-REQ-0450`).
    pub fn from_sdc(
        netlist: GateNetlist,
        libs: Vec<Library>,
        sdc: Sdc,
        options: AnalysisOptions,
    ) -> Result<Self, SessionError> {
        Self::new_multi_corner(netlist, vec![(String::from("default"), libs)], sdc, options)
    }

    /// Creates a multi-corner session from SDC text (`STA-REQ-0450`).
    pub fn new_multi_corner_sdc_str(
        netlist: GateNetlist,
        corners: Vec<(String, Vec<Library>)>,
        sdc_text: &str,
        options: AnalysisOptions,
    ) -> Result<Self, SessionError> {
        let sources = [SdcSource::new("<session>", sdc_text)];
        let (sdc, diags) =
            rusteda_formats::sdc::read(&sources, &SdcEnv::default(), &SdcLimits::default());
        let errs: Vec<Diag> = diags
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if !errs.is_empty() {
            return Err(SessionError::Diag(errs));
        }
        Self::new_multi_corner(netlist, corners, sdc, options)
    }

    /// Creates a multi-corner session from a parsed [`Sdc`] with every library cell added to the
    /// netlist and the backward state kept (`STA-REQ-0450`).
    pub fn new_multi_corner(
        netlist: GateNetlist,
        corners: Vec<(String, Vec<Library>)>,
        sdc: Sdc,
        options: AnalysisOptions,
    ) -> Result<Self, SessionError> {
        Self::with_options(
            netlist,
            corners,
            sdc,
            SessionOptions {
                analysis: options,
                ..SessionOptions::default()
            },
        )
    }

    /// Creates a session with explicit [`SessionOptions`] and runs the full analysis of the
    /// netlist (`STA-REQ-0450`).
    pub fn with_options(
        mut netlist: GateNetlist,
        corners: Vec<(String, Vec<Library>)>,
        sdc: Sdc,
        options: SessionOptions,
    ) -> Result<Self, SessionError> {
        if corners.is_empty() || corners.iter().any(|(_, l)| l.is_empty()) {
            return Err(SessionError::Diag(vec![Diag::error(
                "T0005",
                "at least one corner with a Liberty library is required",
            )]));
        }
        let corner_refs: Vec<CornerLibs<'_>> = corners
            .iter()
            .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
            .collect();
        let timing_libs =
            compile_corner_libs(&corner_refs).map_err(|d| SessionError::Diag(vec![d]))?;
        if options.intern_library_cells {
            intern_all_lib_cells(&mut netlist, &timing_libs[0]);
        }
        let frozen = netlist.freeze();
        let compiled = CompiledConstraints::compile(&sdc, &frozen, &corner_refs)
            .map_err(|e| SessionError::Diag(e.diags))?;
        let (analysis, record) = EngineAnalysis::analyze_with(
            &frozen,
            &corner_refs,
            Some(timing_libs),
            compiled,
            &options.analysis,
            Ctx::NONE,
            Propagation::Full,
            true,
        )
        .map_err(SessionError::Diag)?;
        let backward = if options.backward {
            full_backward(&analysis, &record)
        } else {
            Vec::new()
        };
        let n_nodes = analysis.graph.num_nodes();
        let n_corners = analysis.corners.len();
        let state = into_state(analysis, record.lowered, backward);
        drop(corner_refs);
        let node_keys = session_node_keys(&netlist);
        let key_index = key_index(&node_keys);
        Ok(Self {
            netlist,
            corner_libs: corners,
            sdc,
            options,
            node_keys,
            key_index,
            state,
            last_stats: IncrementalStats {
                total_nodes: n_nodes,
                dirty_instances: 0,
                dirty_nets: 0,
                forward_seeds: n_nodes,
                forward_nodes_evaluated: n_nodes * n_corners,
                forward_nodes_changed: n_nodes,
                backward_seeds: n_nodes,
                backward_nodes_evaluated: n_nodes * n_corners,
                backward_nodes_changed: n_nodes,
            },
        })
    }

    /// The netlist.
    #[inline]
    #[must_use]
    pub fn netlist(&self) -> &GateNetlist {
        &self.netlist
    }

    /// The libraries of every corner.
    #[inline]
    #[must_use]
    pub fn corner_libs(&self) -> &[(String, Vec<Library>)] {
        &self.corner_libs
    }

    /// The options of the session.
    #[inline]
    #[must_use]
    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    /// The analysis of the current netlist.
    #[inline]
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The timing graph of the current netlist.
    #[inline]
    #[must_use]
    pub fn graph(&self) -> &TimingGraph {
        &self.state.graph
    }

    /// The analysis of every corner.
    #[inline]
    #[must_use]
    pub fn corners(&self) -> &[CornerAnalysis] {
        &self.state.corners
    }

    /// The analysis of the first corner.
    #[inline]
    #[must_use]
    pub fn default_corner(&self) -> &CornerAnalysis {
        &self.state.corners[0]
    }

    /// The summary over the corners.
    #[inline]
    #[must_use]
    pub fn sta_summary(&self) -> &StaSummary {
        &self.state.sta_summary
    }

    /// The statistics of the last update.
    #[inline]
    #[must_use]
    pub fn last_stats(&self) -> IncrementalStats {
        self.last_stats
    }

    /// The canonical node keys of the current netlist, in [`NodeId`] order (`STA-REQ-0452`).
    #[inline]
    #[must_use]
    pub fn node_keys(&self) -> &[SessionNodeKey] {
        &self.node_keys
    }

    /// The node of `key` in the current netlist.
    #[inline]
    #[must_use]
    pub fn node_of_key(&self, key: SessionNodeKey) -> Option<NodeId> {
        self.key_index.get(&key).copied()
    }

    /// The key of the node named `name`: a port bit (`name` or `name[index]`) or a connected pin
    /// `inst/pin` (`STA-REQ-0452`).
    #[must_use]
    pub fn key_of_name(&self, name: &str) -> Option<SessionNodeKey> {
        let mut pb = 0u32;
        for port in self.netlist.ports() {
            let pname = self.netlist.symbols().resolve(port.name);
            for (k, &net) in port.bits.iter().enumerate() {
                let bit_name = match self.netlist.net(net).ok().and_then(|n| n.index) {
                    Some(i) => format!("{pname}[{i}]"),
                    None => pname.to_string(),
                };
                if bit_name == name {
                    return Some(SessionNodeKey::PortBit(pb + k as u32));
                }
            }
            pb += port.bits.len() as u32;
        }
        let (inst, pin) = split_pin_name(name)?;
        let inst = self.netlist.find_instance(inst)?;
        let pin = self.netlist.find_pin(inst, pin).ok()?;
        self.netlist.pin(pin).ok()?.conn?;
        Some(SessionNodeKey::Pin(pin))
    }

    /// The node named `name`: a port bit or a connected pin `inst/pin`.
    #[must_use]
    pub fn find_node(&self, name: &str) -> Option<NodeId> {
        self.node_of_key(self.key_of_name(name)?)
    }

    /// The name of `node`: `port`, `port[index]` or `inst/pin`.
    #[must_use]
    pub fn node_name(&self, node: NodeId) -> String {
        let Some(&key) = self.node_keys.get(node.index()) else {
            return String::new();
        };
        match key {
            SessionNodeKey::PortBit(pb) => {
                let mut left = pb as usize;
                for port in self.netlist.ports() {
                    if left < port.bits.len() {
                        let pname = self.netlist.symbols().resolve(port.name);
                        return match self.netlist.net(port.bits[left]).ok().and_then(|n| n.index) {
                            Some(i) => format!("{pname}[{i}]"),
                            None => pname.to_string(),
                        };
                    }
                    left -= port.bits.len();
                }
                String::new()
            }
            SessionNodeKey::Pin(pin) => {
                let Ok(p) = self.netlist.pin(pin) else {
                    return String::new();
                };
                let Ok(inst) = self.netlist.instance(p.inst) else {
                    return String::new();
                };
                let Ok(cell) = self.netlist.cell(inst.cell) else {
                    return String::new();
                };
                let sym = self.netlist.symbols();
                format!(
                    "{}/{}",
                    sym.resolve(inst.name),
                    sym.resolve(cell.pins[p.cell_pin as usize].name)
                )
            }
        }
    }

    /// The backward state of corner `corner`, when the session keeps it.
    #[must_use]
    pub fn backward(&self, corner: usize) -> Option<&CornerBackwardState> {
        self.state.backward.get(corner)
    }

    /// The worst slack of `node` in the first corner (`NaN` without the backward state).
    #[must_use]
    pub fn pin_slack(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.backward(0).map_or(f64::NAN, |b| {
            b.pin_slack[node.index()][tr.index()][an.index()]
        })
    }

    /// The required time of `node` in the first corner (`NaN` without the backward state).
    #[must_use]
    pub fn pin_required(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.backward(0).map_or(f64::NAN, |b| {
            b.pin_required[node.index()][tr.index()][an.index()]
        })
    }

    /// The transition time of `node` in the first corner.
    #[must_use]
    pub fn pin_slew(&self, node: NodeId, tr: Tr, an: An) -> f64 {
        self.state.corners[0]
            .node_values
            .transition(node.index(), tr.index(), an.index())
    }

    /// The pins of cell `name` as a netlist adds it (`WS-REQ-0151`).
    #[must_use]
    pub fn cell_pins(&self, name: &str) -> Option<Vec<(String, PinDir)>> {
        revision_cell_pins(&self.corner_libs, name)
    }

    /// Ensures `cell_name` is a cell type of the netlist, adding it with the pins of its library
    /// cell if necessary, and returns its [`CellId`].
    pub fn ensure_cell(&mut self, cell_name: &str) -> Result<CellId, SessionError> {
        if let Some(cid) = self.netlist.find_cell(cell_name) {
            return Ok(cid);
        }
        let Some(pins) = self.cell_pins(cell_name) else {
            return Err(SessionError::UnknownCell(cell_name.to_owned()));
        };
        Ok(self.netlist.add_cell_type(cell_name, &pins)?)
    }

    /// The timing table `sta.rbin` of the current netlist, as `ruststa run` would commit it for
    /// it with `store_mode`, `precision` and `top_k_paths` (`STA-REQ-0602`).
    pub fn sta_store(
        &self,
        store_mode: StoreMode,
        precision: StorePrecision,
        top_k_paths: usize,
    ) -> Result<StaStoreData, SessionError> {
        let frozen = self.netlist.freeze();
        let slices: Vec<&[Library]> = self.corner_libs.iter().map(|(_, l)| l.as_slice()).collect();
        let netview = NetlistView::with_corners(&frozen, &slices)
            .map_err(|e| SessionError::Diag(vec![Diag::error(e.code(), e.to_string())]))?;
        let input = StoreInput {
            netview: &netview,
            graph: &self.state.graph,
            constraints: &self.state.constraints,
            corners: &self.state.corners,
            diags: &self.state.diags,
        };
        Ok(build_store(
            &input,
            &self.corner_libs,
            backend_name(&self.options.analysis),
            precision,
            store_mode,
            top_k_paths,
        ))
    }

    /// Recomputes the analysis after edits that changed the objects of `dirty`: a cone
    /// propagation from the previous values, the rest as in the full analysis (`STA-REQ-0601`).
    /// On an error the previous analysis stays.
    pub(super) fn recompute(
        &mut self,
        dirty: &DirtyTracker,
    ) -> Result<IncrementalStats, SessionError> {
        let new_keys = session_node_keys(&self.netlist);
        let mapping = NodeMapping::from_key_index(
            &self.key_index,
            self.node_keys.len(),
            &new_keys,
            &dirty.keys,
        );
        let frozen = self.netlist.freeze();
        let corner_refs: Vec<CornerLibs<'_>> = self
            .corner_libs
            .iter()
            .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
            .collect();
        let compiled = CompiledConstraints::compile(&self.sdc, &frozen, &corner_refs)
            .map_err(|e| SessionError::Diag(e.diags))?;
        let timing_libs: Vec<TimingLib> =
            self.state.corners.iter().map(|c| c.lib.clone()).collect();
        let prior = PriorAnalysis {
            graph: &self.state.graph,
            corners: self
                .state
                .corners
                .iter()
                .zip(&self.state.lowered)
                .map(|(c, l)| PriorCorner {
                    lowered: l,
                    node_values: &c.node_values,
                })
                .collect(),
        };
        let (analysis, record) = EngineAnalysis::analyze_with(
            &frozen,
            &corner_refs,
            Some(timing_libs),
            compiled,
            &self.options.analysis,
            Ctx::NONE,
            Propagation::Cone {
                prior: &prior,
                mapping: &mapping,
            },
            true,
        )
        .map_err(SessionError::Diag)?;

        let n_new = analysis.graph.num_nodes();
        let mut stats = IncrementalStats {
            total_nodes: n_new,
            dirty_instances: dirty.insts.len(),
            dirty_nets: dirty.nets.len(),
            ..IncrementalStats::default()
        };
        for (i, ch) in record.changed.iter().enumerate() {
            stats.forward_seeds = stats.forward_seeds.max(record.seeds[i]);
            stats.forward_nodes_evaluated += record.evaluated[i] as usize;
            stats.forward_nodes_changed = stats
                .forward_nodes_changed
                .max(ch.as_ref().map_or(n_new, Vec::len));
        }
        let mut backward = Vec::new();
        if self.options.backward {
            for (i, ca) in analysis.corners.iter().enumerate() {
                let all: Vec<u32>;
                let changed = match &record.changed[i] {
                    Some(c) => c.as_slice(),
                    None => {
                        all = (0..n_new as u32).collect();
                        &all
                    }
                };
                let (state, seeds, evaluated, changed_n) = propagate_backward_cone(
                    Some(&self.state.graph),
                    Some(&self.state.lowered[i]),
                    &self.state.corners[i].node_values,
                    &self.state.backward[i],
                    &analysis.graph,
                    &record.lowered[i],
                    &ca.propagated,
                    &ca.node_values,
                    changed,
                    &ca.endpoints.records,
                    &mapping,
                );
                stats.backward_seeds = stats.backward_seeds.max(seeds);
                stats.backward_nodes_evaluated += evaluated;
                stats.backward_nodes_changed = stats.backward_nodes_changed.max(changed_n);
                backward.push(state);
            }
        }
        drop(prior);
        self.state = into_state(analysis, record.lowered, backward);
        self.key_index = key_index(&new_keys);
        self.node_keys = new_keys;
        self.last_stats = stats;
        Ok(stats)
    }
}

/// The name of the backend whose values the analyses of `options` give, as a timing table
/// records it.
pub(crate) fn backend_name(options: &AnalysisOptions) -> &'static str {
    match options.backend {
        crate::engine::prop::lower::BackendKind::Cpu => "cpu",
        _ => "cuda",
    }
}

/// `inst/pin` split at its last unescaped `/`.
pub(crate) fn split_pin_name(name: &str) -> Option<(&str, &str)> {
    let b = name.as_bytes();
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        if b[i] == b'/' {
            // A slash after an odd number of backslashes is escaped.
            let backslashes = b[..i].iter().rev().take_while(|&&c| c == b'\\').count();
            if backslashes % 2 == 0 {
                let (inst, pin) = (&name[..i], &name[i + 1..]);
                return (!inst.is_empty() && !pin.is_empty()).then_some((inst, pin));
            }
        }
    }
    None
}

fn key_index(keys: &[SessionNodeKey]) -> FxHashMap<SessionNodeKey, NodeId> {
    let mut map = FxHashMap::with_capacity_and_hasher(keys.len(), Default::default());
    for (i, &k) in keys.iter().enumerate() {
        map.insert(k, NodeId(i as u32));
    }
    map
}

/// The backward state of every corner of `analysis`, propagated over every node.
fn full_backward(
    analysis: &EngineAnalysis<'_>,
    record: &PropagationRecord,
) -> Vec<CornerBackwardState> {
    analysis
        .corners
        .iter()
        .zip(&record.lowered)
        .map(|(ca, lowered)| {
            propagate_backward_all(
                &analysis.graph,
                lowered,
                &ca.propagated,
                &ca.node_values,
                &ca.endpoints.records,
            )
        })
        .collect()
}

fn into_state(
    analysis: EngineAnalysis<'_>,
    lowered: Vec<LoweredCorner<'static>>,
    backward: Vec<CornerBackwardState>,
) -> SessionState {
    let EngineAnalysis {
        netview: _,
        graph,
        constraints,
        corners,
        scope,
        completeness,
        sta_summary,
        sta_status,
        diags,
    } = analysis;
    SessionState {
        graph,
        constraints,
        corners,
        lowered,
        backward,
        scope,
        completeness,
        sta_summary,
        sta_status,
        diags,
    }
}
