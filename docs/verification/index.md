<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Verification scope

RustSTA separates engine tests, independent checking, recomputation consistency
and external reference comparisons. Each answers a different question.

| Evidence | Existing infrastructure | Boundary |
|---|---|---|
| Analytic timing cases | Hand-derived checks for clocks, latches, constants, slews, loads and exceptions | Small synthetic inputs; not process sign-off |
| Independent checker | `ruststa-check`, separate Liberty/netlist/SDC readers and f64 timing implementation | Supported cases and explicit work budgets |
| Full recomputation | `ruststa check --method full` | Consistency with the same engine |
| Incremental consistency | `TimingSession` mutation tests vs full analysis | Rust API coverage; CLI `update` still computes the full result |
| Dual-PDK coverage | Pinned Sky130 HD, GF180 7T and 9T libraries | Exact profiles and inputs recorded in the PDK suite |
| CPU/CUDA consistency | Differential propagation tests with explicit f64/f32 contracts | CUDA requires an actual device; absent hardware is unverified |
| OpenSTA comparison | Run a pinned external executable on identical standard-format inputs | Requires matched scope and tracked outputs; not established by compilation |

## Run the registered checks

```bash
cargo xtask verify sta
cargo xtask pdk fetch
cargo xtask verify pdk --profile pr --require
```

These commands describe how to collect evidence. Their presence in the docs
does not imply that every profile, operating corner, GPU or large design has
been verified for the current checkout. Preserve dated results and environment
details with each run.

The [PDK suite](pdk.md) defines pinned inputs, profiles, mutation counts and
resource controls. Use `--require` when missing libraries must fail a gate;
an optional skipped run is unverified, never a pass for that library.

## Known checker boundaries

The independent checker has bounded path work. On large reconvergent designs
with exceptions it can reach its enumeration limit and return `undecided`.
Some large structural-netlist conversions and exception-free timing cases have
also shown checker limitations. A mismatch requires investigation; agreement
with a full rerun does not erase an independent-check mismatch.

Keep the checker separate from the engine implementation. Correct a disagreement
with independent evidence, and keep unverified outcomes visible.

## External timing comparisons

Use OpenSTA only as a black-box executable over Verilog, Liberty, SDC and any
supported parasitic inputs. Do not inspect, compare or translate its source.
Record the executable version/hash, source input hashes, corner, clock/wire
models, endpoint coverage, units and tolerance. Compare like-for-like setup/hold
checks and rise/fall transitions.

This documentation release does not publish a numerical OpenSTA speedup or a
sign-off equivalence claim. A missing reference executable is `unverified`.
For performance comparisons, report complete command wall time and peak RSS
beside timing agreement; see [measurement guidance](../guide/performance.md).

## Specifications and analytic sources

- [Timing verification requirements](../specs/sta/verification.md).
- [Requirement-to-implementation evidence](requirements.md).
- [Analytic timing derivations](../../verification/analytic/sta/README.md).
- [Analytic exception cases](../../verification/analytic/sta-exceptions/README.md).
- [Pinned PDK manifest](../../verification/pdks.toml).
