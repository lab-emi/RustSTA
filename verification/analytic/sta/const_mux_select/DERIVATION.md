# Multiplexer select constant and conditional arc disabling (STA-REQ-0155, STA-REQ-0117)

Multiplexer `g` (`MUX2`, `Y = (A0 & !S) | (A1 & S)`) has its select `S` tied to `TIELO` (`0`),
`A0` driven directly by `ra/Q`, and `A1` driven by `rb/Q` through inverter `i` [@liberty2017;
@bhasker2009static, ch. 3].

### Derivations

1. **Cofactor evaluation at `g` with `S = 0` (`STA-REQ-0155`):**
   - Residual function is `(A0 & 1) | (A1 & 0) = A0`, which is independent of `A1`;
   - `g: S -> Y` (constant input) and `g: A1 -> Y` (equal cofactors `A0 == A0`, or false `when`
     condition `S`) are disabled; only `g: A0 -> Y` remains enabled.
2. **Transition at `g/Y = f/D` (`STA-REQ-0117`, `C = 0.01 pF`):** the delay calculation ignores
   logic values, so the disabled arcs still carry their transitions (and no arrival). `S` is on
   the tie net (transition `0`); every model has `ms = 0`:
   - `A0 -> Y` and `A1 -> Y` (`BUF` model): rise `0.06 + 1.0 * 0.01 = 0.07 ns`, fall
     `0.05 + 1.1 * 0.01 = 0.061 ns`;
   - `S -> Y` (`XOR` model, non-unate): rise `0.07 + 1.8 * 0.01 = 0.088 ns`, fall
     `0.06 + 1.6 * 0.01 = 0.076 ns`;
   - late transition: rise `0.088 ns`, fall `0.076 ns`; early: rise `0.07 ns`, fall `0.061 ns`
     (with the enabled arc alone the late transition would be `0.07 ns` / `0.061 ns`).
3. **Arrivals and slacks at `f/D`:**
   - Through `A0`: rise `0.41 + 0.219 = 0.629 ns`, fall `0.359 + 0.20 = 0.559 ns`;
   - (Through disabled `A1`: rise `0.359 + 0.136 + 0.2165 = 0.7115 ns`,
     fall `0.41 + 0.1085 + 0.1972 = 0.7157 ns`);
   - `f/D` setup `2.0 - 0.10 - 0.629 = 1.271 ns` (vs `1.1843 ns` with `A1` enabled),
     hold `0.559 - 0.05 = 0.509 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, nb, n1;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 INV i (.A(qb), .Y(nb));
 MUX2 g (.A0(qa), .A1(nb), .S(lo), .Y(n1));
 DFF f (.D(n1), .CK(clk), .Q(q));
endmodule
```

## Constraints (`base.sdc`)

```tcl
create_clock -name clk -period 2 [get_ports clk]
set_input_delay -clock clk 0.1 [get_ports {a b}]
```

## Expected values

### Variant `base`

| Quantity | Value (ns / pF) |
|---|---|
| `A0 rise` | `0.629000` |
| `A0 fall` | `0.559000` |
| `A1 rise` | `0.711500` |
| `A1 fall` | `0.715700` |
| `f/D setup` | `1.271000` |
| `f/D hold` | `0.509000` |
| `with A1 enabled f/D setup` | `1.184300` |
| `g/Y late rise transition` | `0.088000` |
| `g/Y late fall transition` | `0.076000` |
| `g/Y early rise transition` | `0.070000` |
| `g/Y early fall transition` | `0.061000` |
