# RustSTA lowering and propagation backends

Every forward propagation of the timing engine lowers the timing graph, the corner library view,
the per-node capacitive loads, the startpoint rows and the tag-map records into the timing task of
`rusteda-compute` (`crates/core/rusteda-compute/src/timing`, specified in
[timing.md](../compute/timing.md); design decisions in [decisions.md](../compute/decisions.md) and
runtime selection in [compute backends](../compute/README.md)). The engine contains no GPU kernels
or `unsafe` device code of its own.

**STA-REQ-0400** Backends compute per-tag arrivals, per-pin transitions and predecessors through
`rusteda_compute::timing::TimingPropagationBackend` (COMPUTE-REQ-0010):

- `cpu`: `f64` on the CPU, level-parallel across the rayon thread pool (STA-REQ-0405,
  COMPUTE-REQ-0027); its scalar order is the reference for backend consistency;
- `cuda`: CUDA C kernels on the CUDA runtime (STA-REQ-0407, COMPUTE-REQ-0028), in `f64` by default
  (bit-identical to `cpu`) or in `f32` when single precision is requested (`--precision f32`);
- `gpu`: the first compiled GPU backend whose device passes the per-process self-test
  (STA-REQ-0404, COMPUTE-REQ-0001).

Everything else (netlist view, graph construction, cycle breaking, constraints, backward required
times, timing checks and reports) is shared on the host.

**STA-REQ-0401** Lowered data layout. Nodes are numbered `0..n` by canonical topological ID
`(level, canonical_node_key)` (the longest distance from a source after cycle breaking, ties broken
by the frozen netlist's canonical node order), so that the nodes of level `l` form the contiguous
slice `level_ptr[l] .. level_ptr[l + 1]` in `LevelizedTimingCsr` (COMPUTE-REQ-0011). Incoming
enabled propagation arcs are stored per level-ordered node in CSR form (`in_ptr`, `in_arcs`),
ordered by `(topo_rank(src), arc_idx)`; each `InArc` is three 32-bit words `{src, class, map}`:
the level-order position of its source (`src < level_ptr[l]`), its `ArcClass` index in
`CompiledLutBank::classes`, and the index of its tag map in `LevelizedTimingCsr::map_ptr` or
`NONE` (`u32::MAX`) for the identity map. `ArcClass` descriptors are shared across all instances of
a library cell: each cell propagation arc holds its allowed `(ti, to)` transition bits, `delay[2]`
and `slew[2]` table indices for rising and falling outputs, and `setup[2]` table indices for latch
tag-map windows; the wire class sets the `WIRE` flag bit and passes rising and falling transitions
unchanged (COMPUTE-REQ-0013). Liberty lookup tables of up to three axes are flattened into one
`LutDesc` array and one `f64` data array (`CompiledLutBank`). Tag-map records (`TagMapRec`,
COMPUTE-REQ-0012) encode latch `D -> Q` transparency windows (`src_tag`, `dst_tag`, time origin
`shift`, `open`, `close`, `close_slew`, and `limit` for `set_max_time_borrow`) as well as
exception state transitions. Capacitive loads `load[v][tr * 2 + an]` are stored per level-ordered
node. Per-node values (`TimingValues` / `TimingStateSoA`, COMPUTE-REQ-0016) store per-tag arrivals
`arr[((v * T + t) * 2 + tr) * 2 + an]`, per-pin transitions `slew[(v * 2 + tr) * 2 + an]` (shared
across all tags, including untagged sources), and predecessors `pred` (`src | (ti << 31)` or
`NO_PRED`), with `+inf` (early) and `-inf` (late) as absent sentinels and `+0.0` canonicalization.

**STA-REQ-0402** Level-synchronous GPU execution. The GPU backend launches one kernel per level
(COMPUTE-REQ-0017, COMPUTE-REQ-0028); each thread computes one node by seeding its start row (if
any), visiting its incoming arcs sequentially in CSR order, evaluating the lookup tables, updating
the per-pin transitions and reducing candidate arrivals per tag with minimum (early) or maximum
(late). Because reductions are sequential per node in CSR order without atomics, results are
bit-identical across repeated runs on every backend and precision (COMPUTE-REQ-0019). Values stay
resident on the device between levels and are copied back to the host view according to the
requested `Readback` mode (COMPUTE-REQ-0022).

**STA-REQ-0403** Consistency across backends and precisions (COMPUTE-REQ-0019):

- in `f64` (the default precision), the `cuda` backend and the `cpu` backend produce bit-identical
  arrivals, transitions and predecessors (`to_bits() == to_bits()`) for every node, tag, transition
  and analysis bound;
- in `f32` (`--precision f32`), for every node, tag, transition and analysis bound with magnitude
  `|v| <= 100 ns`, the GPU and CPU values differ by at most `max(1e-4 ns, 2e-5 * |v|)` (and within
  `1e-4 ns + 1e-5 * |v|` on standard paths), and absent values agree, except where a candidate
  arrival lies within that bound of a tag-map window edge (`open`, `close - setup`, or `limit`).
  Tolerance reason: `f32` has a 24-bit significand (`6e-8` relative rounding per operation), which
  accumulates over paths of a few hundred to a few thousand arcs and table interpolations.

**STA-REQ-0404** Runtime selection and error reporting. The build feature `cuda` enables the CUDA
backend in `rusteda-compute`. Requesting `cuda` selects it, and requesting `gpu` selects the first
compiled GPU backend whose device passes its per-process self-test (COMPUTE-REQ-0001,
COMPUTE-REQ-0010). An uncompiled or unavailable GPU backend maps to `BACKEND_UNAVAILABLE` with
diagnostic `A0501` (exit status 1), a problem exceeding device memory or 32-bit index limits maps
to `INPUT_REJECTED` (`A0502`), and compilation, device or validation failures map to `A0503`–`A0505`
(COMPUTE-REQ-0025), never to a silent fallback or wrong values. The legacy `ruststa sta --backend
cuda|gpu` command is retired and exits with an input error naming `ruststa run`.

**STA-REQ-0405** Parallel CPU determinism. The `cpu` backend computes the nodes of each level in
parallel on the calling thread's rayon thread pool (`--threads`, COMPUTE-REQ-0027). Each node
reduces its incoming arcs sequentially in CSR order in `f64` with `-0.0` canonicalized to `+0.0`,
so arrivals, transitions and predecessors are bit-identical for any thread count (`1`, `3`, `8`, …).

**STA-REQ-0406** Device initialization. The GPU backend fills the device value arrays with the
absent sentinels (`+inf` early, `-inf` late, `NO_PRED` predecessors) and scatters the compact
`StartRows` onto their startpoint nodes on the device (COMPUTE-REQ-0015, COMPUTE-REQ-0017), so that
only the startpoint rows are transferred from the host.

**STA-REQ-0407** CUDA kernels. The `cuda` backend executes the level-synchronous kernels of
STA-REQ-0402 and STA-REQ-0406 written in CUDA C (`timing/kernels/timing.cu`) on the CUDA runtime of
`rusteda-compute`, compiled at run time with NVRTC for the device's architecture with the options of
COMPUTE-REQ-0002 and COMPUTE-REQ-0028 (`-DREAL_F64` and `--fmad=false` for `f64` bit-identity with
the CPU; single precision for `f32`), on the lowered ABI of STA-REQ-0401 and under the consistency
rules of STA-REQ-0403. It is available on NVIDIA GPUs only.

**STA-REQ-0408** Preparation overlap. When a GPU backend is requested, runtime selection, the
device self-test and NVRTC kernel compilation (`rusteda_compute::timing::prewarm`,
COMPUTE-REQ-0029) can run on a background thread while the netlist is read, the timing graph is
built and constraints are compiled (COMPUTE-REQ-0003). Propagation waits for the preparation and
reports any preparation failure through STA-REQ-0404.

**STA-REQ-0024** Propagation memory budget (AGENTS rule 9.2). Before it allocates the propagation
state of a corner, the analysis estimates it as nodes × tags × 160 bytes (arrivals and
predecessors in the compute state and in node order, required times and slacks of the backward
pass). An estimate above the limit (`AnalysisOptions::max_state_bytes`; `--max-state-gib` of
`ruststa run` and `ruststa update`, default 16 GiB, a finite number greater than 0) is error
`T0023` naming the corner, the nodes and the tags, before any of the state is allocated.
