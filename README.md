<p align="center">
  <img src="docs/assets/ruststa-wordmark.svg" alt="RustSTA" width="440">
</p>

<h1 align="center">Timing feedback for the next design iteration.</h1>

<p align="center">
  <a href="https://lab-emi.github.io/RustEDA/modules/ruststa/">Documentation</a> ·
  <a href="https://github.com/lab-emi/RustEDA">RustEDA</a> ·
  <a href="https://lab-emi.github.io/RustEDA/benchmarks/">Benchmarks</a> ·
  <a href="LICENSE">Apache-2.0</a>
</p>

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="docs/assets/ruststa-banner.svg">
  <img src="docs/assets/ruststa-banner.gif" alt="RustSTA analyzes gate-level Verilog, Liberty, SDC and SPEF with persistent multi-corner timing results, bounded queries and incremental updates" width="1200" height="360">
</picture>

RustSTA analyzes gate-level Verilog, Liberty, SDC and SPEF with persistent multi-corner timing results, bounded queries and incremental updates.

Part of **[RustEDA](https://github.com/lab-emi/RustEDA)**, infrastructure for chip-design recursive self-improvement. Native tools shorten the work between inference steps; verified results guide the next iteration.

## Quick start

Install [Rust with rustup](https://rustup.rs/) and Git. The checkout pins Rust 1.97.1.
The default build needs no GPU or Python runtime.

```console,lab
$ git clone https://github.com/lab-emi/RustSTA.git
$ cd RustSTA
$ cargo build --locked --release -p rusteda-cli -p ruststa-cli
```

Add the built binaries to your shell path, then confirm the CPU build:

```bash
export PATH="$PWD/target/release:$PATH"
```

```console
$ ruststa --version
ruststa 0.1.0 (backends: cpu)
```

From the repository root, analyze the bundled eight-cell chain in a fresh workspace:

```bash
FIX=verification/fixtures/synthetic/sta
WS=.work/first-timing

rusteda init --workspace "$WS" --format json
rusteda import --workspace "$WS" --base-rev rev0 \
  --lib "$FIX/lib/wt_15.lib" \
  --netlist "$FIX/net/chain_8.v" \
  --sdc "$FIX/sdc/chain_8.sdc" --format json
ruststa run --workspace "$WS" --base-rev rev1 \
  --threads 2 --max-paths 5 --max-state-gib 1 --format json
ruststa report --workspace "$WS" --rev rev2 --section summary --format json
ruststa path --workspace "$WS" --rev rev2 --target path1 --format json
ruststa check --workspace "$WS" --base-rev rev2 --method both --format json
```

The synthetic library is a test fixture. This flow creates `rev0` → `rev1` →
`rev2` → `rev3`; queries read `rev2`, and checking adds `sta_check.json` in `rev3`.
A successful operation, timing closure and a verification verdict are separate
results. Inspect corner, wire model and unconstrained endpoints alongside slack.

The [complete quickstart](docs/guide/quickstart.md) explains reports and artifacts.
For your own design, follow [the timing workflow](docs/guide/timing.md).

## A workspace you can inspect

| Command | Result |
|---|---|
| `rusteda init` / `import` | Create a workspace and import Verilog, Liberty and SDC |
| `ruststa run` / `update` | Analyze a revision and persist `sta.rbin` |
| `ruststa path` / `inspect` / `report` | Read timing paths, pins, endpoints and summaries |
| `ruststa check` | Compare saved timing with an independent checker, a full rerun, or both |
| `rusteda show` / `log` / `diff` / `export` | Inspect revisions and export structural Verilog |

Updates to an existing workspace require an explicit `--base-rev revN`.
A stale revision fails with `REVISION_CONFLICT` (exit 4). Queries accept `--rev`;
handles such as `path1` belong to the selected revision. Initialization creates
`rev0` and takes no base revision.

[Agent integration](docs/guide/agents.md) · [Commands and exit codes](docs/reference/cli.md) ·
[JSON reports and artifacts](docs/reference/reports.md)

## Timing scope

| Available now | Current boundary |
|---|---|
| Multi-corner 1D/2D/3D Liberty NLDM tables | No CCS/ECSM or sign-off accuracy claim |
| Setup/hold, latch borrowing, recovery/removal, gating checks | Ideal clocks; generated/propagated clocks and CPPR are outside current scope |
| Constants and supported `-from`/`-to` timing exceptions | Ordered `-through` exception hops are rejected by the engine |
| Ideal or lumped wire-load models | No SPEF annotation or distributed RC timing |
| CPU f64 and optional CUDA f64/f32 propagation | Metal/Accelerate is not a shipped backend |
| Incremental `TimingSession` Rust API | The CLI `ruststa update` currently reports full recomputation |

Unsupported semantic constructs produce diagnostics. See the
[complete support table](docs/reference/scope.md),
[backend guide](docs/guide/backends.md) and [scope](https://lab-emi.github.io/RustEDA/verification/).

## Verification and performance

The test infrastructure includes hand-derived timing cases, a separately
implemented `ruststa-check`, full-vs-incremental consistency checks, and pinned
**SkyWater `sky130_fd_sc_hd`** and **GlobalFoundries
`gf180mcu_fd_sc_mcu7t5v0` / `gf180mcu_fd_sc_mcu9t5v0`** Liberty libraries.

```console,lab
$ cargo xtask pdk fetch
$ cargo xtask verify pdk --profile pr --require
```

Coverage and limits are explicit: the independent checker can return `undecided`
on bounded large exception cases; a missing external tool or device is
unverified. No numerical RustSTA-vs-OpenSTA speedup is claimed by this release.
Compare pinned reference executables as black boxes and report complete command
wall time, peak memory, timing scope and numerical agreement together.

[Verification scope](docs/verification/index.md) · [Dual-PDK suite](docs/verification/pdk.md) ·
[Performance and memory](docs/guide/performance.md)

## Documentation

- [Get started](https://lab-emi.github.io/RustEDA/modules/ruststa/guide/quickstart/) and [install from source](docs/guide/installation.md).
- [Timing manual](docs/user/sta/README.md), [CLI manual](docs/user/cli/README.md), and [formal specifications](docs/specs/sta/README.md).
- [Architecture](docs/architecture/index.md), [requirements and evidence](docs/verification/requirements.md), and [RustEDA brand](docs/brand/README.md).
- [Build and publish the documentation](docs/about/maintaining-docs.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for specification-first development,
clean-room boundaries, independent verification and memory budgets.
[Development checks](docs/contributing.md) cover layout, formatting, shared
crates, fixtures and tests. Everyone participating follows the
[Code of Conduct](CODE_OF_CONDUCT.md).

Report vulnerabilities according to [SECURITY.md](SECURITY.md).
RustSTA is licensed under [Apache-2.0](LICENSE); see [NOTICE](NOTICE) and
[REUSE.toml](REUSE.toml).
