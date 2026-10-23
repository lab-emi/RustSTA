# One-sided -max and -min I/O delays (STA-REQ-0219)

Input `d` has only `set_input_delay -max 0.5`, input `e` has only `set_input_delay -min 0.1`,
output `q` has only `set_output_delay -min -0.1`, and output `p` has only
`set_output_delay -max 0.4` [@sdc2016; @bhasker2009static, ch. 7]. Each one-sided constraint times
only its specified bound and leaves the opposite bound unset.

### Derivations

1. **`r1/D` (from `d`, `-max 0.5` only):**
   - late arrival `0.5 + 0.21 = 0.71 ns`, setup `2.0 - 0.10 - 0.71 = 1.19 ns`; no hold check.
2. **`r2/D` (from `e`, `-min 0.1` only):**
   - early arrival `0.1 + 0.192 = 0.292 ns`, hold `0.292 - 0.05 = 0.242 ns`; no setup check.
3. **Output `q` (`r1 -> b2 -> q`, `-min -0.1` only):**
   - early fall arrival `0.359 + (0.18 + 0.10 * 0.08 + 0) = 0.547 ns`,
     hold `0.547 - (0.0 - (-0.1)) = 0.447 ns`; no setup check.
4. **Output `p` (`r2/Q`, `-max 0.4` only, `C = 0`):**
   - late rise arrival `0.40 ns`, setup `2.0 - 0.4 - 0.40 = 1.20 ns`; no hold check.

## Netlist (`net.v`)

```verilog
module m (input clk, input d, input e, output q, output p);
 wire n1, n2, n3;
 BUF b1 (.A(d), .Y(n1));
 DFF r1 (.D(n1), .CK(clk), .Q(n2));
 BUF b2 (.A(n2), .Y(q));
 BUF b3 (.A(e), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(p));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk -max 0.5 [get_ports d]
set_input_delay -clock clk -min 0.1 [get_ports e]
set_output_delay -clock clk -min -0.1 [get_ports q]
set_output_delay -clock clk -max 0.4 [get_ports p]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `r1/D setup` | `1.190000` |
| `r2/D hold` | `0.242000` |
| `q hold` | `0.447000` |
| `p setup` | `1.200000` |
