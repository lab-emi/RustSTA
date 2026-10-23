# ADR 0007: Binary timing result store (`sta.rbin`) and zero-copy mapped queries

- Status: accepted
- Date: 2026-10-17

## Context

Phase 1 of RustSTA had no persistent binary timing store; every `report_checks`, `report_tns`, or
`report_wns` invocation required re-parsing the gate-level Verilog netlist, Liberty libraries, and
SDC constraints and re-running timing propagation. In Phase 2 (`STA-REQ-0302`, `STA-REQ-0312` to
`STA-REQ-0315`, `WS-REQ-0030` to `WS-REQ-0038`), `ruststa run` writes `sta.rbin` (`STA v1`) into a
workspace revision (`rev/<n>/sta.rbin`), and read-only commands (`ruststa query`, `rusteda inspect`,
`rusteda summary`) must answer summary, endpoint, path, and pin queries from a cold process on a
1M-cell netlist in `<= 25 ms` (warm) / `<= 50 ms` (cold) without loading the netlist or libraries.

Three layout tensions govern `sta.rbin`:

1. **Per-node storage size vs. multi-clock launch tags**: most pins in a digital block are reached
   by one active clock launch tag, while a minority of pins in clock-muxed or latch-borrowing cones
   carry multiple active tags. Allocating `T_max` tag slots inline per node bloats `NODE_REC` when
   `T_max` is large, whereas storing all tags out-of-line adds an extra cache-line indirection on
   every single-clock pin query.
2. **Pre-materialized paths vs. arbitrary-`N` path queries**: materializing `PathPointRec` arrays for
   10,000 paths bloats `sta.rbin`, but walking predecessors backward through `NODE_REC` on the fly
   causes dependent pointer-chasing cache misses across the multi-hundred-megabyte `NODE_REC` table.
3. **Open latency vs. payload integrity**: computing `xxh3-64` over a ~250 MB `sta.rbin` file at
   5–10 GB/s takes 25–50 ms by itself, consuming the entire cold-query latency budget before any
   section is read.

## Decision

We adopt the 24-section `STA v1` layout specified in [`docs/specs/sta/store.md`](../specs/sta/store.md):

1. **Inline tag 0 + CSR overflow (`NODE_REC` + `TAG_EXTRA_PTR` + `TAG_EXTRA`)**:
   - `NodeRec` (152 bytes) groups hot fields into two 64-byte cache lines plus a 24-byte tail:
     - Cache line 0 (bytes `0..64`): `slew_ns` (`[[f64; 2]; 2]`, 32B) and worst node `slack_ns`
       (`[[f64; 2]; 2]`, 32B).
     - Cache line 1 (bytes `64..128`): `arr0_ns` (`[[f64; 2]; 2]`, 32B) and `req0_ns`
       (`[[f64; 2]; 2]`, 32B) for the first active launch tag (`tag0_id`).
     - Tail (bytes `128..152`): `pred0` (`[[u32; 2]; 2]`, 16B), `tag0_id` (`u32`), `active_tags`
       (`u16`), and `flags` (`u16`).
   - Additional active tags (`active_rank >= 1`) are stored in CSR form in `TAG_EXTRA_PTR` and
     `TAG_EXTRA` (56 bytes per extra active tag). Single-clock designs pay zero `TAG_EXTRA` records.
2. **Two-tier path storage (`PATHS`/`PATH_POINTS` + `PATH_RANK`/`PATH_CHAIN`)**:
   - The top `K` (`top_k_paths`, default `100`) worst paths are materialized directly in `PATHS`
     (56B `PathHeaderRec`) and `PATH_POINTS` (32B `PathPointRec`), answering common `query paths`
     requests with a single contiguous slice read.
   - Up to `path_limit` (default `10,000`) ranked paths are indexed in `PATH_RANK` (8B) and stored
     as compact forward chains of packed `u64` steps in `PATH_CHAIN_PTR` + `PATH_CHAIN` (`8 bytes`
     per step: `node_id`, `tr`, `point_kind`, `active_tag_rank`, `corner_node_bucket`).
   - Reconstructing a path with `rank >= K` scans its `PATH_CHAIN` slice in two passes: Pass 1
     prefetches the two 64-byte cache lines of each step's `NodeRec` in batches of 8 before Pass 2
     reads arrivals and slews, replacing sequential pointer-chasing stalls with pipelined memory
     reads.
3. **Zero-allocation `"inst/pin"` hashing (`NAME_HASH` and `INST_HASH`)**:
   - Pin names follow the deterministic composite pattern `"<inst_name>/<pin_name>"` without storing
     a separate heap string per pin in `STRINGS`. `hash_node_name` hashes `"inst/pin"` on a 256-byte
     stack buffer with `xxh3_64` and disambiguates hash collisions via `NodeId`, keeping `STRINGS`
     proportional to `N_ports + N_instances + N_cells` rather than `N_pins`.
4. **Two-mode open contract (`O(1)` structure validation vs. full `xxh3-64`)**:
   - Mapped query opens (`StaStore::from_rbin`, `StaStore::open_rev`) validate the 64-byte header,
     24-entry section directory, and `StaMeta` invariants in `O(1)` time (`< 50 us`), avoiding a
     full-file checksum pass on read-only queries (`WS-REQ-0031`).
   - Explicit verification (`StaStore::verify_hash`, `StaStore::open_for_mutation_input`) checks the
     full-file `xxh3-64` payload digest whenever `sta.rbin` is verified (`rusteda verify`) or loaded
     as an incremental/check input (`ruststa check`).
5. **Dual storage modes (`StoreMode::Full` vs `StoreMode::Summary`)**:
   - `StoreMode::Full` (default) writes all 24 sections for full query and incremental support.
   - `StoreMode::Summary` omits `NODE_REC`, `TAG_EXTRA_PTR`, `TAG_EXTRA`, `PATH_CHAIN_PTR`, and
     `PATH_CHAIN`, reducing artifact size to a few tens of kilobytes while still serving `summary`,
     `endpoints`, top-`K` `paths`, and `scope`/`notes` queries (`WS-REQ-0035`).

## Consequences

- Opening `sta.rbin` via `mmap` and running `query_summary`, `endpoint_table(limit=100)`,
  `query_path_handle(0..100)`, `reconstruct_path_from_chain`, and `inspect_pin` on 500k-cell and
  1M-cell stores completes in well under a millisecond warm (far below the `25 ms` warm / `50 ms`
  cold budget of `WS-REQ-0030` to `WS-REQ-0038`).
- Canonical `-0.0 -> +0.0` normalization and deterministic sorting guarantee bitwise-identical
  `sta.rbin` output across `--threads 1` and `--threads 32` (`STA-REQ-0302`, `STA-REQ-0405`).

## Amendment (2026-10-23): format version 2

Version 1 packed several indices into narrow bit fields and truncated values that did not fit:
`PATH_RANK` named an endpoint in 20 bits, a `PATH_CHAIN` step named its node in 24 bits and its
tag slot in 5, `ENDPOINTS` kept the name bucket in 22 bits of `kind_flags`, and `PATH_CHAIN_PTR`
was `u32`. OpenC910 (4,444,505 endpoints, 3,960,518 instances) did not fit, and the analysis
was rejected with `T0022`. Version 2 stores each of these at full width: `PATH_RANK` is 12 bytes
(`u32` endpoint, `u16` corner), `ENDPOINTS` is 36 bytes with its own `name_bucket_plus1` word, a
chain step is three words `[node, step_word, name_bucket]` with a 29-bit tag slot, and
`PATH_CHAIN_PTR` is `u64`. The remaining fixed-width fields (`u32` string offsets and ids, the
20-bit cell-name id and 12-bit pin count of an `INST_HASH` bucket) are checked before the table
is built, and an analysis that would overflow them is rejected with `T0022` rather than written
truncated (specification: [`docs/specs/sta/store.md`](../specs/sta/store.md), section 6). The
engine reader opens version 2 only; the shared workspace reader also converts version 1 tables.
On VexRiscv (220,450 endpoints) the larger records add about 4 bytes per endpoint and per ranked
path and 4 bytes per chain step.
