<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Performance and memory

RustSTA separates timing propagation from mapped result queries. Measure the
workflow you actually need: input import, graph lowering, analysis, persistence,
and any host/device synchronization all contribute to elapsed time.

This documentation release claims no general RustSTA-vs-OpenSTA speedup.
Head-to-heads on single tasks and hosts are published with their scope, below. The
source contains benchmark harnesses; a harness is not itself a measured result.
Avoid treating a GPU propagation microbenchmark as end-to-end timing
performance.

## Published head-to-head · 2026-11-13

The RustEDA Ibex host campaign of 2026-11-13 (IHP SG13G2 / EZ130 with two real
SRAMs, on a 4-vCPU cloud host) replayed the flow's recorded RustSTA 0.4.3
(commit [`d453782`](https://github.com/lab-emi/RustSTA/commit/d453782b118224ee27517857a59ad5a597a0fb4c), built with Rust 1.100.0) and OpenSTA 3.1.0 commands on the
same final netlist, native SPEF, Liberty and SDC at TT and SS, with the method of
2026-11-02 below. RustSTA's median wall time was 4.692 s at 2 threads and
3.403 s at 4 threads, against 7.349 s and 6.881 s for OpenSTA (1.57× and
2.02×). Its CPU time per run was 6.19–6.56 s and 6.25–6.42 s, OpenSTA's
8.63–9.03 s and 9.69–10.63 s. Worst peak RSS was 152,088 and 197,060 KiB
against 176,060 and 176,252 KiB. **RustSTA was faster at both thread counts and
used 13.6 % less peak memory at 2 threads, but 11.8 % more at 4 threads**; the
campaign did not investigate the cause of that increase. Numerical agreement on
identical SPEF `passed` the 1 ps gate: the eight compared worst slacks differ by
at most 0.2697 ps. The host is a different virtual machine from 2026-11-05's,
on which every LibreLane stage took less time, so the two campaigns' times are
not a before/after.

Since commit
[`de210a6`](https://github.com/lab-emi/RustSTA/commit/de210a66e5cef12e9e0cb11eec8a2e11ee77f0ba),
made after the campaign, RustSTA's peak memory is below OpenSTA's at both thread
counts. The increase was memory that glibc's allocator kept resident after it
was freed, not live data. In 7 interleaved same-load runs with OpenSTA and the
campaign's binary on the 4-vCPU 2.30 GHz VM of the campaign's same-load gate
reruns, the timing section's worst peak RSS was 124,120 and 154,144 KiB at 2
and 4 threads against OpenSTA's 176,172 and 176,308 KiB (**29.5 % and 12.6 %
less**), where the campaign's binary used 165,156 and 194,124 KiB. Wall and CPU
ranges overlap with the campaign binary's, so the speed is unchanged (1.56× and
1.94× faster than OpenSTA in those runs), and the outputs are byte-identical.
Times from that host are not comparable with the campaign's above.

[All eight compared values and the campaign context](../development-status.md) ·
[The change after the campaign](../development-status.md#peak-memory-below-opensta-2026-11-13) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-13) ·
[RustSTA peak memory and RustROAD extraction](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-peak-memory-and-rustroad-extraction-2026-11-13)

## Published head-to-head · 2026-11-05

The RustEDA Ibex host campaign of 2026-11-05 (IHP SG13G2 / EZ130 with two real
SRAMs, on a 4-vCPU cloud host) replayed the flow's recorded RustSTA 0.4.3
(commit [`3550739`](https://github.com/lab-emi/RustSTA/commit/35507399f3a298e8e94ff76cdbd24413e64e1236)) and OpenSTA 3.1.0 commands on the same final netlist, native
SPEF, Liberty and SDC at TT and SS, with the method of 2026-11-02 below.
RustSTA's median wall time was 6.825 s at 2 threads and 5.317 s at 4 threads,
against 11.168 s and 10.566 s for OpenSTA. Worst peak RSS was 148,164 and
169,188 KiB against 180,320 and 180,528 KiB. **RustSTA was faster and used less
memory on this task and host.** Numerical agreement on identical SPEF `passed`
the 1 ps gate: the eight compared worst slacks differ by at most 0.1639 ps.

[All eight compared values and the campaign context](../development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta-2026-11-05)

## Published head-to-head · 2026-11-02

The RustEDA Ibex host campaign (IHP SG13G2 / EZ130 with two real SRAMs, on a
4-vCPU cloud host) replayed the flow's recorded RustSTA 0.4.3 and OpenSTA 3.1.0
commands on the same final netlist, native SPEF, Liberty and SDC at TT and SS.
Each repetition ran in a fresh copy of the timing directory with every command
under GNU time: one warm-up and three interleaved measured repetitions at 2 and
4 threads. RustSTA's scope is init, import, one run covering both corners and
eight reports; OpenSTA's is one process per corner. Wall time sums each
contender's processes, and peak RSS is its largest single process.

RustSTA's median wall time was 17.898 s at 2 threads and 15.976 s at 4 threads,
against 15.253 s and 13.497 s for OpenSTA. Worst peak RSS was 498,004 and
502,876 KiB against 266,916 and 267,704 KiB. **OpenSTA was faster and used less
memory on this host.** Numerical agreement on identical SPEF `failed` the 1 ps
gate: TT setup worst slack differs by 1.4052 ps (native SPEF) and 1.2838 ps
(OpenRCX SPEF), and the other six compared values are within 0.2153 ps.

[All eight compared values and the campaign context](../development-status.md) ·
[STA head-to-head](https://lab-emi.github.io/RustEDA/benchmarks/#ruststa-vs-opensta) ·
[All benchmarks](https://lab-emi.github.io/RustEDA/benchmarks/)

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
Neither replaces a complete command boundary measurement. With glibc, `ruststa`
fixes the allocator's mmap threshold at 1 MiB unless `MALLOC_MMAP_THRESHOLD_`
or `glibc.malloc.mmap_threshold` in `GLIBC_TUNABLES` already sets it, and
returns the allocator's free pages at the end of every analysis phase; record
either variable beside the peak RSS when it is set.

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
