# Three-state bus and enable/disable propagation (STA-REQ-0013, STA-REQ-0014, STA-REQ-0223)

Two three-state buffers `z1` and `z2` (`BUFZ`, `three_state : "!EN"`, output pin capacitance
`COUT_Z = 0.005 pF`) drive the shared bus net `y`, which also drives inverter `i2` (`0.01 pF`) and
top-level output port `y` (`set_load 0.02 pF`). The library tables use the two-axis template `t2`
(`input_net_transition`, `total_output_net_capacitance`) [@liberty2017; @bhasker2009static, ch. 3].

### Derivations

1. **Bus capacitance (`STA-REQ-0223`):**
   `C_bus = C(i2/A) + C(z1/Z) + C(z2/Z) + load(y) = 0.01 + 0.005 + 0.005 + 0.02 = 0.04 pF`.
2. **Register launches (`r1/Q = qa`, `r2/Q = qe`, each driving two `0.01 pF` loads, `C = 0.02 pF`):**
   - `qa` and `qe` rise: `0.40 + 1.0 * 0.02 = 0.42 ns`, slew `0.08 + 1.0 * 0.02 = 0.10 ns`;
   - `qa` and `qe` fall: `0.35 + 0.9 * 0.02 = 0.368 ns`, slew `0.07 + 1.0 * 0.02 = 0.09 ns`.
3. **Inverter `i1` (`qe -> qe2`, `C = 0.01 pF`):**
   - `qe2` fall (from `qe` rise): `0.42 + (0.08 + 0.15 * 0.10 + 1.5 * 0.01) = 0.53 ns`,
     slew `0.04 + 1.2 * 0.01 = 0.052 ns`;
   - `qe2` rise (from `qe` fall): `0.368 + (0.10 + 0.20 * 0.09 + 2.0 * 0.01) = 0.506 ns`,
     slew `0.05 + 1.5 * 0.01 = 0.065 ns`.
4. **Candidate arcs onto bus `y` (`C_bus = 0.04 pF`, `STA-REQ-0013`, `0223`):**
   - `z1, z2` data `A -> Z` (`BUF`): rise `0.42 + (0.20 + 0.10 * 0.10 + 1.0 * 0.04) = 0.67 ns`
     (slew `0.06 + 1.0 * 0.04 = 0.10 ns`); fall `0.368 + (0.18 + 0.10 * 0.09 + 1.2 * 0.04) = 0.605 ns`
     (slew `0.05 + 1.1 * 0.04 = 0.094 ns`).
   - `z1` enable `EN -> Z` (`three_state_enable`, positive unate from `qe` rise):
     rise `0.42 + (0.15 + 0.10 * 0.10 + 1.0 * 0.04) = 0.62 ns` (slew `0.10 ns`);
     fall `0.42 + (0.13 + 0.10 * 0.10 + 1.0 * 0.04) = 0.60 ns` (slew `0.09 ns`).
   - `z1` disable `EN -> Z` (`three_state_disable`, negative unate from `qe` fall):
     rise `0.368 + (0.09 + 0.05 * 0.09) = 0.4625 ns` (slew `0.0 ns`);
     fall `0.368 + (0.07 + 0.05 * 0.09) = 0.4425 ns` (slew `0.0 ns`).
   - `z2` enable `EN -> Z` (from `qe2` rise at `0.506 ns`, slew `0.065 ns`):
     rise `0.506 + (0.15 + 0.10 * 0.065 + 0.04) = 0.7025 ns` (slew `0.10 ns`);
     fall `0.506 + (0.13 + 0.10 * 0.065 + 0.04) = 0.6825 ns` (slew `0.09 ns`).
   - `z2` disable `EN -> Z` (from `qe2` fall at `0.53 ns`, slew `0.052 ns`):
     rise `0.53 + (0.09 + 0.05 * 0.052) = 0.6226 ns` (slew `0.0 ns`);
     fall `0.53 + (0.07 + 0.05 * 0.052) = 0.6026 ns` (slew `0.0 ns`).
5. **Merged bus `y` and downstream `r3/D` slacks:**
   - `y` arrivals: rise `[0.4625, 0.7025] ns`, fall `[0.4425, 0.6825] ns`;
   - `y` slews: rise `[0.0, 0.10] ns`, fall `[0.0, 0.094] ns`;
   - port `y`: setup `2.0 - 0.3 - 0.7025 = 0.9975 ns`, hold `0.4425 - (-0.3) = 0.7425 ns`;
   - through `i2` to `r3/D` (`C = 0.01 pF`):
     late fall `0.7025 + (0.08 + 0.15 * 0.10 + 0.015) = 0.8125 ns`,
     late rise `0.6825 + (0.10 + 0.20 * 0.094 + 0.02) = 0.8213 ns`,
     so `r3/D` setup `2.0 - 0.10 - 0.8213 = 1.0787 ns`;
     early fall `0.4625 + (0.08 + 0 + 0.015) = 0.5575 ns`,
     early rise `0.4425 + (0.10 + 0 + 0.02) = 0.5625 ns`,
     so `r3/D` hold `0.5575 - 0.05 = 0.5075 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input b, output y, output q);
 wire qa, qe, qe2, n1;
 DFF r1 (.D(a), .CK(clk), .Q(qa));
 DFF r2 (.D(b), .CK(clk), .Q(qe));
 INV i1 (.A(qe), .Y(qe2));
 BUFZ z1 (.A(qa), .EN(qe), .Z(y));
 BUFZ z2 (.A(qa), .EN(qe2), .Z(y));
 INV i2 (.A(y), .Y(n1));
 DFF r3 (.D(n1), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.2 [get_ports {a b}]
set_output_delay -clock clk 0.3 [get_ports {y q}]
set_load 0.02 [get_ports y]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `C_bus` | `0.040000` |
| `arc z1,z2 A->Z rise` | `0.670000`, `0.100000` |
| `arc z1 enable Z->1 rise` | `0.620000`, `0.100000` |
| `arc z1 disable 0->Z rise` | `0.462500`, `0.000000` |
| `arc z2 enable Z->1 rise` | `0.702500`, `0.100000` |
| `arc z2 disable 0->Z rise` | `0.622600`, `0.000000` |
| `arc z1,z2 A->Z fall` | `0.605000`, `0.094000` |
| `arc z1 enable Z->0 fall` | `0.600000`, `0.090000` |
| `arc z1 disable 1->Z fall` | `0.442500`, `0.000000` |
| `arc z2 enable Z->0 fall` | `0.682500`, `0.090000` |
| `arc z2 disable 1->Z fall` | `0.602600`, `0.000000` |
| `y arr rise late` | `0.702500` |
| `y arr rise early` | `0.462500` |
| `y arr fall late` | `0.682500` |
| `y arr fall early` | `0.442500` |
| `y slew rise late` | `0.100000` |
| `y slew rise early` | `0.000000` |
| `y slew fall late` | `0.094000` |
| `y slew fall early` | `0.000000` |
| `y setup` | `0.997500` |
| `y hold` | `0.742500` |
| `r3/D setup` | `1.078700` |
| `r3/D hold` | `0.507500` |
