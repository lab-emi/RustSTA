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
constraint of the pin for that sense that the pulse group rule chooses (STA-REQ-0801,
STA-REQ-0816), evaluated at the late transition time of the opening transition; the slack is
`W - required`, the worst over the opening edges. Clock
uncertainty and CPPR do not enter the check (CPPR is off in this version). A group without a
constraint table is a note of the library view and checks nothing. These groups are pulse checks
and are no longer counted in `completeness.unanalyzed_arcs`.

**STA-REQ-0801** Conditional groups and minimum period (the default rule `worst`). Groups with a
`when` condition are taken as all possible: the worst (largest) constraint over the groups of a pin
applies, whatever their order, and one aggregated note `T0132` per corner counts the conditional
groups. A `minimum_period` group constrains the clock period at its pin: its `rise_constraint` and
`fall_constraint` are evaluated at the late transition time of the pin's rising and falling
transition; the required period is the largest of them over the groups, and the slack is
`period(C) - required` for every clock `C` at the pin. The pin attributes `min_pulse_width_high`,
`min_pulse_width_low` and `min_period` (LIBERTY-REQ-0049) are unconditional scalar constraints of
their pin, after its groups, and enter the same largest constraint, so a pin that states only the
attributes is checked with them. On the standard cells of both default libraries every pin that
states the attributes also has conditional groups, and the attribute equals the worst group.

**STA-REQ-0802** Unclocked pulse pins. An instance pin with pulse groups that no clock reaches is
not checked: the corner counts it in `unclocked_pulse_pins`, the command envelope reports the
worst corner's count as `completeness.unchecked_pulse_width_pins`, and the analysis emits one
warning `T0133` per corner. Its pulse checks are never reported as met: the pulse status is
`unchecked` when no pulse check was evaluated and `partial` when some were (STA-REQ-0813).

**STA-REQ-0816** The pulse group rule `last`. `ruststa run` and `update` take
`--pulse-groups worst|last` (default `worst`, STA-REQ-0801). Under `last` the required width of a
sense is the constraint of the last listed `min_pulse_width` group of the pin that states it
(`rise_constraint` for the high pulse, `fall_constraint` for the low pulse), else the pin attribute
of that sense; the required period is the `rise_constraint` of the last listed `minimum_period`
group that states one, at the late rising transition time, else `min_period`. The note `T0132`
names the rule, and a note `T0142` per corner counts the `fall_constraint` tables of
`minimum_period` groups on checked pins that the rule does not read. The rule depends on the order
of the groups in the library; it reproduces the choice that a reference timing tool makes, observed
as a black box by reordering the groups of a copied library (a pin's attribute applies only to a
sense that no group states, and a period group with only a `fall_constraint` is not read). The
default `worst` does not depend on the order and is never smaller.

**STA-REQ-0817** The pulse pin scope. `ruststa run` and `update` take `--pulse-pins all|clock-ends`
(default `all`). Under `clock-ends` a clocked instance pin whose clock passes on through its cell,
that is, an arc of the cell leads from the pin to an output pin of the instance that one of the
pin's clocks reaches (the clock pin of a clock gater, the input of a clock buffer), is not checked:
the corner counts it in `pass_through_pulse_pins`, the command envelope reports the worst corner's
count as `completeness.pass_through_pulse_pins`, a warning `T0141` per corner names the count, and
the pulse status is `partial` (STA-REQ-0813), never `met`. The pins where a clock ends (flip-flop
and latch clock pins, asynchronous pins driven by a clock) are checked under both scopes. A
reference timing tool, observed as a black box, checks only the pins where a clock ends; `all`
also checks the clock pins of clock gaters, whose internal latch has its own minimum pulse width.
The rules of a run are its scope: the envelope reports `scope.pulse_rules` (`groups`, `pins`), the
`SCOPE` of `sta.rbin` records them per corner with `pass_through_pulse_pins`, and the `limits`
table records them in its corner records (STA-REQ-0806).

## Electrical design rules

**STA-REQ-0803** `max_transition`. Every port and connected instance pin (STA-REQ-0819) is
checked with its transition
time: the propagated transition, or for a pin without one its static transition (STA-REQ-0809):
for each transition, the late transition time against the tightest limit that applies to that
transition (STA-REQ-0805); the row reports the transition with the smallest slack
`limit - value`. A pin with a transition time but no limit is counted as `unlimited`. A pin
without any transition time (an untimed pin, STA-REQ-0809) is counted, and its check is not
evaluated where a limit applies. The pins of an ideal clock network follow STA-REQ-0818.

**STA-REQ-0804** `max_capacitance` and `max_fanout`. Every driver of a net (an input or inout port,
a connected output or inout instance pin, STA-REQ-0819) whose net is not constant (STA-REQ-0809)
is checked against the limits of its net. The capacitance is the
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
that `C` launches (with `-data_path`), both without either option, under the clock limit scope
`paths` (STA-REQ-0808), and whose `-rise` or `-fall` restricts it to one transition. Every constraint object holds one value per slot (design, port,
clock path and transition), and a later command on the same slot replaces an earlier one. The
options `-clock_path`, `-data_path`, `-rise` and `-fall` on the design or a port are ignored with a
warning `T0130`; an object that is not checked (a pin, cell, net or library object, an output
port for capacitance or fanout, a clock for fanout, an input port for `set_fanout_load`) is ignored
with a warning `T0131`. `set_max_transition`, `set_max_capacitance`, `set_max_fanout` and
`set_fanout_load` are analyzed and carry no `D0021`; `set_min_capacitance` and
`set_port_fanout_number` remain recorded with `D0021`.

**STA-REQ-0808** The clock limit scope. `ruststa run` and `update` take
`--clock-limits paths|domain` (default `paths`). Under `paths` a clock's design-rule slots apply
as STA-REQ-0805 states: the clock-path slot to its clock network, the data-path slot to the data
pins that it launches. Under `domain` the data-path slot of a clock (written by `-data_path` or by
a command without either option) applies to every pin of its domain, its clock network included,
and a command with `-clock_path` alone writes no slot: it is applied nowhere, and a warning
`T0144` counts such commands. The scope reproduces the choice that a reference timing tool makes,
observed as a black box with each option, both, neither, `-rise` and `-fall`, in either command
order and with ideal and propagated clocks; `paths` follows the definitions of the constraint
format and is never looser on a clock pin whose clock-path limit is the smaller. The two scopes
differ exactly on the checked pins of the clock network of a clock whose clock-path and data-path
slots differ (for a transition): under either scope these pins are counted in
`clock_scope_pins`, and a note `T0147` per corner names the count and the slot that the scope
applies to them. The scope of a run is part of its scope: the envelope reports
`scope.drv_rules.clock_limits`, the `SCOPE` of `sta.rbin` records it per corner, and the `limits`
table records it in bit 2 of the rules of each corner (STA-REQ-0806).

**STA-REQ-0809** Constant nets, untimed pins and static transitions. A net whose logic value is
constant (netlist literals and tie cells, `set_case_analysis` and `set_logic_*`, propagated
through the cells, STA-REQ-0150) never switches: its driver is not checked for
`max_capacitance` and `max_fanout`, and is counted in `constant_drivers`. Its pins still have the
transition that the propagation gives them, because the delay calculation ignores logic values
(STA-REQ-0116): tie outputs 0, and every pin behind them through the arcs that logic values
disable and through the clock-to-output arcs of sequential cells that no clock reaches; these
transitions enter the live pins downstream as well. A pin that no transition reaches (a pin tied
to a netlist literal and its cone) never switches: it is checked for `max_transition` with the
static transition 0 and counted in `static_transition_pins`; a note `T0143` per corner names both
counts. Any other pin without a transition (reached only through an arc that `set_disable_timing`
or loop breaking disables, or on an undriven net) is untimed: it is counted in `untimed_pins`, its
limited transition check is unevaluated (STA-REQ-0813), and a warning `T0145` per corner names the
count. The rules reproduce the pin sets and transitions of a reference timing tool, observed as a
black box on tie cells, buffers and gates fed by ties with controlling and non-controlling
constants, gates with asymmetric arcs (`nand3`, `xor2`, `mux2`) behind ties and case analysis,
literals, latches with a constant enable, an unclocked and a tied-off flip-flop clock, with ideal
wires and extracted parasitics, on both default libraries.

**STA-REQ-0819** Unconnected pins. The design rules check the pins of the netlist: the ports and
the connected instance pins. A library pin of an instance that the netlist leaves unconnected (not
listed, or written `.P()`), such as the unused output of a tie cell with two outputs or the
unused complement output of a flip-flop, drives no net and has no load and no receiver, and a
cell's internal pins (direction `internal`) are not pins of the design: neither is checked, and
their checks are not counted as unevaluated. The unconnected output and inout pins are counted in
`unconnected_output_pins`, and a note `T0148` per corner names the count and the first five
pins; unconnected input pins are reported by `T0010` (STA-REQ-0002). A reference timing tool,
observed as a black box, also lists these pins under the transition limit: the internal and
unconnected input pins with the transition 0, the unconnected outputs with the table transition
of the cell at zero load. A limit below such a zero-load transition therefore makes an
unconnected output a violator of the reference only; this is a declared and counted difference.

**STA-REQ-0818** Ideal clock networks. The pins of the clock network (STA-REQ-0200) of a clock
that is not propagated (`set_propagated_clock`), when every clock that reaches the pin there is
ideal, are not checked for any design rule: their transitions are the ideal clock transition, and
the network that drives them is not yet built. Their limited checks (the transition of every such
pin, the capacitance and fanout of every such driver) are unevaluated (STA-REQ-0813), the corner
counts the pins in `ideal_clock_pins`, and a warning `T0146` per corner names the count, so a
design rule with an ideal clock is `partial` or `unchecked`, never `met`. The network includes
the pins behind clock buffers, inverters and gaters and the data pins that the clock reaches, as
observed of a reference timing tool. Their pulse checks follow STA-REQ-0815.

## Tables, summaries and reports

**STA-REQ-0806** The `limits` table. `ruststa run` and `ruststa update` commit, with `sta.rbin` and
with the same upstream tables, a table `limits` (`limits.rbin`, rbin kind `LIMITS`, version 1): per
corner the rules (bit 0 for the pulse groups `last`, bit 1 for the pulse pins `clock-ends`, bit 2
for the clock limit scope `domain`; other bits are a corrupt table), the unclocked, conditional
and pass-through pulse counts, the constant drivers, static-transition, ideal-clock, untimed,
clock-scope and unconnected output pin counts of the design rules (STA-REQ-0809, STA-REQ-0818,
STA-REQ-0808, STA-REQ-0819), the counts of every kind (checked,
violating, worst slack, unlimited, kept rows) and the kept rows with the names of their pin, net
and clock, the value, limit, slack, transition and source. Equal inputs give byte-identical tables; `update --verify-full` requires the incremental
table to equal the table of a full analysis byte for byte, as for `sta.rbin`; a timing session
verified against a full analysis compares the limit checks of every corner. The backward pass of
`sta.rbin` reads the lowered problem that the analysis kept, or a problem lowered again by the same
function (latch transitions pinned and wire model prepared), so `update` and `run` of one revision
commit the same bytes, with extracted parasitics too. An `update` whose base
revision has a current `sta.rbin` but no current `limits` table analyzes it again. The summary of
the table holds `limit_rows` and `limit_rows_truncated`.

**STA-REQ-0807** Summaries and status. The `SCOPE` of `sta.rbin` records the counts of every kind
of every corner (checked, violating, unlimited, unevaluated, worst slack). The revision summary of
`sta` reports, as the worst corner: `max_trans_violating`, `max_cap_violating`,
`max_fanout_violating` with `max_transition_status`, `max_capacitance_status`, `max_fanout_status`
and `drv_status`, and `min_pulse_width_violating` (high and low) and `min_period_violating` with
`pulse_width_status`, all following STA-REQ-0813. The `sta` status holds `timing` (`violated` when
a setup, hold, pulse width or period check fails), `drv` and `pulse_width`. The `SummaryRec` rows
of a corner carry its transition and capacitance violations. The command envelopes of `run`,
`update`, `report`, `path`, `inspect` and the summaries of `whatif` add the metrics
`min_pulse_width_violating`, `worst_min_pulse_width_slack_ns`, `min_period_violating`,
`worst_min_period_slack_ns`, `pulse_width_status`, `max_transition_violating`,
`worst_max_transition_slack_ns`, `max_transition_status`, `max_capacitance_violating`,
`worst_max_capacitance_slack_pf`, `max_capacitance_status`, `max_fanout_violating`,
`worst_max_fanout_slack`, `max_fanout_status` and `drv_status`, and the completeness
`unchecked_pulse_width_pins`, `pass_through_pulse_pins`, `unevaluated_limit_checks`,
`drv_constant_drivers`, `drv_static_transition_pins`, `drv_ideal_clock_pins`, `drv_untimed_pins`,
`drv_clock_scope_pins`, `drv_unconnected_output_pins` and `drv_unlimited` (per design rule), the
worst corner of each, or the `--corner` alone.

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
`--corner` every corner is reported. The `total` of a row section is the number of checked rows of
its corners and kinds; the section adds `stored` (the kept rows), `truncated` (the checked rows
left out by the row budget) and `unevaluated`, and its `matched` and `shown` count kept rows. Rows are ordered by slack, worst first; `--sort pin` (or
`name`) orders by pin and `--sort value` by value, largest first; `--reverse` reverses. The filters
are `violating`, `slack<X`, `pin~TEXT` (also matching the net) and `clock=NAME`; another filter is a
usage error. A revision without a `limits` table is an error `T0136`.

**STA-REQ-0812** Required times and the endpoint kind. `inspect --pin` (and a pin, port or net
handle) reports for an endpoint, with its endpoint slacks `slack_ns` and `hold_slack_ns`, the
required times of its own checks: `required_setup_ns` (also as `required_ns`) and
`required_hold_ns` are the required times of the worst setup and hold path that ends at it in the
corner, as `ruststa path --to` reports them. For another pin, and for the instance pins of
`inspect --inst` and the `pins` report, which give node slacks, they are the late and the early
required time of the node at the transition of its worst late and early slack; at a latch data
pin the node values also cover the paths through the transparent latch and can be earlier than its
own check's. In the `endpoints` and `checks` sections of `report`, `--kind setup` (or `hold`) keeps
the endpoints with a setup (or hold) slack and applies the filters `violating` and `slack<X` and
the sort `slack` to that slack alone; without `--sort`, `--kind hold` orders the endpoints by hold
slack, worst first (ties by name), so that `--limit N` lists the `N` worst hold endpoints; `both`
keeps the earlier behaviour.

**STA-REQ-0813** Status rules. One rule gives the counts and statuses of the revision summary and
of every command envelope, over the reported corners. A violating count is the worst corner's and
is `null` when its kinds have no checked row. A check whose value, limit or slack is not a finite
number is not evaluated: it is counted as `unevaluated`, never as checked, and a warning `T0137`
per corner and kind names the count. The status of a group of checks (the pulse width and period
checks together, or one design rule) is `violated` when a checked row has a negative slack;
otherwise `unconstrained` when nothing is checked and nothing is left unevaluated (no pin with a
pulse constraint, or no limit of the kind anywhere), `unchecked` when nothing is checked but some
checks were left unevaluated (every pulse pin unclocked, STA-REQ-0802, or every value not finite),
`partial` when some checks were evaluated and others not (unclocked pulse pins, pulse pins left
out by the scope `clock-ends`, STA-REQ-0817, values that are not finite, the limited checks of
untimed pins, STA-REQ-0809, and of ideal clock networks, STA-REQ-0818, or a design rule checked
on some pins or drivers while others have a value but no limit, `completeness.drv_unlimited`), and
`met` when every check was evaluated without violation. `drv_status` is `violated` when a design rule is violated, `met` when all three
are met, `unconstrained` when all three are unconstrained, `unchecked` when none was evaluated, and
`partial` otherwise, for example when `max_fanout` has no limit anywhere. An analysis without limit
counts reports every count as `null` and every status as `unchecked` (CONTRIBUTING Rule 4.2).

**STA-REQ-0814** Paths to an endpoint by corner. `ruststa path --to NAME` and the pin and port
targets give, for each check family that `--kind` selects (setup with recovery, hold with
removal), the worst path of the endpoint in `--corner`, or over all corners without it. It is the
first path of that endpoint, corner and family in rank order. A requested family without a path
(in that corner) gives no path and a warning `T0138`, never a silent empty answer.

**STA-REQ-0815** Pulse width on an ideal clock. On an ideal clock the arrival `L` of each edge at
a pin is the ideal clock latency (`set_clock_latency`, by transition and bound), so the width is
the waveform width moved by the latency difference of the closing and opening edges; the
transition distortion of the clock network is not modeled. The analysis notes `T0139` per corner
with the number of pins checked on an ideal clock. A reference timing tool, compared as a black
box, takes the width from the propagated network for an ideal clock too and ignores the ideal
latency there; the measured difference is recorded with the comparison of the suite's
physical-design tool. Signoff uses propagated clocks (STA-REQ-0710), where both agree.

## Parasitics per corner

**STA-REQ-0820** Extracted parasitics are bound to named corners (RCLI-REQ-0075). A revision whose
`parasitics` table holds sources for some corners analyzes each corner with its own source; a
corner without one is an error `T0070` that names the annotated corners, never a fallback to ideal
wires or to another corner's source. Only the sources of the analyzed corners are read and parsed;
they stay resident during the analysis, so the state estimate of each corner (`--max-state-gib`,
`T0023`) counts the parsed parasitics of every analyzed corner. A version 1 table annotates its one
corner.

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
of planted tightened limits, and the endpoint slacks. On the clock pins of the second default
library, which state conditional pulse groups and the equal pin attributes, the reference tool's
required width follows the last listed group, so its result depends on the order of the groups;
STA-REQ-0801 takes the largest. That comparison is therefore made on a copy of the library whose
pins keep only the attributes, read by both tools, and the order-dependent difference on the
original library is recorded as a measured deviation, not as a pass. The reference tool rejects
`set_max_capacitance` on a clock, so that slot of STA-REQ-0805 has the hand derivations of the
engine suite as its only oracle. The design rules on constant nets, unclocked outputs and
literals (STA-REQ-0809) and on ideal clock networks (STA-REQ-0818) are derived by hand on a
library with tables linear in the input transition and the load (`tests/drv_constants.rs`,
`tests/limits.rs`) and compared, as checked and violating pin sets and transition values, with
the reference tool on a constant-net probe of both default libraries at three corners with
extracted parasitics; the clock limit scopes (STA-REQ-0808) are compared with planted
`-clock_path` and `-data_path` limits: `domain` gives the reference's limits on every pin, and
`paths` differs exactly on the clock network pins, as a declared and counted rule choice.
