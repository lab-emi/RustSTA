// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Ideal clocks (owner: E5).
//!
//! Engine section C. The [`ClockModel`](super::ClockModel) of Phase 2a: a clock edge reaches every
//! pin of its clock network at the edge's waveform time plus the clock's source and network
//! latency (`set_clock_latency`), with the clock's ideal transition (`set_clock_transition`). The
//! transition at a pin follows the sense of the clock network from the clock source to the pin.

use rustc_hash::FxHashMap;
use rusteda_formats::sdc::{EarlyLateScope, EdgeScope, MinMaxScope, SetupHoldScope};

use super::{ClockArrival, ClockModel, SENSE_BOTH, SENSE_NEG, SENSE_POS, SenseBits};
use crate::engine::graph::{ArcKind, ArcSense, TimingGraph};
use crate::engine::input::constr::{ClockLatencyTarget, CompiledConstraints};
use crate::engine::types::{An, ClockId, NodeId, Tr};

/// Ideal clock model for one corner (`STA-REQ-0200`, `STA-REQ-0217`, `STA-REQ-0220`).
#[derive(Debug, Clone)]
pub struct IdealClockModel {
    /// Per-node list of `(ClockId, SenseBits)` reaching the node in the clock network.
    clock_sense: Vec<Vec<(ClockId, SenseBits)>>,
    /// Per-clock source latency `[an][edge]` in ns (`Early` uses `early`/`min`, `Late` uses
    /// `late`/`max`; `STA-REQ-0217`).
    source_latency_ns: Vec<[[f64; 2]; 2]>,
    /// Per-clock network latency `[an][edge]` in ns (`STA-REQ-0217`).
    network_latency_ns: Vec<[[f64; 2]; 2]>,
    /// Per-clock transition `[an][tr]` in ns (`STA-REQ-0217`).
    transition_ns: Vec<[[f64; 2]; 2]>,
    /// Optional per-`(ClockId, NodeId)` source latency override `[an][edge]`.
    pin_source_latency_ns: FxHashMap<(ClockId, NodeId), [[f64; 2]; 2]>,
    /// Optional per-`(ClockId, NodeId)` network latency override `[an][edge]`.
    pin_network_latency_ns: FxHashMap<(ClockId, NodeId), [[f64; 2]; 2]>,
}

impl IdealClockModel {
    /// Builds the [`IdealClockModel`] for `graph` and `constraints` (`STA-REQ-0200`,
    /// `STA-REQ-0217`).
    #[must_use]
    pub fn build(graph: &TimingGraph, constraints: &CompiledConstraints) -> Self {
        let n_nodes = graph.num_nodes();
        let n_clocks = constraints.clocks.len();
        let g_arcs = graph.arcs();

        // 1. Trace positive/negative clock sense bits from each clock's source nodes across
        // enabled net and combinational arcs, skipping latch D -> Q arcs (`STA-REQ-0200`,
        // `STA-REQ-0217`).
        let mut clock_sense: Vec<Vec<(ClockId, SenseBits)>> = vec![Vec::new(); n_nodes];
        for (ci, ck) in constraints.clocks.iter().enumerate() {
            let cid = ClockId(ci as u32);
            let mut bits = vec![0u8; n_nodes];
            let mut stack = Vec::new();
            for &src in &ck.sources {
                if src.index() < n_nodes {
                    bits[src.index()] |= SENSE_POS;
                    stack.push((src, SENSE_POS));
                }
            }
            while let Some((v, b)) = stack.pop() {
                for &ai in graph.outgoing(v) {
                    let arc = &g_arcs[ai as usize];
                    if arc.kind.is_latch_data() {
                        continue;
                    }
                    let nb = match arc.kind {
                        ArcKind::Net { .. } => b,
                        _ => match arc.sense {
                            ArcSense::PositiveUnate => b,
                            ArcSense::NegativeUnate => ((b & 1) << 1) | ((b >> 1) & 1),
                            ArcSense::NonUnate => SENSE_BOTH,
                        },
                    };
                    let w = arc.dst.index();
                    if (bits[w] | nb) != bits[w] {
                        bits[w] |= nb;
                        stack.push((arc.dst, bits[w]));
                    }
                }
            }
            for (v, &b) in bits.iter().enumerate() {
                if b != 0 {
                    clock_sense[v].push((cid, b));
                }
            }
        }

        // 2. Extract per-clock source latency, network latency, and transition tables.
        let mut source_latency_ns = Vec::with_capacity(n_clocks);
        let mut network_latency_ns = Vec::with_capacity(n_clocks);
        let mut transition_ns = Vec::with_capacity(n_clocks);
        for ck in &constraints.clocks {
            let mut src_lat = [[0.0; 2]; 2];
            for an in An::ALL {
                let ai = an.index();
                for edge in Tr::ALL {
                    let ei = edge.index();
                    src_lat[ai][ei] = ck.source_latency_ns[ai][ai][ei];
                }
            }
            source_latency_ns.push(src_lat);
            network_latency_ns.push(ck.network_latency_ns);
            transition_ns.push(ck.transition_ns);
        }

        // 3. Apply any `ClockLatencyTarget::Node` overrides: if the target node is a source of a
        // clock, update that clock's latency; otherwise record a per-pin latency override on
        // clocks reaching that pin.
        let mut pin_source_latency_ns: FxHashMap<(ClockId, NodeId), [[f64; 2]; 2]> =
            FxHashMap::default();
        let mut pin_network_latency_ns: FxHashMap<(ClockId, NodeId), [[f64; 2]; 2]> =
            FxHashMap::default();

        for spec in &constraints.clock_latencies {
            let ClockLatencyTarget::Node(node) = spec.target else {
                continue;
            };
            let ans = min_max_indices(spec.min_max);
            let trs = edge_indices(spec.edge);
            let els = early_late_indices(spec.early_late);

            let mut matched_source = false;
            for (ci, ck) in constraints.clocks.iter().enumerate() {
                let cid = ClockId(ci as u32);
                if spec.clock.is_some_and(|c| c != cid) {
                    continue;
                }
                if ck.sources.contains(&node) {
                    matched_source = true;
                    if spec.source {
                        for &el in &els {
                            for &an in &ans {
                                if el == an {
                                    for &tr in &trs {
                                        source_latency_ns[ci][an][tr] = spec.delay_ns;
                                    }
                                }
                            }
                        }
                    } else {
                        for &an in &ans {
                            for &tr in &trs {
                                network_latency_ns[ci][an][tr] = spec.delay_ns;
                            }
                        }
                    }
                }
            }
            if !matched_source && node.index() < n_nodes {
                for &(cid, _) in &clock_sense[node.index()] {
                    if spec.clock.is_some_and(|c| c != cid) {
                        continue;
                    }
                    let ci = cid.index();
                    if spec.source {
                        let entry = pin_source_latency_ns
                            .entry((cid, node))
                            .or_insert(source_latency_ns[ci]);
                        for &el in &els {
                            for &an in &ans {
                                if el == an {
                                    for &tr in &trs {
                                        entry[an][tr] = spec.delay_ns;
                                    }
                                }
                            }
                        }
                    } else {
                        let entry = pin_network_latency_ns
                            .entry((cid, node))
                            .or_insert(network_latency_ns[ci]);
                        for &an in &ans {
                            for &tr in &trs {
                                entry[an][tr] = spec.delay_ns;
                            }
                        }
                    }
                }
            }
        }

        Self {
            clock_sense,
            source_latency_ns,
            network_latency_ns,
            transition_ns,
            pin_source_latency_ns,
            pin_network_latency_ns,
        }
    }

    /// Returns the `(ClockId, SenseBits)` pairs reaching `node`.
    #[inline]
    #[must_use]
    pub fn node_clocks(&self, node: NodeId) -> &[(ClockId, SenseBits)] {
        self.clock_sense
            .get(node.index())
            .map_or(&[], Vec::as_slice)
    }

    /// Source latency (ns) of `clock` at source edge `edge` in bound `an`.
    #[inline]
    #[must_use]
    pub fn source_latency(&self, clock: ClockId, edge: Tr, an: An) -> f64 {
        self.source_latency_ns
            .get(clock.index())
            .map_or(0.0, |t| t[an.index()][edge.index()])
    }

    /// Network latency (ns) of `clock` at source edge `edge` in bound `an`.
    #[inline]
    #[must_use]
    pub fn network_latency(&self, clock: ClockId, edge: Tr, an: An) -> f64 {
        self.network_latency_ns
            .get(clock.index())
            .map_or(0.0, |t| t[an.index()][edge.index()])
    }

    /// Total clock latency `lat_src + lat_net` (ns) of `clock` at source edge `edge` in bound `an`.
    #[inline]
    #[must_use]
    pub fn clock_latency(&self, clock: ClockId, edge: Tr, an: An) -> f64 {
        self.source_latency(clock, edge, an) + self.network_latency(clock, edge, an)
    }

    /// Clock latency contributed to a `set_input_delay` or `set_output_delay` (`STA-REQ-0218`):
    /// omits `lat_src` when `source_latency_included` is true and omits `lat_net` when
    /// `network_latency_included` is true.
    #[inline]
    #[must_use]
    pub fn io_latency(
        &self,
        clock: ClockId,
        edge: Tr,
        an: An,
        source_latency_included: bool,
        network_latency_included: bool,
    ) -> f64 {
        let src = if source_latency_included {
            0.0
        } else {
            self.source_latency(clock, edge, an)
        };
        let net = if network_latency_included {
            0.0
        } else {
            self.network_latency(clock, edge, an)
        };
        src + net
    }

    /// Ideal clock transition (slew in ns) of `clock` for transition `tr` in bound `an`.
    #[inline]
    #[must_use]
    pub fn clock_transition(&self, clock: ClockId, tr: Tr, an: An) -> f64 {
        self.transition_ns
            .get(clock.index())
            .map_or(0.0, |t| t[an.index()][tr.index()])
    }

    /// Resolves the clock uncertainty (ns) for `(launch_clock, launch_edge)` captured by
    /// `(capture_clock, capture_edge)` at optional endpoint/clock pin `pin` (`STA-REQ-0220`):
    /// inter-clock `set_clock_uncertainty -from ... -to ...` overrides per-pin and per-capture-clock
    /// `set_clock_uncertainty`.
    #[must_use]
    pub fn uncertainty(
        &self,
        constraints: &CompiledConstraints,
        launch_clock: ClockId,
        launch_edge: Tr,
        capture_clock: ClockId,
        capture_edge: Tr,
        pin: Option<NodeId>,
        is_setup: bool,
    ) -> f64 {
        let mut inter_match = None;
        for u in &constraints.inter_clock_uncertainties {
            let sh_ok = match u.setup_hold {
                SetupHoldScope::Setup => is_setup,
                SetupHoldScope::Hold => !is_setup,
                SetupHoldScope::Both => true,
            };
            if sh_ok
                && u.from_clock == launch_clock
                && edge_scope_matches(u.from_edge, launch_edge)
                && u.to_clock == capture_clock
                && edge_scope_matches(u.to_edge, capture_edge)
            {
                inter_match = Some(u.uncertainty_ns);
            }
        }
        if let Some(val) = inter_match {
            return val;
        }
        if let Some(p) = pin
            && let Some(pu) = constraints
                .pin_clock_uncertainties
                .iter()
                .find(|x| x.node == p)
        {
            if is_setup {
                if let Some(v) = pu.setup_ns {
                    return v;
                }
            } else if let Some(v) = pu.hold_ns {
                return v;
            }
        }
        let Some(ck) = constraints.clocks.get(capture_clock.index()) else {
            return 0.0;
        };
        if is_setup {
            ck.uncertainty_setup_ns[capture_edge.index()]
        } else {
            ck.uncertainty_hold_ns[capture_edge.index()]
        }
    }
}

impl ClockModel for IdealClockModel {
    fn scope_name(&self) -> &'static str {
        "ideal"
    }

    fn arrival(
        &self,
        clock: ClockId,
        edge: Tr,
        pin: NodeId,
        tr: Tr,
        an: An,
    ) -> Option<ClockArrival> {
        let senses = self.clock_sense.get(pin.index())?;
        let &(_, bits) = senses.iter().find(|&&(c, _)| c == clock)?;
        let reaches = if edge == tr {
            bits & SENSE_POS != 0
        } else {
            bits & SENSE_NEG != 0
        };
        if !reaches {
            return None;
        }
        let ai = an.index();
        let ei = edge.index();
        let src_lat = self
            .pin_source_latency_ns
            .get(&(clock, pin))
            .map_or_else(|| self.source_latency(clock, edge, an), |t| t[ai][ei]);
        let net_lat = self
            .pin_network_latency_ns
            .get(&(clock, pin))
            .map_or_else(|| self.network_latency(clock, edge, an), |t| t[ai][ei]);
        let slew = self.clock_transition(clock, tr, an);
        Some(ClockArrival {
            latency: src_lat + net_lat,
            slew,
        })
    }

    fn pin_clocks(&self, pin: NodeId) -> &[(ClockId, SenseBits)] {
        self.node_clocks(pin)
    }
}

#[inline]
fn edge_scope_matches(scope: EdgeScope, edge: Tr) -> bool {
    match scope {
        EdgeScope::Rise => edge == Tr::Rise,
        EdgeScope::Fall => edge == Tr::Fall,
        EdgeScope::Both => true,
    }
}

fn min_max_indices(mm: MinMaxScope) -> Vec<usize> {
    match mm {
        MinMaxScope::Min => vec![An::Early.index()],
        MinMaxScope::Max => vec![An::Late.index()],
        MinMaxScope::Both => vec![An::Early.index(), An::Late.index()],
    }
}

fn edge_indices(e: EdgeScope) -> Vec<usize> {
    match e {
        EdgeScope::Rise => vec![Tr::Rise.index()],
        EdgeScope::Fall => vec![Tr::Fall.index()],
        EdgeScope::Both => vec![Tr::Rise.index(), Tr::Fall.index()],
    }
}

fn early_late_indices(el: EarlyLateScope) -> Vec<usize> {
    match el {
        EarlyLateScope::Early => vec![An::Early.index()],
        EarlyLateScope::Late => vec![An::Late.index()],
        EarlyLateScope::Both => vec![An::Early.index(), An::Late.index()],
    }
}
