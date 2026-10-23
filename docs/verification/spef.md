# SPEF verification

The additive parasitic model is intentionally specified as lumped NLDM plus
first-moment Elmore delay. This is not an effective-capacitance waveform solver.
Format transport, electrical model, timing consistency and a design's timing
closure are checked separately.

## Independent electrical check

`verification/spef_acceptance.py` reads four bounded synthetic RC networks from
`verification/fixtures/synthetic/spef/moments.json`. Each has at most eight nodes.
The timing input has an ideal source and one loaded output. A separate Ngspice
nodal solve evaluates `G τ = C` with the source clamped to zero and capacitance
as injected current. With resistance in kOhm and capacitance in pF, the numeric
voltage equals the first moment in ns. This checks the resistor-network equations
independently of the engine's tree traversal; it does not claim a transient
50%-threshold delay. The cases include branching, zero-resistance contraction,
asymmetry and a range of R/C values. This first-moment formulation follows the
RC interconnect treatment in [@sapatnekar2004timing].

The absolute comparison limit is 1e-9 ns. Input, decoded bytes, nodes, elements,
subprocess output and running time are bounded. Standard input is closed. Each
process receives a 2 GiB address-space cap and a 40-second timeout; cases run
serially and peak RSS is measured.

## Real-library checks

The same harness accepts a local manifest containing exactly the two baseline
libraries, their mapped netlists, constraints and extracted SPEFs. The manifest
may reference `.spef.gz`. Each case runs import, f64 analysis, full checking,
no-op update and another full check. An additional explicitly enabled engine
test performs a real cell resize and restoration against full recomputation
on both libraries (`dual_pdk_extracted_incremental_matches_full`).

OpenSTA reads exactly the same Liberty, SDC and detailed SPEF. The unchanged
1 ps comparison gate is recorded for the complete extraction. A separate,
clearly marked zero-resistance projection verifies names, capacitance loading
and cell timing without claiming that the projection is a physical extraction.
The projection must meet the 1 ps gate; full-R differences remain visible and
make the overall acceptance result `partial`. No reference implementation code
is read, compared, translated or used to close a difference.

An input manifest has the form:

```json
{"cases":[
  {"tag":"sky130","library":"cells-a.lib","netlist":"mapped-a.v","sdc":"a.sdc","spef":"a.spef.gz"},
  {"tag":"gf180","library":"cells-b.lib","netlist":"mapped-b.v","sdc":"b.sdc","spef":"b.spef.gz"}
]}
```

Supply actual paths on the local machine. The harness pins file and executable
hashes and rejects changes during a run. Missing references are unverified,
never a pass. Full local receipts are retained in the chosen workspace.

```sh
python3 verification/spef_acceptance.py --workspace work/rc-check --base-rev empty \
  --rusteda target/release/rusteda --ruststa target/release/ruststa \
  --opensta /path/to/sta --ngspice /path/to/ngspice --cases cases.json
```

## Memory and work preflight

Encoded and decoded sources are each capped at 32 MiB by default. Parsed data,
name indices, zero-R contraction, CSR and four transition/bound components have
a conservative additional footprint below 512 MiB at default caps. Timing tag
state adds `160 × nodes × tags` bytes and explicit clock state; the engine checks
the combined estimate before allocating. Immutable parasitics are Arc-shared.
No paths, cuts or cones are enumerated without a limit.

The nodal cases need less than 1 MiB of electrical data. The real extracted cases
have fewer than 10,000 RC nodes and one corner; expected extra RC state is below
4 MiB, with process memory dominated by library parsing. Builds/tests use two
workers, timeouts and recorded RSS. GPU checks, when available, use a physical
memory cgroup instead of an address-space limit that blocks device mappings.

See the [HTML result report](spef-report.html) for measured results and remaining
differences. Passing the model checks is not a setup/hold or fabrication signoff.
