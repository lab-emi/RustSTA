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
their own status, `max_transition_status`, `max_capacitance_status`,
`max_fanout_status`, `drv_status` and `pulse_width_status`, with counts. A status
is `met` only when every check was evaluated without violation: `partial` when
some checks were not evaluated (unclocked pulse pins, pulse pins left out by
`--pulse-pins clock-ends`, values that are not finite, pins without any transition
`completeness.drv_untimed_pins`, pins of an ideal clock network
`completeness.drv_ideal_clock_pins`), when a design rule is
checked on some pins but others have no limit (`completeness.drv_unlimited`), or
when a design rule has no limit anywhere; `unconstrained` when there is nothing
to check, and `unchecked` when nothing was evaluated or the table did not check
them. A count without a checked row is `null`, never zero. The rules of the
pulse checks are part of the scope (`scope.pulse_rules`: `groups` `worst` or
`last`, `pins` `all` or `clock-ends`), and so is the clock limit scope of the
design rules (`scope.drv_rules.clock_limits`: `paths` or `domain`; the clock network pins
on which the two differ are counted in `completeness.drv_clock_scope_pins`). The drivers
of constant nets, which are not checked for capacitance and fanout, and the pins
checked with a static transition are counted in `completeness.drv_constant_drivers`
and `completeness.drv_static_transition_pins`; unconnected instance output pins,
which drive no net, are not checked and are counted in
`completeness.drv_unconnected_output_pins`.

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
that many endpoints. The limit row sections also give `stored` and `truncated`
rows: their `total` counts every checked row, while only the kept rows can match
a filter. Path handles refer to the selected revision. `ruststa path --to NAME`
gives the worst path of the endpoint over all corners, or in `--corner`, and
warns `T0138` when a requested check has no path. `inspect --pin` of an endpoint
reports the required times of that worst path with the endpoint slacks; the
`pins` section reports node values. `report --section endpoints --kind hold`
orders the endpoints by hold slack unless `--sort` is given.

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
