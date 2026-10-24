# RustSTA superseded requirements (legacy engine, removed by Z1)

The requirements in this file belong to the Phase 1 legacy engine (`crates/engines/ruststa/src/*.rs`
outside `src/engine/`) and the legacy checker (`crates/verify/ruststa-check/src/*.rs` outside
`src/rev/`). They are retained verbatim so that the frozen legacy tests keep citing the exact text
they verify until package `Z1` removes the legacy engine, legacy checker, and this file.

The Phase 2a timing rules that replace them are defined in [`inputs.md`](inputs.md) (`STA-REQ-0014`
to `0017`), [`delay.md`](delay.md) (`STA-REQ-0108`, `0109`, `0120` to `0125`),
[`constants.md`](constants.md) (`STA-REQ-0150` to `0157`), and [`checks.md`](checks.md)
(`STA-REQ-0217` to `0228`); see [ADR 0004](../../adr/0004-timing-conventions.md).

## Superseded Phase 2a arc classification (replaced by STA-REQ-0017 in version 0.4)

STA-REQ-0013 below is the arc classification of versions up to 0.3. Version 0.4 checks
`min_pulse_width` and `minimum_period` groups as pulse checks (STA-REQ-0800, STA-REQ-0801), and
STA-REQ-0017 in [`inputs.md`](inputs.md) replaces it. The classification of every other arc kind is
the same in both; the independent checker, which models no pulse check, keeps citing STA-REQ-0013
for those kinds.

**STA-REQ-0013** Supported `timing_type` values and arc classification
([`tristate_bus`](../../../verification/analytic/sta/tristate_bus/DERIVATION.md)) [@liberty2017;
@bhasker2009static, ch. 3]:
- combinational data arcs: `combinational` (also when `timing_type` is absent),
  `combinational_rise`, `combinational_fall`;
- sequential launch arcs: `rising_edge`, `falling_edge`;
- three-state arcs on outputs with `three_state`: `three_state_enable`, `three_state_enable_rise`,
  `three_state_enable_fall`, `three_state_disable`, `three_state_disable_rise`,
  `three_state_disable_fall` (propagated from the enable pin to the three-state output,
  STA-REQ-0223);
- timing checks: `setup_rising`, `setup_falling`, `hold_rising`, `hold_falling`, `recovery_rising`,
  `recovery_falling`, `removal_rising`, `removal_falling`;
- asynchronous preset/clear arcs: `clear` and `preset` are retained in the library view and are not
  propagated as combinational data arcs (asynchronous controls are checked by `recovery_*` and
  `removal_*`, STA-REQ-0224);
- unanalyzed arc kinds (`non_seq_setup_*`, `non_seq_hold_*`, `min_pulse_width`, `minimum_period`,
  `skew_*`) are counted per instantiated cell in `completeness.unanalyzed_arcs` and reported as one
  aggregated T warning per kind rather than per-library-cell notes.

## Superseded library rule (legacy engine, removed by Z1)

**STA-REQ-0011** Supported `timing_type` values: `combinational` (also when absent),
`combinational_rise`, `combinational_fall`, `rising_edge`, `falling_edge`, `setup_rising`,
`setup_falling`, `hold_rising`, `hold_falling`, `recovery_rising`, `recovery_falling`,
`removal_rising`, `removal_falling`, `clear`, `preset`. Other types (for example
`three_state_enable`, `min_pulse_width`, `skew_*`, `non_seq_*`) are ignored with a note per cell.

## Superseded per-tag slew rules (legacy engine, removed by Z1)

**STA-REQ-0101** Every node carries, for each launch tag (STA-REQ-0201) and each transition
(rise, fall), a late arrival time, an early arrival time, a late transition time and an early
transition time. An absent value means that no path of that tag and transition reaches the node.

**STA-REQ-0115** Propagation: the late arrival of a node for (tag, transition) is the maximum over
incoming arcs of source late arrival plus arc delay, and its late transition is the maximum of the
arc output transitions over the same arcs (worst-transition merge). The early values use minima and
the early source values, with the early load. A cell arc evaluates its delay and output transition
from the source transition of the same analysis (late or early).

## Superseded launch, relation and latch rules (legacy engine, removed by Z1)

**STA-REQ-0201** A launch tag is a pair (clock, source edge) with edge rise or fall. Arrival times
of a tag are measured from its launch edge time `t_L` (the first rise or fall of the clock
waveform). Data propagation starts at:
- input ports with `set_input_delay -clock C value`: tag (C, rise), or (C, fall) with
  `-clock_fall`; arrival `value` (the `-max` value for the late and the `-min` value for the early
  analysis), transition from `set_input_transition` (default 0) for both rise and fall;
- the output of every edge arc whose clock pin is in the network of clock C: the tag is (C, e)
  where e is the source edge that makes the pin's active edge (the active edge itself for positive
  sense, the opposite edge for negative sense); arrival is the clock latency plus the arc delay at
  the clock transition;
- the source ports of every clock C (clock used as data): a rise with tag (C, rise) and a fall with
  tag (C, fall), both at the clock latency with the clock transition. These arrivals propagate
  like any data, with the delays of the cells they pass, so that logic fed by a clock is timed;
  they end at clock pins, where edge arcs do not propagate.
Input ports without an input delay, and nodes reached only from them, carry no arrival.

**STA-REQ-0210** For a launch tag (L, e_L) and a capture edge (C, e_C), the setup relation `D_s` is
the smallest positive value of `t_C + k T_C - t_L` over integers k, where `t_L` and `t_C` are the
edge times and `T_C` the capture period; the search runs over the least common multiple of the
two periods, which must be at most 1000 times the larger period (else an error). The hold relation
is `D_h = D_s - T_C`.

**STA-REQ-0213** Latches. A latch output launches its own tag at the opening edge of its enable
(the active edge of its edge arc, STA-REQ-0201), and its D-to-Q arc is an ordinary combinational
arc, so data arriving during the transparent phase propagates through it with its original tag
(time borrowing). Data passes only while the latch is open: in the late analysis, the arrival that
enters the D-to-Q arc is limited to at most the latest arrival that meets the setup check at the
closing edge, `D_s + lat_C - s` with the setup time `s` of the data transition at the D pin's late
transition time and the clock transition (the maximum time borrowed). In the early analysis an
arrival passes the D-to-Q arc only if it is not earlier than the opening edge (the smallest
non-negative relation from the tag's launch edge to the opening edge, plus the latency); an earlier
change appears at the opening edge through the enable-to-output arc, which launches the latch's
own tag. This is
conservative: every path through the latch is checked downstream with the later of the two launch
possibilities. The setup slack at D follows the time-borrowing convention
[@bhasker2009static, ch. 8]: with `O` the opening edge relation of the tag (the setup relation to
the latch's launch edge, STA-REQ-0210) and `S` the setup-check slack against the closing edge
(STA-REQ-0211), the slack is `O + lat_C - arr_late` when data arrives before the opening edge,
0 when it arrives while the latch is open and before the required time of the closing edge
(the borrowed time is checked downstream), and `S` (negative) when it arrives later.

## Superseded constant and check rules (replaced in version 0.4 by STA-REQ-0116, 0159, 0161, 0229)

The rules below are those of versions up to 0.4.0 before the transitions next to logic constants
were aligned with the delay calculation of the pin (STA-REQ-0116). STA-REQ-0159 replaces
STA-REQ-0152 (it adds the disabling of a unate arc that the residual function contradicts and lets
arcs disabled by logic values carry their transitions), STA-REQ-0161 replaces STA-REQ-0154 (latches
with a constant enable), and STA-REQ-0229 replaces STA-REQ-0211 (the related pin transition is the
one of the capture clock path). The legacy engine keeps citing STA-REQ-0211.

**STA-REQ-0152** Disabling combinational arcs controlled by constants
([`const_tielo_and2`](../../../verification/analytic/sta/const_tielo_and2/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. For a combinational arc from input pin `p` to output pin
`y` with boolean output expression `f`:
- if `p` itself is at constant `0` or `1`, or `y` evaluates to constant `0` or `1` under Kleene
  evaluation, the arc `p -> y` is disabled;
- more generally, after substituting all constant input pins of the cell into `f`, if the cofactors
  of the residual function with respect to `p` are identically equal (`f|_{p=0} == f|_{p=1}` for
  every assignment to the remaining `X` inputs), the output is independent of `p` and the arc
  `p -> y` is disabled.
The disable reason is `case_analysis` if any constant input of the cell was seeded or derived from
`set_case_analysis` / `set_logic_*`, and `constant` otherwise. Disabled arcs propagate neither
arrivals nor pin transitions (STA-REQ-0108).

**STA-REQ-0154** Sequential pins held constant
([`const_flop_d`](../../../verification/analytic/sta/const_flop_d/DERIVATION.md))
[@bhasker2009static, ch. 7; @sdc2016]. When a sequential data pin (`D`) or asynchronous control pin
(`RN`, `SN`) is at constant `0` or `1`, it carries no signal transition, so no setup, hold,
recovery or removal check is timed at that pin (its endpoint status is `no_data_arrival`,
STA-REQ-0226). The cell's `CK -> Q` edge arc still launches both output transitions if `CK` is in a
clock network, because the stored state is not treated as constant across clock cycles unless `clear`
or `preset` is held active. When a sequential clock pin (`CK`, `CKN`) or latch enable pin (`G`) is
at constant `0` or `1`, the clock network does not propagate through it, its edge arcs do not
launch, and checks referencing that clock pin have status `unclocked`.

**STA-REQ-0211** Setup and hold checks. For a check arc between a constrained pin `D` and a related
clock pin `CK` in the network of clock `C`, the capture edge is the source edge of `C` that makes
the arc's active edge at `CK` (`*_rising`: a rise at `CK`; `*_falling`: a fall at `CK`). For each
launch tag reaching `D` and each transition `x` of `D`:
- setup time `s` = `rise_constraint` (`x` rise) or `fall_constraint` (`x` fall) of the `setup_*`
  arc evaluated at `(related_pin_transition = slew_CK_late, constrained_pin_transition =
  slew[D][x][late])`;
  setup slack = `D_s + lat_C(early) - s - u_setup - arr[D][tag][x][late]`;
- hold time `h` from the `hold_*` arc evaluated at `(related_pin_transition = slew_CK_early,
  constrained_pin_transition = slew[D][x][early])`;
  hold slack = `arr[D][tag][x][early] - (D_h + lat_C(late) + h + u_hold)`.
Here `lat_C` is the capture clock latency (`-min` for setup, `-max` for hold) and `u` the clock
uncertainty (`set_clock_uncertainty`) of the capture clock. A data transition whose constraint
table is absent from the check arc is not checked.

