# RustSTA command line and reports

Design rationale for the workspace-native `ruststa` command line (`crates/apps/ruststa-cli`) is
recorded in [ADR 0008](../../adr/0008-cli-on-workspace.md). Global flags, stream rules, diagnostic
codes, and exit codes are specified in [`docs/specs/cli/README.md`](../cli/README.md).

**STA-REQ-0300** Commands on a RustEDA workspace (`-w, --workspace <DIR>`):

```text
ruststa [GLOBAL] run         -w <WS> --base-rev <REV> [--corner <NAME>]... [--backend cpu|cuda|gpu]
                             [--precision f64|f32] [--max-paths <N>] [--max-state-gib <GIB>]
                             [--max-limit-rows <N>] [--require-wire-model <MODEL>]
                             [--pulse-groups worst|last] [--pulse-pins all|clock-ends]
                             [--clock-limits paths|domain]
                             [--sta-store full|summary] [--check [--tolerance <NS>]] [-m <TEXT>]
ruststa [GLOBAL] update      -w <WS> --base-rev <REV> [--corner <NAME>]... [--backend cpu|cuda|gpu]
                             [--precision f64|f32] [--max-paths <N>] [--max-state-gib <GIB>]
                             [--max-limit-rows <N>] [--require-wire-model <MODEL>]
                             [--pulse-groups worst|last] [--pulse-pins all|clock-ends]
                             [--clock-limits paths|domain]
                             [--sta-store full|summary] [--check [--tolerance <NS>]] [--verify-full]
                             [-m <TEXT>]
ruststa [GLOBAL] check       -w <WS> --base-rev <REV> [--method independent|full|both]
                             [--tolerance <NS>] [-m <TEXT>]
ruststa [GLOBAL] path        -w <WS> [--rev <REV|HEAD>] (--target <HANDLE>... | --to <NAME>...)
                             [--kind setup|hold|both] [--corner <NAME>] [--exact]
ruststa [GLOBAL] inspect     -w <WS> [--rev <REV|HEAD>] (--target <HANDLE>... | --inst <NAME>
                             | --net <NAME> | --pin <NAME>) [--corner <NAME>]
ruststa [GLOBAL] report      -w <WS> [--rev <REV|HEAD>] [--section <SECTION>...]
                             [--kind setup|hold|both] [--corner <NAME>] [--sort <KEY>]
                             [--reverse] [--filter <EXPR>] [--limit <N>] [--target-margin <NS>]
                             [--exact]
ruststa [GLOBAL] completions [<SHELL>]
```

`--max-limit-rows`, `--require-wire-model`, `--pulse-groups`, `--pulse-pins`, `--clock-limits`
(STA-REQ-0808), the report sections
`limits`, `pulse_width`,
`min_period`, `drv`, `max_transition`, `max_capacitance`, `max_fanout` and `hold_fix`, and
`--target-margin` are specified in [limits.md](limits.md) (STA-REQ-0806 to STA-REQ-0832).
`--max-paths` sets the number of worst setup/hold paths included in `top_paths` (default `5`);
`--max-state-gib` limits the estimated propagation state of one corner (default `16`, STA-REQ-0024);
`--backend` selects the propagation backend (`cpu`, `cuda`, `gpu`, STA-REQ-0400; default `cpu`).
An uncompiled or unavailable GPU backend fails with `BACKEND_UNAVAILABLE` (exit `1`) and diagnostic
`A0501`. The global option `--threads <N>` sets the number of worker threads of parallel CPU phases
(`0`, the default: one per available CPU); a value the platform rejects is reported with code
`A0001` and exit status `2` (CLI-REQ-0016).

**STA-REQ-0301** Standard output in `--format text` mode begins with `rev<N>  <command>  (base rev<M>)`
for mutating commands and ends with `next: <command>` lines; in `--format json` mode, every
invocation prints a single `rusteda.v1` JSON envelope on standard output (`schema`, `ok`, `command`,
`workspace`, `base_rev`, `rev`, `new_rev`, `scope`, `completeness`, `metrics`, command-specific
fields, `diagnostics`, `error`, `next_actions`). Diagnostics use the format of CLI-REQ-0004 with
codes `T<nnnn>`, `C<nnnn>`, `W<nnnn>`, and `A<nnnn>`.

**STA-REQ-0302** `ruststa run` analyzes every corner of `--base-rev` (or the `--corner` subset) and
commits `sta.rbin` (`STA v2`, [`docs/specs/sta/store.md`](store.md)) with upstream `netlist`,
`constraints`, and `libraries` (or `libraries.<corner>`), plus `meta.json` `summary.sta` and
`status.sta` (`timing = "met" | "violated"`). All stored and printed floating-point values
canonicalize `-0.0` to `+0.0`. The legacy `ruststa::analyze` JSON report also exposes `top`,
`backend`, `clocks`, `setup`, `hold`, `unconstrained_endpoints`, `endpoints`, `paths`, `counts`,
`notes`, `runtime_s`, and `phase_s`.

**STA-REQ-0303** `ruststa update` resolves the nearest ancestor revision with a valid `sta` table
and reports `incremental {from_rev, mode: "incremental" | "full" | "up_to_date", computation,
reason, changed_instances, changed_nets, dirty_nodes, total_nodes}` and `metric_deltas`. `mode`
classifies the change since the ancestor; `computation` states how the timing values were computed:
`"full"` (this version recomputes the whole graph in every mode but `up_to_date`) or `"none"`
(`up_to_date`). When `--base-rev` already has a valid `sta` table, `update` commits no new revision
(`new_rev: null`, exit `0`). `--verify-full` checks the updated results against a fresh full run
(`C0503` on discrepancy).

**STA-REQ-0304** Constraint and library errors during `run` or `update` are reported as structured
diagnostics (`T0001`–`T0006`, `D`-series, `L`-series) with `file:line:col` locations and exit status
`1` (`INPUT_REJECTED`).

**STA-REQ-0305** Unknown object or path handles (`path<N>`, `inst<N>`, `net<N>`, `pin<N>`, `port<N>`,
`sel<N>`) and unknown pin/port/instance names on `path` or `inspect` return `UNKNOWN_HANDLE` (exit
`1`, `T0008`/`W0007`), suggesting `<prefix><N>` when a malformed `<prefix>_<N>` handle is passed.

**STA-REQ-0306** `ruststa check` verifies a revision's `sta.rbin` with `--method independent`
(`ruststa_check::rev` reading `cache/*.lib`, round-tripped Verilog, and canonical SDC without
depending on the engine crate), `--method full` (fresh full engine recomputation), or `--method both`,
and commits `sta_check.json` (`schema = "ruststa.sta_check.v1"`) and `status.sta_check` (`verdict =
"agree" | "disagree" | "undecided"`). Exit status is `0` on `agree`, `3` on `disagree` (`mm<N>`
mismatches), and `5` on `undecided`. The legacy report starts with `format` (`ruststa-sta-report`)
and `format_version` (`1`).

**STA-REQ-0307** `ruststa check` compares whether each endpoint check is timed, not only the
slacks of the checks both sides time: a setup or hold check that the timing table times and the
independent checker does not, or the reverse, and an endpoint that the checker times and the table
does not list, are mismatches `C0502` (`--method independent`); a table whose endpoint count or
timed checks differ from a fresh full run gives mismatches `C0504` (`--method full`). Each such
mismatch names the endpoint and field with `checker` and `sta` set to `timed` or `untimed`, and
counts among `slacks_compared`.

**STA-REQ-0308** The diagnostics of a successful command (the warnings and notes of constraint
compilation, of the analysis and of the commit) are part of its `rusteda.v1` envelope with
`--format json` and are written to standard error with `--format text`, exactly as those of a
failed command.

**STA-REQ-0310** `run_with(options, libraries, ctx)` and `EngineAnalysis::analyze` execute with a
job context (JOB-REQ-0002), reporting phases and notes in deterministic order. `run` is `run_with`
with `Ctx::NONE`.

**STA-REQ-0311** Observing a timing run or passing a cancel token that is never set changes no
value, no endpoint, and no `sta.rbin` or report field other than run times (JOB-REQ-0005).

**STA-REQ-0312** `Analysis::report(options)` and `StaStore` queries (`query_summary`, `query_scope`)
give identical summary and scope values for the same analysis options.

**STA-REQ-0313** `Analysis::endpoint_table()` and `StaStore::query_endpoints` give, per endpoint in
deterministic order, the endpoint node, worst setup and hold slack (`NaN` when absent), and capture
clock.

**STA-REQ-0314** `Analysis::worst_path(node, kind)` and `StaStore::query_paths_to` give the worst
path of any endpoint and check kind on request, matching `StaStore::query_path_handle` for the same
endpoint and check kind.

**STA-REQ-0315** `Analysis::name_index()` and `StaStore`'s `NAME_HASH` / `INST_HASH` tables resolve
ports, `"inst/pin"` pins, and instances in `O(1)` time from mapped `sta.rbin` sections.
