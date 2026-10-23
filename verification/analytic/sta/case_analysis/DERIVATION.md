# Case analysis, logic constants and explicit arc disabling (STA-REQ-0156, STA-REQ-0157)

Multiplexer `g` (`MUX2`, driving `f/D`) is timed under four SDC variants on the same netlist
[@sdc2016; @bhasker2009static, ch. 7]:
- `live`: no constants (`A0`, `A1` and `S` all timed);
- `case1`: `set_case_analysis 1 [get_ports s]` (selects `A1`, disables `A0 -> Y` and `S -> Y` with
  reason `case_analysis`);
- `logic0`: `set_logic_zero [get_ports s]` (selects `A0`, disables `A1 -> Y` and `S -> Y` with
  reason `case_analysis`);
- `case1dis`: `set_case_analysis 1 [get_ports s]` plus
  `set_disable_timing -from A1 -to Y [get_cells g]` (disables `A1 -> Y` with reason `user_disabled`,
  leaving `f/D` with no arrival).

### Derivations

1. **Path arrivals to `g/Y = f/D` (`C = 0.01 pF`):**
   - via `A0`: rise `0.629 ns`, fall `0.559 ns`;
   - via `A1` (through inverter `i`): rise `0.359 + 0.136 + 0.2165 = 0.7115 ns`,
     fall `0.41 + 0.1085 + 0.1972 = 0.7157 ns`;
   - via `S` (input delay `0.1 ns`, `s = 0`): rise `0.1 + 0.325 = 0.425 ns`,
     fall `0.1 + 0.302 = 0.402 ns`.
2. **Variant slacks at `f/D`:**
   - `live`: setup `2.0 - 0.10 - 0.7157 = 1.1843 ns`, hold `0.402 - 0.05 = 0.352 ns`;
   - `case1` (`A1` only): setup `2.0 - 0.10 - 0.7157 = 1.1843 ns`,
     hold `0.7115 - 0.05 = 0.6615 ns`;
   - `logic0` (`A0` only): setup `2.0 - 0.10 - 0.629 = 1.271 ns`,
     hold `0.559 - 0.05 = 0.509 ns`;
   - `case1dis`: all arcs into `g/Y` are disabled (`A0 -> Y` and `S -> Y` by `case_analysis`,
     `A1 -> Y` by `user_disabled`), so `f/D` has no arrival (`status = no_data_arrival`).

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input c, input s, output q);
 wire qa, qc, nc, n1;
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 INV i (.A(qc), .Y(nc));
 MUX2 g (.A0(qa), .A1(nc), .S(s), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
```

## Constraints (`live.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a c s}]
```

## Constraints (`case1.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a c s}]
set_case_analysis 1 [get_ports s]
```

## Constraints (`logic0.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a c s}]
set_logic_zero [get_ports s]
```

## Constraints (`case1dis.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a c s}]
set_case_analysis 1 [get_ports s]
set_disable_timing -from A1 -to Y [get_cells g]
```

## Expected values

### Variant `live`

| Quantity | Value (ns / pF) |
|---|---|
| `f/D setup` | `1.184300` |
| `f/D hold` | `0.352000` |

### Variant `case1`

| Quantity | Value (ns / pF) |
|---|---|
| `f/D setup` | `1.184300` |
| `f/D hold` | `0.661500` |

### Variant `logic0`

| Quantity | Value (ns / pF) |
|---|---|
| `f/D setup` | `1.271000` |
| `f/D hold` | `0.509000` |

### Variant `case1dis`

| Quantity | Value (ns / pF) |
|---|---|
| `f/D` | `no arrival` |
