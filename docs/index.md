# RustSTA

RustSTA analyzes gate-level Verilog, Liberty, SDC and SPEF with persistent multi-corner timing results, bounded queries and incremental updates.

[All RustEDA modules](https://lab-emi.github.io/RustEDA/)

<picture>
  <source media="(prefers-reduced-motion: reduce)" srcset="assets/ruststa-banner.svg">
  <img src="assets/ruststa-banner.gif" alt="Timing feedback for the next design iteration." width="1200">
</picture>

## RustSTA vs OpenSTA at a glance

Ibex RTL-to-GDS on IHP SG13G2 with EZ130 cells and two SRAM macros, 2026-11-13: RustEDA and LibreLane 3.0.17 (OpenROAD, Yosys, KLayout, Magic, Netgen) ran one after the other on the same 4-vCPU host from the same prepared inputs, and the same signoff scripts checked both layouts. Every RustEDA engine was built with Rust 1.100.0, and RustEDA's step graph ran under its Rust executor, `rusteda flow exec`.

- **1.6× faster than OpenSTA at 2 threads and 2.0× at 4** on the final Ibex netlist and SPEF at TT and SS (4.692 s and 3.403 s against 7.349 s and 6.881 s). After the campaign, RustSTA `de210a6` uses **29.5 % less peak memory than OpenSTA at 2 threads and 12.6 % less at 4** (worst 124,120 and 154,144 against 176,172 and 176,308 KiB, in same-load runs with OpenSTA and the campaign's binary), with unchanged speed and byte-identical outputs (in the campaign: 13.6 % less at 2 threads but 11.8 % more at 4, 197,060 against 176,252 KiB).
- All eight compared worst slacks agree with OpenSTA **within 0.270 ps**.
- The flow's static timing takes 26.96 s against 138.93 s for LibreLane's OpenSTA steps (**5.2× faster**), and the native layout is timing-clean where LibreLane's has 112 capacitance and fanout violations.
- The frontend STA and ECO stage, whose ECO rounds and planning helpers are now `rusteda` commands built by this module, takes 13.85 s against 48.36 s for LibreLane's four resizer repair steps (**3.5× faster**); in same-load runs it took a median 13.21 s against 18.20 s for LibreLane's two resizer steps before global routing alone (**0.73×**, ranges do not overlap; 1.18× on 2026-11-05).
- The whole RustEDA flow finishes **4.4× sooner end to end** (751.54 s against 3,283.05 s).

LibreLane's layout fails the shared timing gate (112 maximum-capacitance and fanout violations at TT and SS) and its equivalence check does not complete, so the receipt keeps its own ratio fields null under its publication rule; the multiples here divide the recorded times. Stages are paired by phase, not as identical tasks. Against the three losses of 2026-11-05, in same-load runs: repair before global routing now takes 0.73× the time of LibreLane's two pre-route resizer steps (ranges do not overlap; 1.18× on 2026-11-05), OpenRCX on RustEDA's layout is level with its time on LibreLane's (1.00×, ranges overlap; 1.36× on 2026-11-05) and the latch-up check on RustEDA's layout takes 0.76× its time on LibreLane's (1.11× with the checker of that campaign). RustEDA is still not faster or not better everywhere: the shared timing gate (RustSTA and OpenSTA on each layout's OpenRCX SPEF) is level (0.98×, ranges overlap), RustROAD's standalone extraction took 1.22× OpenRCX's time at 4 threads in the campaign and, after three changes since, is narrowly faster at 4 threads (0.85×, faster in all 30 runs of two A/Bs; the ranges are apart in one and overlap by 47 ms in the other) and still 1.52× slower at 2 ([third round](https://lab-emi.github.io/RustEDA/benchmarks/#rustroad-extraction-third-round-2026-11-14)), and the native layout keeps less hold margin. RustSTA's peak memory at 4 threads, 11.8 % above OpenSTA's in the campaign, is 12.6 % below it since a change after the campaign ([details](#peak-memory-below-opensta-2026-11-13)). [All stages, gates and losses](https://lab-emi.github.io/RustEDA/benchmarks/#ibex-host-campaign-2026-11-13)

## PULPissimo benchmark scope

The [PULPissimo benchmark](https://lab-emi.github.io/RustEDA/benchmarks/pulpissimo/) is synthesis-only and does not time RustSTA. It compares the PULPissimo Ibex core with a bounded Yosys 0.70/ABC reference on SKY130 HD; no SPEF, constraint run or timing result is part of it. On SKY130 the full PULPissimo SoC still exceeds a 2 h synthesis budget with behavioral L2 memory; an IHP SG13G2 / EZ130 synthesis-only test with 80 official SRAM macros completes in 426 s. Neither is a physical signoff result.

## Peak memory below OpenSTA · 2026-11-13

After the 2026-11-13 host campaign, RustSTA commit
[`de210a6`](https://github.com/lab-emi/RustSTA/commit/de210a66e5cef12e9e0cb11eec8a2e11ee77f0ba)
brought the peak memory of the Ibex timing section below OpenSTA's at both
thread counts. In 7 interleaved same-load runs with OpenSTA and the campaign's
binary, **its worst peak RSS was 154,144 KiB against OpenSTA's 176,308 KiB at
4 threads (12.6 % less)**, where the campaign's binary used 194,124 KiB
(10.1 % more) in the same runs, and **124,120 against 176,172 KiB at 2 threads
(29.5 % less)**. Its speed is unchanged (wall and CPU ranges overlap with the
campaign binary's): 1.94× faster than OpenSTA at 4 threads and 1.56× at 2 in
these runs.

The extra memory was memory that glibc's allocator kept resident after it was
freed, not live data: the live heap peaks at about 135 MiB at 4 threads, and
the resident peak was 30 to 54 MiB above it. The campaign's one analysis
change since 2026-11-05, `c55e325`, added about 13 MiB of such kept pages at
4 threads and no live heap. The `ruststa` program now fixes glibc's mmap
threshold at 1 MiB, unless the environment sets it, and returns the allocator's
free pages at the end of every analysis phase. Given the same inputs at one
work path, `de210a6` and the campaign's binary write byte-identical outputs in
all 11 steps, and a full native run with the new RustSTA and RustROAD binaries
produced a GDS, DEF, native SPEF, netlists and mapped netlist byte-identical to
the campaign's. RustSTA's live heap is unchanged.

[Table, method and what remains](development-status.md#peak-memory-below-opensta-2026-11-13) ·
[RustSTA peak memory and RustROAD extraction](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-peak-memory-and-rustroad-extraction-2026-11-13) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

## Ibex host campaign · 2026-11-13

In the RustEDA Ibex host campaign of 2026-11-13 (IHP SG13G2 / EZ130 with two
real SRAMs, on a 4-vCPU cloud host), RustSTA 0.4.3 (commit [`d453782`](https://github.com/lab-emi/RustSTA/commit/d453782b118224ee27517857a59ad5a597a0fb4c), built
with Rust 1.100.0) and OpenSTA 3.1.0 analysed the same final netlist, SPEF,
Liberty and SDC at TT and SS. **Numerical agreement `passed` the 1 ps gate**:
the eight compared worst slacks differ by at most 0.2697 ps, and the shared
timing gate passed with zero setup, hold, slew, capacitance and fanout
violations.

On the native SPEF, RustSTA's complete pipeline took a median 4.692 s at
2 threads and 3.403 s at 4 threads; OpenSTA's two per-corner processes took
7.349 s and 6.881 s (1.57× and 2.02×). Worst peak RSS was 152,088 and
197,060 KiB against 176,060 and 176,252 KiB. **RustSTA was faster at both
thread counts, but its peak memory at 4 threads was 11.8 % above OpenSTA's** in
the campaign, where it was 6.3 % below on 2026-11-05, on another host; at
2 threads it was 13.6 % below. The campaign did not investigate the cause of
the increase. Since then it has been found and fixed: with `de210a6` the worst
peak RSS is 12.6 % below OpenSTA's at 4 threads and 29.5 % below at 2 in
same-load runs ([above](#peak-memory-below-opensta-2026-11-13)). Times are
medians of three interleaved measured repetitions after one warm-up.

Since 2026-11-05 the frontend ECO rounds and planning helpers became `rusteda`
commands built by this module, and the one analysis change, `c55e325`, prepares
each node's RC waveforms as soon as its sources are done. The frontend stage
took 13.85 s in the flow; in same-load runs it took a median 13.21 s against
18.20 s for LibreLane's two resizer steps before global routing (0.73×, ranges
do not overlap; 1.18× on 2026-11-05). The host is a different virtual machine
from 2026-11-05's, on which every LibreLane stage took less time, so the two
campaigns' times are history, not a before/after.

[Scope and all eight compared values](development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-13) ·
[Where RustEDA still loses or is at parity](https://lab-emi.github.io/RustEDA/benchmarks/#where-rusteda-still-loses-or-is-at-parity-2026-11-13) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

## Ibex host campaign · 2026-11-05

In the RustEDA Ibex host campaign of 2026-11-05 (IHP SG13G2 / EZ130 with two
real SRAMs, on a 4-vCPU cloud host), RustSTA 0.4.3 (commit [`3550739`](https://github.com/lab-emi/RustSTA/commit/35507399f3a298e8e94ff76cdbd24413e64e1236)) and
OpenSTA 3.1.0 analysed the same final netlist, SPEF, Liberty and SDC at TT and
SS. **Numerical agreement `passed` the 1 ps gate**: the eight compared worst
slacks differ by at most 0.1639 ps, and the shared timing gate passed with zero
setup, hold, slew, capacitance and fanout violations.

On the native SPEF, RustSTA's complete pipeline took a median 6.825 s at
2 threads and 5.317 s at 4 threads; OpenSTA's two per-corner processes took
11.168 s and 10.566 s. Worst peak RSS was 148,164 and 169,188 KiB against
180,320 and 180,528 KiB. **RustSTA was faster and used less memory on this
task and host** (three interleaved measured repetitions after one warm-up).
The layout and host differ from 2026-11-02's, below.

[Scope and all eight compared values](development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-05) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

## Ibex host campaign · 2026-11-02

In the RustEDA Ibex host campaign (IHP SG13G2 / EZ130 with two real SRAMs, on a
4-vCPU cloud host), RustSTA 0.4.3 and OpenSTA 3.1.0 analysed the same final
netlist, SPEF, Liberty and SDC at TT and SS. **Numerical agreement `failed` the
1 ps gate**: TT setup worst slack differs by 1.4052 ps on the native SPEF and
1.2838 ps on the OpenRCX SPEF; the other six compared values are within
0.2153 ps. The shared timing gate, which keeps the worse of the two engines'
values on the OpenRCX SPEF, `passed` at TT and SS with zero setup, hold, slew,
capacitance and fanout violations.

On the native SPEF, RustSTA's complete pipeline (init, import, one run covering
both corners, eight reports) took a median 17.898 s at 2 threads and 15.976 s at
4 threads. OpenSTA's two per-corner processes took 15.253 s and 13.497 s. Worst
peak RSS was 498,004 and 502,876 KiB against 266,916 and 267,704 KiB. **OpenSTA
was faster and used less memory on this host.** Wall time sums each
contender's processes and is the median of three interleaved measured
repetitions after one warm-up; peak RSS is the largest single process, worst of
the three. This is one task on one host.

[Scope, all eight compared values and method](development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

## Development update · 2026-11-01

The source now includes mixed standard-cell/SRAM Liberty conventions, explicit
global early/late timing derates and driving-cell waveforms on extracted and
propagated-clock paths. RC preparation runs independent nodes within each graph
level on a bounded rayon pool, then commits results in topological order. The
memory estimate includes an additional 184 bytes per node for jobs/results.

[Source, evidence and remaining limits](development-status.md)


## Development status · 2026-10-28

**RustSTA 0.4.3** adds non-tree extracted RC networks to the integrated timing
flow. SPEF import, effective-capacitance waveforms and propagated primary clocks
are supported. Dual-PDK reference/circuit checks accompany full/incremental and
CPU/CUDA consistency checks. Mixed-library SRAM timing and global derates remain
experimental work outside this snapshot.

[Progress, measured agreement and known residuals →](development-status.md)

## Analyze. Trace. Iterate.

Run multi-corner static timing analysis, keep the result as an immutable revision,
then inspect the paths and endpoints that need attention. Queries read the
memory-mapped timing store without rerunning propagation.

```bash
ruststa run --workspace ./ws --base-rev rev1 --max-paths 5 --format json
ruststa report --workspace ./ws --rev rev2 --section summary --format json
ruststa path --workspace ./ws --rev rev2 --target path1 --format json
```

Start with the [bundled design](guide/quickstart.md) or
[import your own netlist](guide/timing.md).

## A CLI you can compose

<div class="grid cards" markdown>

- **Follow the critical path.**

    ---

    Analyze setup, hold, recovery, removal, latches and clock gating.
    Trace a path, inspect a pin, or filter endpoint reports.

    [Timing workflow →](guide/timing.md)

- **Give agents a contract.**

    ---

    JSON on stdout, coded diagnostics on stderr, shell-safe handles,
    and explicit revision guards for every update to an existing workspace.

    [Agent integration →](guide/agents.md)

- **Keep the computation predictable.**

    ---

    Deterministic CPU reference propagation in f64. Optional native CUDA.
    Explicit path and propagation-state budgets.

    [Backends and precision →](guide/backends.md)

- **Inspect the evidence.**

    ---

    Separate checker and full rerun modes, analytic fixtures, incremental
    consistency checks, and pinned Sky130 and GF180 libraries.

    [Verification scope →](verification/index.md)

</div>

## From source to a timing report

| Step | Input | Result |
|---|---|---|
| Import | Structural Verilog, Liberty, SDC | A design revision with compiled input tables |
| Analyze | An explicit base revision and operating corners | `sta.rbin`, summary and ranked paths |
| Inspect | A revision, path handle or endpoint filter | JSON or text reports without another analysis |
| Check | The stored result and a verification method | `sta_check.json` with explicit status and diagnostics |

The default CPU build needs no GPU or Python runtime. The
[quickstart](guide/quickstart.md) uses checked-in synthetic inputs and shows the
complete import → analysis → report → check flow.

## Choose your path

| I want to… | Start here |
|---|---|
| Build the tools and analyze a small design | [Installation](guide/installation.md) → [quickstart](guide/quickstart.md) |
| Analyze my own netlist across corners | [Timing workflow](guide/timing.md) → [supported scope](reference/scope.md) |
| Drive timing from an agent or CI | [Agent guide](guide/agents.md) → [CLI](reference/cli.md) → [reports](reference/reports.md) |
| Assess correctness and verification limits | [Checking](guide/checking.md) → [evidence](verification/index.md) |
| Measure runtime or choose CPU/CUDA | [Performance](guide/performance.md) → [backends](guide/backends.md) |
| Contribute to RustSTA | [Architecture](architecture/index.md) → [contributing](contributing.md) |

!!! note "Analysis scope"
    RustSTA 0.4.3 is a source preview. It supports ideal and explicitly propagated
    primary clocks, ideal/lumped wires and detailed SPEF RC networks. Generated
    clocks, CPPR, CCS and crosstalk remain outside scope. The independent checker
    returns `undecided` for extracted RC; full recomputation checks consistency.
    Check the [support table](reference/scope.md) before interpreting a result.

Part of the [RustEDA tool family](https://github.com/lab-emi).
Use [RustSys](https://lab-emi.github.io/RustEDA/modules/rustsys/) to synthesize RTL to standard cells,
then analyze the exported structural netlist with RustSTA.
