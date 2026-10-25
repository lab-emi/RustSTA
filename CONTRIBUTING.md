# Contributing to RustSTA

RustSTA is one of the tools of RustEDA, a suite of electronic design automation tools in Rust that
share their core crates and a disk workspace. This file holds the rules that every change follows:
the eight engineering rules of RustEDA, the rules of this repository, ownership and hand-over
between work items, and the checks to run before sending a change. The phases of the suite and
their work items are in the [scope](https://lab-emi.github.io/RustEDA/verification/).

Instruction files for coding agents, such as `AGENTS.md`, stay local and are never committed; this
file is the tracked statement of the rules.

## Engineering rules

The rules are numbered so that sources, specifications and reviews can cite them, for example as
Rule 7.1.

### Rule 1: Clean room and independent core

1. Never copy or translate source code under the GPL or another restrictive license, and never
   study the internals of a program whose license does not allow it. Algorithms from the
   literature are re-expressed in our own code and cited with a key from
   [`docs/references.bib`](docs/references.bib).
2. Other programs are used only as black boxes over standard file formats (Verilog, BLIF, AIGER,
   Liberty, SDC, SPEF, DEF). There is no code-level differential comparison: their source code is
   never read, diffed against ours, ported, or used to explain or close a difference. Their
   released binaries may be run and compared directly (Rule 8.4).
3. The core crates and the engine and checker crates never depend on Python, machine-learning
   frameworks or user-interface crates. User-interface crates are used only by `gui` and `apps`
   crates; `cargo xtask layout` enforces it.

### Rule 2: Daemonless disk workspace

1. No background daemons, resident servers or blocking prompts on standard input. Every command
   runs to completion or to a deterministic budget, commits or discards its work, and exits with a
   fixed exit status.
2. Every command works on a workspace directory (`-w/--workspace DIR`): it takes a shared lock on
   `workspace.lock` to read and an exclusive lock to commit, and reads binary tables through
   memory maps. Committed revisions are immutable.
3. Every mutating command takes `--base-rev REV` and fails with `REVISION_CONFLICT` when the
   workspace's `HEAD` is another revision. `HEAD` itself is never accepted as a base.

The file-based commands of the current engine (`ruststa sta`, `ruststa check sta`,
`ruststa show`) predate these rules; Phase 2a replaces them with commands on the workspace.

### Rule 3: Memory model and data structures

1. No `Rc<RefCell<T>>` graphs: mutable designs use generational typed indices (`InstId`, `NetId`,
   `PinId`, `CellId`) into arenas, so that a stale id fails with an error instead of naming another
   object.
2. Names are interned per design database, never in a process-global interner.
3. Mutable representations stay separate from the dense compute views (structures of arrays and
   compressed sparse rows) that CPU and GPU kernels read. The netlist, the timing graph and the
   other domain graphs are distinct structures, linked by id tables.

### Rule 4: Shell-safe handles and honest status

1. Handles (`rev42`, `path17`, `sel8`, `inst102`) match `[A-Za-z0-9_-]+`: never `#`, `$`, spaces
   or other characters that a shell interprets.
2. A property that was not checked is never reported as passing, and a metric that was not
   computed is never reported as zero: a design-rule check that did not run is reported as
   `"drc_status": "unchecked"`, a revision without timing as `"timing": "not_analyzed"`, and a
   metric without any checked endpoint as `null`. Every result states its scope: corner, wire
   model, parasitics and unconstrained endpoints.

### Rule 5: CPU first, then native GPU backends

1. Every task of `rusteda-compute` first has a deterministic pure-Rust CPU backend. The CPU
   backend is authoritative (timing runs on it in `f64`) and it is the default backend.
2. GPU backends are native: CUDA C kernels compiled at run time on Linux, and Metal on macOS as a
   roadmap item. Only `rusteda-compute` uses GPU runtime crates (`cudarc`, later the Metal
   bindings); portable GPU kernel languages are not used. A GPU backend that cannot be used is an
   error (`BACKEND_UNAVAILABLE`), never a silent fallback to the CPU.
3. Every GPU kernel has a consistency test against the CPU backend, with an explicit bound that
   the compute specification ([`docs/specs/compute/`](docs/specs/compute/)) states. The timing
   task computes on CUDA in `f64` by default, bit-identical to the CPU, and in `f32` only on
   request.
4. `unsafe` code exists only in the GPU glue of `rusteda-compute` and the memory maps of
   `rusteda-workspace`, under a module-scoped `allow`, with a `// SAFETY:` comment on every
   block. The kernel launches of the legacy engine (`crates/engines/ruststa/src/gpu/`) are the one
   documented exception until work item C2 removes them. Checker crates forbid unsafe code, and
   engine crates do as soon as their GPU code lives in `rusteda-compute`.

### Rule 6: Precision by algorithm, and determinism

1. Layout geometry and design-rule coordinates use integer database units (`i64`), never floating
   point.
2. Timing propagation merges arrivals with `max` and `min`, never by floating-point summation.
   Results depend neither on the number of threads nor on the order of statements in the inputs,
   and ties are broken by the canonical topological id (in adjacency lists, path ranks and
   endpoint order). The same inputs give byte-identical tables.
3. Parallel reductions on a GPU (density maps, gradients) offer a deterministic mode with 64-bit
   fixed-point integer atomics or reductions in a fixed order.
4. Ill-conditioned linear solvers and circuit-simulation residuals are verified in `f64`.

### Rule 7: No silent drops, and two default PDKs

1. Nothing in an input (Verilog, Liberty, SDC, later SPEF) is dropped silently. A construct is
   either used, or listed in the specification of its reader as having no effect on results, or
   reported: an unsupported or ignored construct produces a diagnostic, either an error or a coded
   warning aggregated per construct with its count and first location. No timing constraint and no
   library attribute is ever ignored without one.
2. Liberty, technology-mapping and timing features are tested on the standard cells of both
   default PDKs, `sky130_fd_sc_hd` and `gf180mcu_fd_sc_mcu7t5v0`/`gf180mcu_fd_sc_mcu9t5v0`, so
   that no feature is fitted to one library's naming or table conventions.
3. PDK libraries are fetched at pinned versions, with their SHA-256 sums and sizes recorded, into
   the directory that `RUSTEDA_PDK_DIR` names (default `target/pdk`); they are never committed.
   Tracked design data is synthetic and lives only below a `fixtures/synthetic/` directory.

### Rule 8: Compilation is only the entry ticket

1. Passing `cargo check` and self-written unit tests is not evidence of correctness.
2. No algorithm is validated only by checks written under its own assumptions. The specification
   and its hand derivations come first; the timing engine and the independent checker are then
   written by different authors from that specification. Timing changes keep incremental timing
   equal to a full run and the engine in agreement with the checker on both default PDKs. A
   timing convention enters a tracked suite only after a black-box comparison confirms it
   (Rule 8.4).
3. A change made for performance reports end-to-end wall-clock time, including memory maps, the
   lowering into compute buffers and host-device synchronization, together with its effect on the
   results; isolated kernel times are not enough. Times measured on a shared machine are
   indicative; acceptance numbers come from a quiet machine.
4. Comparisons with other programs run their released binaries as black boxes over standard file
   formats and compare what can be observed from outside: run time, peak memory and the outputs
   (quality of results, timing values, equivalence verdicts). Their harnesses, the pinned program
   versions, inputs fetched by checksum, results and thresholds may be tracked, may name the open
   programs, and may serve as acceptance gates. A program that is not installed leaves its
   comparison `unverified`, which fails under `--require` and is never reported as a pass. The
   specification, not another program, defines correct behavior: a difference is resolved against
   the specification, and a tracked tolerance names the program and its version.

## Repository rules

### Layout and files

- Crates live at `crates/<group>/<name>/`, with the groups `core`, `formats`, `engines`, `verify`,
  `gui` and `apps` and kebab-case names; the top level holds only the entries of an allowlist.
  `cargo xtask layout` checks both, and the dependency directions between the crate groups: for
  example, verification crates never depend on an engine, so the checker cannot reuse engine
  code.
- Files are at most 1 MiB, and Rust sources at most 1,200 lines: split a longer module.
- Every Rust source starts with an SPDX header:

  ```rust
  // SPDX-License-Identifier: Apache-2.0
  // SPDX-FileCopyrightText: 2026 RustSTA contributors
  ```

  Files of the shared crates and shared specifications name `2026 RustEDA contributors` instead.
  Other files take their license and copyright from [`REUSE.toml`](REUSE.toml).
- Design data (`.lib`, `.v`, `.sdc`, `.spef` and the like) is tracked only below a
  `fixtures/synthetic/` directory. The cell libraries of the fixtures are produced by
  `ruststa-fixtures`, and `cargo xtask fixtures` regenerates them in
  `verification/fixtures/synthetic/sta/lib/`. The gate-level netlists of the fixtures are checked
  in under `verification/fixtures/synthetic/sta/netlists/`; no tool of this repository
  regenerates them.
- Scratch output stays in directories that git ignores: `cargo xtask layout` also checks the
  untracked files that git does not ignore.

### Lab policy for tracked text

The lab that develops RustEDA applies these rules to every tracked file, and a pre-commit hook in
the lab checks them. Its word lists are kept outside the repository, because they consist of the
words that the rules keep out.

- Names of open third-party EDA programs appear only where a black-box comparison runs them
  (Rule 8.4): its harness, its manifest of pinned versions, its results and their documentation,
  and `docs/references.bib`.
- No names of commercial programs or foundries anywhere, the bibliography included: their licenses
  commonly forbid publishing comparisons, so a comparison with a commercial binary stays local.
- Another program's output is reported as a measured comparison (program, version, difference),
  never as the definition of correctness, and never explained by that program's source code
  (Rules 1.2 and 8.4).
- No absolute paths of a local machine (home directories, mount points) and no paths of local
  scratch directories.
- No name of the repository of another RustEDA tool: tracked text refers to such a tool by its
  role, for example the synthesis tool of the suite.

### Specifications and requirements

- Every behavior starts as a specification in `docs/specs/<area>/` with requirement IDs of the
  form `<AREA>-REQ-<nnnn>` (for example `STA-REQ-0400`). Tests cite the IDs they verify, and
  `cargo xtask coverage` fails on a requirement that no source or test cites.
- A requirement that a later work item implements carries the marker `(planned: <id>)` with the id
  of that work item, until the work item is accepted.
- A changed rule gets a new ID. Its superseded text moves to `docs/specs/sta/legacy.md`, which the
  tests of the legacy engine keep citing until that engine is deleted.
- Shared code and shared specifications use neutral single-token prefixes (`CORE`, `WS`,
  `COMPUTE`, `VERILOG`, `LIBERTY`, `SDC`, `JOB`, `RCLI`), never the prefix of one tool.
- Specifications and hand derivations cite textbooks, papers and format documents with keys from
  [`docs/references.bib`](docs/references.bib); manuals of commercial programs are not cited. A key
  that a shared file cites must be in the bibliography of every RustEDA tool repository, and a new
  one is added to all of them in the same change.
- Decisions that concern only RustSTA are recorded in [`docs/adr/`](docs/adr/). A decision about a
  shared crate is recorded in its shared specification (for example
  `docs/specs/compute/decisions.md`), and an older ADR on the same subject is marked superseded.

### Shared crates and specifications

- The crates `rusteda-core`, `rusteda-workspace` and `rusteda-compute` (`crates/core/`),
  `rusteda-formats` (`crates/formats/`) and `rusteda-cli` (`crates/apps/`), their specifications
  in `docs/specs/{core,workspace,compute,verilog,liberty,sdc,rusteda-cli}/`, and any other file
  that `xtask/src/shared.rs` lists are identical in the repositories of all RustEDA tools. Their
  wording is tool-neutral.
- After changing a shared file, run `cargo xtask shared update`, which records the change in
  `verification/shared.sha256`, and list the changed files with their SHA-256 in the hand-over
  report. `cargo xtask shared diff <other-repository>` lists what differs from a clone of another
  tool.
- One tool builds a shared item and the other reviews it before adopting it. Until the legacy
  engine is deleted, changes to the Liberty model, to the API of the Liberty reader and to the
  Verilog syntax tree are additive: no removal, rename or signature change, and a new field or
  variant only if the other tools still compile.

### Dependencies

- Third-party dependencies come from crates.io only, are pinned to exact versions (`=x.y.z`) in the root
  `[workspace.dependencies]`, and must pass `cargo deny check`.
- The pins follow a list that the RustEDA tools share. A third-party crate that is not on it is
  not added in a change; propose it with a justification first.

### Code

- Match the style of the surrounding code, document public APIs and remove dead code.
- The new timing engine lives in `crates/engines/ruststa/src/engine/` and the new checker in
  `crates/verify/ruststa-check/src/rev/`. The files of the legacy engine and checker are frozen
  until work item Z1 deletes them.
- Work of Phase 2b stays out of the work items of Phase 2a. Where 2a code needs a 2b feature, it
  calls the seam stub, which reports "not supported yet" with a diagnostic.

### Tests

- Test inputs are synthetic (Rule 7.3), and tests cite the requirement IDs they verify.
- Tests write only below `CARGO_TARGET_TMPDIR` (integration tests and benchmarks) or below
  `std::env::temp_dir()`, which follows `TMPDIR`, never to a fixed path such as `/tmp`.
- GPU tests are ignored by default and run with `cargo xtask verify sta --gpu` on a machine with an
  NVIDIA GPU and the NVRTC library of a CUDA toolkit. They must pass before a change to
  `rusteda-compute` or to the lowering of the engine onto it is merged.

## Ownership and hand-over

Work is divided into work items (see the [scope](https://lab-emi.github.io/RustEDA/verification/)), each with its
specification, its acceptance criteria and the paths it owns.

- Every path has exactly one owner at a time. A work item edits only the paths it owns. An edit
  outside them is allowed only when it is unavoidable and small (a module declaration, a
  dependency line, a call-site update), and the hand-over report lists it.
- A few files have no owner and are merged: `Cargo.lock`; the generated
  `docs/verification/requirements.md` and `verification/shared.sha256`, which are regenerated after
  a merge (`cargo xtask coverage`, `cargo xtask shared update`); and `docs/references.bib`, to
  which entries are only appended, merged by key.
- The dependency pins of the root `Cargo.toml` change only with the shared pin list. The dispatch
  files of the shared crates (the command dispatcher of `rusteda-cli` and `lib.rs` of
  `rusteda-workspace`) belong to the workspace work item (W1C), which declares the modules that
  other work items provide.
- Hand-overs are explicit: a path that changes owner is named, with its new owner, in the
  hand-over report of the work item that gives it up. In Phase 2a, E0 hands the scaffolds of the
  new engine and checker trees to the work items that fill them; the legacy engine, the legacy
  checker and their tests are frozen and pass to Z1, which deletes them; the legacy GPU module
  passes through C2 to Z1.
- Dependencies between work items gate acceptance, not the start of work: a work item may start
  on the published interface of a dependency.
- A work item is merged as soon as it passes its acceptance criteria and the checks below. Two
  reviews follow, and their fixes land as a second merge. Fixes keep public APIs stable where they
  can; a fix that has to change an API updates every user in the same merge.
- The hand-over report states what was built; API notes for dependents (types, functions,
  invariants); acceptance results with numbers; deviations from the specification, with reasons;
  edits outside the owned paths; the shared files changed, with their SHA-256; and follow-ups.
- The timing engine and the independent checker are written by different authors (Rule 8.2).

## Checks before sending a change

Run from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features   # after the tests of the changed crates
cargo xtask layout
cargo xtask coverage                    # rewrites docs/verification/requirements.md
cargo xtask shared update               # only after changing a shared file
cargo xtask shared check                # the shared files match verification/shared.sha256
cargo xtask fixtures
git status --porcelain -- verification/fixtures   # no output: the committed fixtures are fresh
cargo xtask verify sta
cargo deny check
reuse lint
```

- `cargo xtask layout` checks locations, SPDX headers, sizes, the places of design data and the
  dependency rules ([Layout and files](#layout-and-files)).
- `cargo xtask coverage` fails when a requirement is cited by no source or test and carries no
  `(planned: <id>)` marker.
- `cargo xtask verify sta` runs the verification suites in release mode on the build without GPU
  backends; `--gpu` adds the GPU tests ([Tests](#tests)).
- A verification product that a work item adds (the SDC and compute suites, the dual-PDK suite with
  its PDK fetch) brings its `cargo xtask` command into this list.
- A change made for performance adds the end-to-end measurement of Rule 8.3, and a change of
  timing semantics the black-box comparison of Rule 8.4, to its report.
- In the lab, the pre-commit hook checks the [lab policy](#lab-policy-for-tracked-text) on every
  commit.
- `cargo deny check` needs `cargo install --locked cargo-deny`, and `reuse lint` needs the
  [REUSE tool](https://reuse.software).

## Commits

- Conventional Commits, for example `feat(sta): check recovery and removal arcs (STA-REQ-0211)`.
- The body explains what changed and why, in terms of specifications and tests.
- Contributions are accepted under the Developer Certificate of Origin: sign off every commit
  (`git commit -s`).


### Canonical first-party foundation (2026-10-25)

The four shared library implementations and the workspace CLI implementation are owned by
lab-emi/RustEDA-Common, the public first-party foundation. Workspace dependencies and Cargo.lock
pin one exact revision over HTTPS; no GitHub credentials are required. The local rusteda-cli package is a
product adapter and retains its package version in command output and revision history.

This supersedes the legacy requirement to copy shared implementation files between tool
repositories, and is the sole first-party exception to the crates.io-only source policy.
Third-party package versions, licenses and source restrictions are unchanged. Shared specification
and automation manifests still require deliberate updates. Canonical core tests run in the
foundation repository; product adapters and engines retain their local tests.

Cross-repository validation uses the pinned foundation source. The historical public main
branch retains its existing build policy until a reviewed migration is merged.
