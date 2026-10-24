<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Supported timing scope

## Implemented scope

- **Workspace & binary containers:** `RBIN v1` (`NETLIST v1`, `LIBERTY v1`, `CONSTR v1`, `STA v2`),
  `sta_check.json` (`ruststa.sta_check.v1`), multi-corner library resolution, and carry-forward
  revision transactions.
- **Detailed parasitics:** bounded plain/gzip SPEF import into `PARASIT v2`,
  one explicit extraction per corner, unit/name mapping, complete net/pin annotation,
  zero-resistance contraction and RC-tree first moments. Coupling is grounded
  at factor 1; unsupported meshes and missing annotation are errors.
- **Liberty timing & wire-load model:** 1D/2D/3D NLDM delay, transition, and constraint tables,
  per-table index overrides, `Table::mid_slew`, rise/fall and min/max pin capacitance ranges,
  driver output-pin capacitance, `three_state` functions and enable/disable arcs, `when` Boolean
  conditions, clock-gating attributes, `wire_load` / `wire_load_selection` (`WireloadLumped` and
  `Ideal`), electrical limit attributes (`max_transition`, `max_capacitance`, `max_fanout`,
  `fanout_load`), and `min_pulse_width` / `minimum_period` groups.
- **SDC subset (`CONSTR v1`):** bounded Tcl interpreter (`set`, `list`, `expr`, `if`, `foreach`,
  `source`, explicit `--sdc-env NAME=VALUE`; `proc` and other Tcl outside the subset are errors
  `D0006`), `create_clock` (with `-add`),
  `set_clock_latency`, `set_clock_uncertainty` (simple and `-from`/`-to`), `set_clock_transition`,
  `set_propagated_clock`, `unset_propagated_clock`,
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
- **Pin limit checks (0.4):** minimum pulse width and minimum period of clock pins, and the
  design rules `max_transition`, `max_capacitance` and `max_fanout` from the library and the
  constraints, in the `limits` table, with a hold-fix report for physical repair
  ([limits](../specs/sta/limits.md)). The pulse rules are selectable: the largest constraint
  over a pin's conditional groups and attributes and every clocked pin (the defaults), or the
  last listed group and only the pins where a clock ends (`--pulse-groups last
  --pulse-pins clock-ends`). The drivers of constant nets are not checked for capacitance and
  fanout, pins on constant nets and unclocked sequential outputs are checked with a static
  transition, an ideal clock network is left out of the design rules (counted, the rules
  `partial`), and a clock's limits map to its network and data pins (`--clock-limits paths`) or
  to its whole domain (`--clock-limits domain`).

## Current limits

The following capabilities sit behind explicit engine seams in Phase 2a and are scheduled for
Phase 2b (see [`docs/roadmap.md`](../roadmap.md)):

- **Generated clocks:** `create_generated_clock` remains unsupported. Explicit
  propagated primary-port clocks are supported on the bounded unate clock
  network; internal sources and unsupported reconvergence fail explicitly.
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
- **Extracted model limits:** detailed SPEF uses a moment-matched pi,
  charge-matched effective load, finite-ramp Thevenin driver and one-pole
  receiver threshold/slew model. Driver resistance is an NLDM sensitivity proxy;
  coupling is grounded. This is not CCS, transistor characterization or crosstalk. General RC meshes and balanced/worst-case
  estimated tree wire-load modes remain unsupported. The independent checker
  returns undecided for extracted RC/propagated clocks; full consistency and
  external comparisons are separate evidence.


Use the [timing workflow](../guide/timing.md) to run an analysis and the [verification guide](../verification/index.md) to assess evidence.
