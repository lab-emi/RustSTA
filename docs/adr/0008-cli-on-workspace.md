# ADR 0008: Workspace-native `ruststa` command line and `rusteda.v1` envelopes

- Status: accepted
- Date: 2026-10-17

## Context

In Phase 1, `ruststa` operated as a standalone file-oriented command (`ruststa sta --lib ... --netlist ... --sdc ...`, `ruststa check sta`, `ruststa show`) that parsed raw Verilog, Liberty, and SDC files on every invocation and wrote ad-hoc run records (`run.json`, `report.json`, `events.jsonl`) under `--runs-root` or `--run-dir` ([ADR 0002](0002-front-ends.md)). That model had three structural limitations at scale:

1. **Redundant front-end parsing on every command**: inspecting a path, pin, or endpoint table required re-reading the full JSON report or re-running analysis, and multi-tool flows with `rusteda` had no shared revision history or carry-forward invalidation.
2. **Ad-hoc output schemas**: Phase 1 text and JSON outputs differed between `sta`, `check sta`, and `show`, and did not provide the uniform `rusteda.v1` envelope (`schema`, `ok`, `command`, `workspace`, `base_rev`, `rev`, `new_rev`, `scope`, `completeness`, `metrics`, `diagnostics`, `error`, `next_actions`) or stable object handles (`path<N>`, `inst<N>`, `net<N>`, `pin<N>`, `port<N>`, `mm<N>`, `sel<N>`).
3. **Unverified incremental and multi-method checking**: Phase 1 had no workspace revision mutation for incremental updates (`ruststa update`) or dual-method verification (`ruststa check --method independent|full|both`) committing `sta_check.json` alongside `sta.rbin` ([ADR 0007](0007-sta-store.md)).

## Decision

We replace the Phase 1 file-and-run-record CLI (`sta`, `check sta`, `show`, `--runs-root`, `@` run references) with a workspace-native `ruststa` command line (`crates/apps/ruststa-cli`) governed by [`docs/specs/cli/README.md`](../specs/cli/README.md) and [`docs/specs/sta/cli.md`](../specs/sta/cli.md):

1. **Workspace-only mutations and queries**:
   - Generic workspace lifecycle commands (`init`, `import`, `log`, `show`, `diff`, `revert`, `export`) live in `rusteda` (`crates/apps/rusteda-cli`).
   - `ruststa` provides three workspace-mutating commands that require `--base-rev <revN>` (never `HEAD`), acquire the workspace lock (`rusteda_workspace::Workspace::begin`), and append to `history.jsonl`:
     - `ruststa run`: runs multi-corner timing analysis on `netlist.rbin`, `.rlib`, and `constraints`, stages `sta.rbin` (`STA v1`, `--sta-store full|summary`), and populates `meta.json` `summary.sta` and `status.sta` (`timing = "met" | "violated"`).
     - `ruststa update`: resolves the nearest ancestor revision with a valid `sta` table; returns `incremental.mode = "up_to_date"` (`new_rev: null`) when `base_rev` already carries a valid `sta` table, runs cone-scoped incremental timing when `<= 25%` of instances changed and constraints/libraries are unchanged, and falls back to a full run otherwise. Supports `--verify-full` and `--check`.
     - `ruststa check`: verifies a revision's `sta.rbin` via `--method independent` (`ruststa_check::rev` reading `cache/*.lib`, round-tripped Verilog, and canonical SDC without depending on the engine), `--method full` (fresh full engine recomputation), or `--method both`, committing `sta_check.json` (`ruststa.sta_check.v1`) and `status.sta_check` (`verdict = "agree" | "disagree" | "undecided"`).
   - `ruststa` provides three read-only mapped query commands (`--rev <REV|HEAD>`, default `HEAD`, never logging to `history.jsonl`) that open `sta.rbin` in `O(1)` time via `StaStore::open_rev`:
     - `ruststa path`: queries paths by `--target path<N>|pin<N>|port<N>` or `--to <NAME>`.
     - `ruststa inspect`: inspects `--target sel<N>|inst<N>|net<N>|pin<N>|port<N>|path<N>` or `--inst`/`--net`/`--pin` across tags.
     - `ruststa report`: renders tabular sections (`summary`, `paths`, `endpoints`, `pins`, `clocks`, `checks`, `unconstrained`, `notes`) with `--filter`, `--sort`, `--reverse`, and `--limit`.
2. **Pre-scanned `--format json` and deterministic `rusteda.v1` envelopes**:
   - Raw `argv` is pre-scanned before Clap parsing so `--format json` wraps usage errors (`USAGE`, `A0003`), `--help`, and `--version` in a single `rusteda.v1` JSON envelope on `stdout`.
   - Every envelope preserves the exact top-level and nested (`scope`, `completeness`, `metrics`) key order, canonicalizes `-0.0` to `+0.0`, rounds percentages to `0.1`, and emits POSIX-quoted `next_actions` (quoting leading `=` tokens for `zsh`).
3. **Deterministic 1-based handles and underscore typo hints**:
   - Handles follow `[A-Za-z0-9_-]+`: `path<N>` (1-based rank over setup then hold checks), `inst<N>`/`net<N>`/`pin<N>`/`port<N>` (`1 +` canonical `NETLIST v1` row), `mm<N>` (checker mismatch rank), and `sel<N>` (`ui_state.json` selection tied to its revision).
   - Malformed underscore handles such as `inst_1` fail with `UNKNOWN_HANDLE` (exit 1, `W0007`/`T0008`) and a `did you mean 'inst1'?` diagnostic hint.
4. **Exit code contract**:
   - `0` (`OK`), `1` (`REJECTED`: `USAGE`, `INPUT_REJECTED`, `NOT_A_WORKSPACE`, `WORKSPACE_EXISTS`, `UNKNOWN_REVISION`, `MISSING_TABLE`, `UNKNOWN_HANDLE`, `SCHEMA_TOO_NEW`, `IO_ERROR`, `BACKEND_UNAVAILABLE` with `A0501`), `2` (`INTERNAL` / `CORRUPT_TABLE`), `3` (`NEGATIVE_VERDICT`: `verdict == "disagree"` with `ok: true`), `4` (`REVISION_CONFLICT`), `5` (`INCONCLUSIVE`: `verdict == "undecided"` with `ok: true`), and `130` (`CANCELLED`).

## Consequences

- Read-only queries (`ruststa path`, `ruststa inspect`, `ruststa report`) execute directly against `mmap`'d `sta.rbin` sections without parsing Verilog, Liberty, or SDC files, meeting the sub-10 ms cold and sub-1.5 ms warm query latency targets on 1M-cell designs.
- `--threads 1` and `--threads 32` produce byte-for-byte identical `sta.rbin` tables and `stdout` payloads.
- End-to-end phase wall times and RSS (`open`, `graph`, `lower`, `propagate`, `backward`, `checks`, `paths`, `sta.rbin write`, `commit`) are continuously benchmarked by `crates/apps/ruststa-cli/benches/run_e2e.rs`.
