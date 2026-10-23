# Unclocked input and output ends under path delays (`unclocked_ends`)

- **Requirement**: `STA-REQ-0264` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `pd` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

An input port without `set_input_delay` has no launch clock and is timed in an analysis only when
`set_max_delay` (setup) or `set_min_delay` (hold) wins the path, starting with arrival `0.0 ns` at
the port. An output port without `set_output_delay` is an endpoint only for such delays, with
neither capture clock nor output delay, so its required time is simply $V$.

### Derivation on `pd`

On [`pd`](../README.md#pd), `in3` and `in4` have no input delay and `out3` and `out4` have no
output delay:
- `in3 -> b3 -> r3/D` (`ur`): arrival `0.2100 ns` (rise) and `0.1920 ns` (fall); `r3/D` is clocked
  by `clk` (`lat_C = 0.10 ns`, `t_su = 0.10 ns`, `u_s = 0.05 ns`, `t_h = 0.05 ns`, `u_h = 0.02 ns`):
  - `max_ur` (`set_max_delay 1.0 -from in3 -to r3/D`): required `1.0 + 0.10 - 0.10 - 0.05 = 0.9500 ns`,
    setup slack `0.9500 - 0.2100 = 0.7400 ns` (hold untimed `--`).
  - `min_ur` (`set_min_delay 0.5 -from in3 -to r3/D`): required `0.5 + 0.10 + 0.05 + 0.02 = 0.6700 ns`,
    hold slack `0.1920 - 0.6700 = -0.4780 ns` (setup untimed `--`).
- `r3/CK -> b4 -> out4` (`ru`): arrival `0.7190 ns` (rise) and `0.6470 ns` (fall); `out4` has no
  capture clock (`required = V`):
  - `max_ru` (`set_max_delay 1.0 -from r3/CK -to out4`): setup slack `1.0 - 0.7190 = 0.2810 ns`.
  - `min_ru` (`set_min_delay 0.5 -from r3/CK -to out4`): hold slack `0.6470 - 0.5 = 0.1470 ns`.
- `in4 -> b5 -> out3` (`uu`): arrival `0.2000 ns` (rise) and `0.1800 ns` (fall); both ends
  unclocked (`required = V`):
  - `max_uu` (`set_max_delay 1.0 -from in4 -to out3`): setup slack `1.0 - 0.2000 = 0.8000 ns`.
  - `min_uu` (`set_min_delay 0.5 -from in4 -to out3`): hold slack `0.1800 - 0.5 = -0.3200 ns`.

| Variant | Path | Setup slack (ns) | Hold slack (ns) |
|---|---|---:|---:|
| `max_ur` (`V=1.0`) | `in3 -> r3/D` | `0.7400` | `--` |
| `min_ur` (`V=0.5`) | `in3 -> r3/D` | `--` | `-0.4780` |
| `max_ru` (`V=1.0`) | `r3/CK -> out4` | `0.2810` | `--` |
| `min_ru` (`V=0.5`) | `r3/CK -> out4` | `--` | `0.1470` |
| `max_uu` (`V=1.0`) | `in4 -> out3` | `0.8000` | `--` |
| `min_uu` (`V=0.5`) | `in4 -> out3` | `--` | `-0.3200` |
