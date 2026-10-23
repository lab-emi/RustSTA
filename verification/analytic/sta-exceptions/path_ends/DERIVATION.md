# Timing paths, startpoints, and endpoints (`path_ends`)

- **Requirement**: `STA-REQ-0251` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Timing exceptions match timing paths that run from a valid startpoint (input ports and clock/enable
pins of edge arcs) to a valid endpoint (constrained check pins and output ports). Outputs of edge
arcs (such as `r1/Q`), internal combinational pins (such as `g/Y`), and clock pins (such as `r3/CK`)
are not startpoints or endpoints.

### Verification on `reconv`

On [`reconv`](../README.md#reconv), the timed register-to-register paths start at `r1/CK` and
`r2/CK` and end at `r3/D`:

```sdc
# Valid startpoint pin r1/CK -> matches path 1
set_false_path -from [get_pins r1/CK]

# Flip-flop Q output r1/Q is not a startpoint -> ignored with D0102 (and D0110), paths unchanged
set_false_path -from [get_pins r1/Q]

# Valid endpoint pin r3/D -> matches both paths
set_false_path -to [get_pins r3/D]

# Combinational output g/Y is not an endpoint -> ignored with D0102 (and D0110), paths unchanged
set_false_path -to [get_pins g/Y]
```

| Variant | `r1 -> r3` setup (ns) | `r1 -> r3` hold (ns) | `r2 -> r3` setup (ns) | `r2 -> r3` hold (ns) | Explanation |
|---|---:|---:|---:|---:|---|
| `base` | `0.8370` | `0.9052` | `1.2710` | `0.5090` | Both paths timed |
| `fp_from_pin` (`r1/CK`) | `--` | `--` | `1.2710` | `0.5090` | `r1/CK` is a startpoint; disables `r1 -> r3` |
| `fp_from_q` (`r1/Q`) | `0.8370` | `0.9052` | `1.2710` | `0.5090` | `r1/Q` is not a startpoint (`D0102`, `D0110`) |
| `fp_to_pin` (`r3/D`) | `--` | `--` | `--` | `--` | `r3/D` is an endpoint; disables both paths |
| `fp_to_internal` (`g/Y`) | `0.8370` | `0.9052` | `1.2710` | `0.5090` | `g/Y` is not an endpoint (`D0102`, `D0110`) |
