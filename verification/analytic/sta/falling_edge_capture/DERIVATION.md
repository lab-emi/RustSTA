# Falling-edge capture (STA-REQ-0210, 0211)

Waveform `{0 0.6}`, period 2. A rising-edge flip-flop launches at 0 and a falling-edge flip-flop
captures: the first fall after 0 is at 0.6 (`D_s = 0.6`), so setup `0.6 - 0.10 - late`; the hold
relation is `D_s - T = -1.4`, so hold `early - (-1.4 + 0.05)`.
