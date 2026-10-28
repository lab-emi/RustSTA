# Development status

Updated **2026-10-28** for the RustEDA integration branch,
[`codex/unified-brand`](https://github.com/lab-emi/RustSTA/tree/codex/unified-brand).
Its source preview is **0.4.3**. The standalone `main` branch can lag this
snapshot; use RustEDA's recursive checkout to reproduce its exact module pin.

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
clocks, CPPR, CCS and crosstalk remain unsupported; the integrated source also
rejects `set_timing_derate` and extracted `set_driving_cell` rather than claiming
the experimental implementation is present.

[Detailed RC evidence](https://lab-emi.github.io/RustEDA/modules/ruststa/verification/rc-waveforms/) ·
[SPEF workflow and topology edits](https://lab-emi.github.io/RustEDA/modules/ruststa/guide/spef/) ·
[Supported scope](https://lab-emi.github.io/RustEDA/modules/ruststa/reference/scope/)

## Active IHP/SRAM development

A separate checkout is developing mixed standard-cell/SRAM library conventions,
global early/late timing derates and driving-cell waveforms on extracted and
propagated-clock paths. This work is **not part of the current integration pin**.
Its component checks do not establish complete IHP Ibex physical closure.

[RustEDA integration progress](https://lab-emi.github.io/RustEDA/engine-status/)
