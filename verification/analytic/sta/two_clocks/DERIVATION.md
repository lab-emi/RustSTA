# Two clocks with periods 3 and 2 (STA-REQ-0210)

Launches of clock A at 0 and 3 within the common period 6, captures of clock B at 0, 2, 4, 6.
The smallest positive capture-minus-launch difference is `4 - 3 = 1` (`D_s = 1`); the hold
relation is `1 - 2 = -1`. Setup `1 - 0.10 - late`, hold `early - (-1 + 0.05)`.
