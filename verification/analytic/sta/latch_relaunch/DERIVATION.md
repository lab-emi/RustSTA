# Latch D-to-Q re-launch in the opening-edge tag, Q slew, and backward required times (STA-REQ-0222, STA-REQ-0227)

The three latch stages of [`latch_open_uncertainty`](../latch_open_uncertainty/DERIVATION.md)
propagate through `D -> Q` and `G -> Q` into downstream gates (`u1`, `u2`, `u3`) and capture
flip-flops `r2`, `r4`, `r6` clocked by `CA` (`req_setup = 2.0 - 0.10 - 0.04 = 1.86 ns`,
`req_hold = 0.05 ns`) [@sakallah1990checktc; @bhasker2009static, ch. 8; @hitchcock1982timing].

### Derivations

1. **Stage 1 (`l1`, `D_open = -0.5`, `R_open = -0.54`, `R_close = 0.36`):**
   - Both arrivals at `l1/D` (`0.629` rise, `0.559` fall, slews `0.07`, `0.061`) exceed `R_close`,
     so both are clipped to `R_close = 0.36` and enter `D -> Q` at `0.36 - (-0.5) = 0.86 ns` in
     `tag_open = (CA, rise)`:
     `Q` rise `max(0.41, 0.86 + 0.267) = 1.127 ns`,
     `Q` fall `max(0.359, 0.86 + 0.2361) = 1.0961 ns`;
     early at `Q` is the `G -> Q` launch (`0.41` rise, `0.359` fall).
   - `Q` slews come from `G -> Q` (`0.09` rise, `0.08` fall), so inverter `u1` adds `0.1085 ns`
     (fall) and `0.136 ns` (rise):
     `r2/D` setup `1.86 - (1.0961 + 0.136) = 0.6279` vs `1.86 - (1.127 + 0.1085) = 0.6245 ns`;
     `r2/D` hold `(0.359 + 0.136) - 0.05 = 0.445 ns`.
2. **Stage 2 (`l2` of cell `LATS`, `D_open = 0.0`, `R_open = -0.04`, `R_close = 0.86`):**
   - `D -> Q` transitions on `LATS` are `0.13 ns` (rise) and `0.12 ns` (fall), larger than the
     `G -> Q` slews (`0.09`, `0.08`), isolating the `Q`-slew rule:
   - `l2/Q` late rise `0.629 + 0.267 = 0.896 ns`, late fall `0.559 + 0.2361 = 0.7951 ns`;
   - buffer `u2` uses the `G -> Q` slews (`0.09`, `0.08`), giving `m4` late rise
     `0.896 + 0.219 = 1.115 ns` and `r4/D` setup `1.86 - 1.115 = 0.745 ns` (vs `0.741 ns` if the
     `D -> Q` slew were used); `r4/D` hold `(0.359 + 0.20) - 0.05 = 0.509 ns`.
3. **Stage 3 (`l3`, `D_open = 1.0`, `R_open = 0.96`):**
   - Data arrives at `0.629 <= R_open` (before the opening edge), so it does not borrow and passes
     nothing through `D -> Q`; only the `G -> Q` launch (`0.41` rise, `0.359` fall) propagates to
     `r6/D`: setup `1.86 - (0.41 + 0.1085) = 1.3415 ns`, hold `0.495 - 0.05 = 0.445 ns`.

## Netlist (`net.v`)

```verilog
module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);
 wire n1, n2, n3, n4, m1, m2, m3, m4, k1, k2, k3, k4;
 DFF r1 (.D(a), .CK(cb), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 LAT l1 (.D(n2), .G(ca), .Q(n3));
 INV u1 (.A(n3), .Y(n4));
 DFF r2 (.D(n4), .CK(ca), .Q(q1));
 DFF r3 (.D(b), .CK(ca), .Q(m1));
 BUF b2 (.A(m1), .Y(m2));
 LATS l2 (.D(m2), .G(ca), .Q(m3));
 BUF u2 (.A(m3), .Y(m4));
 DFF r4 (.D(m4), .CK(ca), .Q(q2));
 DFFN r5 (.D(c), .CKN(ca), .Q(k1));
 BUF b3 (.A(k1), .Y(k2));
 LAT l3 (.D(k2), .G(ca), .Q(k3));
 INV u3 (.A(k3), .Y(k4));
 DFF r6 (.D(k4), .CK(ca), .Q(q3));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `l1 Q rise late` | `1.127000` |
| `l1 Q fall late` | `1.096100` |
| `l1 Q rise early` | `0.410000` |
| `l1 Q fall early` | `0.359000` |
| `r2/D setup` | `0.624500` |
| `r2/D hold` | `0.445000` |
| `l2 Q rise late` | `0.896000` |
| `l2 Q fall late` | `0.795100` |
| `l2 Q rise early` | `0.410000` |
| `l2 Q fall early` | `0.359000` |
| `r4/D setup` | `0.745000` |
| `r4/D hold` | `0.509000` |
| `r4/D setup if D-to-Q slew counted` | `0.741000` |
| `l3 R_open` | `0.960000` |
| `r6/D setup` | `1.341500` |
| `r6/D hold` | `0.445000` |
| `r6/D hold if D-to-Q passed at opening` | `0.322100` |
