// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Preparation and propagation of individual analysis corners.

use super::{
    AnalysisOptions, ArcTagMaps, BackwardPass, CompiledConstraints, ConstantPropagation,
    CornerAnalysis, CornerEndpoints, CornerId, CornerLibs, Ctx, Diag, DriveCellEffect,
    ExceptionContext, FrozenNetlist, FxHashMap, Ideal, IdealClockModel, KeepLowered,
    LatchSlewTables, Library, LimitInputs, LoweredCorner, NetLoads, NetlistView, NoCppr, NodeId,
    NodeOrdered, PhaseClock, Propagation, PropagationRecord, RangeTracker, RelationTable,
    ResolvedWireModel, STATE_BYTES_PER_NODE_TAG, SeededStarts, Spef, TagTable, TimingGraph,
    TimingLib, WireModelOption, add_latch_gq_slew_tables, build_latch_tag_maps,
    build_net_load_overrides, build_start_rows, build_wireload_config, check_corner_limits,
    check_max_transitions, discover_gating_sites, evaluate_all_checks, into_owned_lowered, lower,
    lower_corner_with_derates, propagate_forward_cone, static_transitions,
};

/// The analysis of one corner up to its lowered problem: steps 1 to 7 of [`analyze_one_corner`].
struct CornerSetup<'l> {
    wire_model: ResolvedWireModel,
    net_loads: NetLoads,
    clock_model: IdealClockModel,
    tags: TagTable,
    exceptions: ExceptionContext,
    launch_slews: FxHashMap<NodeId, [f64; 4]>,
    driving_cells: FxHashMap<NodeId, [[Option<DriveCellEffect>; 2]; 2]>,
    latch_maps: ArcTagMaps,
    lowered: LoweredCorner<'l>,
    diags: Vec<Diag>,
}

#[allow(clippy::too_many_arguments)]
fn setup_corner<'l>(
    corner_id: CornerId,
    corner_name: &str,
    corner_libs: &[Library],
    lib: &'l TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
) -> Result<CornerSetup<'l>, Diag> {
    let wires = wire_corner(corner_name, corner_libs, lib, netview, constraints, options)?;
    let prep = prepare_corner(
        corner_id,
        corner_name,
        lib,
        netview,
        graph,
        constraints,
        options,
        wires,
    )?;
    setup_prepared_corner(lib, netview, graph, constraints, options, prep, true)
}

/// Steps 1 and 2 of a corner's setup: its wire model, bound to its extracted parasitics when it
/// has them, its net loads and their diagnostics. A function of the shared inputs and the
/// corner's own, so it can run before earlier corners are analyzed.
pub(super) struct CornerWires {
    wire_model: ResolvedWireModel,
    net_loads: NetLoads,
    diags: Vec<Diag>,
    extracted: bool,
}

pub(super) fn wire_corner(
    corner_name: &str,
    corner_libs: &[Library],
    lib: &TimingLib,
    netview: &NetlistView<'_>,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
) -> Result<CornerWires, Diag> {
    let netlist = netview.frozen();
    let mut diags = Vec::new();

    // 1. Resolve wire model (`STA-REQ-0122`..`0125`).
    let wl_cfg = build_wireload_config(netview, constraints);
    let (mut wire_model, wl_diags) = match options.wire_model {
        WireModelOption::Ideal => (ResolvedWireModel::Ideal(Ideal), Vec::new()),
        WireModelOption::Auto => ResolvedWireModel::resolve(netlist, corner_libs, &wl_cfg),
        WireModelOption::WireloadLumped => {
            let req_cfg = wl_cfg.with_require_wireload(true);
            ResolvedWireModel::resolve(netlist, corner_libs, &req_cfg)
        }
    };
    diags.extend(wl_diags);

    // 2. Compute per-net capacitive loads (`STA-REQ-0110`, `STA-REQ-0121`).
    let corner_rc = options.parasitics_for(corner_name)?;
    let load_overrides = build_net_load_overrides(
        netview,
        constraints,
        wire_model.is_ideal() || corner_rc.is_some(),
    );
    if let Some(input) = corner_rc {
        if options.wire_model != WireModelOption::Auto {
            return Err(Diag::error(
                "T0070",
                "an explicit wire model cannot override extracted parasitics",
            ));
        }
        let max = options.max_state_bytes;
        let data = input.data().ok_or_else(|| {
            Diag::error(
                "T0070",
                format!(
                    "the extracted parasitics of corner {corner_name} were released by an \
                     earlier analysis"
                ),
            )
        })?;
        let bound = Spef::bind(&data, netview, lib, &load_overrides, max)?;
        // The corner no longer reads the parsed extraction.
        drop(data);
        input.bound();
        diags.clear();
        diags.extend(bound.topology().diagnostics(corner_name));
        wire_model = ResolvedWireModel::Extracted(bound);
        if input.coupling_records() > 0 {
            diags.push(Diag::warning(
                "T0074",
                "inter-net coupling grounded at factor 1; crosstalk not analyzed",
            ));
        }
    }
    let net_loads = NetLoads::compute(netlist, lib, &wire_model, &load_overrides);
    Ok(CornerWires {
        wire_model,
        net_loads,
        diags,
        extracted: corner_rc.is_some(),
    })
}

/// A corner that runs at once with others: analyzed, with its own propagation record, or only
/// prepared, when the memory budget of the corners running at once has no room for it.
pub(super) enum CornerRun {
    Analyzed(Box<(CornerAnalysis, PropagationRecord)>),
    Deferred(Box<(TimingLib, CornerPrep)>),
}

/// Steps 1 to 4 of a corner's setup: [`CornerWires`], the clock network, the tag table and the
/// timing exceptions, after which the propagation memory budget of the corner is checked. A
/// function of the shared inputs and the corner's own, so it can run before earlier corners are
/// analyzed.
pub(super) struct CornerPrep {
    wire_model: ResolvedWireModel,
    net_loads: NetLoads,
    clock_model: IdealClockModel,
    tags: TagTable,
    exceptions: ExceptionContext,
    diags: Vec<Diag>,
    /// The estimated propagation state of the corner without the parsed parasitics that every
    /// corner counts ([`parasitic_state_bytes`]).
    pub(super) own_state_bytes: u64,
}

/// The parsed parasitics of every analyzed corner, which can stay resident while any corner is
/// analyzed, so that the budget of each corner counts all of them (`STA-REQ-0820`).
pub(super) fn parasitic_state_bytes(options: &AnalysisOptions) -> u64 {
    options
        .parasitics
        .iter()
        .fold(0u64, |n, p| n.saturating_add(p.estimated_state_bytes()))
}

/// Steps 3 and 4 of a corner's setup, after [`wire_corner`], and its propagation memory budget.
#[allow(clippy::too_many_arguments)]
fn prepare_corner(
    corner_id: CornerId,
    corner_name: &str,
    lib: &TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    wires: CornerWires,
) -> Result<CornerPrep, Diag> {
    let CornerWires {
        wire_model,
        net_loads,
        mut diags,
        extracted,
    } = wires;

    // 3. Build ideal clock network (`STA-REQ-0200`).
    let clock_model = crate::engine::prop::clocks::propagated::build_model(
        netview,
        graph,
        lib,
        &net_loads,
        &wire_model,
        constraints,
        options,
        &mut diags,
    )?;

    // 4. Initialize the tag table and the timing exceptions (`STA-REQ-0217`, `STA-REQ-0220`,
    // `STA-REQ-0271`).
    let mut tags = TagTable::for_clocks_with_budget(constraints.clocks.len(), options.tag_budget)?;
    let (exceptions, exc_diags) =
        ExceptionContext::build(netview, graph, constraints, &clock_model, &mut tags)?;
    if corner_id.index() == 0 {
        diags.extend(exc_diags);
    }
    // Memory budget of the propagation state, checked before any of it is allocated (AGENTS
    // rule 9.2, `STA-REQ-0024`).
    let own_state_bytes = (graph.num_nodes() as u64
        * u64::from(tags.ntags_for_compute())
        * STATE_BYTES_PER_NODE_TAG)
        .saturating_add(
            graph.num_nodes() as u64
                * constraints
                    .clocks
                    .iter()
                    .filter(|c| c.propagated && !c.is_virtual())
                    .count() as u64
                * 128,
        )
        .saturating_add(crate::engine::limits::retained_bytes(
            graph.num_nodes(),
            options.max_limit_rows,
        ))
        .saturating_add(
            if extracted || lib.has_mixed_thresholds() || !constraints.timing_derate.is_identity() {
                graph.arcs().len() as u64 * 192 + graph.num_nodes() as u64 * 128
            } else {
                0
            },
        );
    let state_bytes = own_state_bytes.saturating_add(parasitic_state_bytes(options));
    if state_bytes > options.max_state_bytes {
        let gib = |b: u64| b as f64 / f64::from(1u32 << 30);
        return Err(Diag::error(
            "T0023",
            format!(
                "the propagation state of corner '{corner_name}' needs about {:.2} GiB ({} nodes x \
                 {} tags), above the limit of {:.2} GiB; raise --max-state-gib or reduce the \
                 clocks and timing exceptions",
                gib(state_bytes),
                graph.num_nodes(),
                tags.ntags_for_compute(),
                gib(options.max_state_bytes)
            ),
        ));
    }
    Ok(CornerPrep {
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        diags,
        own_state_bytes,
    })
}

/// Steps 5 to 7 of a corner's setup, after [`prepare_corner`]; with `wire_records` the lowered
/// problem keeps the wire records of its extracted net arcs (`lower_corner_with_derates`).
fn setup_prepared_corner<'l>(
    lib: &'l TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    prep: CornerPrep,
    wire_records: bool,
) -> Result<CornerSetup<'l>, Diag> {
    let starts = start_prepared_corner(lib, netview, graph, constraints, prep)?;
    lower_started_corner(
        lib,
        netview,
        graph,
        constraints,
        options,
        starts,
        None,
        wire_records,
    )
}

/// A corner after steps 5 and 6 of its setup: its preparation, start rows and latch tag maps.
struct CornerStarts {
    prep: CornerPrep,
    start_build: SeededStarts,
    latch_maps: ArcTagMaps,
}

/// Steps 5 and 6 of a corner's setup, after [`prepare_corner`].
fn start_prepared_corner(
    lib: &TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    mut prep: CornerPrep,
) -> Result<CornerStarts, Diag> {
    // 5. Build start rows (`STA-REQ-0217`, `STA-REQ-0218`, `STA-REQ-0219`).
    let start_build = build_start_rows(
        netview,
        graph,
        lib,
        constraints,
        &prep.clock_model,
        &prep.tags,
        &prep.exceptions,
        &prep.net_loads,
        &constraints.timing_derate,
    )?;

    // 6. Build latch `D -> Q` tag-mapping records (`STA-REQ-0221`, `STA-REQ-0222`).
    let latch_maps = build_latch_tag_maps(
        graph,
        constraints,
        &prep.clock_model,
        &NoCppr,
        &prep.tags,
        &mut prep.exceptions,
    )?;
    Ok(CornerStarts {
        prep,
        start_build,
        latch_maps,
    })
}

/// Step 7 of a corner's setup, after [`start_prepared_corner`]: the lowered problem, with the
/// latch `D -> Q` transition tables of `latch_tables` when the corner added them to the bank of
/// `lib` ([`add_latch_gq_slew_tables`]).
#[allow(clippy::too_many_arguments)]
fn lower_started_corner<'l>(
    lib: &'l TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    starts: CornerStarts,
    latch_tables: Option<&LatchSlewTables>,
    wire_records: bool,
) -> Result<CornerSetup<'l>, Diag> {
    let CornerStarts {
        prep,
        start_build,
        latch_maps,
    } = starts;
    let CornerPrep {
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        diags,
        own_state_bytes: _,
    } = prep;

    // 7. Lower onto `rusteda_compute::timing` and pin latch `G -> Q` slews (`STA-REQ-0222`,
    // `STA-REQ-0401`).
    let ntags = tags.len().max(1) as u32;
    let lowered = lower_corner_with_derates(
        netview,
        graph,
        lib,
        &net_loads,
        ntags,
        start_build.rows,
        &start_build.launch_slews,
        &start_build.launch_drives,
        &latch_maps,
        &wire_model,
        options.max_state_bytes,
        constraints.timing_derate,
        latch_tables,
        wire_records,
    )?;

    Ok(CornerSetup {
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        launch_slews: start_build.launch_slews,
        driving_cells: start_build.driving_cells,
        latch_maps,
        lowered,
        diags,
    })
}

/// The timing graph of `netlist` and the lowered problem of every corner, as an analysis with
/// `timing_libs` and `constraints` builds them before it propagates: the state an incremental
/// analysis of a later netlist starts from (`STA-REQ-0601`, `STA-REQ-0610`).
pub fn lower_corners(
    netlist: &FrozenNetlist<'_>,
    corners: &[CornerLibs<'_>],
    timing_libs: &[TimingLib],
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
) -> Result<(TimingGraph, Vec<LoweredCorner<'static>>), Vec<Diag>> {
    if corners.is_empty() || timing_libs.len() != corners.len() {
        return Err(vec![Diag::error(
            "T0005",
            "one corner with a compiled library view per corner is required",
        )]);
    }
    let corner_lib_slices: Vec<&[Library]> = corners.iter().map(|c| c.libs).collect();
    let netview = NetlistView::with_corners(netlist, &corner_lib_slices)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    let mut graph = TimingGraph::build(&netview, corners[0].libs)
        .map_err(|e| vec![Diag::error(e.code(), e.to_string())])?;
    ConstantPropagation::apply(&netview, corners[0].libs, Some(constraints), &mut graph);
    let mut lowered = Vec::with_capacity(corners.len());
    for (idx, (cl, tlib)) in corners.iter().zip(timing_libs).enumerate() {
        let setup = setup_corner(
            CornerId(idx as u32),
            cl.name,
            cl.libs,
            tlib,
            &netview,
            &graph,
            constraints,
            options,
        )
        .map_err(|d| vec![d])?;
        lowered.push(into_owned_lowered(setup.lowered));
    }
    Ok((graph, lowered))
}

/// Steps 1 to 4 of the setup of a corner from its bound wires `start` (`None`: from nothing), up
/// to its propagation memory budget.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_corner(
    corner_id: CornerId,
    corner_name: &str,
    corner_libs: &[Library],
    lib: &TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    start: Option<Result<CornerWires, Diag>>,
) -> Result<CornerPrep, Diag> {
    let wires = match start.transpose()? {
        Some(wires) => wires,
        None => wire_corner(corner_name, corner_libs, lib, netview, constraints, options)?,
    };
    prepare_corner(
        corner_id,
        corner_name,
        lib,
        netview,
        graph,
        constraints,
        options,
        wires,
    )
}

/// The analysis of a corner from its `start`, as [`start_corner`] and
/// [`analyze_prepared_corner`] make it.
#[allow(clippy::too_many_arguments)]
pub(super) fn analyze_one_corner(
    corner_id: CornerId,
    corner_name: &str,
    corner_libs: &[Library],
    lib: TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constant: &[bool],
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    ctx: Ctx<'_>,
    propagation: Propagation<'_>,
    keep_lowered: KeepLowered,
    start: Option<Result<CornerWires, Diag>>,
    record: &mut PropagationRecord,
) -> Result<CornerAnalysis, Diag> {
    let clock = PhaseClock::start(ctx, "corner_setup");
    let prep = start_corner(
        corner_id,
        corner_name,
        corner_libs,
        &lib,
        netview,
        graph,
        constraints,
        options,
        start,
    )?;
    analyze_prepared_corner(
        clock,
        corner_id,
        corner_name,
        lib,
        netview,
        graph,
        constant,
        constraints,
        options,
        ctx,
        propagation,
        keep_lowered,
        prep,
        record,
    )
}

/// Steps 5 to 12 of the analysis of a corner, after [`start_corner`]; `clock` times its setup.
#[allow(clippy::too_many_arguments)]
pub(super) fn analyze_prepared_corner(
    clock: PhaseClock<'_>,
    corner_id: CornerId,
    corner_name: &str,
    mut lib: TimingLib,
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    constant: &[bool],
    constraints: &CompiledConstraints,
    options: &AnalysisOptions,
    ctx: Ctx<'_>,
    propagation: Propagation<'_>,
    keep_lowered: KeepLowered,
    prep: CornerPrep,
    record: &mut PropagationRecord,
) -> Result<CornerAnalysis, Diag> {
    let netlist = netview.frozen();
    // 5. to 7. Start rows, latch maps and lowering, after the wire model, loads, clocks, tags and
    // exceptions of the preparation.
    let CornerSetup {
        wire_model,
        net_loads,
        clock_model,
        tags,
        mut exceptions,
        launch_slews,
        driving_cells,
        latch_maps,
        lowered,
        mut diags,
    } = {
        let starts = start_prepared_corner(&lib, netview, graph, constraints, prep)?;
        // The scalar transition tables of the latch `D -> Q` arcs are added to the corner's own
        // bank, which its lowered problem borrows, instead of to a copy of the whole bank.
        let latch_tables =
            add_latch_gq_slew_tables(graph, &starts.start_build.launch_slews, &mut lib.lut_bank);
        lower_started_corner(
            &lib,
            netview,
            graph,
            constraints,
            options,
            starts,
            Some(&latch_tables),
            // A table-only analysis of every node reads only the prepared arc values; a cone
            // propagation compares the wire records with those of the earlier analysis.
            keep_lowered != KeepLowered::ForTable || !matches!(propagation, Propagation::Full),
        )?
    };
    clock.finish();
    let relations = RelationTable::new();
    let mut range_tracker = RangeTracker::new();

    // 8. Propagate arrivals and slews (`STA-REQ-0400`..`0407`): every node, or the cone of the
    // nodes whose inputs differ from an earlier analysis (`STA-REQ-0453`, `STA-REQ-0601`).
    let clock = PhaseClock::start(ctx, "propagation");
    let propagated = match propagation {
        Propagation::Full => {
            let res = lowered
                .propagate(options.backend, options.precision, ctx)
                .map_err(|e| lower::compute_error_diag(&e))?;
            record.changed.push(None);
            record.seeds.push(graph.num_nodes());
            res
        }
        Propagation::Cone { prior, mapping } => {
            let pc = prior.corners.get(corner_id.index()).ok_or_else(|| {
                Diag::error(
                    "T0100",
                    format!("the earlier analysis has no corner '{corner_name}'"),
                )
            })?;
            let (res, changed, seeds) = propagate_forward_cone(
                Some(prior.graph),
                Some(pc.lowered),
                pc.node_values,
                graph,
                &lowered,
                mapping,
                options.backend,
                options.precision,
                ctx,
            )
            .map_err(|e| lower::compute_error_diag(&e))?;
            record.changed.push(Some(changed));
            record.seeds.push(seeds);
            res
        }
    };
    record.evaluated.push(propagated.stats.nodes_evaluated);
    let node_values = propagated.to_node_order(graph);
    let mut propagated = propagated;
    // Everything after the propagation reads the node-ordered copy, which holds the same values;
    // a table-only corner releases the level-ordered ones at once.
    if keep_lowered == KeepLowered::ForTable {
        propagated.values = Default::default();
    }
    clock.finish();
    // Static transitions of the pins without a propagated one, for the design rules (STA-REQ-0809).
    let statics = static_transitions(
        graph,
        netview,
        &lib,
        &lowered,
        &wire_model,
        &net_loads,
        &node_values,
        constant,
    );
    // A table's required times are propagated over the lowered problem once the corner's check
    // records and endpoints are built; until then it keeps only what that pass reads.
    let for_table = match keep_lowered {
        KeepLowered::No => {
            drop(lowered);
            None
        }
        KeepLowered::Full => {
            record.lowered.push(into_owned_lowered(lowered));
            None
        }
        KeepLowered::ForTable => {
            let mut lowered = lowered;
            lowered.retain_for_required_times();
            Some(lowered)
        }
    };

    // 9. Check unanalyzed arcs, max capacitance, and max transition (`STA-REQ-0017`,
    // `STA-REQ-0015`, `STA-REQ-0120`).
    let clock = PhaseClock::start(ctx, "electrical_checks");
    let (unanalyzed_arcs, unanalyzed_diags) = lib.unanalyzed_arc_warnings(netlist);
    diags.extend(unanalyzed_diags);
    net_loads.check_max_capacitance(netlist, &lib, &mut range_tracker);
    check_max_transitions(netview, &lib, &node_values, &mut range_tracker);
    // Pulse widths, periods and design rules of the pins (`STA-REQ-0800` to `STA-REQ-0810`).
    let limits = check_corner_limits(
        &LimitInputs {
            netview,
            lib: &lib,
            constraints,
            clock_model: &clock_model,
            tags: &tags,
            node_values: &node_values,
            net_loads: &net_loads,
            pulse_rules: options.pulse_rules,
            clock_limits: options.clock_limits,
            constant,
            statics: &statics,
        },
        options.max_limit_rows,
        corner_id.index() == 0,
        &mut range_tracker,
        &mut diags,
    );
    clock.finish();

    // 10. Discover clock-gating sites, evaluate all timing checks, and classify endpoints
    // (`STA-REQ-0229`..`0226`).
    let clock = PhaseClock::start(ctx, "timing_checks");
    let gating_sites = discover_gating_sites(netview, graph, &lib, &clock_model);
    let check_records = evaluate_all_checks(
        netview,
        graph,
        &lib,
        constraints,
        &clock_model,
        &NoCppr,
        &tags,
        &mut exceptions,
        &gating_sites,
        &NodeOrdered(&node_values),
        &mut range_tracker,
    )?;
    diags.extend(range_tracker.warnings(&lib.lut_bank));
    if corner_id.index() == 0 {
        diags.extend(exceptions.diagnostics());
    }

    clock.finish();
    let clock = PhaseClock::start(ctx, "endpoints");
    let endpoints = CornerEndpoints::build(
        corner_name,
        netview,
        graph,
        constraints,
        &clock_model,
        &tags,
        &gating_sites,
        &NodeOrdered(&node_values),
        check_records,
        constant,
    );
    clock.finish();
    let table_only = for_table.is_some();
    if let Some(lowered) = for_table {
        // The required times and slacks of the timing table (`STA-REQ-0227`), after which
        // the corner's lowered problem is released.
        let clock = PhaseClock::start(ctx, "required_times");
        record.required.push(BackwardPass::for_table(
            graph,
            &lowered,
            tags.len().max(1),
            &endpoints.records,
            &node_values,
        ));
        drop(lowered);
        clock.finish();
    }

    let mut ca = CornerAnalysis {
        corner_id,
        corner_name: corner_name.to_string(),
        lib,
        wire_model,
        net_loads,
        clock_model,
        tags,
        exceptions,
        relations,
        launch_slews,
        driving_cells,
        latch_maps,
        gating_sites,
        propagated,
        node_values,
        endpoints,
        unanalyzed_arcs,
        limits,
        diags,
    };
    if table_only {
        ca.retain_for_table();
    }
    Ok(ca)
}
