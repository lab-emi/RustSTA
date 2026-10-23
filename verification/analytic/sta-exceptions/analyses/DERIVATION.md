# Exception types and analyses (`analyses`)

- **Requirement**: `STA-REQ-0250` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Each exception command defines one exception with a type (`set_false_path`, `set_multicycle_path`,
`set_max_delay`, `set_min_delay`), a path specification, and the analyses (setup, hold, or both) to
which it applies. This derivation uses the [`reconv`](../README.md#reconv) circuit with base clock
`clk` of period `2.0 ns` (`D_s = 2.0 ns`, `D_h = 0.0 ns`).

### Baseline arrivals and slacks on `reconv`

From [`../README.md`](../README.md#reconv), the path arrivals at `r3/D` (`t_su = 0.10 ns`,
`t_h = 0.05 ns`) are:

- Path 1 (`r1/CK -> b1 -> b2 -> g/A -> r3/D`): rise arrival `1.063 ns`, fall arrival `0.9552 ns`.
  Baseline setup slack `1.9 - 1.063 = 0.837 ns`; baseline hold slack `0.9552 - 0.05 = 0.9052 ns`.
- Path 2 (`r2/CK -> g/B -> r3/D`): rise arrival `0.629 ns`, fall arrival `0.559 ns`.
  Baseline setup slack `1.9 - 0.629 = 1.271 ns`; baseline hold slack `0.559 - 0.05 = 0.509 ns`.

### SDC variants and derived path slacks

```sdc
# fp_setup: disables only the setup analysis from r1/CK
set_false_path -setup -from [get_pins r1/CK]

# fp_hold: disables only the hold analysis from r1/CK
set_false_path -hold -from [get_pins r1/CK]

# mcp2_noflag: applies to both analyses with N = 2 and M = 0
set_multicycle_path 2 -from [get_pins r1/CK]

# mcp2_both: both -setup and -hold together -> N = 2, M = 0, plus warning D0113
set_multicycle_path 2 -setup -hold -from [get_pins r1/CK]

# mcp_h1: -hold only -> applies only to hold analysis with M = 1 (setup N remains 1)
set_multicycle_path 1 -hold -from [get_pins r1/CK]

# mcp0: N = 0 (non-positive integer multiplier allowed) -> D_s + (0 - 1)*2 = 0.0 ns
set_multicycle_path 0 -setup -from [get_pins r1/CK]
```

| Variant | `r1 -> r3` setup (ns) | `r1 -> r3` hold (ns) | `r2 -> r3` setup (ns) | `r2 -> r3` hold (ns) | Derivation on `r1 -> r3` |
|---|---:|---:|---:|---:|---|
| `base` | `0.8370` | `0.9052` | `1.2710` | `0.5090` | `1.9 - 1.063 = 0.8370`; `0.9552 - 0.05 = 0.9052` |
| `fp_setup` | `--` | `0.9052` | `1.2710` | `0.5090` | Setup untimed; hold unchanged at `0.9052` |
| `fp_hold` | `0.8370` | `--` | `1.2710` | `0.5090` | Setup unchanged at `0.8370`; hold untimed |
| `mcp2_noflag` | `2.8370` | `-1.0948` | `1.2710` | `0.5090` | `N = 2, M = 0`: setup `(4 - 0.1) - 1.063 = 2.8370`, hold `0.9552 - (2 + 0.05) = -1.0948` |
| `mcp2_both` | `2.8370` | `-1.0948` | `1.2710` | `0.5090` | Same as `mcp2_noflag` (`M` stays `0`), emits `D0113` |
| `mcp_h1` | `0.8370` | `2.9052` | `1.2710` | `0.5090` | `N = 1, M = 1`: setup `0.8370`, hold `0.9552 - (-2 + 0.05) = 2.9052` |
| `mcp0` | `-1.1630` | `2.9052` | `1.2710` | `0.5090` | `N = 0, M = 0`: setup `(0 - 0.1) - 1.063 = -1.1630`, hold `0.9552 - (-2 + 0.05) = 2.9052` |
