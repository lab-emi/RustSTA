# Numbers, handles, text, and tabular sections

Part of the [workspace execution and envelope protocol](protocol.md) of the `ruststa` command.

**PROTO-REQ-0100** Numbers in JSON envelopes use `canon_f64` (converting `-0.0` to `+0.0` and
emitting shortest round-trip IEEE 754 `f64` decimals for `_ns`, `_pf`, and `_s` fields, and `0.1`
rounded decimals for `cell_delay_pct` and `wire_delay_pct`). In `--format text` mode, nanoseconds
and picofarads are formatted to 4 decimals (`0.0000` for `-0.0`), or full round-trip precision when
`--exact` is passed; missing values render as `—` in text and `null` in JSON.

**PROTO-REQ-0110** Every `rusteda.v1` envelope on a timed revision includes the three standard
header objects in fixed key order:
- `scope`: `corner`, `wire_model`, `parasitics`, `drc_status`, `wire_load`, `delay_calculator`,
  `clock_model`, `cppr`, `backend`, `precision` (plus `corners` when multi-corner);
- `completeness`: `constrained_endpoints`, `unconstrained_endpoints`, `clock_domains`,
  `unconstrained_by_reason`, `unconstrained_inputs`, `disabled_arcs`, `unanalyzed_arcs`;
- `metrics`: `wns_ns`, `tns_ns`, `violating_endpoints`, `whs_ns`, `ths_ns`,
  `hold_violating_endpoints`, `worst_setup_slack_ns`, `worst_hold_slack_ns`.

**PROTO-REQ-0120** Every object handle emitted by `ruststa` matches `[A-Za-z0-9_-]+` without
underscores between prefix and number: `path<N>` (1-based rank over setup checks then hold checks),
`inst<N>`, `net<N>`, `pin<N>`, and `port<N>` (`1 +` canonical row in `NETLIST v1`), `mm<N>`
(1-based mismatch rank in `ruststa check`), and `sel<N>` (selection entry from `ui_state.json`).

**PROTO-REQ-0130** `ruststa report` provides eight tabular section models from `sta.rbin`:
`summary`, `paths`, `endpoints`, `pins`, `clocks`, `checks`, `unconstrained`, and `notes`. Each
section object in JSON contains `{columns, total, matched, shown, rows}`.

**PROTO-REQ-0131** Filter expressions (`ruststa report --filter <EXPR>`): supports `setup<N`,
`hold<N`, `slack<N`, `violating`, `clock=<NAME>`, `kind=setup|hold`, and substring matching
`endpoint~<SUBSTR>`, `pin~<SUBSTR>`, or `name~<SUBSTR>`. Malformed filter expressions are rejected
with `USAGE` (`error[A0003]`, exit `1`).

**PROTO-REQ-0132** Sorting (`ruststa report --sort <KEY> [--reverse]`) uses `f64::total_cmp` for
numeric keys (`slack`, `setup`, `hold`, `arrival`, `required`, `cap`, `slew`, `period`, `fanout`,
`rank`) with deterministic tie-breaking by object name or index so row order never depends on
`--threads`. Unknown sort keys are rejected with `USAGE` (`error[A0003]`, exit `1`).
