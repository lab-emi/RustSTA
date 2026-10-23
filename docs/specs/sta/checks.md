# RustSTA clocks, timing checks and slack

## Clocks and startpoints

**STA-REQ-0200** The clock network of a clock is the set of nodes reachable from its source ports
through enabled net arcs and enabled combinational cell arcs, not passing through edge arcs nor
through the data-to-output arcs of latches (a clock that reaches a latch's data input is data
there). A node of the network records the clock and its sense: positive when reached only through
positive-unate arcs and an even number of negative-unate arcs, negative for an odd number, and both
when a non-unate arc or reconvergence with different senses reaches it. In Phase 2a clocks are
ideal: a clock edge reaches every node of its network at the source edge time plus the clock
latency (`set_clock_latency`, default 0), with the clock transition (`set_clock_transition`,
default 0). Cell delays in the clock network are not used in ideal-clock mode.

**STA-REQ-0217** Launch tags and input-delay clock latency
([`io_delay_latency`](../../../verification/analytic/sta/io_delay_latency/DERIVATION.md))
[@sdc2016; @bhasker2009static, ch. 7]. A launch tag is a pair `(clock_edge, exc_state)` where
`clock_edge = (C, e)` names a clock `C` and source edge `e` (`rise` or `fall`), and `exc_state` is
the exception state (`exceptions.md`). Arrival times of a tag are measured relative
to its launch edge time `t_L` (the first `rise` or `fall` of `C`'s waveform). For clock `C` and
edge `e`, let `lat_src(C, e, an)` and `lat_net(C, e, an)` be the `-source` and network clock
latencies (`set_clock_latency`, default 0; `-max` / `-late` for `an = late`, `-min` / `-early` for
`an = early`, per edge `e`), and `lat(C, e, an) = lat_src(C, e, an) + lat_net(C, e, an)`. Data
propagation starts at:
- input ports with `set_input_delay -clock C value`: tag `(C, rise)`, or `(C, fall)` with
  `-clock_fall`; the start arrival in analysis `an` is
  `value(an) + (0 if -source_latency_included else lat_src(C, e, an)) + (0 if -network_latency_included else lat_net(C, e, an))`;
  the port transition comes from `set_input_transition` (default 0) or `set_driving_cell`
  (STA-REQ-0218);
- the output of every enabled edge arc (`rising_edge`, `falling_edge`) whose clock pin is in the
  network of clock `C`: tag `(C, e)` where `e` is the source edge that produces the pin's active
  edge (`e = active` for positive sense, the opposite edge for negative sense); the launch arrival
  is `lat(C, e, an) + delay_CKQ(slew_clk, C_out)`;
- the source ports of every clock `C` (clock used as data): a rise with tag `(C, rise)` at
  `lat(C, rise, an)` and a fall with tag `(C, fall)` at `lat(C, fall, an)`, both with the clock
  transition, propagating through combinational logic and stopping at sequential clock pins.

**STA-REQ-0218** Output-delay latency flags and `set_driving_cell`
([`io_delay_latency`](../../../verification/analytic/sta/io_delay_latency/DERIVATION.md))
[@sdc2016; @liberty2017]. On `set_output_delay -clock C value` (captured by `(C, rise)`, or
`(C, fall)` with `-clock_fall`), the capture clock latency `lat_C` used in STA-REQ-0212 is
`(0 if -source_latency_included else lat_src(C, e_C, an_C)) + (0 if -network_latency_included else lat_net(C, e_C, an_C))`,
with `an_C = early` (`-min`) for setup and `an_C = late` (`-max`) for hold. On an input port with
`set_driving_cell -lib_cell CELL -pin PIN`, the port's drive delay for transition `tr` in analysis
`an` is `delay_arc(s_in, C_port) - delay_arc(s_in, 0.0)` of the driving cell's combinational arc to
`PIN` (added to the port's input-delay start arrival), and the port's pin transition
`slew[port][tr][an]` is `slew_arc(s_in, C_port)`.

**STA-REQ-0219** One-sided `-max` and `-min` I/O delays
([`one_sided_delays`](../../../verification/analytic/sta/one_sided_delays/DERIVATION.md))
[@sdc2016; @bhasker2009static, ch. 7]. When `set_input_delay` on a port and clock edge specifies
`-max` without a matching `-min`, only the `late` start arrival is seeded and the `early` arrival
from that port remains absent (and conversely when `-min` is given without `-max`). When
`set_output_delay` on an output port and clock edge specifies `-max` without `-min`, only the setup
check is evaluated at the port and no hold check is created (and conversely when `-min` is given
without `-max`). Each one-sided I/O constraint is recorded with a diagnostic note.

## Clock relations and checks

**STA-REQ-0220** Setup and hold relations over the common period
([`two_clocks_hold`](../../../verification/analytic/sta/two_clocks_hold/DERIVATION.md))
[@bhasker2009static, ch. 7 and 8; @sapatnekar2004timing]. For a launch edge `(L, e_L)` with first
waveform edge `t_L` and period `T_L` and a capture edge `(C, e_C)` with first waveform edge `t_C`
and period `T_C`, times are converted to integer femtoseconds (`10^-15 s`) and the common period is
`P = lcm(T_L, T_C)` (`P <= 1000 * max(T_L, T_C)`, else a T error). Over all launch edges
`l = t_L + k * T_L` in `[t_L, t_L + P)`:
- the setup relation `D_s` is the smallest strictly positive capture-minus-launch difference:
  `D_s = min_l (min { c = t_C + j * T_C : c > l } - l)`;
- the hold relation `D_h` is the largest non-positive capture-minus-launch difference (i.e. the
  closest capture edge at or before a launch edge):
  `D_h = max_l (max { c = t_C + j * T_C : c <= l } - l)`.
When `T_L == T_C` and the waveforms are identical, `D_s = T_C` and `D_h = 0`. When `T_L != T_C`,
`D_h` is in general strictly greater than `D_s - T_C` (for example, `T_L = 3 ns, T_C = 2 ns` with
`t_L = t_C = 0` has `D_s = 1 ns` at `l = 3, c = 4` and `D_h = 0 ns` at `l = 0, c = 0`).

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

**STA-REQ-0212** Output ports with `set_output_delay -clock C value` are endpoints captured by
`(C, rise)`, or `(C, fall)` with `-clock_fall`: setup slack = `D_s + lat_C(early) - v_max - u_setup
- arr_late`, hold slack = `arr_early - (D_h + lat_C(late) - v_min + u_hold)`, where `lat_C` obeys
the latency-inclusion flags of STA-REQ-0218 and one-sided bounds obey STA-REQ-0219.

## Latches, three-state buses, recovery/removal and clock gating

**STA-REQ-0221** Latch transparency window, opening-edge uncertainty and
`set_max_time_borrow`
([`latch_open_uncertainty`](../../../verification/analytic/sta/latch_open_uncertainty/DERIVATION.md))
[@sakallah1990checktc; @bhasker2009static, ch. 8]. For a level-sensitive latch enabled by clock `C`
whose transparent pulse has width `W` between its opening source edge `e_open` and its closing
source edge `e_close`, a tag launched at `(L, e_L)` is checked against the closing edge at relation
`D_close = D_s((L, e_L) -> (C, e_close))` and the opening edge of that same pulse at
`D_open = D_close - W`. With capture latency `lat_C`, setup constraint `s`, setup uncertainty `u_s`
of `C`, and optional `set_max_time_borrow B` on the latch or clock:
- the opening required time is `R_open = D_open + lat_C(early) - u_s` (setup uncertainty applies to
  the opening edge as well as the closing edge);
- the closing required time is
  `R_close = min(D_close + lat_C(early) - s - u_s, R_open + B)` when `B` is specified, and
  `D_close + lat_C(early) - s - u_s` otherwise;
- for late arrival `a = arr[D][tag][x][late]`, the setup slack at `D` is `R_open - a` when
  `a <= R_open` (no time borrowed), `0.0` when `R_open < a <= R_close` (time borrowed: `b = a -
  R_open`), and `R_close - a` (negative, violating) when `a > R_close`;
- the hold check at `D` is evaluated against the closing edge hold relation
  `D_h = D_h((L, e_L) -> (C, e_close))`:
  `hold_slack = arr[D][tag][x][early] - (D_h + lat_C(late) + h + u_h)`.

**STA-REQ-0222** Latch `D -> Q` re-launch in the opening-edge tag and `Q` slew
([`latch_relaunch`](../../../verification/analytic/sta/latch_relaunch/DERIVATION.md))
[@sakallah1990checktc; @bhasker2009static, ch. 8]. The latch `G -> Q` enable arc launches the
latch's opening-edge tag `tag_open = (C, e_open)` at `lat_C + delay_GQ(slew_G, C_Q)`. The `D -> Q`
combinational arc is a tag-mapping arc that maps every borrowing arrival at `D` into `tag_open`
(whose time origin is `D_open`):
- a late arrival `a = arr[D][tag][tr_D][late]` passes through `D -> Q` if and only if `a > R_open`
  (it borrows into the transparent window); it is clipped at `R_close` so it enters `D -> Q` at
  `min(a, R_close) - D_open` relative to `tag_open` and contributes
  `(min(a, R_close) - D_open) + delay_DQ(slew[D][tr_D][late], C_Q)` to `Q` in `tag_open` (merged
  with the `G -> Q` launch at `Q`); arrivals with `a <= R_open` are already waiting at the gate
  when the latch opens and are represented by the `G -> Q` launch arc;
- the pin transition `slew[Q][tr_Q][an]` at `Q` is the transition of the `G -> Q` enable arc.

**STA-REQ-0223** Three-state enable/disable propagation and bus merging
([`tristate_bus`](../../../verification/analytic/sta/tristate_bus/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. On a three-state output pin `Z` (`three_state` in
Liberty), `three_state_enable` (`Z -> 1` in `cell_rise`/`rise_transition`, `Z -> 0` in
`cell_fall`/`fall_transition`) and `three_state_disable` (`0 -> Z` in
`cell_rise`/`rise_transition`, `1 -> Z` in `cell_fall`/`fall_transition`) propagate from the enable
pin `EN` to `Z` alongside the data arc `A -> Z`. When multiple three-state output pins drive a
shared bus net, the net's capacitance sums all load pins, all three-state driver output-pin
capacitances (STA-REQ-0121) and any port/wire load, and the net merges arrivals (`max` for `late`,
`min` for `early` per tag and transition) and pin slews (`max` for `late`, `min` for `early` per
transition) across all enabled drivers.

**STA-REQ-0224** Recovery and removal checks and the `async_default` path group
([`recovery_removal`](../../../verification/analytic/sta/recovery_removal/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 8]. `recovery_rising` / `recovery_falling` and
`removal_rising` / `removal_falling` check arcs on asynchronous control pins (`clear`, `preset`)
are evaluated with the setup (`recovery`) and hold (`removal`) formulas of STA-REQ-0211 for the
deasserting transition(s) present in the constraint tables. Their endpoints carry check roles
`recovery` and `removal` in the `async_default` path group, and contribute to the overall late
(`setup`) and early (`hold`) WNS, TNS and violating-endpoint counts.

**STA-REQ-0216** Clock-gating checks [@bhasker2009static, ch. 10]. A combinational cell input pin
in the network of clock `C` is a gating clock pin when another input pin of the cell (the enable) is
in no clock network and the output function is constant whenever the clock pin is at some value
`v` (for an AND-like gate `v = 0`: active-high gating; for an OR-like gate `v = 1`: active-low
gating). The enable must be stable while the clock pin is at `1 - v`: the leading edge is the
transition of the clock pin away from `v` and the trailing edge the transition back to `v`, each
mapped to a source edge of `C` through the pin's sense (STA-REQ-0200).

**STA-REQ-0225** Active-high (AND) and active-low (OR) clock-gating relations
([`clock_gating`](../../../verification/analytic/sta/clock_gating/DERIVATION.md))
[@bhasker2009static, ch. 10]. For each launch tag `(L, e_L)` and transition reaching a clock-gating
enable pin gated by clock `C`:
- for active-high (AND) gating (`v = 0`), the active pulse starts at the leading edge (clock pin
  `0 -> 1`) with setup relation `D_s(leading)` and ends at the trailing edge (clock pin `1 -> 0`)
  with hold relation `h(trailing)`, the smallest non-negative relation from the launch edge to the
  trailing edge (`min { c_trail >= l } - l`);
- for active-low (OR) gating (`v = 1`), from first principles the active pulse is the low phase,
  starting at the leading edge (clock pin `1 -> 0`) with setup relation `D_s(leading)` and ending
  at the trailing edge (clock pin `0 -> 1`) with hold relation `h(trailing) = min { c_trail >= l }
  - l`.
Setup slack is `D_s(leading) + lat_C(early) - u_setup - arr_late`, and hold slack is
`arr_early - (h(trailing) + lat_C(late) + u_hold)` (gating margins default to 0).

## Endpoints, required times and paths

**STA-REQ-0214** The slack of an endpoint is its smallest slack over tags, transitions and checks of
the kind (late: setup/recovery/latch-setup/clock-gating-setup/output-max; early:
hold/removal/latch-hold/clock-gating-hold/output-min). The worst negative slack (WNS) of a kind is
the smallest endpoint slack if negative, else 0; the total negative slack (TNS) is the sum of the
negative endpoint slacks in canonical endpoint order. Both are reported overall, per corner, and
per path group.

**STA-REQ-0226** Endpoint status taxonomy and completeness
([`endpoint_status`](../../../verification/analytic/sta/endpoint_status/DERIVATION.md))
[@sdc2016; @bhasker2009static, ch. 7]. Every potential endpoint is classified into exactly one
status:
- `constrained`: at least one timed check (setup/recovery or hold/removal) applies to the endpoint;
- `unclocked`: sequential check pin whose related clock pin is not in any clock network (including
  a clock pin held constant, STA-REQ-0154);
- `no_data_arrival`: clocked check pin that no launch tag reaches (for example, fed by a primary
  input without `set_input_delay`, by a constant pin of STA-REQ-0154, or by arcs disabled by
  constants);
- `false_path_only`: every path reaching the endpoint is excluded by `set_false_path` or
  `set_clock_groups` (`exceptions.md`);
- `unconstrained_output`: top-level output port without `set_output_delay` and without an
  applicable `set_max_delay` / `set_min_delay`;
- `disabled`: endpoint whose check arcs are disabled by `set_disable_timing` or `when`.
Every endpoint whose status is not `constrained` is counted in `unconstrained_endpoints` and broken
down by status in `completeness.unconstrained_by_reason`.

**STA-REQ-0227** Backward required-time propagation and per-pin slack
([`latch_relaunch`](../../../verification/analytic/sta/latch_relaunch/DERIVATION.md))
[@hitchcock1982timing; @bhasker2009static, ch. 7]. Required times are seeded at every constrained
endpoint per `(tag, tr, an)` (`req_late =arr_late + setup_slack`, `req_early = arr_early -
hold_slack`) and propagated backwards in reverse topological order: across an enabled arc `u -> v`
with delay `d`, `req[u][tag][tr_in][late] = min(req[u][tag][tr_in][late], req[v][tag][tr_out][late]
- d)` and `req[u][tag][tr_in][early] = max(req[u][tag][tr_in][early], req[v][tag][tr_out][early] -
d)`, mapping across latch `D -> Q` tag maps for borrowed arrivals. Each node stores the required
time and slack (`req_late - arr_late` for setup, `arr_early - req_early` for hold) of its
worst-slack tag per transition.

**STA-REQ-0215** A reported path is reconstructed backwards from an endpoint by following, at each
node, the incoming arc and source transition that produce the worst arrival of the tag (ties broken
by canonical topological ID), up to the startpoint. Each path point lists the pin, the cell and arc
kind (`clock`, `launch`, `cell`, `net`, `input`), the transition, the incremental delay, the
arrival and the pin transition time.

**STA-REQ-0228** Clock-pin path startpoint and setup-first handle ranking
([`recovery_removal`](../../../verification/analytic/sta/recovery_removal/DERIVATION.md))
[@hitchcock1982timing; @bhasker2009static, ch. 7]. A register-launched path starts at the launch
clock pin (`r/CK` or latch `l/G`, with the clock latency point and the `CK -> Q` launch arc as its
first steps) rather than at the register output `Q`; an input-delay path starts at the input port.
Path handles (`path1`, `path2`, …) rank all late (`setup` / `recovery`) paths in increasing slack
order before any early (`hold` / `removal`) paths, with ties broken by canonical endpoint and
predecessor topological ID.
