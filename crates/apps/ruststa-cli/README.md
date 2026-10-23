# ruststa-cli

The `ruststa` command-line program ([`docs/specs/sta/cli.md`](../../../docs/specs/sta/cli.md),
[`docs/user/cli/README.md`](../../../docs/user/cli/README.md),
[ADR 0008](../../../docs/adr/0008-cli-on-workspace.md)): static timing analysis, incremental
updates, independent checking, and zero-copy queries on a RustEDA workspace.

## Subcommands

- **Mutations (`--base-rev <REV>`):**
  - `ruststa run`: runs multi-corner static timing analysis on `--base-rev`, commits `sta.rbin`
    (`STA v2`), `summary.sta`, and `status.sta` (plus `sta_check.json` when `--check` is passed) as
    a new workspace revision, and prints the text summary or `rusteda.v1` JSON envelope.
  - `ruststa update`: incrementally updates timing from `--base-rev` via `TimingSession` (or falls
    back to a full run with a documented reason), reporting `incremental` diagnostics and
    `metric_deltas`.
  - `ruststa check`: verifies `sta.rbin` on `--base-rev` using `--method independent|full|both`
    (`ruststa_check::rev` and/or a fresh full engine run) and commits `sta_check.json`
    (`ruststa.sta_check.v1`) and `status.sta_check`
    ([`docs/specs/sta/verification.md`](../../../docs/specs/sta/verification.md)).
- **Queries (`--rev <REV|HEAD>`, default `HEAD`):**
  - `ruststa path`: reconstructs point-by-point timing paths by handle (`--target
    path<N>|pin<N>|port<N>`) or endpoint name (`--to <NAME>`).
  - `ruststa inspect`: inspects worst early/late rise/fall arrivals, slews, required times, and
    slacks on `--target sel<N>|inst<N>|net<N>|pin<N>|port<N>|path<N>` or `--inst`/`--net`/`--pin`.
  - `ruststa report`: queries tabular sections (`summary`, `paths`, `endpoints`, `pins`, `clocks`,
    `checks`, `unconstrained`, `notes`) from memory-mapped `sta.rbin` with `--filter`, `--sort`,
    `--reverse`, `--limit`, and `--exact`.
  - `ruststa completions`: prints shell completion scripts (`bash`, `zsh`, `fish`, `elvish`,
    `powershell`).

## Build features and backends

`ruststa-cli` has no default Cargo features:

- Default build (`cargo build --release -p ruststa-cli`): compiles the `cpu` backend (`f64` default,
  `f32` opt-in).
- CUDA build (`cargo build --release -p ruststa-cli --features cuda`): adds the `cuda` and `gpu`
  backends through `rusteda-compute` (`f64` default, bit-identical to CPU; `f32` opt-in). Requesting
  an uncompiled or unavailable GPU backend exits `1` with `BACKEND_UNAVAILABLE` (`A0501`).

## Verification and benchmarks

- Integration tests under `tests/` cover the CLI contract (`cli_contract`), runnable documentation
  examples (`docs_examples`), engine-vs-checker agreement (`checker_agreement`), analytic cases,
  metamorphic properties, and planted defects; `cargo xtask verify sta` runs the registered suites.
- `benches/run_e2e.rs` measures end-to-end timing phases and RSS at `100k`, `500k`, and `1M` cells;
  `benches/backends.rs` compares CPU and GPU propagation backends.
