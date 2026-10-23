# Negative extrapolated slew clamped at zero, delay unclamped (STA-REQ-0120)

Cell `x` (`XTR`, delay `0.05 - 0.04 * s + 1.0 * C`, transition `0.10 - 0.08 * s + 0.5 * C`) is
driven by input port `a` with `set_input_delay 0.3 ns` and `set_input_transition 2.0 ns` (outside
the `[0, 1]` table index range) [@liberty2017; @bhasker2009static, ch. 3].

### Derivations

1. **Extrapolation at `x/Y` (`n1`, `s = 2.0 ns`, `C = 0.01 pF`):**
   - raw delay `0.05 - 0.04 * 2.0 + 1.0 * 0.01 = -0.02 ns` (not clamped);
   - raw output transition `0.10 - 0.08 * 2.0 + 0.5 * 0.01 = -0.055 ns`, clamped to
     `slew_out = max(0.0, -0.055) = 0.0 ns`;
   - arrival at `n1`: `0.3 + (-0.02) = 0.28 ns` (rise and fall).
2. **Downstream buffer `b` (`n1 -> n2 = r1/D`, `s_in = 0.0 ns`, `C = 0.01 pF`):**
   - `n2` rise `0.28 + (0.20 + 0.10 * 0.0 + 1.0 * 0.01) = 0.49 ns`;
   - `n2` fall `0.28 + (0.18 + 0.10 * 0.0 + 1.2 * 0.01) = 0.472 ns`.
3. **Slacks at `r1/D`:**
   - setup `2.0 - 0.10 - 0.49 = 1.41 ns` (whereas clamping the negative delay would give `1.39 ns`,
     and leaving the negative slew unclamped would give `1.4155 ns`);
   - hold `0.472 - 0.05 = 0.422 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, output q);
 wire n1, n2;
 XTR x (.A(a), .Y(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r1 (.D(n2), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.3 [get_ports a]
set_input_transition 2.0 [get_ports a]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `XTR delay` | `-0.020000` |
| `XTR raw transition` | `-0.055000` |
| `XTR transition` | `0.000000` |
| `n2 rise` | `0.490000` |
| `n2 fall` | `0.472000` |
| `r1/D setup` | `1.410000` |
| `r1/D hold` | `0.422000` |
| `clamped-delay setup` | `1.390000` |
| `unclamped-slew setup` | `1.415500` |
