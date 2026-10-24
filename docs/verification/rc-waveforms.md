# RC waveform acceptance

Version 0.3 uses a three-moment passive pi, a charge-matched finite-ramp
Thevenin driver and per-sink threshold/slew propagation. Its driver resistance
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

The engine's driver rule (STA-REQ-0726, STA-REQ-0727) has its own external cases in
`crates/engines/ruststa/tests/rc_driver_spice.rs`: two pi loads near a table breakpoint with the
forward 1 fF resistance (crossings, transition and end-of-ramp charge within the same 1e-6 ns and
1e-8 pF tolerances) and two pi loads below one thousandth of the driver resistance, where the
simulated pi and its lumped total give driver waveforms within 1e-4 of the transition while the
fitted waveform transition differs from the table transition by more than 1 %:

```sh
RUSTSTA_NGSPICE=/path/to/ngspice RUSTSTA_RC_DRIVER_WORKSPACE=work/engine-driver-oracle \
RUST_TEST_THREADS=2 cargo test -p ruststa --test rc_driver_spice -- --ignored
```

Run binaries with an OS memory ceiling and timeout, as in the repository's
engineering rules. Both evidence directories must be fresh. Native core crates
have no simulator or Python runtime dependency.

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
