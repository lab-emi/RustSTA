# Exception diagnostics and run scope counts (`diagnostics`)

- **Requirement**: `STA-REQ-0270` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv, latch, groups` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

The timing analysis emits structured diagnostics at the source location of each exception command
and records exception counts and `false_path_only` endpoint counts in the run scope:

| Diagnostic | Severity | Condition and Analytic Example |
|---|---|---|
| `D0102` | Warning | Ignored non-startpoint or non-endpoint object, e.g. `set_false_path -from [get_pins r1/Q]` or `set_false_path -to [get_pins g/Y]` on [`reconv`](../README.md#reconv) |
| `D0110` | Warning | Exception command with no usable `-from`, `-through`, or `-to` object (either omitted or all objects ignored under `D0102`), e.g. `set_false_path -setup` or `set_false_path -from [get_pins r1/Q]` |
| `D0111` | Warning | Tie conflict between two exceptions of equal type, specificity, and analysis scope that match the same path with different effects, e.g. `o_later_wins` on [`reconv`](../README.md#reconv) |
| `D0112` | Note | Exception that wins no analysis of any timed path and supplies no setup multiplier to any hold check, e.g. `set_false_path -from [get_pins r1/CK] -to [get_pins r2/D]` on [`latch`](../README.md#latch) (`fp_r1_r2`) |
| `D0113` | Warning | `set_multicycle_path` with both `-setup` and `-hold` specified together (hold multiplier remains `0`), e.g. `mcp2_both` on [`reconv`](../README.md#reconv) |
| `D0114` | Note | `set_clock_groups` with `-allow_paths` (separates no clocks in STA), e.g. `allow_paths` on [`groups`](../README.md#groups) |

### Scope accounting

For `fp_to_pin` (`set_false_path -to [get_pins r3/D]`) on [`reconv`](../README.md#reconv):
- `false_path` exception count: `1` (`0` unused with `D0112`).
- Endpoint `r3/D` has status `false_path_only` in both setup and hold and is counted under the
  `false_path_only` unconstrained reason.
