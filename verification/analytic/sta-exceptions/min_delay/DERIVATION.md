# Minimum delay (`set_min_delay`) required time (`min_delay`)

- **Requirement**: `STA-REQ-0263` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `pd` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

When `set_min_delay V` wins the hold analysis of a path, its required time measured from the
launch edge is:

$$t_{\text{req,hold}} = V + \text{lat}_C + h - v_{\min} + u_h$$

where $\text{lat}_C$ is the capture clock latency for a pin timing check (`0` for an output port),
$h$ is the hold time of the check, $v_{\min}$ is the `-min` output delay of an output port, and
$u_h$ is the hold uncertainty of the capture clock. The hold slack is the early arrival minus
$t_{\text{req,hold}}$.

### Derivation on `pd`

On [`pd`](../README.md#pd) (`clk` latency `0.10 ns`, hold uncertainty `0.02 ns`, `out1` output
delay `0.20 ns`):
- `min_rr` (`set_min_delay 0.5 -from [get_pins r1/CK] -to [get_pins r2/D]`):
  - Early arrival at `r2/D` is `0.10 + 0.359 + 0.200 = 0.6590 ns`.
  - Required time is `0.5 + 0.10 + 0.05 + 0.02 = 0.6700 ns`.
  - Hold slack is `0.6590 - 0.6700 = -0.0110 ns` (setup remains `1.2210 ns`).
- `min_ro` (`set_min_delay 0.5 -from [get_pins r2/CK] -to [get_ports out1]`):
  - Early arrival at `out1` is `0.10 + 0.359 + 0.188 = 0.6470 ns`.
  - Output port check has `lat_C = 0`: required time is `0.5 - 0.20 + 0.02 = 0.3200 ns`.
  - Hold slack is `0.6470 - 0.3200 = 0.3270 ns` (setup remains `1.1310 ns`).

| Variant | `r1 -> r2` setup (ns) | `r1 -> r2` hold (ns) | `r2 -> out1` setup (ns) | `r2 -> out1` hold (ns) |
|---|---:|---:|---:|---:|
| `base` | `1.2210` | `0.4890` | `1.1310` | `0.7270` |
| `min_rr` (`V=0.5`) | `1.2210` | `-0.0110` | `1.1310` | `0.7270` |
| `min_ro` (`V=0.5`) | `1.2210` | `0.4890` | `1.1310` | `0.3270` |
