# RC waveform acceptance

Version 0.3 uses a three-moment passive pi, a charge-matched finite-ramp
Thevenin driver and per-sink threshold/slew propagation; version 0.4.2 takes the
effective capacitance from one Newton-Raphson correction of that driver and drives
the wires of a resistive pi with the driver's own waveform. Its driver resistance
is explicitly estimated from local NLDM delay sensitivity. This parameter is
not a transistor DC characterization; CCS, crosstalk and signoff accuracy are
outside the validated scope. See [the equations and budgets](../specs/sta/rc-waveforms.md).

The former first-moment-only result remains in the version 0.2 report. The new
[HTML report](rc-report.html) and JSON receipts describe full nonzero-R timing.
The endpoint comparison threshold remains **0.001 ns**, with the reference
calculator's default settings and identical Liberty, SDC and SPEF files. No
zero-resistance projection replaces extracted timing in this acceptance.

## Independent electrical evidence

`crates/core/rusteda-compute/tests/rc_transient.rs` invokes Ngspice as an
external executable. Five four-node driver circuits and four single-pole
interconnect circuits run serially within their test; the two tests may run on
two workers. The simulator solves the circuit ODEs independently of the analytic
poles, charge integrals and threshold roots used by the implementation.

The driver cases verify charge residual at the end of the fitted active ramp,
the single-capacitance NLDM fit and actual pi-root delay/slew crossings. Wire
cases cover ideal-source steps, slow/fast ramps and non-default slew derate and
trip points. Time tolerance is 1e-6 ns; charge tolerance is 1e-8 pF. These tests
validate the declared circuit model, not transistor waveforms hidden by NLDM.
Extracted waveform scope requires explicit input-port transitions;
`set_driving_cell` in that scope is an explicit T0077 error.
They require an explicit simulator path and evidence directory; an absent tool
cannot produce a pass. External cases, logs and measurements remain local.

```sh
RUSTEDA_SPICE_SIMULATOR=/path/to/ngspice \
RUSTEDA_RC_ORACLE_WORKSPACE=work/driver-oracle \
RUSTEDA_RC_WIRE_WORKSPACE=work/wire-oracle \
RUST_TEST_THREADS=2 cargo test -p rusteda-compute --test rc_transient -- --ignored
```

The engine's driver rule (STA-REQ-0728, STA-REQ-0727) and the wires that follow its waveform
(STA-REQ-0729) have their own external cases in `crates/engines/ruststa/tests/rc_driver_spice.rs`:
two pi loads near a table breakpoint with the forward 1 fF resistance (root crossings and
transition of the one-step capacitance's source within 1e-6 ns; the end-of-ramp charge of the pi
below that of the corrected capacitance by less than the gap to the charge solution; a sink
buffered from the root behind one pole of its first moment within 1e-6 ns of
`rc::wire_from_driver`) and two pi loads below one thousandth of the driver resistance, where the
simulated pi and its lumped total give driver waveforms within 1e-4 of the transition while the
fitted waveform transition differs from the table transition by more than 1 %:

```sh
RUSTSTA_NGSPICE=/path/to/ngspice RUSTSTA_RC_DRIVER_WORKSPACE=work/engine-driver-oracle \
RUST_TEST_THREADS=2 cargo test -p ruststa --test rc_driver_spice -- --ignored
```

These circuits check the declared model. Its choice (one correction from the total load, wires
driven by the driver waveform) was checked against transistor-level transients of the default
PDKs' cells (Ngspice 45.2 with the PDK device models; sky130 `clkbuf_1` at tt/ss/ff, `buf_2` and
`inv_2` at tt; gf180 `clkbuf_1` at tt/ff, `buf_2` and `inv_1` at tt, `clkbuf_2` at ss). Each cell
is characterized by the same simulator into 48 lumped loads; the resulting one-dimensional tables
drive the models, so that only the model error remains. Thirty pi loads (2 to 50 fF, 30 Ohm to
3 kOhm) and 24 two-segment RC trees per cell and edge give (mean / maximum absolute error):

| Quantity | Charge solution + ramp wires (0.4.1) | One step + driver-waveform wires (0.4.2) |
| --- | --- | --- |
| Driver delay, pi loads | 3.06 / 45.0 ps | 1.76 / 33.5 ps |
| Driver delay, RC trees | 5.58 / 32.2 ps | 2.32 / 20.5 ps |
| Driver transition, pi loads | 20.5 / 204 ps | 17.8 / 174 ps |
| Sink arrival (driver + wire), RC trees | 6.50 / 39.6 ps | 0.64 / 9.0 ps |
| Sink transition, RC trees | 38.2 / 172 ps | 24.4 / 114 ps |

The largest remaining errors are at 3 kOhm, where a single linear Thevenin source is a coarse
model of a transistor driving a strongly shielded load. The harness and its receipts stay local
with the timing comparison.

Run binaries with an OS memory ceiling and timeout, as in the repository's
engineering rules. Both evidence directories must be fresh. Native core crates
have no simulator or Python runtime dependency.

## Networks that are not trees (0.4.3)

Extractions of routed designs contain RC networks that are not trees: two parallel resistors on
one pin, and the via node of a sink pin that no resistor connects to the rest of the net. Version
0.4.3 times them (STA-REQ-0730 to STA-REQ-0732) instead of rejecting the net.

`crates/engines/ruststa/tests/rc_mesh_spice.rs` binds five planted networks behind a buffer (a
tree, a loop between two branches, a loop through the driver node, a parallel resistor and a
ladder with two loops) and lets Ngspice solve them. Two operating points of the resistor network
with injected currents give every node's first and second moments; the bound sink moments and
the driving-point pi agree with them to a relative 3e-15. A transient of the finite-ramp
Thevenin driver into the full network agrees with the pi and single-pole model within 0.08 ps
(1.6e-4 of the sink arrival) on every network, as on the tree; the same networks without their
loop-closing resistors have first moments 19 to 87 % away.

```sh
RUSTSTA_NGSPICE=/path/to/ngspice RUSTSTA_RC_MESH_WORKSPACE=work/engine-mesh-oracle \
RUST_TEST_THREADS=2 cargo test -p ruststa --test rc_mesh_spice -- --ignored
```

The same planted networks behind `buf_1` of both default PDKs at tt were also simulated at
transistor level (Ngspice 45.2 with the PDK device models; the sinks carry their Liberty pin
capacitance) and timed by RustSTA and by OpenSTA 3.1.0 from the same Liberty, SDC and SPEF. The
cell characterization differs from the simulator by a constant per PDK (about -14 ps for sky130,
+91 ps for gf180) that cancels in the wire delay (sink arrival minus driver pin arrival) and in
the change that a loop makes to an arrival:

| Network | Wire delay error, RustSTA (sky130 / gf180) | Wire delay error, OpenSTA 3.1.0 |
| --- | --- | --- |
| tree | 0.26 / 0.16 ps | 0.25 / 0.16 ps |
| loop between branches | 0.19 / 0.12 ps | 13.1 / 14.7 ps |
| loop through the driver | 0.19 / 0.12 ps | 4.7 / 5.1 ps |
| parallel resistor | 0.14 / 0.09 ps | 4.0 / 4.6 ps |
| ladder with two loops | 0.12 / 0.07 ps | 26.5 / 29.0 ps |

OpenSTA 3.1.0 gives a network with a loop the delays of a tree without some of its resistors: the
parallel resistor and the loop through the driver leave its delays unchanged, and the loop
between branches raises the delays it should lower. The specification, checked against the
circuit, defines the behavior; the difference is a measured comparison (Rule 8.4). On the two extracted nets that 0.4.2 refused (a routed sky130 ALU whose
input net has a sink via without a resistor path, and a routed gf180 register file whose address
net has two parallel 0.12 Ohm resistors) the two tools agree to 0.0001 ps on every sink's arrival
and transition at three corners, and the whole designs stay within the 1 ps endpoint gate (0.62
and 0.67 ps). For the unconnected sink both give zero wire delay; the suite's own extraction of the
same net gives it 0.83 ps, which bounds what the missing resistance hides there. OpenSTA 3.1.0
removes such a component's capacitance, including the sink's pin capacitance, from the driver's
load; STA-REQ-0731 keeps it.

## Dual-library endpoint gate

The bounded `verification/rc_waveforms.py` runner imports each real routed
extraction, runs f64 timing with propagated clocks, compares full checking,
updates and checks again. The independent reference reads the same standard
files. Every constrained setup/hold endpoint must be present on both sides.
Tool and input hashes are pinned and checked again at completion. A real cell
resize/restore on each library is additionally checked against full timing by
`dual_pdk_extracted_incremental_matches_full`, using `RUSTSTA_RC_CASES`.
The ordinary update check alone is labelled as a no-op.

```sh
python3 verification/rc_waveforms.py --workspace work/rc-validation --base-rev empty \
  --rusteda target/release/rusteda --ruststa target/release/ruststa \
  --opensta /path/to/sta --cases cases.json
```

The manifest has exactly `sky130` and `gf180` tags, each with `library`,
`netlist`, `sdc` and `spef` paths, as in the SPEF verification guide. Inputs are
bounded to 32 MiB for extraction and 256 MiB for identity hashing. Each process
has a 2 GiB address-space cap, 40-second timeout and 32 MiB output cap; RSS is
measured. These tiny one-corner designs have <1000 timing pins and <10000 RC
nodes. The new slew-only pass is O(nodes+arcs) times fixed numeric iteration
limits; no startpoint-to-endpoint paths are materialized. CPU/CUDA propagation
consumes the same f64 prepared records; waveform preparation stays on the CPU.

The independent checker still reports extracted RC as undecided. Circuit
validation, endpoint parity, incrementality, setup/hold closure and fabrication
checks are separate verdicts. Passing the 1 ps comparison does not repair an
existing hold violation or complete full DRC/LVS.
