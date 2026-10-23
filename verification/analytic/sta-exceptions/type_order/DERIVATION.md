# Precedence of exception types (`type_order`)

- **Requirement**: `STA-REQ-0256` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Clock groups apply first; otherwise, within each analysis (setup or hold) of a path, exception types
take precedence in the order:

$$\text{false path} \succ \text{max/min delay} \succ \text{multicycle path}$$

regardless of specificity or command order.

### Derivation on `reconv`

```sdc
# p_fp_over_maxd: clock-level false_path (specificity 2) beats pin-to-pin max_delay (specificity 24)
set_max_delay 1.5 -from [get_pins r1/CK] -to [get_pins r3/D]
set_false_path -from [get_clocks clk]

# p_maxd_over_mcp: clock-level max_delay (specificity 2) beats pin-to-pin multicycle_path (specificity 24) in setup
# Note: in hold, max_delay does not apply, so the setup multicycle N = 2 still shifts hold on r1 -> r3!
set_max_delay 1.5 -from [get_clocks clk]
set_multicycle_path 2 -setup -from [get_pins r1/CK] -to [get_pins r3/D]

# p_mind_over_mcp: clock-level min_delay (0.8 ns) beats pin-level hold multicycle (M = 1) in hold
set_min_delay 0.8 -from [get_clocks clk]
set_multicycle_path 1 -hold -from [get_pins r1/CK]

# p_fps_mcp: setup false_path beats setup multicycle in setup, while N = 2 still shifts hold on r1 -> r3
set_false_path -setup -from [get_pins r1/CK]
set_multicycle_path 2 -setup -from [get_pins r1/CK]
```

| Variant | `r1 -> r3` setup (ns) | `r1 -> r3` hold (ns) | `r2 -> r3` setup (ns) | `r2 -> r3` hold (ns) |
|---|---:|---:|---:|---:|
| `p_fp_over_maxd` | `--` | `--` | `--` | `--` |
| `p_maxd_over_mcp` | `0.3370` | `-1.0948` | `0.7710` | `0.5090` |
| `p_mind_over_mcp` | `0.8370` | `0.1052` | `1.2710` | `-0.2910` |
| `p_fps_mcp` | `--` | `-1.0948` | `1.2710` | `0.5090` |

Arithmetic check for `p_maxd_over_mcp`:
- Setup on `r1 -> r3`: `max_delay 1.5` wins (`1.5 - 0.1 - 1.063 = 0.3370 ns`); on `r2 -> r3`:
  `1.5 - 0.1 - 0.629 = 0.7710 ns`.
- Hold on `r1 -> r3`: `max_delay` does not apply to hold, and the winning setup multicycle path on
  `r1 -> r3` has `N = 2`, so hold required time is `(2 - 1)*2 + 0.05 = 2.05 ns` and hold slack is
  `0.9552 - 2.05 = -1.0948 ns`.
