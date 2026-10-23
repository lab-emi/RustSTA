# The workspace execution and envelope protocol of `ruststa`

The workspace execution and envelope protocol (`crates/apps/ruststa-cli`,
[ADR 0008](../../adr/0008-cli-on-workspace.md)) connects the command-line interface
([`docs/specs/cli/README.md`](README.md)) to the timing engine (`ruststa::engine`), the binary
timing store (`sta.rbin`, [ADR 0007](../../adr/0007-sta-store.md)), the independent checker
(`ruststa_check::rev`), and the shared workspace layer (`rusteda-workspace`). Numbers, text
formatting, and tabular sections are specified in [tables.md](tables.md) (PROTO-REQ-0100 to 0199).

## Commands and `next_actions`

**PROTO-REQ-0001** Every `ruststa` subcommand is classified as either a workspace mutation (`run`,
`update`, `check`), which requires `--base-rev <revN>` (`HEAD` is rejected) and commits a new
revision (except `update` when `incremental.mode == "up_to_date"`), or a read-only workspace query
(`path`, `inspect`, `report`, `completions`), which accepts `--rev <REV|HEAD>` (default `HEAD`).

**PROTO-REQ-0002** Read-only query commands (`path`, `inspect`, `report`) read timing data from the
revision's mapped `sta.rbin` store (`StaStore::open_rev`), while legacy `ruststa::view` provides the
in-memory view model for `ruststa::Analysis`.

**PROTO-REQ-0010** Every subcommand and option provides deterministic help text accessible via
`--help` in both `text` and `json` (`help` field in the `rusteda.v1` envelope) formats.

**PROTO-REQ-0020** Every `next_actions` entry in a `rusteda.v1` envelope (and every `next:` line in
text mode) is rendered using canonical POSIX shell word quoting (`rusteda_workspace::text::words` /
`quote_arg`), quoting any token with a leading `=` so `words(next_action)` round-trips every
argument word exactly.

**PROTO-REQ-0021** Every `ruststa` command emitted in `next_actions` of a successful or rejected
envelope uses concrete revision names (`rev<N>`, never `HEAD` on mutations) and executes cleanly
with exit status `0` when run against the workspace.

## Execution and transactions

**PROTO-REQ-0030** Workspace mutations (`run`, `update`, `check`) open the workspace, acquire the
exclusive transaction lock (`Workspace::begin`), stage `sta.rbin` and/or `sta_check.json` with their
upstream table hashes, populate `summary` and `status`, and commit atomically (`Txn::commit`).

**PROTO-REQ-0031** Exit classes and statuses follow `rusteda_workspace::exit`: `OK = 0`,
`REJECTED = 1`, `INTERNAL = 2`, `NEGATIVE_VERDICT = 3` (`verdict == "disagree"`),
`REVISION_CONFLICT = 4`, `INCONCLUSIVE = 5` (`verdict == "undecided"`), and `CANCELLED = 130`.

**PROTO-REQ-0032** Of two concurrent mutations targeting the same `--base-rev`, the workspace lock
serializes or rejects the second so that at most one commits the next revision and the other exits
with status `4` (`REVISION_CONFLICT`) and recovery `next_actions` (`rusteda log`, `rusteda diff`,
`rusteda show`).

**PROTO-REQ-0033** All revision files (`sta.rbin`, `sta_check.json`, `meta.json`) and
`history.jsonl` entries are written atomically via `rusteda-workspace`; JSON envelopes on standard
output are pretty-printed with keys in the exact order of the workspace contract.

**PROTO-REQ-0034** On `ruststa path --to <NAME>` and `ruststa inspect --pin/--inst/--net <NAME>`,
names are resolved against `sta.rbin`'s `NAME_HASH` and `INST_HASH` tables: an unknown name returns
`UNKNOWN_HANDLE` (exit `1`), and a non-endpoint pin passed to `path --to` returns `INPUT_REJECTED`
(exit `1`).

## Revision records and history

**PROTO-REQ-0050** A committed timing revision directory `revisions/rev<N>/` holds `meta.json`,
carried upstream tables (`netlist.rbin`, `constraints.rbin`), `sta.rbin` (from `run` or `update`),
and optionally `sta_check.json` (from `check`, `run --check`, or `update --check`).

**PROTO-REQ-0051** Revision identifiers are monotonic `rev<N>` (`rev0`, `rev1`, `rev2`, …) managed
by `rusteda-workspace`.

**PROTO-REQ-0052** `meta.json` records `rev`, `parent`, `command`, `message`, `tables` (with `path`,
`hash`, and `upstream`), `libraries`, `sources`, `summary.sta` (`wns_ns`, `tns_ns`,
`violating_endpoints`, `whs_ns`, `ths_ns`, `hold_violating_endpoints`, `worst_setup_slack_ns`,
`worst_hold_slack_ns`, `corners`), and `status` (`sta.timing = "met" | "violated"`,
`sta_check.verdict = "agree" | "disagree" | "undecided"`).

**PROTO-REQ-0053** Every mutating command (`run`, `update`, `check`) appends one JSON Line record to
`history.jsonl` (including rejected and conflicted invocations), whereas read-only queries (`path`,
`inspect`, `report`, `completions`) never append to `history.jsonl`.

**PROTO-REQ-0060** Revision references: `--base-rev` on mutations requires an explicit `rev<N>`
(rejecting `HEAD` with `UNKNOWN_REVISION`, exit `1`); `--rev` on queries accepts `rev<N>` or `HEAD`
(default `HEAD`) and resolves `HEAD` to the concrete `rev<N>` in the output envelope.

**PROTO-REQ-0061** `ruststa update` walks the parent chain from `--base-rev` to locate the nearest
ancestor revision with a `sta` table, computing `metric_deltas` (`wns_ns`, `tns_ns`,
`violating_endpoints`, `whs_ns`, `ths_ns`, `hold_violating_endpoints`) between that ancestor and the
updated revision.

**PROTO-REQ-0062** Opening a revision's `sta.rbin` validates the `STA v1` magic, format version
(`SCHEMA_TOO_NEW` exit `1` for newer versions), section directory (`CORRUPT_TABLE` exit `2` on
truncation or corruption), and, for mutation inputs (`update`, `check`), the full `xxh3-64` payload
checksum. In the job protocol, `--backend cuda-native` is accepted as an alias for `cuda`, and
`--backend wgpu` is rejected with `BACKEND_UNAVAILABLE` (`A0501`, exit `1`).
