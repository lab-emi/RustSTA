# Unate sense refinement of XOR2 with a constant input (STA-REQ-0153)

Gate `g` (`XOR2`, `Y = A ^ B`, declared `timing_sense : non_unate`) has `B` tied to `TIEHI`
(`1`) and `A` driven by flip-flop `rc` (`qc`) [@liberty2017; @bhasker2009static, ch. 3].

### Derivations

1. **Sense refinement (`STA-REQ-0153`):**
   - Substituting `B = 1` gives `Y = A ^ 1 = !A` (`Y|_{A=0} = 1 > Y|_{A=1} = 0`), refining
     `g: A -> Y` from `non_unate` to `negative_unate` (and disabling `g: B -> Y`).
   - Therefore `qc` rise (`0.41 ns`, slew `0.09 ns`) produces only a `Y` fall, and `qc` fall
     (`0.359 ns`, slew `0.08 ns`) produces only a `Y` rise.
2. **Arrivals at `f/D` (`C = 0.01 pF`):**
   - `Y` fall (from `qc` rise): `0.41 + (0.28 + 0.20 * 0.09 + 2.2 * 0.01) = 0.41 + 0.32 = 0.73 ns`;
   - `Y` rise (from `qc` fall): `0.359 + (0.30 + 0.25 * 0.08 + 2.5 * 0.01) = 0.359 + 0.345 = 0.704 ns`;
   - (Without sense refinement, the spurious `qc` rise -> `Y` rise arc would give
     `0.41 + (0.30 + 0.25 * 0.09 + 0.025) = 0.7575 ns` and `qc` fall -> `Y` fall would give
     `0.359 + (0.28 + 0.20 * 0.08 + 0.022) = 0.677 ns`).
3. **Slacks at `f/D`:**
   - setup `2.0 - 0.10 - 0.73 = 1.17 ns` (vs `1.1425 ns` without sense refinement);
   - hold `0.704 - 0.05 = 0.654 ns` (vs `0.627 ns` without sense refinement).

## Netlist (`net.v`)

```verilog
module m (input clk, input c, output q);
 wire hi, qc, n1;
 TIEHI t1 (.Y(hi));
 DFF rc (.D(c), .CK(clk), .Q(qc));
 XOR2 g (.A(qc), .B(hi), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports c]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `Y fall` | `0.730000` |
| `Y rise` | `0.704000` |
| `f/D setup` | `1.170000` |
| `f/D hold` | `0.654000` |
| `non-unate f/D setup` | `1.142500` |
| `non-unate f/D hold` | `0.627000` |
