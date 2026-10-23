# `-ignore_clock_latency` on `set_max_delay` and `set_min_delay` (`ignore_clock_latency`)

- **Requirement**: `STA-REQ-0265` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `pd` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

When `set_max_delay` or `set_min_delay` is specified with `-ignore_clock_latency`, all launch and
capture clock latencies and clock uncertainties are omitted:
- Launch arrival excludes the launch clock latency (`0.10 ns` on `pd`).
- Setup required time is $V - s - v_{\max}$; hold required time is $V + h - v_{\min}$.

### Derivation on `pd`

On [`pd`](../README.md#pd):
- `maxi_rr` (`set_max_delay 1.0 -ignore_clock_latency -from r1/CK -to r2/D`):
  arrival without launch latency is `0.41 + 0.219 = 0.6290 ns`; required is `1.0 - 0.10 = 0.9000 ns`;
  setup slack is `0.9000 - 0.6290 = 0.2710 ns`.
- `mini_rr` (`set_min_delay 0.5 -ignore_clock_latency -from r1/CK -to r2/D`):
  arrival without launch latency is `0.359 + 0.200 = 0.5590 ns`; required is `0.5 + 0.05 = 0.5500 ns`;
  hold slack is `0.5590 - 0.5500 = 0.0090 ns`.
- `maxi_ro` (`set_max_delay 1.0 -ignore_clock_latency -from r2/CK -to out1`):
  arrival without launch latency is `0.41 + 0.209 = 0.6190 ns`; required is `1.0 - 0.20 = 0.8000 ns`;
  setup slack is `0.8000 - 0.6190 = 0.1810 ns`.
- `mini_ro` (`set_min_delay 0.5 -ignore_clock_latency -from r2/CK -to out1`):
  arrival without launch latency is `0.359 + 0.188 = 0.5470 ns`; required is `0.5 - 0.20 = 0.3000 ns`;
  hold slack is `0.5470 - 0.3000 = 0.2470 ns`.
- `maxi_ur` / `mini_ur` (`in3 -> r3/D`):
  arrival is `0.2100 ns` (rise) and `0.1920 ns` (fall); required is `1.0 - 0.10 = 0.9000 ns` (setup
  slack `0.9000 - 0.2100 = 0.6900 ns`) and `0.5 + 0.05 = 0.5500 ns` (hold slack
  `0.1920 - 0.5500 = -0.3580 ns`).
- `maxi_ru` / `mini_ru` (`r3/CK -> out4`):
  arrival without launch latency is `0.6190 ns` (rise) and `0.5470 ns` (fall); required is `1.0 ns`
  (setup slack `0.3810 ns`) and `0.5 ns` (hold slack `0.0470 ns`).

| Variant | Path | Setup slack (ns) | Hold slack (ns) |
|---|---|---:|---:|
| `maxi_rr` (`V=1.0`) | `r1/CK -> r2/D` | `0.2710` | `0.4890` |
| `mini_rr` (`V=0.5`) | `r1/CK -> r2/D` | `1.2210` | `0.0090` |
| `maxi_ro` (`V=1.0`) | `r2/CK -> out1` | `0.1810` | `0.7270` |
| `mini_ro` (`V=0.5`) | `r2/CK -> out1` | `1.1310` | `0.2470` |
| `maxi_ur` (`V=1.0`) | `in3 -> r3/D` | `0.6900` | `--` |
| `mini_ur` (`V=0.5`) | `in3 -> r3/D` | `--` | `-0.3580` |
| `maxi_ru` (`V=1.0`) | `r3/CK -> out4` | `0.3810` | `--` |
| `mini_ru` (`V=0.5`) | `r3/CK -> out4` | `--` | `0.0470` |
