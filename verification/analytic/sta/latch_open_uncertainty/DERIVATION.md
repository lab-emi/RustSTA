# Latch transparency window, opening-edge uncertainty and max time borrow (STA-REQ-0221)

Three positive latches `l1`, `l2`, `l3` (`LAT`, transparent while `CA` is high `[0, 1]`, pulse
width `W = 1.0 ns`, `set_clock_uncertainty -setup 0.04` on `CA`) are fed through buffers (`arr_rise
= 0.629 ns`, `arr_fall = 0.559 ns`) from:
- `r1` launched by `CB` rising at `0.5 ns` (`D_close = 0.5 ns`, `D_open = -0.5 ns`, `D_h = -1.5 ns`);
- `r2` launched by `CA` rising at `0.0 ns` (`D_close = 1.0 ns`, `D_open = 0.0 ns`, `D_h = -1.0 ns`);
- `r3` (`DFFN`) launched by `CA` falling at `1.0 ns` (`D_close = 2.0 ns`, `D_open = 1.0 ns`,
  `D_h = 0.0 ns`).
Variant `mtb` adds `set_max_time_borrow 0.3 [get_cells l2]` [@sakallah1990checktc;
@bhasker2009static, ch. 8].

### Derivations

1. **Latch `l1` (`D_close = 0.5`, `D_open = -0.5`):**
   - `R_open = -0.5 - 0.04 = -0.54 ns`, `R_close = 0.5 - 0.10 - 0.04 = 0.36 ns`;
   - `a = 0.629 > R_close`: setup slack `0.36 - 0.629 = -0.269 ns`;
   - hold slack against closing edge (`D_h = -1.5`): `0.559 - (-1.5 + 0.05) = 2.009 ns`.
2. **Latch `l2` (`D_close = 1.0`, `D_open = 0.0`):**
   - `R_open = 0.0 - 0.04 = -0.04 ns`, `R_close = 1.0 - 0.10 - 0.04 = 0.86 ns` in `base`;
     `R_open < 0.629 <= 0.86`, so `l2/D` borrows and setup slack is `0.0 ns`;
   - in `mtb`, `R_close = min(0.86, -0.04 + 0.3) = 0.26 ns`, so setup slack is
     `0.26 - 0.629 = -0.369 ns`;
   - hold slack (`D_h = -1.0`): `0.559 - (-1.0 + 0.05) = 1.509 ns`.
3. **Latch `l3` (`D_close = 2.0`, `D_open = 1.0`):**
   - `R_open = 1.0 - 0.04 = 0.96 ns`, `R_close = 1.86 ns`;
   - `a = 0.629 <= R_open`: setup slack `0.96 - 0.629 = 0.331 ns` (reduced by the `0.04 ns`
     opening-edge setup uncertainty);
   - hold slack (`D_h = 0.0`): `0.559 - 0.05 = 0.509 ns`.

## Netlist (`net.v`)

```verilog
module m (input ca, input cb, input a, input b, input c, output q1, output q2, output q3);
 wire n1, n2, m1, m2, k1, k2;
 DFF r1 (.D(a), .CK(cb), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 LAT l1 (.D(n2), .G(ca), .Q(q1));
 DFF r2 (.D(b), .CK(ca), .Q(m1));
 BUF b2 (.A(m1), .Y(m2));
 LAT l2 (.D(m2), .G(ca), .Q(q2));
 DFFN r3 (.D(c), .CKN(ca), .Q(k1));
 BUF b3 (.A(k1), .Y(k2));
 LAT l3 (.D(k2), .G(ca), .Q(q3));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
```

## Constraints (`mtb.sdc`)

```tcl
create_clock -name CA -period 2 -waveform {0 1} [get_ports ca]
create_clock -name CB -period 2 -waveform {0.5 1.5} [get_ports cb]
set_clock_uncertainty -setup 0.04 [get_clocks CA]
set_max_time_borrow 0.3 [get_cells l2]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `a rise` | `0.629000` |
| `a fall` | `0.559000` |
| `l1 D_open` | `-0.500000` |
| `l1 R_open` | `-0.540000` |
| `l1 R_close` | `0.360000` |
| `l1/D setup` | `-0.269000` |
| `l2 D_open` | `0.000000` |
| `l2 R_open` | `-0.040000` |
| `l2 R_close` | `0.860000` |
| `l2/D setup` | `0.000000` |
| `l3 D_open` | `1.000000` |
| `l3 R_open` | `0.960000` |
| `l3 R_close` | `1.860000` |
| `l3/D setup` | `0.331000` |
| `l1/D hold` | `2.009000` |
| `l2/D hold` | `1.509000` |
| `l3/D hold` | `0.509000` |

### Variant `mtb`

| Quantity | Value (ns / pF) |
|---|---|
| `a rise` | `0.629000` |
| `a fall` | `0.559000` |
| `l1 D_open` | `-0.500000` |
| `l1 R_open` | `-0.540000` |
| `l1 R_close` | `0.360000` |
| `l1/D setup` | `-0.269000` |
| `l2 D_open` | `0.000000` |
| `l2 R_open` | `-0.040000` |
| `l2 R_close` | `0.260000` |
| `l2/D setup` | `-0.369000` |
| `l3 D_open` | `1.000000` |
| `l3 R_open` | `0.960000` |
| `l3 R_close` | `1.860000` |
| `l3/D setup` | `0.331000` |
| `l1/D hold` | `2.009000` |
| `l2/D hold` | `1.509000` |
| `l3/D hold` | `0.509000` |
