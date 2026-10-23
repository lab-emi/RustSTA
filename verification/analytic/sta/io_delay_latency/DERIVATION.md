# Input/output delay clock latency, latency-inclusion flags and driving cells (STA-REQ-0217, STA-REQ-0218)

Three input-to-register paths (`d -> b1 -> r1`, `e -> b2 -> r2`, `f -> b3 -> r3`) and two
register-to-output paths (`r1 -> q`, `r2 -> p`) are timed under two SDC variants:
- `base`: source latency `0.2 ns`, network latency `-max 0.3 ns` / `-min 0.1 ns`,
  `-network_latency_included` on `e`, `set_driving_cell -lib_cell BUF -pin Y` on `f`, and
  `-source_latency_included` on `p`;
- `rf`: per-edge clock latency `-rise 0.3 ns` / `-fall 0.2 ns`, with `-clock_fall` on `d` and `q`
[@sdc2016; @liberty2017; @bhasker2009static, ch. 7].

### Derivations (`base`)

1. **Clock latencies:** `lat_late = 0.2 + 0.3 = 0.5 ns`, `lat_early = 0.2 + 0.1 = 0.3 ns`.
2. **Input `d` (`set_input_delay 0.5`, full latency):**
   - start arrival: late `0.5 + 0.5 = 1.0 ns`, early `0.5 + 0.3 = 0.8 ns`;
   - after `b1` (`s = 0`, `C = 0.01 pF`, rise delay `0.21 ns`, fall delay `0.192 ns`):
     `r1/D` late `1.21 ns`, early `0.992 ns`;
   - `r1/D` setup `2.0 + 0.3 - 0.10 - 1.21 = 0.99 ns`,
     hold `0.992 - (0.5 + 0.05) = 0.442 ns`.
3. **Input `e` (`set_input_delay 0.5 -network_latency_included`):**
   - start arrival adds only `lat_src = 0.2 ns`: `0.7 ns` (late and early);
   - `r2/D` late `0.7 + 0.21 = 0.91 ns`, early `0.7 + 0.192 = 0.892 ns`;
   - `r2/D` setup `2.0 + 0.3 - 0.10 - 0.91 = 1.29 ns`,
     hold `0.892 - (0.5 + 0.05) = 0.342 ns`.
4. **Input `f` (`set_input_delay 0.5` + `set_driving_cell BUF`):**
   - drive delay `delay(0, 0.01) - delay(0, 0)`: rise `0.21 - 0.20 = 0.01 ns` (slew `0.07 ns`),
     fall `0.192 - 0.18 = 0.012 ns` (slew `0.061 ns`);
   - after `b3`: late rise `1.0 + 0.01 + (0.20 + 0.007 + 0.01) = 1.227 ns`,
     early fall `0.8 + 0.012 + (0.18 + 0.0061 + 0.012) = 1.0101 ns`;
   - `r3/D` setup `2.0 + 0.3 - 0.10 - 1.227 = 0.973 ns`,
     hold `1.0101 - 0.55 = 0.4601 ns`.
5. **Outputs `q` and `p` (`CK -> Q` at `C = 0`: rise `0.40 ns`, fall `0.35 ns`):**
   - `q` (full capture latency): setup `2.0 + 0.3 - 0.4 - (0.5 + 0.40) = 1.00 ns`,
     hold `(0.3 + 0.35) - (0.5 - 0.4) = 0.55 ns`;
   - `p` (`-source_latency_included`, capture uses only `lat_net`):
     setup `2.0 + 0.1 - 0.4 - (0.5 + 0.40) = 0.80 ns`,
     hold `(0.3 + 0.35) - (0.3 - 0.4) = 0.75 ns`.

### Derivations (`rf`)

1. `d` launched on clock fall (`t_L = 1.0 ns`, `lat_fall = 0.2 ns`), captured on rise (`t_C = 2.0 ns`,
   `lat_rise = 0.3 ns`, `D_s = 1.0 ns`, `D_h = -1.0 ns`):
   - `r1/D` late `0.5 + 0.2 + 0.21 = 0.91 ns`, early `0.5 + 0.2 + 0.192 = 0.892 ns`;
   - `r1/D` setup `1.0 + 0.3 - 0.10 - 0.91 = 0.29 ns`,
     hold `0.892 - (-1.0 + 0.3 + 0.05) = 1.542 ns`.
2. `q` launched on rise (`lat_rise = 0.3 ns`) and captured on fall (`lat_fall = 0.2 ns`,
   `D_s = 1.0 ns`, `D_h = -1.0 ns`):
   - `q` setup `1.0 + 0.2 - 0.4 - (0.3 + 0.40) = 0.10 ns`,
     hold `(0.3 + 0.35) - (-1.0 + 0.2 - 0.4) = 1.85 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input d, input e, input f, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(q));
 BUF b2 (.A(e), .Y(n2));
 DFF r2 (.D(n2), .CK(clk), .Q(p));
 BUF b3 (.A(f), .Y(n3));
 DFF r3 (.D(n3), .CK(clk), .Q());
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency -source 0.2 [get_clocks clk]
set_clock_latency -max 0.3 [get_clocks clk]
set_clock_latency -min 0.1 [get_clocks clk]
set_input_delay -clock clk 0.5 [get_ports d]
set_input_delay -clock clk 0.5 -network_latency_included [get_ports e]
set_input_delay -clock clk 0.5 [get_ports f]
set_driving_cell -lib_cell BUF -pin Y [get_ports f]
set_output_delay -clock clk 0.4 [get_ports q]
set_output_delay -clock clk 0.4 -source_latency_included [get_ports p]
```

## Constraints (`rf.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_clock_latency -rise 0.3 [get_clocks clk]
set_clock_latency -fall 0.2 [get_clocks clk]
set_input_delay -clock clk -clock_fall 0.5 [get_ports d]
set_input_delay -clock clk 0.5 [get_ports e]
set_input_delay -clock clk 0.5 [get_ports f]
set_output_delay -clock clk -clock_fall 0.4 [get_ports q]
set_output_delay -clock clk 0.4 [get_ports p]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `d start late` | `1.000000` |
| `d start early` | `0.800000` |
| `r1/D setup` | `0.990000` |
| `r1/D hold` | `0.442000` |
| `r2/D setup` | `1.290000` |
| `r2/D hold` | `0.342000` |
| `f drive delay rise` | `0.010000` |
| `f drive delay fall` | `0.012000` |
| `f slew rise` | `0.070000` |
| `f slew fall` | `0.061000` |
| `r3/D setup` | `0.973000` |
| `r3/D hold` | `0.460100` |
| `q setup` | `1.000000` |
| `q hold` | `0.550000` |
| `p setup` | `0.800000` |
| `p hold` | `0.750000` |

### Variant `rf`

| Quantity | Value (ns / pF) |
|---|---|
| `r1/D setup` | `0.290000` |
| `r1/D hold` | `1.542000` |
| `r2/D setup` | `1.190000` |
| `r2/D hold` | `0.642000` |
| `q setup` | `0.100000` |
| `q hold` | `1.850000` |
| `p setup` | `1.200000` |
| `p hold` | `0.750000` |
