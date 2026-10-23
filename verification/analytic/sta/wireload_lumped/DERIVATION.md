# Lumped wire-load model and port-connected net exclusion (STA-REQ-0122, STA-REQ-0123, STA-REQ-0124, STA-REQ-0125)

Library `lin_wl` defines `default_wire_load : "wl_lin"` (`default_wire_load_mode : top`) with
`capacitance : 0.001 pF`, `slope : 2.0`, and `fanout_length` points `(1, 2.0)` and `(3, 5.0)`.
Internal nets `n1` (fanout 1), `n2` (fanout 2, interpolated), and `n3` (fanout 5, slope-extrapolated)
receive lumped wire capacitance, while output-port nets `y` (with `set_load 0.004 pF`) and `z`
(unloaded port net) demonstrate the port-net rule [@liberty2017; @sdc2016; @bhasker2009static,
ch. 4].

### Derivations

1. **Fanout lengths `L(F)` (`STA-REQ-0123`):**
   - `L(1) = 2.0`;
   - `L(2) = 2.0 + (5.0 - 2.0) * (2 - 1) / (3 - 1) = 3.5`;
   - `L(5) = 5.0 + 2.0 * (5 - 3) = 9.0`;
   - `L(0) = max(0.0, 2.0 - 2.0 * (1 - 0)) = 0.0`.
2. **Net capacitances (`STA-REQ-0122` to `0125`):**
   - `C(n1) = 1 * 0.01 + 0.001 * L(1) = 0.012 pF`;
   - `C(n2) = 2 * 0.01 + 0.001 * L(2) = 0.0235 pF`;
   - `C(n3) = 5 * 0.01 + 0.001 * L(5) = 0.059 pF`;
   - `C(y) = 0.004 pF` (port net with explicit `set_load 0.004`, no wire-load table capacitance);
   - `C(z) = 0.002 pF` (or `0.0 pF` under `STA-REQ-0124` when port nets exclude wire load).
3. **Stage arrivals and transitions:**
   - `n1` (`r1/Q` at `C = 0.012 pF`): rise `0.412 ns` (slew `0.092 ns`),
     fall `0.3608 ns` (slew `0.082 ns`);
   - `n2` (`b1/Y` at `C = 0.0235 pF`):
     rise `0.412 + (0.20 + 0.0092 + 0.0235) = 0.6447 ns` (slew `0.0835 ns`),
     fall `0.3608 + (0.18 + 0.0082 + 0.0282) = 0.5772 ns` (slew `0.07585 ns`);
   - `n3` (`b2/Y` at `C = 0.059 pF`):
     rise `0.6447 + (0.20 + 0.00835 + 0.059) = 0.91205 ns` (slew `0.119 ns`),
     fall `0.5772 + (0.18 + 0.007585 + 0.0708) = 0.835585 ns` (slew `0.1149 ns`).
4. **Endpoint slacks:**
   - `r2/D`: setup `2.0 - 0.10 - 0.91205 = 0.98795 ns`, hold `0.835585 - 0.05 = 0.785585 ns`;
   - port `y` (`x1/Y` at `C = 0.004 pF`, rise `0.85705 ns`, fall `0.769585 ns`):
     setup `2.0 - 0.2 - 0.85705 = 0.94295 ns`, hold `0.769585 + 0.2 = 0.969585 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, output y, output z);
 wire n1, n2, n3, w3, w4, w5;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 BUF b1 (.A(n1), .Y(n2));
 BUF b2 (.A(n2), .Y(n3));
 BUF x1 (.A(n2), .Y(y));
 DFF r2 (.D(n3), .CK(clk), .Q());
 BUF x2 (.A(n3), .Y(z));
 BUF x3 (.A(n3), .Y(w3));
 BUF x4 (.A(n3), .Y(w4));
 BUF x5 (.A(n3), .Y(w5));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_output_delay -clock clk 0.2 [get_ports {y z}]
set_load 0.004 [get_ports y]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `L(1)` | `2.000000` |
| `L(2)` | `3.500000` |
| `L(5)` | `9.000000` |
| `L(0)` | `0.000000` |
| `C n1` | `0.012000` |
| `C n2` | `0.023500` |
| `C n3` | `0.059000` |
| `C y` | `0.004000` |
| `C z` | `0.002000` |
| `n1 rise` | `0.412000` |
| `n1 fall` | `0.360800` |
| `n2 rise` | `0.644700` |
| `n2 fall` | `0.577200` |
| `n3 rise` | `0.912050` |
| `n3 fall` | `0.835585` |
| `r2/D setup` | `0.987950` |
| `r2/D hold` | `0.785585` |
| `y setup` | `0.942950` |
| `y hold` | `0.969585` |
| `z setup` | `0.674050` |
| `z hold` | `1.229475` |
