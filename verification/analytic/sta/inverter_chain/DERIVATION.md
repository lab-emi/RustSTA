# Inverter chain (STA-REQ-0110 to 0115, 0201, 0210 to 0212)

`r1 (DFF) -> u1 (INV) -> u2 (INV) -> r2 (DFF)`, clock period 2 ns, input delay 0.5 ns on `d`,
output delay 0.3 ns and load 0.02 pF on `q`.

1. Launch at the rise at 0: `Q_rise = CKQ_R(0, 0.01)`, `Q_fall = CKQ_F(0, 0.01)`, transitions
   `CKQ_R.s(0.01)` and `CKQ_F.s(0.01)` (load: one inverter input).
2. `u1` is negative unate: `n2_fall = Q_rise + INV_F(s(Q_rise), 0.01)`, `n2_rise = Q_fall +
   INV_R(s(Q_fall), 0.01)`; `u2` likewise to `n3`.
3. Setup at `r2/D`: the capture edge is the rise at 2 (`D_s = 2`), so
   `slack = 2 - 0.10 - max(n3_rise, n3_fall)`. Hold: `D_h = 0`, `slack = min(n3) - 0.05`.
4. `r1/D` (input delay): setup `2 - 0.10 - 0.5`, hold `0.5 - 0.05`.
5. Output `q`: `Q` of `r2` at load 0.02: setup `2 - 0.3 - max(Q)`, hold `min(Q) + 0.3`.
