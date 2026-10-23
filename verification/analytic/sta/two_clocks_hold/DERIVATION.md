# Setup and hold relations over the common period (STA-REQ-0220)

Path `r1 (CK = ca) -> b -> r2 (CK = cb)` is timed under three clock-pair variants (`p32`:
`T_A = 3, T_B = 2`; `p23`: `T_A = 2, T_B = 3`; `off`: `T_A = 3` with waveform `{0.5 2}`,
`T_B = 2`) [@bhasker2009static, ch. 7 and 8; @sapatnekar2004timing]. The data arrival at `r2/D` is
`late = 0.629 ns` (rise) and `early = 0.559 ns` (fall).

### Derivations

1. **Variant `p32` (`T_L = 3`, `T_C = 2`, `P = 6`, launches `{0, 3}`, captures `{0, 2, 4, 6}`):**
   - `D_s = min(2 - 0, 4 - 3) = 1.0 ns`;
   - `D_h = max(0 - 0, 2 - 3) = 0.0 ns` (whereas `D_s - T_C = -1.0 ns`);
   - `r2/D` setup `1.0 - 0.10 - 0.629 = 0.271 ns`, hold `0.559 - (0.0 + 0.05) = 0.509 ns`.
2. **Variant `p23` (`T_L = 2`, `T_C = 3`, `P = 6`, launches `{0, 2, 4}`, captures `{0, 3, 6}`):**
   - `D_s = min(3 - 0, 3 - 2, 6 - 4) = 1.0 ns`;
   - `D_h = max(0 - 0, 0 - 2, 3 - 4) = 0.0 ns` (whereas `D_s - T_C = -2.0 ns`);
   - `r2/D` setup `1.0 - 0.10 - 0.629 = 0.271 ns`, hold `0.559 - 0.05 = 0.509 ns`.
3. **Variant `off` (`T_L = 3` rising at `0.5`, `T_C = 2` rising at `0`, `P = 6`, launches
   `{0.5, 3.5}`):**
   - `D_s = min(2.0 - 0.5, 4.0 - 3.5) = 0.5 ns`;
   - `D_h = max(0.0 - 0.5, 2.0 - 3.5) = -0.5 ns` (whereas `D_s - T_C = -1.5 ns`);
   - `r2/D` setup `0.5 - 0.10 - 0.629 = -0.229 ns`, hold `0.559 - (-0.5 + 0.05) = 1.009 ns`.

## Netlist (`net.v`)

```verilog
module m (input ca, input cb, input a, output q);
 wire n1, n2;
 DFF r1 (.D(a), .CK(ca), .Q(n1));
 BUF b (.A(n1), .Y(n2));
 DFF r2 (.D(n2), .CK(cb), .Q(q));
endmodule
```

## Constraints (`p32.sdc`)

```tcl
create_clock -name A -period 3 [get_ports ca]
create_clock -name B -period 2 [get_ports cb]
```

## Constraints (`p23.sdc`)

```tcl
create_clock -name A -period 2 [get_ports ca]
create_clock -name B -period 3 [get_ports cb]
```

## Constraints (`off.sdc`)

```tcl
create_clock -name A -period 3 -waveform {0.5 2} [get_ports ca]
create_clock -name B -period 2 [get_ports cb]
```

## Expected values

### Variant `p32`

| Quantity | Value (ns / pF) |
|---|---|
| `D_s` | `1.000000` |
| `D_h` | `0.000000` |
| `legacy D_h` | `-1.000000` |
| `r2/D setup` | `0.271000` |
| `r2/D hold` | `0.509000` |
| `legacy r2/D hold` | `1.509000` |

### Variant `p23`

| Quantity | Value (ns / pF) |
|---|---|
| `D_s` | `1.000000` |
| `D_h` | `0.000000` |
| `legacy D_h` | `-2.000000` |
| `r2/D setup` | `0.271000` |
| `r2/D hold` | `0.509000` |
| `legacy r2/D hold` | `2.509000` |

### Variant `off`

| Quantity | Value (ns / pF) |
|---|---|
| `D_s` | `0.500000` |
| `D_h` | `-0.500000` |
| `legacy D_h` | `-1.500000` |
| `r2/D setup` | `-0.229000` |
| `r2/D hold` | `1.009000` |
| `legacy r2/D hold` | `2.009000` |
