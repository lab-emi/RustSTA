# RustSTA timing exceptions and clock groups

Timing exceptions (`set_false_path`, `set_multicycle_path`, `set_max_delay`, `set_min_delay`) and
clock groups (`set_clock_groups`) decide which timing checks apply to a path and which relation
their required times use [@sdc2016; @bhasker2009static, ch. 7 and 8]. This file defines their
meaning in the analysis; the command syntax is that of the SDC reader
([docs/specs/sdc](../sdc/sdc.md)). Every requirement has a hand derivation on the linear synthetic
library in
[`verification/analytic/sta-exceptions`](../../../verification/analytic/sta-exceptions/README.md).
A requirement whose implementation is not accepted yet carries the marker `(planned: <package>)`.

Notation. Times are in ns. For a path, `L` is its launch clock with period `T_L`, `C` its capture
clock with period `T_C`, and `D_s` and `D_h` are the setup and hold relations of its launch and
capture edges ([checks.md](checks.md)): over the common period of the two clocks, `D_s` is the
smallest difference between a capture edge and an earlier launch edge (the minimum over the launch
edges `l` of the earliest capture edge after `l`, minus `l`), and `D_h` is the largest difference
between a capture edge and a launch edge at or after it (the maximum over `l` of the latest capture
edge at or before `l`, minus `l`). Arrival and required times of a path are measured from its
launch edge.

## Exceptions and the paths they name

**STA-REQ-0250** Exception types and analyses. Each exception command defines one
exception with a type, a path specification (the options `-from`, `-through`, `-to`, their
`-rise_`/`-fall_` forms and `-rise`/`-fall`, STA-REQ-0252 to 0255) and the analyses it applies
to. The setup analysis comprises every late check (setup, recovery, clock-gating setup, latch data
setup, output-delay `-max` checks), the hold analysis every early check (hold, removal,
clock-gating hold, latch data hold, output-delay `-min` checks).
- `set_false_path`: the setup analysis with `-setup`, the hold analysis with `-hold`, both with
  neither or both options.
- `set_max_delay value`: the setup analysis; `set_min_delay value`: the hold analysis.
- `set_multicycle_path N` (an integer, which may be 0 or negative): with `-setup` only, the setup
  analysis with setup multiplier `N`; with `-hold` only, the hold analysis with hold multiplier
  `M = N`; with neither option, or with both, both analyses with setup multiplier `N` and hold
  multiplier 0. Both options together get warning D0113, because the hold multiplier stays 0.
  `-start` and `-end` choose the clock whose period scales the multiplier (STA-REQ-0260, 0261).
`-comment` has no effect. A command that names no `-from`, `-through` or `-to` object has no
effect and gets warning D0110.

**STA-REQ-0251** Paths. An exception is matched against timing paths. A path runs
through propagation arcs from a startpoint to an endpoint:
- startpoints are input ports and the clock pins at which edge arcs start (the clock pins of
  flip-flops, the enable pins of latches);
- endpoints are the constrained pins of timing checks (setup and hold, recovery and removal,
  latch data, clock-gating enable pins) and output ports;
- the launch clock edge is the clock and source edge (rise or fall at the clock source) of the
  path's launch, the capture clock edge the clock and source edge of the check's capture;
- the start transition is the transition at the startpoint: the clock pin's own active transition
  (rise for a rising-edge flip-flop, also when the clock reaches the pin through an inversion), or
  the data transition at an input port; the end transition is the data transition at the
  endpoint.
A path to a latch data pin ends there; the data that passes the latch's data-to-output arc starts
new paths at the latch enable pin (STA-REQ-0267). Clock pins are not endpoints, and the outputs of
edge arcs and the data pins of checks are not startpoints.

**STA-REQ-0252** `-from`. A clock matches the paths launched by that clock; an
input port or a pin that is a startpoint matches the paths that start there; a cell matches the
paths that start at any of its pins that are startpoints. `-rise_from` and `-fall_from` also
require, for a port or pin, the start transition rise or fall and, for a clock, the launch source
edge rise or fall (so `-rise_from` a clock does not match a flip-flop that launches on the falling
source edge, even when its clock pin rises). Any other object (a pin that is not a startpoint, a
cell without startpoint pins, a net) gets warning D0102 and is ignored.

**STA-REQ-0253** `-to`, `-rise` and `-fall`. A clock matches the paths captured by
that clock; an output port or a pin that is an endpoint matches the paths that end there; a cell
matches the paths that end at any of its endpoint pins. `-rise_to` and `-fall_to` also require,
for a port or pin, the end transition rise or fall and, for a clock, the capture source edge rise
or fall. The options `-rise` and `-fall` restrict an exception, whatever its other options, to the
paths whose end transition is a rise or a fall. Any other object (a pin that is not an endpoint,
such as a clock pin or a cell output, a cell without endpoint pins) gets warning D0102 and is
ignored.

**STA-REQ-0254** (planned: E13) `-through`. Each `-through` option names a set of pins, ports,
nets and cells, and the options are ordered. A path matches when it passes through an object of
every set in the order of the options: a pin or port when the path contains it (startpoints and
endpoints included), a net when the path contains a pin of the net, a cell when the path contains
a pin of the cell. `-rise_through` and `-fall_through` also require the transition of the path at
that pin.

**STA-REQ-0255** Matching. An exception matches a path when every option it names
matches the path (the options are conjunctive) and an option matches when any of its objects
matches (the objects of one option are alternatives). Objects ignored under STA-REQ-0252 to 0254
do not count, so an exception with an option whose objects are all ignored matches no path; it
gets warning D0110. An option may mix clocks with ports, pins and cells; for the precedence of
STA-REQ-0257 such an option counts as naming a design object for the paths it matches through a
design object, and as naming a clock for the paths it matches only through a clock.

## Precedence

**STA-REQ-0256** Precedence of types. Clock groups apply first: a path whose launch
and capture clocks are separated (STA-REQ-0266) is not timed, whatever its exceptions. Otherwise
each analysis of a path uses at most one exception, its winner among the exceptions that match
the path and apply to the analysis. A false path beats a maximum or minimum delay, which beats a
multicycle path, whatever the specificity of the exceptions.

**STA-REQ-0257** Specificity. Among exceptions of the same type, the one with the
highest specificity for the path wins. The specificity is the sum of 16 when the path matches a
`-from` port, pin or cell, 8 when it matches a `-to` port, pin or cell, 4 when the exception has
`-through`, 2 when it matches a `-from` clock and 1 when it matches a `-to` clock. Pins, ports and
cells rank alike, and so do `rise_`/`fall_` qualifiers and their absence. In decreasing order:
`-from` pin with `-to` pin; `-from` pin with `-to` clock; `-from` pin; `-from` clock with `-to`
pin; `-to` pin; `-from` clock with `-to` clock; `-from` clock; `-to` clock.

**STA-REQ-0258** Ties and conflicts. Among exceptions of equal type and
specificity, one that applies only to the analysis at hand (`-setup` or `-hold` only, or a maximum
or minimum delay) beats one that applies to both analyses, and among the remaining ones the one
defined later in the constraint evaluation order wins. When exceptions that tie in type,
specificity and analyses both match a path and differ in effect (multiplier, `-start`/`-end`, or
delay value), the analysis reports warning D0111 once per pair of commands, with both source
locations.

## Effects

**STA-REQ-0259** False paths. A path whose winner in an analysis is a false path is
not timed in that analysis: it contributes no slack to its endpoint. An endpoint none of whose
paths is timed in an analysis because of false paths has no slack of that kind and the status
`false_path_only`. Exceptions change no delay or transition, and no arrival time except through
the unclocked starts of STA-REQ-0264 and the latch rule of STA-REQ-0267: they select which checks
apply and their required times.

**STA-REQ-0260** Multicycle setup relation. A path's setup multiplier `N` and its
period `T_s` come from the winner among the multicycle paths that match the path and apply to the
setup analysis (STA-REQ-0256 to 0258 applied to multicycle paths alone): `T_s = T_C` with `-end`
(the default) and `T_s = T_L` with `-start`; without such an exception `N = 1`. When a multicycle
path (or no exception) wins the setup analysis, the setup check uses the relation
`D_s + (N - 1) * T_s` in place of `D_s`; latencies, uncertainty, setup times and output delays
apply unchanged.

**STA-REQ-0261** Multicycle hold relation. When a multicycle path (or no exception)
wins the hold analysis, the hold check uses the relation `D_h + (N - 1) * T_s - M * T_h` in place
of `D_h`, with `N` and `T_s` of STA-REQ-0260 (taken also when a false path or a maximum delay wins
the setup analysis) and the hold multiplier `M` of the winner: `M` from a multicycle path with
`-hold` only, 0 from one that applies to both analyses, 0 without a winner. `T_h = T_L` with
`-start` (the default for the hold multiplier) and `T_h = T_C` with `-end`. The hold check thus
moves with the setup multiplier unless a hold multiplier moves it back.

**STA-REQ-0262** Maximum delay. When `set_max_delay V` wins the setup analysis of a
path, its required time is `V` from the launch edge in place of the clock relation:
`required = V + lat_C - s - v_max - u_s`, where `lat_C` is the capture clock latency of setup checks
when the endpoint is the constrained pin of a timing check (not for an output port), `s` the setup
time of the check (0 for clock-gating checks), `v_max` the `-max` output delay of an output port,
and `u_s` the setup uncertainty of the capture clock when the endpoint has a capture clock. The
arrival is the path's usual late arrival, including the launch latency and the input delay.

**STA-REQ-0263** Minimum delay. When `set_min_delay V` wins the hold analysis of a
path, `required = V + lat_C + h - v_min + u_h`, with the capture latency of hold checks, the hold
time `h` of the check (0 for clock-gating checks), the `-min` output delay `v_min` of an output
port and the hold uncertainty `u_h`, under the conditions of STA-REQ-0262; the hold slack is the
early arrival minus the required time.

**STA-REQ-0264** Unclocked ends. A path from an input port without input delay has no
launch clock; it is timed in an analysis only when a maximum (setup) or minimum (hold) delay wins
it, with arrival 0 and the port's input transition at the port. An output port without output
delay is an endpoint only for such delays, with neither capture clock nor output delay, so
`required = V`. Clocks never match these unclocked ends, and multicycle paths do not apply to
them.

**STA-REQ-0265** `-ignore_clock_latency`. A maximum or minimum delay with
`-ignore_clock_latency` leaves out every clock latency and the clock uncertainty: the arrival
excludes the launch latency (of the launching clock pin or of the input delay), and
`required = V - s - v_max` (setup) or `V + h - v_min` (hold).

**STA-REQ-0266** Clock groups. A `set_clock_groups` command with
`-asynchronous`, `-logically_exclusive` or `-physically_exclusive` (equivalent here, because this
version does not analyse crosstalk) separates two clocks when each is in one of its `-group`
lists and no list contains both; with a single `-group`, every other defined clock forms the
second group. A path whose launch and capture clocks are separated by any command is timed in
neither analysis (STA-REQ-0256). `-name` has no effect. With `-allow_paths` the command separates
no clocks (the option matters only to crosstalk analysis) and gets note D0114.

**STA-REQ-0267** Latches. Exceptions apply to the checks at a latch data pin like
to any check (STA-REQ-0268). Only the arrivals that are timed at the data pin pass the
data-to-output arc with borrowed time ([checks.md](checks.md)), each in its own analysis: a false
path passes no arrival in the analyses it wins, and a multicycle path that wins the setup analysis
moves the opening and closing edges of the latch check, and with them the borrowing, by
`(N - 1) * T_s`. A maximum or minimum delay that wins at a latch data pin checks the pin like a
flip-flop data pin against `V` (no borrowing), and its arrivals pass no borrowed time. The paths
through the data-to-output arc start at the latch enable pin (STA-REQ-0251): `-from` the enable
pin or the latch cell matches them, and `-from` an earlier startpoint does not.

**STA-REQ-0268** Check kinds. Exceptions apply alike to every check of an endpoint:
setup and hold, recovery and removal, latch data checks, output delays and clock-gating checks.
A multicycle path at a clock-gating enable pin shifts the leading-edge (setup) relation by
`(N - 1) * T_s` and the trailing-edge (hold) relation as in STA-REQ-0261.

## Analysis

**STA-REQ-0269** Path semantics and states. The slack of an endpoint in an analysis
is the minimum, over the paths that end there (with each launch, start transition and end
transition), of the path slack under the path's winners (STA-REQ-0256 to 0268); paths that are
not timed are left out. An implementation that carries exception states with its arrivals must
give exactly this result. The number of distinct exception states at a node is bounded by a limit
of at least 64 states; exceeding it is a T error that names the node, and states are never merged
to stay within the limit.

**STA-REQ-0270** Diagnostics and scope. The analysis reports, at the source location
of the command, warning D0102 for each ignored object (STA-REQ-0252 to 0254), warning D0110 for an
exception without a usable object, warning D0111 for each conflict of STA-REQ-0258, note D0112 for
each exception that is used nowhere (it wins no analysis of a timed path and supplies no setup
multiplier of STA-REQ-0261), warning D0113 (STA-REQ-0250) and note D0114 (STA-REQ-0266). The scope
of the run lists the number of exceptions of each type, the number of clock-group commands and
the number of exceptions with note D0112; endpoints with status `false_path_only` are counted
among the unconstrained endpoints by reason.

**STA-REQ-0271** Exceptions in the analysis. The full analysis of `ruststa run` and
`ruststa update` applies the exceptions and clock groups of the revision as STA-REQ-0250 to
STA-REQ-0270 define them: each startpoint launches a propagation tag that carries its `-from`
state (STA-REQ-0251, STA-REQ-0252; the paths relaunched by a latch start at its enable pin,
STA-REQ-0267), and each check of each tag resolves its winners at its endpoint, end transition and
capture edge: a false path or a clock group removes the check, a multicycle path moves its
relation, and a maximum or minimum delay replaces it (at an output port without capture clock
latency). Every endpoint slack and status equals the result of STA-REQ-0269. What this version
does not analyse is never dropped silently: `-ignore_clock_latency` is error `T0071`
(STA-REQ-0265); a maximum or minimum delay that could time a path from an input port without input
delay or to an output port without output delay (STA-REQ-0264) gets warning `T0072`, and such
paths are not timed; an exception that resolves differently for the rising and the falling data
of a latch data pin is error `T0073`; and a timing session, which keeps the base tags of the
clocks, refuses constraints with exceptions or clock groups with error `T0074`.
