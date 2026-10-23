<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# CPU and CUDA backends

The default build uses deterministic pure Rust CPU propagation. Start there for
reproducibility and to establish a reference before trying acceleration.

| Option | Values | Default |
|---|---|---|
| `--backend` | `cpu`, `cuda`, `gpu` | `cpu` |
| `--precision` | `f64`, `f32` | `f64` |
| `--threads` | Worker count; `0` selects the available pool | `0` |
| `--max-paths` | Ranked paths stored per corner | `5` |
| `--max-state-gib` | Estimated propagation state limit per corner | `16` |

CPU f64 propagation is designed for bit-identical results across worker counts,
with deterministic ID tie-breaking. The default precision is f64; a higher
thread count does not imply a different numerical model.

## Build and select CUDA

```bash
cargo build --locked --release -p ruststa-cli --features cuda
target/release/ruststa --version
target/release/ruststa run --workspace ./ws --base-rev rev1 \
  --backend cuda --precision f64 --max-paths 5 --format json
```

The backend dynamically loads the NVIDIA driver and NVRTC at runtime, so a CUDA
toolkit is not required to compile the Rust package. A working device and runtime
libraries are needed to execute CUDA kernels.

`gpu` chooses an available compiled GPU backend after a device self-test.
If CUDA was not compiled in, the device is unavailable, or self-testing fails,
the command fails with `BACKEND_UNAVAILABLE` (`A0501`, exit 1).

CPU/CUDA f64 propagation has a bitwise equality contract. Opt-in f32 propagation
has an explicit tolerance of `max(1e-4 ns, 2e-5 × |v|)` for `|v| ≤ 100 ns`.
These are test contracts for propagation, not a speedup claim or a statement
that every analysis phase executes on the GPU.

CUDA verification needs a real device. Missing hardware is unverified coverage.
Metal/Accelerate is a family architecture direction, not a shipped RustSTA
backend. See [compute decisions](../specs/compute/decisions.md) and
[measurement guidance](performance.md).
