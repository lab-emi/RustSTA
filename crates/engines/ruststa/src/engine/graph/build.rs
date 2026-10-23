// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Graph construction: nodes and arcs from the netlist and library views (owner: E1).
//!
//! Engine section A. One node per port bit and pin row; net arcs from each driver to the loads
//! of its (aliased) net; cell arcs from the library view; three-state and inout handling.
//! Combinational cycles are broken in name order inside the non-trivial strongly connected
//! components only, so that values do not depend on the order of netlist statements and acyclic
//! designs pay no name sort. Every disabled arc keeps its reason.

use std::borrow::Cow;

use rusteda_core::liberty::{Cell, Library, Pin, TimingSense, TimingType};

use crate::engine::graph::levels::{Levels, build_out_csr_and_indeg, kahn_longest_paths};
use crate::engine::input::netview::{NetlistView, NetlistViewError};
use crate::engine::types::{LibArc, NetRow, NodeId, Tr};

/// Why a propagation arc of the timing graph is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DisableReason {
    /// Disabled by combinational cycle breaking (STA-REQ-0102).
    LoopBreak,
    /// Disabled by constant propagation from netlist constants or tie cells (E7).
    Constant,
    /// Disabled by `set_case_analysis` or `set_logic_*` (E7).
    CaseAnalysis,
    /// Disabled because the arc's Liberty `when` condition evaluates to false (E7).
    FalseWhen,
    /// Disabled by user constraint `set_disable_timing` (E7).
    UserDisabled,
}

/// Unateness (timing sense) of a timing arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArcSense {
    PositiveUnate,
    NegativeUnate,
    NonUnate,
}

/// Kind of a propagation arc in the timing graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArcKind {
    /// A net (wire) arc from a driver node to a load node on `net`.
    Net { net: NetRow },
    /// A combinational cell arc (`combinational`, `combinational_rise`, `combinational_fall`).
    Comb {
        inst: u32,
        lib_arc: LibArc,
        outs: [bool; 2],
    },
    /// A three-state enable cell arc (`three_state_enable*`).
    TristateEnable {
        inst: u32,
        lib_arc: LibArc,
        outs: [bool; 2],
    },
    /// A three-state disable cell arc (`three_state_disable*`).
    TristateDisable {
        inst: u32,
        lib_arc: LibArc,
        outs: [bool; 2],
    },
    /// A transparent latch data-to-output cell arc (`D -> Q`), paired with the index of the
    /// corresponding setup check in [`TimingGraph::checks`] (STA-REQ-0213).
    LatchData {
        inst: u32,
        lib_arc: LibArc,
        outs: [bool; 2],
        setup_check: u32,
    },
}

impl ArcKind {
    /// True if this is a wire (`Net`) arc.
    #[inline]
    pub fn is_net(&self) -> bool {
        matches!(self, ArcKind::Net { .. })
    }

    /// True if this is a cell arc (`Comb`, `TristateEnable`, `TristateDisable`, `LatchData`).
    #[inline]
    pub fn is_cell(&self) -> bool {
        !self.is_net()
    }

    /// True if this is a three-state enable or disable cell arc.
    #[inline]
    pub fn is_tristate(&self) -> bool {
        matches!(
            self,
            ArcKind::TristateEnable { .. } | ArcKind::TristateDisable { .. }
        )
    }

    /// True if this is a latch data-to-output arc.
    #[inline]
    pub fn is_latch_data(&self) -> bool {
        matches!(self, ArcKind::LatchData { .. })
    }

    /// Output transitions `[rise, fall]` that this arc can produce.
    #[inline]
    pub fn outs(&self) -> [bool; 2] {
        match *self {
            ArcKind::Net { .. } => [true, true],
            ArcKind::Comb { outs, .. }
            | ArcKind::TristateEnable { outs, .. }
            | ArcKind::TristateDisable { outs, .. }
            | ArcKind::LatchData { outs, .. } => outs,
        }
    }

    /// For a cell arc, returns `Some((inst, lib_arc))`; for a net arc, `None`.
    #[inline]
    pub fn inst_and_lib_arc(&self) -> Option<(u32, LibArc)> {
        match *self {
            ArcKind::Net { .. } => None,
            ArcKind::Comb { inst, lib_arc, .. }
            | ArcKind::TristateEnable { inst, lib_arc, .. }
            | ArcKind::TristateDisable { inst, lib_arc, .. }
            | ArcKind::LatchData { inst, lib_arc, .. } => Some((inst, lib_arc)),
        }
    }
}

/// A propagation arc in the timing graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphArc {
    pub src: NodeId,
    pub dst: NodeId,
    pub sense: ArcSense,
    pub kind: ArcKind,
}

const _: () = assert!(size_of::<GraphArc>() <= 28);

/// A sequential clock-to-output launch arc (`rising_edge` or `falling_edge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LaunchArc {
    pub inst: u32,
    pub lib_arc: LibArc,
    pub clock_pin: NodeId,
    pub out_pin: NodeId,
    pub edge: Tr,
    pub sense: ArcSense,
}

/// Kind of a timing check arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckKind {
    Setup,
    Hold,
    Recovery,
    Removal,
}

impl CheckKind {
    /// True for setup-like checks (`Setup`, `Recovery`).
    #[inline]
    pub fn is_setup(self) -> bool {
        matches!(self, CheckKind::Setup | CheckKind::Recovery)
    }

    /// True for hold-like checks (`Hold`, `Removal`).
    #[inline]
    pub fn is_hold(self) -> bool {
        matches!(self, CheckKind::Hold | CheckKind::Removal)
    }
}

/// A timing check (`setup`, `hold`, `recovery`, `removal`) between a clock/control pin and a
/// data/constrained pin on an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckArc {
    pub inst: u32,
    pub lib_arc: LibArc,
    pub data_pin: NodeId,
    pub clock_pin: NodeId,
    pub kind: CheckKind,
    pub edge: Tr,
}

/// A non-fatal diagnostic emitted during [`TimingGraph::build`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDiagnostic {
    /// Diagnostic code (`T0010` unconnected inputs, `T0011` undriven net, `T0012` broken cycle).
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

/// Errors that can occur when building a [`TimingGraph`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphBuildError {
    /// Netlist view error.
    #[error(transparent)]
    Netlist(#[from] NetlistViewError),
    /// A cell referenced by the netlist is missing from the library.
    #[error("unknown cell '{cell}' (STA-REQ-0003)")]
    UnknownCell { cell: String },
    /// A cell pin referenced by the netlist is missing from the library cell.
    #[error("cell '{cell}' has no pin '{pin}' (STA-REQ-0003)")]
    UnknownCellPin { cell: String, pin: String },
}

impl GraphBuildError {
    /// Stable `T`-prefixed diagnostic code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            GraphBuildError::Netlist(e) => e.code(),
            GraphBuildError::UnknownCell { .. } => "T0004",
            GraphBuildError::UnknownCellPin { .. } => "T0005",
        }
    }
}

/// The levelized timing graph of a revision.
#[derive(Debug, Clone)]
pub struct TimingGraph {
    n_nodes: usize,
    n_port_bits: usize,
    arcs: Vec<GraphArc>,
    disabled: Vec<Option<DisableReason>>,
    launches: Vec<LaunchArc>,
    disabled_launches: Vec<Option<DisableReason>>,
    checks: Vec<CheckArc>,
    disabled_checks: Vec<Option<DisableReason>>,
    levels: Levels,
    diagnostics: Vec<GraphDiagnostic>,
}

#[derive(Clone, Copy)]
enum CellPropKind {
    Comb([bool; 2]),
    TristateEnable([bool; 2]),
    TristateDisable([bool; 2]),
    LatchData {
        outs: [bool; 2],
        cell_setup_check: usize,
    },
    Edge(Tr),
}

#[derive(Clone, Copy)]
struct CellArcTemplate {
    from_cp: u32,
    to_cp: u32,
    lib_arc: LibArc,
    sense: ArcSense,
    kind: CellPropKind,
}

#[derive(Clone, Copy)]
struct CellCheckTemplate {
    data_cp: u32,
    clock_cp: u32,
    lib_arc: LibArc,
    kind: CheckKind,
    edge: Tr,
}

struct CellTemplate {
    n_pins: usize,
    arcs: Vec<CellArcTemplate>,
    checks: Vec<CellCheckTemplate>,
}

impl TimingGraph {
    /// Builds the timing graph from a [`NetlistView`] and a corner's Liberty libraries.
    pub fn build(
        netview: &NetlistView<'_>,
        libs: &[Library],
    ) -> Result<TimingGraph, GraphBuildError> {
        let frozen = netview.frozen();
        let n_nodes = netview.num_nodes();
        let n_port_bits = netview.num_port_bits();
        let n_cells = netview.num_cells();

        // 1. Extract per-cell timing arc and check templates once per cell type in CELLS.
        let mut cell_templates = Vec::with_capacity(n_cells);
        let mut max_cell_pins = 0usize;
        for cell_row in 0..n_cells {
            let cell_name = netview.cell_name(cell_row as u32);
            let lib_cell = libs
                .iter()
                .flat_map(|l| &l.cells)
                .find(|c| c.name == cell_name)
                .ok_or_else(|| GraphBuildError::UnknownCell {
                    cell: cell_name.to_string(),
                })?;
            let tpl = extract_cell_template(netview, cell_row as u32, lib_cell)?;
            max_cell_pins = max_cell_pins.max(tpl.n_pins);
            cell_templates.push(tpl);
        }

        // 2. Emit net propagation arcs from each driver to each load of every canonical net.
        let mut arcs = Vec::with_capacity(n_nodes);
        for &net in netview.canonical_nets() {
            let drvs = netview.drivers(net);
            let lds = netview.loads(net);
            if drvs.is_empty() || lds.is_empty() {
                continue;
            }
            for &d in drvs {
                for &l in lds {
                    // An inout port/pin is both a driver and a load of its net; never emit a
                    // self-loop wire arc from a node to itself.
                    if d != l {
                        arcs.push(GraphArc {
                            src: d,
                            dst: l,
                            sense: ArcSense::PositiveUnate,
                            kind: ArcKind::Net { net },
                        });
                    }
                }
            }
        }

        // 3. Emit cell propagation arcs, sequential launches, and timing checks per instance,
        // reusing a single scratch buffer of size `max_cell_pins` instead of allocating per instance.
        let mut launches = Vec::new();
        let mut checks = Vec::new();
        let mut pin_to_node = vec![u32::MAX; max_cell_pins];
        let mut check_idx_map = Vec::new();

        for (inst_row, inst_rec) in frozen.instances.iter().enumerate() {
            let cell_row = inst_rec.cell as usize;
            let tpl = &cell_templates[cell_row];
            let inst_pins = frozen.instance_pins(inst_row as u32);
            for p in inst_pins.clone() {
                let cp = frozen.pin_cellpin[p] as usize;
                pin_to_node[cp] = (n_port_bits + p) as u32;
            }

            // Emit checks first so LatchData arcs can reference the emitted setup check index.
            check_idx_map.clear();
            if !tpl.checks.is_empty() {
                check_idx_map.resize(tpl.checks.len(), u32::MAX);
                for (ci, ch) in tpl.checks.iter().enumerate() {
                    let data_node = pin_to_node[ch.data_cp as usize];
                    let clock_node = pin_to_node[ch.clock_cp as usize];
                    if data_node != u32::MAX && clock_node != u32::MAX {
                        let idx = checks.len() as u32;
                        check_idx_map[ci] = idx;
                        checks.push(CheckArc {
                            inst: inst_row as u32,
                            lib_arc: ch.lib_arc,
                            data_pin: NodeId(data_node),
                            clock_pin: NodeId(clock_node),
                            kind: ch.kind,
                            edge: ch.edge,
                        });
                    }
                }
            }

            // Emit propagation and launch arcs.
            for a in &tpl.arcs {
                let f = pin_to_node[a.from_cp as usize];
                let t = pin_to_node[a.to_cp as usize];
                if f == u32::MAX || t == u32::MAX {
                    continue;
                }
                let src = NodeId(f);
                let dst = NodeId(t);
                let inst = inst_row as u32;
                match a.kind {
                    CellPropKind::Comb(outs) => arcs.push(GraphArc {
                        src,
                        dst,
                        sense: a.sense,
                        kind: ArcKind::Comb {
                            inst,
                            lib_arc: a.lib_arc,
                            outs,
                        },
                    }),
                    CellPropKind::TristateEnable(outs) => arcs.push(GraphArc {
                        src,
                        dst,
                        sense: a.sense,
                        kind: ArcKind::TristateEnable {
                            inst,
                            lib_arc: a.lib_arc,
                            outs,
                        },
                    }),
                    CellPropKind::TristateDisable(outs) => arcs.push(GraphArc {
                        src,
                        dst,
                        sense: a.sense,
                        kind: ArcKind::TristateDisable {
                            inst,
                            lib_arc: a.lib_arc,
                            outs,
                        },
                    }),
                    CellPropKind::LatchData {
                        outs,
                        cell_setup_check,
                    } => {
                        let setup_check = check_idx_map
                            .get(cell_setup_check)
                            .copied()
                            .unwrap_or(u32::MAX);
                        let kind = if setup_check != u32::MAX {
                            ArcKind::LatchData {
                                inst,
                                lib_arc: a.lib_arc,
                                outs,
                                setup_check,
                            }
                        } else {
                            ArcKind::Comb {
                                inst,
                                lib_arc: a.lib_arc,
                                outs,
                            }
                        };
                        arcs.push(GraphArc {
                            src,
                            dst,
                            sense: a.sense,
                            kind,
                        });
                    }
                    CellPropKind::Edge(edge) => launches.push(LaunchArc {
                        inst,
                        lib_arc: a.lib_arc,
                        clock_pin: src,
                        out_pin: dst,
                        edge,
                        sense: a.sense,
                    }),
                }
            }

            // Reset scratch slots touched by this instance.
            for p in inst_pins {
                let cp = frozen.pin_cellpin[p] as usize;
                pin_to_node[cp] = u32::MAX;
            }
        }

        let mut disabled = vec![None; arcs.len()];
        let mut diagnostics = Vec::new();
        for w in netview.warnings() {
            diagnostics.push(GraphDiagnostic {
                code: w.code,
                message: w.message.clone(),
            });
        }
        for &net in netview.undriven_nets() {
            diagnostics.push(GraphDiagnostic {
                code: "T0011",
                message: format!(
                    "net {} has no driver; its loads are unconstrained (STA-REQ-0002)",
                    netview.net_name(net)
                ),
            });
        }

        // 4. Fast Kahn pass: checks acyclicity and computes longest-path levels in one pass.
        // Only when `visited < n_nodes` (at least one combinational cycle exists) do we run
        // Tarjan's SCC + name-ordered DFS inside the non-trivial SCCs!
        let (out_off, out_arcs, mut indeg) = build_out_csr_and_indeg(n_nodes, &arcs, &disabled);
        let (level, visited) = kahn_longest_paths(n_nodes, &arcs, &out_off, &out_arcs, &mut indeg);

        let levels = if visited == n_nodes {
            Levels::from_kahn(n_nodes, &arcs, &disabled, level, out_off, out_arcs)
        } else {
            break_cycles_in_sccs(
                netview,
                n_nodes,
                &arcs,
                &out_off,
                &out_arcs,
                &mut disabled,
                &mut diagnostics,
            );
            Levels::compute(n_nodes, &arcs, &disabled)
        };

        let disabled_launches = vec![None; launches.len()];
        let disabled_checks = vec![None; checks.len()];

        Ok(TimingGraph {
            n_nodes,
            n_port_bits,
            arcs,
            disabled,
            launches,
            disabled_launches,
            checks,
            disabled_checks,
            levels,
            diagnostics,
        })
    }

    /// Total number of nodes in the timing graph (`num_port_bits() + num_pins()`).
    #[inline]
    pub fn num_nodes(&self) -> usize {
        self.n_nodes
    }

    /// Number of top-level port bit nodes (`0..num_port_bits()`).
    #[inline]
    pub fn num_port_bits(&self) -> usize {
        self.n_port_bits
    }

    /// Number of connected instance pin nodes (`num_nodes() - num_port_bits()`).
    #[inline]
    pub fn num_pins(&self) -> usize {
        self.n_nodes - self.n_port_bits
    }

    /// All propagation arcs (both enabled and disabled).
    #[inline]
    pub fn arcs(&self) -> &[GraphArc] {
        &self.arcs
    }

    /// Refines the timing sense of propagation arc `arc_idx` (`STA-REQ-0152`).
    #[inline]
    pub fn set_arc_sense(&mut self, arc_idx: usize, sense: ArcSense) {
        self.arcs[arc_idx].sense = sense;
    }

    /// Disable status (`None` if enabled, `Some(reason)` if disabled) per propagation arc.
    #[inline]
    pub fn disabled(&self) -> &[Option<DisableReason>] {
        &self.disabled
    }

    /// Returns why arc `arc_idx` is disabled, or `None` if enabled.
    #[inline]
    pub fn arc_disable_reason(&self, arc_idx: usize) -> Option<DisableReason> {
        self.disabled[arc_idx]
    }

    /// True if propagation arc `arc_idx` is disabled.
    #[inline]
    pub fn is_arc_disabled(&self, arc_idx: usize) -> bool {
        self.disabled[arc_idx].is_some()
    }

    /// Disables propagation arc `arc_idx` with `reason` (unless already disabled) and marks the
    /// graph ready for [`Self::relevelize`].
    #[inline]
    pub fn disable_arc(&mut self, arc_idx: usize, reason: DisableReason) {
        if self.disabled[arc_idx].is_none() {
            self.disabled[arc_idx] = Some(reason);
        }
    }

    /// Recomputes levels and CSR arc tables after disabling arcs (e.g. in E7 constant propagation).
    pub fn relevelize(&mut self) {
        self.levels = Levels::compute(self.n_nodes, &self.arcs, &self.disabled);
    }

    /// Sequential clock-to-output launch arcs (`rising_edge` / `falling_edge`).
    #[inline]
    pub fn launches(&self) -> &[LaunchArc] {
        &self.launches
    }

    /// Disable status (`None` if enabled, `Some(reason)` if disabled) per launch arc.
    #[inline]
    pub fn disabled_launches(&self) -> &[Option<DisableReason>] {
        &self.disabled_launches
    }

    /// Returns why launch arc `launch_idx` is disabled, or `None` if enabled.
    #[inline]
    pub fn launch_disable_reason(&self, launch_idx: usize) -> Option<DisableReason> {
        self.disabled_launches[launch_idx]
    }

    /// True if launch arc `launch_idx` is disabled.
    #[inline]
    pub fn is_launch_disabled(&self, launch_idx: usize) -> bool {
        self.disabled_launches[launch_idx].is_some()
    }

    /// Disables launch arc `launch_idx` with `reason` (unless already disabled).
    #[inline]
    pub fn disable_launch(&mut self, launch_idx: usize, reason: DisableReason) {
        if self.disabled_launches[launch_idx].is_none() {
            self.disabled_launches[launch_idx] = Some(reason);
        }
    }

    /// Timing check arcs (`setup`, `hold`, `recovery`, `removal`).
    #[inline]
    pub fn checks(&self) -> &[CheckArc] {
        &self.checks
    }

    /// Disable status (`None` if enabled, `Some(reason)` if disabled) per timing check arc.
    #[inline]
    pub fn disabled_checks(&self) -> &[Option<DisableReason>] {
        &self.disabled_checks
    }

    /// Returns why timing check arc `check_idx` is disabled, or `None` if enabled.
    #[inline]
    pub fn check_disable_reason(&self, check_idx: usize) -> Option<DisableReason> {
        self.disabled_checks[check_idx]
    }

    /// True if timing check arc `check_idx` is disabled.
    #[inline]
    pub fn is_check_disabled(&self, check_idx: usize) -> bool {
        self.disabled_checks[check_idx].is_some()
    }

    /// Disables timing check arc `check_idx` with `reason` (unless already disabled).
    #[inline]
    pub fn disable_check(&mut self, check_idx: usize, reason: DisableReason) {
        if self.disabled_checks[check_idx].is_none() {
            self.disabled_checks[check_idx] = Some(reason);
        }
    }

    /// Levelization and canonical topological ordering of the graph.
    #[inline]
    pub fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Exact longest-path level of `node`.
    #[inline]
    pub fn level(&self, node: NodeId) -> u32 {
        self.levels.level(node)
    }

    /// Canonical topological identifier `(level(node), node.0)`.
    #[inline]
    pub fn topo_id(&self, node: NodeId) -> (u32, u32) {
        self.levels.topo_id(node)
    }

    /// Dense 0-based topological rank (`0..num_nodes()`) of `node` in [`Self::order`].
    #[inline]
    pub fn topo_rank(&self, node: NodeId) -> u32 {
        self.levels.topo_rank(node)
    }

    /// All nodes in canonical topological order `(level(v), v.0)`.
    #[inline]
    pub fn order(&self) -> &[NodeId] {
        self.levels.order()
    }

    /// CSR level boundary offsets into [`Self::order`].
    #[inline]
    pub fn level_start(&self) -> &[u32] {
        self.levels.level_start()
    }

    /// Enabled incoming arc indices of `node`, ordered by `(topo_id(src), arc_idx)`.
    #[inline]
    pub fn incoming(&self, node: NodeId) -> &[u32] {
        self.levels.incoming(node)
    }

    /// Enabled outgoing arc indices of `node`, ordered by `(topo_id(dst), arc_idx)`.
    #[inline]
    pub fn outgoing(&self, node: NodeId) -> &[u32] {
        self.levels.outgoing(node)
    }

    /// Non-fatal diagnostics recorded during graph construction (`T0010`..`T0012`).
    #[inline]
    pub fn diagnostics(&self) -> &[GraphDiagnostic] {
        &self.diagnostics
    }
}

fn extract_cell_template(
    netview: &NetlistView<'_>,
    cell_row: u32,
    lib_cell: &Cell,
) -> Result<CellTemplate, GraphBuildError> {
    let frozen = netview.frozen();
    let pin_range = frozen.cell_pins(cell_row);
    let n_pins = pin_range.len();

    // Map each Liberty pin index `0..lib_cell.pins.len()` to the netlist's `0..n_pins` index.
    let mut lib_to_cp = vec![u32::MAX; lib_cell.pins.len()];
    for (lib_idx, lp) in lib_cell.pins.iter().enumerate() {
        for cp in 0..n_pins {
            if netview.cell_pin_name(cell_row, cp as u32) == lp.name {
                lib_to_cp[lib_idx] = cp as u32;
                break;
            }
        }
    }

    let find_cp_by_name = |name: &str| -> Option<u32> {
        lib_cell
            .pins
            .iter()
            .position(|p| p.name == name)
            .and_then(|li| {
                let cp = lib_to_cp[li];
                (cp != u32::MAX).then_some(cp)
            })
    };

    let mut arcs = Vec::new();
    let mut checks = Vec::new();

    for (to_lib_idx, pin) in lib_cell.pins.iter().enumerate() {
        let to_cp = lib_to_cp[to_lib_idx];
        if to_cp == u32::MAX {
            continue;
        }
        for arc in &pin.arcs {
            let Some(from_cp) = find_cp_by_name(&arc.related_pin) else {
                continue;
            };
            let tt = arc.kind();
            let check = match tt {
                TimingType::SetupRising => Some((CheckKind::Setup, Tr::Rise)),
                TimingType::SetupFalling => Some((CheckKind::Setup, Tr::Fall)),
                TimingType::HoldRising => Some((CheckKind::Hold, Tr::Rise)),
                TimingType::HoldFalling => Some((CheckKind::Hold, Tr::Fall)),
                TimingType::RecoveryRising => Some((CheckKind::Recovery, Tr::Rise)),
                TimingType::RecoveryFalling => Some((CheckKind::Recovery, Tr::Fall)),
                TimingType::RemovalRising => Some((CheckKind::Removal, Tr::Rise)),
                TimingType::RemovalFalling => Some((CheckKind::Removal, Tr::Fall)),
                _ => None,
            };
            if let Some((kind, edge)) = check {
                let lib_arc = LibArc(checks.len() as u32);
                checks.push(CellCheckTemplate {
                    data_cp: to_cp,
                    clock_cp: from_cp,
                    lib_arc,
                    kind,
                    edge,
                });
                continue;
            }

            let kind = match tt {
                TimingType::Combinational => CellPropKind::Comb([true, true]),
                TimingType::CombinationalRise => CellPropKind::Comb([true, false]),
                TimingType::CombinationalFall => CellPropKind::Comb([false, true]),
                TimingType::ThreeStateEnable => CellPropKind::TristateEnable([true, true]),
                TimingType::ThreeStateEnableRise => CellPropKind::TristateEnable([true, false]),
                TimingType::ThreeStateEnableFall => CellPropKind::TristateEnable([false, true]),
                TimingType::ThreeStateDisable => CellPropKind::TristateDisable([true, true]),
                TimingType::ThreeStateDisableRise => CellPropKind::TristateDisable([true, false]),
                TimingType::ThreeStateDisableFall => CellPropKind::TristateDisable([false, true]),
                TimingType::RisingEdge => CellPropKind::Edge(Tr::Rise),
                TimingType::FallingEdge => CellPropKind::Edge(Tr::Fall),
                _ => continue,
            };

            let sense = match arc.sense() {
                Some(TimingSense::PositiveUnate) => ArcSense::PositiveUnate,
                Some(TimingSense::NegativeUnate) => ArcSense::NegativeUnate,
                Some(TimingSense::NonUnate) => ArcSense::NonUnate,
                _ => derive_sense(pin, &arc.related_pin),
            };

            let lib_arc = LibArc(arcs.len() as u32);
            arcs.push(CellArcTemplate {
                from_cp,
                to_cp,
                lib_arc,
                sense,
                kind,
            });
        }
    }

    // Upgrade combinational D -> Q arcs on latches to `LatchData` when `from_cp` has a Setup check
    // against a clock pin `clock_cp` that also has an Edge arc `clock_cp -> to_cp` (STA-REQ-0213).
    for ai in 0..arcs.len() {
        let CellPropKind::Comb(outs) = arcs[ai].kind else {
            continue;
        };
        let from_cp = arcs[ai].from_cp;
        let to_cp = arcs[ai].to_cp;
        let found = checks.iter().position(|ch| {
            ch.kind == CheckKind::Setup
                && ch.data_cp == from_cp
                && arcs.iter().any(|e| {
                    matches!(e.kind, CellPropKind::Edge(_))
                        && e.from_cp == ch.clock_cp
                        && e.to_cp == to_cp
                })
        });
        if let Some(cell_setup_check) = found {
            arcs[ai].kind = CellPropKind::LatchData {
                outs,
                cell_setup_check,
            };
        }
    }

    Ok(CellTemplate {
        n_pins,
        arcs,
        checks,
    })
}

/// Derives timing sense from the output pin's boolean function when `timing_sense` is omitted
/// (STA-REQ-0012).
fn derive_sense(pin: &Pin, related: &str) -> ArcSense {
    let Some(f) = &pin.function else {
        return ArcSense::NonUnate;
    };
    let vars = f.vars();
    let Some(k) = vars.iter().position(|v| v == related) else {
        return ArcSense::NonUnate;
    };
    if vars.len() > 6 {
        return ArcSense::NonUnate;
    }
    let names: Vec<&str> = vars.iter().map(String::as_str).collect();
    let Some(tt) = f.truth_table(&names) else {
        return ArcSense::NonUnate;
    };
    let n = names.len();
    let (mut up, mut down) = (false, false);
    for m in 0..(1u64 << n) {
        if (m >> k) & 1 == 1 {
            continue;
        }
        let lo = (tt >> m) & 1;
        let hi = (tt >> (m | (1 << k))) & 1;
        if hi > lo {
            up = true;
        }
        if lo > hi {
            down = true;
        }
    }
    match (up, down) {
        (true, false) => ArcSense::PositiveUnate,
        (false, true) => ArcSense::NegativeUnate,
        _ => ArcSense::NonUnate,
    }
}

/// Finds all non-trivial strongly connected components via iterative Tarjan's algorithm and breaks
/// combinational cycles inside each non-trivial SCC via name-ordered DFS (STA-REQ-0102).
fn break_cycles_in_sccs(
    netview: &NetlistView<'_>,
    n_nodes: usize,
    arcs: &[GraphArc],
    out_off: &[u32],
    out_arcs: &[u32],
    disabled: &mut [Option<DisableReason>],
    diagnostics: &mut Vec<GraphDiagnostic>,
) {
    let mut index = vec![u32::MAX; n_nodes];
    let mut lowlink = vec![0u32; n_nodes];
    let mut on_stack = vec![false; n_nodes];
    let mut scc_stack: Vec<u32> = Vec::new();
    let mut dfs_stack: Vec<(u32, u32)> = Vec::new();
    let mut next_idx = 0u32;
    let mut non_trivial_sccs: Vec<Vec<u32>> = Vec::new();

    for start in 0..n_nodes as u32 {
        if index[start as usize] != u32::MAX {
            continue;
        }
        index[start as usize] = next_idx;
        lowlink[start as usize] = next_idx;
        next_idx += 1;
        scc_stack.push(start);
        on_stack[start as usize] = true;
        dfs_stack.push((start, out_off[start as usize]));

        while let Some(&mut (v, ref mut cursor)) = dfs_stack.last_mut() {
            let end = out_off[v as usize + 1];
            if *cursor < end {
                let ai = out_arcs[*cursor as usize];
                *cursor += 1;
                if disabled[ai as usize].is_some() {
                    continue;
                }
                let w = arcs[ai as usize].dst.0;
                if index[w as usize] == u32::MAX {
                    index[w as usize] = next_idx;
                    lowlink[w as usize] = next_idx;
                    next_idx += 1;
                    scc_stack.push(w);
                    on_stack[w as usize] = true;
                    dfs_stack.push((w, out_off[w as usize]));
                } else if on_stack[w as usize] {
                    lowlink[v as usize] = lowlink[v as usize].min(index[w as usize]);
                }
            } else {
                dfs_stack.pop();
                if let Some(&(parent, _)) = dfs_stack.last() {
                    lowlink[parent as usize] = lowlink[parent as usize].min(lowlink[v as usize]);
                }
                if lowlink[v as usize] == index[v as usize] {
                    let mut scc = Vec::new();
                    while let Some(u) = scc_stack.pop() {
                        on_stack[u as usize] = false;
                        scc.push(u);
                        if u == v {
                            break;
                        }
                    }
                    let is_non_trivial = if scc.len() > 1 {
                        true
                    } else {
                        let u = scc[0];
                        (out_off[u as usize]..out_off[u as usize + 1]).any(|k| {
                            let ai = out_arcs[k as usize] as usize;
                            disabled[ai].is_none() && arcs[ai].dst.0 == u
                        })
                    };
                    if is_non_trivial {
                        non_trivial_sccs.push(scc);
                    }
                }
            }
        }
    }

    // Break cycles in name order inside each non-trivial SCC.
    let mut in_scc = vec![u32::MAX; n_nodes];
    let mut broken_records: Vec<(String, String, u32)> = Vec::new();

    for scc in &non_trivial_sccs {
        let m = scc.len();
        let mut names: Vec<Cow<'_, str>> = Vec::with_capacity(m);
        for (local_idx, &u) in scc.iter().enumerate() {
            in_scc[u as usize] = local_idx as u32;
            names.push(netview.node_name_str(NodeId(u)));
        }

        let mut local_start: Vec<usize> = (0..m).collect();
        local_start.sort_by(|&a, &b| names[a].cmp(&names[b]));

        let mut local_out: Vec<Vec<u32>> = vec![Vec::new(); m];
        for (local_idx, &u) in scc.iter().enumerate() {
            let lo = out_off[u as usize] as usize;
            let hi = out_off[u as usize + 1] as usize;
            for &ai in &out_arcs[lo..hi] {
                if disabled[ai as usize].is_none() {
                    let w = arcs[ai as usize].dst.0 as usize;
                    if in_scc[w] != u32::MAX {
                        local_out[local_idx].push(ai);
                    }
                }
            }
            local_out[local_idx].sort_by(|&a, &b| {
                let wa = in_scc[arcs[a as usize].dst.index()] as usize;
                let wb = in_scc[arcs[b as usize].dst.index()] as usize;
                names[wa].cmp(&names[wb]).then(a.cmp(&b))
            });
        }

        let mut color = vec![0u8; m];
        let mut stack: Vec<(usize, usize)> = Vec::new();
        for s_local in local_start {
            if color[s_local] != 0 {
                continue;
            }
            color[s_local] = 1;
            stack.push((s_local, 0));
            while let Some(&mut (u_local, ref mut pos)) = stack.last_mut() {
                if *pos < local_out[u_local].len() {
                    let ai = local_out[u_local][*pos];
                    *pos += 1;
                    let w_local = in_scc[arcs[ai as usize].dst.index()] as usize;
                    match color[w_local] {
                        0 => {
                            color[w_local] = 1;
                            stack.push((w_local, 0));
                        }
                        1 => {
                            disabled[ai as usize] = Some(DisableReason::LoopBreak);
                            broken_records.push((
                                names[u_local].to_string(),
                                names[w_local].to_string(),
                                ai,
                            ));
                        }
                        _ => {}
                    }
                } else {
                    color[u_local] = 2;
                    stack.pop();
                }
            }
        }

        for &u in scc {
            in_scc[u as usize] = u32::MAX;
        }
    }

    // Sort broken cycle records by (src_name, dst_name, arc_idx) so diagnostics across multiple
    // SCCs are emitted in a canonical statement-order-independent sequence.
    broken_records.sort();
    for (u_name, w_name, _) in broken_records {
        diagnostics.push(GraphDiagnostic {
            code: "T0012",
            message: format!(
                "combinational cycle broken at arc {u_name} -> {w_name} (STA-REQ-0102)"
            ),
        });
    }
}
