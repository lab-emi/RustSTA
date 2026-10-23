# RustSTA user manual

RustSTA (`ruststa`) computes setup, hold, recovery, removal, latch, and clock-gating slacks of a
gate-level netlist inside a RustEDA workspace (`rusteda`), stores the complete timing state in a
memory-mapped binary container (`sta.rbin`), and verifies every endpoint and pin against an
independent workspace checker (`ruststa check`).

See [The `ruststa` and `rusteda` commands](../cli/README.md) for the complete CLI reference and
[`docs/specs/sta/README.md`](../../specs/sta/README.md) for the formal requirements.

## 1. Workspace timing workflow

Every timing run operates on an immutable workspace revision created by `rusteda`:

1. **Initialize a workspace (`rev0`):** `rusteda init -w <WS>` creates `manifest.json`,
   `workspace.lock`, `history.jsonl`, `cache/`, and `revisions/rev0000/`.
2. **Import inputs (`rev1`):** `rusteda import -w <WS> --base-rev rev0 --lib <FILE> [--corner
   <NAME>] --netlist <FILE> --sdc <FILE> [--sdc-env <NAME=VALUE>]...` compiles the Liberty
   libraries into `cache/*.rlib` (`LIBERTY v1`), flattens the structural Verilog netlist into
   `netlist.rbin` (`NETLIST v1`), and compiles the SDC files into `constraints.rbin` (`CONSTR v1`)
   and `constraints/merged.sdc` (`CANON`).
3. **Run static timing (`rev2`):** `ruststa run -w <WS> --base-rev rev1` analyzes every corner of
   the revision (or the subset selected by `--corner`), writes `sta.rbin` (`STA v2`), records
   `summary.sta` and `status.sta` in `meta.json`, and prints the summary or `rusteda.v1` JSON
   envelope.
4. **Query or check (`rev2` / `rev3`):** `ruststa path`, `ruststa inspect`, and `ruststa report`
   read `sta.rbin` without re-running timing; `ruststa check -w <WS> --base-rev rev2` runs the
   independent checker (`--method independent|full|both`) and commits `sta_check.json`.

```console
$ rusteda init -w ./ws
$ rusteda import -w ./ws --base-rev rev0 --lib wt_full.lib --netlist counter.v --sdc counter.sdc
$ ruststa run -w ./ws --base-rev rev1 --max-paths 2 --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa run",
  "workspace": "./ws",
  "base_rev": "rev1",
  "rev": null,
  "new_rev": "rev2",
  "scope": {
    "corner": "default"
  },
  "completeness": {
    "constrained_endpoints": 17,
    "unconstrained_endpoints": 0
  },
  "metrics": {
    "violating_endpoints": 7,
    "hold_violating_endpoints": 0
  },
  "top_paths": [
    {
      "handle": "path1",
      "kind": "setup"
    },
    {
      "handle": "path2",
      "kind": "setup"
    }
  ],
  "truncated_paths": 5,
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
```

When constraints or netlists are updated in a subsequent revision, `ruststa update` reuses the
parent revision's timing store or recomputes the affected cones, and `ruststa check` verifies the
committed slacks:

```console
$ ruststa report -w ./ws --rev rev2 --section summary --format json
$ ruststa path -w ./ws --rev rev2 --to r2/D --kind setup --format json
$ ruststa check -w ./ws --base-rev rev2 --method independent --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa check",
  "workspace": "./ws",
  "base_rev": "rev2",
  "rev": null,
  "new_rev": "rev3",
  "scope": {},
  "completeness": {},
  "metrics": {},
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
```

## 2. Inputs: netlists, libraries, corners, and constraints

Full specification: [`docs/specs/sta/inputs.md`](../../specs/sta/inputs.md) and
[`docs/specs/sdc/sdc.md`](../../specs/sdc/sdc.md).

### Structural netlist (`NETLIST v1`)

`rusteda import --netlist <FILE>` reads structural Verilog, flattens any module hierarchy using `/`
as the path separator, and stores a canonical `FrozenNetlist` in `netlist.rbin`
([`docs/specs/workspace/netlist.md`](../../specs/workspace/netlist.md)):

- Escaped identifiers have their leading `\` and trailing whitespace stripped, with internal `\`
  and `/` escaped as `\\` and `\/` so unescaped `/` unambiguously marks hierarchy boundaries.
- `supply0`/`supply1` nets and `1'b0`/`1'b1` literals map to constant `0`/`1` drivers; connections
  to Liberty `pg_pin` power/ground pins are dropped with an aggregated `V` warning.
- Every cell and pin direction (`CELL_PIN_DIR`) is validated against the revision's libraries.

### Liberty libraries and multi-corner resolution (`LIBERTY v1`)

Each `--lib <FILE>` imported with `rusteda import` is cached verbatim in `cache/<name>-<xxh3>.lib`
and compiled into `cache/<name>-<xxh3>.rlib`
([`docs/specs/workspace/rlib.md`](../../specs/workspace/rlib.md)). A `--corner <NAME>` following
`--lib` assigns that library to the named operating corner (default `"default"`):

```console
$ rusteda init -w ./ws_mc
$ rusteda import -w ./ws_mc --base-rev rev0 --lib wt_full.lib --corner slow --lib wt_15.lib --corner fast --netlist chain_8.v --sdc chain_8.sdc --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "rusteda import",
  "workspace": "./ws_mc",
  "base_rev": "rev0",
  "rev": null,
  "new_rev": "rev1",
  "scope": {},
  "completeness": {},
  "metrics": {},
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
```

All library time, capacitance, resistance, voltage, and current values are scaled to internal units
of nanoseconds (`ns`), picofarads (`pF`), kilohms (`kOhm`), volts (`V`), and milliamperes (`mA`)
(`STA-REQ-0010`).

### SDC constraints (`CONSTR v1`)

`rusteda import --sdc <FILE>... [--sdc-env <NAME=VALUE>]...` evaluates the SDC files in a bounded,
deterministic Tcl interpreter ([ADR 0005](../../adr/0005-sdc-layer.md)):

- **Tcl subset:** `set`, `unset`, `incr`, `append`, `list`, `llength`, `lindex`, `lrange`,
  `lappend`, `concat`, `join`, `split`, `string`, `expr`, `if`, `while`, `for`, `foreach`, `proc`,
  `return`, and `source` (bounded to recursion depth 64 and step budget 1,000,000).
- **Hereditary reproducibility:** the interpreter never reads process environment variables;
  `$::env(NAME)` resolves strictly from `--sdc-env NAME=VALUE` bindings recorded with their hashes
  in `meta.json`.
- **Object queries:** `get_ports`, `get_pins`, `get_cells`, `get_nets`, `get_clocks`, `all_inputs`,
  `all_outputs`, `all_clocks`, and `all_registers` (with `-hierarchical`, `-filter`, `-of_objects`,
  `-quiet`, `-nocase`, `-regexp`).
- **Dual output:** the compiler writes both `constraints.rbin` (`CONSTR v1`, read by the engine)
  and `constraints/merged.sdc` (`CANON`, a flat, Tcl-free canonical SDC file read by the
  independent checker's own parser).

## 3. Delay calculation, slews, and wire models

Full specification: [`docs/specs/sta/delay.md`](../../specs/sta/delay.md) and
[ADR 0004](../../adr/0004-timing-conventions.md).

### Timing graph and cycle breaking

The timing graph creates one node per connected cell pin and top-level port, plus net arcs (driver
to sink) and cell arcs (`combinational*`, `rising_edge`, `falling_edge`, `three_state_enable*`,
`three_state_disable*`, and preset/clear arcs). If combinational cycles exist, strongly connected
components (`SCC`s) are identified by Tarjan's algorithm and broken deterministically in canonical
node/arc order (`STA-REQ-0102`), emitting `note[T0002]`.

### Per-pin worst transition times

Every node maintains four transition (slew) slots—`early rise`, `early fall`, `late rise`, and
`late fall`—computed as the worst transition over **all enabled fan-in arcs**, including untagged
combinational sources (`STA-REQ-0108`). When an unclocked input has no explicit transition
constraint, its slew defaults to `Table::mid_slew` of the receiving arc's delay table (`0.0` when
no points exist).

### 1D, 2D, and 3D NLDM lookup tables

Cell delays (`cell_rise`, `cell_fall`, `three_state_enable*`, `three_state_disable*`) and output
transitions (`rise_transition`, `fall_transition`, `retaining_rise`, `retaining_fall`) are
evaluated by linear interpolation inside the table's axis range and linear extrapolation outside it
(`STA-REQ-0109`–`0111`). Extrapolated output slews below `0.0` are clamped to `0.0`, whereas
extrapolated cell delays are not clamped.

### Wire models (`Ideal` and `WireloadLumped`)

Net load capacitance and wire delay follow the active wire model (`STA-REQ-0120`–`0125`):

- **`WireloadLumped`:** selected when the library or SDC specifies a `wire_load` model (via
  `set_wire_load_model`, `default_wire_load`, or `wire_load_selection` by total cell area) and the
  mode is `top` or `enclosed` (or unspecified). A net's fanout `F` (number of sink cell input pins)
  interpolates wire length `L(F)` from `fanout_length` (extrapolating above the last entry with
  `slope`), adding `C_wire = L(F) * cap_per_length` to the net capacitance and
  `D_wire = R_wire * C_total` to each net arc when `R_wire > 0`.
- **Top-level port net exclusion (`STA-REQ-0123`):** nets directly connected to any top-level module
  port (`input`, `output`, or `inout`) receive zero wire-load capacitance and zero wire resistance;
  their external load comes from `set_load` on the port.
- **Driver output-pin capacitance (`STA-REQ-0122`):** the driving cell output pin's Liberty
  capacitance (`rise_capacitance` / `fall_capacitance` or `capacitance`) is added to the total load
  seen by the driver.
- **`Ideal`:** zero wire capacitance and zero wire delay, used when no `wire_load` group resolves:
  no `set_wire_load_model` or `set_wire_load_selection_group` in the SDC, and no
  `default_wire_load_selection` or `default_wire_load` in the corner library. `set_wire_load_mode`
  only chooses among `top`, `enclosed` and `segmented`; any other value (for example `none`) is
  rejected with `D0005` rather than selecting ideal wires.
- **Library defaults apply automatically:** a library that declares `default_wire_load` (for
  example `sky130_fd_sc_hd`) gets `WireloadLumped` wires even when the SDC names no wire-load
  model. A tool that uses wire loads only when the SDC selects one reports different net loads for
  such a library. To compare under ideal wires, analyze with a copy of the library that has no
  `wire_load`, `default_wire_load` or `default_wire_load_selection` entries. The run scope records
  the model that was used (`wire_model`, `wire_load`).

## 4. Constants, case analysis, and disabled arcs

Full specification: [`docs/specs/sta/constants.md`](../../specs/sta/constants.md).

Before delay propagation, the engine runs Kleene three-valued (`0`, `1`, `X`) constant propagation
to a fixpoint (`STA-REQ-0150`–`0157`):

1. **Seeds:** `NETLIST v1` constant `0`/`1` nets (`supply0`/`supply1`, `1'b0`/`1'b1`),
   `set_case_analysis 0|1|zero|one`, `set_logic_zero`, `set_logic_one`, and constant Liberty cell
   functions (`0`, `1`). (`set_case_analysis rising|falling` is rejected with `D0101`.)
2. **Arc disabling:** any pin holding constant `0` or `1` disables timing arcs starting or ending
   at that pin; any combinational arc whose cofactor under the constant side-inputs is constant is
   disabled (`DisabledReason::Constant`); `when` conditions that evaluate to `0` under the active
   constants are disabled (`DisabledReason::WhenCondition`); `set_disable_timing` disables matching
   cell or pin arcs (`DisabledReason::UserExplicit`).
3. **Unate sense refinement:** when a `non_unate` arc's cell function simplifies under the constant
   side-inputs to purely positive-unate or negative-unate in the arc's input pin, its effective
   timing sense is refined accordingly (`STA-REQ-0154`).

## 5. Clocks, sequential elements, and timing checks

Full specification: [`docs/specs/sta/checks.md`](../../specs/sta/checks.md).

### Clocks and multi-period relations

`create_clock` defines ideal clocks with period, waveform edges `[rise, fall]`, source latency
(`set_clock_latency -source`), network latency (`set_clock_latency`), transition
(`set_clock_transition`), and uncertainty (`set_clock_uncertainty`, with `-from`/`-to` overriding
per-clock values). When launch and capture clocks have commensurate periods `P_L` and `P_C`, setup
and hold relations are evaluated over the least common multiple window `L = lcm(P_L, P_C)` (up to
1,024 edges; `STA-REQ-0211`, `0212`):

- **Default setupcapture edge:** for each launch edge at time `t_L`, the closest strictly later
  capture edge `t_C > t_L`.
- **Default hold capture edge:** for each default setup pair `(t_L, t_C)`, the hold check compares
  against the latest capture edge strictly before `t_C` (including concurrent edges at `t_L == t_H`).

### Flip-flops, latches, recovery/removal, and clock gating

- **Flip-flops:** `rising_edge` / `falling_edge` arcs launch data at the active clock edge plus
  launch latency; `setup_rising` / `setup_falling` and `hold_rising` / `hold_falling` checks test
  late and early arrivals at `D`, `SE`, and `EN` pins.
- **Level-sensitive latches (`STA-REQ-0220`–`0222`):** active-high (`clear` / `preset` / `iq`) and
  active-low (`RisingEdge` vs `FallingEdge` closing edge) latches allow late arrivals after the
  opening edge `O` to borrow time up to `min(C - O - U_setup - T_setup, max_time_borrow)` and
  propagate downstream in the **opening-edge launch tag** `max(A_in, O + L_open) + D_D_to_Q`.
- **Recovery and removal (`STA-REQ-0224`):** asynchronous `clear` and `preset` checks are evaluated
  in the `async_default` path group and included in overall setup/hold WNS and TNS.
- **Clock gating (`STA-REQ-0216`, `0225`):** integrated clock-gating cells
  (`clock_gating_integrated_cell` / `non_seq_setup_*` / `non_seq_hold_*`) and combinational AND/OR
  gating cells check enable signals against the active and inactive clock edges.

## 6. Timing exceptions and clock groups

Full specification: [`docs/specs/sta/exceptions.md`](../../specs/sta/exceptions.md).

Phase 2a supports `-from` (`-rise_from`, `-fall_from`), `-to` (`-rise_to`, `-fall_to`), and
`-rise`/`-fall` qualifiers on:

| Command | Effect |
|---|---|
| `set_clock_groups -asynchronous \| -physically_exclusive \| -logically_exclusive` | Cuts timing checks between clocks in distinct groups (`-allow_paths` is rejected with `D0114`) |
| `set_false_path` | Disables matching setup, hold, recovery, removal, and clock-gating checks |
| `set_max_delay <D>` / `set_min_delay <D>` | Replaces the setup or hold relation with path delay bound `D` (`-ignore_clock_latency` omits launch/capture clock latencies) |
| `set_multicycle_path <N>` | Shifts the setup capture edge by `(N - 1)` periods (`-setup`, default) or the hold capture edge by `N` periods (`-hold`, `-start` or `-end`) |

When multiple exceptions match a `(startpoint, launch_clock, launch_trans, endpoint, capture_clock,
capture_trans, check_kind)` tuple, precedence is:

1. `set_clock_groups` (highest)
2. `set_false_path`
3. `set_max_delay` / `set_min_delay`
4. `set_multicycle_path` (lowest)

Within the same exception kind, specificity is ranked by a 5-bit score (`from_pin = 8` > `from_clock = 4`,
`to_pin = 2` > `to_clock = 1`, transition qualifier `= 1`), with ties broken by last-specified
command (`note[D0111]`).

## 7. Binary store (`sta.rbin`), incremental updates, and the independent checker

Full specifications: [`docs/specs/sta/store.md`](../../specs/sta/store.md),
[`docs/specs/sta/verification.md`](../../specs/sta/verification.md), and
[ADR 0007](../../adr/0007-sta-store.md).

### `STA v2` (`sta.rbin`) and storage modes

`sta.rbin` is a 24-section, 8-byte-aligned `RBIN v1` file ([`docs/specs/sta/store.md`](../../specs/sta/store.md)):

- **`--sta-store full` (default):** writes all 24 sections, including `NODE_REC` (152 bytes per
  `(corner, node)` with slews, required times, slacks, primary tag arrivals, and predecessors) and
  `TAG_EXTRA`. Supports `ruststa inspect` on any pin or instance, arbitrary ranked path
  reconstruction, and incremental seeding (`ruststa update`).
- **`--sta-store summary`:** omits `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA` while retaining
  summaries, endpoints, ranked path chains, top-`K` materialized paths, and name hashes.

### Incremental timing (`TimingSession` and `ruststa update`)

`ruststa update -w <WS> --base-rev <REV>` compares the revision's netlist, libraries, and
constraints against the nearest ancestor with a full `sta.rbin`:

- If `sta.rbin` on `--base-rev` is already current, `update` reports `mode: "up_to_date"` (`new_rev:
  null`, exit `0`).
- For netlist cell swaps, buffer insertions, net splits, or localized constraint changes,
  `TimingSession` invalidates only the transitive forward fanout cone for arrivals/slews and the
  reverse fanin cone for required times, re-propagating level-by-level (`--verify-full` checks
  bitwise agreement against a fresh full run).

### Independent workspace checker (`ruststa check`)

`ruststa check -w <WS> --base-rev <REV> [--method independent|full|both]` commits `sta_check.json`
(`schema: "ruststa.sta_check.v1"`) and `status.sta_check`:

- **`independent`:** `ruststa_check::rev` reads the verbatim cached `.lib` files, re-parses the
  revision netlist via its own Verilog reader, parses `constraints/merged.sdc` (`CANON`) via its
  own canonical-SDC reader, recomputes constants, arrivals, slews, required times, exceptions, and
  slacks in `f64`, and compares every pin and endpoint against `sta.rbin` within `--tolerance`
  (default `1e-6 ns` for `f64` stores, `max(1e-4 ns, 2e-5 * |v|)` for `f32` stores).
- **`full`:** runs a fresh full engine pass and compares `sta.rbin` bitwise.
- **`both`:** runs both `independent` and `full` checks.
- **Exit statuses:** `0` (`agree`), `3` (`disagree`, with `mm<N>` mismatch handles), `5`
  (`undecided`, when both a broken combinational loop SCC and a `-through` exception are present).
