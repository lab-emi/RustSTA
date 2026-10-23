<div class="ruststa-hero" markdown>

<p class="ruststa-eyebrow">RUSTEDA / STATIC TIMING ANALYSIS</p>

# Every path. A clear answer.

Turn gate-level Verilog, Liberty and SDC into timing results you can inspect,
check and automate. Written in Rust. Built for terminals, CI and coding agents.

[Run your first timing analysis](guide/quickstart.md){ .md-button .md-button--primary }
[Build an agent workflow](guide/agents.md){ .md-button }

<p class="ruststa-release">Source preview · 0.1.0 · Apache-2.0 · CPU reference + optional CUDA</p>

</div>

<figure class="ruststa-banner">
  <picture>
    <source media="(prefers-reduced-motion: no-preference)" srcset="assets/ruststa-banner.gif" type="image/gif">
    <img src="assets/ruststa-banner.svg" alt="Netlist, Liberty and SDC inputs feed a timing graph. A highlighted critical path connects its launch and capture registers to a path report." width="1200" height="400" data-animation="assets/ruststa-banner.gif" data-poster="assets/ruststa-banner.svg">
  </picture>
  <figcaption>Netlist → timing graph → critical path. Illustration. <button type="button" class="ruststa-motion-control" aria-pressed="false" hidden>Pause animation</button></figcaption>
</figure>

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
    RustSTA is a source preview. The current engine uses ideal clocks and ideal
    or lumped wire-load models. Generated clocks, propagated clocks, CPPR and SPEF
    annotation are outside the implemented scope. Check the
    [support table](reference/scope.md) before interpreting a result.

Part of the [RustEDA tool family](https://github.com/lab-emi).
Use [RustSys](https://lab-emi.github.io/RustSys/) to synthesize RTL to standard cells,
then analyze the exported structural netlist with RustSTA.
