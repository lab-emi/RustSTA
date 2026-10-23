# The `ruststa` command

`ruststa` (`crates/apps/ruststa-cli`, [ADR 0008](../../adr/0008-cli-on-workspace.md)) is the
command-line program of RustSTA on a RustEDA workspace. Timing mutations (`run`, `update`, `check`)
and mapped queries (`path`, `inspect`, `report`) are specified in
[`docs/specs/sta/cli.md`](../sta/cli.md) (STA-REQ-0300 to 0315) and
[`docs/specs/sta/verification.md`](../sta/verification.md) (STA-REQ-0500 to 0512). This document
specifies global options, streams, `rusteda.v1` envelopes, diagnostic and exit codes, report
sections, and shell completions.

| Document | Requirements |
|---|---|
| this file | CLI-REQ: global options, streams and envelopes, exit statuses, `report`, `completions` |
| [protocol.md](protocol.md) | PROTO-REQ-0001 to 0099: workspace transactions, canonical `next_actions`, execution, revision records |
| [tables.md](tables.md) | PROTO-REQ-0100 to 0199: numbers, fields, text, tabular sections, filters, sorting |

```text
ruststa [GLOBAL] run         -w <WS> --base-rev <REV> [OPTIONS]
ruststa [GLOBAL] update      -w <WS> --base-rev <REV> [OPTIONS]
ruststa [GLOBAL] check       -w <WS> --base-rev <REV> [OPTIONS]
ruststa [GLOBAL] path        -w <WS> [--rev <REV|HEAD>] [OPTIONS]
ruststa [GLOBAL] inspect     -w <WS> [--rev <REV|HEAD>] [OPTIONS]
ruststa [GLOBAL] report      -w <WS> [--rev <REV|HEAD>] [OPTIONS]
ruststa [GLOBAL] completions [<SHELL>]
```

## Global options

**CLI-REQ-0001** Global options are accepted before or after the subcommand: `-w, --workspace <DIR>`
(required for every subcommand except `completions`), `--format <text|json>` (default `text`),
`--threads <N>` (worker threads, `0` = one per available CPU), `-q, --quiet` (suppresses note
diagnostics on standard error; conflicts with `-v`), `-v, --verbose` (repeatable), `--color
<auto|always|never>` (default `auto`: colour when standard error is a terminal, `NO_COLOR` is unset
or empty and `TERM` is not `dumb`), and `--progress <auto|always|never|live|log|off>` (default
`auto`). `-V, --version` prints `ruststa <version> (backends: ...)`.

**CLI-REQ-0016** The global option `--threads <N>` sets the number of worker threads of every
parallel CPU phase of any command (`0`, the default: one per available CPU). `sta.rbin` and standard
output are byte-for-byte identical across `--threads 1` and `--threads 32`. When the platform cannot
start the threads, the command stops with `error[A0001]` and exit status `2`.

## Streams, envelopes, and messages

**CLI-REQ-0002** Standard input is never read by any `ruststa` command. Standard output carries
command output only: in `--format json` mode, a single `rusteda.v1` JSON envelope; in `--format
text` mode, human-readable summaries, tables, or completion scripts. Standard error carries only
coded diagnostics and their `  help:` lines.

**CLI-REQ-0003** `--format <text|json>` selects standard output formatting across all subcommands,
`--help`, `--version`, and usage errors (via raw `argv` pre-scanning before Clap parsing).

**CLI-REQ-0004** Diagnostics on standard error have the form `<severity>[<code>]: <location>: <text>`
(JOB-REQ-0007), optionally followed by `  help: <text>`. In `--format json` mode, the same
diagnostics are emitted in the envelope's `diagnostics` array.

**CLI-REQ-0005** Invalid command lines are reported as `USAGE` (`error[A0003]`, exit status `1`)
with a `help:` hint naming `<command> --help`. In `--format json` mode, usage errors, `--help`, and
`--version` still emit a valid `rusteda.v1` JSON envelope on standard output (`exit 1` for usage,
`exit 0` for help and version).

**CLI-REQ-0006** Application diagnostic codes: `A0001` worker threads cannot start (exit `2`),
`A0002` internal failure / panic caught at the top level (exit `2`), `A0003` invalid command line
(`USAGE`, exit `1`), `A0004` I/O error (`IO_ERROR`, exit `1`), `A0030` shell not detected or
unsupported in `completions` (`USAGE`, exit `1`), `A0130` cancelled (`CANCELLED`, exit `130`), and
`A0501` uncompiled or unavailable propagation backend (`BACKEND_UNAVAILABLE`, exit `1`).

**CLI-REQ-0007** Colour: only the severity word on standard error is coloured (`error` bold red,
`warning` bold yellow, `note` bold cyan); standard output is never coloured.

**CLI-REQ-0008** Progress and quiet mode: `--quiet` suppresses note diagnostics on standard error;
engine runs never write directly to standard output or standard error.

**CLI-REQ-0009** Exit statuses follow the workspace contract: `0` success (`run`, `update`, `path`,
`inspect`, `report`, `completions`, `--help`, `--version`, and `check` with verdict `agree`), `1`
rejected (`USAGE`, `INPUT_REJECTED`, `NOT_A_WORKSPACE`, `WORKSPACE_EXISTS`, `UNKNOWN_REVISION`,
`MISSING_TABLE`, `UNKNOWN_HANDLE`, `SCHEMA_TOO_NEW`, `IO_ERROR`, `BACKEND_UNAVAILABLE`), `2`
internal or corruption failure (`INTERNAL`, `CORRUPT_TABLE`), `3` negative verdict (`verdict ==
"disagree"` with `ok: true`), `4` concurrent or stale base revision (`REVISION_CONFLICT`), `5`
inconclusive verification (`verdict == "undecided"` with `ok: true`), and `130` cancelled.

**CLI-REQ-0010** Workspace tables (`sta.rbin`, `sta_check.json`), `meta.json`, and `history.jsonl`
are staged and committed atomically through `rusteda_workspace::Txn` before the command prints its
result on standard output.

**CLI-REQ-0011** A broken pipe on standard output ends output silently; the exit status is that of
the computation.

**CLI-REQ-0012** A panic anywhere in a command is caught at the top level and reported as
`INTERNAL` (`error[A0002]`, exit status `2`).

## `ruststa report`

**CLI-REQ-0100** `ruststa report -w <WS> [--rev <REV|HEAD>]` renders one or more sections from
`sta.rbin`: `--section <summary|paths|endpoints|pins|clocks|checks|unconstrained|notes>`
(repeatable or comma-separated; defaults to `summary,paths`), `--kind <setup|hold|both>`,
`--corner <NAME>`, `--sort <KEY>`, `--reverse`, `--filter <EXPR>` (PROTO-REQ-0131), `--limit <N>`
(`0` = all rows), and `--exact`.

**CLI-REQ-0101** Every requested section in `--format json` mode is emitted under
`sections.<name>` with keys `{columns, total, matched, shown, rows}` in deterministic order.

**CLI-REQ-0102** Requesting `--section pins` on a revision whose `sta.rbin` was written with
`--sta-store summary` fails with `INPUT_REJECTED` (exit `1`) and a message explaining that per-pin
records require `--sta-store full`.

**CLI-REQ-0103** `ruststa path` and `ruststa inspect` query `sta.rbin` by 1-based handles
(`path<N>`, `inst<N>`, `net<N>`, `pin<N>`, `port<N>`, `sel<N>`) or by object name (`--to`, `--inst`,
`--net`, `--pin`).

## `ruststa completions`

**CLI-REQ-0150** `ruststa completions [<SHELL>]` prints the completion script for `bash`, `elvish`,
`fish`, `powershell`, or `zsh` (or wraps `{shell, script}` in a `rusteda.v1` envelope under
`--format json`); without `<SHELL>`, the shell is inferred from `$SHELL`, and `A0030` (`USAGE`,
exit `1`) is reported when `$SHELL` is missing or unsupported.
