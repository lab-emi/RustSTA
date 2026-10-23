// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Exception-aware timing propagation and endpoint slack evaluator (`STA-REQ-0250` to
//! `STA-REQ-0270`, owner: E8).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use rusteda_compute::timing::{ArcClass, LutDesc, NONE};
use rusteda_core::diag::{Diag, Severity};
use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::FrozenNetlist;
use rusteda_formats::sdc::{Env, Limits, Sdc, Source};

use super::check::evaluate_all_endpoints;
use super::helpers::{
    canonical_net_totals, clock_latency, clock_uncertainty, io_clock_latency, map_cell_arcs,
    max_time_borrow_for_latch, port_input_slew, propagate_clock_sense, setup_relation_ns,
    source_edges,
};
use super::resolve::{
    AnalysisKind, EndpointExceptionResult, EndpointStatus, ExceptionScopeSummary, ExceptionTable,
    PathAnalysisResolution,
};
use super::state::{
    DEFAULT_NODE_EXCEPTION_STATE_BUDGET, ExceptionStateTable, PropagationTagKey,
    PropagationTagTable, StartLaunchKey, check_node_exception_state_budget,
};
use crate::engine::delay::gate::GateDelay;
use crate::engine::delay::load::{NetLoadOverrides, NetLoads};
use crate::engine::delay::wire::{DriverLoad, Ideal};
use crate::engine::graph::constants::ConstantPropagation;
use crate::engine::graph::{ArcKind, TimingGraph};
use crate::engine::input::constr::{CompiledConstraints, CornerLibs, DesignIndex};
use crate::engine::input::libview::TimingLib;
use crate::engine::input::{NetTarget, NetlistView, NodeKind};
use crate::engine::prop::lower::{
    ArcTagMaps, BackendKind, GraphStartBuilder, LoweredCorner, Precision, TagMapRec, TimingValues,
};
use crate::engine::types::{An, ClockId, LibArc, NodeId, Tr};

/// Options for exception-aware propagation and endpoint evaluation (`STA-REQ-0269`, `STA-REQ-0400`).
#[derive(Debug, Clone)]
pub struct ExceptionEvalOptions {
    /// Maximum number of distinct [`super::state::ExceptionState`]s allowed at any single node
    /// (`>= 64` by default; `STA-REQ-0269`).
    pub max_states_per_node: usize,
    /// Compute backend (`Cpu`, `Cuda`, `Gpu`).
    pub backend: BackendKind,
    /// Numerical precision (`F64` or `F32`).
    pub precision: Precision,
}

impl Default for ExceptionEvalOptions {
    fn default() -> Self {
        Self {
            max_states_per_node: DEFAULT_NODE_EXCEPTION_STATE_BUDGET,
            backend: BackendKind::Cpu,
            precision: Precision::F64,
        }
    }
}

/// Complete output of evaluating timing exceptions on a design and constraint set
/// (`STA-REQ-0250` to `STA-REQ-0270`).
#[derive(Debug, Clone)]
pub struct ExceptionEvaluation {
    /// All potential endpoints in canonical order with their setup/hold slacks and status.
    pub endpoints: Vec<EndpointExceptionResult>,
    /// Lookup map of endpoint results by canonical pin/port name.
    pub by_pin: BTreeMap<String, EndpointExceptionResult>,
    /// Overall setup worst negative slack (`<= 0.0`).
    pub wns_setup: f64,
    /// Overall setup total negative slack (`<= 0.0`).
    pub tns_setup: f64,
    /// Number of endpoints with negative setup slack.
    pub violating_setup: usize,
    /// Overall hold worst negative slack (`<= 0.0`).
    pub wns_hold: f64,
    /// Overall hold total negative slack (`<= 0.0`).
    pub tns_hold: f64,
    /// Number of endpoints with negative hold slack.
    pub violating_hold: usize,
    /// All diagnostics emitted during constraint compilation and exception evaluation
    /// (`D0102`, `D0110`..`D0114`, etc.).
    pub diags: Vec<Diag>,
    /// Run scope counts for exceptions, clock groups, unused exceptions, and unconstrained
    /// endpoints (`STA-REQ-0270`).
    pub scope: ExceptionScopeSummary,
    /// Propagated per-node [`TimingValues`] in [`NodeId`] order.
    pub values: TimingValues,
    /// Total number of propagation tags lowered into the compute task.
    pub ntags: u32,
    /// Total number of distinct interned [`super::state::ExceptionState`]s.
    pub num_exception_states: usize,
    /// Maximum number of distinct [`super::state::ExceptionState`]s observed at any single node.
    pub max_node_exception_states: usize,
}

impl ExceptionEvaluation {
    /// Returns `(setup_slack, hold_slack)` for endpoint `pin`, or `(None, None)` if not found.
    #[inline]
    #[must_use]
    pub fn slack(&self, pin: &str) -> (Option<f64>, Option<f64>) {
        self.by_pin
            .get(pin)
            .map_or((None, None), |e| (e.setup_slack, e.hold_slack))
    }

    /// Returns the [`EndpointStatus`] for endpoint `pin`, if present.
    #[inline]
    #[must_use]
    pub fn status(&self, pin: &str) -> Option<EndpointStatus> {
        self.by_pin.get(pin).map(|e| e.status)
    }

    /// Returns true if any diagnostic has `code`.
    #[inline]
    #[must_use]
    pub fn has_diag(&self, code: &str) -> bool {
        self.diags.iter().any(|d| d.code == code)
    }
}

/// Evaluates an SDC script `sdc_text` (named `sdc_name`) against `netlist`, `libs`, and `tlib`
/// using `opts` (`STA-REQ-0250` to `STA-REQ-0270`).
pub fn evaluate_exceptions(
    netlist: &FrozenNetlist,
    libs: &[Library],
    tlib: &TimingLib,
    sdc_text: &str,
    sdc_name: &str,
    opts: &ExceptionEvalOptions,
) -> Result<ExceptionEvaluation, Diag> {
    let src_name = if sdc_name.is_empty() {
        "<sdc>"
    } else {
        sdc_name
    };
    let sources = [Source::new(src_name, sdc_text)];
    let (sdc, _) = rusteda_formats::sdc::read(&sources, &Env::default(), &Limits::default());
    let corners = [CornerLibs::new("default", libs)];
    let mut compiled = CompiledConstraints::compile_lenient(&sdc, netlist, &corners);
    let sdc_lines: Vec<&str> = sdc_text.lines().collect();
    compiled.diags.retain(|d| {
        if d.code != "D0113" {
            return true;
        }
        d.location
            .as_ref()
            .and_then(|loc| sdc_lines.get(loc.line.saturating_sub(1) as usize))
            .is_none_or(|line| line.contains("-setup") && line.contains("-hold"))
    });
    if let Some(err) = compiled
        .diags
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(err.clone());
    }
    evaluate_compiled_exceptions(netlist, libs, tlib, &compiled, Some(&sdc), opts)
}

/// Evaluates pre-compiled [`CompiledConstraints`] against `netlist`, `libs`, and `tlib` using
/// `opts` (`STA-REQ-0250` to `STA-REQ-0270`).
pub fn evaluate_compiled_exceptions(
    netlist: &FrozenNetlist,
    libs: &[Library],
    tlib: &TimingLib,
    compiled: &CompiledConstraints,
    sdc: Option<&Sdc>,
    opts: &ExceptionEvalOptions,
) -> Result<ExceptionEvaluation, Diag> {
    let nv = NetlistView::new(netlist, libs)
        .map_err(|e| Diag::error(e.code(), format!("netlist view error: {e}")))?;
    let mut graph = TimingGraph::build(&nv, libs)
        .map_err(|e| Diag::error(e.code(), format!("graph build error: {e}")))?;
    ConstantPropagation::apply(&nv, libs, Some(compiled), &mut graph);
    let corners = [CornerLibs::new("default", libs)];
    let mut index = DesignIndex::build(netlist, &corners);
    let all_clk_srcs: Vec<NodeId> = compiled
        .clocks
        .iter()
        .flat_map(|c| c.sources.iter().copied())
        .collect();
    index.recompute_inferred_clock_gating(&all_clk_srcs);

    // 1. Compute net loads (including `set_load` port and net overrides).
    let mut load_overrides = NetLoadOverrides::default();
    for (&nid, pl) in &compiled.port_loads {
        load_overrides.by_port_bit.insert(nid.0, pl.load_pf);
    }
    for (&nr, nl) in &compiled.net_loads {
        if nl.pin_load && !nl.wire_load {
            load_overrides.add_net_pin_load(nr, nl.load_pf[0], nl.load_pf[1]);
        } else {
            load_overrides.add_net_wire_load(nr, nl.load_pf[0], nl.load_pf[1]);
        }
    }
    let net_loads = NetLoads::compute(netlist, tlib, &Ideal, &load_overrides);
    let canon_totals = canonical_net_totals(&nv, &net_loads);
    let cell_arcs = map_cell_arcs(&nv, tlib)?;

    // 2. Propagate ideal clock sense bits across enabled net and combinational arcs, skipping
    // latch D -> Q arcs (`STA-REQ-0200`).
    let clock_sense = propagate_clock_sense(&graph, compiled);

    // 3. Build exception table, hash-consed state table, and propagation tag table.
    let mut state_table = ExceptionStateTable::new();
    let mut tag_table = PropagationTagTable::new();
    let mut exc_table = ExceptionTable::from_compiled_with_sdc(compiled, sdc);

    let n_nodes = graph.num_nodes();
    let mut seeded_node_states = Vec::new();

    // 3a. Discover sequential and latch launches (`STA-REQ-0217`, `STA-REQ-0251`, `STA-REQ-0267`).
    struct LaunchSeed {
        inst: u32,
        clock_pin: NodeId,
        out_pin: NodeId,
        global_arc: LibArc,
        c_id: ClockId,
        src_edge: Tr,
        tag: u32,
        launch_lat: [f64; 2],
    }
    let mut launch_seeds = Vec::new();
    let mut latch_open_tags: BTreeMap<(u32, NodeId, NodeId, ClockId, Tr), u32> = BTreeMap::new();
    let mut latch_gq_early_slew: BTreeMap<(u32, NodeId), [f64; 2]> = BTreeMap::new();

    for l in graph.launches() {
        let cell_row = nv.frozen().instances[l.inst as usize].cell as usize;
        let global_arc = cell_arcs[cell_row][l.lib_arc.index()];
        for &(c_id, bits) in &clock_sense[l.clock_pin.index()] {
            for src_edge in source_edges(l.edge, bits) {
                let start_key = StartLaunchKey {
                    start_node: l.clock_pin,
                    start_tr: l.edge,
                    launch_clock: Some((c_id, src_edge)),
                };
                let exc_state = exc_table.match_start(&mut state_table, start_key);
                let launch_lat = [
                    clock_latency(compiled, c_id, src_edge, An::Early, Some(l.clock_pin)),
                    clock_latency(compiled, c_id, src_edge, An::Late, Some(l.clock_pin)),
                ];
                let tag = tag_table.intern(PropagationTagKey::new(
                    Some((c_id, src_edge)),
                    exc_state,
                    launch_lat,
                ));
                seeded_node_states.push((l.out_pin, exc_state));
                latch_open_tags.insert((l.inst, l.clock_pin, l.out_pin, c_id, src_edge), tag);
                launch_seeds.push(LaunchSeed {
                    inst: l.inst,
                    clock_pin: l.clock_pin,
                    out_pin: l.out_pin,
                    global_arc,
                    c_id,
                    src_edge,
                    tag,
                    launch_lat,
                });
            }
        }
    }

    // 3b. Discover clocked input delays (`STA-REQ-0217`, `STA-REQ-0219`).
    struct InputDelaySeed {
        node: NodeId,
        tr: Tr,
        tag: u32,
        arr: [Option<f64>; 2],
        slew: [f64; 2],
    }
    let mut input_delay_seeds = Vec::new();
    let mut ports_with_input_delay = BTreeSet::new();

    for d in &compiled.input_delays {
        if d.node.index() >= n_nodes {
            continue;
        }
        let Some(c_id) = d.clock else {
            continue;
        };
        ports_with_input_delay.insert(d.node);
        let src_edge = d.clock_edge;
        let launch_lat = [
            io_clock_latency(compiled, d, c_id, src_edge, An::Early),
            io_clock_latency(compiled, d, c_id, src_edge, An::Late),
        ];
        for tr in Tr::ALL {
            let e_raw = d.delay_ns[An::Early.index()][tr.index()];
            let l_raw = d.delay_ns[An::Late.index()][tr.index()];
            if e_raw.is_none() && l_raw.is_none() {
                continue;
            }
            let start_key = StartLaunchKey {
                start_node: d.node,
                start_tr: tr,
                launch_clock: Some((c_id, src_edge)),
            };
            let exc_state = exc_table.match_start(&mut state_table, start_key);
            let tag = tag_table.intern(PropagationTagKey::new(
                Some((c_id, src_edge)),
                exc_state,
                launch_lat,
            ));
            seeded_node_states.push((d.node, exc_state));
            let slew = port_input_slew(compiled, d.node, tr);
            input_delay_seeds.push(InputDelaySeed {
                node: d.node,
                tr,
                tag,
                arr: [
                    e_raw.map(|v| v + launch_lat[0]),
                    l_raw.map(|v| v + launch_lat[1]),
                ],
                slew,
            });
        }
    }

    // 3c. Discover unclocked input ports (`STA-REQ-0264`, `STA-REQ-0115`).
    let clock_source_set: BTreeSet<NodeId> = all_clk_srcs.iter().copied().collect();
    let mut unclocked_input_seeds: Vec<(NodeId, Tr, u32, [f64; 2])> = Vec::new();
    let mut untagged_slews: Vec<(NodeId, Tr, [f64; 2])> = Vec::new();

    for i in 0..nv.num_port_bits() {
        let node = NodeId(i as u32);
        let NodeKind::PortBit { dir, .. } = nv.node_kind(node) else {
            continue;
        };
        if !matches!(dir, Direction::Input | Direction::Inout) {
            continue;
        }
        if clock_source_set.contains(&node) || ports_with_input_delay.contains(&node) {
            continue;
        }
        let timed_by_delay_exc = exc_table.has_unclocked_from_delay_exception(node);
        for tr in Tr::ALL {
            let slew = port_input_slew(compiled, node, tr);
            untagged_slews.push((node, tr, slew));
            if timed_by_delay_exc {
                let start_key = StartLaunchKey {
                    start_node: node,
                    start_tr: tr,
                    launch_clock: None,
                };
                let exc_state = exc_table.match_start(&mut state_table, start_key);
                let tag = tag_table.intern(PropagationTagKey::new(None, exc_state, [0.0, 0.0]));
                seeded_node_states.push((node, exc_state));
                unclocked_input_seeds.push((node, tr, tag, slew));
            }
        }
    }

    // Ensure at least one tag slot exists for `rusteda_compute::timing` (`ntags >= 1`).
    if tag_table.is_empty() {
        tag_table.intern(PropagationTagKey::new(
            None,
            super::state::ExceptionState::EMPTY,
            [0.0, 0.0],
        ));
    }
    let ntags = tag_table.len() as u32;

    // 4. Enforce per-node exception state budget (`STA-REQ-0269`).
    let max_node_exception_states = check_node_exception_state_budget(
        &graph,
        &index,
        &seeded_node_states,
        opts.max_states_per_node,
    )?;

    // 5. Populate `GraphStartBuilder`.
    let mut starts = GraphStartBuilder::new(ntags);

    for &(node, tr, slew) in &untagged_slews {
        for an in An::ALL {
            starts.merge_untagged_slew(&graph, node, tr, an, slew[an.index()]);
        }
    }

    for &(node, tr, tag, slew) in &unclocked_input_seeds {
        for an in An::ALL {
            starts.merge_bound(&graph, node, tag, tr, an, 0.0, slew[an.index()]);
        }
    }

    for seed in &input_delay_seeds {
        for an in An::ALL {
            if let Some(a) = seed.arr[an.index()] {
                starts.merge_bound(
                    &graph,
                    seed.node,
                    seed.tag,
                    seed.tr,
                    an,
                    a,
                    seed.slew[an.index()],
                );
            } else {
                starts.merge_untagged_slew(&graph, seed.node, seed.tr, an, seed.slew[an.index()]);
            }
        }
    }

    for seed in &launch_seeds {
        let ck = &compiled.clocks[seed.c_id.index()];
        let net_cap = match nv.node_net(seed.out_pin) {
            NetTarget::Net(nr) => canon_totals.get(nr.index()).copied().unwrap_or_default(),
            NetTarget::Const(_) => [[0.0; 2]; 2],
        };
        let mut early_q_slew = [0.0; 2];
        for to in Tr::ALL {
            for an in An::ALL {
                let c_load = net_cap[to.index()][an.index()];
                let clk_slew = ck.transition_ns[an.index()][seed.src_edge.index()];
                let ds = tlib.arc_delay(
                    seed.global_arc,
                    to,
                    an,
                    clk_slew,
                    DriverLoad::Lumped { c: c_load },
                );
                if an == An::Early {
                    early_q_slew[to.index()] = ds.slew;
                }
                starts.merge_bound(
                    &graph,
                    seed.out_pin,
                    seed.tag,
                    to,
                    an,
                    seed.launch_lat[an.index()] + ds.delay,
                    ds.slew,
                );
            }
        }
        latch_gq_early_slew
            .entry((seed.inst, seed.out_pin))
            .or_insert(early_q_slew);
        let _ = seed.clock_pin;
    }

    // 6. Build latch D -> Q `ArcTagMaps` (`STA-REQ-0221`, `STA-REQ-0222`, `STA-REQ-0267`).
    let mut tag_maps = ArcTagMaps::new();
    let g_arcs = graph.arcs();
    let g_checks = graph.checks();
    let g_launches = graph.launches();
    let mut preview_table = exc_table.clone();

    for (ai, arc) in g_arcs.iter().enumerate() {
        if graph.is_arc_disabled(ai) {
            continue;
        }
        let ArcKind::LatchData {
            inst, setup_check, ..
        } = arc.kind
        else {
            continue;
        };
        let Some(ch) = g_checks.get(setup_check as usize) else {
            continue;
        };
        let open_pin_edge = g_launches
            .iter()
            .find(|l| l.inst == inst && l.clock_pin == ch.clock_pin && l.out_pin == arc.dst)
            .map_or_else(|| ch.edge.opposite(), |l| l.edge);

        let mut recs = Vec::new();
        for &(c_id, bits) in &clock_sense[ch.clock_pin.index()] {
            let ck_c = &compiled.clocks[c_id.index()];
            let max_borrow_eff =
                max_time_borrow_for_latch(compiled, inst, ch.data_pin, ch.clock_pin, c_id);
            let close_edges = source_edges(ch.edge, bits);
            let open_edges = source_edges(open_pin_edge, bits);
            let Some(&e_close) = close_edges.first() else {
                continue;
            };
            let Some(&e_open) = open_edges.first() else {
                continue;
            };
            let Some(&dst_tag) = latch_open_tags.get(&(inst, ch.clock_pin, arc.dst, c_id, e_open))
            else {
                continue;
            };
            let Some(w_pulse) = setup_relation_ns(
                ck_c.period_ns,
                ck_c.waveform_ns,
                e_open,
                ck_c.period_ns,
                ck_c.waveform_ns,
                e_close,
            ) else {
                continue;
            };
            let close_slew = [
                ck_c.transition_ns[An::Early.index()][e_close.index()],
                ck_c.transition_ns[An::Late.index()][e_close.index()],
            ];

            for (src_tag_idx, src_key) in tag_table.tags().iter().enumerate() {
                let Some((l_id, e_l)) = src_key.launch_clock else {
                    continue;
                };
                let ck_l = &compiled.clocks[l_id.index()];
                let res_r = preview_table.resolve_path_analysis(
                    &state_table,
                    src_key.exc_state,
                    Some((l_id, e_l)),
                    ch.data_pin,
                    Tr::Rise,
                    Some((c_id, e_close)),
                    AnalysisKind::Setup,
                );
                let res_f = preview_table.resolve_path_analysis(
                    &state_table,
                    src_key.exc_state,
                    Some((l_id, e_l)),
                    ch.data_pin,
                    Tr::Fall,
                    Some((c_id, e_close)),
                    AnalysisKind::Setup,
                );
                let setup_winner = match (res_r, res_f) {
                    (PathAnalysisResolution::TimedSetupMulticycle { setup }, _)
                    | (_, PathAnalysisResolution::TimedSetupMulticycle { setup }) => setup,
                    _ => {
                        continue;
                    }
                };
                let Some(d_close_base) = setup_relation_ns(
                    ck_l.period_ns,
                    ck_l.waveform_ns,
                    e_l,
                    ck_c.period_ns,
                    ck_c.waveform_ns,
                    e_close,
                ) else {
                    continue;
                };
                let d_open_base = d_close_base - w_pulse;
                let delta_s = setup_winner.shift_ns(ck_l.period_ns, ck_c.period_ns);
                let d_open = d_open_base + delta_s;
                let d_close = d_close_base + delta_s;
                let lat_c_early =
                    clock_latency(compiled, c_id, e_close, An::Early, Some(ch.clock_pin));
                let u_s = clock_uncertainty(
                    compiled,
                    l_id,
                    e_l,
                    c_id,
                    e_close,
                    AnalysisKind::Setup,
                    Some(ch.clock_pin),
                );
                let open_rel = (lat_c_early - u_s).next_up();
                let close_rel = (d_close - d_open) + lat_c_early - u_s;
                let limit_rel = match max_borrow_eff {
                    Some(b) => (lat_c_early - u_s) + b,
                    None => f64::INFINITY,
                };
                recs.push(TagMapRec {
                    src_tag: src_tag_idx as u32,
                    dst_tag,
                    shift: -d_open,
                    open: [f64::MAX, open_rel],
                    close: [f64::INFINITY, close_rel],
                    close_slew,
                    limit: [f64::INFINITY, limit_rel],
                });
            }
        }

        if recs.is_empty() {
            // Ensure `map != NONE` on the lowered CSR arc so that a blocked latch D -> Q arc does
            // not fall back to the identity tag map (`COMPUTE-REQ-0011`, `STA-REQ-0267`).
            recs.push(TagMapRec {
                src_tag: 0,
                dst_tag: 0,
                shift: 0.0,
                open: [f64::MAX, f64::MAX],
                close: [f64::INFINITY, f64::INFINITY],
                close_slew: [0.0, 0.0],
                limit: [f64::INFINITY, f64::INFINITY],
            });
        }
        tag_maps.insert(ai as u32, recs);
    }

    // 7. Lower onto `LoweredCorner`, pin latch Q slews to G -> Q slews (`STA-REQ-0222`), and
    // run forward propagation (`STA-REQ-0400`).
    let mut lowered = LoweredCorner::lower(
        &nv,
        &graph,
        tlib,
        &net_loads,
        ntags,
        starts.build(),
        &tag_maps,
    )?;
    pin_latch_data_arc_slews(&graph, &mut lowered, &latch_gq_early_slew);

    let prop_res = lowered
        .propagate(opts.backend, opts.precision, Ctx::NONE)
        .map_err(|e| crate::engine::prop::lower::compute_error_diag(&e))?;

    // 8. Evaluate all endpoints (`STA-REQ-0250` to `STA-REQ-0270`).
    let endpoints = evaluate_all_endpoints(
        &nv,
        &graph,
        &index,
        tlib,
        compiled,
        &clock_sense,
        &canon_totals,
        &state_table,
        &tag_table,
        &mut exc_table,
        &prop_res,
    );

    let (diags, scope) = exc_table.finalize_diagnostics_and_scope(&compiled.diags, &endpoints);

    let mut by_pin = BTreeMap::new();
    let mut wns_setup = 0.0f64;
    let mut tns_setup = 0.0f64;
    let mut violating_setup = 0usize;
    let mut wns_hold = 0.0f64;
    let mut tns_hold = 0.0f64;
    let mut violating_hold = 0usize;

    for ep in &endpoints {
        if let Some(s) = ep.setup_slack
            && s < 0.0
        {
            wns_setup = wns_setup.min(s);
            tns_setup += s;
            violating_setup += 1;
        }
        if let Some(h) = ep.hold_slack
            && h < 0.0
        {
            wns_hold = wns_hold.min(h);
            tns_hold += h;
            violating_hold += 1;
        }
        by_pin.insert(ep.pin.clone(), ep.clone());
    }

    Ok(ExceptionEvaluation {
        endpoints,
        by_pin,
        wns_setup,
        tns_setup,
        violating_setup,
        wns_hold,
        tns_hold,
        violating_hold,
        diags,
        scope,
        values: prop_res.to_node_order(&graph),
        ntags,
        num_exception_states: state_table.len(),
        max_node_exception_states,
    })
}

/// Pins the `slew` tables of `LatchData` `ArcClass` descriptors in `lowered` to the `Early`
/// `G -> Q` slews at `Q` so that `D -> Q` never overwrites `slew[Q][tr][an]` (`STA-REQ-0222`).
fn pin_latch_data_arc_slews(
    graph: &TimingGraph,
    lowered: &mut LoweredCorner<'_>,
    latch_gq_early_slew: &BTreeMap<(u32, NodeId), [f64; 2]>,
) {
    if latch_gq_early_slew.is_empty() {
        return;
    }
    let g_arcs = graph.arcs();
    let mut owned_luts: Option<Vec<LutDesc>> = None;
    let mut owned_data: Option<Vec<f64>> = None;
    let mut owned_classes: Option<Vec<ArcClass>> = None;

    for (ai, arc) in g_arcs.iter().enumerate() {
        let ArcKind::LatchData { inst, .. } = arc.kind else {
            continue;
        };
        let csr_idx = lowered.graph_arc_to_csr_arc[ai];
        if csr_idx == NONE {
            continue;
        }
        let Some(&[s_r, s_f]) = latch_gq_early_slew.get(&(inst, arc.dst)) else {
            continue;
        };
        let luts_vec = owned_luts.get_or_insert_with(|| lowered.luts.luts.to_vec());
        let data_vec = owned_data.get_or_insert_with(|| lowered.luts.data.to_vec());
        let classes_vec = owned_classes.get_or_insert_with(|| lowered.luts.classes.to_vec());

        let old_class_idx = lowered.csr.in_arcs[csr_idx as usize].class as usize;
        let old_class = classes_vec[old_class_idx];

        let mut new_slew = [NONE; 2];
        for (ti, val) in [s_r, s_f].into_iter().enumerate() {
            let voff = data_vec.len() as u32;
            data_vec.push(val);
            let lut_idx = luts_vec.len() as u32;
            luts_vec.push(LutDesc {
                n: [0, 0, 0],
                off: [0, 0, 0],
                arg: [0, 0, 0],
                voff,
            });
            new_slew[ti] = lut_idx;
        }

        let new_class_idx = classes_vec.len() as u32;
        classes_vec.push(ArcClass {
            flags: old_class.flags,
            delay: old_class.delay,
            slew: new_slew,
            setup: old_class.setup,
        });
        lowered.csr.in_arcs.to_mut()[csr_idx as usize].class = new_class_idx;
    }

    if let Some(l) = owned_luts {
        lowered.luts.luts = Cow::Owned(l);
    }
    if let Some(d) = owned_data {
        lowered.luts.data = Cow::Owned(d);
    }
    if let Some(c) = owned_classes {
        lowered.luts.classes = Cow::Owned(c);
    }
}
