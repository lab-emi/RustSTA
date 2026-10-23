# RustSTA exception derivations (docs/specs/sta/exceptions.md)

Hand derivations of the timing exceptions and clock groups of
[`docs/specs/sta/exceptions.md`](../../../docs/specs/sta/exceptions.md), one directory per
requirement. Each derivation adds SDC commands to one of the circuits below and derives the
setup and hold slacks that the rule implies. The circuits use the linear synthetic library of
`crates/apps/ruststa-cli/tests/sta_common/mod.rs` (the library of the known-answer cases in
[`verification/analytic/sta`](../sta/README.md)): every table has two index points per axis, so
interpolation is exact and every value below is exact to the decimals shown. Tests that check a
derivation compare within 1e-9 ns.

Every rule here was measured locally against established practice on these circuits; the
departures are stated in [`ties`](ties/DERIVATION.md) and [`latch`](latch/DERIVATION.md).

| Requirement | Derivation | Circuit |
|---|---|---|
| STA-REQ-0250 | [`analyses`](analyses/DERIVATION.md) | `reconv` |
| STA-REQ-0251 | [`path_ends`](path_ends/DERIVATION.md) | `reconv` |
| STA-REQ-0252 | [`from_objects`](from_objects/DERIVATION.md) | `reconv`, `edges` |
| STA-REQ-0253 | [`to_objects`](to_objects/DERIVATION.md) | `reconv`, `edges` |
| STA-REQ-0254 | [`through`](through/DERIVATION.md) | `xbar` |
| STA-REQ-0255 | [`matching`](matching/DERIVATION.md) | `xbar`, `reconv` |
| STA-REQ-0256 | [`type_order`](type_order/DERIVATION.md) | `reconv` |
| STA-REQ-0257 | [`specificity`](specificity/DERIVATION.md) | `reconv`, `xbar` |
| STA-REQ-0258 | [`ties`](ties/DERIVATION.md) | `reconv` |
| STA-REQ-0259 | [`false_path`](false_path/DERIVATION.md) | `reconv` |
| STA-REQ-0260 | [`multicycle_setup`](multicycle_setup/DERIVATION.md) | `twoclk` |
| STA-REQ-0261 | [`multicycle_hold`](multicycle_hold/DERIVATION.md) | `twoclk`, `reconv` |
| STA-REQ-0262 | [`max_delay`](max_delay/DERIVATION.md) | `pd`, `edges` |
| STA-REQ-0263 | [`min_delay`](min_delay/DERIVATION.md) | `pd` |
| STA-REQ-0264 | [`unclocked_ends`](unclocked_ends/DERIVATION.md) | `pd` |
| STA-REQ-0265 | [`ignore_clock_latency`](ignore_clock_latency/DERIVATION.md) | `pd` |
| STA-REQ-0266 | [`clock_groups`](clock_groups/DERIVATION.md) | `groups` |
| STA-REQ-0267 | [`latch`](latch/DERIVATION.md) | `latch` |
| STA-REQ-0268 | [`check_kinds`](check_kinds/DERIVATION.md) | `gate`, `pd` |
| STA-REQ-0269 | [`path_semantics`](path_semantics/DERIVATION.md) | `reconv` |
| STA-REQ-0270 | [`diagnostics`](diagnostics/DERIVATION.md) | `reconv`, `latch`, `groups` |

## Library values

Delay `d0 + ks * s + kc * C` and output transition `s0 + mc * C`, with `s` the input transition
and `C` the load in pF; every input pin has 0.01 pF; setup time 0.10 ns, hold time 0.05 ns.

| Arc | d0 | ks | kc | s0 | mc |
|---|---|---|---|---|---|
| `CKQ` rise / fall (flip-flop and latch enable to output) | 0.40 / 0.35 | 0 / 0 | 1.0 / 0.9 | 0.08 / 0.07 | 1.0 / 1.0 |
| `BUF` and `AND2` rise / fall | 0.20 / 0.18 | 0.10 / 0.10 | 1.0 / 1.2 | 0.06 / 0.05 | 1.0 / 1.1 |
| `INV` rise / fall | 0.10 / 0.08 | 0.20 / 0.15 | 2.0 / 1.5 | 0.05 / 0.04 | 1.5 / 1.2 |
| `XOR2` rise / fall | 0.30 / 0.28 | 0.25 / 0.20 | 2.5 / 2.2 | 0.07 / 0.06 | 1.8 / 1.6 |
| `DQ` rise / fall (latch data to output) | 0.25 / 0.22 | 0.10 / 0.10 | 1.0 / 1.0 | 0.06 / 0.05 | 1.0 / 1.0 |

Recurring values: a flip-flop output with one load (0.01 pF) rises at 0.41 (transition 0.09) and
falls at 0.359 (0.08); a buffer with one load then adds 0.219 to the rise (transition 0.07) and
0.2 to the fall (transition 0.061), so one buffer between flip-flops gives arrivals 0.629 (rise)
and 0.559 (fall) at the next data pin.

## Circuits

The clocks are ideal with latency 0 and transition 0 unless the circuit says otherwise; ports have
no load and input transition 0. Setup slack is `relation + latency - setup time - uncertainty -
late arrival` and hold slack `early arrival - (relation + latency + hold time + uncertainty)`
([docs/specs/sta/checks.md](../../../docs/specs/sta/checks.md)); with one path per end transition,
early and late arrivals coincide.

### `reconv`

```verilog
module m (clk, a, b, y);
  input clk, a, b;
  output y;
  wire q1, q2, n1, n2, n3;
  DFF r1 (.D(a), .CK(clk), .Q(q1));
  DFF r2 (.D(b), .CK(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1));
  BUF b2 (.A(n1), .Y(n2));
  AND2 g (.A(n2), .B(q2), .Y(n3));
  DFF r3 (.D(n3), .CK(clk), .Q(y));
endmodule
```

Base SDC: `create_clock -name clk -period 2 [get_ports clk]`; `D_s = 2`, `D_h = 0`.

| Path to `r3/D` | Rise arrival | Fall arrival | Derivation |
|---|---|---|---|
| from `r1/CK` | 1.063 | 0.9552 | `0.41 + 0.219 + 0.217 + 0.217`; `0.359 + 0.2 + 0.1981 + 0.1981` (buffer and AND inputs see transitions 0.07 and 0.061) |
| from `r2/CK` | 0.629 | 0.559 | `0.41 + 0.219`; `0.359 + 0.2` |

Path slacks without exceptions: from `r1/CK` setup `1.9 - 1.063 = 0.837`, hold
`0.9552 - 0.05 = 0.9052`; from `r2/CK` setup `1.9 - 0.629 = 1.271`, hold `0.559 - 0.05 = 0.509`.
Endpoint `r3/D`: setup 0.837, hold 0.509. The inputs `a` and `b` have no input delay and `y` no
output delay: those paths are not timed.

### `edges`

```verilog
module m (clk, a, b, y, z);
  input clk, a, b;
  output y, z;
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1));
  DFFN r2 (.D(b), .CKN(clk), .Q(q2));
  BUF b1 (.A(q1), .Y(n1));
  BUF b2 (.A(q2), .Y(n2));
  DFFN r3 (.D(n1), .CKN(clk), .Q(y));
  DFF r4 (.D(n2), .CK(clk), .Q(z));
endmodule
```

Base SDC: `create_clock -name clk -period 2 -waveform {0 0.8} [get_ports clk]`. Both paths arrive
at 0.629 (rise) and 0.559 (fall).
- `r1` (launch: rise at 0) to `r3` (capture: fall): `D_s = 0.8`, `D_h = 0.8 - 2 = -1.2`; setup
  `0.8 - 0.1 - 0.629 = 0.071`, hold `0.559 - (-1.2 + 0.05) = 1.709`.
- `r2` (launch: fall at 0.8) to `r4` (capture: rise): `D_s = 2 - 0.8 = 1.2`, `D_h = 0 - 0.8 = -0.8`;
  setup `1.2 - 0.1 - 0.629 = 0.471`, hold `0.559 - (-0.8 + 0.05) = 1.309`.

### `twoclk`

```verilog
module m (ca, cb, a, b, y, z);
  input ca, cb, a, b;
  output y, z;
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(ca), .Q(q1));
  BUF b1 (.A(q1), .Y(n1));
  DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2));
  BUF b2 (.A(q2), .Y(n2));
  DFF r4 (.D(n2), .CK(ca), .Q(z));
endmodule
```

Clock `A` on `ca`, clock `B` on `cb`; the derivations use periods `A` 4 and `B` 1, or `A` 3 and
`B` 2 (`create_clock -name A -period 4 [get_ports ca]` and so on). Both paths arrive at 0.629
(rise) and 0.559 (fall). In both period pairs, both directions have `D_s = 1` and `D_h = 0`:
- `A` 4, `B` 1: `A` to `B` launches at 0 and the next `B` edge is 1; `B` to `A` launches at 0, 1,
  2, 3 and the smallest difference to the `A` edge 4 is `4 - 3 = 1`; the latest capture edge at or
  before each launch gives at most 0.
- `A` 3, `B` 2: `A` to `B` launches at 0 and 3 and captures at 0, 2, 4, 6:
  `D_s = min(2 - 0, 4 - 3) = 1`, `D_h = max(0 - 0, 2 - 3) = 0`; `B` to `A` launches at 0, 2, 4
  and captures at 0, 3, 6: `D_s = min(3, 1, 2) = 1`, `D_h = max(0, -2, -1) = 0`.
Without exceptions every path has setup `1 - 0.1 - 0.629 = 0.271` and hold `0.559 - 0.05 = 0.509`.

### `groups`

```verilog
module m (ca, cb, cc, a, b, y, z, w);
  input ca, cb, cc, a, b;
  output y, z, w;
  wire q1, q2, q3, n1, n2, n3;
  DFF r1 (.D(a), .CK(ca), .Q(q1));
  BUF b1 (.A(q1), .Y(n1));
  DFF r2 (.D(n1), .CK(cb), .Q(y));
  DFF r3 (.D(b), .CK(cb), .Q(q2));
  BUF b2 (.A(q2), .Y(n2));
  DFF r4 (.D(n2), .CK(cc), .Q(z));
  DFF r5 (.D(n2), .CK(ca), .Q(q3));
  BUF b3 (.A(q3), .Y(n3));
  DFF r6 (.D(n3), .CK(cc), .Q(w));
endmodule
```

Clocks `A` (period 4, `ca`), `B` (1, `cb`), `C` (2, `cc`). `b2` drives two pins (0.02 pF):
`0.41 + 0.229 = 0.639` (rise), `0.359 + 0.212 = 0.571` (fall).

| Path | Clocks | `D_s`, `D_h` | Setup | Hold |
|---|---|---|---|---|
| `r1` to `r2` | `A` to `B` | 1, 0 | 0.271 | 0.509 |
| `r3` to `r4` | `B` to `C` | 1, 0 | `1 - 0.1 - 0.639 = 0.261` | `0.571 - 0.05 = 0.521` |
| `r3` to `r5` | `B` to `A` | 1, 0 | 0.261 | 0.521 |
| `r5` to `r6` | `A` to `C` | 2, 0 | `2 - 0.1 - 0.629 = 1.271` | 0.509 |

### `pd`

```verilog
module m (clk, a, in3, in4, out1, out3, out4);
  input clk, a, in3, in4;
  output out1, out3, out4;
  wire q1, n1, q2, n3, q3;
  DFF r1 (.D(a), .CK(clk), .Q(q1));
  BUF b1 (.A(q1), .Y(n1));
  DFF r2 (.D(n1), .CK(clk), .Q(q2));
  BUF b2 (.A(q2), .Y(out1));
  BUF b3 (.A(in3), .Y(n3));
  DFF r3 (.D(n3), .CK(clk), .Q(q3));
  BUF b4 (.A(q3), .Y(out4));
  BUF b5 (.A(in4), .Y(out3));
endmodule
```

Base SDC:

```
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency 0.1 [get_clocks clk]
set_clock_uncertainty -setup 0.05 [get_clocks clk]
set_clock_uncertainty -hold 0.02 [get_clocks clk]
set_output_delay 0.2 -clock clk [get_ports out1]
```

| Path | Rise arrival | Fall arrival | Setup slack | Hold slack |
|---|---|---|---|---|
| `r1/CK` to `r2/D` | `0.1 + 0.41 + 0.219 = 0.729` | `0.1 + 0.359 + 0.2 = 0.659` | `2 + 0.1 - 0.1 - 0.05 - 0.729 = 1.221` | `0.659 - (0.1 + 0.05 + 0.02) = 0.489` |
| `r2/CK` to `out1` | `0.1 + 0.41 + 0.209 = 0.719` | `0.1 + 0.359 + 0.188 = 0.647` | `2 + 0.1 - 0.2 - 0.05 - 0.719 = 1.131` | `0.647 - (0.1 - 0.2 + 0.02) = 0.727` |
| `in3` to `r3/D` | `0.21` | `0.192` | not timed (no input delay) | not timed |
| `r3/CK` to `out4` | 0.719 | 0.647 | not timed (no output delay) | not timed |
| `in4` to `out3` | `0.2` | `0.18` | not timed | not timed |

The buffers driving output ports have no load (`0.2 + 0.1 * 0.09 = 0.209`, `0.18 + 0.1 * 0.08 =
0.188`); `b3` and `b5` see input transition 0 (`0.2 + 0.01 = 0.21`, `0.18 + 0.012 = 0.192`).

### `xbar`

```verilog
module m (clk, a, b, y, z);
  input clk, a, b;
  output y, z;
  wire q1, q2, n1, n2;
  DFF r1 (.D(a), .CK(clk), .Q(q1));
  DFF r2 (.D(b), .CK(clk), .Q(q2));
  AND2 g1 (.A(q1), .B(q2), .Y(n1));
  XOR2 g2 (.A(q1), .B(q2), .Y(n2));
  DFF r3 (.D(n1), .CK(clk), .Q(y));
  DFF r4 (.D(n2), .CK(clk), .Q(z));
endmodule
```

Base SDC: `create_clock -name clk -period 2 [get_ports clk]`. Each flip-flop output drives two
pins (0.02 pF): rise 0.42 (transition 0.10), fall 0.368 (0.09).
- To `r3/D` through `g1` (from `r1` or `r2`): rise `0.42 + 0.22 = 0.64`, fall
  `0.368 + 0.201 = 0.569`; setup `1.9 - 0.64 = 1.26`.
- To `r4/D` through the non-unate `g2`: an output rise follows a rise at `0.42 + 0.35 = 0.77` or a
  fall at `0.368 + 0.3475 = 0.7155`, an output fall a rise at `0.42 + 0.322 = 0.742` or a fall at
  `0.368 + 0.32 = 0.688`; setup `1.9 - 0.77 = 1.13`.

### `latch`

```verilog
module m (clk, a, y);
  input clk, a;
  output y;
  wire q1, n1, n2, n3, n4;
  DFF r1 (.D(a), .CK(clk), .Q(q1));
  BUF b1 (.A(q1), .Y(n1));
  BUF b2 (.A(n1), .Y(n2));
  LAT l (.D(n2), .G(clk), .Q(n3));
  INV u (.A(n3), .Y(n4));
  DFF r2 (.D(n4), .CK(clk), .Q(y));
endmodule
```

Base SDC: `create_clock -name clk -period 2 [get_ports clk]`. The latch is open from the rise at 0
to the fall at 1. Data from `r1` reaches `l/D` at 0.846 (rise) and 0.7571 (fall), while the latch
is open and before `1 - 0.1`: it borrows, and the latch setup slack is 0; the hold check against
the closing edge at -1 gives `0.7571 - (-1 + 0.05) = 1.7071`. At `l/Q` the latch's own launch
gives 0.41 (rise) and 0.359 (fall); the borrowed data gives `0.846 + 0.267 = 1.113` (rise) and
`0.7571 + 0.2361 = 0.9932` (fall). The transitions at `l/Q` are the worst over both arcs: 0.09
(rise) and 0.08 (fall), so the inverter adds 0.1085 to a fall at `n4` and 0.136 to a rise. At
`r2/D`: late fall `1.113 + 0.1085 = 1.2215`, setup `1.9 - 1.2215 = 0.6785`; early rise
`0.359 + 0.136 = 0.495`, hold `0.495 - 0.05 = 0.445`.

### `gate`

```verilog
module m (clk, a, en, y);
  input clk, a, en;
  output y;
  wire q1, e1, gck, q2, n1;
  DFF r1 (.D(en), .CK(clk), .Q(q1));
  BUF be (.A(q1), .Y(e1));
  AND2 g (.A(clk), .B(e1), .Y(gck));
  DFF r0 (.D(a), .CK(clk), .Q(q2));
  BUF bd (.A(q2), .Y(n1));
  DFF r2 (.D(n1), .CK(gck), .Q(y));
endmodule
```

Base SDC: `create_clock -name clk -period 2 [get_ports clk]`. The AND gate gates `clk` with the
enable `e1`, which arrives at `g/B` at 0.629 (rise) and 0.559 (fall). The gating check
(active-high gating, margin 0) has its setup relation at the leading rise (2) and its hold
relation at the trailing fall (1): setup `2 - 0.629 = 1.371`, hold `0.559 - 1 = -0.441`.
