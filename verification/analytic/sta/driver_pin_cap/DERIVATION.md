# Driver output-pin capacitance in net load (STA-REQ-0121)

Buffer `b1` (`BUFO`, output pin capacitance `COUT_O = 0.004 pF` on `Y`) drives net `n2`, which
feeds inverters `u1` and `u2` (`0.01 pF` each) [@liberty2017; @bhasker2009static, ch. 3].

### Derivations

1. **Launch at `r1/Q` (`C = 0.01 pF`):**
   - rise `0.41 ns` (slew `0.09 ns`), fall `0.359 ns` (slew `0.08 ns`).
2. **Net `n2` capacitance (`STA-REQ-0121`):**
   - `C(n2) = C(u1/A) + C(u2/A) + C(b1/Y) = 0.01 + 0.01 + 0.004 = 0.024 pF`.
3. **Buffer `b1` (`n1 -> n2`, `C = 0.024 pF`):**
   - `n2` rise `0.41 + (0.20 + 0.10 * 0.09 + 1.0 * 0.024) = 0.643 ns`,
     slew `0.06 + 1.0 * 0.024 = 0.084 ns`;
   - `n2` fall `0.359 + (0.18 + 0.10 * 0.08 + 1.2 * 0.024) = 0.5758 ns`,
     slew `0.05 + 1.1 * 0.024 = 0.0764 ns`.
4. **Inverter `u1` (`n2 -> n3 = r2/D`, `C = 0.01 pF`):**
   - `n3` fall `0.643 + (0.08 + 0.15 * 0.084 + 1.5 * 0.01) = 0.7506 ns`;
   - `n3` rise `0.5758 + (0.10 + 0.20 * 0.0764 + 2.0 * 0.01) = 0.71108 ns`.
5. **Slacks at `r2/D`:**
   - setup `2.0 - 0.10 - 0.7506 = 1.1494 ns` (vs `1.154 ns` without driver output-pin capacitance);
   - hold `0.71108 - 0.05 = 0.66108 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, output q);
 wire n1, n2, n3, n4;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUFO b1 (.A(n1), .Y(n2));
 INV u1 (.A(n2), .Y(n3));
 INV u2 (.A(n2), .Y(n4));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `C n2` | `0.024000` |
| `n2 rise` | `0.643000` |
| `n2 fall` | `0.575800` |
| `n2 slew rise` | `0.084000` |
| `n2 slew fall` | `0.076400` |
| `r2/D setup` | `1.149400` |
| `r2/D hold` | `0.661080` |
| `without driver cap setup` | `1.154000` |
