# Effective RC load and transitions

**STA-REQ-0720**: Reduce each zero-R-contracted RC tree to a passive driving-point
pi by matching three admittance coefficients. For child admittance (a,b,c) behind
R the parent adds (a,b-R*a*a,c-2*R*a*b+R*R*a*a*a). For root (A,B,C), use
Cfar=B*B/C, Rpi=C*C/(-B*B*B), Cnear=A-Cfar; degenerate resistance gives A lumped.
Include physical Liberty/port capacitance before moments; reject overflow and
nonpassive reductions. No topology or capacitance is discarded to meet a gate.

**STA-REQ-0728**: Fit a finite-ramp Thevenin driver to NLDM's lower and
output-delay trip-point times and take its effective capacitance from one
Newton-Raphson correction (replaces STA-REQ-0726). The resistance proxy is unchanged: the forward
NLDM delay/load sensitivity over 1 fF above the total pi load, `(d(Ctotal + 1 fF) - d(Ctotal)) /
1 fF` at the arc's input transition, times -ln(1-output_threshold); an estimate from the timing
tables, not a measured transistor DC value. The unknowns are the source start t0, the source ramp
duration T and the capacitance C. The residuals are the single-capacitance response
V(t; T, Rd*C) at the NLDM delay d(C) minus the output trip point, at the lower trip time
d(C) - s(C)*derate*(output-lower)/(upper-lower) minus the lower trip point, and the end-of-ramp
charge (C*V(T) - Cnear*Vroot(T) - Cfar*Vfar(T)) / Ctotal of the two-pole pi response. The start
is C = Ctotal with the source of the table transition at the total load, T = s(Ctotal)*derate /
(upper-lower) and t0 = d(Ctotal) - output*T. One step with the exact Jacobian (analytic in the
response; forward table slopes over a relative load step of 1e-7) gives Ceff, clamped to
[Cnear, Ctotal]. The cell delay is NLDM at Ceff; the source duration is refitted at Ceff to the
lower and delay trip-point interval (at most 56 bracketed duration iterations of 48 inner
single-pole solves), and the slew and the root crossing are those of the actual pi root (at most
80 two-pole bisections each), including the nonlinear tail. Apply this to CK->Q too. This is not
CCS. The correction does not iterate to the charge solution: against transistor-level transients
of both default PDKs' buffers and inverters into the same loads (each cell characterized by the
same simulator, so that characterization differences cancel), the converged charge solution
over-estimates the shielding of every resistive load (driver delay low by up to 45 ps at
3 kOhm); the one-step capacitance lowers the mean driver delay error from 3.1 to 1.8 ps on pi
loads and from 5.6 to 2.3 ps on two-segment RC trees (see the verification report). A reference
timing tool's reported effective capacitance agrees with the one-step value to within 0.06 ps of
delay up to 100 Ohm and 0.3 ps up to 500 Ohm on the default cells. The shared
`rusteda_compute::rc::gate_one_step` computes it for the caller's resistance;
`rc::gate_detail_with_resistance` keeps the charge solve of STA-REQ-0726 and `rc::gate_detail`
the estimate of STA-REQ-0721.

**STA-REQ-0727**: A pi load whose resistance is below one thousandth of the
STA-REQ-0728 driver resistance is a lumped load: the cell delay and the output
transition are the NLDM values at the total capacitance (Ceff = Ctotal), as for
a load without resistance. The fitted two-point waveform does not reproduce the
characterized table transition of a lumped load (3 to 5 % below it on the default
PDK cells), and below this ratio the external nodal transient of the pi and of its
lumped total coincide to 1e-4 of the transition, so the characterization replaces
the fit. The comparison is per arc and output transition (rise and fall have
separate resistances). Above the ratio the one-step capacitance of STA-REQ-0728 applies;
the transition is discontinuous at the ratio, as observed of a reference timing
tool, whose reported effective capacitance equals the total there.

**STA-REQ-0729**: Interconnect delay and transition per sink (replaces STA-REQ-0722). When the
transition at the driver pin was set by a cell arc into the net's pi load outside the lumped
regime (STA-REQ-0728, STA-REQ-0727, clock-to-output launches included), the sink voltage is that
driver's pi root response (its resistance and refitted source ramp) through a single pole of the
sink's first moment: for root poles a, b with weight w and sink moment tau, the sink response is
w*H(a,tau) + (1-w)*H(b,tau), with H the ramp response of two cascaded poles (the equal-pole limit
within a relative 1e-6). The delay runs from the root's output trip point to the sink's input
trip point; the transition is the sink's threshold span in library units. Each sink crossing is
bracketed by at most 64 doublings and found by at most 100 bisections to a relative 1e-13. The
driver waveform that set the driver pin's transition (per transition and bound) is the one the
net's sinks follow. Every other net arc of a cell output solves the single-pole ramp response at
the driver transition with pole time equal to the per-sink first moment, and a primary port
without a characterized driving cell uses the exponential RC threshold response with its
annotated transition added to the exponential transition span (both as in STA-REQ-0722). Zero RC
preserves the transition. Distinguish first moments from voltage-threshold delays. A ramp with
the driver's transition is not the driver's waveform: against transistor-level transients of
both default PDKs into two-segment RC trees, the sink arrival (driver and wire delay) of
STA-REQ-0728 with this response has a mean error of 0.64 ps (max 9.0 ps) where the charge
solution with the ramp response had 6.5 ps (max 39.6 ps), and the mean sink transition error
falls from 38 to 24 ps. As observed of a reference timing tool, its sink delays and transitions
equal this response to 0.03 ps when the driver waveform is the same.

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
RC state at the default source budgets. Arc values add 128 bytes/arc, the
slew-only prepass adds 32 bytes/node and the driver waveform record of
STA-REQ-0729 96 bytes/node. Include retained corner/tag state and
clock passes in --max-state-gib before allocation. Numeric work is at most
the fixed nested limits above per transition/bound. A conservative preflight
work cap is eight scalar evaluation units per permitted state byte, configurable
through --max-state-gib. Exhaustion is T0078 before arc allocation. Build/test
workers=2, CPU process caps/timeouts and measured RSS remain mandatory.

The moment-matched pi and charge criteria are independently derived from
network admittance and capacitor charge conservation. Published background:
[@dartu1994gate], [@dartu1996performance] and [@elmore1948transient]. No reference-tool
implementation code is consulted; the 1 fF step and the 1/1000 ratio of STA-REQ-0728 and
STA-REQ-0727, the single correction of STA-REQ-0728 and the driver-waveform sinks of
STA-REQ-0729 were compared black-box with the delay-calculation and path reports of a reference
timing tool (linear synthetic Liberty tables, both PDKs). The remaining measured differences:
above about 1 kOhm the reference's effective capacitance lies between the one-step value and
the charge solution (delay differences up to 8 ps at 3 kOhm on the default cells), and its
driver waveform transition differs from the refitted one by up to 1.5 % (1.5 ps on a
0.1 ns transition).

**STA-REQ-0725**: Read trip-point and slew-derate fields from the parsed Library
metadata on both direct and persisted-library paths. Never select threshold
semantics by library name. Validate normalized polarity and corner compatibility.
