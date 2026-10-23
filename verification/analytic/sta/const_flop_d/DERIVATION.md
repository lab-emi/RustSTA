# Sequential data and clock pins held constant (STA-REQ-0154)

Flip-flop `f` has its data pin `D` tied to `TIELO` (`0`) and its clock pin `CK` driven by `clk`,
while flip-flop `f2` has its clock pin `CK` tied to `TIEHI` (`1`) [@bhasker2009static, ch. 7;
@sdc2016].

### Derivations

1. **Constant data pin `f/D` (`STA-REQ-0154`):**
   - `f/D` is at constant `0`, so no setup or hold check is evaluated at `f/D` (`no_data_arrival`);
   - `f/CK` is clocked by `clk`, so `f: CK -> Q` still launches at `C(qd) = 0.02 pF` (`g/A` and
     `f2/D`): `qd` rise `0.42 ns` (slew `0.10 ns`), fall `0.368 ns` (slew `0.09 ns`).
2. **Constant clock pin `f2/CK` (`STA-REQ-0154`):**
   - `f2/CK` is at constant `1`, so `f2` is `unclocked` and its `D` pin is not a timed endpoint.
3. **Output port `y` (`g/Y` at `C = 0`):**
   - `y` rise `0.42 + (0.20 + 0.10 * 0.10 + 0) = 0.63 ns`,
     `y` fall `0.368 + (0.18 + 0.10 * 0.09 + 0) = 0.557 ns`;
   - `y` setup `2.0 - 0.2 - 0.63 = 1.17 ns`, hold `0.557 + 0.2 = 0.757 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, output y);
 wire lo, hi, qd, q2;
 TIELO t0 (.Y(lo));
 TIEHI t1 (.Y(hi));
 DFF f (.D(lo), .CK(clk), .Q(qd));
 BUF g (.A(qd), .Y(y));
 DFF f2 (.D(qd), .CK(hi), .Q(q2));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_output_delay -clock clk 0.2 [get_ports y]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `y setup` | `1.170000` |
| `y hold` | `0.757000` |
