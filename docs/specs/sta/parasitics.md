# Extracted RC timing

The detailed RC subset of IEEE 1481 SPEF uses an explicit extraction corner,
grounded coupling (factor 1), physical pin capacitances and bounded tree
reductions; version 0.4.3 solves resistor loops on the nodal conductance system and
joins components without a resistor path to their driver (STA-REQ-0730 to STA-REQ-0732). Version 0.3 replaces the original lumped/first-moment-only timing
interpretation with [effective-load and threshold/slew models](rc-waveforms.md).
No crosstalk, CCS or propagated-clock signoff claim is made. Internal units are
ns, pF and kOhm. Version 0.4 binds one source per corner in a `PARASIT` v2 table
(RCLI-REQ-0075, STA-REQ-0820); version 1 tables remain readable.

**STA-REQ-0700**: `rusteda import --spef FILE --spef-corner NAME` atomically
  commits a `PARASIT` v1 mmap container, source identity and parser budgets.
  It requires a netlist and libraries for that corner. All mutations retain
  workspace locking and `--base-rev` conflict checks. Changing the netlist drops
  its parasitics; changing parasitics invalidates timing and check tables.
**STA-REQ-0701**: Parse detailed `D_NET`, name maps, escaped names, port/pin
  connections, ground/coupling capacitances and resistors. Convert explicit units
  to engine units. Reject unknown semantic sections, non-finite/negative values,
  value triplets, duplicate records, unresolved names and malformed structure.
  Coordinate, date, vendor and driving-cell metadata do not affect this model.
  A connection load attribute is rejected to avoid double counting Liberty pins.
**STA-REQ-0702**: Ground each inter-net coupling capacitor once on the local
  endpoint in each net's section, report this approximation, and verify declared
  total capacitance against the sum within max(1e-8 pF, 1e-4 relative).
  Reject same-net coupling, inductance and reduced-net sections.
**STA-REQ-0732**: Bind every SPEF connection to the imported design and verify
  its direction and net (replaces STA-REQ-0703). Require complete annotation of
  driven, loaded signal nets and exactly one driver. Contract zero-resistance
  edges; solve resistor loops (STA-REQ-0730) and join components without a
  resistor path to the driver (STA-REQ-0731) instead of rejecting the net. Never
  substitute ideal or estimated wires for missing extracted data, and never drop
  a connection or a capacitance of the net.
**STA-REQ-0704**: Add transition/bound-specific pin and port capacitances at
  physical RC nodes. Compute first moments by iterative subtree/postorder and
  preorder reduction, then evaluate the current RC waveform model. Report
  `wire_model=spef_rc`, `parasitics=spef`, `delay_calculator=nldm_ceff_ramp`
  and `coupling=grounded_factor_1`. Report physical clock source identities;
  a flow may refuse ideal-clock results for sequential timing.
**STA-REQ-0705**: Enforce configurable input, net, node, element and name-byte
  caps before allocation. Tree passes are O(V+E) after bounded canonical sorting and zero-R union-find; no paths are enumerated, and the
  RC state is included in the corner state budget. Arithmetic overflow and
  non-finite RC products are structured input errors. Share parsed immutable
  parasitics across analysis corners using `Arc`.
**STA-REQ-0706**: Full and incremental analyses use identical parasitic data;
  changed parasitics force a full update. An independent-checker mode without
  RC support returns undecided. Full-recompute checks and an external black-box
  benchmark on both baseline PDKs gate acceptance of extracted timing.

**STA-REQ-0707**: Path reports count the final net hop into an output port
  as wire delay and label it `net`. Its port handle remains a port handle. The
  input port seed is not a wire arc. Cell plus wire delay equals the path arrival
  minus its first data point arrival (within floating-point rounding).

**STA-REQ-0708**: Before RC indexing, validate typed parasitic objects and
  derive memory bounds from actual vectors. Reject malformed connection/resistor
  indices or nonfinite capacitances with a diagnostic. Guard zero-resistance
  component reductions and subtree sums against floating-point overflow.

**STA-REQ-0730**: Resistor loops. After zero-resistance contraction, a
  breadth-first spanning tree of each net from its driver node decides the
  topology. A net without further resistors is reduced by the tree passes of
  STA-REQ-0704 and STA-REQ-0720, bit for bit as before. Every further resistor
  between two distinct contracted nodes closes one independent loop (two parallel
  resistors are a loop). For `k` loops the first moments solve the nodal
  conductance system `G tau = C` with the driver node grounded, in f64, for the
  four transition/bound capacitance vectors: `G = T + A D A^T` with `T` the
  matrix of the spanning tree, `A` the incidence and `D` the conductances of the
  loop-closing resistors. `T^-1` is one postorder and one preorder pass; by the
  Woodbury identity the rest is the `k x k` loop resistance matrix
  `D^-1 + A^T T^-1 A`, symmetric positive definite, factored once by Cholesky
  [@woodbury1950inverting; @golub2013matrix, sec. 4.2].
  Each solution is checked against Kirchhoff's current law at every node and
  refined at most twice; a residual above 1e-9 of the net's total capacitance,
  a non-positive pivot or a negative moment beyond 1e-9 of the largest is error
  `T0070`. The driving-point admittance magnitudes `A = sum C`,
  `B = sum C*tau`, `C = sum C*tau^2` (on a tree they equal the recursion of
  STA-REQ-0720) give the pi of STA-REQ-0720, and the sinks' first moments enter
  STA-REQ-0729 unchanged. A resistor whose two ends contract to one node carries
  no current and has no effect. The scratch of one net, `8 (k^2 + k + 5 n)`
  bytes for `n` RC nodes, must fit in the state budget left after the binding
  estimate, and its work `4 m (k + 24) + k^3 + 32 k^2` for `m` contracted nodes
  in eight units per permitted state byte (`--max-state-gib`); otherwise
  `T0070` names the net, its loops and nodes. No loop, cycle or path is
  enumerated. Note `T0152`, once per corner, counts the loops, their nets and the
  shorted resistors and names the first net. Against operating points of an
  external nodal simulator on planted networks with loops, the bound first
  moments and pi agree to a relative 3e-15, and a transient of the STA-REQ-0728
  driver into the full network agrees with the pi and single-pole model to
  0.08 ps, as on a tree; behind the default PDKs' `buf_1` (transistor-level
  transients) the wire delays of planted loops are within 0.2 ps of the circuit
  (0.26 ps on the same network without its loops). A reference timing tool, observed black-box, times such a network as
  a tree without some of its resistors (wire delay errors of 4 to 29 ps on the
  same planted loops); it agrees with this rule to 0.0001 ps on an extracted net
  whose only loop is a pair of parallel 0.12 Ohm resistors.
**STA-REQ-0731**: Components without a resistor path to the driver. A
  connected component of a net's RC network (resistors of any value) that does
  not contain the driver node is joined to the driver node with zero
  resistance. Its ground and grounded coupling capacitance and the pin and port
  capacitance of its connections load the driver at the root of its pi, and its
  sinks have a zero first moment: no wire delay, the driver pin's waveform. The
  connection section declares those pins part of the net while the extraction
  gives no resistance to them, so zero is a lower bound of their wire delay;
  nothing is dropped. Warning `T0151`, once per corner, counts the nets,
  components, RC nodes, pins and extracted capacitance and names the first net
  with up to eight members. Observed: a reference extractor left the via node of
  one sink pin unconnected on a routed sky130 net; a reference timing tool,
  black-box, also gives such a sink zero wire delay (it agrees with this rule to
  0.0001 ps on that net) but removes the component's capacitance, including the
  sink's pin capacitance, from the driver's load, which this rule keeps. The
  suite's own extraction of the same net gives that sink 0.83 ps of wire delay.

Default parser caps: 32 MiB source, 100,000 nets, 500,000 RC nodes,
1,000,000 RC elements, 16 MiB interned names. Absolute caps are eight times
these values. The importer records the chosen caps; reopening cannot remove
them. Names have a 4096-byte individual cap. No recursion is needed.

Memory preflight: two source copies (64 MiB), name strings/index (<64 MiB),
node/index records (80 MiB), elements (64 MiB), CSR/parent/postorder and four
capacitance/delay components (<80 MiB): <512 MiB parasitic overhead at defaults.
Add Liberty/STA state and retained corner records separately. Builds use two
jobs; tests two threads. Tiny one-corner dual-PDK scenarios have <1000 timing
pins and <10000 RC nodes (<4 MiB RC state); guarded PDK process ceiling 8 GiB,
standard tests 2 GiB, wall deadline 180 seconds, measured with time -v.
A net with `k` resistor loops adds `8 (k^2 + k + 5 n)` bytes of scratch while
it is reduced (STA-REQ-0730); one loop over 10,000 nodes adds 0.4 MiB, 1,000
loops 8 MiB. Only one net is reduced at a time.

Reference: IEEE 1481-1999, Standard for Integrated Circuit (IC) Delay and Power
Calculation System, detailed parasitic exchange syntax; W. C. Elmore,
The Transient Response of Damped Linear Networks with Particular Regard to
Wideband Amplifiers, Journal of Applied Physics 19, 55–63 (1948).
