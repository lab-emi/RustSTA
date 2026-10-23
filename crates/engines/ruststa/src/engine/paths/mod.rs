// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Worst paths and their handles (owner: E6; `STA-REQ-0215`, `STA-REQ-0228`).
//!
//! Engine section D. A timing path is reconstructed backwards from an endpoint by following the
//! predecessor `(src_node, src_tr)` recorded by the forward pass, and for `-nworst k > 1` by a
//! best-first sidetrack deviation search over the enabled incoming arcs of the DAG.
//!
//! - **Clock-pin startpoint (`STA-REQ-0228`)**: a path launched by a sequential cell (`DFF` or
//!   `LAT`) starts at the launching cell's clock/enable pin (`r/CK` or `l/G`, with
//!   [`PathArcKind::Clock`] at the clock latency, followed by `r/Q` or `l/Q` with
//!   [`PathArcKind::Launch`] and the `CK -> Q` delay); a path launched by an input delay starts at
//!   the input port ([`PathArcKind::Input`]).
//! - **Setup-first handle ranking (`STA-REQ-0228`)**: all late (`setup`, `recovery`,
//!   `latch_setup`, `output_max`, `clock_gating_setup`) paths are ranked in increasing slack order
//!   before any early (`hold`, `removal`, `latch_hold`, `output_min`, `clock_gating_hold`) paths,
//!   with ties broken deterministically by canonical endpoint and predecessor topological ID, and
//!   assigned 1-based handles `path1`, `path2`, … ([`PathHandle`]).

pub(crate) mod relaunch;
pub(crate) mod search;

use std::fmt;
use std::str::FromStr;

use search::SearchCtx;
pub use search::propagate_clock_sense;

use crate::engine::delay::load::NetLoads;
use crate::engine::graph::TimingGraph;
use crate::engine::input::NetlistView;
use crate::engine::input::libview::TimingLib;
use crate::engine::prop::backward::{BackwardPass, EndpointCheckRole, EndpointRequirement};
use crate::engine::prop::lower::{IdealSchedule, LoweredCorner, LoweredResult};
use crate::engine::types::{An, CornerId, NodeId, TagId, Tr};

/// 1-based handle of a ranked timing path (`path1`, `path2`, …; `STA-REQ-0228`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathHandle(pub u32);

impl PathHandle {
    /// Creates a 1-based [`PathHandle`] (`rank_1_based >= 1`).
    #[inline]
    #[must_use]
    pub const fn new(rank_1_based: u32) -> Self {
        Self(rank_1_based)
    }

    /// 0-based index into [`PathTable::paths`].
    #[inline]
    #[must_use]
    pub const fn index(self) -> usize {
        self.0.saturating_sub(1) as usize
    }

    /// Formats this handle as `"path<N>"`.
    #[must_use]
    pub fn as_str_id(self) -> String {
        format!("path{}", self.0)
    }
}

impl fmt::Display for PathHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path{}", self.0)
    }
}

impl FromStr for PathHandle {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix("path").ok_or("expected 'path<N>'")?;
        let n: u32 = rest.parse().map_err(|_| "invalid path handle number")?;
        if n == 0 {
            return Err("path handle is 1-based (path1, path2, ...)");
        }
        Ok(Self(n))
    }
}

/// Kind of the timing arc leading into a [`PathPoint`] (`STA-REQ-0215`, `STA-REQ-0228`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathArcKind {
    /// Clock source or launching sequential clock/enable pin (`r/CK` or `l/G`).
    Clock,
    /// Sequential clock-to-output launch arc (`CK -> Q` or `G -> Q`).
    Launch,
    /// Combinational, three-state, or latch `D -> Q` cell arc.
    Cell,
    /// Wire (`Net`) arc from a driver to a load.
    Net,
    /// Primary input port seeded by `set_input_delay`.
    Input,
}

impl PathArcKind {
    /// Canonical lowercase label (`"clock"`, `"launch"`, `"cell"`, `"net"`, `"input"`).
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::Launch => "launch",
            Self::Cell => "cell",
            Self::Net => "net",
            Self::Input => "input",
        }
    }
}

impl fmt::Display for PathArcKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One pin or port along a reconstructed timing path (`STA-REQ-0215`, `STA-REQ-0228`).
#[derive(Debug, Clone, PartialEq)]
pub struct PathPoint {
    /// Graph [`NodeId`] of this point.
    pub node: NodeId,
    /// Canonical pin or port name (`"r0/CK"`, `"r0/Q"`, `"b1/A"`, `"b1/Y"`, `"d"`, `"q"`).
    pub pin_name: String,
    /// Instance name (`Some("r0")` for an instance pin, `None` for a port).
    pub inst_name: Option<String>,
    /// Cell type name (`Some("DFF")` for an instance pin, `None` for a port).
    pub cell_name: Option<String>,
    /// Arc kind leading into this point.
    pub arc_kind: PathArcKind,
    /// Signal transition (`Rise` or `Fall`) at this point.
    pub transition: Tr,
    /// Incremental delay (ns) of the step into this point.
    pub incr_ns: f64,
    /// Cumulative arrival time (ns) at this point.
    pub arrival_ns: f64,
    /// Pin transition time / slew (ns) at this point.
    pub slew_ns: f64,
}

/// A reconstructed timing path with its handle, startpoint, endpoint, slack, logic levels,
/// cell/wire delay shares, and points (`STA-REQ-0215`, `STA-REQ-0228`).
#[derive(Debug, Clone, PartialEq)]
pub struct TimingPath {
    /// 1-based handle (`path1`, `path2`, …).
    pub handle: PathHandle,
    /// Operating corner identifier.
    pub corner: CornerId,
    /// Startpoint [`NodeId`] (`r/CK` or `l/G` for register-launched paths, port for input paths).
    pub startpoint: NodeId,
    /// Canonical startpoint name.
    pub startpoint_name: String,
    /// Endpoint [`NodeId`].
    pub endpoint: NodeId,
    /// Canonical endpoint name.
    pub endpoint_name: String,
    /// Endpoint check role (`Setup`, `Hold`, `Recovery`, `Removal`, `LatchSetup`, `LatchHold`,
    /// `OutputMax`, `OutputMin`, etc.).
    pub role: EndpointCheckRole,
    /// Analysis bound (`An::Late` or `An::Early`).
    pub bound: An,
    /// Path group name (`clock_name`, `"async_default"`, `"clock_gating_default"`, etc.).
    pub path_group: String,
    /// Launch tag [`TagId`].
    pub launch_tag: TagId,
    /// Launch clock name, if clocked.
    pub launch_clock: Option<String>,
    /// Launch clock source edge.
    pub launch_edge: Option<Tr>,
    /// Capture clock name, if clocked.
    pub capture_clock: Option<String>,
    /// Capture clock source edge.
    pub capture_edge: Option<Tr>,
    /// Path slack (ns): `required_ns - arrival_ns` for `An::Late`, `arrival_ns - required_ns` for
    /// `An::Early`.
    pub slack_ns: f64,
    /// Data arrival time (ns) at the endpoint.
    pub arrival_ns: f64,
    /// Required arrival time (ns) at the endpoint.
    pub required_ns: f64,
    /// Intrinsic library check or output delay value (ns).
    pub check_ns: f64,
    /// Number of combinational [`PathArcKind::Cell`] arcs along the data path after launch.
    pub logic_levels: u32,
    /// Total cell delay (ns) along the data path (`Launch` + `Cell` steps).
    pub cell_delay_ns: f64,
    /// Total wire delay (ns) along the data path (`Net` steps).
    pub wire_delay_ns: f64,
    /// Share of cell delay in `%`, rounded to `0.1%` (`cell_delay_pct + wire_delay_pct == 100.0`
    /// when `cell_delay_ns + wire_delay_ns > 0`).
    pub cell_delay_pct: f64,
    /// Share of wire delay in `%`, rounded to `0.1%`.
    pub wire_delay_pct: f64,
    /// Ordered sequence of points from startpoint to endpoint.
    pub points: Vec<PathPoint>,
}

/// Options controlling path search and handle ranking (`STA-REQ-0215`, `STA-REQ-0228`).
#[derive(Debug, Clone)]
pub struct PathSearchOptions {
    /// Maximum number of paths enumerated per `(endpoint, role)` check (`>= 1`, default `1`).
    pub nworst: usize,
    /// Maximum total number of paths returned (`default 100`).
    pub max_paths: usize,
    /// Optional bound filter (`Some(An::Late)` for setup/recovery only, `Some(An::Early)` for
    /// hold/removal only, `None` for setup-first then hold).
    pub bound_filter: Option<An>,
    /// Optional endpoint [`NodeId`] filter.
    pub endpoint_filter: Option<NodeId>,
    /// Optional slack upper bound filter (`slack_ns <= threshold`).
    pub slack_lesser_than: Option<f64>,
    /// Corner identifier assigned to returned paths (default `CornerId(0)`).
    pub corner: CornerId,
}

impl Default for PathSearchOptions {
    fn default() -> Self {
        Self {
            nworst: 1,
            max_paths: 100,
            bound_filter: None,
            endpoint_filter: None,
            slack_lesser_than: None,
            corner: CornerId(0),
        }
    }
}

/// Ranked table of timing paths with 1-based [`PathHandle`]s (`STA-REQ-0215`, `STA-REQ-0228`).
#[derive(Debug, Clone, Default)]
pub struct PathTable {
    paths: Vec<TimingPath>,
}

impl PathTable {
    /// Builds a ranked [`PathTable`] for a corner from its [`BackwardPass`] endpoint requirements
    /// and forward propagation result (`STA-REQ-0215`, `STA-REQ-0228`).
    #[must_use]
    pub fn build(
        netview: &NetlistView<'_>,
        graph: &TimingGraph,
        lib: &TimingLib,
        net_loads: &NetLoads,
        schedule: &IdealSchedule,
        lowered: &LoweredCorner<'_>,
        result: &LoweredResult,
        backward: &BackwardPass,
        options: &PathSearchOptions,
    ) -> Self {
        let nworst = options.nworst.max(1);
        let max_paths = options.max_paths;
        if max_paths == 0 {
            return Self { paths: Vec::new() };
        }

        let ctx = SearchCtx::new(netview, graph, lib, net_loads, schedule, lowered, result);

        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct CheckGroupKey {
            is_early: bool,
            ep_topo: u32,
            ep_node: u32,
            role: EndpointCheckRole,
            capture_clock: Option<u32>,
            group: String,
        }

        let mut groups: std::collections::BTreeMap<CheckGroupKey, Vec<&EndpointRequirement>> =
            std::collections::BTreeMap::new();
        for req in &backward.requirements {
            if let Some(bf) = options.bound_filter
                && req.an != bf
            {
                continue;
            }
            if let Some(ef) = options.endpoint_filter
                && req.node != ef
            {
                continue;
            }
            let key = CheckGroupKey {
                is_early: req.an == An::Early,
                ep_topo: graph.topo_rank(req.node),
                ep_node: req.node.0,
                role: req.role,
                capture_clock: req.capture_clock.map(|c| c.0),
                group: req.path_group.clone(),
            };
            groups.entry(key).or_default().push(req);
        }

        let mut candidates: Vec<(TimingPath, Vec<(u32, u8)>)> = Vec::new();
        for (_key, reqs) in groups {
            let found = ctx.search_k_worst_for_check(&reqs, nworst, options.corner);
            for (path, topo_sig) in found {
                if let Some(limit) = options.slack_lesser_than
                    && path.slack_ns > limit
                {
                    continue;
                }
                candidates.push((path, topo_sig));
            }
        }

        // Setup-first handle ranking (`STA-REQ-0228`):
        // 1. All late (`An::Late`) paths before any early (`An::Early`) paths;
        // 2. Increasing `slack_ns`;
        // 3. Canonical endpoint topological rank `graph.topo_rank(endpoint)`;
        // 4. Role and corner;
        // 5. Reverse predecessor topological signature `(topo_rank(node), tr)` from endpoint to
        //    startpoint;
        // 6. Launch tag.
        candidates.sort_by(|(a, sig_a), (b, sig_b)| {
            let a_early = a.bound == An::Early;
            let b_early = b.bound == An::Early;
            a_early
                .cmp(&b_early)
                .then_with(|| a.slack_ns.total_cmp(&b.slack_ns))
                .then_with(|| {
                    graph
                        .topo_rank(a.endpoint)
                        .cmp(&graph.topo_rank(b.endpoint))
                })
                .then_with(|| a.role.cmp(&b.role))
                .then_with(|| a.corner.cmp(&b.corner))
                .then_with(|| sig_a.cmp(sig_b))
                .then_with(|| a.launch_tag.cmp(&b.launch_tag))
        });

        if candidates.len() > max_paths {
            candidates.truncate(max_paths);
        }

        let paths = candidates
            .into_iter()
            .enumerate()
            .map(|(i, (mut p, _))| {
                p.handle = PathHandle::new((i + 1) as u32);
                p
            })
            .collect();

        Self { paths }
    }

    /// Slice of ranked paths (`path1`, `path2`, …).
    #[inline]
    #[must_use]
    pub fn paths(&self) -> &[TimingPath] {
        &self.paths
    }

    /// Consumes the table and returns the ranked paths.
    #[inline]
    #[must_use]
    pub fn into_paths(self) -> Vec<TimingPath> {
        self.paths
    }

    /// Number of ranked paths.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// True if the table holds no paths.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Looks up a path by its 1-based [`PathHandle`].
    #[inline]
    #[must_use]
    pub fn by_handle(&self, handle: PathHandle) -> Option<&TimingPath> {
        if handle.0 == 0 {
            return None;
        }
        self.paths.get(handle.index())
    }

    /// Looks up a path by its string handle (`"path1"`, `"path2"`, …).
    #[must_use]
    pub fn by_handle_str(&self, handle: &str) -> Option<&TimingPath> {
        let h: PathHandle = handle.parse().ok()?;
        self.by_handle(h)
    }

    /// Returns the worst-slack path for `(endpoint, bound)`, if present.
    #[must_use]
    pub fn worst_for_endpoint(&self, endpoint: NodeId, bound: An) -> Option<&TimingPath> {
        self.paths
            .iter()
            .find(|p| p.endpoint == endpoint && p.bound == bound)
    }
}

/// Convenience helper that builds a [`PathTable`] directly from [`BackwardPass`] and
/// [`LoweredCorner`] / [`LoweredResult`] using default [`NetLoads`] when not already available.
#[must_use]
pub fn build_paths_from_backward(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &TimingLib,
    schedule: &IdealSchedule,
    lowered: &LoweredCorner<'_>,
    result: &LoweredResult,
    backward: &BackwardPass,
    options: &PathSearchOptions,
) -> PathTable {
    let loads = NetLoads::compute(
        netview.frozen(),
        lib,
        &crate::engine::delay::wire::Ideal,
        &crate::engine::delay::load::NetLoadOverrides::default(),
    );
    PathTable::build(
        netview, graph, lib, &loads, schedule, lowered, result, backward, options,
    )
}
