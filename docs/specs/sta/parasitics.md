# Extracted RC timing

The detailed RC subset of IEEE 1481 SPEF uses an explicit extraction corner,
grounded coupling (factor 1), physical pin capacitances and bounded tree
reductions. Version 0.3 replaces the original lumped/first-moment-only timing
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
**STA-REQ-0703**: Bind every SPEF connection to the imported design and verify
  its direction and net. Require complete annotation of driven, loaded signal
  nets and exactly one driver. Reject cycles and disconnected RC nodes, with
  zero-resistance edges contracted before tree checks. Never substitute ideal
  or estimated wires for missing extracted data.
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

Reference: IEEE 1481-1999, Standard for Integrated Circuit (IC) Delay and Power
Calculation System, detailed parasitic exchange syntax; W. C. Elmore,
The Transient Response of Damped Linear Networks with Particular Regard to
Wideband Amplifiers, Journal of Applied Physics 19, 55–63 (1948).
