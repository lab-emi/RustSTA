# False paths and `false_path_only` endpoint status (`false_path`)

- **Requirement**: `STA-REQ-0259` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

A path whose winning exception in an analysis is `set_false_path` is not timed in that analysis and
contributes no slack to its endpoint. When all paths to an endpoint in an analysis are disabled by
false paths, the endpoint has no slack in that analysis and receives status `false_path_only`.
False paths do not alter cell delays or signal transitions on remaining paths.

### Derivation on `reconv`

On [`reconv`](../README.md#reconv):
- At `g/A` (driven by `r1` through `b1, b2`), the transition is `0.07 ns` (rise) and `0.061 ns`
  (fall); at `g/B` (driven by `r2`), the transition is `0.09 ns` (rise) and `0.08 ns` (fall).
- When `set_false_path -from [get_pins r1/CK]` disables `r1 -> r3`, the arrival and transition from
  `r2/CK` through `g/B -> g/Y -> r3/D` are unchanged (`0.629 ns` rise, `0.559 ns` fall), so
  endpoint `r3/D` has setup slack `1.2710 ns` and hold slack `0.5090 ns`.
- When `set_false_path -to [get_pins r3/D]` (or `-from [get_clocks clk]`) disables both `r1 -> r3`
  and `r2 -> r3`, `r3/D` has no timed paths and its endpoint status is `false_path_only`.

| Variant | `r1 -> r3` setup (ns) | `r1 -> r3` hold (ns) | `r2 -> r3` setup (ns) | `r2 -> r3` hold (ns) | `r3/D` endpoint setup / hold (ns) |
|---|---:|---:|---:|---:|---|
| `base` | `0.8370` | `0.9052` | `1.2710` | `0.5090` | `0.8370 / 0.5090` |
| `fp_from_pin` (`r1/CK`) | `--` | `--` | `1.2710` | `0.5090` | `1.2710 / 0.5090` |
| `fp_from_to` (`r1/CK -> r3/D`) | `--` | `--` | `1.2710` | `0.5090` | `1.2710 / 0.5090` |
| `fp_to_pin` (`r3/D`) | `--` | `--` | `--` | `--` | `false_path_only` |
