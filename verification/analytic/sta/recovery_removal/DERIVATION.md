# Asynchronous recovery/removal checks and setup-first path ranking (STA-REQ-0224, STA-REQ-0228)

Flip-flop `r0` launches an active-low reset deassertion through three buffers `b1 -> b2 -> b3`
to the asynchronous clear pin `f1/RN` of `DFFR` (`recovery_rising = 0.12 ns` on rise only,
`removal_rising = 0.06 ns` on rise only, clock period `1.0 ns`) [@liberty2017;
@bhasker2009static, ch. 8; @hitchcock1982timing].

### Derivations

1. **Reset deassertion arrival at `f1/RN`:**
   - `r0/Q` rise (`C = 0.01 pF`): `0.41 ns`, slew `0.09 ns`;
   - `b1/Y`: `0.41 + 0.219 = 0.629 ns`, slew `0.07 ns`;
   - `b2/Y`: `0.629 + 0.217 = 0.846 ns`, slew `0.07 ns`;
   - `b3/Y = f1/RN`: `0.846 + 0.217 = 1.063 ns`, slew `0.07 ns`.
2. **Recovery and removal slacks at `f1/RN` (`async_default` group, `STA-REQ-0224`):**
   - recovery (rise only): `1.0 - 0.12 - 1.063 = -0.183 ns` (violating);
   - removal (rise only): `1.063 - 0.06 = 1.003 ns`;
   - falling transition at `f1/RN` has no constraint table (asserting edge) and is not checked.
3. **Synchronous endpoints and setup-first handle ranking (`STA-REQ-0228`):**
   - `r0/D` and `f1/D`: setup `1.0 - 0.10 - 0.2 = 0.70 ns`, hold `0.2 - 0.05 = 0.15 ns`;
   - output `q`: setup `1.0 - 0.3 - 0.40 = 0.30 ns`, hold `0.35 + 0.3 = 0.65 ns`;
   - overall late WNS is `min(0.0, -0.183, 0.70, 0.30) = -0.183 ns`, and `path1` starts at
     `r0/CK` (`clock` -> `launch` `r0/CK -> r0/Q`) and ends at `f1/RN` (`recovery`).

## Netlist (`net.v`)

```verilog
module m (input clk, input rst, input d, output q);
 wire rq, r1, r2, rn;
 DFF r0 (.D(rst), .CK(clk), .Q(rq));
 BUF b1 (.A(rq), .Y(r1));
 BUF b2 (.A(r1), .Y(r2));
 BUF b3 (.A(r2), .Y(rn));
 DFFR f1 (.D(d), .CK(clk), .RN(rn), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 1 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {rst d}]
set_output_delay -clock clk 0.3 [get_ports q]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `RN rise` | `1.063000` |
| `f1/RN recovery` | `-0.183000` |
| `f1/RN removal` | `1.003000` |
| `f1/D setup` | `0.700000` |
| `f1/D hold` | `0.150000` |
| `r0/D setup` | `0.700000` |
| `r0/D hold` | `0.150000` |
| `q setup` | `0.300000` |
| `q hold` | `0.650000` |
| `setup WNS` | `-0.183000` |
