<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Install from source

RustSTA 0.4.3 is a development preview on the RustEDA integration branch. It is not
published on crates.io. Linux is the CI baseline; other platforms have no CI
verification claim.

Install Git and [Rust through rustup](https://rustup.rs/), then build both CLI tools:

```bash
git clone --branch codex/unified-brand https://github.com/lab-emi/RustSTA.git
cd RustSTA
cargo build --locked --release -p rusteda-cli -p ruststa-cli
export PATH="$PWD/target/release:$PATH"
ruststa --version
```

`rust-toolchain.toml` pins Rust 1.97.1. Rustup selects and installs that toolchain
when you invoke Cargo in the checkout. `rusteda` manages the disk workspace;
`ruststa` performs timing analysis and reads timing results.

The default build uses the pure Rust CPU backend. No Python, GPU driver, CUDA
toolkit, resident process or GUI is required. Python is only used to author the
documentation and brand assets.

For CUDA, build `ruststa-cli` with `--features cuda` and read the
[backend requirements](backends.md). Build-time availability and runtime device
availability are separate checks.

## Build within a memory budget

Set worker concurrency to match the host. On a pure CPU Linux build, for example:

```bash
free -h
(
  ulimit -v 16777216
  CARGO_BUILD_JOBS=2 /usr/bin/time -v timeout 900s \
    cargo build --locked --release -p rusteda-cli -p ruststa-cli
)
```

This example caps virtual memory at 16 GiB and wall time at 15 minutes. Increase
the time allowance deliberately for a slower machine. CUDA needs a cgroup memory
guard instead of a virtual-memory limit; see [performance and memory](performance.md).

Continue with [your first timing run](quickstart.md).
