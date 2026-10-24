# Changelog

All notable changes to this project are documented in this file.
The format follows the "Keep a Changelog" convention; versions follow semantic versioning.

## [Unreleased]

## [0.4.3] - 2026-10-24

Extracted nets whose RC network is not a tree. Version 0.4.2 refused the whole timing run on two
reference-extracted routed designs of the suite (a sky130 ALU: a sink via with no resistor path to
the rest of its net; a gf180 register file: two parallel resistors on one pin). Both are now
timed, and their endpoints agree with the reference timing tool within 0.62 and 0.67 ps at three
corners. Every net that is a tree is timed bit for bit as before: on 44 routed cases (the 32 cases
of 0.4.2's comparison, six of the dual-library fixture and six further extractions of the final
layouts of the two designs) every command output and every committed table equals that of 0.4.2
after normalizing the version.

### Changed
- **Resistor loops (STA-REQ-0730):** a net with resistor loops (after zero-resistance
  contraction; two parallel resistors are a loop) is solved on its nodal conductance system
  instead of being rejected. The first moments solve `G tau = C` exactly in f64 (spanning tree
  passes, the Woodbury identity and a Cholesky factor of the `k x k` loop resistance matrix, a
  Kirchhoff residual check with refinement), and the driving-point pi takes the admittance
  coefficients `sum C`, `sum C*tau`, `sum C*tau^2`. Scratch and work are bounded by
  `--max-state-gib`; no loop or path is enumerated. Note `T0152` counts the loops per corner.
  Against an external nodal simulator the moments agree to 3e-15 and the modelled sink arrivals
  of planted loops to 0.08 ps; against transistor-level transients of both default PDKs'
  `buf_1` the wire delays agree within 0.2 ps, where the reference timing tool, which times a
  loop as a tree without some of its resistors, is 4 to 29 ps off.
- **Components without a resistor path to the driver (STA-REQ-0731):** such a part of a net is
  joined to the driver with zero resistance instead of rejecting the net. Its capacitance loads
  the driver, its sinks get no wire delay (a lower bound), and warning `T0151` per corner counts
  the nets, components, nodes, pins and capacitance and names the first net and its members. The
  reference timing tool also gives such sinks no wire delay but drops the component's
  capacitance from the driver's load.
- **Binding (STA-REQ-0732, replaces STA-REQ-0703):** the binding no longer rejects cycles and
  disconnected RC nodes; complete annotation, one driver and the connection checks are
  unchanged.
- A resistor whose two ends are one zero-resistance node carries no current and is ignored
  (counted in `T0152`); 0.4.2 rejected it as a loop.

### Added
- `Spef::topology` (`RcTopology`) with the counts behind `T0151` and `T0152`.
- `tests/rc_mesh_spice.rs` (ignored by default): operating points and transients of an external
  nodal simulator on five planted networks with and without loops.
- The incremental-versus-full and CPU-versus-CUDA tests of extracted RC also run networks with
  loops and with unconnected parts.

### Performance
- End-to-end `ruststa run` (three corners, two threads, median of seven sequential runs on a
  shared machine) is unchanged on tree-only designs (0.178 s vs 0.180 s on a GF180 transmitter,
  0.488 s vs 0.479 s on a GF180 register file, 0.390 s vs 0.410 s on a sky130 multiplier); peak
  memory is unchanged (within 0.6 %). A net with `k` loops adds `8 (k^2 + k + 5 n)` bytes of
  scratch while it is reduced.

### Known differences
- On networks with resistor loops the reference timing tool's delays differ from the solved
  network (and from the circuit) by up to 29 ps on the planted probes; on the extracted nets
  seen so far the only loops are parallel resistors of about 0.1 Ohm, where both agree to
  0.0001 ps.
- The reference timing tool removes the capacitance of a component without a resistor path from
  its driver's load; RustSTA keeps it, so the driver's delay can be larger by the effect of that
  capacitance.

## [0.4.2] - 2026-10-24

Effective capacitance and wire response of extracted RC nets. On the final layouts of eight
routed designs of the suite (32 design, library and extraction cases with native and reference
extraction, 96 corner comparisons), the largest endpoint difference against the reference timing
tool falls from 5.21 ps (54 comparisons above 1 ps) to 0.97 ps (none above 1 ps), and against
transistor-level transients of the default cells the modelled arrival at the sinks of RC trees
improves from 6.5 ps to 0.64 ps mean error.

### Changed
- **Effective capacitance (STA-REQ-0728, replaces STA-REQ-0726):** the Thevenin driver's
  effective capacitance is one Newton-Raphson correction of the driver system (two trip-point
  residuals of the single-capacitance response and the end-of-ramp charge) from the total load,
  no longer the converged charge solution. The resistance (forward sensitivity over 1 fF) and
  the lumped regime (STA-REQ-0727) are unchanged. The converged solution over-estimates the
  shielding of resistive loads: against transistor-level transients of buffers and inverters of
  both default PDKs (each characterized by the same simulator), the mean driver delay error falls
  from 3.1 to 1.8 ps on pi loads and from 5.6 to 2.3 ps on two-segment RC trees. Driver delays into
  resistive nets are larger than in 0.4.1 by up to a few picoseconds per stage.
- **Wires of a resistive driver (STA-REQ-0729, replaces STA-REQ-0722):** when the transition at a
  driver pin comes from a cell arc (clock-to-output launches included) into a resistive pi, every
  sink of the net follows that driver's own waveform through a single pole of its first moment,
  instead of a ramp with the driver's transition; other nets keep the ramp, primary ports the
  exponential response. Sink transitions of shielded nets are larger (by up to 22 ps on the
  synthetic probes), and the sink arrival against the circuit improves as stated above.
- The RC preparation is faster: the correction replaces the bounded bisection of the charge
  solve. End-to-end `ruststa run` (three corners, two threads, median of seven sequential runs
  on a shared machine) takes 0.17 s instead of 0.39 s on a GF180 transmitter, 0.39 s instead of
  1.10 s on a GF180 register file and 0.40 s instead of 1.26 s on a sky130 multiplier; peak
  memory is unchanged (within 1 %). The driver waveform record adds 96 bytes per pin.

### Added
- `rusteda_compute::rc::gate_one_step` and `rusteda_compute::rc::wire_from_driver` (shared crate,
  additive; `gate_detail`, `gate_detail_with_resistance` and `wire` are unchanged).
- `ruststa::engine::delay::driver::pi_drive` returns the driver waveform (resistance and source
  duration) with the response; `TimingLib::arc_delay_drive_3d` does so for a library arc.
- The external-simulator case of the engine's driver rule (`tests/rc_driver_spice.rs`, ignored by default)
  also checks the sink response of STA-REQ-0729 and the position of the one-step capacitance
  between the charge solution and the total load.

### Known differences
- Above about 1 kOhm of pi resistance the reference timing tool's effective capacitance lies
  between the one-step value and the charge solution (driver delay differences up to 8 ps at
  3 kOhm on the default cells; 0.06 ps up to 100 Ohm, 0.3 ps up to 500 Ohm). Its driver waveform
  transition differs from the refitted one by up to 1.5 % (1.5 ps on a 0.1 ns transition).

## [0.4.1] - 2026-10-24

Shared crates converged with those of the suite's logic synthesis tool: both repositories vendor
the same 209 shared files with the same `verification/shared.sha256`, and `cargo xtask shared
diff` finds no difference. The timing engine is unchanged: on the routed dual-library fixture,
its probes and the sixteen-netlist corpus, every committed table (`netlist`, `constraints`,
`parasitics`, `sta`, `limits`) equals that of 0.4.0 byte for byte, and so does every command
output apart from the tool version, the time stamps and the program path.

### Added
- **BLIF and AIGER readers:** `rusteda_formats::blif` and `rusteda_formats::aiger`
  (`docs/specs/blif/`, `docs/specs/aiger/`); `rusteda import --rtl` also takes `.blif`, `.aag` and
  `.aig` files and records their format in `rtl.json` (RCLI-REQ-0021, RCLI-REQ-0022). Their
  diagnostics keep their letters: `DiagCode::LETTERS` is `ABCDGLSTVW` (WS-REQ-0190).
- **Streaming netlist export:** `rusteda export --netlist` writes from the validated frozen
  netlist tables with bounded memory (`EXPORT_BUDGET`) and parallel formatting in source order,
  without building a mutable netlist; the text is the same as before. The writer can also build
  frozen tables directly (`verilog::netlist::to_frozen_netlist`, `FrozenBuildLimits`).
- **Bounded netlist import:** a netlist that the streaming reader cannot take is read by the full
  reader only up to `rusteda import --max-netlist-ast-mib` (default 64 MiB, RCLI-REQ-0024); a
  larger one is rejected (`V0300`, naming the budget and the option) instead of being expanded
  without a bound as in 0.4.0. To read such a file as 0.4.0 did, give a larger budget after
  estimating the full reader's memory for it.

### Changed
- The fast flat netlist reader is the synthesis tool's: nested concatenations, at most
  `MAX_RANGE_WIDTH` bits and 64 nesting levels. A part-select against the declared direction and
  a decimal constant of more than 32 digits take the full reader instead, with the same result
  within its budget (see **Bounded netlist import**); above the budget the import is rejected.
- The requirement of `rusteda show` for summaries without `drv_status` is RCLI-REQ-0023 (it was
  numbered RCLI-REQ-0021 in 0.4.0, a number the shared specification already gave to BLIF and
  AIGER import).
- The external-simulator oracle tests of `rusteda-compute` (ignored by default) read
  `RUSTEDA_SPICE_SIMULATOR`, `RUSTEDA_RC_ORACLE_WORKSPACE` and `RUSTEDA_RC_WIRE_WORKSPACE`
  instead of variables named after this repository.
- `cargo xtask shared` also covers `docs/specs/blif` and `docs/specs/aiger`.

### Fixed
- The Verilog lexer read a `:` followed directly by a comment (`2'b01:// x`) as the operator
  `:/`; a comment is whitespace after a case label or a ternary colon.

## [0.4.0] - 2026-10-23

### Added
- **Pin limit checks (`STA-REQ-0800` to `STA-REQ-0817`):** minimum pulse width (high and low) and
  minimum period of every clock pin on the propagated or ideal clock waveform, with the latest
  opening and earliest closing arrival, required widths from the `min_pulse_width` tables at the
  opening transition time and the worst of conditional (`when`) groups and of the pin attributes
  `min_pulse_width_high`, `min_pulse_width_low` and `min_period`; on an ideal clock the width is
  the declared waveform with the clock latencies (note `T0139`); electrical design rules
  `max_transition`, `max_capacitance` and `max_fanout` from library pin limits and defaults and
  from `set_max_transition`, `set_max_capacitance`, `set_max_fanout` (design, port and clock-domain
  slots) and `set_fanout_load`. Rows are kept in a bounded heap (`--max-limit-rows`), counts cover
  every check.
- **`limits` table:** `limits.rbin` (`LIMITS` v1) committed by `run` and `update` with `sta.rbin`;
  `update --verify-full` and session verification compare it bit for bit.
- **Report sections** `limits`, `pulse_width`, `min_period`, `drv`, `max_transition`,
  `max_capacitance`, `max_fanout`, and the hold-fix report `hold_fix` with `--target-margin`:
  every hold violator per corner with the needed delay, its driving net and the stages of its
  worst hold path with nets and pin slack windows, plus a merged list over the corners
  (`STA-REQ-0830` to `STA-REQ-0832`).
- **Metrics and status:** `min_pulse_width_violating`, `min_period_violating`,
  `max_transition_violating`, `max_capacitance_violating`, `max_fanout_violating`, their worst
  slacks, `pulse_width_status`, `max_transition_status`, `max_capacitance_status`,
  `max_fanout_status`, `drv_status`, and the completeness counts `unchecked_pulse_width_pins`,
  `unevaluated_limit_checks` and `drv_unlimited`, in `run`, `update`, queries and `whatif`. One
  rule (`STA-REQ-0813`) gives every status: `met`, `violated`, `partial`, `unconstrained` or
  `unchecked`; a count without a checked row is `null`; unclocked pulse pins and checks with a
  value that is not finite (`T0137`) are never reported as met.
- **Liberty pin pulse attributes (LIBERTY-REQ-0049, WS-REQ-0117):** the shared reader keeps
  `min_pulse_width_high`, `min_pulse_width_low` and `min_period` on their `Pin` instead of ignoring
  them (no timing arc is added), and the compiled library format `LIBERTY` v4 carries them; a
  cached v3 `.rlib` is compiled again from its `.lib`.
- **Selectable pulse rules (`STA-REQ-0816`, `STA-REQ-0817`):** `run` and `update` take
  `--pulse-groups worst|last` (the largest constraint of a pin's groups and attributes, the
  default, or per transition the last listed group that states it) and `--pulse-pins
  all|clock-ends` (every clocked pin, the default, or only the pins where a clock ends; clock-gater
  inputs are then counted in `completeness.pass_through_pulse_pins`, warned with `T0141`, and the
  pulse status is `partial`). The envelope reports `scope.pulse_rules`; the `limits` table and the
  `sta.rbin` scope record the rules per corner; `T0142` counts unused period fall tables under
  `last`.
- **Design rules on constant nets (`STA-REQ-0809`):** the drivers of constant nets (tie cells,
  netlist literals, case analysis, propagated through cells) are not checked for
  `max_capacitance` and `max_fanout` (`completeness.drv_constant_drivers`); pins that the
  propagation leaves without a transition (constant nets and their cones, outputs of sequential
  cells that no clock reaches) are checked for `max_transition` with a static transition from the
  corner's delay calculation (`completeness.drv_static_transition_pins`, note `T0143`); any other
  pin without a transition is counted as untimed (`completeness.drv_untimed_pins`, warning
  `T0145`) and its limited check is unevaluated, so the rule is never `met` with a silent gap.
- **Ideal clock networks (`STA-REQ-0822`, replaces `STA-REQ-0818`):** the pins of an ideal
  clock's network are left out of the design rules; their limited checks are unevaluated
  (`completeness.drv_ideal_clock_pins`, warning `T0146`). The network includes the pins that the
  clock reaches only through arcs that constants or case analysis disable (a clock gate stopped by
  a tie, a clock port under case analysis), and a flop whose clock such a constant stops has the
  output transition of its clock-to-output arc at the ideal clock transition.
- **Constant pins pass no arrival (`STA-REQ-0164`):** the wires of a constant net (from a tie, a
  port or pin under case analysis, or a gate a constant controls), the wire into a load pin under
  case analysis and the cell arcs into a pin held constant are disabled with the constant's
  reason and carry only their transitions. The loads of a port under case analysis that also has
  an input delay were in that clock's domain and took its design-rule limit; checks at a flop data
  pin held by case analysis were timed.
- **Unconnected pins (`STA-REQ-0819`):** the library pins that the netlist leaves unconnected are
  not pins of the design and are not checked; the unconnected output pins are counted
  (`completeness.drv_unconnected_output_pins`, note `T0148` with the first five names).
- **Clock limit scope (`STA-REQ-0808`):** `run` and `update` take `--clock-limits paths|domain`:
  `-clock_path` limits on the clock network and `-data_path` limits on the data pins (the
  default), or the data-path limit of a clock on its whole domain with `-clock_path` alone not
  applied (warning `T0144`). The clock network pins on which the two scopes differ are counted
  under either scope (`completeness.drv_clock_scope_pins`, note `T0147`). The envelope reports
  `scope.drv_rules`; the `sta.rbin` scope and bit 2 of the `limits` table's corner rules record
  it.
- **`rusteda_compute::rc::gate_detail_with_resistance`** (shared compute crate): the bounded
  charge solve and waveform of `rc::gate_detail` for a caller-supplied Thevenin resistance;
  `rc::gate_detail` keeps its estimate and results bit for bit.
- **`T0140`** warns when an import drops the `parasitics` table because the netlist changed
  (RCLI-REQ-0076).
- **Extracted parasitics per corner (`STA-REQ-0820`, RCLI-REQ-0075):** `rusteda import` takes
  repeatable `--spef FILE --spef-corner NAME` pairs into a `PARASIT` v2 table; later imports add or
  replace corners; every analyzed corner uses its own extraction and a corner without one fails
  with `T0070`.
- **`--require-wire-model spef_rc|ideal|wireload_lumped`** on `run`, `update` and `whatif`
  (`STA-REQ-0821`, `T0135`).

### Changed
- **Transitions ignore logic values (`STA-REQ-0117`, replaces `STA-REQ-0116`):** the transition of
  a pin merges every arc
  that carries one: the enabled arcs, the arcs that constants, case analysis and false `when`
  conditions disable (with their library sense), the transitions that a sense refinement removes,
  the clock-to-output arcs of sequential cells that logic values disable or that no clock reaches,
  and the `D -> Q` arc of a latch with a constant enable; tie outputs start at transition 0 and a
  port held by case analysis keeps its input transition. These arcs carry no arrival and no
  required time. Live pins behind gates with constant side inputs had optimistic transitions
  (3 to 84 ps on a synthesized core with case analysis) and their delays and checks followed;
  constant nets now have propagated transitions, and only literals and their cones keep the
  static transition 0 (`STA-REQ-0809`). The propagation of a propagated clock merges the same
  arcs, with the input ports, the tie cells and the launched outputs of sequential cells as
  transition sources: a flop output that enters the clock network (an AND clock gate, a clock
  multiplexer select) seeds the clock propagation with its transition, repeated until the seeds
  no longer change (`STA-REQ-0714`); the clock latency below such a gate was 1.5 to 7.4 ps off on
  the probes, optimistic for hold. A loop that these arcs would close is cut and warned with
  `T0149`.
- **RC driver resistance and lumped pi loads (`STA-REQ-0726`, replaces `STA-REQ-0721`;
  `STA-REQ-0727`):** the Thevenin resistance of a cell output that drives an extracted pi load is
  the forward delay sensitivity of the table over 1 fF above the total load (it was the central
  sensitivity over 1 % of the total load), and a pi whose resistance is below one thousandth of
  that resistance is a lumped load: the delay and the transition are the table values at the
  total capacitance. The fitted waveform transition of a nearly lumped load lay 3 to 5 % below the
  table transition: on a GF180 slow-corner clock net (pi resistance 16.9 Ohm, switch at 17.8 Ohm)
  the clock buffer transition was 14.5 ps low and endpoint slacks were 6.8 ps (setup) and 5.3 ps
  (hold) off the reference; a table breakpoint within 1 fF above the total load changed the
  resistance by up to 10 % (0.85 to 1.1 ps transitions at a sky130 clock gate and flop outputs).
  On the routed and hold-closed physical-design workspaces the extracted endpoint slacks agree
  with the reference within 0.33 ps at every corner, with native and reference-extracted
  parasitics, on both PDKs (the differential fixture within 0.55 ps). Above about 100 Ohm a
  residual of the reference's effective capacitance remains (up to 3 % of the transition on the
  probe grid), where its driver transition is not a continuous function of the wire resistance.
- **Unate arcs contradicted by constants (`STA-REQ-0159`, replaces `STA-REQ-0152`):** a declared
  `positive_unate` (`negative_unate`) arc whose residual function is negative (positive) unate in
  its input is disabled: an `XOR2` next to a tie keeps only the arc of the right sense (it timed
  the wrong-sense arc as well, 28 to 756 ps pessimistic on the probes).
- **Latches with a constant enable (`STA-REQ-0163`, replaces `STA-REQ-0161` and `STA-REQ-0154`):**
  a latch whose enable a constant holds active is transparent (late arrivals pass `D -> Q` in
  their own tags, no early arrival); held inactive it is opaque. It made the outputs behind it
  unconstrained and left them out of the clock's design-rule limits. A check that a false `when`,
  a constant or case analysis disables is no longer timed where arrivals reach its pins (a
  `when : "RN"` setup check of a flop whose reset is tied).
- **Held clear and preset (`STA-REQ-0162`, replaces `STA-REQ-0151`):** a flop or latch whose
  asynchronous clear or preset is held active (tie or case analysis) is no longer a constant: its
  clock-to-output arc launches and the cone below it is timed and checked for capacitance and
  fanout. With `set_case_analysis 0` on the reset of a synthesized core the reference reported
  max_transition violators that RustSTA missed, and slacks were up to 1 ns (setup) and 390 ps
  (hold) optimistic.
- **Clock gate outputs (`STA-REQ-0162`):** the output of a clock gate (`clock_gate_out_pin`) is the
  conjunction of its `clock_gate_clock_pin` and `clock_gate_enable_pin` inputs: an enable or clock
  held at 0 makes it constant 0 (whatever the test enable, the state function and the statetable),
  its clock arc carries only a transition and the flops below are unclocked. Such an output was
  never constant and was checked for capacitance and fanout.
- **Related pin transition of checks (`STA-REQ-0229`, replaces `STA-REQ-0211`):** setup and
  recovery checks (and the closing edge of a latch setup) take the early transition of the
  capture clock pin, hold and removal checks the late one, the transitions of the capture clock
  path; the earlier rule had them the other way round, which was invisible while each clock pin
  had one arc.
- **Inverted-data latches (`STA-REQ-0230`):** the `D -> Q` arc of a latch keeps the sense of its
  library arc when the latch group stores the inverted data pin; note `T0150` counts such
  instances and names their cells.
- `min_pulse_width` and `minimum_period` groups are no longer counted as unanalyzed arcs.
- The `CORNERS` record of the `limits` table (unreleased `LIMITS` v1) is 80 bytes and carries the
  constant-driver, static-transition, ideal-clock, untimed, clock-scope and unconnected-output pin
  counts of the design rules.
- The analyzed design-rule commands no longer carry `D0021`; the import-time `D0021` wording is
  neutral about which analysis checks them.
- The `sta` revision summary reports measured design-rule counts with `drv_status`; `rusteda show`
  reports a summary without `drv_status` as `unchecked` with `null` counts (RCLI-REQ-0021), and
  the shared table builder writes them so (WS-REQ-0137). The `sta` status gains `drv` and
  `pulse_width`; `timing` also covers pulse width and period violations.

### Fixed
- `path --to` (and pin or port targets) with `--corner` returned no path when the endpoint's worst
  path was in another corner, and without `--corner` it searched only the worst setup corner; it
  now gives the worst path in `--corner`, or over all corners, and warns `T0138` for a requested
  check family without a path (`STA-REQ-0814`).
- The row sections of `report` give the checked count as `total`, with `stored`, `truncated` and
  `unevaluated`, when the row budget leaves rows out.
- The state estimate of a corner counts the parsed parasitics of every analyzed corner, and one
  import holds at most eight per-file budgets of decoded SPEF (`T0072`).
- `inspect`, its instance pins and the `pins` section reported the hold-side required time as
  `required_ns` and the setup required time as `required_hold_ns`; they now give
  `required_setup_ns` (and `required_ns`) and `required_hold_ns` at the worst slack's transition.
- `report --kind setup|hold` now applies to the `endpoints` and `checks` sections, and without
  `--sort` the `endpoints` section of `--kind hold` is ordered by hold slack, so `--limit N` lists
  the worst hold endpoints.
- `inspect` of a latch data endpoint reported the node's required time, which also covers the
  paths through the transparent latch; an endpoint now reports the required times of its worst
  setup and hold path, consistent with its endpoint slacks and with `path --to` (`STA-REQ-0812`).
- On revisions with extracted parasitics, `update` (full fallback) and `run` of the same revision
  could commit `sta.rbin` files that differed by about 1e-4 ns in required times behind latches:
  the backward pass lowered a corner again without pinning the latch `G -> Q` transitions. Both
  now lower a corner through one function (`STA-REQ-0806`).
- A design rule checked on some pins while others have no limit is `partial`, not `met`
  (`STA-REQ-0813`).
- A clock-to-output arc into a flop or latch output held by `set_case_analysis` launched its
  arrival whenever a clock reached the clock pin, so the held output stayed in the clock's domain
  and took the clock's `max_transition` limit (eight extra violators on a synthesized core with
  register outputs under case analysis); a disabled launch now launches no arrival
  (`STA-REQ-0164`), and one that `set_disable_timing` disables (`-from CK -to Q`) no longer
  launches either (`STA-REQ-0156`). Arcs that logic values disable still carry their transition.

## [0.3.0] and earlier development previews

### Added
- **RustEDA workspace and generic CLI (`rusteda-workspace`, `rusteda-cli`):**
  - Immutable, content-addressed disk workspace (`manifest.json`, `workspace.lock`, `history.jsonl`,
    `cache/`, `revisions/rev<NNNN>/meta.json`) with advisory shared/exclusive locking, atomic
    staging commits, carry-forward table hardlinking, and `metric_deltas`.
  - Little-endian, 8-byte-aligned `RBIN v1` binary container with `xxh3-64` content hashing and
    read-only `memmap2` views (`NETLIST v1`, `LIBERTY v1`, `CONSTR v1`, `STA v1`).
  - `rusteda` command-line driver: `init`, `import` (`--lib`, `--corner`, `--rtl`, `--top`, `-D`,
    `-I`, `-P`, `--no-predefined-macros`, `--netlist`, `--sdc`, `--sdc-env`, `-m`), `log`, `show`,
    `diff`, `revert`, `export --netlist`, and `completions`, emitting deterministic text or
    `rusteda.v1` JSON envelopes.
- **Core netlist and Liberty model extensions (`rusteda-core`):**
  - `GateNetlist` with generational arenas (`InstId`, `NetId`, `PinId`, `CellId`), `SymbolInterner`,
    in-place cell swap, buffer insertion, net split, and zero-copy `FrozenNetlist` (`NETLIST v1`)
    with `A8a` escaped identifier rules (`\/` and `\\`, printable ASCII `33..=126`).
  - Liberty extensions (`RS-LIB.A`, `RS-LIB.B`, `RS-LIB.C`, `RS-LIB.D`): typed `TimingType` enum
    (including `three_state_enable`/`disable`, `non_seq_setup`/`hold`, `min_pulse_width`,
    `minimum_period`, `skew_*`, `nochange_*`, `clear`, `preset`), 1D/2D/3D lookup-table templates
    with per-table index overrides, `Table::mid_slew`, output-pin capacitance, rise/fall capacitance
    ranges, `three_state` functions, `when` Boolean conditions (`Func`), `statetable` and
    `state_function`, `clock_gating_integrated_cell` and `clock_gate_*` pin roles, slew threshold
    and derate attributes, `wire_load`, `wire_load_table`, `wire_load_selection`, and
    `operating_conditions` (`tree_type`), plus power/isolation/level-shifter/retention/PG-pin
    attributes.
- **Format readers and SDC compiler (`rusteda-formats`):**
  - Bounded, deterministic Tcl interpreter and SDC compiler (`rusteda_formats::sdc`) supporting
    clocks, latencies, uncertainties (`-from`/`-to`), transitions, I/O delays (`-add_delay`,
    `-network_latency_included`, `-source_latency_included`), `set_driving_cell`, `set_load`,
    `set_wire_load_model`, `set_wire_load_mode`, `set_case_analysis`, `set_logic_zero`,
    `set_logic_one`, `set_disable_timing`, `set_max_transition`, `set_max_capacitance`,
    `set_max_fanout`, `set_max_time_borrow`, `set_false_path`, `set_multicycle_path`,
    `set_max_delay`, `set_min_delay`, `set_clock_groups`, `group_path`, and `set_units`, producing
    both `CONSTR v1` (`constraints.rbin`) and canonical `constraints/merged.sdc` (`CANON`).
  - `rusteda_formats::liberty::read_file_diags` and `read_text_diags` with complete construct
    classification and aggregated `L` diagnostics.
  - Streaming structural Verilog reader (`RS-VLOG`) with hierarchy flattening, bus/concatenation
    expansion, `supply0`/`supply1` tie lowering, and `pg_pin` supply port pruning.
- **Shared compute timing task (`rusteda-compute::timing`):**
  - Level-parallel `TimingPropagationTask` (`FlatTimingGraph`) executing in `f64` or `f32` on the
    CPU (`rayon`) or on NVIDIA GPUs via NVRTC-compiled CUDA kernels (`--features cuda`), with
    `BACKEND_UNAVAILABLE` (`A0501`) diagnostics when a requested GPU backend is unavailable.
- **Phase 2a static timing engine (`ruststa::engine`):**
  - Multi-corner static timing analysis over `FrozenNetlist`, `.rlib`, and `CONSTR v1`: per-pin
    worst transition times across all enabled fan-in arcs, per-tag arrival and required times,
    `WireloadLumped` and `Ideal` wire models (with top-level port net exclusion and driver
    output-pin capacitance), 1D–3D NLDM interpolation/extrapolation, and Kleene 3-valued (`0`, `1`,
    `X`) constant propagation with cofactor arc disabling and `non_unate` sense refinement.
  - Sequential timing checks: flip-flops, active-high and active-low latches (opening-edge
    uncertainty, `set_max_time_borrow`, and opening-edge `D -> Q` re-launch), asynchronous
    `recovery`/`removal` in `async_default`, combinational and integrated clock-gating checks,
    endpoint status classification, and `-from`/`-to` timing exceptions (`set_false_path`,
    `set_multicycle_path`, `set_max_delay`, `set_min_delay`, `set_clock_groups`).
  - Backward required-time propagation, per-pin slacks, setup-then-hold `k`-worst path extraction,
    and revision-scoped handles (`path<N>`, `pin<N>`, `port<N>`, `inst<N>`, `net<N>`).
  - 24-section `STA v1` (`sta.rbin`) binary timing store (`Full` and `Summary` modes) with `O(1)`
    zero-copy mapped queries (`StaStore`) and incremental cone-invalidated timing updates
    (`TimingSession`).
- **Independent workspace checker (`ruststa_check::rev`):**
  - Engine-independent revision checker reading verbatim cached `.lib` files, re-parsed structural
    Verilog netlists, and canonical SDC (`constraints/merged.sdc`) with zero dependency on
    `ruststa` or `rusteda_formats::sdc`; evaluates constants, delays, checks, and timing exceptions
    (including `-through`) in `f64`, emits `C0500`–`C0999` diagnostics and `mm<N>` mismatch handles,
    and commits `sta_check.json` (`ruststa.sta_check.v1`).
- **Workspace `ruststa` CLI (`ruststa-cli`):**
  - Subcommands `run`, `update`, `check` (`--method independent|full|both`), `path`, `inspect`,
    `report`, and `completions`, with `rusteda.v1` JSON envelopes, `next_actions`, and the
    `run_e2e` scale benchmark.
- **Verification fixtures and Dual-PDK automation (`ruststa-fixtures`, `xtask`):**
  - 15 synthetic `wt_*` Liberty libraries, 8 topology netlist/SDC pairs, and deterministic
    `DesignGen` scale netlists (`1k` to `1M` cells).
  - `cargo xtask pdk fetch` and `cargo xtask verify pdk` (`--profile pr|full`, `RUSTEDA_PDK_DIR`)
    for pinned `sky130_fd_sc_hd` and `gf180mcu` standard-cell libraries.
- Shared read-only view of the timing table (`rusteda_workspace::sta`, WS-REQ-0126 to
  WS-REQ-0136), written from the format specification for the other tools of the suite, with a
  builder of minimal tables; a test checks it against the engine's reader on tables that
  `ruststa run` writes (counts, summaries, endpoints, ranked paths and their points).
- `--max-state-gib` on `ruststa run` and `ruststa update`: the analysis estimates the propagation
  state of each corner before allocating it and stops with `T0023` above the limit (default
  16 GiB; `STA-REQ-0024`).
- `cargo xtask verify pdk` runs every generated design of every library through `ruststa run` and
  `ruststa check --method independent` (the checker must agree); the PDK suite previously checked
  only the library API on three designs and skipped designs it did not find.
- `cargo xtask verify pdk` runs 1,000 random sizing and buffering mutations on each `tt` library
  of both PDK families with the incremental timing session and compares it with a full analysis
  after every step. The test existed but nothing ran it, it compared only every 25th step, and it
  passed as `SKIPPED` without libraries; it now fails when a library is missing.
- `cargo xtask verify` and `verify pdk` run each test step with `RUST_TEST_THREADS=4`, a timeout
  and, where available, a memory-limited user scope (`RUSTEDA_VERIFY_MEMORY_MAX`,
  `RUSTEDA_VERIFY_TIMEOUT_S`, `RUSTEDA_VERIFY_NO_SCOPE`); `verify pdk --timeout` now applies.

### Fixed
- `ruststa run` and `ruststa update` applied neither constants nor timing exceptions: netlist
  constants, tie cells, `set_case_analysis`, `set_logic_zero`/`set_logic_one`, `set_false_path`,
  `set_multicycle_path`, `set_max_delay`, `set_min_delay` and `set_clock_groups` were accepted at
  import and then had no effect on the reported timing. The analysis now applies them
  (`STA-REQ-0158`, `STA-REQ-0271`); forms it does not analyse yet are errors `T0071`, `T0073`,
  `T0074` or warning `T0072` instead of being dropped, and `set_case_analysis rising|falling`
  gets warning `T0040`.
- Sense refinement and cofactor disabling changed the library's arcs of cells without any
  constant input; they now apply only under constant inputs (`STA-REQ-0153`).
- Constant propagation scanned every graph arc for each load of a constant net on every round;
  its fixed point is now a worklist, linear in the pins of the netlist.
- The timing table's backward pass looked up the arc classes of the library instead of those of
  the lowered problem, which panicked once constant propagation refined an arc sense.
- `ruststa check` counted only endpoint checks that both sides time, so a check that one side did
  not time (a constant, an exception) was never a difference, and `--method full` paired endpoint
  lists by position without comparing their lengths. Such differences are now mismatches `C0502`
  and `C0504` (`STA-REQ-0307`).
- The independent checker read `[get_ports [list a b]]` of the canonical constraints as one
  pattern and dropped the input and output delays of such lists.
- Unanalyzed library arcs are reported by timing type instead of as `unknown`, the revision summary
  reports `null` instead of `0.0` for the worst slack of a corner without timed checks, and a timing
  table whose endpoint or corner count `PATH_RANK` cannot name is error `T0022` instead of being
  packed with truncated indices.
- `ruststa update` reports `incremental.computation`, which states that the values come from a full
  recomputation (`STA-REQ-0303`).
- `ruststa run`, `update` and `check` dropped every diagnostic of a successful command: warnings
  of the constraints and of the analysis (for example `T0015`, `T0016`, `D0102`, `D0110`) never
  reached the envelope or standard error. They now do (`STA-REQ-0308`).
- An exception whose `-from` or `-to` objects were all ignored lost only that option and so
  matched every path of the other one; it now matches no path (`STA-REQ-0255`).
- `set_clock_uncertainty -rise` and `-fall` were accepted and dropped; they are now error `D0011`
  (`SDC-REQ-0023`). `-level_sensitive` input and output delays, `set_load -subtract_pin_load` and
  minimum/maximum operating conditions are analysed without their effect and now get warnings
  `T0041`, `T0042` and `T0043` (`STA-REQ-0022`).
- The independent checker read `set_case_analysis rising` and `falling` as the constants `1` and
  `0`; they leave the pin unknown (`STA-REQ-0150`).
- The independent checker's matching of timing exceptions enumerated paths without a bound (more
  than 50 GB on a design of 8,000 cells with `-from [all_registers]`); it now stops at 200,000
  paths with the verdict `undecided` (`STA-REQ-0506`).
- The objects of an exception option were kept in an unsorted list and searched linearly for
  every startpoint and check; they are now sorted, and matching is a binary search.
- The timing table's query API named ranked paths from `path0` while the command line names them
  from `path1`, so `ruststa path --target path<N>` past the last path reported `path<N-1>` in its
  diagnostic. Every handle of the API now uses the command line's numbering (`STA-REQ-0305`).
- `cargo xtask verify pdk` wrote a WNS of `0.000000` for runs without a timed check (and one
  endpoint for every scale run) in `runs.tsv`, `report.json` and `summary.json`; these runs now
  report `unconstrained` (JSON `null`) and their real endpoint counts.

### Changed
- `rusteda_core::liberty::Library::templates` is now a `BTreeMap<String, Template>` (was
  `HashMap<String, Template>`) so that lookup-table templates iterate in deterministic key order.
- `ruststa-cli` now builds with no default Cargo features (`cpu` backend only by default; pass
  `--features cuda` to enable the `cuda` and `gpu` backends), and the default propagation precision
  for both `cpu` and `cuda` is `f64` (bit-identical to CPU), with `f32` opt-in via
  `--precision f32`.
- Repository crates and specifications follow the shared RustEDA contract (`rusteda-core`,
  `rusteda-workspace`, `rusteda-compute`, `rusteda-formats`, `rusteda-cli`); [`docs/roadmap.md`](docs/roadmap.md)
  replaces `ROADMAP.md`, and [`docs/specs/compute/decisions.md`](docs/specs/compute/decisions.md)
  supersedes [`docs/adr/0001-gpu-runtimes-and-native-cuda.md`](docs/adr/0001-gpu-runtimes-and-native-cuda.md).

### Removed
- Legacy run-record CLI subcommands (`ruststa sta`, `ruststa check sta`, `ruststa show`) and options
  (`--runs-root`, `@` run references, `--record`, `--run-dir`, `--label`, `--print`, `--from-run`),
  replaced by the workspace commands (`ruststa run`, `update`, `check`, `path`, `inspect`, `report`).
- CubeCL and `wgpu` GPU runtimes and dependencies (`--backend wgpu`).
