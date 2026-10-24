# Effective RC load and transitions

**STA-REQ-0720**: Reduce each zero-R-contracted RC tree to a passive driving-point
pi by matching three admittance coefficients. For child admittance (a,b,c) behind
R the parent adds (a,b-R*a*a,c-2*R*a*b+R*R*a*a*a). For root (A,B,C), use
Cfar=B*B/C, Rpi=C*C/(-B*B*B), Cnear=A-Cfar; degenerate resistance gives A lumped.
Include physical Liberty/port capacitance before moments; reject overflow and
nonpassive reductions. No topology or capacitance is discarded to meet a gate.

**STA-REQ-0726**: Fit a finite-ramp Thevenin driver to NLDM's lower and
output-delay trip-point times (replaces STA-REQ-0721). Its explicit resistance
proxy is the forward NLDM delay/load sensitivity over 1 fF above the total pi
load (the table at the arc's input transition, `(d(Ctotal + 1 fF) - d(Ctotal))
/ 1 fF`), multiplied by -ln(1-output_threshold); this is an estimate from the
available timing tables, not a measured transistor DC value. A table breakpoint
less than 1 fF above the total load enters the estimate. For each candidate
Ceff in [Cnear,Ctotal], fit the source duration to the lower and delay
trip-point interval of the single-capacitance response. Match charge at the
end of that active source ramp:
Ceff*Vnominal(T) = Cnear*Vroot(T) + Cfar*Vfar(T).
Evaluate the two-pole pi response analytically, bracket the load root and verify
the final charge residual (relative 1e-8). Maximum counts: 64 load bisections,
56 bracketed duration iterations, 48 inner single-pole solves and 80 two-pole crossings.
Cell delay uses NLDM at Ceff; slew is the actual pi root's threshold span,
including the nonlinear tail. Apply this to CK->Q too. This is not CCS. The
shared `rusteda_compute::rc::gate_detail_with_resistance` solves the circuit for
the caller's resistance; `rc::gate_detail` keeps the estimate of STA-REQ-0721.

**STA-REQ-0727**: A pi load whose resistance is below one thousandth of the
STA-REQ-0726 driver resistance is a lumped load: the cell delay and the output
transition are the NLDM values at the total capacitance (Ceff = Ctotal), as for
a load without resistance. The fitted two-point waveform does not reproduce the
characterized table transition of a lumped load (3 to 5 % below it on the default
PDK cells), and below this ratio the external nodal transient of the pi and of its
lumped total coincide to 1e-4 of the transition, so the characterization replaces
the fit. The comparison is per arc and output transition (rise and fall have
separate resistances). Above the ratio the charge solve of STA-REQ-0726 applies;
the transition is discontinuous at the ratio, as observed of a reference timing
tool, whose reported effective capacitance equals the total there.

**STA-REQ-0722**: For interconnect driven by a cell, solve the single-pole ramp
response with pole time equal to the per-sink Elmore moment, at the Liberty
input delay and slew thresholds. Report threshold delay relative to the driver's
output threshold and receiver slew in library units. A primary port without a
characterized driving cell uses an exponential RC threshold response with its
annotated transition added to the exponential transition span. Distinguish
first moments from voltage-threshold delays. Zero RC preserves the transition.

**STA-REQ-0723**: Evaluate all RC arc delay/transition values in a bounded f64
CPU topological pass independently of launch tags. Reuse the same contiguous
arc records in CPU/CUDA forward and backward propagation. Re-evaluate on edits,
including unchanged neighboring cells whose loads change; no stale slew or
required-time record may survive an incremental update. CUDA propagation must
match CPU and reports host waveform preparation as part of the total runtime.

**STA-REQ-0724**: Keep the dual-PDK endpoint error gate at 0.001 ns and test
full detailed extraction with propagated clocks. A missing external executable
is unverified. Also check charge/threshold equations against an external nodal
transient simulator with bounded circuits and explicit numeric tolerances;
validate zero-R, shielding, slow/fast transitions, slew derates and convergence.

Memory preflight: a sequential one-corner reduction needs <512 MiB additional
RC state at the default source budgets. Arc values add 128 bytes/arc, and the
slew-only prepass adds 32 bytes/node. Include retained corner/tag state and
clock passes in --max-state-gib before allocation. Numeric work is at most
the fixed nested limits above per transition/bound. A conservative preflight
work cap is eight scalar evaluation units per permitted state byte, configurable
through --max-state-gib. Exhaustion is T0078 before arc allocation. Build/test
workers=2, CPU process caps/timeouts and measured RSS remain mandatory.

The moment-matched pi and charge criteria are independently derived from
network admittance and capacitor charge conservation. Published background:
[@dartu1994gate]. No reference-tool implementation code is consulted; the 1 fF
step and the 1/1000 ratio of STA-REQ-0726 and STA-REQ-0727 were measured black-box from the
delay-calculation reports of a reference timing tool (linear synthetic Liberty tables, both
PDKs).

**STA-REQ-0725**: Read trip-point and slew-derate fields from the parsed Library
metadata on both direct and persisted-library paths. Never select threshold
semantics by library name. Validate normalized polarity and corner compatibility.
