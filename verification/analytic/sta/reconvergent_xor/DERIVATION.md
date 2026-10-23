# Reconvergent XOR (STA-REQ-0112, 0115)

`r1/Q` drives the XOR directly and through a buffer (load of `r1/Q`: 0.02 pF). The XOR is
non-unate: each of the four input arrivals (`Q` rise and fall, buffer output rise and fall) reaches
both output transitions, `a + XOR_R(s, 0.01)` and `a + XOR_F(s, 0.01)`. The late arrival at `r2/D`
is the largest of the eight, the early arrival the smallest; setup `2 - 0.10 - late`, hold
`early - 0.05`.
