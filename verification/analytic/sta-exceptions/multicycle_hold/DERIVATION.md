# Multicycle hold relation with setup coupling and `-start`/`-end` (`multicycle_hold`)

- **Requirement**: `STA-REQ-0261` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `twoclk, reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

When a multicycle path (or no exception) wins the hold analysis, the hold check uses the relation:

$$R_{\text{hold}} = D_h + (N - 1) T_s - M T_h$$

where $(N, T_s)$ come from the winning setup multicycle path on the path (even if `set_false_path`
or `set_max_delay` overrides the setup check itself), $M$ is the hold multiplier from a winning
`-hold`-only multicycle path ($M = 0$ otherwise), and $T_h = T_L$ with `-start` (the default for
`-hold`) or $T_h = T_C$ with `-end`.

### Derivation on `twoclk` and `reconv`

On [`twoclk`](../README.md#twoclk) with `(T_A, T_B) = (4.0, 1.0)` (`D_h = 0.0`, fall arrival `0.559 ns`, `t_h = 0.05 ns`, baseline hold slack `0.5090 ns`):
- `ab_s3`: $N = 3, T_s = T_B = 1.0, M = 0 \implies R_{\text{hold}} = +2.0\text{ ns}$, hold slack `0.5090 - 2.0 = -1.4910 ns`.
- `ab_h2`: $N = 1, M = 2, T_h = T_A = 4.0$ (default `-start`) $\implies R_{\text{hold}} = -8.0\text{ ns}$, hold slack `0.5090 + 8.0 = 8.5090 ns`.
- `ab_h2_end`: $N = 1, M = 2, T_h = T_B = 1.0$ (`-end`) $\implies R_{\text{hold}} = -2.0\text{ ns}$, hold slack `0.5090 + 2.0 = 2.5090 ns`.
- `ab_s3_h2`: $N = 3, T_s = 1.0, M = 2, T_h = 4.0 \implies R_{\text{hold}} = 2.0 - 8.0 = -6.0\text{ ns}$, hold slack `0.5090 + 6.0 = 6.5090 ns`.
- `ab_s3start_h2end`: $N = 3, T_s = 4.0, M = 2, T_h = 1.0 \implies R_{\text{hold}} = 8.0 - 2.0 = +6.0\text{ ns}$, hold slack `0.5090 - 6.0 = -5.4910 ns`.

On [`reconv`](../README.md#reconv) (`T = 2.0 ns`, baseline `r1 -> r3` hold `0.9052 ns`, `r2 -> r3` hold `0.5090 ns`):
- `mcp2_h1`: `set_multicycle_path 2 -setup -from r1/CK` and `set_multicycle_path 1 -hold -from r1/CK`
  gives $(N - 1)T - M T = (1 - 1) \times 2 = 0$, restoring hold slack to `0.9052 ns`.
- `n_maxd_mcp`: `set_max_delay 1.5 -from r1/CK` and `set_multicycle_path 2 -setup -from r1/CK`:
  `max_delay` wins setup (`0.3370 ns`), while `N = 2` still shifts hold to `-1.0948 ns`.
- `n_clk_setup_pin_hold`: `set_multicycle_path 2 -setup -from clk` and
  `set_multicycle_path 1 -hold -from r1/CK`: `r1 -> r3` gets $N = 2$ from `clk` and $M = 1$ from
  `r1/CK` (hold slack `0.9052 ns`), while `r2 -> r3` gets $N = 2, M = 0$ (hold slack `-1.4910 ns`).

| Circuit / Variant | Path 1 setup (ns) | Path 1 hold (ns) | Path 2 setup (ns) | Path 2 hold (ns) |
|---|---:|---:|---:|---:|
| `twoclk (4,1)` / `ab_s3` | `2.2710` | `-1.4910` | `0.2710` | `0.5090` |
| `twoclk (4,1)` / `ab_h2` | `0.2710` | `8.5090` | `0.2710` | `0.5090` |
| `twoclk (4,1)` / `ab_h2_end` | `0.2710` | `2.5090` | `0.2710` | `0.5090` |
| `twoclk (4,1)` / `ab_s3_h2` | `2.2710` | `6.5090` | `0.2710` | `0.5090` |
| `twoclk (4,1)` / `ab_s3start_h2end` | `8.2710` | `-5.4910` | `0.2710` | `0.5090` |
| `twoclk (3,2)` / `ab_s3_h2` | `4.2710` | `2.5090` | `0.2710` | `0.5090` |
| `twoclk (3,2)` / `ab_s3start_h2end` | `6.2710` | `-1.4910` | `0.2710` | `0.5090` |
| `reconv` / `mcp2_h1` | `2.8370` | `0.9052` | `1.2710` | `0.5090` |
| `reconv` / `n_maxd_mcp` | `0.3370` | `-1.0948` | `1.2710` | `0.5090` |
| `reconv` / `n_clk_setup_pin_hold` | `2.8370` | `0.9052` | `3.2710` | `-1.4910` |
