# Binary timing result store (`sta.rbin` / `STA v2`)

The binary timing result store (`sta.rbin`, artifact kind `"STA"`, format version `2`) persists the
complete output of a static timing run in the shared `RBIN v1` container
(`crates/core/rusteda-workspace/src/rbin.rs`, specified in
[`docs/specs/workspace/README.md`](../workspace/README.md)). It satisfies the binary store,
incremental staging, and mapped query requirements (`STA-REQ-0302`, `STA-REQ-0305`, `STA-REQ-0312`
to `STA-REQ-0315`, `STA-REQ-0401`, `STA-REQ-0405`, `WS-REQ-0030` to `WS-REQ-0038`, `WS-REQ-0045`).
Design rationale and layout trade-offs are recorded in
[ADR 0007](../../adr/0007-sta-store.md).

## 1. Container and opening contract

`sta.rbin` is a little-endian, 8-byte-aligned `RBIN v1` file with `kind = "STA"` and
`format_version = 2`. The engine reader (`StaStore`) opens version `2` only and rejects an older
table with a corrupt-format error, so a revision analyzed by an older `ruststa` is analyzed again
(`ruststa run`). The shared workspace reader (`rusteda_workspace::sta`) reads versions `1` and `2`
([`docs/specs/workspace/sta.md`](../workspace/sta.md)). Every fixed-stride section uses a `#[repr(C)]` `Pod + Zeroable` record type
so `bytemuck::try_cast_slice` succeeds directly on an `mmap`'d file (`Revision::open_rbin`) or
aligned buffer (`Rbin::read`) without copying or heap allocation.

Two open paths are provided on `StaStore`:

1. **Mapped query open (`StaStore::from_rbin`, `StaStore::read_path`, `StaStore::open_rev`,
   `StaStore::open_head`)**: validates the 64-byte `RBIN v1` header, the 24-entry section directory
   (exact section IDs `1..=24`, element sizes, 8-byte alignments, and in-file bounds), and the
   64-byte `StaMeta` structure invariants (`store_mode`, `precision`, `num_corners`, `num_nodes`,
   `NODE_REC` length, `EP_SLACK` length, `EP_ORDER` length, and `PATH_CHAIN_PTR` length) in `O(1)`
   time without hashing the file payload (`WS-REQ-0036`).
2. **Full payload verification (`StaStore::verify_hash`, `StaStore::open_verified_path`,
   `StaStore::open_for_mutation_input`)**: recomputes `xxh3-64` over `[header_len .. EOF)` and
   verifies that it matches the stored checksum (`WS-REQ-0035`, `WS-REQ-0036`). Commands that
   consume `sta.rbin` as a mutation input (`ruststa check`) or verify a revision (`rusteda verify`)
   call this path.

## 2. Section directory (`1..=24`)

Every `STA v2` file contains the 24 sections below in ascending section ID order:

| ID | Name | `elem_size` | Description |
|---:|---|---:|---|
| 1 | `STR_BYTES` | 1 | Concatenated UTF-8 string bytes (string `0` is `""`). |
| 2 | `STR_OFFS` | 4 | `u32` byte offsets into `STR_BYTES` (`N_strings + 1` sentinel entries). |
| 3 | `META` | 64 | Single `StaMeta` record: counts, `store_mode`, `precision`, `num_top_k_paths`, `backend_str`. |
| 4 | `CORNERS` | 16 | `CornerRec` per operating corner (`name_str`, `lib_count`, `flags`). |
| 5 | `CLOCKS` | 48 | `ClockRec` per constraint clock (`period_ns`, waveform edges, setup/hold uncertainty). |
| 6 | `TAGS` | 16 | `TagRec` per launch tag (`clock_id`, `launch_edge`, `exc_state`, `launch_source_node`, `path_group_str`). |
| 7 | `DISABLED_ARCS` | 16 | `DisabledArcRec` per disabled timing arc (`arc_idx`, `src_node`, `dst_node`, `reason`). |
| 8 | `NODE_REC` | 152 | Per-`(corner, node)` `NodeRec` indexed by `corner_idx * num_nodes + node_id` (`Full` mode; empty in `Summary` mode). |
| 9 | `TAG_EXTRA_PTR` | 4 | CSR row pointer `(num_corners * num_nodes + 1) * u32` into `TAG_EXTRA` (`Full` mode; empty in `Summary` mode). |
| 10 | `TAG_EXTRA` | 56 | `TagExtraRec` for active launch tags beyond inline primary tag (`Full` mode; empty in `Summary` mode). |
| 11 | `ENDPOINTS` | 36 | `EndpointRec` per checked timing endpoint (`data_node`, `clock_node`, `capture_clock`, `path_group_str`, `check_arc_idx`, `kind_flags`, `worst_setup_rank`, `worst_hold_rank`, `name_bucket_plus1`). |
| 12 | `EP_SLACK` | 16 | `EpSlackRec` (`setup_slack`, `hold_slack`) indexed by `corner_idx * num_endpoints + ep_idx`. |
| 13 | `EP_ORDER` | 4 | Pre-sorted `u32` endpoint indices per `(corner_idx, is_hold)` bucket (`ascending slack`, ties by `endpoint_idx`). |
| 14 | `PATH_RANK` | 12 | `PathRankRec` (`slack_ns: f32`, `endpoint_idx: u32`, `corner_idx: u16`, `flags: u8`, `pad: u8`) for up to `path_limit` worst paths. |
| 15 | `PATH_CHAIN_PTR` | 8 | CSR row pointer `(num_ranked_paths + 1) * u64` into `PATH_CHAIN`. |
| 16 | `PATH_CHAIN` | 4 | `u32` words storing 4 header words (`required`, `check_value`) followed by three words `[node, step_word, name_bucket]` per step. |
| 17 | `PATHS` | 56 | `PathHeaderRec` for the top-`K` materialized worst paths (`K = num_top_k_paths`, default `100`). |
| 18 | `PATH_POINTS` | 32 | `PathPointRec` slices referenced by `PathHeaderRec.{point_begin, point_end}`. |
| 19 | `NAME_HASH` | 4 | Open-addressed `xxh3_64` hash table (`4 * u32` per bucket) mapping port names and `"inst/pin"` names to `(NodeId, prefix_str, leaf_str)`. |
| 20 | `INST_HASH` | 4 | Open-addressed `xxh3_64` hash table (`4 * u32` per bucket) mapping instance names to `(inst_str, cell_str, pin_node_begin, pin_count)`. |
| 21 | `SUMMARY` | 80 | `SummaryRec` per `(corner, path_group)` with `setup_wns`, `setup_tns`, `setup_violating`, `hold_wns`, `hold_tns`, `hold_violating`, DRV counts, and endpoint counts. |
| 22 | `UNCONSTRAINED` | 12 | `UnconstrainedRec` per unconstrained endpoint (`node`, `reason`, `reserved` name bucket), sorted by `(node, reason)`. |
| 23 | `NOTES` | 16 | `NoteRec` (`code_str`, `message_str`, `severity`, `count`) for aggregated diagnostics and notes. |
| 24 | `SCOPE` | 1 | Canonical JSON bytes of `ScopeReport` (top module, corners, wire/clock/CPPR models, backend, precision, `store_mode`, exceptions, and `CompletenessReport`). |

## 3. Record layouts and canonicalization

- **`NodeRec` (152 bytes)**:
  - `slew` (`[[f64; 2]; 2]` for `[Rise/Fall][Early/Late]`, 32B), `required` (`[[f64; 2]; 2]`, 32B),
    and `slack` (`[[f64; 2]; 2]`, 32B).
  - Primary tag `tag` (`u32`, 4B), `pad` (`u32` storing the node's `NAME_HASH` bucket index for
    `O(1)` reverse name resolution), `arrival` (`[[f64; 2]; 2]`, 32B), and `pred` (`[[u32; 2]; 2]`,
    16B).
- **`TagExtraRec` (56 bytes)**: stores `tag` (`u32`), `pad` (`u32`), `arrival` (`[[f64; 2]; 2]`),
  and `pred` (`[[u32; 2]; 2]`) for each active launch tag beyond the inline primary tag.
- **Sentinels and `-0.0` canonicalization (`STA-REQ-0302`, `STA-REQ-0405`)**:
  - Every floating-point field (`f64` and `f32`) passes through `canon_f64` / `canon_f32`,
    converting `-0.0` (`-0.0f64.to_bits() == 0x8000_0000_0000_0000`) to `+0.0` (`0x0000_0000_0000_0000`)
    and canonicalizing quiet `NaN` payloads.
  - Absent early arrivals use `+inf` (`f64::INFINITY`), absent late arrivals use `-inf`
    (`f64::NEG_INFINITY`), absent required times use `-inf` (early) and `+inf` (late), and absent
    slacks use `+inf` (or `NaN` in `EpSlackRec`).
  - Determinism: parallel construction (`rayon`) sorts all candidate paths, endpoints, disabled
    arcs, and unconstrained endpoints by canonical deterministic keys, producing byte-for-byte
    identical `sta.rbin` images across `--threads 1` and `--threads 32`.

- **`EndpointRec` (36 bytes)**: `kind_flags` bits `0..8` hold the `EndpointKindTag` and bits `8..10`
  the capture clock edge (`0 = Rise`, `1 = Fall`, `2 = None`); bits `10..32` are zero.
  `name_bucket_plus1` is the `NAME_HASH` bucket of `data_node` plus one, or `0` when the node has
  no bucket.
- **`PathRankRec` (12 bytes)**: `flags` bit `0` is the hold flag, bit `1` the endpoint transition
  (`0 = Rise`, `1 = Fall`), bits `2..4` the check kind (`0` setup, `1` hold, `2` recovery, `3`
  removal), and bit `7` marks a top-`K` path.
- **`PATH_CHAIN` step (3 words)**: `node` is the full `u32` node id; `step_word` bit `0` is the
  transition, bits `1..3` the `PathPointKind` and bits `3..32` the tag slot (`0` = primary
  `NodeRec.tag`, `k` = `TAG_EXTRA` offset `k - 1`); `name_bucket` is the node's `NAME_HASH` bucket.

## 4. Storage modes (`Full` vs `Summary`)

- **`StoreMode::Full` (`--store-mode full`, default, `WS-REQ-0034`)**: populates all 24 sections,
  including `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA`. Supports all mapped queries, arbitrary-`N`
  path reconstruction up to `path_limit`, `inspect_pin`, `inspect_instance`, and incremental
  seeding (`STA-REQ-0315`).
- **`StoreMode::Summary` (`--store-mode summary`, `WS-REQ-0035`)**: omits `NODE_REC`,
  `TAG_EXTRA_PTR`, and `TAG_EXTRA` (stored as 0-length sections) while retaining `SUMMARY`,
  `ENDPOINTS`, `EP_SLACK`, `EP_ORDER`, `PATH_RANK`, `PATH_CHAIN_PTR`, `PATH_CHAIN`, top-`K` `PATHS`
  and `PATH_POINTS`, `NAME_HASH`, `INST_HASH`, `UNCONSTRAINED`, `NOTES`, and `SCOPE`. Calling
  `require_full_mode_for_incremental()`, `inspect_instance()`, or `inspect_pin()` on a non-endpoint
  node in a `Summary` store returns `StaStoreError::SummaryModeUnavailable` (`INPUT_REJECTED`).

## 5. Zero-copy mapped queries

`StaStore` serves `ruststa query` and `rusteda inspect` directly from mapped sections (`WS-REQ-0030`
to `WS-REQ-0038`):

- `query_summary()` / `query_scope()`: reads `SUMMARY` (80B records), `META`, `CORNERS`, and
  `SCOPE` without touching `NODE_REC`, `PATHS`, or `PATH_POINTS`.
- `endpoint_table(corner_idx)` / `query_endpoints(corner_idx, check_kind, limit)`: returns the
  compact `<= 22`-byte `CompactEndpointEntry` table in ascending `NodeId` order (`STA-REQ-0313`) or
  slices `EP_ORDER` in `O(limit)`.
- `query_path_handle(rank_idx)` / `reconstruct_path_from_chain(rank_idx)`:
  - For `rank_idx < num_top_k_paths`, reads `PATHS[rank_idx]` and its slice in `PATH_POINTS`.
  - For `num_top_k_paths <= rank_idx < num_ranked_paths` in `Full` mode, walks `PATH_CHAIN` in two
    passes: Pass 1 batch-touches each step's `NodeRec` to prefetch cache lines; Pass 2 reads
    arrivals and slews from `NODE_REC` / `TAG_EXTRA` and computes step delays (`STA-REQ-0314`).
- `inspect_pin(pin_name, corner_idx)` / `inspect_port(port_name, corner_idx)`: probes `NAME_HASH`
  (`xxh3_64` over `"inst/pin"` on a 256-byte stack buffer without heap allocation) and reads
  `NODE_REC` + `TAG_EXTRA` in `O(1)` (`STA-REQ-0315`).
- `inspect_instance(inst_name, corner_idx)`: probes `INST_HASH` and reads the contiguous slice of
  `NODE_REC` for all pins of the instance in `O(pins_on_inst)`.

## 6. Capacity limits (`T0022`)

Every index of the table is stored at its full width, and an analysis that a field cannot hold is
rejected before the table is built, with error `T0022` (`INPUT_REJECTED`), never packed truncated
(AGENTS rule 9.2):

| Limit | Field |
|---|---|
| `num_endpoints <= 4_294_967_294` | `PathRankRec.endpoint_idx`, `EP_ORDER` (`u32`, `u32::MAX` is `NO_INDEX`). |
| `num_corners <= 16` | `PathRankRec.corner_idx` and the per-corner sections. |
| string bytes `<= 4_294_967_295` and strings `< 4_294_967_295` | `STR_OFFS` (`u32`) and every `*_str` id. |
| cell-name string id `<= 1_048_575` | `INST_HASH` bucket word 2, bits `0..20`. |
| pins per instance `<= 4_095` | `INST_HASH` bucket word 2, bits `20..32`. |
| tag slot `<= 536_870_911` | `PATH_CHAIN` `step_word` bits `3..32` (the tag table holds at most `4096` tags). |

The string bound counts every name the builder can add (ports, instances, cells, cell pins,
corners, clocks, path groups and diagnostics), so a table that passes the check cannot overflow.
`PATH_CHAIN_PTR` is `u64`, so the path chains have no 4-GiB word limit. Format version `1` packed
endpoint indices into 20 bits, node ids into 24 bits, endpoint name buckets into 22 bits and chain
pointers into `u32`; a design with more than `1_048_575` endpoints (OpenC910 has `4_444_505`) could
not be stored.

## 7. Workspace revision staging

`StaStoreData::stage_to_txn(txn, upstream)` writes `sta.rbin` into a workspace transaction (`Txn`)
alongside `upstream` artifact hashes and populates `summary.sta` (`setup_wns_ns`, `setup_tns_ns`,
`setup_violating_endpoints`, `hold_wns_ns`, `hold_tns_ns`, `hold_violating_endpoints`,
`max_trans_violating`, `max_cap_violating`, `total_endpoints`, `unconstrained_endpoints`,
`corners`) and `status.sta` (`timing = "met" | "violated"`) (`WS-REQ-0038`, `WS-REQ-0045`).
