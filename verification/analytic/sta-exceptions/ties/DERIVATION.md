# Tie-breaking by single-analysis scope and constraint order (`ties`)

- **Requirement**: `STA-REQ-0258` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Among exceptions of equal type and specificity on a path:
1. An exception that applies only to the analysis at hand (`-setup` or `-hold` only) beats one that
   applies to both analyses (no flag or both flags).
2. Among remaining ties, the exception defined later in the constraint evaluation order wins, and
   if two tied exceptions match a path with different effects, warning `D0111` is emitted once per
   pair of commands.

> [!NOTE]
> Departure from some legacy timers that merge or partially overwrite overlapping object lists in
> place: RustSTA uses a clean, order-consistent rule where among equal type, specificity, and
> analysis scope, the later command in the SDC stream wins and emits conflict warning `D0111`.

### Derivation on `reconv`

```sdc
# o_specific_setup: -setup only (N=3) beats both-analysis (N=2) in setup, even when defined earlier
set_multicycle_path 3 -setup -from [get_pins r1/CK]
set_multicycle_path 2 -from [get_pins r1/CK]

# o_specific_hold: -hold only (M=1) beats both-analysis (N=3, M=0) in hold, while N=3 wins setup
set_multicycle_path 1 -hold -from [get_pins r1/CK]
set_multicycle_path 3 -from [get_pins r1/CK]

# o_later_wins: equal specificity (16) and scope (-setup); later command (N=2 on r1/CK) wins on r1->r3 (D0111)
set_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]
set_multicycle_path 2 -setup -from [get_pins r1/CK]

# o_later_wins_rev: reversed order -> later command (N=3 on {r1/CK r2/CK}) wins on both (D0111)
set_multicycle_path 2 -setup -from [get_pins r1/CK]
set_multicycle_path 3 -setup -from [get_pins {r1/CK r2/CK}]

# o_max_later: later set_max_delay (1.5 ns on r1/CK) wins on r1->r3 over earlier 1.2 ns on {r1/CK r2/CK} (D0111)
set_max_delay 1.2 -from [get_pins {r1/CK r2/CK}]
set_max_delay 1.5 -from [get_pins r1/CK]

# o_hold_later: later set_multicycle_path -hold (M=1 on r1/CK) wins on r1->r3 over earlier M=2 (D0111)
set_multicycle_path 2 -hold -from [get_pins {r1/CK r2/CK}]
set_multicycle_path 1 -hold -from [get_pins r1/CK]
```

| Variant | `r1 -> r3` setup (ns) | `r1 -> r3` hold (ns) | `r2 -> r3` setup (ns) | `r2 -> r3` hold (ns) |
|---|---:|---:|---:|---:|
| `o_specific_setup` | `4.8370` | `-3.0948` | `1.2710` | `0.5090` |
| `o_specific_hold` | `4.8370` | `-1.0948` | `1.2710` | `0.5090` |
| `o_later_wins` | `2.8370` | `-1.0948` | `5.2710` | `-3.4910` |
| `o_later_wins_rev` | `4.8370` | `-3.0948` | `5.2710` | `-3.4910` |
| `o_max_later` | `0.3370` | `0.9052` | `0.4710` | `0.5090` |
| `o_hold_later` | `0.8370` | `2.9052` | `1.2710` | `4.5090` |
