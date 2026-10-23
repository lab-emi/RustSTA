# Maximum delay (`set_max_delay`) required time (`max_delay`)

- **Requirement**: `STA-REQ-0262` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `pd, edges` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

When `set_max_delay V` wins the setup analysis of a path, its required time measured from the
launch edge is:

$$t_{\text{req,setup}} = V + \text{lat}_C - s - v_{\max} - u_s$$

where $\text{lat}_C$ is the capture clock latency for a pin timing check (`0` for an output port),
$s$ is the setup time of the check, $v_{\max}$ is the `-max` output delay of an output port, and
$u_s$ is the setup uncertainty of the capture clock. The path arrival is the late arrival measured
from the launch edge (including launch clock latency and input delay).

### Derivation on `pd` and `edges`

On [`pd`](../README.md#pd) (`clk` period `2.0 ns`, latency `0.10 ns`, setup uncertainty `0.05 ns`,
`out1` output delay `0.20 ns`):
- `max_rr` (`set_max_delay 1.0 -from [get_pins r1/CK] -to [get_pins r2/D]`):
  - Late arrival at `r2/D` is `0.10 + 0.41 + 0.219 = 0.7290 ns`.
  - Required time is `1.0 + 0.10 - 0.10 - 0.05 = 0.9500 ns`.
  - Setup slack is `0.9500 - 0.7290 = 0.2210 ns` (hold remains `0.4890 ns`).
- `max_ro` (`set_max_delay 1.0 -from [get_pins r2/CK] -to [get_ports out1]`):
  - Late arrival at `out1` is `0.10 + 0.41 + 0.209 = 0.7190 ns`.
  - Output port check does not add capture clock latency (`lat_C = 0`): required time is
    `1.0 - 0.20 - 0.05 = 0.7500 ns`.
  - Setup slack is `0.7500 - 0.7190 = 0.0310 ns` (hold remains `0.7270 ns`).

On [`edges`](../README.md#edges) (`r2/CKN` launches on falling edge at `0.8 ns`, arrival from
launch edge is `0.6290 ns`):
- `set_max_delay 1.0 -from [get_pins r2/CKN] -to [get_pins r4/D]`:
  required time from the launch edge is `1.0 - 0.10 = 0.9000 ns`, so setup slack is
  `0.9000 - 0.6290 = 0.2710 ns`.

| Circuit / Variant | Path | Setup slack (ns) | Hold slack (ns) | Formula |
|---|---|---:|---:|---|
| `pd` / `base` | `r1/CK -> r2/D` | `1.2210` | `0.4890` | `(2.0 + 0.1 - 0.1 - 0.05) - 0.729 = 1.2210` |
| `pd` / `max_rr` (`V=1.0`) | `r1/CK -> r2/D` | `0.2210` | `0.4890` | `(1.0 + 0.1 - 0.1 - 0.05) - 0.729 = 0.2210` |
| `pd` / `base` | `r2/CK -> out1` | `1.1310` | `0.7270` | `(2.0 + 0.1 - 0.2 - 0.05) - 0.719 = 1.1310` |
| `pd` / `max_ro` (`V=1.0`) | `r2/CK -> out1` | `0.0310` | `0.7270` | `(1.0 - 0.2 - 0.05) - 0.719 = 0.0310` |
