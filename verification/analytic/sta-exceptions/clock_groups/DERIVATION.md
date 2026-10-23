# Clock groups (`set_clock_groups`) (`clock_groups`)

- **Requirement**: `STA-REQ-0266` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `groups` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

`set_clock_groups` with `-asynchronous`, `-logically_exclusive`, or `-physically_exclusive`
separates any two clocks that appear in distinct `-group` lists (and do not appear together in any
single `-group` list). With only one `-group`, all other defined clocks form the implicit second
group. Separated clock pairs are untimed in both analyses and take precedence over all path
exceptions (`set_max_delay`, `set_multicycle_path`, etc.). With `-allow_paths`, no clocks are
separated in static timing analysis and note `D0114` is emitted.

### Derivation on `groups`

On [`groups`](../README.md#groups) (clocks `A` period `4.0`, `B` period `1.0`, `C` period `2.0`):
- `ab` (`r1 -> r2`, `A -> B`): baseline `0.2710 / 0.5090 ns`
- `bc` (`r3 -> r4`, `B -> C`): baseline `0.2610 / 0.5210 ns`
- `ba` (`r3 -> r5`, `B -> A`): baseline `0.2610 / 0.5210 ns`
- `ac` (`r5 -> r6`, `A -> C`): baseline `1.2710 / 0.5090 ns`

```sdc
# async: group {A} vs group {B C} -> separates (A,B) and (A,C); (B,C) stays timed
set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks {B C}]

# one_group: single -group {A} -> separates A from every other clock {B, C}
set_clock_groups -asynchronous -group [get_clocks A]

# logical: group {A} vs group {B} -> separates (A,B) and (B,A); (B,C) and (A,C) stay timed
set_clock_groups -logically_exclusive -group [get_clocks A] -group [get_clocks B]

# over_maxd: clock groups beat pin-to-pin set_max_delay on r1 -> r2
set_clock_groups -asynchronous -group [get_clocks A] -group [get_clocks B]
set_max_delay 0.8 -from [get_pins r1/CK] -to [get_pins r2/D]

# allow_paths: separates no clocks (emits note D0114)
set_clock_groups -asynchronous -allow_paths -group [get_clocks A] -group [get_clocks B]
```

| Variant | `A -> B` (`ab / abh`) | `B -> C` (`bc / bch`) | `B -> A` (`ba / bah`) | `A -> C` (`ac / ach`) |
|---|---|---|---|---|
| `base` | `0.2710 / 0.5090` | `0.2610 / 0.5210` | `0.2610 / 0.5210` | `1.2710 / 0.5090` |
| `async` | `-- / --` | `0.2610 / 0.5210` | `-- / --` | `-- / --` |
| `one_group` | `-- / --` | `0.2610 / 0.5210` | `-- / --` | `-- / --` |
| `logical` | `-- / --` | `0.2610 / 0.5210` | `-- / --` | `1.2710 / 0.5090` |
| `over_maxd` | `-- / --` | `0.2610 / 0.5210` | `-- / --` | `1.2710 / 0.5090` |
| `allow_paths` | `0.2710 / 0.5090` | `0.2610 / 0.5210` | `0.2610 / 0.5210` | `1.2710 / 0.5090` |
