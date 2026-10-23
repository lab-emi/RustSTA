# Exact path-slack minimum across reconvergent exception states (`path_semantics`)

- **Requirement**: `STA-REQ-0269` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

The slack of an endpoint in an analysis is the exact minimum, over all timed paths ending at that
endpoint, of each path's slack under its own winning exception. When paths from `r1/CK` and `r2/CK`
reconverge at `AND2 g` on [`reconv`](../README.md#reconv) under different exceptions, their arrival
states must not be merged into a single scalar arrival at `g/Y`.

### Derivation on `reconv`

On [`reconv`](../README.md#reconv), `r1 -> r3` has late arrival `1.0630 ns` (baseline setup slack
`0.8370 ns`, hold slack `0.9052 ns`) and `r2 -> r3` has late arrival `0.6290 ns` (baseline setup
slack `1.2710 ns`, hold slack `0.5090 ns`):
- Under `mcp2_h1` (`set_multicycle_path 2 -setup -from [get_pins r1/CK]` and
  `set_multicycle_path 1 -hold -from [get_pins r1/CK]`):
  - Path `r1 -> r3` has setup slack `3.9 - 1.0630 = 2.8370 ns` and hold slack `0.9052 ns`.
  - Path `r2 -> r3` has no exception, so its setup slack is `1.9 - 0.6290 = 1.2710 ns` and hold
    slack `0.5090 ns`.
  - The endpoint `r3/D` setup slack is therefore $\min(2.8370, 1.2710) = 1.2710\text{ ns}$ (governed
    by `r2 -> r3`, even though `r1 -> r3` has the larger arrival `1.0630 ns`!).
- Under `fp_from_pin` (`set_false_path -from [get_pins r1/CK]`):
  - Path `r1 -> r3` is untimed; `r3/D` setup slack is `1.2710 ns` and hold slack is `0.5090 ns`.
- Under `s_pin_vs_clk` (`N = 2` on `r1/CK`, `N = 3` on `clk`):
  - Path `r1 -> r3` has setup slack `2.8370 ns` and hold slack `-1.0948 ns`.
  - Path `r2 -> r3` has setup slack `5.2710 ns` and hold slack `-3.4910 ns`.
  - Endpoint `r3/D` setup slack is $\min(2.8370, 5.2710) = 2.8370\text{ ns}$, and hold slack is
    $\min(-1.0948, -3.4910) = -3.4910\text{ ns}$.

| Variant | `r1 -> r3` setup / hold (ns) | `r2 -> r3` setup / hold (ns) | Endpoint `r3/D` setup / hold (ns) |
|---|---|---|---|
| `base` | `0.8370 / 0.9052` | `1.2710 / 0.5090` | `0.8370 / 0.5090` |
| `mcp2_h1` | `2.8370 / 0.9052` | `1.2710 / 0.5090` | `1.2710 / 0.5090` |
| `fp_from_pin` | `-- / --` | `1.2710 / 0.5090` | `1.2710 / 0.5090` |
| `s_pin_vs_clk` | `2.8370 / -1.0948` | `5.2710 / -3.4910` | `2.8370 / -3.4910` |
