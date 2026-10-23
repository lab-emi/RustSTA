<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Architecture

RustSTA separates mutable design data, contiguous compute views and persisted
timing results. CLI operations connect them through immutable disk revisions.

```text
Structural Verilog + Liberty + SDC
                 │
            rusteda import
                 │
        Immutable design revision
                 │
            ruststa engine
                 │
       CPU reference / native CUDA
                 │
              sta.rbin ─────────► mapped path / pin / report queries
                 │
           ruststa check
                 │
      independent checker + full rerun
                 │
           sta_check.json
```

| Crate | Responsibility |
|---|---|
| `rusteda-core` | Typed design IDs, central netlist storage, Liberty model, diagnostics and job context |
| `rusteda-workspace` | Disk workspace, locks, revisions, binary containers and shared readers |
| `rusteda-formats` | Verilog, Liberty and bounded SDC parsing |
| `rusteda-compute` | CPU reference and optional native CUDA timing propagation |
| `rusteda-cli` | Generic `rusteda` workspace commands |
| `ruststa` | Timing graph construction, NLDM delay calculation, propagation, result storage and `TimingSession` |
| `ruststa-check` | Independent workspace timing checker |
| `ruststa-cli` | `ruststa` command-line operations and mapped queries |
| `ruststa-fixtures` | Synthetic cell libraries, netlists and test generators |
| `xtask` | Repository checks, requirement coverage, fixture and verification tooling |

Core crates have no Python or GUI runtime dependency. String interning belongs
to a design instance. Compute uses separate levelized views and deterministic
merges. Searches and path extraction need explicit work/memory budgets, including
on small reconvergent graphs.

The checker does not depend on the engine or shared SDC compiler. Standard
artifacts form the boundary to reference executables, not their source code.

Read [timing conventions](../adr/0004-timing-conventions.md),
[binary store design](../adr/0007-sta-store.md),
[CLI/workspace decisions](../adr/0008-cli-on-workspace.md), and
[compute decisions](../specs/compute/decisions.md) for the rationale.
