# RustSTA verification

**STA-REQ-0500** Independent checker (`ruststa-check`, command `ruststa check sta` and workspace
checker [`ruststa_check::rev`](../../../crates/verify/ruststa-check/src/rev/mod.rs)): it reads the
same netlist, libraries and constraints, recomputes arrival times by a demand-driven evaluation
(memoized recursion from each endpoint over its fanin cone, in `f64`), with its own table
interpolation, constant propagation, clock-network analysis, timing exception matching and check
computation, and compares every endpoint's setup, hold, recovery, removal and clock-gating slack,
arrival, required time and status with the RustSTA report within a tolerance: `1e-9 ns` on
closed-form synthetic libraries (`wt_*`), by default `1e-6 ns` for a report of the `cpu` backend
(including fetched PDK libraries) and `1e-3 ns` for a GPU backend (`f32` values, STA-REQ-0403;
enough for arrivals up to about 40 ns, larger designs need an explicit `--tolerance`). Its verdict
is `agree`, `disagree` (with deterministic `mm<N>` mismatch handles) or `undecided`. It does not
depend on the engine crate.

**STA-REQ-0501** Known-answer cases (`verification/analytic/sta/` and
`verification/analytic/sta-exceptions/`): circuits whose arrivals, slews, required times and slacks
follow from hand derivations with tables that are linear in their variables (so bilinear
interpolation and linear extrapolation are exact), including inverter chains, reconvergent fanout
with different depths, non-unate arcs, negative-edge flip-flops, latches with time borrowing and
`D -> Q` relaunch, input and output delays, two clocks with a 3:2 period ratio, lumped wire-load
models, three-state buses, constant propagation and case analysis, and Phase 2a timing exceptions
(`set_false_path`, `set_multicycle_path`, `set_max_delay`, `set_min_delay`, `set_clock_groups`).

**STA-REQ-0502** Exhaustive path enumeration: on generated circuits of at most 12 cells (and on
acyclic DAGs up to 5,000 nodes in the workspace checker) the checker enumerates every path from
every startpoint to every endpoint and confirms that the worst path arrival and slack equal the
propagated endpoint arrival and slack within `1e-9 ns`.

**STA-REQ-0503** Metamorphic properties and timing invariants
([`crates/engines/ruststa/tests/invariants.rs`](../../../crates/engines/ruststa/tests/invariants.rs)
and [`crates/apps/ruststa-cli/tests/checker_agreement.rs`](../../../crates/apps/ruststa-cli/tests/checker_agreement.rs)):
increasing a clock period by `d` increases every setup slack of paths launched and captured by that
clock on the same edges by `k * d` (`k >= 1`) and leaves hold slacks unchanged; adding `d` to an
input delay shifts downstream setup slacks by `-d` and hold slacks by `+d`; adding `d` to an output
delay shifts output port setup slacks by `-d` and hold slacks by `-d`; tightening a library Lut2D by
`alpha > 1` never increases setup slack; renaming instances and permuting netlist statements changes
nothing; `--threads 1` and `--threads 8` (and 32) produce byte-identical `sta.rbin` stores and
bit-identical arrivals and slacks; the CPU and GPU backends agree per STA-REQ-0403.

**STA-REQ-0504** Planted defects: the checker must report a difference (`disagree` with `mm<N>`
mismatch records) when a report's slack or WNS is altered, when a cell's table is scaled in the
checker's input but not the engine's, and when a timing arc is removed.

**STA-REQ-0505** Unit and integration tests cover every requirement of this specification that is
observable at the crate API, and cite the requirement IDs. `cargo xtask coverage` writes the matrix
of the requirements of every specification and the Rust sources and tests that cite them to
[`docs/verification/requirements.md`](../../verification/requirements.md), and fails when a
requirement has no citing file, has a remaining `(planned: ...)` marker for an integrated package,
or is defined twice.

**STA-REQ-0506** Checker budgets (AGENTS rule 9.2). The checker's matching of timing exceptions
enumerates at most 200,000 paths; a design that needs more is not checked against its exceptions:
the verdict is `undecided` with the reason, never an exhausted memory.

**STA-REQ-0510** `ruststa check sta` prints on standard output one line per difference (at most
`--limit`, default 20, 0 = all), then `… N more differences (--limit 0 prints all)` when lines were
omitted, then the verdict line `verdict: <verdict> (<n> endpoint slacks compared, largest
difference <d> ns, tolerance <t> ns)`. Differences are `difference[<kind>]: <pin> checker <a> ns,
report <b> ns, difference <d> ns`, with `—` for an absent slack. Exit status 0 `agree`, 3
`disagree`, 1 `error`.

**STA-REQ-0511** `check sta` accepts `--from-run <RUN>` of a timing run (PROTO-REQ-0061); Liberty
errors are `C0104`, report write errors `C0105`.

**STA-REQ-0512** The checker's JSON report contains `format` (`ruststa-check-sta-report`),
`format_version` (1) and the `tolerance` used.

## Verification suites and dual-PDK matrix

- `cargo xtask verify [sta|sdc|compute|all] [--gpu]` loads all
  `verification/suites/<domain>/*.toml` files in deterministic sorted path order and runs every
  registered verification suite.
- `cargo xtask verify pdk [--profile pr|full] [--require] [--gpu]` runs the dual-PDK verification
  matrix across `sky130_fd_sc_hd` (`tt_025C_1v80`, `ss_100C_1v60`, `ff_n40C_1v95`) and
  `gf180mcu_fd_sc_mcu{7t,9t}5v0` (`tt_025C_5v00`, `ss_125C_4v50`, `ff_n40C_5v50`); see
  [`docs/verification/pdk.md`](../../verification/pdk.md) for the fetch protocol, profiles, and lab
  GPU verification procedure.
