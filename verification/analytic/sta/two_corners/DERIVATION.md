# Per-corner library resolution and multi-corner analysis (STA-REQ-0015)

A two-register buffer path `r1 -> b -> r2` is analysed across two operating corners (`fast`
with scale `k = 1.0` and `slow` with scale `k = 2.0` on all cell delays, transitions and setup/hold
constraints, while pin capacitances stay `0.01 pF`) [@bhasker2009static, ch. 2; @liberty2017].

### Derivations

1. **Corner `fast` (`k = 1.0`):**
   - `r1/Q` (`C = 0.01 pF`): rise `0.41 ns` (slew `0.09 ns`), fall `0.359 ns` (slew `0.08 ns`);
   - `b/Y = r2/D` (`C = 0.01 pF`): rise `0.41 + 0.219 = 0.629 ns`, fall `0.359 + 0.20 = 0.559 ns`;
   - `r2/D` setup (`s = 0.10 ns`): `2.0 - 0.10 - 0.629 = 1.271 ns`;
   - `r2/D` hold (`h = 0.05 ns`): `0.559 - 0.05 = 0.509 ns`.
2. **Corner `slow` (`k = 2.0`):**
   - `r1/Q` (`C = 0.01 pF`): rise `2 * 0.41 = 0.82 ns` (slew `0.18 ns`), fall `2 * 0.359 = 0.718 ns`
     (slew `0.16 ns`);
   - `b/Y = r2/D` (`C = 0.01 pF`):
     rise `0.82 + 2 * (0.20 + 0.10 * 0.18 + 0.01) = 0.82 + 0.456 = 1.276 ns`,
     fall `0.718 + 2 * (0.18 + 0.10 * 0.16 + 0.012) = 0.718 + 0.416 = 1.134 ns`;
   - `r2/D` setup (`s = 2 * 0.10 = 0.20 ns`): `2.0 - 0.20 - 1.276 = 0.524 ns`;
   - `r2/D` hold (`h = 2 * 0.05 = 0.10 ns`): `1.134 - 0.10 = 1.034 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(q));
endmodule
```

## Constraints (`fast.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
```

## Constraints (`slow.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
```

## Expected values

### Variant `fast`

| Quantity | Value (ns / pF) |
|---|---|
| `Q rise` | `0.410000` |
| `Q fall` | `0.359000` |
| `r2/D late` | `0.629000` |
| `r2/D early` | `0.559000` |
| `r2/D setup` | `1.271000` |
| `r2/D hold` | `0.509000` |

### Variant `slow`

| Quantity | Value (ns / pF) |
|---|---|
| `Q rise` | `0.820000` |
| `Q fall` | `0.718000` |
| `r2/D late` | `1.276000` |
| `r2/D early` | `1.134000` |
| `r2/D setup` | `0.524000` |
| `r2/D hold` | `1.034000` |
