# Development status

Updated **2026-11-01**. This page describes the source published with the
RustEDA development update. Use the superproject’s exact gitlinks and the
repository lockfile when reproducing a run.

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
The two complete Ibex native runs measured 472.2 s and 472.6 s; their maximum
same-SPEF RustSTA/OpenSTA difference is 0.476 ps.

These are retained identity-bound results, not a new campaign run during the
publication. GPU, Ngspice and scale tests listed as unchecked in that source QA
record remain unchecked for this revision. Generated clocks, CPPR, CCS and
crosstalk remain outside the supported flow scope.

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

The isolated workspace passes 415 regular Rust tests (19 ignored), formatting,
all-target/all-feature Clippy and rustdoc. Repository layout and shared-file
integrity pass; requirement coverage is 596 requirements with zero gaps and
fixture regeneration checks pass. The standalone and unified strict documentation
builds and rendered link checks pass. External qualification above is retained
from the exact measured source, not rerun by these publication checks.
