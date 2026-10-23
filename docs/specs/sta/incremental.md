# Incremental timing (`TimingSession` and `update_revision`)

Incremental static timing analysis (`crates/engines/ruststa/src/engine/incr`) updates timing state
after gate-level netlist mutations or across workspace revisions without re-evaluating unchanged
regions of the timing graph. Two execution surfaces share a common forward and backward cone
propagation engine (`cone.rs`):

- `TimingSession` (`session.rs`): a live in-memory session over a mutable
  `rusteda_core::netlist::GateNetlist` (`STA-REQ-0450` to `STA-REQ-0455`).
- `update_revision` (`update.rs`): a cross-revision workspace update (`STA-REQ-0456` to
  `STA-REQ-0458`) reading an ancestor revision's `sta.rbin` (`STA v2` in [store.md](store.md)) and
  committing an updated child revision.

## 1. Live in-memory incremental session (`TimingSession`)

**STA-REQ-0450** Session state and lifecycle. `TimingSession` owns a mutable `GateNetlist`, the
per-corner Liberty libraries (`Library`) and compiled library views (`TimingLib`), the parsed SDC
constraints (`Sdc`), the current levelized `TimingGraph`, per-corner lowered problems
(`LoweredCorner`), per-node forward values (`TimingValues`), per-node and per-tag backward required
times and slacks (`CornerBackwardState`), endpoint records and corner summaries (`CornerEndpoints`),
ranked path tables (`PathTable` with 1-based handles `path1`, `path2`, …), and the `StaStoreData`
tables. Initial construction (`TimingSession::new`, `TimingSession::from_sdc`,
`TimingSession::new_multi_corner`, `TimingSession::new_multi_corner_sdc_str`) pre-interns all
library cell definitions into the `GateNetlist` and runs an initial full propagation (`Scope::All`).

**STA-REQ-0451** Supported netlist edit categories and batch coalescing. `TimingSession` supports
six netlist mutation categories, both individually and coalesced in a single batch call
(`TimingSession::apply_edits(&[SessionEdit])`):

1. Cell swap / resize (`swap_cell`, `swap_cell_by_name`, `SessionEdit::SwapCell`).
2. Buffer insertion (`insert_buffer`, `SessionEdit::InsertBuffer`) splitting a subset of a net's
   load pins onto a newly created buffer instance and output net.
3. Buffer removal (`remove_buffer`, `SessionEdit::RemoveBuffer`) merging a buffer's output loads
   back onto its input net and removing the buffer instance and output net.
4. Pin connect, disconnect, and move (`connect_pin`, `disconnect_pin`, `move_pin`,
   `SessionEdit::ConnectPin`, `SessionEdit::DisconnectPin`, `SessionEdit::MovePin`).
5. Instance and net addition and removal (`add_instance`, `remove_instance`, `add_net`,
   `remove_net`, `SessionEdit::AddInstance`, `SessionEdit::RemoveInstance`, `SessionEdit::AddNet`,
   `SessionEdit::RemoveNet`).
6. Batch edits (`apply_edits`) coalescing all dirty instances, nets, and pins across the batch into
   a single graph rebuild, constraint compilation, forward cone pass, and backward cone pass.

**STA-REQ-0452** Canonical node correspondence and arena slot safety. Across any sequence of
`GateNetlist` edits, `session_node_keys` assigns each `NodeId` (`0..num_nodes`) in `GateNetlist::freeze`
order a `SessionNodeKey`: `SessionNodeKey::PortBit(pb)` for top-level port bits `0..num_port_bits`
and `SessionNodeKey::Pin(PinId)` for connected instance pins in `(inst_id_index_order,
inst.connected_order)`. Because `PinId` carries a generation counter, removing a buffer or instance
and subsequently inserting a new buffer or instance that reuses the same arena slot index produces a
distinct `PinId` and never aliases stale timing state from the deleted pin. Similarly, disconnecting
and reconnecting a pin appends the pin to `inst.connected`, and `NodeMapping::from_session_keys`
tracks the resulting `NodeId` permutation bijectively (`new_to_old`, `old_to_new`).

**STA-REQ-0453** Forward cone seeding and level-ordered propagation (`Scope::Cone`). For each
corner, `propagate_forward_cone` maps the previous `TimingValues` (`arr`, `slew`, `pred`) from old
`NodeId` coordinates to new level-ordered coordinates for every preserved node, and initializes new
or unmapped nodes to absent sentinels (`+inf` early, `-inf` late, `NO_PRED`). A level-ordered node
`new_pos` is enqueued as a forward cone seed if and only if:

- it is marked `directly_dirty` by the edit tracker (a pin of a modified instance or a driver/load
  of a modified net), or has no corresponding `old_node`;
- its capacitive load `load[new_pos]` differs bitwise (`to_bits() != to_bits()`) from
  `old_lowered.load[old_pos]` (capturing driver delay/slew sensitivity when a downstream load pin is
  resized, added, or removed);
- its `StartRow` values differ bitwise from `old_lowered.starts`;
- its enabled incoming CSR arc list `(mapped_old_src_node, ArcClass, TagMapRec_slice, WireRec)`
  differs from `old_lowered`; or
- any predecessor `pred` recorded in its carried `TimingValues` is no longer a valid incoming
  `src_pos` in `new_lowered.csr`.

The seed list is passed to `rusteda_compute::timing::Request` with `Scope::Cone { seeds }`
(COMPUTE-REQ-0018), which propagates level-by-level in `LevelizedTimingCsr` order, compares updated
`arr`, `slew`, and `pred` bitwise (`to_bits() == to_bits()`), and enqueues fan-out targets only when
a node's values change bitwise.

**STA-REQ-0454** Backward required-time cone propagation. After forward cone propagation and
endpoint check evaluation (`evaluate_all_checks`), `propagate_backward_cone` seeds per-tag required
times `node_req[pos * 4 * T + (t * 2 + tr) * 2 + an]` from all check records (`Setup`, `Hold`,
`Recovery`, `Removal`, `ClockGating`, `LatchSetup`, `LatchHold`, `OutputDelay`, `MaxDelay`,
`MinDelay`, and `LatchBorrow` bounds). In reverse topological level order, a node `u` is re-evaluated
if its endpoint seed changed, its outgoing CSR arc list or capacitive load changed, its forward
arrival or transition changed (`changed_pos`), or any fan-out target `v` changed its required time
`node_req[v]` bitwise. When every fan-out target `v` of `u` is unchanged, `u` and its incoming arcs
are unchanged, and `u` did not change in the forward pass, `u` reuses its carried `node_req` row in
`O(1)` time without evaluating outgoing LUTs. Per-pin worst `pin_required` and `pin_slack` are then
updated from the converged `node_req` and `node_values`.

**STA-REQ-0455** Bitwise equivalence to full analysis (`verify_against_full`). After any single edit
or batch of edits, the incremental state in `TimingSession` is bitwise identical
(`to_bits() == to_bits()`, with `-0.0` canonicalized to `+0.0`) to a fresh full analysis
(`Scope::All`) on `netlist.freeze()` across every corner for:

- per-node, per-tag arrivals `arr[node][tag][tr][an]` and predecessors `pred[node][tag][tr][an]`;
- per-node transitions `slew[node][tr][an]`, required times `pin_required[node][tr][an]`, and slacks
  `pin_slack[node][tr][an]`;
- per-node, per-tag backward required times `backward.required(node, tag, tr, an)`;
- corner and multi-corner summaries (`wns_setup_ns`, `tns_setup_ns`, `wns_hold_ns`, `tns_hold_ns`,
  violating and unconstrained endpoint counts);
- ranked path handles (`path1`, `path2`, …) and their startpoint, endpoint, arrival, required, and
  slack values; and
- all 24 sections of `StaStoreData` (`sta.rbin`).

## 2. Cross-revision workspace update (`update_revision`)

**STA-REQ-0456** Revision update mode selection (`UpToDate`, `Incremental`, `Full`). Given a target
workspace revision (`HEAD` when `None`) and `UpdateOptions`, `update_revision` selects one of three
modes:

1. `UpdateMode::UpToDate`: selected when `!options.force_full`, the target revision already contains
   a valid `sta.rbin` table (`TABLE_NAME = "sta"`), and its recorded `Upstream` hashes match the
   revision's current `netlist`, `libraries`, `constraints`, and `parasitics` hashes. No new
   revision is committed (`committed_rev == base_rev`).
2. `UpdateMode::Incremental`: selected when `!options.force_full` and the nearest ancestor revision
   carrying `sta.rbin` in the parent chain has `StoreMode::Full`, identical `libraries` (hash and
   corners), identical `constraints` hash, identical `parasitics` hash, identical top-level port
   definitions (`!diff.ports_changed`), and both `instance_dirty_fraction <= max_dirty_fraction` and
   `net_dirty_fraction <= max_dirty_fraction` (default `DEFAULT_MAX_DIRTY_FRACTION = 0.25`).
3. `UpdateMode::Full(FullUpdateReason)`: selected otherwise, recording the exact fallback reason
   (`Forced`, `NoAncestorSta`, `AncestorSummaryOnly`, `ConstraintsChanged`, `LibrariesChanged`,
   `ParasiticsChanged`, `PortsChanged`, or `DirtyFractionExceeded`).

When `mode != UpdateMode::UpToDate`, `update_revision` stages the updated `sta.rbin` with the target
revision's upstream hashes and commits a new child revision.

**STA-REQ-0457** Cross-revision `FrozenNetlist` diff and `sta.rbin` state reconstruction.
`diff_frozen_netlists(old, new)` compares two `FrozenNetlist`s in canonical order:

- Top-level ports are compared by count, bit count, name, direction, and bus range.
- Nets are keyed by `FrozenNetKey` (`Named(name)` for named nets, or driver identity
  `AnonDrivenByPin { inst, pin }` / `AnonDrivenByPortBit(pb)` for anonymous nets created by buffer
  insertion) so anonymous nets across revisions do not cause false diffs.
- Each net's signature includes its sorted connected `EndpointRef`s `(inst_name, cell_name,
  pin_name)` and `PortBit(pb)`, so resizing a load cell or rewiring a pin marks the incident net and
  all its connected pins dirty.
- `NodeMapping::from_frozen_names` matches port bits `0..n_port_bits` and connected instance pins by
  `(inst_name, pin_name)`.
- `reconstruct_timing_values_from_store` reconstructs `TimingValues` (`arr`, `slew`, `pred`) in
  `NodeId` order for each corner from the ancestor's `sta.rbin` (`NODE_REC` inline primary tag plus
  `TAG_EXTRA_PTR` / `TAG_EXTRA` overflow tags) and feeds `propagate_forward_cone`.

**STA-REQ-0458** Self-checking `--verify-full` mode (`UpdateOptions::verify_full`). When
`options.verify_full` is `true` during an `UpdateMode::Incremental` update, `update_revision` also
executes a fresh full propagation (`Scope::All`) on the target revision's `FrozenNetlist` and
compares every `NodeRec` (`slew`, `required`, `slack`, `arrival`, `pred`), `StaSummary`, and all
`StaStoreData` sections bitwise (`to_bits() == to_bits()`). Any discrepancy returns
`UpdateError::BitwiseMismatch(BitwiseMismatch)` before committing.
