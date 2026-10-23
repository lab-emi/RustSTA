# ADR 0001: GPU runtime of the propagation backend: native CUDA

- Status: superseded by the decision record of the shared compute crate,
  [docs/specs/compute/decisions.md](../specs/compute/decisions.md) (2026-10-16)
- Date: 2026-09-28; amended 2026-10-16 (CubeCL and wgpu removed)

## Context
RustSTA offloads the arrival propagation of its timing engine to GPUs (STA-REQ-0400). Its first
version had three GPU runtimes: kernels in the CubeCL language on CUDA and, through wgpu, on
Vulkan, Metal and DirectX 12 devices, and the same kernels written in CUDA C (`cuda-native`), which
showed what the portable path costs. The RustEDA development plan (section 1.3) then chose not to
couple the tools to a cross-platform GPU kernel language: compute-heavy tasks sit behind
coarse-grained Rust traits of the shared crate `rusteda-compute`, with native backends: `cpu` (pure
Rust, always built), `cuda` on Linux and, later, Metal on Apple Silicon.

## Decision
- `rusteda-compute`, shared by the RustEDA tools, owns runtime selection, the per-process device
  self-test and the CUDA device layer (COMPUTE-REQ-0001 to 0003). Engines implement one task trait
  with one implementation per GPU runtime.
- The one GPU runtime is `cuda` (Cargo feature `cuda`): kernels written in CUDA C, compiled at run
  time by NVRTC for the device's architecture and launched through the CUDA driver API
  (COMPUTE-REQ-0002, STA-REQ-0407). Host code uses the `cudarc` crate (MIT OR Apache-2.0), pinned
  to an exact version, which loads the CUDA driver and NVRTC libraries at run time. Kernels stay
  stable-Rust-buildable: no nightly compiler, no `nvcc` at build time and no device-code crates.
- CubeCL and wgpu are removed with their two runtimes; the CUDA C runtime takes the name `cuda`
  (STA-REQ-0400, STA-REQ-0404). Tests run the GPU runtime against the CPU results (STA-REQ-0403).
- Unsafe code is confined to backend glue, each block with a `// SAFETY:` comment (AGENTS rules
  8.4): the module `cuda` of `rusteda-compute` and, until the propagation becomes a compute task of
  `rusteda-compute`, the kernel launches of the timing engine (`src/gpu/cuda.rs` of `ruststa`).
- With a GPU backend, the timing engine prepares the runtime and compiles its kernels on a second
  thread while the netlist is read, the graph is built and the constraints are applied
  (STA-REQ-0408, COMPUTE-REQ-0003).

## Consequences
- Each propagation kernel has one implementation; the backend-consistency suite (`sta-backends`)
  compares it with the CPU backend.
- The default build of the command line has the `cuda` runtime (Cargo feature `cuda`), as it had
  the CubeCL runtimes before; `--no-default-features` builds the `cpu` backend only. Building needs
  no CUDA toolkit. Running `cuda` needs the CUDA driver and the NVRTC library of a CUDA toolkit;
  without them the runtime fails its preparation and the analysis reports an input error.
- GPUs other than NVIDIA ones, which the wgpu runtime served, have no backend. Apple Silicon gets
  the planned Metal backend of `rusteda-compute`; for the GPUs of other vendors on Linux and
  Windows no backend is planned.
- The lock file lost the 210 third-party packages of the CubeCL and wgpu trees. Run records of
  earlier versions still load: `cuda-native` reads as `cuda`, and `wgpu` names the removed runtime
  (PROTO-REQ-0062).
