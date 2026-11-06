<p align="center">
  <img src="docs/assets/ruststa-wordmark.svg" alt="RustSTA" width="360">
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
  <img src="docs/assets/ruststa-banner.gif" alt="RustSTA analyzes gate-level Verilog, Liberty, SDC and SPEF with persistent multi-corner timing results, bounded queries and incremental updates" width="1200">
</picture>

RustSTA analyzes gate-level Verilog, Liberty, SDC and SPEF with persistent multi-corner timing results, bounded queries and incremental updates.

Part of **[RustEDA](https://github.com/lab-emi/RustEDA)**, infrastructure for chip-design recursive self-improvement. Native tools shorten the work between inference steps; verified results guide the next iteration.

## RustSTA vs OpenSTA at a glance

Ibex RTL-to-GDS on IHP SG13G2 with EZ130 cells and two SRAM macros, 2026-11-05: RustEDA and LibreLane 3.0.17 (OpenROAD, Yosys, KLayout, Magic, Netgen) ran one after the other on the same 4-vCPU host from the same prepared inputs, and the same signoff scripts checked both layouts.

- **1.6× faster than OpenSTA at 2 threads and 2.0× at 4** on the final Ibex netlist and SPEF at TT and SS (6.825 s and 5.317 s against 11.168 s and 10.566 s), with **17.8 % and 6.3 % less peak memory**.
- All eight compared worst slacks agree with OpenSTA **within 0.164 ps**.
- The flow's static timing takes 35.42 s against 183.31 s for LibreLane's OpenSTA steps (**5.2× faster**), and the native layout is timing-clean where LibreLane's has 112 capacitance and fanout violations.
- The whole RustEDA flow finishes **4.2× sooner end to end**.

LibreLane's layout fails the shared timing gate (112 maximum-capacitance and fanout violations at TT and SS) and its equivalence check does not complete, so the receipt keeps its own ratio fields null under its publication rule; the multiples here divide the recorded times. Stages are paired by phase, not as identical tasks. RustEDA is not faster everywhere: its repair before global routing takes 1.18× LibreLane's two pre-route resizer steps alone, OpenRCX runs 1.36× longer on RustEDA's DEF, and the latch-up check 1.11× longer on its layout. [All stages, gates and losses](https://lab-emi.github.io/RustEDA/benchmarks/)

## Latest development · 2026-11-05

The RustEDA Ibex host campaign of 2026-11-05 (IHP SG13G2 / EZ130 with two real
SRAMs, on a 4-vCPU Intel Xeon cloud host with 15.7 GiB RAM and no swap; a
different and slower machine than on 2026-11-02) ran RustSTA 0.4.3 (commit
[`3550739`](https://github.com/lab-emi/RustSTA/commit/35507399f3a298e8e94ff76cdbd24413e64e1236), CPU backend) in the repository's current native recipe and
against OpenSTA 3.1.0 on the same final netlist, SPEF, Liberty and SDC at TT and
SS. **Numerical agreement `passed` the 1 ps gate on this layout**: the eight
compared worst slacks (setup and hold, TT and SS, native and OpenRCX SPEF)
differ by at most 0.1639 ps, and the RustEDA producer completed every step. The
shared timing gate passed on the native layout with zero setup, hold, slew,
capacitance and fanout violations. The difference measured on the 2026-11-02
layout was not fixed: re-timed with this campaign's RustSTA and OpenSTA binaries
on a fresh OpenRCX SPEF of that layout, TT setup worst slack still differs by
1.2838 ps, as it did then. The gap is specific to that layout; the 2026-11-02
section below keeps it.

The head-to-head replays the flow's recorded commands on the native SPEF at 2
and 4 threads, with the same scope as before (RustSTA: init, import, one run
covering both corners and eight reports; OpenSTA: one process per corner).
**RustSTA was faster and used less memory on this task and host.**

| | RustSTA 0.4.3 | OpenSTA 3.1.0 |
| --- | ---: | ---: |
| Median wall, 2 threads (fastest) | 6.825 s (6.685 s) | 11.168 s (10.567 s) |
| Median wall, 4 threads (fastest) | 5.317 s (5.171 s) | 10.566 s (9.875 s) |
| Worst peak RSS, 2 threads | 148,164 KiB | 180,320 KiB |
| Worst peak RSS, 4 threads | 169,188 KiB | 180,528 KiB |

OpenSTA took 1.64× (2 threads) and 1.99× (4 threads) RustSTA's median wall
time. Wall times are medians of three measured repetitions after one warm-up,
interleaved; peak RSS is the worst of the three. The layout differs from
2026-11-02's (14,178 instead of 16,329 routed nets), so the two campaigns'
times are not a before/after of RustSTA alone. This is one task on one host.

[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-05) ·
[Host campaign 2026-11-05](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-05) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

## Ibex host campaign · 2026-11-02

The RustEDA Ibex host campaign (IHP SG13G2 / EZ130 with two real SRAMs, on a
4-vCPU Intel Xeon cloud host with 15.7 GiB RAM and no swap) ran RustSTA 0.4.3
(commit [`0c2bcc6`](https://github.com/lab-emi/RustSTA/commit/0c2bcc6a539580ed43aac1bad6badb82db4976c4),
CPU backend) and OpenSTA 3.1.0 on the same final netlist, SPEF, Liberty and SDC
at TT and SS. **Numerical agreement `failed` the 1 ps gate.** Of eight compared
worst slacks (setup and hold, TT and SS, native and OpenRCX SPEF), two exceed
it: TT setup differs by **1.4052 ps** on the native SPEF (49.279521 against
49.280926 ns) and by 1.2838 ps on the OpenRCX SPEF, with RustSTA reporting the
smaller slack in both. The other six values are within 0.2153 ps. This stopped
the RustEDA producer at its `timing_accuracy` gate. The design itself meets
timing: the shared timing gate, which keeps the worse of the two engines' values
on the OpenRCX SPEF, `passed` at TT and SS with zero setup, hold, slew,
capacitance and fanout violations.

The head-to-head replays the flow's recorded commands on the native SPEF at 2
and 4 threads. RustSTA's scope is its complete pipeline: init, import, one run
covering both corners and eight reports. OpenSTA's scope is two processes, one
per corner (read, link, SDC, SPEF, full reports). Wall time sums each
contender's processes; peak RSS is its largest single process. **OpenSTA was
faster and used less memory on this host.**

| | RustSTA 0.4.3 | OpenSTA 3.1.0 |
| --- | ---: | ---: |
| Median wall, 2 threads | 17.898 s | 15.253 s |
| Median wall, 4 threads | 15.976 s | 13.497 s |
| Worst peak RSS, 2 / 4 threads | 498,004 / 502,876 KiB | 266,916 / 267,704 KiB |

RustSTA took 1.17× (2 threads) and 1.18× (4 threads) OpenSTA's median wall
time. Wall times are medians of three measured repetitions after one warm-up,
interleaved; peak RSS is the worst of the three. This is one task on one host.

The measured source is the 2026-11-01 update, which added mixed
standard-cell/SRAM Liberty conventions, explicit global early/late timing
derates and driving-cell waveforms on extracted and propagated-clock paths. RC
preparation runs independent nodes within each graph level on a bounded rayon
pool, then commits results in topological order. The memory estimate includes
an additional 184 bytes per node for jobs/results.

[Current source and qualification](docs/development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta) ·
[Host campaign, method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#host-campaign-method-and-deviations) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

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
ruststa 0.4.3 (backends: cpu)
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
| `rusteda init` / `import` | Create a workspace and import Verilog, Liberty, SDC and SPEF |
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
| Setup/hold, latch borrowing, recovery/removal, gating checks | Ideal and explicit propagated primary clocks; generated clocks and CPPR remain outside scope |
| Constants and supported `-from`/`-to` timing exceptions | Ordered `-through` exception hops are rejected by the engine |
| Ideal, lumped wire-load and detailed SPEF RC networks, including resistor loops | Moment-matched pi, effective load and driver waveforms; disconnected components emit `T0151`; no crosstalk/CCS claim |
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
unverified. Compare pinned reference executables as black boxes and report
complete command wall time, peak memory, timing scope and numerical agreement
together. No general RustSTA-vs-OpenSTA speedup is claimed. Head-to-heads are
published with that scope on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-05).
In the 2026-11-05 Ibex host campaign RustSTA's complete pipeline was faster:
OpenSTA took 1.64× (2 threads) and 1.99× (4 threads) its median wall time, with
1.22× and 1.07× RustSTA's worst peak RSS, and the two agreed within 0.1639 ps on
identical SPEF. In the 2026-11-02 campaign, on another host and layout, OpenSTA
was faster (RustSTA 1.17× and 1.18× its median wall time) and the two differed
by up to 1.4052 ps, which `failed` the 1 ps gate. Each is one task on one host.

In the RustEDA Ibex flow on IHP SG13G2 / EZ130 (two real SRAMs), RustSTA drives
the frontend and post-route repairs and signs off TT/SS extracted timing beside
OpenSTA. On identical SPEF the two agreed within 0.806 ps in run16 (2026-10-30)
and within 0.4754 ps in R20/R21 (2026-11-01), against a 1 ps gate. The
2026-11-02 host campaign reran the run16-class recipe with RustSTA `0c2bcc6` and
measured 1.4052 ps, so the gate `failed`; the 2026-11-05 campaign ran the current
recipe with RustSTA `3550739` and measured 0.1639 ps on its layout, so the gate
`passed`. The whole-flow comparison with
LibreLane is on the
[RustEDA benchmarks page](https://lab-emi.github.io/RustEDA/benchmarks/).

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

### Extracted RC timing

`rusteda import --spef routed.spef.gz --spef-corner tt --base-rev rev1 -w work`
imports a bounded detailed RC extraction for a previously imported netlist and
corner. It can also be combined with `--lib`, `--netlist` and `--sdc` in one
transaction. Run the usual `ruststa run` and `ruststa check --method full` on the
returned revisions. The scope reports `spef_rc`, `spef` and
`nldm_ceff_ramp`; missing extraction and a different corner are errors. Resistor
loops are solved on the nodal conductance system, and a part of a net without a
resistor path to its driver joins the driver with warning `T0151`.

Plain text and gzip are accepted under the same encoded/decoded byte cap. CRC
errors, missing annotation and budget overruns fail
explicitly without committing a partial revision. This uses moment-matched pi loads, charge-matched effective capacitance,
a finite-ramp Thevenin driver and RC threshold/slew propagation, with
factor-one grounded coupling. Explicit propagated primary clocks are supported; crosstalk and CCS current tables are not. The independent checker currently reports extracted RC as
undecided; use full recomputation and the measured dual-PDK black-box suite.
See [the SPEF workflow](docs/guide/spef.md),
[parasitic timing](docs/specs/sta/parasitics.md) and
[the verification report](docs/verification/rc-report.html).
