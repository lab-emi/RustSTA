# ruststa

The RustSTA static timing engine ([`docs/specs/sta/README.md`](../../../docs/specs/sta/README.md)):
multi-corner static timing analysis of gate-level netlists in a RustEDA workspace.

## Architecture (`src/engine/`)

- **`netlist_view`**: zero-copy `NetlistView` over the workspace's `FrozenNetlist` (`NETLIST v1`),
  resolving instances, nets, ports, connected pins, and constant `0`/`1` drivers.
- **`library`**: corner `TimingLibrary` and `CompiledCell` compiled from `.rlib` (`LIBERTY v1`),
  with 1D, 2D, and 3D NLDM delay, transition, and constraint tables (`lu_table_template`),
  `Table::mid_slew`, pin capacitance ranges, three-state functions, `when` conditions, clock-gating
  roles, and unit scaling to `ns`, `pF`, and `kOhm`.
- **`graph`**: flat `TimingGraph` of pin and port `NodeId`s and `TimingArc`s, deterministic Tarjan
  SCC cycle breaking (`T0002`), and topological levelization.
- **`constants`**: Kleene three-valued (`0`, `1`, `X`) constant propagation (`STA-REQ-0150`–`0157`),
  `set_case_analysis`, `set_logic_zero`, `set_logic_one`, `set_disable_timing`, cofactor and
  `when`-condition arc disabling, and `non_unate` sense refinement.
- **`wire`** and **`delay`**: `Ideal` and `WireloadLumped` wire models (with top-level port net
  exclusion and driver output-pin capacitance) and linear NLDM interpolation/extrapolation.
- **`sdc`** and **`setup`**: `CONSTR v1` constraint binding, clock networks, launch tags, and
  harmonic multi-period clock relations (`STA-REQ-0211`, `0212`).
- **`propagate`**: lowering to `rusteda_compute::timing::TimingPropagationTask`
  ([`docs/specs/sta/lowering.md`](../../../docs/specs/sta/lowering.md)) for level-parallel
  propagation on the CPU (`f64`/`f32`) or on an NVIDIA GPU via NVRTC CUDA kernels (`f64`/`f32`,
  Cargo feature `cuda`), accumulating worst early/late rise/fall transitions over all enabled
  fan-in arcs and per-tag arrivals (`STA-REQ-0108`).
- **`checks`**, **`exceptions`**, and **`paths`**: setup, hold, recovery, removal (`async_default`),
  active-high/low latch time borrowing (`STA-REQ-0220`–`0222`), and combinational/integrated
  clock-gating checks; `-from`/`-to` timing exceptions (`set_false_path`, `set_multicycle_path`,
  `set_max_delay`, `set_min_delay`, `set_clock_groups`); backward required-time propagation,
  per-pin slacks, setup-then-hold `k`-worst path extraction, and revision-scoped handles
  (`path<N>`, `pin<N>`, `port<N>`, `inst<N>`, `net<N>`).
- **`store`** and **`incremental`**: 24-section `STA v2` (`sta.rbin`) binary writer and zero-copy
  mapped reader (`StaStore`, `Full` and `Summary` modes;
  [`docs/specs/sta/store.md`](../../../docs/specs/sta/store.md)), and `TimingSession` cone
  invalidation and level-bucketed incremental re-propagation.
