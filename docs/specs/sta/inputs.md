# RustSTA inputs

## Netlist

**STA-REQ-0001** The netlist is structural Verilog (IEEE 1364-2005, the structural subset of
VERILOG-REQ-0302) or a frozen workspace netlist (`FrozenNetlist`, CORE-REQ-0130): one or more
modules, the top module named by `--top` or unique otherwise. Module instances whose type is a
module of the netlist are flattened; instance names are joined with `/` into hierarchical names.
Instances of library cells become timing cells. `assign` of one net to another (or a constant)
joins the nets. Constants `1'b0`/`1'b1` are nets without transitions ([constants.md](constants.md)).

**STA-REQ-0002** Every net has at most one non-three-state driver: a top-level input or inout port,
a cell output pin, or a constant. Multiple drivers on a net are a T error unless all cell drivers
on the net are three-state outputs (`three_state`, STA-REQ-0013). A net without a driver is
reported and its loads receive no arrival (`no_data_arrival`).

**STA-REQ-0003** A cell instance must use a cell of the libraries, and each connected pin's
direction must match the library pin direction; unknown cells, unknown pins and direction
mismatches are T errors. Unconnected cell inputs are reported with one aggregated T warning;
unconnected outputs are allowed.

## Cell libraries and corners

**STA-REQ-0010** Cell timing comes from Liberty libraries [@liberty2017] (LIBERTY-REQ): pin
direction, function, `three_state` expression, and capacitance (`rise_capacitance`/`fall_capacitance`
when given, else `capacitance`), timing groups with `related_pin`, `timing_sense`, `timing_type`
and `when`, and the tables `cell_rise`, `cell_fall`, `rise_transition`, `fall_transition`,
`rise_constraint`, `fall_constraint`. Library units (`time_unit`, `capacitive_load_unit`) are
converted to nanoseconds and picofarads.

**STA-REQ-0012** `timing_sense` is `positive_unate`, `negative_unate` or `non_unate`; when absent
it is derived from the output function (unate in the related pin if the function is monotone in
it), else `non_unate`. Constant inputs refine a `non_unate` sense when the residual function becomes
monotone (STA-REQ-0153). Edge arcs (`rising_edge`, `falling_edge`) launch both output transitions.

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

**STA-REQ-0014** Lookup table axes and index overrides
([`tristate_bus`](../../../verification/analytic/sta/tristate_bus/DERIVATION.md)) [@liberty2017].
Delay, transition and constraint tables have 1, 2 or 3 axes typed by their template variable role
(`input_net_transition`, `total_output_net_capacitance`, `related_pin_transition`,
`constrained_pin_transition`, `related_out_total_output_net_capacitance`, `output_net_length`),
with per-table `index_1`, `index_2` and `index_3` vectors overriding the template's default index
vectors when present.

**STA-REQ-0015** Per-corner library resolution and multi-corner analysis
([`two_corners`](../../../verification/analytic/sta/two_corners/DERIVATION.md)) [@bhasker2009static,
ch. 2; @liberty2017]. Each operating corner of a revision resolves its cells from that corner's
library list. A duplicate cell name within the same corner is a T error (never silent
first-definition-wins). The analysis evaluates every corner of the revision (or the corners named
by `--corner`), sharing the netlist graph and compiled constraints across corners while keeping
compiled lookup tables, net loads, slews, arrivals, required times and slacks independent per
corner.

**STA-REQ-0016** Slew thresholds and `slew_derate_from_library`
([`thresholds`](../../../verification/analytic/sta/thresholds/DERIVATION.md)) [@liberty2017;
@bhasker2009static, ch. 3]. Each library header supplies `input_threshold_pct_rise`,
`input_threshold_pct_fall`, `output_threshold_pct_rise`, `output_threshold_pct_fall`,
`slew_lower_threshold_pct_rise`, `slew_lower_threshold_pct_fall`, `slew_upper_threshold_pct_rise`,
`slew_upper_threshold_pct_fall` and `slew_derate_from_library` (default `1.0`). They are recorded in
the corner metadata; libraries combined in the same corner must agree on these attributes (a
mismatch within one corner is a T error). In the lumped NLDM delay model (`Ideal` and
`WireloadLumped`), delay, transition and constraint tables are already characterised in library slew
units and are evaluated without scaling by `slew_derate_from_library` (`slew_derate_from_library`
and trip-point thresholds apply only to RC interconnect slew degradation in Phase 2b).

## Constraints

**STA-REQ-0020** Constraints are read from an SDC file [@sdc2016] (SDC-REQ). Supported commands
include `create_clock`, `set_input_delay`, `set_output_delay`, `set_load`, `set_input_transition`,
`set_driving_cell`, `set_clock_uncertainty`, `set_clock_latency`, `set_clock_transition`,
`set_case_analysis`, `set_logic_zero`, `set_logic_one`, `set_disable_timing`, `set_false_path`,
`set_multicycle_path`, `set_max_delay`, `set_min_delay`, `set_clock_groups`, `set_max_time_borrow`,
`set_wire_load_model`, `set_wire_load_mode`, `group_path`, `set_units`, and design-rule commands,
together with the object queries `get_ports`, `get_pins`, `get_cells`, `get_nets`, `get_clocks`,
`all_inputs`, `all_outputs`, `all_clocks`, `all_registers` and `current_design`. Unsupported
commands are reported with diagnostic codes so that no constraint is silently ignored.

**STA-REQ-0021** Clock sources are top-level ports or pins. A clock's rising edge occurs at the
first waveform value (default 0) and its falling edge at the second (default half the period),
repeated every period.

**STA-REQ-0022** Constraint options that the analysis accepts but does not model are reported by
the analysis, never dropped silently: an input or output delay with `-level_sensitive` is timed as
an edge-triggered delay with warning `T0041`; `set_load -subtract_pin_load` is applied as a plain
load with warning `T0042`; and `set_operating_conditions` with `-min`, `-max`, `-min_library`,
`-max_library` or `-analysis_type bc_wc|on_chip_variation` gets warning `T0043`, each corner being
analysed with its own libraries.
