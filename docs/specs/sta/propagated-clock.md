# Propagated primary clocks

**STA-REQ-0710**: Compile explicit `set_propagated_clock` and
  `unset_propagated_clock` on clock collections in command order. Default clocks
  remain ideal; `is_propagated` queries reflect the selection. Generated-clock
  commands remain unsupported. Report `propagated` when all physical clocks use
  propagated timing, `mixed` when only some do; virtual clocks retain ideal time.
**STA-REQ-0711**: Propagate each selected primary-port clock independently,
  using two source-edge tags and the canonical CPU f64 timing CSR, NLDM loads and
  active RC records. Seed source latency and `set_input_transition` (default zero),
  then use actual network arrival and slew at launch/capture pins. Propagation
  replaces ideal network latency and clock transition. Emit T0076 when such
  nonzero ideal annotations are replaced. Source latency overrides are retained.
  CPPR is explicitly off. The same reference clock results feed CPU/CUDA data
  propagation. No clock paths are enumerated.
**STA-REQ-0712**: Reject non-unate/reconvergent inversion clocks, clock loops,
  clock propagation through a transparent latch data arc, internal clock sources
  and clock-source driving-cell models with T0075. They must not silently use
  ideal clocks. Independent verification without this feature returns undecided.
**STA-REQ-0713**: Preflight combined clock/data/RC state and work. Retained
  clock arrival/slew is <=128*N_nodes*N_propagated_clocks bytes and Arc-shared
  across session snapshots; discard clock predecessors. One two-tag clock
  propagation at a time costs <=320*N_nodes + 64*N_arcs transient bytes.
  Bound visits by `N_clocks*(N_nodes+N_arcs) <= max_state_bytes/8` and domains by
  the existing tag budget. Add retained clock state to regular data-state checks.
  CPU reference, full/incremental consistency, native CUDA f64 consistency and
  external dual-PDK propagated-clock timing comparisons gate acceptance.
**STA-REQ-0714**: The transition of a pin of a propagated clock network merges
  every arc into it as in the data propagation (STA-REQ-0117), including the
  outputs of sequential cells whose launched transitions reach the network (a
  flop output into a clock gate or a clock multiplexer select). The
  propagation seeds those outputs, without arrivals, with the transitions of
  their clock-to-output arcs at the transitions that the previous propagation
  gives their clock pins, and repeats until the seeded transitions no longer
  change: with k such launches in series it runs k + 1 times, never more than
  one plus the number of such launches. Each repetition counts against the work
  bound of STA-REQ-0713 (T0023). Without such launches it runs once.

The clock arrival is relative to its source waveform edge, as in ideal clocks;
waveform/period relations are applied by the existing relation table, once.
For a source edge e and a pin transition t, the clock arrival is source latency
plus propagated cell and wire delay, not a second sum of ideal network latency.
I/O delay references retain source latency; propagated network latency is not an
ideal insertion-delay annotation. Clock launch path points show propagated
insertion delay, while data wire-delay totals exclude the input seed.

For a one-clock XOR+FF fixture (<32 timing nodes, <100 RC nodes, one corner),
clock state is <64 KiB. General retained state is 96 bytes/node/selected clock;
the conservative 128-byte allowance includes indexing. Full/corner state is
160 bytes/node/data tag, plus clock, CSR and RC allocations. Work is
O(clocks*(V+E)) on the guarded DAG. Tests use two workers, 2 GiB cgroup ceilings,
180-second deadlines and time-v RSS receipts; PDK scenarios run sequentially.
The clock model and RC first moments follow the definitions of interconnect and
clock timing in [@sapatnekar2004timing] and [@bhasker2009static].
