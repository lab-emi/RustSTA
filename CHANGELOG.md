# Changelog

All notable changes to this project are documented in this file.
The format follows the "Keep a Changelog" convention; versions follow semantic versioning.

## [Unreleased]

### Added
- **RustEDA workspace and generic CLI (`rusteda-workspace`, `rusteda-cli`):**
  - Immutable, content-addressed disk workspace (`manifest.json`, `workspace.lock`, `history.jsonl`,
    `cache/`, `revisions/rev<NNNN>/meta.json`) with advisory shared/exclusive locking, atomic
    staging commits, carry-forward table hardlinking, and `metric_deltas`.
  - Little-endian, 8-byte-aligned `RBIN v1` binary container with `xxh3-64` content hashing and
    read-only `memmap2` views (`NETLIST v1`, `LIBERTY v1`, `CONSTR v1`, `STA v1`).
  - `rusteda` command-line driver: `init`, `import` (`--lib`, `--corner`, `--rtl`, `--top`, `-D`,
    `-I`, `-P`, `--no-predefined-macros`, `--netlist`, `--sdc`, `--sdc-env`, `-m`), `log`, `show`,
    `diff`, `revert`, `export --netlist`, and `completions`, emitting deterministic text or
    `rusteda.v1` JSON envelopes.
- **Core netlist and Liberty model extensions (`rusteda-core`):**
  - `GateNetlist` with generational arenas (`InstId`, `NetId`, `PinId`, `CellId`), `SymbolInterner`,
    in-place cell swap, buffer insertion, net split, and zero-copy `FrozenNetlist` (`NETLIST v1`)
    with `A8a` escaped identifier rules (`\/` and `\\`, printable ASCII `33..=126`).
  - Liberty extensions (`RS-LIB.A`, `RS-LIB.B`, `RS-LIB.C`, `RS-LIB.D`): typed `TimingType` enum
    (including `three_state_enable`/`disable`, `non_seq_setup`/`hold`, `min_pulse_width`,
    `minimum_period`, `skew_*`, `nochange_*`, `clear`, `preset`), 1D/2D/3D lookup-table templates
    with per-table index overrides, `Table::mid_slew`, output-pin capacitance, rise/fall capacitance
    ranges, `three_state` functions, `when` Boolean conditions (`Func`), `statetable` and
    `state_function`, `clock_gating_integrated_cell` and `clock_gate_*` pin roles, slew threshold
    and derate attributes, `wire_load`, `wire_load_table`, `wire_load_selection`, and
    `operating_conditions` (`tree_type`), plus power/isolation/level-shifter/retention/PG-pin
    attributes.
- **Format readers and SDC compiler (`rusteda-formats`):**
  - Bounded, deterministic Tcl interpreter and SDC compiler (`rusteda_formats::sdc`) supporting
    clocks, latencies, uncertainties (`-from`/`-to`), transitions, I/O delays (`-add_delay`,
    `-network_latency_included`, `-source_latency_included`), `set_driving_cell`, `set_load`,
    `set_wire_load_model`, `set_wire_load_mode`, `set_case_analysis`, `set_logic_zero`,
    `set_logic_one`, `set_disable_timing`, `set_max_transition`, `set_max_capacitance`,
    `set_max_fanout`, `set_max_time_borrow`, `set_false_path`, `set_multicycle_path`,
    `set_max_delay`, `set_min_delay`, `set_clock_groups`, `group_path`, and `set_units`, producing
    both `CONSTR v1` (`constraints.rbin`) and canonical `constraints/merged.sdc` (`CANON`).
  - `rusteda_formats::liberty::read_file_diags` and `read_text_diags` with complete construct
    classification and aggregated `L` diagnostics.
  - Streaming structural Verilog reader (`RS-VLOG`) with hierarchy flattening, bus/concatenation
    expansion, `supply0`/`supply1` tie lowering, and `pg_pin` supply port pruning.
- **Shared compute timing task (`rusteda-compute::timing`):**
  - Level-parallel `TimingPropagationTask` (`FlatTimingGraph`) executing in `f64` or `f32` on the
    CPU (`rayon`) or on NVIDIA GPUs via NVRTC-compiled CUDA kernels (`--features cuda`), with
    `BACKEND_UNAVAILABLE` (`A0501`) diagnostics when a requested GPU backend is unavailable.
- **Phase 2a static timing engine (`ruststa::engine`):**
  - Multi-corner static timing analysis over `FrozenNetlist`, `.rlib`, and `CONSTR v1`: per-pin
    worst transition times across all enabled fan-in arcs, per-tag arrival and required times,
    `WireloadLumped` and `Ideal` wire models (with top-level port net exclusion and driver
    output-pin capacitance), 1D–3D NLDM interpolation/extrapolation, and Kleene 3-valued (`0`, `1`,
    `X`) constant propagation with cofactor arc disabling and `non_unate` sense refinement.
  - Sequential timing checks: flip-flops, active-high and active-low latches (opening-edge
    uncertainty, `set_max_time_borrow`, and opening-edge `D -> Q` re-launch), asynchronous
    `recovery`/`removal` in `async_default`, combinational and integrated clock-gating checks,
    endpoint status classification, and `-from`/`-to` timing exceptions (`set_false_path`,
    `set_multicycle_path`, `set_max_delay`, `set_min_delay`, `set_clock_groups`).
  - Backward required-time propagation, per-pin slacks, setup-then-hold `k`-worst path extraction,
    and revision-scoped handles (`path<N>`, `pin<N>`, `port<N>`, `inst<N>`, `net<N>`).
  - 24-section `STA v1` (`sta.rbin`) binary timing store (`Full` and `Summary` modes) with `O(1)`
    zero-copy mapped queries (`StaStore`) and incremental cone-invalidated timing updates
    (`TimingSession`).
- **Independent workspace checker (`ruststa_check::rev`):**
  - Engine-independent revision checker reading verbatim cached `.lib` files, re-parsed structural
    Verilog netlists, and canonical SDC (`constraints/merged.sdc`) with zero dependency on
    `ruststa` or `rusteda_formats::sdc`; evaluates constants, delays, checks, and timing exceptions
    (including `-through`) in `f64`, emits `C0500`–`C0999` diagnostics and `mm<N>` mismatch handles,
    and commits `sta_check.json` (`ruststa.sta_check.v1`).
- **Workspace `ruststa` CLI (`ruststa-cli`):**
  - Subcommands `run`, `update`, `check` (`--method independent|full|both`), `path`, `inspect`,
    `report`, and `completions`, with `rusteda.v1` JSON envelopes, `next_actions`, and the
    `run_e2e` scale benchmark.
- **Verification fixtures and Dual-PDK automation (`ruststa-fixtures`, `xtask`):**
  - 15 synthetic `wt_*` Liberty libraries, 8 topology netlist/SDC pairs, and deterministic
    `DesignGen` scale netlists (`1k` to `1M` cells).
  - `cargo xtask pdk fetch` and `cargo xtask verify pdk` (`--profile pr|full`, `RUSTEDA_PDK_DIR`)
    for pinned `sky130_fd_sc_hd` and `gf180mcu` standard-cell libraries.
- Shared read-only view of the timing table (`rusteda_workspace::sta`, WS-REQ-0126 to
  WS-REQ-0136), written from the format specification for the other tools of the suite, with a
  builder of minimal tables; a test checks it against the engine's reader on tables that
  `ruststa run` writes (counts, summaries, endpoints, ranked paths and their points).
- `--max-state-gib` on `ruststa run` and `ruststa update`: the analysis estimates the propagation
  state of each corner before allocating it and stops with `T0023` above the limit (default
  16 GiB; `STA-REQ-0024`).
- `cargo xtask verify pdk` runs every generated design of every library through `ruststa run` and
  `ruststa check --method independent` (the checker must agree); the PDK suite previously checked
  only the library API on three designs and skipped designs it did not find.
- `cargo xtask verify pdk` runs 1,000 random sizing and buffering mutations on each `tt` library
  of both PDK families with the incremental timing session and compares it with a full analysis
  after every step. The test existed but nothing ran it, it compared only every 25th step, and it
  passed as `SKIPPED` without libraries; it now fails when a library is missing.
- `cargo xtask verify` and `verify pdk` run each test step with `RUST_TEST_THREADS=4`, a timeout
  and, where available, a memory-limited user scope (`RUSTEDA_VERIFY_MEMORY_MAX`,
  `RUSTEDA_VERIFY_TIMEOUT_S`, `RUSTEDA_VERIFY_NO_SCOPE`); `verify pdk --timeout` now applies.

### Fixed
- `ruststa run` and `ruststa update` applied neither constants nor timing exceptions: netlist
  constants, tie cells, `set_case_analysis`, `set_logic_zero`/`set_logic_one`, `set_false_path`,
  `set_multicycle_path`, `set_max_delay`, `set_min_delay` and `set_clock_groups` were accepted at
  import and then had no effect on the reported timing. The analysis now applies them
  (`STA-REQ-0158`, `STA-REQ-0271`); forms it does not analyse yet are errors `T0071`, `T0073`,
  `T0074` or warning `T0072` instead of being dropped, and `set_case_analysis rising|falling`
  gets warning `T0040`.
- Sense refinement and cofactor disabling changed the library's arcs of cells without any
  constant input; they now apply only under constant inputs (`STA-REQ-0153`).
- Constant propagation scanned every graph arc for each load of a constant net on every round;
  its fixed point is now a worklist, linear in the pins of the netlist.
- The timing table's backward pass looked up the arc classes of the library instead of those of
  the lowered problem, which panicked once constant propagation refined an arc sense.
- `ruststa check` counted only endpoint checks that both sides time, so a check that one side did
  not time (a constant, an exception) was never a difference, and `--method full` paired endpoint
  lists by position without comparing their lengths. Such differences are now mismatches `C0502`
  and `C0504` (`STA-REQ-0307`).
- The independent checker read `[get_ports [list a b]]` of the canonical constraints as one
  pattern and dropped the input and output delays of such lists.
- Unanalyzed library arcs are reported by timing type instead of as `unknown`, the revision summary
  reports `null` instead of `0.0` for the worst slack of a corner without timed checks, and a timing
  table whose endpoint or corner count `PATH_RANK` cannot name is error `T0022` instead of being
  packed with truncated indices.
- `ruststa update` reports `incremental.computation`, which states that the values come from a full
  recomputation (`STA-REQ-0303`).
- `ruststa run`, `update` and `check` dropped every diagnostic of a successful command: warnings
  of the constraints and of the analysis (for example `T0015`, `T0016`, `D0102`, `D0110`) never
  reached the envelope or standard error. They now do (`STA-REQ-0308`).
- An exception whose `-from` or `-to` objects were all ignored lost only that option and so
  matched every path of the other one; it now matches no path (`STA-REQ-0255`).
- `set_clock_uncertainty -rise` and `-fall` were accepted and dropped; they are now error `D0011`
  (`SDC-REQ-0023`). `-level_sensitive` input and output delays, `set_load -subtract_pin_load` and
  minimum/maximum operating conditions are analysed without their effect and now get warnings
  `T0041`, `T0042` and `T0043` (`STA-REQ-0022`).
- The independent checker read `set_case_analysis rising` and `falling` as the constants `1` and
  `0`; they leave the pin unknown (`STA-REQ-0150`).
- The independent checker's matching of timing exceptions enumerated paths without a bound (more
  than 50 GB on a design of 8,000 cells with `-from [all_registers]`); it now stops at 200,000
  paths with the verdict `undecided` (`STA-REQ-0506`).
- The objects of an exception option were kept in an unsorted list and searched linearly for
  every startpoint and check; they are now sorted, and matching is a binary search.
- The timing table's query API named ranked paths from `path0` while the command line names them
  from `path1`, so `ruststa path --target path<N>` past the last path reported `path<N-1>` in its
  diagnostic. Every handle of the API now uses the command line's numbering (`STA-REQ-0305`).
- `cargo xtask verify pdk` wrote a WNS of `0.000000` for runs without a timed check (and one
  endpoint for every scale run) in `runs.tsv`, `report.json` and `summary.json`; these runs now
  report `unconstrained` (JSON `null`) and their real endpoint counts.

### Changed
- `rusteda_core::liberty::Library::templates` is now a `BTreeMap<String, Template>` (was
  `HashMap<String, Template>`) so that lookup-table templates iterate in deterministic key order.
- `ruststa-cli` now builds with no default Cargo features (`cpu` backend only by default; pass
  `--features cuda` to enable the `cuda` and `gpu` backends), and the default propagation precision
  for both `cpu` and `cuda` is `f64` (bit-identical to CPU), with `f32` opt-in via
  `--precision f32`.
- Repository crates and specifications follow the shared RustEDA contract (`rusteda-core`,
  `rusteda-workspace`, `rusteda-compute`, `rusteda-formats`, `rusteda-cli`); [`docs/roadmap.md`](docs/roadmap.md)
  replaces `ROADMAP.md`, and [`docs/specs/compute/decisions.md`](docs/specs/compute/decisions.md)
  supersedes [`docs/adr/0001-gpu-runtimes-and-native-cuda.md`](docs/adr/0001-gpu-runtimes-and-native-cuda.md).

### Removed
- Legacy run-record CLI subcommands (`ruststa sta`, `ruststa check sta`, `ruststa show`) and options
  (`--runs-root`, `@` run references, `--record`, `--run-dir`, `--label`, `--print`, `--from-run`),
  replaced by the workspace commands (`ruststa run`, `update`, `check`, `path`, `inspect`, `report`).
- CubeCL and `wgpu` GPU runtimes and dependencies (`--backend wgpu`).
