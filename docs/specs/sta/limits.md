# Pin limit checks, per-corner parasitics and the hold-fix report

Version 0.4 adds the checks that a block-level signoff reads next to setup and hold: the minimum
pulse width and the minimum period of clock pins, and the electrical design rules
`max_transition`, `max_capacitance` and `max_fanout` [@liberty2017; @sdc2016;
@bhasker2009static, ch. 7]. It binds extracted parasitics to every analyzed corner, lets a command
require its wire model, and reports hold violators in a form that a physical-design tool can turn
into delay-cell insertions. The engine module is `engine::limits`; the command-line surface is in
[cli.md](cli.md).

## Pulse width and period

**STA-REQ-0800** Minimum pulse width. A `timing` group of type `min_pulse_width` constrains the
pin that owns it: its `rise_constraint` is the minimum width of the high pulse and its
`fall_constraint` the minimum width of the low pulse, each a table of the transition time of the
pin (`related_pin_transition`) or a scalar. For every clock `C` that reaches an instance pin `p`
with such groups, and for each pulse sense, the opening transition (rise for the high pulse, fall
for the low pulse) comes from a source edge `e_o` of `C` (the same edge for a positive sense, the
opposite edge for a negative sense), and the closing transition from the next source edge
`e_c = opposite(e_o)`, at waveform time `t(e_c)` moved one period later when it does not follow
`t(e_o)`. With the clock arrival `L` of the clock model (ideal or propagated), the width is taken
with the latest opening and the earliest closing arrival:
`W = (t(e_c) + L(e_c, close, early)) - (t(e_o) + L(e_o, open, late))`. The required width is the
largest constraint of the pin's groups for that sense, each evaluated at the late transition time
of the opening transition; the slack is `W - required`, the worst over the opening edges. Clock
uncertainty and CPPR do not enter the check (CPPR is off in this version). A group without a
constraint table is a note of the library view and checks nothing. These groups are pulse checks
and are no longer counted in `completeness.unanalyzed_arcs`.

**STA-REQ-0801** Conditional groups and minimum period. Groups with a `when` condition are taken as
all possible: the worst (largest) constraint over the groups of a pin applies, and one aggregated
note `T0132` per corner counts the conditional groups. A `minimum_period` group constrains the
clock period at its pin: its `rise_constraint` and `fall_constraint` are evaluated at the late
transition time of the pin's rising and falling transition; the required period is the largest of
them over the groups, and the slack is `period(C) - required` for every clock `C` at the pin. The
pin-level attributes `min_pulse_width_high`, `min_pulse_width_low` and `min_period` are not part of
the compiled library model; on the standard cells of both default libraries every pin that states
them also has the groups, and the attributes equal the worst group, so the groups give the same
checks.

**STA-REQ-0802** Unclocked pulse pins. An instance pin with pulse groups that no clock reaches is
not checked: the corner counts it in `unclocked_pulse_pins`, the command envelope reports the
worst corner's count as `completeness.unchecked_pulse_width_pins`, and the analysis emits one
warning `T0133` per corner. Its pulse checks are never reported as met.

## Electrical design rules

**STA-REQ-0803** `max_transition`. Every port and instance pin with a propagated transition time is
checked: for each transition, the late transition time against the tightest limit that applies
to that transition (STA-REQ-0805); the row reports the transition with the smallest slack
`limit - value`. A pin with a transition time but no limit is counted as `unlimited`.

**STA-REQ-0804** `max_capacitance` and `max_fanout`. Every driver of a net (an input or inout port,
an output or inout instance pin) is checked against the limits of its net. The capacitance is the
total of the net's alias class (the pin capacitances of the library, the wire capacitance of the
wire model or of the extracted parasitics, and `set_load` on ports), the largest over the bounds
for each transition. The fanout is the sum of the `fanout_load` of the instance load pins (the
pin's attribute or the library's `default_fanout_load`) and of `set_fanout_load` on the output
ports of the net (zero without one); the driver itself is not its own load.

**STA-REQ-0805** Limits and their sources. The limit of a check is the smallest of the limits that
apply, and the row records its source (`liberty`, `sdc_design`, `sdc_port`, `sdc_clock`; ties keep
that order): the library limit of an instance pin (its `max_transition`, `max_capacitance` or
`max_fanout`, or the library default of LIBERTY-REQ-0018); a constraint on the current design
(every pin and port); a constraint on a port (the port itself; for capacitance and fanout only an
input or inout port, which drives a net); and a constraint on a clock `C`, for the transition and
the capacitance, whose domain is the clock network of `C` (with `-clock_path`) and the data pins
that `C` launches (with `-data_path`), both without either option, and whose `-rise` or `-fall`
restricts it to one transition. Every constraint object holds one value per slot (design, port,
clock path and transition), and a later command on the same slot replaces an earlier one. The
options `-clock_path`, `-data_path`, `-rise` and `-fall` on the design or a port are ignored with a
warning `T0130`; an object that is not checked (a pin, cell, net or library object, an output
port for capacitance or fanout, a clock for fanout, an input port for `set_fanout_load`) is ignored
with a warning `T0131`. `set_max_transition`, `set_max_capacitance`, `set_max_fanout` and
`set_fanout_load` are analyzed and carry no `D0021`; `set_min_capacitance` and
`set_port_fanout_number` remain recorded with `D0021`.

## Tables, summaries and reports

**STA-REQ-0806** The `limits` table. `ruststa run` and `ruststa update` commit, with `sta.rbin` and
with the same upstream tables, a table `limits` (`limits.rbin`, rbin kind `LIMITS`, version 1): per
corner the counts of every kind (checked, violating, worst slack, unlimited, kept rows) and the
kept rows with the names of their pin, net and clock, the value, limit, slack, transition and
source. Equal inputs give byte-identical tables; `update --verify-full` requires the incremental
table to equal the table of a full analysis byte for byte, as for `sta.rbin`; a timing session
verified against a full analysis compares the limit checks of every corner. An `update` whose base
revision has a current `sta.rbin` but no current `limits` table analyzes it again. The summary of
the table holds `limit_rows` and `limit_rows_truncated`.

**STA-REQ-0807** Summaries and status. The `SCOPE` of `sta.rbin` records the counts of every kind
of every corner. The revision summary of `sta` reports, as the worst corner: `max_trans_violating`,
`max_cap_violating`, `max_fanout_violating` with `drv_status` (`met` or `violated`), and
`min_pulse_width_violating` (high and low) and `min_period_violating` with `pulse_width_status`.
The `sta` status holds `timing` (`violated` when a setup, hold, pulse width or period check fails),
`drv` and `pulse_width`. The `SummaryRec` rows of a corner carry its transition and capacitance
violations. A table without these counts (an analysis that did not check them) reports the counts
as `null` and the statuses as `unchecked`, never as zero (CONTRIBUTING Rule 4.2). The command
envelopes of `run`, `update`, `report`, `path`, `inspect` and the summaries of `whatif` add the
metrics `min_pulse_width_violating`, `worst_min_pulse_width_slack_ns`, `min_period_violating`,
`worst_min_period_slack_ns`, `pulse_width_status`, `max_transition_violating`,
`worst_max_transition_slack_ns`, `max_capacitance_violating`, `worst_max_capacitance_slack_pf`,
`max_fanout_violating`, `worst_max_fanout_slack` and `drv_status`, and the completeness
`unchecked_pulse_width_pins` and `drv_unlimited` (per design rule).

**STA-REQ-0810** Bounded rows. Every check is counted, and at most `--max-limit-rows` rows
(default 100000, at most 10000000) are kept per corner and kind, the worst slack first, ties by
node and clock, in a heap bounded by that number (AGENTS rule 3.4). The analysis visits every node,
net driver and clock pin once: `O(V + E + P * C)` for `P` pulse pins and `C` clocks. The kept rows,
`min(budget, nodes) x 6 x 48` bytes per corner at most, are part of the state budget of the corner
(`--max-state-gib`, `T0023`). When rows are left out, a warning `T0134` names the kept and the
checked count.

**STA-REQ-0811** Report sections. `ruststa report` has the sections `limits` (the counts per corner
and kind), `pulse_width` (high and low rows), `min_period`, `drv` (the three design rules) and
`max_transition`, `max_capacitance`, `max_fanout`, read from the `limits` table; without
`--corner` every corner is reported. Rows are ordered by slack, worst first; `--sort pin` (or
`name`) orders by pin and `--sort value` by value, largest first; `--reverse` reverses. The filters
are `violating`, `slack<X`, `pin~TEXT` (also matching the net) and `clock=NAME`; another filter is a
usage error. A revision without a `limits` table is an error `T0136`.

**STA-REQ-0812** Required times and the endpoint kind. `inspect`, its instance pins and the `pins`
report give `required_setup_ns` (also as `required_ns`) and `required_hold_ns`: the late and the
early required time at the transition of the worst late and early slack of the pin, which is the
required time of the worst setup and hold path to it. In the `endpoints` and `checks` sections of
`report`, `--kind setup` (or `hold`) keeps the endpoints with a setup (or hold) slack and applies
the filters `violating` and `slack<X` and the sort `slack` to that slack alone; `both` keeps the
earlier behaviour.

## Parasitics per corner

**STA-REQ-0820** Extracted parasitics are bound to named corners (RCLI-REQ-0075). A revision whose
`parasitics` table holds sources for some corners analyzes each corner with its own source; a
corner without one is an error `T0070` that names the annotated corners, never a fallback to ideal
wires or to another corner's source. Only the sources of the analyzed corners are read and parsed.
A version 1 table annotates its one corner.

**STA-REQ-0821** Required wire model. `run`, `update` and `whatif` take
`--require-wire-model spef_rc|ideal|wireload_lumped`: a revision without parasitics fails a
requirement of `spef_rc` before it is analyzed, and an analysis in which a corner uses another wire
model fails after it; both are `INPUT_REJECTED` with an error `T0135` and commit nothing. Without the
option the wire model follows the revision, as before.

## Hold-fix report

**STA-REQ-0830** `report --section hold_fix [--target-margin NS] [--corner NAME] [--limit N]` lists,
per corner (all, or `--corner`), every endpoint whose hold slack is below the margin (default 0),
worst first and at most `--limit` per corner, with its instance and cell, capture clock, hold slack,
the delay it needs `max(0, margin - slack)`, its setup slack, its net, the net's driver and fanout,
and its worst hold path in that corner: startpoint, launch clock, arrival, required time, check
value and the stages (pin, instance, cell, arc, transition, net with its driver and fanout,
incremental delay, arrival, transition time, and the setup and hold slack of the pin, which bound
the delay that the pin can take). Counts give the endpoints with a hold check, the violating, the
listed and the shown ones.

**STA-REQ-0831** The merged list of `hold_fix` gives every endpoint below the margin in any reported
corner, with its worst hold slack and corner, the delay it needs, its smallest setup slack over the
corners, and its hold slack in each corner, worst first and bounded by `--limit`.

**STA-REQ-0832** The worst hold path of an endpoint in a corner is the first hold-family path of
that endpoint and corner in rank order, read from the ranked paths of the timing table; the nets
come from the netlist of the revision, whose pins must be those of the table. The report reads
only the shown endpoints' paths, with one pass over the path ranks.

## Verification

The checks are derived by hand on synthetic libraries with constant tables (`tests/limits.rs` of
the engine, `tests/signoff_cli.rs` of the command line). A black-box comparison with a reference
timing tool on the routed pipeline fixture of the suite's physical-design tool, on both default
libraries at three corners each with their own extracted parasitics, is kept with that tool's
harness and results; it compares the pulse width, period and design-rule rows, the violator sets
of planted tightened limits, and the endpoint slacks.
