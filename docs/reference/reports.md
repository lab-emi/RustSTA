<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Reports and artifacts

RustSTA returns `rusteda.v1` JSON envelopes. Common fields identify the command,
workspace, `base_rev`, queried `rev`, committed `new_rev`, `scope`, `completeness`,
`metrics`, diagnostics and errors. Operation-specific fields add tables, paths or
verification results. Use the [protocol](../specs/cli/protocol.md) as the exact contract.

## Read scope before slack

Timing results identify their operating corner, clock and wire models,
parasitics, backend and precision. Endpoint completeness identifies constrained
and unconstrained coverage. An unchecked DRC is `drc_status: "unchecked"`.
It must not be reported as a zero-violation check. The electrical design rules
(`max_transition`, `max_capacitance`, `max_fanout`) and the pulse checks have
their own status, `drv_status` and `pulse_width_status`, with counts; a table
that did not check them reports `unchecked` with `null` counts.

```bash
ruststa report --workspace ./ws --rev rev2 --section limits --format json
ruststa report --workspace ./ws --rev rev2 --section drv --filter violating --format json
ruststa report --workspace ./ws --rev rev2 --section hold_fix --target-margin 0.05 --format json
```

```bash
ruststa report --workspace ./ws --rev rev2 --section summary --format json
ruststa report --workspace ./ws --rev rev2 --section unconstrained --format json
ruststa report --workspace ./ws --rev rev2 --section endpoints \
  --filter 'slack<0' --limit 10 --format json
```

Tables expose columns and row counts, including total/matched/shown counts.
Bounded output is a window into the result, not proof that the design has only
that many endpoints. Path handles refer to the selected revision.

## Workspace artifacts

| Artifact | Role |
|---|---|
| `manifest.json` | Current workspace head and revision allocation |
| `workspace.lock` | Shared/exclusive coordination lock |
| `history.jsonl` | Mutation history |
| `cache/*.lib`, `cache/*.rlib` | Verbatim and compiled Liberty inputs |
| `revisions/revNNNN/netlist.rbin` | Compiled structural netlist |
| `revisions/revNNNN/constraints.rbin` | Compiled SDC |
| `revisions/revNNNN/constraints/merged.sdc` | Canonical constraints used by checking |
| `revisions/revNNNN/sta.rbin` | Persisted multi-corner timing state |
| `revisions/revNNNN/sta_check.json` | Verification verdict, methods, tolerance and differences |

The current engine writes a 24-section `STA v2` timing store in the shared
`RBIN v1` container. Mapped queries open the persisted result without another
timing propagation. See [store format](../specs/sta/store.md) and
[workspace format](../specs/workspace/README.md).

Full storage supports detailed path and pin queries. Summary storage omits
detail; do not assume every query is available after `--sta-store summary`.
