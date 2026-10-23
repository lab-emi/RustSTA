# Library trip-point thresholds and slew derate in lumped NLDM (STA-REQ-0016)

The inverter chain `r1 -> u1 -> u2 -> r2` is evaluated with library `lin_thr`, which declares
`input_threshold_pct_rise : 40`, `input_threshold_pct_fall : 60`, `slew_lower_threshold_pct_* : 30`,
`slew_upper_threshold_pct_* : 70`, and `slew_derate_from_library : 0.5` [@liberty2017;
@bhasker2009static, ch. 3]. Because lumped NLDM lookup tables are already indexed in library slew
units and have no RC interconnect slew degradation, the delays, transitions and slacks equal those
of [`inverter_chain`](../inverter_chain/DERIVATION.md) bit-for-bit.

### Derivations

1. `r1/Q` (`C = 0.01 pF`): rise `0.41 ns` (slew `0.09 ns`), fall `0.359 ns` (slew `0.08 ns`).
2. `u1/Y` (`n2`): fall `0.41 + 0.1085 = 0.5185 ns` (slew `0.052 ns`),
   rise `0.359 + 0.136 = 0.495 ns` (slew `0.065 ns`).
3. `u2/Y` (`n3`): rise `0.5185 + (0.10 + 0.20 * 0.052 + 0.02) = 0.6489 ns`,
   fall `0.495 + (0.08 + 0.15 * 0.065 + 0.015) = 0.59975 ns`.
4. Slacks:
   - `r2/D` setup `2.0 - 0.10 - 0.6489 = 1.2511 ns`, hold `0.59975 - 0.05 = 0.54975 ns`;
   - `r1/D` setup `2.0 - 0.10 - 0.5 = 1.40 ns`, hold `0.5 - 0.05 = 0.45 ns`;
   - port `q` (`C = 0.02 pF`, `Q` rise `0.42 ns`, fall `0.368 ns`):
     setup `2.0 - 0.3 - 0.42 = 1.28 ns`, hold `0.368 + 0.3 = 0.668 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input d, output q);
 wire n1, n2, n3;
 DFF r1 (.D(d), .CK(clk), .Q(n1));
 INV u1 (.A(n1), .Y(n2));
 INV u2 (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_output_delay -clock clk 0.3 [get_ports q]
set_load 0.02 [get_ports q]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `r2/D setup` | `1.251100` |
| `r2/D hold` | `0.549750` |
| `r1/D setup` | `1.400000` |
| `r1/D hold` | `0.450000` |
| `q setup` | `1.280000` |
| `q hold` | `0.668000` |
