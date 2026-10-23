# RustSTA roadmap

RustSTA is the static timing analysis tool of RustEDA, a suite of electronic design automation
tools written in Rust. The tools share their core crates and a disk workspace: every command is a
deterministic process that reads one revision of a workspace directory and may commit a new one,
and text inputs are parsed once, at import, into binary tables that later commands map into
memory. The suite is built in phases 0 to 5. For each phase, this roadmap gives the deliverables,
the acceptance gate, RustSTA's part and its status. A tool gets crates only when its phase starts;
until then it is described here.

The engineering rules that every phase follows are in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Status

| Phase | Scope | RustSTA's part | Status (October 2026) |
|---|---|---|---|
| [0](#phase-0-workspace-foundation) | Workspace foundation | The workspace crate with its table codecs, the `rusteda` command and the extensions of the Liberty model | In progress |
| [1](#phase-1-logic-synthesis) | Logic synthesis | None: the synthesis tool of the suite builds this phase in its own repository | In progress there |
| [2](#phase-2-static-timing-analysis) | Static timing analysis | The whole phase | 2a in progress; 2b follows 2a |
| [3](#phase-3-the-first-closed-loop) | The first closed loop of synthesis and timing | Slack queries for the optimizer, a shared reader of timing paths, session export | Planned |
| [4](#phase-4-native-gpu-acceleration) | Native GPU acceleration and a layout viewport prototype | Batching of corners and Monte Carlo scenarios in the timing task | Roadmap |
| [5](#phase-5-and-later-further-tools) | Placement, routing, design-rule checking, circuit simulation | None | Roadmap |

Phases follow their dependencies, not a calendar. A phase is divided into work items with short
ids (E5, K1, ...). A requirement that a work item still has to implement carries the marker
`(planned: <id>)` of that work item in its specification until the work item is accepted. The
status column changes when a phase, or a part of it, changes state.

## Default libraries

Phases 0 to 3 are verified on the standard cells of two open process design kits (PDKs) at once,
so that no feature is fitted to one library's naming or table conventions:

| Library | Corners | What it exercises |
|---|---|---|
| `sky130_fd_sc_hd` | `tt_025C_1v80`, `ss_100C_1v60`, `ff_n40C_1v95` | Multi-input complex gates (`a21oi`, `maj3`), detailed pin attributes, the delay and transition tables of the non-linear delay model |
| `gf180mcu_fd_sc_mcu7t5v0`, `gf180mcu_fd_sc_mcu9t5v0` | `tt_025C_3v30` | Other table index conventions, drive-strength suffixes (`_1`, `_2`, `_4`), sequential and clock-gating cells |

The libraries are fetched at pinned versions, with their SHA-256 sums and sizes recorded, into the
directory that `RUSTEDA_PDK_DIR` names (default `target/pdk`). They are never committed: tracked
test inputs are synthetic.

## Phase 0: workspace foundation

Deliverables:

- `rusteda-core`: a symbol interner per netlist database, generational typed ids (`InstId`,
  `NetId`, `PinId`, `CellId`), the gate-level netlist (`GateNetlist`, frozen into the dense
  `FrozenNetlist` view), coded diagnostics and the Liberty library model.
- `rusteda-workspace`: workspace creation, atomic updates of `manifest.json`, shared and exclusive
  locks on `workspace.lock`, the `history.jsonl` log, immutable revisions, the `.rbin` table
  container read through memory maps, and the codecs of the netlist and compiled-library tables.
- `rusteda-formats`: the Liberty reader, whose libraries are compiled into `cache/*.rlib`, and the
  structural Verilog reader and writer.
- `rusteda`, the command of the shared workspace: `init`, `import`, `log`, `show`, `diff`,
  `revert` and `export`.

Acceptance gate: a structural Verilog netlist of 500k gates and the `sky130_fd_sc_hd` and
`gf180mcu_fd_sc_mcu7t5v0` libraries import into a workspace without reader warnings on standard
cells, and later read queries start cold, through memory maps, in under 10 ms.

RustSTA builds `rusteda-workspace` with its codecs, the `rusteda` command, and the extensions of
the Liberty model and reader that both tools need. The synthesis tool builds the gate-level
netlist model and a streaming structural Verilog reader, which the shared crates adopt. Each tool
reviews the other's shared work before adopting it.

Status: the shared crates are seeded from RustSTA's readers, Liberty model and GPU device layer,
and the repository follows the RustEDA layout. The work items below complete the phase; the gate
is measured once the netlist import and the classification of every Liberty construct of the
standard cells are in place.

| Id | Work item |
|---|---|
| W1C | `rusteda-workspace` in this repository, and the core of the `rusteda` command |
| W1L | The LIBERTY v1 table (`.rlib`) and the compiled-library cache of `rusteda import --lib` |
| W1N | The NETLIST v1 table over the frozen netlist |
| W1I | `rusteda import --netlist` and `rusteda export --netlist` |
| A-W2 | Adoption of the gate-level netlist model: symbols, arenas, `GateNetlist`, `FrozenNetlist` |
| A-VLOG | Adoption of the streaming structural Verilog reader and writer |
| LIB-AD | Liberty extensions: typed timing arcs, templates with up to three axes, units, limits, three-state functions, output-pin capacitance, power-intent attributes, and diagnostics aggregated per construct |
| LIB-C | Liberty extensions: conditional arcs, state tables, clock-gating pin roles, slew thresholds, bus expansion, and no warnings on the standard cells of the default libraries |
| LIB-B | Liberty extensions: wire-load models and their selection, operating conditions |

## Phase 1: logic synthesis

The synthesis tool of the suite builds this phase in its own repository: a reader and elaborator
for a synthesizable Verilog and SystemVerilog subset, an and-inverter-graph optimizer, technology
mapping onto the compiled libraries of both default PDKs, and a built-in combinational
equivalence checker.

Acceptance gate: the EPFL and IWLS benchmark suites synthesize to both default PDKs inside a
workspace, every synthesized netlist passes the formal equivalence check, and area and estimated
delay stay within tracked targets.

RustSTA has no work item in this phase. The shared crates it builds, among them the Liberty
extensions of Phase 0, serve the synthesis tool as well.

## Phase 2: static timing analysis

Deliverables:

1. An SDC reader driven by command schemas, with a bounded, deterministic Tcl subset evaluated at
   import into the constraints table (CONSTR v1): clocks, input and output delays, loads and
   transitions, false paths, multicycle paths, minimum and maximum delays, case analysis and the
   other commands of the subset. An unsupported command, option or construct is an error or a
   diagnostic, never silently ignored.
2. A timing graph over the frozen netlist; delay and transition interpolation in the tables of the
   non-linear delay model; wire-load estimation and a hook for RC models and parasitics.
3. Levelized forward propagation of arrival and transition times on the timing task of
   `rusteda-compute` (CPU in `f64`; CUDA), backward propagation of required times, setup and hold
   slack of every pin and endpoint, timing exceptions, and common path pessimism removal (CPPR).
4. Analysis of every corner of a revision: the graph and the constraints are shared, while
   libraries, loads and timing state are kept per corner.
5. Incremental timing: propagation through the changed cone after local gate sizing, buffer
   insertion or other netlist edits, and `ruststa update` from one revision to the next.
6. The `ruststa` command on the workspace: `run`, `update`, `check`, `path`, `inspect` and
   `report`. Results are stored in the timing table `sta.rbin` (STA v1), which queries map into
   memory: on a design of 1M cells, a path query starts cold in under 5 ms and other reads in
   under 10 ms, on local storage.
7. The independent checker on the workspace: it recomputes the timing of a revision from the same
   inputs with its own code, and `ruststa check` commits its verdict as the `sta_check` table.

Acceptance gate:

- Timing values: on designs synthesized for both default PDKs, the arrival, transition, required
  and slack values of the full timing graph are measured locally against established practice and
  reported to the lab. These measurements are not part of the repository.
- Incremental invariant: after 1,000 randomized local gate-sizing and buffering mutations on each
  default PDK, incremental timing equals a full run every time, bit for bit: values, predecessors,
  endpoint order, handles and reported paths.
- The tracked suites pass: known-answer cases derived by hand; the agreement of the engine and the
  checker on every pin; invariance under thread count, statement order and renaming; the
  consistency of the CUDA backend with the CPU backend; and the dual-PDK suite
  (`cargo xtask verify pdk`), in its pull-request profile on hosted CI and in its full profile,
  with designs of 500k and 1M cells, on the lab machine.

### Phase 2a and 2b

Phase 2 is delivered in two parts:

- **Phase 2a** meets the acceptance gate with ideal clocks on designs synthesized for the default
  PDKs, and delivers everything the gate needs: the SDC reader, the timing graph, delay
  calculation with the lumped wire-load model, propagation on the timing task, checks, required
  times and paths, constants and case analysis, exceptions with `-from` and `-to`, several
  corners, the timing table, the incremental update, the checker and the command. With ideal
  clocks CPPR has no effect, so 2a needs none. The SDC commands of 2b are rejected at import with
  a "not supported yet" error.
- **Phase 2b** follows 2a: propagated and generated clocks, derates, CPPR, exceptions with
  `-through`, the tree wire-load model, SPEF parasitics, and local measurements on the
  timing-contest benchmarks (TAU, ISPD), which need SPEF, propagated clocks with CPPR and pairs of
  early and late libraries. These benchmarks state no license, so they are used only locally.

Phase 2a also corrects rules of the current engine that local measurements against established
practice exposed: the hold relation between clocks of different periods, the clock latency of
input delays, transitions kept per launch clock instead of one worst transition per pin, the
pairing of latch edges, input and output delays given for one bound only, three-state arcs that
are not propagated, constant nets, the output-pin capacitance of drivers, and negative transitions
from table extrapolation. A changed rule gets a new requirement id; the superseded text stays in
`docs/specs/sta/legacy.md` as long as the legacy engine exists.

The new engine is built beside the current one, in `crates/engines/ruststa/src/engine/`, and the
new checker in `crates/verify/ruststa-check/src/rev/`. The current engine, checker and file-based
commands keep their paths until Z1 removes them at the end of 2a.

Status: the current command, `ruststa sta`, analyzes a structural Verilog netlist, Liberty
libraries and an SDC file read from disk, in one corner with ideal clocks and ideal wires: setup,
hold, recovery, removal and clock-gating checks and latch time borrowing, with propagation on the
CPU in `f64` or on a CUDA GPU in `f32`. `ruststa check sta` recomputes every endpoint slack
independently. Wire load, exceptions, per-pin required times, several corners, the workspace and
incremental timing are missing. Phase 2a has started with the specifications, the engine
scaffolds and the timing task of `rusteda-compute`.

Work items of Phase 2a, which build on those of Phase 0:

| Id | Work item |
|---|---|
| T1 | Timing semantics of 2a: rules with new requirement ids and their hand derivations |
| T2 | Exception semantics: rules and hand derivations |
| E0 | Engine and checker namespaces, seam scaffolds with "not supported yet" stubs, private copies of the legacy SDC lexer |
| E1 | Netlist view and timing graph: drivers and loads, cycle breaking, levels, the canonical topological id |
| E2 | Constraint compiler: object queries resolved against the netlist, units per corner |
| E3 | Library view and delay calculation: typed arcs, libraries per corner, table lookup with up to three axes, net loads |
| E4 | Lumped wire-load estimation and the RC hook |
| E5 | Propagation setup, clock relations, timing checks, endpoint status, several corners |
| E6 | Required times, slack per pin, worst paths and their handles |
| E7 | Constants, case analysis and disabled arcs |
| E8 | Timing exceptions with `-from` and `-to` |
| E10 | The timing table `sta.rbin` (STA v1) and mapped queries |
| E11 | Incremental timing: timing sessions, the update of a revision, the incremental invariant |
| S1 | SDC layer: schema-driven reader, bounded Tcl subset, the CONSTR v1 table and `rusteda import --sdc` (shared) |
| C1 | Timing task of `rusteda-compute`: the CPU backend in `f64` and the CUDA backend, `f64` by default and `f32` on request (shared) |
| C2 | Lowering of the engine onto the timing task; removal of the legacy GPU code |
| R1 | Independent checker on the workspace |
| R2 | Exceptions in the checker |
| K1 | The `ruststa` command on the workspace |
| V1 | Fixtures, design generators, suite registry and PDK fetch |
| V2 | Integration suites, the dual-PDK suite and CI |
| D0 | Roadmap and engineering rules |
| D1 | README, user guide and changelog |
| P1 | Local measurement setup; it is not tracked, and its results are reported to the lab |
| Z1 | Removal of the legacy engine, checker and file-based commands |

The longest chain of dependencies in 2a is E0, E1, E2, E5, E6, E10, K1, V2, Z1.

Work items of Phase 2b:

| Id | Work item |
|---|---|
| T3 | Clock-network semantics: propagated and generated clocks, latency, derates, CPPR, minimum pulse width |
| S2 | SDC commands of 2b: propagated and generated clocks, derates, `-through`, `-reference_pin`, minimum pulse width, ideal networks |
| S3 | SPEF reader, the parasitics table and `rusteda import --spef` (shared) |
| E12 | Constraint compiler for 2b |
| E9 | Propagated clocks, derates, generated clocks and CPPR |
| E13 | Exceptions with `-through` |
| E14 | Tree wire-load model and the SPEF wire model |
| R3 | Checker for 2b |
| P1b | Local measurements on the timing-contest benchmarks; not tracked |

## Phase 3: the first closed loop

Deliverables:

- Timing-driven optimization after technology mapping in the synthesis tool (gate sizing, buffer
  insertion, pin swapping, logic cloning), guided by incremental slack queries to RustSTA.
- The agent workflow across `rusteda`, the synthesis tool and `ruststa`: optimistic concurrency
  through `--base-rev`, JSON output in the `rusteda.v1` envelope, and
  `rusteda session export-script`.
- `rusteda-studio` 0.1, a GUI workbench: a workspace watcher, a netlist hierarchy browser, a table
  of critical paths, a logic-cone viewer, cross-probing to the RTL, and selections shared through
  `ui_state.json`.

Acceptance gate, the closed-loop milestone:

1. Synthesize an RTL design for `sky130_fd_sc_hd` and for `gf180mcu` (rev1).
2. Analyze its timing with RustSTA (rev2) and show the violating paths in the workbench.
3. Select a violating path in the workbench (`sel1`, pinned to rev2).
4. Run an agent optimization on `sel1` with `--base-rev rev2`, producing rev3.
5. Incremental timing equals full timing, the netlists are proven equivalent,
   `rusteda diff --from rev2 --to rev3` shows the change, the workbench updates live, and the
   workspace rolls back cleanly to rev2.

RustSTA's part, planned:

| Id | Work item |
|---|---|
| X1 | A shared reader of the path sections of STA v1, so that the synthesis tool and the workbench resolve `path<N>` handles without depending on `ruststa` (shared) |
| X2 | A slack-query interface on timing sessions for the optimizer of the synthesis tool |
| X3 | `rusteda session export-script`: the commands of `history.jsonl` as a replayable POSIX shell script (shared) |

The two tools never depend on each other directly. For the optimization step of the gate, the
loop runs `ruststa update` once per committed revision, which needs no new coupling. How the
optimizer queries slack within one process is decided together with the synthesis tool before
Phase 3 starts.

## Phase 4: native GPU acceleration

Deliverables:

- CUDA (Linux) and Metal (macOS) backends in `rusteda-compute` for batched timing propagation
  (several corners, Monte Carlo scenarios, sweeps over large netlists) and for parallel pattern
  simulation in synthesis.
- A time-boxed prototype of a high-density layout viewport in the workbench on macOS and Linux:
  five million instanced rectangles with pan, zoom, picking, high-DPI scaling and docking, with no
  frame read back from the GPU. The workbench is a client of the workspace, so its rendering can
  change without touching the core crates or the commands.

Acceptance gate:

- The CPU, CUDA and Metal backends agree within the consistency bounds that the compute
  specification ([`docs/specs/compute/`](specs/compute/README.md)) states.
- The end-to-end wall-clock speedup, including the lowering into compute buffers and host-device
  synchronization, is measured and reported on large benchmarks.

Status: a roadmap item, not planned beyond provisions in the interface of the timing task.
Phase 2a gives the timing task a CUDA backend that computes in `f64` by default, bit-identical to
the CPU backend, and in `f32` on request. The task's interface is designed to take several
corners, so batching corners needs no new interface; Monte Carlo scenarios would add per-scenario
scale factors for the arcs. Local measurements show no end-to-end gain from the GPU for a single
corner: the gains this phase aims at come from keeping state on the device and from batching
corners and scenarios. Metal can be neither built nor tested on the lab's Linux machines.

## Phase 5 and later: further tools

Each tool gets its crates when its phase starts.

- Placement (RustPlace): GPU-accelerated analytical placement with weighted-average wirelength in
  `f32`, FFT-based electrostatic density and deterministic accumulation in `i64` fixed point;
  legalization and detailed placement on the CPU in integer database units.
- Routing (RustRoute): global routing by congestion negotiation and track-based detailed routing.
- Design-rule checking (RustDRC): a spatial index and boolean geometry in integer database units.
- Circuit simulation (RustSpice): transistor-level simulation with sparse direct and iterative
  solvers in `f64` and batched device evaluation on the GPU.
