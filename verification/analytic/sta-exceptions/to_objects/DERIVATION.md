# `-to`, `-rise_to`, `-fall_to`, `-rise`, and `-fall` (`to_objects`)

- **Requirement**: `STA-REQ-0253` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `reconv, edges` ([`../README.md`](../README.md))
- **References**: [@sdc2016; @bhasker2009static]

`-to` matches capture clocks, endpoint ports/pins, and cells (via their endpoint pins). `-rise_to`
and `-fall_to` filter by the end data transition at a port/pin or by the capture source edge at a
clock. `-rise` and `-fall` restrict an exception to paths whose end data transition is rise or fall.

### Derivation on `reconv` and `edges`

On [`reconv`](../README.md#reconv):
- Path 1 (`r1/CK -> r3/D`): rise arrival `1.063 ns` (setup slack `1.9 - 1.063 = 0.8370 ns`, hold
  slack `1.063 - 0.05 = 1.0130 ns`), fall arrival `0.9552 ns` (setup slack
  `1.9 - 0.9552 = 0.9448 ns`, hold slack `0.9552 - 0.05 = 0.9052 ns`).
- Path 2 (`r2/CK -> r3/D`): rise arrival `0.629 ns` (setup slack `1.9 - 0.629 = 1.2710 ns`, hold
  slack `0.629 - 0.05 = 0.5790 ns`), fall arrival `0.559 ns` (setup slack
  `1.9 - 0.559 = 1.3410 ns`, hold slack `0.559 - 0.05 = 0.5090 ns`).
- When `-rise_to [get_pins r3/D]` disables the rising end transition at `r3/D`, only the falling
  end transition remains timed: setup slack becomes `0.9448 ns` on `r1 -> r3` and `1.3410 ns` on
  `r2 -> r3`, while hold slack (already governed by the faster fall arrival) remains `0.9052 ns`
  and `0.5090 ns`.
- When `-fall_to [get_pins r3/D]` disables the falling end transition at `r3/D`, only the rising
  end transition remains timed: setup slack stays `0.8370 ns` and `1.2710 ns`, while hold slack
  becomes `1.0130 ns` on `r1 -> r3` and `0.5790 ns` on `r2 -> r3`.
- `-rise -from [get_pins r1/CK]` disables only the rising end transition of `r1 -> r3`, giving
  setup `0.9448 ns` and hold `0.9052 ns` on `r1 -> r3` and leaving `r2 -> r3` at `1.2710 / 0.5090 ns`.

On [`edges`](../README.md#edges):
- `r3` (`DFFN`) captures on the falling edge of `clk` and `r4` (`DFF`) captures on the rising edge
  of `clk`: `-rise_to [get_clocks clk]` matches `r2 -> r4` (`--`), and
  `set_multicycle_path 2 -setup -fall_to [get_clocks clk]` shifts `r1 -> r3` setup by `+2.0 ns` to
  `2.0710 ns` and hold by `-2.0 ns` to `-0.2910 ns`.

| Circuit / Variant | Path 1 setup (ns) | Path 1 hold (ns) | Path 2 setup (ns) | Path 2 hold (ns) |
|---|---:|---:|---:|---:|
| `reconv` / `fp_to_pin` (`r3/D`) | `--` | `--` | `--` | `--` |
| `reconv` / `fp_to_cell` (`r3`) | `--` | `--` | `--` | `--` |
| `reconv` / `fp_rise_to` (`r3/D`) | `0.9448` | `0.9052` | `1.3410` | `0.5090` |
| `reconv` / `fp_fall_to` (`r3/D`) | `0.8370` | `1.0130` | `1.2710` | `0.5790` |
| `reconv` / `fp_rise_flag` (`-rise -from r1/CK`) | `0.9448` | `0.9052` | `1.2710` | `0.5090` |
| `edges` / `rise_to_clk` | `0.0710` | `1.7090` | `--` | `--` |
| `edges` / `fall_to_clk` | `--` | `--` | `0.4710` | `1.3090` |
| `edges` / `mcp2_fall_to_clk` | `2.0710` | `-0.2910` | `0.4710` | `1.3090` |
