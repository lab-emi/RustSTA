<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Performance and memory

RustSTA separates timing propagation from mapped result queries. Measure the
workflow you actually need: input import, graph lowering, analysis, persistence,
and any host/device synchronization all contribute to elapsed time.

This documentation release publishes no numerical RustSTA-vs-OpenSTA speedup.
The source contains benchmark harnesses; a harness is not itself a measured
result. Avoid treating a GPU propagation microbenchmark as end-to-end timing
performance.

## Measure an analysis

```bash
free -h
(
  ulimit -v 16777216
  RUSTSTA_PHASES=1 /usr/bin/time -v -o timing.time timeout 180s \
    ruststa run --workspace ./ws --base-rev rev1 \
      --threads 2 --max-paths 10 --max-state-gib 4 --format json \
      > timing.json 2> timing.stderr
)
```

Use a fresh, equivalently prepared workspace for each mutating repetition.
Record cold/warm cache policy, host CPU, worker allocation, input hashes, backend,
precision, corner, wire model and unconstrained endpoint count. Report WNS/TNS,
path/check agreement and peak RSS beside wall time. If import is outside the
timer, label the number analysis-only.

The `RUSTSTA_PHASES=1` diagnostics help locate import, graph and propagation
costs. `/usr/bin/time -v` supplies whole-process maximum resident set size.
Neither replaces a complete command boundary measurement.

## Bound the workload

Estimate state as objects × bytes per entry × corners × workers, accounting for
per-node/arc vectors, timing tags and worker scratch. Ranked path counts do not
bound every other state table. Keep `--max-paths` and `--max-state-gib` explicit
when running unfamiliar inputs.

Use topological propagation and bounded extraction rather than enumerating all
paths through reconvergent logic. Restrict report output with `--limit`.
Run large verification jobs with two to four test workers and serialize jobs
estimated above 4 GiB using `flock .work/scale.lock`.

For CUDA, prefer a cgroup RSS/swap guard so virtual address reservations work:

```bash
systemd-run --user --scope -p MemoryMax=16G -p MemorySwapMax=4G \
  timeout 180s ruststa run --workspace ./ws --base-rev rev1 \
    --backend cuda --precision f64 --max-paths 10 --format json
```

If the user service manager is unavailable, arrange an equivalent OS/container
memory limit before a large GPU run. Do not apply a CPU address-space cap to a
CUDA process.

## Existing harnesses

The CLI package includes `run_e2e` and `backends` benchmark targets, and the
[PDK suite](../verification/pdk.md) includes large store/query cases. Run them
only after an input-specific memory estimate and host preflight. Compare CUDA
results to CPU reference results and external timing to pinned OpenSTA outputs.
