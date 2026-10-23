# `-from`, `-rise_from`, and `-fall_from` (`from_objects`)

- **Requirement**: `STA-REQ-0252` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv, edges` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

`-from` matches clocks (paths launched by that clock), startpoint ports/pins, and cells (via their
startpoint pins). `-rise_from` and `-fall_from` additionally filter by the start transition at a
port/pin or by the launch source edge at a clock.

### Derivation on `reconv` and `edges`

On [`reconv`](../README.md#reconv):
- `fp_from_pin` (`-from [get_pins r1/CK]`) and `fp_from_cell` (`-from [get_cells r1]`) both match the
  paths starting at `r1/CK`, leaving `r2 -> r3` (`1.2710 ns` setup, `0.5090 ns` hold).
- `fp_from_clk` (`-from [get_clocks clk]`) matches both paths.

On [`edges`](../README.md#edges) (`clk` period `2.0 ns`, waveform `{0 0.8}`):
- `r1` (`DFF`, rising-edge `r1/CK`) launches on the rising source edge at `0.0 ns` to `r3` (`DFFN`):
  baseline setup `0.0710 ns`, hold `1.7090 ns`.
- `r2` (`DFFN`, falling-edge `r2/CKN`) launches on the falling source edge at `0.8 ns` to `r4` (`DFF`):
  baseline setup `0.4710 ns`, hold `1.3090 ns`.

```sdc
# On edges:
set_false_path -rise_from [get_clocks clk]
set_false_path -fall_from [get_clocks clk]
set_false_path -rise_from [get_pins r2/CKN]
set_false_path -fall_from [get_pins r2/CKN]
```

| Circuit / Variant | Path 1 setup (ns) | Path 1 hold (ns) | Path 2 setup (ns) | Path 2 hold (ns) | Explanation |
|---|---:|---:|---:|---:|---|
| `reconv` / `fp_from_pin` | `--` | `--` | `1.2710` | `0.5090` | Matches startpoint pin `r1/CK` |
| `reconv` / `fp_from_cell` | `--` | `--` | `1.2710` | `0.5090` | Matches cell `r1` via its startpoint pin `r1/CK` |
| `reconv` / `fp_from_clk` | `--` | `--` | `--` | `--` | Matches launch clock `clk` on both paths |
| `edges` / `base` | `0.0710` | `1.7090` | `0.4710` | `1.3090` | Baseline (`r1->r3` and `r2->r4`) |
| `edges` / `rise_from_clk` | `--` | `--` | `0.4710` | `1.3090` | Rising source edge launches `r1`, not `r2` |
| `edges` / `fall_from_clk` | `0.0710` | `1.7090` | `--` | `--` | Falling source edge launches `r2`, not `r1` |
| `edges` / `rise_from_ckn` | `0.0710` | `1.7090` | `0.4710` | `1.3090` | `r2/CKN` active transition is fall; rise matches nothing (`D0112`) |
| `edges` / `fall_from_ckn` | `0.0710` | `1.7090` | `--` | `--` | `r2/CKN` active transition is fall; disables `r2 -> r4` |
