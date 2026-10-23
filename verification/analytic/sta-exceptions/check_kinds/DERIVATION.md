# Exceptions on clock-gating checks and output delays (`check_kinds`)

- **Requirement**: `STA-REQ-0268` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `gate, pd` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Exceptions apply uniformly to all endpoint check kinds: setup/hold, recovery/removal, latch data
checks, output delays, and clock-gating checks. At a clock-gating enable pin, `set_multicycle_path`
shifts the leading-edge (setup) relation by $(N - 1) T_s$ and the trailing-edge (hold) relation by
$(N - 1) T_s - M T_h$, and `set_max_delay V` sets the setup required time to $V + \text{lat}_C - u_s$
(with check setup margin $s = 0$).

### Derivation on `gate` and `pd`

On [`gate`](../README.md#gate) (`clk` period `2.0 ns`, AND clock gate `g` with enable `g/B` driven
by `r1 -> be`):
- Enable arrival at `g/B`: `0.6290 ns` (rise) and `0.5590 ns` (fall).
- Baseline clock-gating relations at `g/B`: setup against leading rise at `2.0 ns` gives
  `2.0 - 0.6290 = 1.3710 ns`; hold against trailing fall at `1.0 ns` gives
  `0.5590 - 1.0 = -0.4410 ns`.
- `fp_to_gB` (`-to [get_pins g/B]`) or `fp_to_g` (`-to [get_cells g]`): disables the clock-gating
  checks at `g/B` (`-- / --`), leaving `r0 -> r2` at `1.2710 / 0.5090 ns`.
- `mcp2_to_gB` (`set_multicycle_path 2 -setup -to [get_pins g/B]`): shifts both relations by
  `+2.0 ns`, so clock-gating setup is `4.0 - 0.6290 = 3.3710 ns` and clock-gating hold is
  `0.5590 - 3.0 = -2.4410 ns`.
- `maxd_to_gB` (`set_max_delay 0.5 -to [get_pins g/B]`): setup required is `0.5 ns`, so
  clock-gating setup slack is `0.5 - 0.6290 = -0.1290 ns` (hold stays `-0.4410 ns`).

On [`pd`](../README.md#pd) (`out1` output delay `0.20 ns`):
- `mcp2_out1` (`set_multicycle_path 2 -setup -to [get_ports out1]`): shifts `r2 -> out1` setup by
  `+2.0 ns` from `1.1310 ns` to `3.1310 ns` and hold by `-2.0 ns` from `0.7270 ns` to `-1.2730 ns`.

| Circuit / Variant | Endpoint | Setup slack (ns) | Hold slack (ns) |
|---|---|---:|---:|
| `gate` / `base` | `g/B` | `1.3710` | `-0.4410` |
| `gate` / `fp_to_gB` | `g/B` | `--` | `--` |
| `gate` / `mcp2_to_gB` (`N=2`) | `g/B` | `3.3710` | `-2.4410` |
| `gate` / `maxd_to_gB` (`V=0.5`) | `g/B` | `-0.1290` | `-0.4410` |
| `pd` / `mcp2_out1` (`N=2`) | `out1` | `3.1310` | `-1.2730` |
