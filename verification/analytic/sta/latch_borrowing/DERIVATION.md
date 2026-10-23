# Latch time borrowing (STA-REQ-0213)

A positive latch (`G` = clock) is open from the rise at 0 to the fall at 1. Data launched at 0
reaches `l/D` through two buffers at `a` with `0 < a < 1 - 0.10`: it arrives while the latch is
open and before the closing requirement, so the latch's setup slack is 0 (time borrowed). The
latch output is the later of its own launch (`CKQ` at the opening edge) and the borrowed data
(`a + DQ`), per transition, with the larger transition time. Through the inverter to `r2/D`:
setup `2 - 0.10 - late`.
