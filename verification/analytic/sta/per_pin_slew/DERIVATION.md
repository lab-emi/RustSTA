# Per-pin worst slew including untagged arcs (STA-REQ-0108, STA-REQ-0109)

Flip-flop `r1` drives both data inputs `A0` and `A1` of multiplexer `x` (`MUX2`), while primary
input `s` drives `x/S` with `set_input_transition 0.4 ns` and no `set_input_delay` (so `s` carries
a transition but no launch tag) [@bhasker2009static, ch. 3; @hitchcock1982timing].

### Derivations

1. **Launch at `r1/Q` (`C = 0.02 pF` from `x/A0` and `x/A1`):**
   - rise `0.40 + 1.0 * 0.02 = 0.42 ns`, slew `0.08 + 1.0 * 0.02 = 0.10 ns`;
   - fall `0.35 + 0.9 * 0.02 = 0.368 ns`, slew `0.07 + 1.0 * 0.02 = 0.09 ns`.
2. **At `x/Y` (`n2`, `C = 0.01 pF`):**
   - Tagged arrivals from `A0` and `A1` (`BUF` model):
     rise `0.42 + (0.20 + 0.10 * 0.10 + 0.01) = 0.64 ns` (arc slew `0.06 + 0.01 = 0.07 ns`),
     fall `0.368 + (0.18 + 0.10 * 0.09 + 0.012) = 0.569 ns` (arc slew `0.05 + 0.011 = 0.061 ns`).
   - Untagged arc `S -> Y` (`XOR` model, `s_in = 0.4 ns`, `C = 0.01 pF`):
     output rise slew `0.07 + 1.8 * 0.01 = 0.088 ns`,
     output fall slew `0.06 + 1.6 * 0.01 = 0.076 ns`.
   - Per-pin slews at `x/Y` (`STA-REQ-0108`):
     late rise `max(0.07, 0.088) = 0.088 ns`, late fall `max(0.061, 0.076) = 0.076 ns`;
     early rise `min(0.07, 0.088) = 0.07 ns`, early fall `min(0.061, 0.076) = 0.061 ns`.
3. **Downstream buffer `b` (`n2 -> n3 = r2/D`, `C = 0.01 pF`, `STA-REQ-0109`):**
   - late rise `0.64 + (0.20 + 0.10 * 0.088 + 0.01) = 0.8588 ns`,
     late fall `0.569 + (0.18 + 0.10 * 0.076 + 0.012) = 0.7686 ns`;
   - early rise `0.64 + (0.20 + 0.10 * 0.07 + 0.01) = 0.857 ns`,
     early fall `0.569 + (0.18 + 0.10 * 0.061 + 0.012) = 0.7671 ns`;
   - `r2/D` setup `2.0 - 0.10 - 0.8588 = 1.0412 ns` (vs `1.043 ns` under the superseded per-tag
     slew rule); `r2/D` hold `0.7671 - 0.05 = 0.7171 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input s, output q);
 wire n1, n2, n3;
 DFF r1 (.D(a), .CK(clk), .Q(n1));
 MUX2 x (.A0(n1), .A1(n1), .S(s), .Y(n2));
 BUF b (.A(n2), .Y(n3));
 DFF r2 (.D(n3), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports a]
set_input_transition 0.4 [get_ports s]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `Q rise` | `0.420000` |
| `Q fall` | `0.368000` |
| `Q slew rise` | `0.100000` |
| `Q slew fall` | `0.090000` |
| `Y rise` | `0.640000` |
| `Y fall` | `0.569000` |
| `Y slew rise late` | `0.088000` |
| `Y slew fall late` | `0.076000` |
| `Y slew rise early` | `0.070000` |
| `Y slew fall early` | `0.061000` |
| `r2/D setup` | `1.041200` |
| `r2/D hold` | `0.717100` |
| `legacy r2/D setup` | `1.043000` |
