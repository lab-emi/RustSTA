# Constant propagation from a tie cell through an AND gate (STA-REQ-0150, STA-REQ-0162, STA-REQ-0159, STA-REQ-0164)

Tie cell `t0` (`TIELO`, `function : "0"`) drives input `B` of `AND2` gate `g1`, whose input `A`
is driven by flip-flop `ra` (`qa`). The output `n1` of `g1` feeds `OR2` gate `g2` alongside `rb/Q`
(`qb`), driving capture flip-flop `f1` [@liberty2017; @bhasker2009static, ch. 3 and 7; @sdc2016].

### Derivations

1. **Kleene propagation (`STA-REQ-0150`, `0162`):**
   - `t0/Y = lo = 0` -> `g1/B = 0`;
   - `g1/Y` (`A & B` with `B = 0`) evaluates to `X & 0 = 0` (`n1 = 0`).
2. **Arc disabling (`STA-REQ-0159`):**
   - `g1` output is constant `0`, so `g1: A -> Y` and `g1: B -> Y` are disabled (`reason = constant`);
   - at `g2` (`n1 | qb` with `n1 = 0`), `g2: A -> Y` from constant `n1` is disabled, while
     `g2: B -> Y` has residual function `0 | qb = qb` (`positive_unate`) and stays enabled;
   - the wires of the constant nets, `t0/Y -> g1/B` and `g1/Y -> g2/A`, pass no arrival
     (`STA-REQ-0164`), five arcs disabled with `reason = constant` in all.
3. **Arrivals and slacks at `f1/D`:**
   - Only `rb/Q -> g2/B -> g2/Y` reaches `f1/D`:
     rise `0.41 + 0.219 = 0.629 ns`, fall `0.359 + 0.20 = 0.559 ns`;
   - `f1/D` setup `2.0 - 0.10 - 0.629 = 1.271 ns` (vs `1.054 ns` if `ra -> g1 -> g2` were not
     disabled), hold `0.559 - 0.05 = 0.509 ns`.

## Netlist (`net.v`)

```verilog
module m (input clk, input a, input b, output q);
 wire lo, qa, qb, n1, n2;
 TIELO t0 (.Y(lo));
 DFF ra (.D(a), .CK(clk), .Q(qa));
 DFF rb (.D(b), .CK(clk), .Q(qb));
 AND2 g1 (.A(qa), .B(lo), .Y(n1));
 OR2 g2 (.A(n1), .B(qb), .Y(n2));
 DFF f1 (.D(n2), .CK(clk), .Q(q));
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
| `f1/D setup` | `1.271000` |
| `f1/D hold` | `0.509000` |
| `without constants f1/D setup` | `1.054000` |
