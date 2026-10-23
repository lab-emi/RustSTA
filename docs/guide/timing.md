<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Analyze a design

RustSTA starts from a gate-level netlist. Synthesize RTL with a tool such as
[RustSys](https://lab-emi.github.io/RustSys/), export structural Verilog, and use
the Liberty library matching the mapped cells. Supply SDC constraints for the
clocks and I/O timing of the design.

## Import a design revision

```bash
rusteda init --workspace ./ws
rusteda import --workspace ./ws --base-rev rev0 \
  --lib ./cells_tt.lib --corner tt \
  --lib ./cells_ss.lib --corner ss \
  --netlist ./gates.v --sdc ./design.sdc --format json
ruststa run --workspace ./ws --base-rev rev1 \
  --threads 4 --max-paths 20 --max-state-gib 4 --format json
```

Repeat `--lib` and place `--corner` after the corresponding library. Without a
corner selection on `ruststa run`, the engine analyzes all imported corners.
Use repeatable `--corner NAME` flags to select a subset.

The import compiles SDC with a bounded Tcl subset. Pass permitted environment
inputs explicitly as `--sdc-env NAME=VALUE`. Unsupported constraints are diagnosed;
see [supported scope](../reference/scope.md).

## Find the endpoints that need attention

```bash
ruststa report --workspace ./ws --rev rev2 --section summary --format json
ruststa report --workspace ./ws --rev rev2 --section endpoints \
  --filter 'slack<0' --sort slack --limit 20 --format json
ruststa report --workspace ./ws --rev rev2 --section unconstrained --format json
ruststa path --workspace ./ws --rev rev2 --target path1 --format json
```

Use the returned revision and handles together. Constrain the endpoints you care
about before treating a clean report as useful coverage. Ideal-clock and wire
model assumptions belong beside any quoted timing number.

## Update an imported revision

Import a changed netlist or constraints against the current explicit revision,
then run `ruststa update` on the new revision. `update` reports the change class
and `incremental.computation: "full"`: the CLI currently recomputes timing. The
separate Rust `TimingSession` API has incremental cone invalidation; it is not a
promise of incremental CLI speed.

Use `rusteda log`, `show` and `diff` to inspect the revision chain. A conflicting
writer produces `REVISION_CONFLICT` with exit status 4; reload the workspace state
and decide whether the proposed mutation still applies.

See the [complete timing manual](../user/sta/README.md) for latches, recovery,
removal, constants, exceptions and multi-corner conventions.
