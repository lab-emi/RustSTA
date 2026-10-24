# ADR 0004: Phase 2a static timing conventions

- Status: accepted
- Date: 2026-10-16

## Context

Phase 1 of RustSTA implemented a single-corner ideal-wire static timing engine with per-tag slews
(`STA-REQ-0101`, `0115`), zero clock latency on `set_input_delay` startpoints (`STA-REQ-0201`),
`D_h = D_s - T_C` for the hold relation (`STA-REQ-0210`), latch borrowing that carried the upstream
launch tag through `D -> Q` without setup uncertainty at the opening edge (`STA-REQ-0213`), and no
constant propagation, three-state enable/disable propagation, driver output-pin capacitance, or
wire-load estimation (`STA-REQ-0011`, `0110`). Extending the engine to multi-corner industrial cell
libraries and SDC constraints requires explicit, unambiguous conventions for each of these points.

## Decision

The Phase 2a timing specifications ([`docs/specs/sta/`](../specs/sta/README.md)) adopt the
following conventions, while moving the superseded Phase 1 texts to
[`docs/specs/sta/legacy.md`](../specs/sta/legacy.md) until package `Z1` removes the legacy engine:

1. **Per-pin slews (`STA-REQ-0108`, `0109`).** Transition times are stored per pin, transition and
   analysis bound (`slew[node][tr][an]`), reduced as the worst (`max` for `late`, `min` for `early`)
   output transition across all enabled incoming arcs, including arcs whose source pin carries a
   transition but no launch tag. Cell arc delays and output transitions are evaluated once per
   `(tr_in, tr_out, an)` from the source pin slew, and shared across all launch tags reaching the
   source pin (COMPUTE-REQ-0015 to 0018).
2. **Extrapolation clamping (`STA-REQ-0120`).** Linear extrapolation outside a transition table's
   index range clamps negative output slews at `0.0`, preventing runaway negative slews on
   high-fanout nets. Extrapolated arc delays and constraint values are not clamped at zero. Out-of-range
   table lookups and `max_transition`/`max_capacitance` violations emit one aggregated warning per
   construct.
3. **Driver output-pin capacitance (`STA-REQ-0121`).** The capacitive load `C_net` of a net sums, in
   canonical `NET_PINS` order, the load pin capacitances, the output-pin capacitance of every driver
   pin on the net (including all `three_state` drivers on a bus), explicit `set_load` values, and
   wire capacitance.
4. **Lumped wire-load model and port-net exclusion (`STA-REQ-0122` to `0125`).** In
   `WireloadLumped` mode (`WireModel`), an internal net with `F` connected instance load pins adds
   `C_wire = capacitance * L(F)` with linear interpolation and `slope` extrapolation on
   `fanout_length`. Nets connected to any top-level port receive `C_wire = 0.0`.
5. **Per-corner resolution and threshold/derate rules (`STA-REQ-0015`, `0016`).** Cells are resolved
   independently per corner, and duplicate cell definitions within a corner are an error. Library
   slew trip points and `slew_derate_from_library` are verified consistent within each corner and
   recorded in corner metadata; in lumped NLDM delay calculation, table values are evaluated directly
   without multiplying by `slew_derate_from_library`.
6. **Three-state bus propagation (`STA-REQ-0013`, `0223`).** `three_state_enable` (`Z -> 1` rise,
   `Z -> 0` fall) and `three_state_disable` (`0 -> Z` rise, `1 -> Z` fall) arcs propagate from `EN`
   to `Z`, and multi-driver three-state bus nets merge arrivals and slews across all enabled
   drivers.
7. **Input/output delay clock latency and driving cells (`STA-REQ-0217`, `0218`).** `set_input_delay`
   start arrivals include the reference clock's source and network latency unless
   `-source_latency_included` or `-network_latency_included` is given; `set_output_delay` capture
   latencies obey the same flags. `set_driving_cell` adds `delay(s_in, C_port) - delay(s_in, 0)` to
   the port arrival and sets the port transition to `slew(s_in, C_port)`.
8. **One-sided `-max` / `-min` I/O delays (`STA-REQ-0219`).** Specifying only `-max` (or only
   `-min`) on `set_input_delay` or `set_output_delay` seeds only the corresponding analysis bound
   and leaves the opposite bound unset.
9. **Hold relation over the common period (`STA-REQ-0220`).** Over all launch edges `l` in the
   common period `P = lcm(T_L, T_C)` (in integer femtoseconds), `D_s = min_l (min { c > l } - l)`
   and `D_h = max_l (max { c <= l } - l)`. This replaces `D_h = D_s - T_C`, which is optimistic by
   multiples of the clock period whenever `T_L != T_C`.
10. **Latch window, opening-edge uncertainty, and `D -> Q` tag re-launch (`STA-REQ-0221`, `0222`).**
    A latch check pairs each closing edge `D_close` with the opening edge of the same enable pulse
    `D_open = D_close - W`, subtracts setup uncertainty `u_s` at both `R_open` and `R_close`,
    applies `set_max_time_borrow`, and re-launches borrowed arrivals (`a > R_open`) through `D -> Q`
    into the latch enable's opening-edge tag `tag_open` at `(min(a, R_close) - D_open) + delay_DQ`,
    with `slew[Q]` taken from the `G -> Q` enable arc.
11. **Recovery/removal, clock gating, and endpoint completeness (`STA-REQ-0224` to `0228`).**
    Asynchronous `recovery_*` and `removal_*` checks form the `async_default` group; combinational
    AND (`v = 0`, active-high) and OR (`v = 1`, active-low) clock-gating checks pair leading and
    trailing edges of the gated pulse; every potential endpoint receives an explicit status in
    `completeness.unconstrained_by_reason`.
12. **Constants, case analysis, and unate sense refinement (`STA-REQ-0150` to `0157`).** Kleene
    three-valued evaluation (`0`, `1`, `X`) from netlist ties, tie cells, `set_case_analysis` and
    `set_logic_*` disables controlled combinational arcs, refines `non_unate` arcs (such as `XOR2`
    with a constant input) to `positive_unate` or `negative_unate`, suppresses checks on constant
    sequential pins while preserving clocked `CK -> Q` launches, and records disabled-arc counts by
    reason.
13. **Transitions ignore logic values; checks take the capture clock path's transition (version
    0.4, `STA-REQ-0116`, `0159`, `0161`, `0229`, `0230`).** Black-box comparisons with a reference
    timing tool on both default libraries (gates with constant side inputs next to their twins
    whose constant input is a port with transition 0, case analysis on and off, clock muxes with a
    tied select, latches with tied enables) showed that the reference's pin transitions do not
    depend on logic values, while its arrivals do. RustSTA now merges into a pin's transition
    every arc that only logic values disable (and the clock-to-output arcs of unclocked
    sequential cells), with the arc's library sense; these arcs carry no arrival and no required
    time. A unate arc whose sense the residual function contradicts is disabled; a latch whose
    enable a constant holds active is transparent for late arrivals. Since a pin's early and late
    transitions now differ wherever several arcs merge, the related pin transition of a check is
    that of the capture clock path (early for setup and recovery, late for hold and removal),
    which the same comparison confirmed; with a single arc into each clock pin the results are
    those of the earlier rule.

## Consequences

- Each convention has a closed-form hand derivation on the linear synthetic library in
  [`verification/analytic/sta/`](../../verification/analytic/sta/README.md), checked to `1e-9 ns` by
  both the engine (`E3`–`E7`) and the independent checker (`R1`).
- Separating per-pin slews from per-tag arrivals shrinks the per-tag node state and matches the
  shared `rusteda-compute::timing` ABI ([`docs/specs/compute/timing.md`](../specs/compute/timing.md)).
