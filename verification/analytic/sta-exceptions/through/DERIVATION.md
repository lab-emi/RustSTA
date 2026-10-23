# Ordered `-through`, `-rise_through`, and `-fall_through` (`through`)

- **Requirement**: `STA-REQ-0254` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `xbar` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

Each `-through` option names a set of pins, ports, nets, and cells, and multiple `-through` options
are matched in order along the path. `-rise_through` and `-fall_through` also require the specified
signal transition at that pin.

### Derivation on `xbar`

On [`xbar`](../README.md#xbar) (`clk` period `2.0 ns`):
- `s13` (`r1 -> g1/A -> g1/Y -> r3/D`) and `s23` (`r2 -> g1/B -> g1/Y -> r3/D`): baseline setup
  slack `1.9 - 0.64 = 1.2600 ns`.
- `s14` (`r1 -> g2/A -> g2/Y -> r4/D`) and `s24` (`r2 -> g2/B -> g2/Y -> r4/D`) through `XOR2 g2`:
  - `g2/A` rise (`0.42 ns`, slew `0.10 ns`) produces `g2/Y` rise `0.42 + 0.35 = 0.7700 ns` (setup
    `1.9 - 0.77 = 1.1300 ns`) and `g2/Y` fall `0.42 + 0.322 = 0.7420 ns` (setup `1.1580 ns`).
  - `g2/A` fall (`0.368 ns`, slew `0.09 ns`) produces `g2/Y` rise `0.368 + 0.3475 = 0.7155 ns`
    (setup `1.9 - 0.7155 = 1.1845 ns`) and `g2/Y` fall `0.368 + 0.32 = 0.6880 ns` (setup
    `1.2120 ns`).
  - When `thr_rise` (`set_false_path -rise_through [get_pins g2/A]`) disables paths with a rising
    transition at `g2/A`, the remaining `r1 -> r4` paths have a falling transition at `g2/A`, whose
    worst arrival at `r4/D` is `0.7155 ns`, giving setup slack `1.9 - 0.7155 = 1.1845 ns`!

```sdc
# thr_pin: matches only r1 -> r3
set_false_path -through [get_pins g1/A]

# thr_out / thr_net / thr_cell: matches both r1 -> r3 and r2 -> r3
set_false_path -through [get_pins g1/Y]
set_false_path -through [get_nets n1]
set_false_path -through [get_cells g1]

# thr_rise: disables rising transition at g2/A on r1 -> r4
set_false_path -rise_through [get_pins g2/A]

# thr_order: g1/A then g1/Y matches r1 -> r3
set_false_path -through [get_pins g1/A] -through [get_pins g1/Y]

# thr_order_rev: g1/Y then g1/A is in reverse topological order -> matches no path (D0112)
set_false_path -through [get_pins g1/Y] -through [get_pins g1/A]
```

| Variant | `s13` (ns) | `s14` (ns) | `s23` (ns) | `s24` (ns) |
|---|---:|---:|---:|---:|
| `base` | `1.2600` | `1.1300` | `1.2600` | `1.1300` |
| `thr_pin` (`g1/A`) | `--` | `1.1300` | `1.2600` | `1.1300` |
| `thr_out` (`g1/Y`) | `--` | `1.1300` | `--` | `1.1300` |
| `thr_net` (`n1`) | `--` | `1.1300` | `--` | `1.1300` |
| `thr_cell` (`g1`) | `--` | `1.1300` | `--` | `1.1300` |
| `thr_rise` (`-rise_through g2/A`) | `1.2600` | `1.1845` | `1.2600` | `1.1300` |
| `thr_order` (`g1/A` then `g1/Y`) | `--` | `1.1300` | `1.2600` | `1.1300` |
| `thr_order_rev` (`g1/Y` then `g1/A`) | `1.2600` | `1.1300` | `1.2600` | `1.1300` |
