<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Supported timing scope

## Implemented scope

- **Workspace & binary containers:** `RBIN v1` (`NETLIST v1`, `LIBERTY v1`, `CONSTR v1`, `STA v2`),
  `sta_check.json` (`ruststa.sta_check.v1`), multi-corner library resolution, and carry-forward
  revision transactions.
- **Liberty timing & wire-load model:** 1D/2D/3D NLDM delay, transition, and constraint tables,
  per-table index overrides, `Table::mid_slew`, rise/fall and min/max pin capacitance ranges,
  driver output-pin capacitance, `three_state` functions and enable/disable arcs, `when` Boolean
  conditions, clock-gating attributes, `wire_load` / `wire_load_selection` (`WireloadLumped` and
  `Ideal`), and electrical limit attributes (`max_transition`, `max_capacitance`, `max_fanout`).
- **SDC subset (`CONSTR v1`):** bounded Tcl interpreter (`set`, `list`, `expr`, `if`, `foreach`,
  `source`, explicit `--sdc-env NAME=VALUE`; `proc` and other Tcl outside the subset are errors
  `D0006`), `create_clock` (with `-add`),
  `set_clock_latency`, `set_clock_uncertainty` (simple and `-from`/`-to`), `set_clock_transition`,
  `set_input_delay`, `set_output_delay`, `set_driving_cell`, `set_input_transition`, `set_load`,
  `set_wire_load_model`, `set_wire_load_mode`, `set_case_analysis`, `set_logic_zero`,
  `set_logic_one`, `set_disable_timing`, `set_max_transition`, `set_max_capacitance`,
  `set_max_fanout`, `set_max_time_borrow`, `set_false_path`, `set_multicycle_path`,
  `set_max_delay`, `set_min_delay`, `set_clock_groups`, `group_path`, and `set_units`.
- **Timing analysis, incremental updates, and checking:** per-pin worst transition times over all
  enabled fan-in arcs, per-tag arrival and required times, harmonic multi-period clock expansion,
  latch time borrowing and opening-edge `D -> Q` re-launch, `async_default` recovery/removal,
  combinational and integrated clock-gating checks, netlist constants, tie cells and case analysis
  applied before propagation (`STA-REQ-0158`), `-from`/`-to` timing exceptions and clock groups
  applied by `ruststa run` and `ruststa update` (`STA-REQ-0271`), incremental cone invalidation in
  timing sessions (`TimingSession`), and engine-independent checking (`ruststa-check`) that also
  reports every endpoint check only one side times (`STA-REQ-0307`).

## Current limits

The following capabilities sit behind explicit engine seams in Phase 2a and are scheduled for
Phase 2b (see [`docs/roadmap.md`](../roadmap.md)):

- **Propagated and generated clocks:** `set_propagated_clock` and `create_generated_clock` are
  diagnosed and rejected at constraint compilation; clocks are ideal in Phase 2a.
- **Timing derates and CPPR:** `set_timing_derate` and common-path pessimism removal (`--cppr
  violating|all`) are Phase 2b extensions (`cppr` is `"off"` in Phase 2a).
- **Ordered `-through` exception hops in the engine:** `-through`, `-rise_through`, and
  `-fall_through` on timing exceptions are rejected at import with diagnostic `D0011` in Phase 2a
  (the independent checker evaluates a separate supported subset and may return
  `undecided` when its work budgets are exceeded).
- **Exception forms not analysed yet (`STA-REQ-0271`):** `-ignore_clock_latency` is error `T0071`;
  a `set_max_delay` or `set_min_delay` that could time a path from an input port without input
  delay or to an output port without output delay gets warning `T0072` and such paths are not
  timed; an exception that differs for rising and falling data at a latch data pin is error
  `T0073`; timing sessions refuse exceptions and clock groups (`T0074`).
- **`ruststa update` recomputes the whole graph:** it classifies the change since the ancestor
  revision (`incremental.mode`) and states `incremental.computation: "full"`; the incremental
  engine of timing sessions is not yet used by the command.
- **Lumped wire-load model:** `WireloadLumped` adds the wire capacitance of the `wire_load` table
  to the net load and no wire resistance, so a library with a `default_wire_load` (such as
  `sky130_fd_sc_hd`) gives no wire delay; resistive tree models are a Phase 2b item.
- **Distributed RC parasitics and tree wire loads:** `SPEF` parasitic annotation
  (`rusteda import --spef`) and balanced/worst-case tree wire-load modes (`WireloadTree`) are
  Phase 2b items; Phase 2a supports `WireloadLumped` and `Ideal`.


Use the [timing workflow](../guide/timing.md) to run an analysis and the [verification guide](../verification/index.md) to assess evidence.
