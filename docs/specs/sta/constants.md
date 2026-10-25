# RustSTA constants, case analysis and disabled arcs

Constant propagation and arc disabling run on the timing graph before arrival and slew propagation
[@bhasker2009static, ch. 3 and 7; @sdc2016; @liberty2017]. Every requirement has a hand derivation
on the linear synthetic library in
[`verification/analytic/sta`](../../../verification/analytic/sta/README.md).

## Constant sources and Kleene propagation

**STA-REQ-0150** Constant sources
([`const_tielo_and2`](../../../verification/analytic/sta/const_tielo_and2/DERIVATION.md))
[@sdc2016; @bhasker2009static, ch. 7]. Every node of the timing graph carries a logic state in the
three-valued domain `{0, 1, X}` (initialized to `X`). Constant values `0` or `1` are seeded from:
1. netlist constant nets (`1'b0` / `1'b1` on `PIN_NET` and `ASSIGNS`, STA-REQ-0001);
2. library tie cells whose output pin `function` is the constant boolean expression `"0"` or `"1"`;
3. `set_case_analysis 0|1|zero|one` and `set_logic_zero` / `set_logic_one` on top-level ports or
   cell pins (`set_case_analysis rising|falling` leaves the logic state at `X`).

**STA-REQ-0162** Forward three-valued evaluation (replaces STA-REQ-0151)
([`const_tielo_and2`](../../../verification/analytic/sta/const_tielo_and2/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. Constants propagate in topological order across nets (a
single constant driver sets the net and all its load pins to that constant) and across combinational
cell output `function` expressions using Kleene three-valued logic (`!0 = 1`, `!1 = 0`, `!X = X`;
`0 & X = 0`, `1 & X = X`; `1 | X = 1`, `0 | X = X`; `0 ^ b = b`, `1 ^ b = !b`, `X ^ _ = X`).
- The output of a clock gate, a pin with `clock_gate_out_pin`, takes the value of the conjunction
  of the cell's input pins with `clock_gate_clock_pin` or `clock_gate_enable_pin` (`X` without
  any): a clock or enable held at `0` makes it `0`. The cell's `function`, `state_function`,
  `statetable`, `clock_gating_integrated_cell` and its `clock_gate_test_pin` take no part, so a
  test enable held at `1` does not pass a clock whose enable is held at `0`, and a negative-edge
  clock gate gives `0` as well, as observed of a reference timing tool.
- The outputs of sequential cells (an `ff` or `latch` group, or `clear`/`preset` arcs) take no
  constant from their inputs, also when an asynchronous `clear` or `preset` expression is held
  active: the stored state is not treated as a constant, its clock-to-output arc launches
  (STA-REQ-0163), and the cone below it is timed and checked for every design rule.

## Arc disabling and sense refinement

**STA-REQ-0159** Disabling combinational arcs controlled by constants (replaces STA-REQ-0152)
([`const_tielo_and2`](../../../verification/analytic/sta/const_tielo_and2/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. For a combinational arc from input pin `p` to output pin
`y` with boolean output expression `f`:
- if `p` itself is at constant `0` or `1`, or `y` evaluates to constant `0` or `1` under Kleene
  evaluation, the arc `p -> y` is disabled;
- more generally, after substituting all constant input pins of the cell into `f`, if the cofactors
  of the residual function with respect to `p` are identically equal (`f|_{p=0} == f|_{p=1}` for
  every assignment to the remaining `X` inputs), the output is independent of `p` and the arc
  `p -> y` is disabled;
- an arc declared `positive_unate` whose residual function is negative unate in `p` (STA-REQ-0153),
  or declared `negative_unate` whose residual function is positive unate, cannot switch `y` and is
  disabled. For example `XOR2` (`Y = A ^ B`) with separate positive and negative unate arcs from
  `A` and `B = 1` keeps only the negative unate arc `A -> Y`.
The disable reason is `case_analysis` if any constant input of the cell was seeded or derived from
`set_case_analysis` / `set_logic_*`, and `constant` otherwise. Disabled arcs propagate no arrival;
an arc that only logic values disable still carries its output transition into `y`
(STA-REQ-0117).

**STA-REQ-0153** Unate sense refinement under constant inputs
([`const_xor_tiehi`](../../../verification/analytic/sta/const_xor_tiehi/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. When a combinational arc `p -> y` with declared
`timing_sense : non_unate` remains enabled after substituting the cell's constant inputs into the
output expression `f`, its effective timing sense is refined from the residual function `r`:
- if `r|_{p=0} <= r|_{p=1}` for all assignments to the remaining `X` inputs (a rise on `p` can only
  cause a rise on `y`, and a fall on `p` only a fall on `y`), the effective sense is
  `positive_unate`;
- if `r|_{p=0} >= r|_{p=1}` for all assignments to the remaining `X` inputs (a rise on `p` can only
  cause a fall on `y`, and a fall on `p` only a rise on `y`), the effective sense is
  `negative_unate`;
- otherwise the effective sense remains `non_unate`.
For example, `XOR2` (`Y = A ^ B`) with `B = 1` refines `A -> Y` to `negative_unate`, and with
`B = 0` refines `A -> Y` to `positive_unate`.

**STA-REQ-0163** Sequential pins held constant (replaces STA-REQ-0161)
([`const_flop_d`](../../../verification/analytic/sta/const_flop_d/DERIVATION.md))
[@bhasker2009static, ch. 7; @sdc2016]. When a sequential data pin (`D`) or asynchronous control pin
(`RN`, `SN`) is at constant `0` or `1`, it carries no signal transition, so no setup, hold,
recovery or removal check is timed at that pin (its endpoint status is `no_data_arrival`,
STA-REQ-0226). The cell's `CK -> Q` edge arc still launches both output transitions if `CK` is in a
clock network, because the stored state is not treated as constant across clock cycles, also when
`clear` or `preset` is held active (STA-REQ-0162). A check that a false `when` (STA-REQ-0155), a
constant or case analysis disables is not timed, also where arrivals reach both of its pins. When a
sequential clock pin (`CK`, `CKN`) or latch enable pin (`G`) is at constant `0` or `1`, the clock
network does not propagate through it, its edge arcs launch no arrival (their transitions still
enter the output pin, STA-REQ-0117), and checks referencing that clock pin have status
`unclocked`. A latch whose enable expression (the `enable` of its `latch` group) the constants
make:
- active (`1`) is transparent: its `D -> Q` arc is a combinational arc that passes the late
  arrivals at `D` into `Q` in their own launch tags, with the arc's delay at the transition of
  `D`, and no early arrival (no hold path passes a latch); the outputs below are timed as data of
  those tags;
- inactive (`0`) is opaque: its `D -> Q` arc passes no arrival and carries its transition only.

**STA-REQ-0164** Constant pins pass no arrival
([`const_tielo_and2`](../../../verification/analytic/sta/const_tielo_and2/DERIVATION.md))
[@bhasker2009static, ch. 7; @sdc2016]. A pin at a constant (STA-REQ-0150, STA-REQ-0162) passes no
arrival into its fanout and receives none:
- the wires of a net whose driver is constant (a tie output, a port or pin under case analysis, a
  gate output that a constant controls) and the wire into a load pin that case analysis holds are
  disabled with the reason of the constant (`constant` or `case_analysis`);
- so is every cell arc into a pin held constant (an output pin under case analysis, a clock gate
  output, STA-REQ-0159).
A port held by case analysis keeps the arrival of its own `set_input_delay`, so the port itself is
in that clock's domain for the design rules (STA-REQ-0805), but its loads are not, and no check at
its loads is timed. These arcs carry their transitions (STA-REQ-0117) and are counted in
`completeness.disabled_arcs` (STA-REQ-0157).

**STA-REQ-0155** Multiplexer select constants and conditional (`when`) arcs
([`const_mux_select`](../../../verification/analytic/sta/const_mux_select/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. On a multiplexer (`Y = (A0 & !S) | (A1 & S)`), holding
`S = 0` disables `A1 -> Y` and `S -> Y` and leaves `A0 -> Y` enabled (`positive_unate`); holding
`S = 1` disables `A0 -> Y` and `S -> Y` and leaves `A1 -> Y` enabled. When a Liberty timing group
carries a `when` boolean condition and substituting the cell's constant inputs makes `when`
evaluate to `0`, that timing arc or check is disabled with reason `false_when`.

**STA-REQ-0156** Explicit arc disabling (`set_disable_timing`)
([`case_analysis`](../../../verification/analytic/sta/case_analysis/DERIVATION.md)) [@sdc2016].
`set_disable_timing [-from <pin>] [-to <pin>] <objects>` disables every matching timing arc on the
named cells or pins with reason `user_disabled`: all arcs of a cell when neither `-from` nor `-to`
is given, the arcs matching the named input and/or output pin names when `-from` and/or `-to` are
given on a cell, or the arcs incident to a pin object.

**STA-REQ-0157** Completeness accounting for disabled arcs and endpoints
([`case_analysis`](../../../verification/analytic/sta/case_analysis/DERIVATION.md))
[@bhasker2009static, ch. 7; @sdc2016]. Every disabled arc is recorded in `completeness.disabled_arcs`
with its reason (`cycle_break`, `constant`, `case_analysis`, `false_when`, `user_disabled`). An
endpoint whose check arcs are disabled has status `disabled`; an endpoint whose data pin receives no
arrival because all upstream data arcs were disabled by constants, case analysis or
`set_disable_timing` has status `no_data_arrival` (or `disabled`), and is counted in
`completeness.unconstrained_by_reason` (STA-REQ-0226).

**STA-REQ-0158** Constants in every analysis. The full analysis of `ruststa run` and
`ruststa update`, and every graph of a timing session, apply `set_disable_timing`, the netlist
constants, the tie cells and case analysis (STA-REQ-0150 to STA-REQ-0157) to the timing graph
before arrival times propagate; there is no analysis path that skips them. The fixed point of
STA-REQ-0162 is computed with a worklist, so its work is linear in the pins of the netlist. Sense
refinement and cofactor disabling (STA-REQ-0159, STA-REQ-0153) apply to cells with at least one
constant input; without one, the library's arcs and senses stand. `set_case_analysis rising` or
`falling` leaves the pin at `X` and both transitions are timed: each such pin gets warning `T0040`
in the analysis, so the constraint is never dropped silently.
