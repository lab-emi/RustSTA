# Endpoint status taxonomy and completeness accounting (STA-REQ-0226)

The circuit exercises each endpoint status class in a single netlist [@sdc2016;
@bhasker2009static, ch. 7]:
- `r1/D`, `r2/D`, `r6/D`, `r6/RN`, and output port `y`: `constrained` (5 endpoints);
- `r3/D` (clocked by `ck2`, which has no `create_clock`): `unclocked`;
- `r4/D` (clocked by `clk`, fed by input `u` without `set_input_delay`): `no_data_arrival`;
- `r5/D` (clocked by `clk`, fed by `TIELO` constant `0`): `no_data_arrival`;
- output port `z` (no `set_output_delay`): `unconstrained_output`.

### Derivations

1. `b1` drives 3 pins (`r2/D`, `r3/D`, `r6/D`, `C = 0.03 pF`):
   - `n2` rise `0.41 + (0.20 + 0.009 + 0.03) = 0.649 ns`,
     fall `0.359 + (0.18 + 0.008 + 0.036) = 0.583 ns`.
2. Constrained slacks (`period = 2.0 ns`):
   - `r1/D`: setup `2.0 - 0.10 - 0.2 = 1.70 ns`, hold `0.2 - 0.05 = 0.15 ns`;
   - `r2/D` and `r6/D`: setup `2.0 - 0.10 - 0.649 = 1.251 ns`, hold `0.583 - 0.05 = 0.533 ns`;
   - `r6/RN`: recovery `2.0 - 0.12 - 0.2 = 1.68 ns`, removal `0.2 - 0.06 = 0.14 ns`;
   - port `y`: setup `2.0 - 0.3 - 0.40 = 1.30 ns`, hold `0.35 + 0.3 = 0.65 ns`;
   - worst setup slack `1.251 ns`, worst hold slack `0.14 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input ck2, input a, input u, input rn, output y, output z);
 wire lo, n1, n2, q2, q3, q4;
 TIELO t0 (.Y(lo));
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(q2));
 DFF r3 (.D(n2), .CK(ck2), .Q(q3));
 DFF r4 (.D(u), .CK(clk), .Q(q4));
 DFF r5 (.D(lo), .CK(clk), .Q(z));
 DFFR r6 (.D(n2), .CK(clk), .RN(rn), .Q(y));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a rn}]
set_output_delay -clock clk 0.3 [get_ports y]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `r1/D setup` | `1.700000` |
| `r1/D hold` | `0.150000` |
| `r2/D setup` | `1.251000` |
| `r2/D hold` | `0.533000` |
| `r6/D setup` | `1.251000` |
| `r6/D hold` | `0.533000` |
| `r6/RN recovery` | `1.680000` |
| `r6/RN removal` | `0.140000` |
| `y setup` | `1.300000` |
| `y hold` | `0.650000` |
| `worst setup` | `1.251000` |
| `worst hold` | `0.140000` |
