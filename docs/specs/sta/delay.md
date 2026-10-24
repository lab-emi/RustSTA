# RustSTA timing graph and delay calculation

## Timing graph

**STA-REQ-0100** The timing graph has one node per cell pin and per top-level port. Its propagation
arcs are:
- net arcs from each driver of a net to each of its loads;
- cell arcs from `related_pin` to the pin that owns the timing group, for combinational types,
  three-state enable/disable types (`three_state_enable*`, `three_state_disable*`, STA-REQ-0017),
  and edge types (`rising_edge`, `falling_edge`); arcs of type `clear` and `preset` are not
  propagated (asynchronous controls are covered by recovery and removal checks, STA-REQ-0224).
Check arcs (`setup_*`, `hold_*`, `recovery_*`, `removal_*`) are not propagation arcs.

**STA-REQ-0102** A cycle of propagation arcs is broken before propagation: within each non-trivial
strongly connected component, the subgraph is traversed depth-first from the nodes in increasing
order of their canonical name, following the outgoing arcs of a node in increasing order of the
canonical name of their target, and every back-edge arc that closes a cycle is disabled with reason
`cycle_break` and reported. Acyclic components require no name sort, and the broken-arc set and all
timing values are independent of netlist statement order (STA-REQ-0503).

## Per-pin slews and per-tag arrivals

**STA-REQ-0108** Per-pin slews and per-tag arrivals
([`per_pin_slew`](../../../verification/analytic/sta/per_pin_slew/DERIVATION.md))
[@bhasker2009static, ch. 3; @hitchcock1982timing]. Each node carries:
- per transition (`rise`, `fall`) and analysis bound (`early`, `late`), one pin transition time
  (slew) `slew[node][tr][an]`. For a startpoint or untagged primary input port it is initialized from
  `set_input_transition` (default `0.0`), a `set_driving_cell` transition, or `set_clock_transition`
  (on clock sources). At every downstream node, `slew[node][tr][late]` is the maximum of the output
  transitions over all enabled incoming arcs that can produce `tr` at `node` (and
  `slew[node][tr][early]` is the minimum), including arcs whose source pin carries a transition even
  when no launch tag reaches that source pin;
- per launch tag `(clock_edge, exc_state)` (STA-REQ-0217), transition (`rise`, `fall`) and analysis
  bound (`early`, `late`), an arrival time `arr[node][tag][tr][an]` (or absent when no path of that
  tag and transition reaches `node`).

**STA-REQ-0109** Arc evaluation and arrival reduction with per-pin slews
([`per_pin_slew`](../../../verification/analytic/sta/per_pin_slew/DERIVATION.md))
[@bhasker2009static, ch. 3; @hitchcock1982timing]. An enabled cell arc from source pin `u` (input
transition `tr_in`) to target pin `v` (output transition `tr_out`) in analysis `an` (`early` or
`late`) evaluates its delay `d` and output transition `s_out` at `(slew[u][tr_in][an],
C_net(v, tr_out, an))` once per `(tr_in, tr_out, an)`, independent of the launch tag. For every
launch tag `tag` present at `(u, tr_in, an)`, the candidate arrival at `(v, tag, tr_out, an)` is
`arr[u][tag][tr_in][an] + d`, reduced across incoming arcs in topological CSR order by `max` for
`late` and `min` for `early`.

**STA-REQ-0116** Transitions ignore logic values
([`const_mux_select`](../../../verification/analytic/sta/const_mux_select/DERIVATION.md))
[@bhasker2009static, ch. 3 and 7]. The transition of a pin is a property of the pin, not of the
paths through it, and the delay calculation that gives it ignores logic values: the merge of
STA-REQ-0108 runs over every arc that carries a transition into the pin, each evaluated at the
transition of its source pin with its library timing sense:
- the enabled arcs;
- the arcs that only logic values disable (reasons `constant`, `case_analysis` and `false_when`,
  STA-REQ-0159, STA-REQ-0155), with the transitions that a sense refinement (STA-REQ-0153) removes
  from an enabled arc;
- the clock-to-output arcs of sequential cells that launch no arrival because logic values
  disable them or because no clock reaches their clock pin (STA-REQ-0161);
- the `D -> Q` arc of a latch whose enable is held constant (STA-REQ-0161).
Arcs that break a combinational cycle or that `set_disable_timing` disables carry nothing. The
outputs of tie cells (constant drivers without timing arcs) have the transition 0; a port held
constant by case analysis keeps its input transition. These arcs carry no arrival and no required
time. An arc that logic values disable and that would close a loop with these transitions is left
out of the merge and counted with warning `T0149`. The propagation of a propagated clock merges the
same arcs, with the input ports and tie cells as its transition sources (STA-REQ-0710).

## Net loads and delay lookup

**STA-REQ-0110** The load of a net depends on the transition of its driver and on the analysis. For
a rise it sums over its load pins the `rise_capacitance`, and for a fall the `fall_capacitance`
(each falling back to `capacitance` when absent); when a pin has `rise_capacitance_range` or
`fall_capacitance_range`, the early analysis uses the range minimum and the late analysis the range
maximum instead. Port loads (`set_load`) are added: the `-max` value in the late analysis, the
`-min` value in the early analysis (both equal to the plain value when not distinguished). Driver
output-pin capacitances (STA-REQ-0121) and wire-model capacitances (STA-REQ-0122 to 0125) are added
to form the total net capacitance `C_net`.

**STA-REQ-0111** In the `Ideal` and `WireloadLumped` wire models, net arcs have zero delay and copy
transitions (`slew[driver][tr][an]`) unchanged from the driver to each load pin.

**STA-REQ-0112** A cell arc with sense `positive_unate` maps an input rise to an output rise and a
fall to a fall; `negative_unate` maps rise to fall and fall to rise; `non_unate` maps each input
transition to both output transitions (subject to constant sense refinement, STA-REQ-0153).
`combinational_rise` and `combinational_fall` produce only the named output transition. An edge arc
produces both output transitions from the active clock edge only (rise at the clock pin for
`rising_edge`, fall for `falling_edge`).

**STA-REQ-0113** The delay of an arc producing an output rise is `cell_rise` evaluated at the input
transition time and the output load for a rise; the output transition time is `rise_transition` at
the same point; likewise `cell_fall` and `fall_transition` for an output fall. A missing table gives
delay 0 (or transition 0) and a note naming the cell and arc.

**STA-REQ-0114** Table lookup. A table has one, two or three index vectors (from the table, else
from its template, STA-REQ-0014) and variables named by the template (`input_net_transition`,
`total_output_net_capacitance`, `related_pin_transition`, `constrained_pin_transition`,
`related_out_total_output_net_capacitance`, `output_net_length`); the variable order of the
template decides which argument indexes which dimension. A value is interpolated multilinearly in
each dimension between the two nearest index points, and extrapolated linearly from the two
outermost points outside the index range. A scalar table (one value) is constant. Indices must be
strictly increasing; otherwise the library is rejected.

**STA-REQ-0120** Extrapolation clamping and range warnings
([`slew_clamp`](../../../verification/analytic/sta/slew_clamp/DERIVATION.md)) [@liberty2017;
@bhasker2009static, ch. 3]. When linear extrapolation outside a transition table's index range
produces a negative output transition (`rise_transition` or `fall_transition`), the transition is
clamped to zero: `slew_out = max(0.0, raw_slew)`. Delay tables (`cell_rise`, `cell_fall`) and
constraint tables (`rise_constraint`, `fall_constraint`) are not clamped at zero: a negative
extrapolated delay or constraint value is used as computed. Any lookup outside a table's index
range, or any pin whose slew or load exceeds `max_transition` or `max_capacitance`, emits one
aggregated T warning per construct.

**STA-REQ-0121** Driver output-pin capacitance
([`driver_pin_cap`](../../../verification/analytic/sta/driver_pin_cap/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 3]. Every driver cell output pin connected to a net (whether
a single combinational/sequential output or multiple `three_state` outputs on a bus) contributes its
own pin capacitance (`rise_capacitance`/`fall_capacitance` or `capacitance`, with early/late range
bounds as in STA-REQ-0110) to the net's capacitive load `C_net`, summed in canonical `NET_PINS`
order together with the load pins, port loads and wire capacitance.

## Wire-load models

**STA-REQ-0122** Wire model hook (`WireModel`)
([`wireload_lumped`](../../../verification/analytic/sta/wireload_lumped/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 4]. Interconnect loading and delay are abstracted behind
`WireModel`, which supplies the driver load (`Lumped { c }` in Phase 2a; `Pi { c_near, r, c_far }`
in Phase 2b) and per-load wire delay and slew degradation (`(0.0, slew_driver)` in `Ideal` and
`WireloadLumped`).

**STA-REQ-0123** Lumped wire-load capacitance and slope extrapolation
([`wireload_lumped`](../../../verification/analytic/sta/wireload_lumped/DERIVATION.md))
[@liberty2017; @bhasker2009static, ch. 4]. In `WireloadLumped` mode with active `wire_load` group
`W`, an internal net (not connected to any top-level port, STA-REQ-0124) with `F` connected
instance load pins has wire length `L(F)` from `W.fanout_length`:
- if `F` matches an entry `(F_i, L_i)` or lies between adjacent entries `(F_0, L_0)` and
  `(F_1, L_1)`, `L(F) = L_0 + (L_1 - L_0) * (F - F_0) / (F_1 - F_0)`;
- if `F` exceeds the largest fanout entry `(F_max, L_max)`,
  `L(F) = L_max + W.slope * (F - F_max)`;
- if `F` is below the smallest fanout entry `(F_min, L_min)`,
  `L(F) = max(0.0, L_min - W.slope * (F_min - F))`.
The net's wire capacitance is `C_wire = W.capacitance * L(F)` (unless overridden on the net by
`set_load -wire_load`), and wire resistance is ignored in `WireloadLumped` mode.

**STA-REQ-0124** Port-connected nets in lumped wire-load mode
([`wireload_lumped`](../../../verification/analytic/sta/wireload_lumped/DERIVATION.md))
[@liberty2017; @sdc2016]. Any net connected to a top-level port (input, output or inout) receives
no wire-load capacitance from the `wire_load` table (`C_wire = 0.0`), unless an explicit
`set_load` command applies to the port or net; its load is the sum of its connected pin
capacitances and explicit `set_load` values.

**STA-REQ-0125** Wire-load model selection and mode
([`wireload_lumped`](../../../verification/analytic/sta/wireload_lumped/DERIVATION.md))
[@liberty2017; @sdc2016]. For each corner, the active `wire_load` group is selected in priority
order:
1. explicit SDC `set_wire_load_model -name <name>` (or `set_wire_load_selection_group` matched
   against the sum of instantiated cell areas);
2. the corner library's `default_wire_load_selection` table matched against total cell area;
3. the corner library's `default_wire_load`.
When none is present (or `WireModel::Ideal` is selected), wires are `ideal`. The wire-load mode
(`top`, `enclosed`, `segmented`) comes from `set_wire_load_mode` or `default_wire_load_mode`
(default `top`); for a flattened netlist all nets belong to the top module, and the selected model
name and `wire_model` (`ideal` or `wireload_lumped`) are recorded in the run scope and status.
