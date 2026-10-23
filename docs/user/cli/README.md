# The `ruststa` and `rusteda` commands

`rusteda` manages the disk workspace and design revisions
([`docs/specs/rusteda-cli/README.md`](../../specs/rusteda-cli/README.md)), while `ruststa` runs
static timing analysis, incremental updates, independent checking, and zero-copy queries on the
workspace ([`docs/specs/sta/cli.md`](../../specs/sta/cli.md)). See the
[RustSTA user manual](../sta/README.md) for the timing model.

```text
rusteda [GLOBAL OPTIONS] <COMMAND>

  init          initialize a new workspace at rev0
  import        import Liberty libraries, RTL, netlists, or SDC constraints as a new revision
  log           list committed revisions newest first
  show          show metadata, tables, libraries, sources, summary, and status of a revision
  diff          compare tables, libraries, and summary metrics between two revisions
  revert        move HEAD from --base-rev to an existing revision --to
  export        export a committed table (--netlist FILE)
  completions   print a shell completion script

ruststa [GLOBAL OPTIONS] <COMMAND>

  run           analyze timing on --base-rev and commit sta.rbin as a new revision
  update        incrementally update (or re-run) timing from --base-rev
  check         verify a revision's timing store (--method independent|full|both)
  path          reconstruct worst timing paths by handle (path<N>, pin<N>, port<N>) or --to <NAME>
  inspect       inspect timing on an instance, net, pin, port, path, or selection handle
  report        query tabular sections of sta.rbin with filters and sort orders
  completions   print a shell completion script
```

## 1. Global options

Global options are accepted before or after the subcommand token on both `rusteda` and `ruststa`:

| Option | Meaning |
|---|---|
| `-w`, `--workspace <DIR>` | Workspace directory (required for every subcommand except `completions`) |
| `--format text\|json` | Output format on standard output: human-readable text or a single `rusteda.v1` JSON envelope (pre-scanned before argument parsing so usage errors, `--help`, and `--version` also emit JSON envelopes under `--format json`) |
| `--threads <N>` | Worker threads for parallel CPU phases (`0`, default: one per available CPU; outputs are deterministic and independent of `<N>`) |
| `-q`, `--quiet` | Suppress `note` diagnostics and progress on standard error |
| `-v`, `--verbose` | Emit per-phase progress and verbose notes on standard error |
| `--color auto\|always\|never` | ANSI color policy on standard error (`NO_COLOR` is honored) |
| `--progress auto\|always\|never` | Progress reporting on standard error |
| `-V`, `--version` | Print tool version (for `ruststa`, includes compiled backends: `ruststa 0.1.0 (backends: cpu)` or `ruststa 0.1.0 (backends: cpu, cuda)`) |

Neither `rusteda` nor `ruststa` reads from standard input.

## 2. Workspace setup and revision management (`rusteda`)

```console
$ rusteda init -w ./ws --format json
$ rusteda import -w ./ws --base-rev rev0 --lib wt_full.lib --netlist counter.v --sdc counter.sdc -m "initial import" --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "rusteda import",
  "workspace": "./ws",
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

Key `rusteda import` flags:

| Option | Meaning |
|---|---|
| `--base-rev <REV>` | Expected `HEAD` revision (`rev<N>`, never `HEAD`); required |
| `-m`, `--message <TEXT>` | Optional commit message recorded in `meta.json` |
| `--lib <FILE>` | Liberty library to cache and compile to `.rlib` (repeatable) |
| `--corner <NAME>` | Operating corner for the immediately preceding `--lib` (default `"default"`) |
| `--netlist <FILE>` | Gate-level structural Verilog netlist (`netlist.rbin`) |
| `--sdc <FILE>...` | SDC constraint files (`constraints.rbin` and `constraints/merged.sdc`) |
| `--sdc-env <NAME=VALUE>` | Explicit environment variable binding for `$::env(NAME)` inside `--sdc` (repeatable) |

## 3. Timing mutations (`ruststa run`, `update`, `check`)

All workspace-changing commands require `--base-rev <REV>` (never `HEAD`), acquire an advisory lock
on `workspace.lock`, write a compact audit entry to `history.jsonl`, and exit `4`
(`REVISION_CONFLICT`) if `HEAD` does not match `--base-rev`.

### `ruststa run`

```text
ruststa run -w <WS> --base-rev <REV> [--corner <NAME>]... [--backend cpu|cuda|gpu]
            [--precision f64|f32] [--max-paths <N>] [--max-state-gib <GIB>]
            [--sta-store full|summary] [--check [--tolerance <NS>]] [-m <TEXT>]
```

| Option | Meaning |
|---|---|
| `--base-rev <REV>` | Base revision containing `netlist`, `constraints`, and `libraries` |
| `--corner <NAME>` | Analyze only the named corner(s) (repeatable; default: all corners of `--base-rev`) |
| `--backend cpu\|cuda\|gpu` | Propagation backend (default `cpu`; `cuda` and `gpu` require `--features cuda` and a passing device self-test, otherwise `BACKEND_UNAVAILABLE` / `A0501`) |
| `--precision f64\|f32` | Floating-point precision of propagation (default `f64`) |
| `--max-paths <N>` | Number of worst paths included in `top_paths` (default `5`; presentation only) |
| `--max-state-gib <GIB>` | Limit of the estimated propagation state of one corner (nodes × tags × 160 bytes; default `16`); above it the run stops with `T0023` before allocating |
| `--sta-store full\|summary` | `sta.rbin` storage mode (`full`, default, retains per-pin `NODE_REC` and `TAG_EXTRA`; `summary` omits them) |
| `--check` | Also run the independent checker and commit both `sta` and `sta_check` in the new revision |
| `--tolerance <NS>` | Slack tolerance in nanoseconds when `--check` is given |
| `-m <TEXT>` | Commit message |

### `ruststa update`

```text
ruststa update -w <WS> --base-rev <REV> [--backend cpu|cuda|gpu] [--precision f64|f32]
               [--max-paths <N>] [--max-state-gib <GIB>] [--check] [--verify-full] [-m <TEXT>]
```

Updates timing relative to the nearest ancestor with a `full` `sta.rbin`: `incremental.mode`
classifies the change (`"incremental"` for a small netlist change, otherwise `"full"` with a
documented `reason`), and `incremental.computation` states how the values were computed; this
version recomputes the whole graph (`"full"`). If `--base-rev` already has a current `sta` table,
`update` commits nothing (`new_rev: null`, `incremental.mode: "up_to_date"`,
`incremental.computation: "none"`, exit `0`). `--verify-full` additionally runs a fresh full
engine pass and verifies bitwise agreement.

### `ruststa check`

```text
ruststa check -w <WS> --base-rev <REV> [--method independent|full|both]
              [--tolerance <NS>] [-m <TEXT>]
```

Verifies `sta.rbin` on `--base-rev` and commits `sta_check.json` (`ruststa.sta_check.v1`) and
`status.sta_check`:

- `--method independent` (default): runs `ruststa_check::rev` in `f64` from cached `.lib` files,
  re-parsed Verilog netlist, and canonical SDC (`constraints/merged.sdc`).
- `--method full`: runs a fresh full engine pass and compares `sta.rbin` bitwise.
- `--method both`: runs both `independent` and `full`.
- `--tolerance <NS>`: maximum allowed absolute slack difference in nanoseconds (default `1e-6` for
  `f64` stores, `max(1e-4, 2e-5 * |v|)` for `f32` stores).

```console
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
$ ruststa update -w ./ws --base-rev rev2 --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa update",
  "workspace": "./ws",
  "base_rev": "rev2",
  "rev": null,
  "new_rev": null,
  "scope": {},
  "completeness": {},
  "metrics": {},
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
$ ruststa check -w ./ws --base-rev rev2 --method both --format json
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

## 4. Zero-copy queries (`ruststa path`, `inspect`, `report`)

Queries accept `--rev <REV|HEAD>` (default `HEAD`; the envelope always reports the resolved
concrete `rev<N>`), memory-map `sta.rbin`, and do not modify the workspace or `history.jsonl`.

### `ruststa path`

```text
ruststa path -w <WS> [--rev <R>] (--target path<N>|pin<N>|port<N>)... | (--to <NAME>)...
             [--kind setup|hold|both] [--corner <C>] [--exact]
```

Returns point-by-point timing paths (`pin`, `handle`, `inst`, `cell`, `arc`
`clock|launch|cell|net|input`, `transition`, `incr_ns`, `arrival_ns`, `slew_ns`), launch and
capture clocks, `arrival_ns`, `required_ns`, and `slack_ns`.

### `ruststa inspect`

```text
ruststa inspect -w <WS> [--rev <R>]
                (--target sel<N>|inst<N>|net<N>|pin<N>|port<N>|path<N>)...
                | --inst <NAME> | --net <NAME> | --pin <NAME> [--corner <C>]
```

Inspects an object by revision-scoped handle or hierarchical name, returning worst early/late
rise/fall arrivals, slews, required times, and slacks across launch tags (`--sta-store full`).

### `ruststa report`

```text
ruststa report -w <WS> [--rev <R>]
               [--section summary|paths|endpoints|pins|clocks|checks|unconstrained|notes]...
               [--kind setup|hold|both] [--corner <C>] [--sort <KEY>] [--reverse]
               [--filter <EXPR>] [--limit <N>] [--exact]
```

Queries tabular views from `sta.rbin`. Each requested section returns `{columns, total, matched,
shown, rows}`:

| Section | Contents |
|---|---|
| `summary` | Per-corner, per-path-group WNS, TNS, violating endpoints, and DRV counts |
| `paths` | Ranked timing paths (`handle`, `kind`, `slack_ns`, `startpoint`, `endpoint`, `logic_levels`) |
| `endpoints` | Checked timing endpoints with worst setup and hold slacks (`setup`, `hold`, `endpoint`, `clock`) |
| `pins` | Per-pin arrivals, slews, required times, and slacks (`--limit 0 --format json` exports full-precision pin timing) |
| `clocks` | Constraint clocks, periods, waveform edges, and uncertainties |
| `checks` | Individual setup, hold, recovery, removal, latch, and clock-gating check records |
| `unconstrained` | Unconstrained endpoints grouped by reason (`no_clock`, `no_arrival`, `no_required`, `disabled_only`, `constant_only`, `false_path_only`) |
| `notes` | Aggregated engine notes and warnings stored in `sta.rbin` |

```console
$ ruststa path -w ./ws --rev rev2 --target path1 --format json
$ ruststa inspect -w ./ws --rev rev2 --pin r2/D --format json
$ ruststa report -w ./ws --rev rev2 --section endpoints --filter 'setup<0' --sort setup --limit 5 --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa report",
  "workspace": "./ws",
  "base_rev": null,
  "rev": "rev2",
  "new_rev": null,
  "scope": {},
  "completeness": {},
  "metrics": {},
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
```

## 5. Revision and object handles

All handles match `[A-Za-z0-9_-]+` and are scoped to a specific workspace revision
([`docs/specs/workspace/README.md`](../../specs/workspace/README.md)):

| Handle | Definition |
|---|---|
| `rev<N>` | Revision number (`rev0`, `rev1`, ...; directory `revisions/rev<NNNN>`) |
| `inst<N>` | `1 +` row index in `NETLIST v1` `INSTANCES` |
| `net<N>` | `1 +` row index in `NETLIST v1` `NETS` |
| `port<N>` | `1 +` row index in `NETLIST v1` `PORTS` |
| `pin<N>` | `1 +` row index in `NETLIST v1` `PIN_NET` (connected instance pins) |
| `path<N>` | 1-based rank over all checked `(endpoint, kind)` paths in `sta.rbin` (setup checks by ascending slack, then hold, ties by topological ID) |
| `mm<N>` | 1-based rank in `sta_check.json` `mismatches` |
| `sel<N>` | GUI selection handle from `ui_state.json` (valid only for its recorded revision) |

Passing a malformed handle such as `inst_841` fails with `UNKNOWN_HANDLE` and suggests `inst841`.

## 6. JSON envelope (`rusteda.v1`), diagnostics, and exit statuses

### Envelope structure

Under `--format json`, every command prints exactly one `rusteda.v1` JSON object on standard output
with keys in fixed contract order:

1. `"schema"`: `"rusteda.v1"`
2. `"ok"`: `true` when the command completed (including negative checker verdicts), `false` on
   errors
3. `"command"`: canonical command name (e.g. `"ruststa run"`, `"ruststa check"`, `"rusteda import"`)
4. `"workspace"`: workspace path string or `null`
5. `"base_rev"`: base revision for mutations or `null` for queries
6. `"rev"`: resolved target revision for queries or `null` for mutations
7. `"new_rev"`: committed revision (`"rev2"`) or `null`
8. `"scope"`: operating corner, `wire_model`, `parasitics`, `drc_status`, `wire_load`,
   `delay_calculator`, `clock_model`, `cppr`, `backend`, `precision` (and `corners` when multiple)
9. `"completeness"`: `constrained_endpoints`, `unconstrained_endpoints`, `clock_domains`,
   `unconstrained_by_reason`, `unconstrained_inputs`, `disabled_arcs`, `unanalyzed_arcs`
10. `"metrics"`: `wns_ns`, `tns_ns`, `violating_endpoints`, `whs_ns`, `ths_ns`,
    `hold_violating_endpoints`, `worst_setup_slack_ns`, `worst_hold_slack_ns`
11. Tool-specific fields (such as `"top_paths"`, `"truncated_paths"`, `"incremental"`,
    `"metric_deltas"`, `"verdict"`, `"mismatches"`, `"paths"`, `"sections"`)
12. `"diagnostics"`: array of `{severity, code, location, message, help}` (`location` is
    `{file, line, col}` or `null`)
13. `"error"`: `{code, message, details}` or `null`
14. `"next_actions"`: array of POSIX-quoted follow-up commands (words starting with `=` are quoted
    for `zsh` compatibility)

### Diagnostic code families

| Prefix | Domain |
|---|---|
| `A` | Application, CLI, thread pool (`A0001`–`A0030`), and compute backend availability (`A0501`) |
| `W` | Workspace, revisions, locks, and `.rbin` container validation |
| `V` | Structural Verilog reader and netlist import |
| `L` | Liberty library reader (`read_file_diags`) |
| `D` | SDC constraint parser, Tcl evaluator, object queries, and timing exceptions (`D0001`–`D0114`) |
| `C` | Independent timing checker (`ruststa-check`, `C0500`–`C0999`) |
| `T` | Timing engine (`ruststa`) |

### Exit statuses and error codes

| Exit status | Meaning | Error / verdict codes |
|---:|---|---|
| `0` | Command succeeded (`ruststa check` verdict `agree`) | `error: null` |
| `1` | Usage error, rejected input, missing table/handle, or unavailable GPU backend | `USAGE`, `INPUT_REJECTED`, `NOT_A_WORKSPACE`, `WORKSPACE_EXISTS`, `UNKNOWN_REVISION`, `MISSING_TABLE`, `UNKNOWN_HANDLE`, `SCHEMA_TOO_NEW`, `IO_ERROR`, `BACKEND_UNAVAILABLE` (`A0501`) |
| `2` | Internal failure or corrupt workspace table | `INTERNAL`, `CORRUPT_TABLE` |
| `3` | Negative verification verdict | `ruststa check` verdict `disagree` (`ok: true`) |
| `4` | Concurrent or stale base revision | `REVISION_CONFLICT` |
| `5` | Inconclusive verification verdict | `ruststa check` verdict `undecided` (`ok: true`) |
| `130` | Interrupted / cancelled (`SIGINT`) | `CANCELLED` (`A0130`) |

## 7. Shell completions

```console
$ ruststa completions bash --format json
{
  "schema": "rusteda.v1",
  "ok": true,
  "command": "ruststa completions",
  "workspace": null,
  "base_rev": null,
  "rev": null,
  "new_rev": null,
  "scope": {},
  "completeness": {},
  "metrics": {},
  "diagnostics": [],
  "error": null,
  "next_actions": []
}
```
