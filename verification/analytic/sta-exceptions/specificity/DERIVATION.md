# Specificity ranking within an exception type (`specificity`)

- **Requirement**: `STA-REQ-0257` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv, xbar` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Among exceptions of the same type, the exception with the highest specificity weight for the path
wins, where specificity is the sum of:
- `16` for a matched `-from` port, pin, or cell
- `8` for a matched `-to` port, pin, or cell
- `4` when the exception specifies `-through`
- `2` for a matched `-from` clock
- `1` for a matched `-to` clock

### Derivation on `reconv` and `xbar`

```sdc
# On reconv:
# s_pin_vs_clk: -from pin (16, N=2) vs -from clk (2, N=3) -> r1 gets N=2, r2 gets N=3
set_multicycle_path 2 -setup -from [get_pins r1/CK]
set_multicycle_path 3 -setup -from [get_clocks clk]

# s_from_vs_to: -from pin (16, N=3) vs -to pin (8, N=2) -> r1 gets N=3, r2 gets N=2
set_multicycle_path 2 -setup -to [get_pins r3/D]
set_multicycle_path 3 -setup -from [get_pins r1/CK]

# s_clkpin_vs_topin: -from clk -to pin (2+8=10, N=3) vs -to pin (8, N=2) -> both get N=3
set_multicycle_path 3 -setup -from [get_clocks clk] -to [get_pins r3/D]
set_multicycle_path 2 -setup -to [get_pins r3/D]

# s_topin_vs_clkclk: -to pin (8, N=3) vs -from clk -to clk (2+1=3, N=2) -> both get N=3
set_multicycle_path 3 -setup -to [get_pins r3/D]
set_multicycle_path 2 -setup -from [get_clocks clk] -to [get_clocks clk]

# s_fromclk_vs_toclk: -from clk (2, N=2) vs -to clk (1, N=3) -> both get N=2
set_multicycle_path 3 -setup -to [get_clocks clk]
set_multicycle_path 2 -setup -from [get_clocks clk]

# On xbar:
# thr_vs_fromclk: -through g1/A (4, N=3) vs -from clk -to clk (3, N=2) -> s13 gets N=3 (5.2600 ns)
set_multicycle_path 3 -setup -through [get_pins g1/A]
set_multicycle_path 2 -setup -from [get_clocks clk] -to [get_clocks clk]

# thr_vs_topin: -through g1/A (4, N=3) vs -to r3/D (8, N=2) -> s13 and s23 get N=2 (3.2600 ns)
set_multicycle_path 3 -setup -through [get_pins g1/A]
set_multicycle_path 2 -setup -to [get_pins r3/D]
```

| Circuit / Variant | Slacks (ns) | Winning specificities |
|---|---|---|
| `reconv` / `s_pin_vs_clk` | `s1 = 2.8370, h1 = -1.0948, s2 = 5.2710, h2 = -3.4910` | `r1`: `16 > 2` (`N=2`); `r2`: `2` (`N=3`) |
| `reconv` / `s_from_vs_to` | `s1 = 4.8370, h1 = -3.0948, s2 = 3.2710, h2 = -1.4910` | `r1`: `16 > 8` (`N=3`); `r2`: `8` (`N=2`) |
| `reconv` / `s_clkpin_vs_topin` | `s1 = 4.8370, h1 = -3.0948, s2 = 5.2710, h2 = -3.4910` | `10 > 8` (`N=3` on both) |
| `reconv` / `s_topin_vs_clkclk` | `s1 = 4.8370, h1 = -3.0948, s2 = 5.2710, h2 = -3.4910` | `8 > 3` (`N=3` on both) |
| `reconv` / `s_fromclk_vs_toclk` | `s1 = 2.8370, h1 = -1.0948, s2 = 3.2710, h2 = -1.4910` | `2 > 1` (`N=2` on both) |
| `xbar` / `thr_vs_fromclk` | `s13 = 5.2600, s14 = 3.1300, s23 = 3.2600, s24 = 3.1300` | `s13`: `4 > 3` (`N=3`); others: `3` (`N=2`) |
| `xbar` / `thr_vs_topin` | `s13 = 3.2600, s14 = 1.1300, s23 = 3.2600, s24 = 1.1300` | `s13, s23`: `8 > 4` (`N=2`) |
