# Active-high (AND) and active-low (OR) combinational clock-gating checks (STA-REQ-0216, STA-REQ-0225)

Enable signals launched by rising-edge flip-flop `r1` (`t_L = 0 ns`, `e2` driving `c1/B` and
`c2/B`, `C = 0.02 pF`) and falling-edge flip-flop `r4` (`t_L = 1 ns`, `f2` driving `c3/B` and
`c4/B`, `C = 0.02 pF`) gate clock `clk` (period `2.0 ns`, rise at `0`, fall at `1`) through AND
gates `c1`, `c3` (active-high, `v = 0`) and OR gates `c2`, `c4` (active-low, `v = 1`)
[@bhasker2009static, ch. 10].

### Derivations

1. **Enable arrivals (`C(e) = 0.01 pF`, `C(e2) = C(f2) = 0.02 pF`):**
   - `late = 0.41 + (0.20 + 0.009 + 0.02) = 0.639 ns` (rise);
   - `early = 0.359 + (0.18 + 0.008 + 0.024) = 0.571 ns` (fall).
2. **AND gate `c1` (active-high, launch at `0`):**
   - leading edge is next clock rise (`D_s = 2.0 ns`): setup `2.0 - 0.639 = 1.361 ns`;
   - trailing edge is clock fall ending the `[0, 1]` pulse (`h = 1.0 ns`):
     hold `0.571 - 1.0 = -0.429 ns`.
3. **AND gate `c3` (active-high, launch at `1`):**
   - leading edge is next clock rise at `2` (`D_s = 2.0 - 1.0 = 1.0 ns`):
     setup `1.0 - 0.639 = 0.361 ns`;
   - trailing edge is clock fall at `1` (`h = 1.0 - 1.0 = 0.0 ns`):
     hold `0.571 - 0.0 = 0.571 ns`.
4. **OR gate `c2` (active-low, launch at `0`, first-principles active low pulse `[1, 2]`):**
   - leading edge is clock fall at `1` (`D_s = 1.0 ns`): setup `1.0 - 0.639 = 0.361 ns`;
   - trailing edge is clock rise at `0` (`h = 0.0 ns` from first principles):
     hold `0.571 - 0.0 = 0.571 ns`.
5. **OR gate `c4` (active-low, launch at `1`, active low pulse `[1, 2]`):**
   - leading edge is next clock fall at `3` (`D_s = 3.0 - 1.0 = 2.0 ns`):
     setup `2.0 - 0.639 = 1.361 ns`;
   - trailing edge is clock rise at `2` (`h = 2.0 - 1.0 = 1.0 ns` from first principles):
     hold `0.571 - 1.0 = -0.429 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input d, output q1, output q2, output q3, output q4);
 wire e, e2, f, f2, g1, g2, g3, g4;
 DFF r1 (.D(a), .CK(clk), .Q(e));
 BUF b (.A(e), .Y(e2));
 DFFN r4 (.D(a), .CKN(clk), .Q(f));
 BUF bf (.A(f), .Y(f2));
 AND2 c1 (.A(clk), .B(e2), .Y(g1));
 OR2 c2 (.A(clk), .B(e2), .Y(g2));
 AND2 c3 (.A(clk), .B(f2), .Y(g3));
 OR2 c4 (.A(clk), .B(f2), .Y(g4));
 DFF r2 (.D(d), .CK(g1), .Q(q1));
 DFF r3 (.D(d), .CK(g2), .Q(q2));
 DFF r5 (.D(d), .CK(g3), .Q(q3));
 DFF r6 (.D(d), .CK(g4), .Q(q4));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a d}]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `enable late` | `0.639000` |
| `enable early` | `0.571000` |
| `c1/B setup` | `1.361000` |
| `c1/B hold` | `-0.429000` |
| `c2/B setup` | `0.361000` |
| `c2/B hold` | `0.571000` |
| `c3/B setup` | `0.361000` |
| `c3/B hold` | `0.571000` |
| `c4/B setup` | `1.361000` |
| `c4/B hold` | `-0.429000` |
| `r2/D setup` | `1.800000` |
| `r2/D hold` | `0.050000` |
