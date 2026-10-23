# RustSTA specification

RustSTA (crate `ruststa`, command `ruststa`) computes static timing of a gate-level netlist across
one or more operating corners: signal arrival and transition times, required times, setup, hold,
recovery, removal, latch and clock-gating slacks of every timing endpoint, and the worst paths.

| Topic | File |
|---|---|
| Inputs: netlist, cell libraries, corners, constraints | [inputs.md](inputs.md) |
| Timing graph, slews, wire load and delay calculation | [delay.md](delay.md) |
| Constants, case analysis and disabled arcs | [constants.md](constants.md) |
| Clocks, relations, timing checks, latches and slack | [checks.md](checks.md) |
| Command line and reports | [cli.md](cli.md) |
| Binary timing result store (`sta.rbin`) and mapped queries | [store.md](store.md) |
| Incremental timing (`TimingSession` and `update_revision`) | [incremental.md](incremental.md) |
| Propagation lowering and backends | [lowering.md](lowering.md) |
| Verification | [verification.md](verification.md) |
| Superseded Phase 1 requirements (legacy engine, removed by Z1) | [legacy.md](legacy.md) |

Requirement IDs have the form `STA-REQ-nnnn`. Requirements whose Phase 2 implementation is in a
downstream work package carry `(planned: <WP>)` until that package lands. Background: the static
timing model follows the block-oriented formulation of [@hitchcock1982timing], with the non-linear
delay model and timing checks of the Liberty format [@liberty2017] and the constraint language of
[@sdc2016]. Textbook treatment: [@bhasker2009static; @sapatnekar2004timing]. Latch timing follows
[@sakallah1990checktc]. The Phase 2a timing conventions are recorded in
[ADR 0004](../../adr/0004-timing-conventions.md) and derived by hand in
[`verification/analytic/sta`](../../../verification/analytic/sta/README.md).

## Scope of Phase 2a

- Multi-corner static timing on the zero-copy workspace netlist (`FrozenNetlist`), compiled
  Liberty models (`.rlib`), and compiled SDC constraints (`CONSTR v1`).
- Per-pin worst transition times (early and late, rise and fall) over all enabled fan-in arcs,
  including untagged sources; per-tag arrival and required times.
- Ideal wires or lumped wire-load models (`WireloadLumped`) with top-level port net exclusion and
  driver output-pin capacitance.
- Three-valued constant propagation (`0`, `1`, `X`), `set_case_analysis`, `set_logic_zero`,
  `set_logic_one`, `set_disable_timing`, and unate sense refinement.
- Flip-flops, latches (with opening-edge uncertainty, `set_max_time_borrow`, and `D -> Q`
  re-launch in the opening-edge tag), three-state buses (`three_state_enable`/`disable`),
  asynchronous recovery/removal checks (`async_default`), AND/OR clock-gating checks, and timing
  exceptions (`-from`/`-to`, multicycle, `set_max_delay`/`set_min_delay`, `set_clock_groups`).
- Phase 2b additions (propagated and generated clocks, timing derates, CPPR, `-through` exceptions,
  balanced-tree RC wire loads and SPEF parasitics) sit behind explicit engine traits.
