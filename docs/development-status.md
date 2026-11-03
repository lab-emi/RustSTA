# Development status

Updated **2026-11-02**. This page describes the source published with the
RustEDA development update. Use the superproject’s exact gitlinks and the
repository lockfile when reproducing a run.

## Ibex host campaign · 2026-11-02

The RustEDA host campaign reran Ibex with two real SRAMs on IHP SG13G2 / EZ130
on a 4-vCPU Intel Xeon cloud host (15.7 GiB RAM, no swap), with RustSTA 0.4.3 at
commit
[`0c2bcc6`](https://github.com/lab-emi/RustSTA/commit/0c2bcc6a539580ed43aac1bad6badb82db4976c4)
(CPU backend), the source described below, and OpenSTA 3.1.0 from the nixpkgs
OpenROAD 26Q3 build. The native flow used the published run16-class recipe and
is `unqualified`: the component-qualification evidence that the 16-gate
composer needs is not published.

**Numerical agreement `failed` the 1 ps gate.** Both engines analysed the same
final netlist, Liberty and SDC with each of the two SPEF files, CPPR off. Two of
the eight compared worst slacks exceed 1 ps, both TT setup, and in both RustSTA
reports the smaller slack:

| SPEF | Corner | Check | RustSTA | OpenSTA | Difference |
| --- | --- | --- | ---: | ---: | ---: |
| Native | TT | Setup | 49.279521 ns | 49.280926 ns | **1.4052 ps** |
| Native | TT | Hold | 0.147036 ns | 0.147072 ns | 0.036 ps |
| Native | SS | Setup | 48.840157 ns | 48.840347 ns | 0.1906 ps |
| Native | SS | Hold | 0.268527 ns | 0.268436 ns | 0.0909 ps |
| OpenRCX | TT | Setup | 49.296423 ns | 49.297707 ns | **1.2838 ps** |
| OpenRCX | TT | Hold | 0.142664 ns | 0.142711 ns | 0.0476 ps |
| OpenRCX | SS | Setup | 48.868532 ns | 48.868607 ns | 0.0746 ps |
| OpenRCX | SS | Hold | 0.265683 ns | 0.265467 ns | 0.2153 ps |

The failure stopped the RustEDA producer at its `timing_accuracy` gate after
114 of 119 manifest steps; the remaining gates ran separately with the recipe's
scripts. The design itself meets timing: the shared timing gate, which keeps the
worse of the two engines' values on the OpenRCX SPEF, `passed` at TT and SS with
zero setup, hold, slew, capacitance and fanout violations. The residual is
reported, not waived.

The module head-to-head replays the flow's recorded commands on the native SPEF
at 2 and 4 threads (`--threads` for RustSTA, `-threads` for OpenSTA). Each
repetition runs in a fresh copy of the timing directory with every command
under GNU time. RustSTA's scope is init, import, one run covering both corners
and eight reports; OpenSTA's is one process per corner (read, link, SDC, SPEF,
full reports). Wall time sums each contender's processes, and peak RSS is its
largest single process. Each contender had one warm-up and three measured
repetitions, interleaved.

| | RustSTA 0.4.3 | OpenSTA 3.1.0 |
| --- | ---: | ---: |
| Median wall, 2 threads (fastest) | 17.898 s (17.463 s) | 15.253 s (14.746 s) |
| Median wall, 4 threads (fastest) | 15.976 s (14.992 s) | 13.497 s (13.041 s) |
| Worst peak RSS, 2 threads | 498,004 KiB | 266,916 KiB |
| Worst peak RSS, 4 threads | 502,876 KiB | 267,704 KiB |

**OpenSTA was faster and used less memory on this host.** RustSTA's median wall
time was 1.17× OpenSTA's at 2 threads and 1.18× at 4 threads, with 1.87× and
1.88× its worst peak RSS. These quotients apply to this task on this host only.

[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta) ·
[Host campaign, method and deviations](https://lab-emi.github.io/RustEDA/benchmarks/#host-campaign-method-and-deviations) ·
[Measuring performance](guide/performance.md)

## Mixed libraries and parallel RC preparation

The source now includes mixed standard-cell/SRAM Liberty conventions, explicit
global early/late timing derates and driving-cell waveforms on extracted and
propagated-clock paths. RC preparation runs independent nodes within each graph
level on a bounded rayon pool, then commits results in topological order. The
memory estimate includes an additional 184 bytes per node for jobs/results.

Recorded qualification for the parallel-RC source: 259 regular tests passed
(17 ignored), 10 real-PDK campaigns passed across Sky130 HD and GF180 7T/9T,
and 332 OpenSTA black-box comparison values retained the previous maxima
(0.036 ps Sky130, 0.049 ps GF180; 1 ps gate). Full/incremental and 1/2/8-thread
replays retained bit-identical timing tables on the recorded extracted designs.
The two complete Ibex native runs R20 and R21 (2026-11-01) measured 472.2 s and
472.6 s; their maximum same-SPEF RustSTA/OpenSTA difference is 0.475 ps. Their
recipe inputs are not in the repository, so the 2026-11-02 host campaign above
ran the published run16-class recipe instead.

The qualification and R20/R21 results in this section are retained,
identity-bound results from the 2026-11-01 update, not rerun for this update;
only the host campaign section above is new measurement evidence, taken from the
RustEDA host receipt. GPU, Ngspice and scale tests listed as unchecked in the
2026-11-01 source QA record remain unchecked for this revision. Generated
clocks, CPPR, CCS and crosstalk remain outside the supported flow scope.

[Whole-flow evidence and measurement scope](https://lab-emi.github.io/RustEDA/benchmarks/) ·
[RustEDA engine status](https://lab-emi.github.io/RustEDA/engine-status/)

## Earlier qualification record

The following measurements describe the earlier source revisions named here.

## Integrated capabilities

| Area | Available in the integration |
| --- | --- |
| Timing | Multi-corner NLDM; setup/hold, latch borrowing, recovery/removal and supported clock-gating checks |
| Clocks | Ideal or explicitly propagated primary clocks on the supported bounded clock network |
| Parasitics | Bounded plain/gzip SPEF; effective capacitance and driver waveforms; resistor loops solved in f64 |
| Disconnected RC | Components without a driver resistor path retain capacitance, use zero wire delay and emit `T0151` |
| Queries and updates | Immutable timing tables, bounded path queries and incremental `TimingSession`; CLI update currently recomputes in full |
| Verification | Analytic cases, independent checking for supported scopes, full/incremental consistency, CPU/CUDA and dual-PDK external comparisons |

The engine baseline is
[`26bd6eb` (0.4.3)](https://github.com/lab-emi/RustSTA/commit/26bd6eb),
followed by the documented pulse-width residual and SPEF candidate-safety checks.
The [integration changelog](https://github.com/lab-emi/RustSTA/blob/codex/unified-brand/CHANGELOG.md)
records the per-version evidence and limits.

## Measured agreement and remaining differences

| Evidence | Recorded result and boundary |
| --- | --- |
| 0.4.2 extracted timing | 32 design/library/extraction cases, 96 corner comparisons; maximum endpoint difference from OpenSTA fell from 5.21 to 0.97 ps |
| 0.4.3 non-tree extraction | Previously rejected Sky130 ALU and GF180 register-file cases agree within 0.62 and 0.67 ps; 44 tree-only cases retain identical tables after version normalization |
| Independent electrical checks | Ngspice moment and transient checks exercise trees, loops and driver/wire waveforms; these validate the stated model, not complete silicon behavior |
| Minimum pulse width | Three reported slow-corner Sky130 cases retain 1.28–1.86 ps differences from OpenSTA; the residual is documented, not waived |

These are retained qualification results, not new measurements performed for
this documentation update. Resistor-loop behavior can differ from a reference
that drops edges; the independent circuit model is part of the evidence.

The independent checker returns `undecided` for extracted RC. A successful
`--method full` check establishes consistency with a full run. Generated
clocks, CPPR, CCS and crosstalk remain unsupported; the new derate and driving-cell implementation has the recorded scope above.

[Detailed RC evidence](verification/rc-waveforms.md) ·
[SPEF workflow and topology edits](guide/spef.md) ·
[Supported scope](reference/scope.md)

## Publication checks

For the 2026-11-01 publication, the isolated workspace passed 415 regular Rust
tests (19 ignored), formatting, all-target/all-feature Clippy and rustdoc.
Repository layout and shared-file integrity passed; requirement coverage was
597 requirements with zero gaps and fixture regeneration checks passed. The
standalone and unified strict documentation builds and rendered link checks
passed. External qualification was retained from the exact measured source, not
rerun by these publication checks. These checks were not rerun for the
2026-11-02 update, whose host campaign section is new measurement evidence from
the RustEDA host receipt.
