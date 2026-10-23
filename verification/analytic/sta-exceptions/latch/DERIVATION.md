# Exceptions at active-high transparent latches (`latch`)

- **Requirement**: `STA-REQ-0267` ([`docs/specs/sta/exceptions.md`](../../../../docs/specs/sta/exceptions.md))
- **Circuit(s)**: `latch` ([`../README.md`](../README.md))
- **References**: [@sakallah1990checktc; @sdc2016; @bhasker2009static]

Paths to a latch data pin (`l/D`) end at `l/D`; paths through the latch (`GQ` and `DQ`) start at the
latch enable pin (`l/G`). Only arrivals that are timed at `l/D` pass the `D -> Q` arc with borrowed
time:
- If a false path wins at `l/D`, no borrowed arrival passes `l/D`, so `r2/D` sees only the latch's
  own launch from `l/G`.
- If a multicycle path with setup multiplier $N$ wins at `l/D`, the opening and closing edges of the
  latch check (and its borrowing window) shift by $(N - 1) T_s$.
- If `set_max_delay V` wins at `l/D`, `l/D` is checked like a flip-flop data pin against $V$
  ($t_{\text{req}} = V - s$, no borrowing) and passes no borrowed time to `l/Q`.

> [!NOTE]
> Departure from some legacy timers on `set_max_delay` to a latch data pin: while both suppress
> borrowed propagation through `D -> Q` (so `r2/D` sees `1.3815 ns`), RustSTA evaluates the setup
> slack at `l/D` directly against `V - s` (`0.5 - 0.1 - 0.846 = -0.4460 ns`) without time borrowing.

### Derivation on `latch`

From [`../README.md`](../README.md#latch) (`clk` period `2.0 ns`, open on `[0, 1]`):
- Arrival from `r1/CK` at `l/D`: `0.8460 ns` (rise) and `0.7571 ns` (fall).
- Baseline at `l/D`: borrows `0.8460 ns` (setup slack `0.0000 ns`), hold against closing edge at
  `-1.0 ns` gives `0.7571 - (-1.0 + 0.05) = 1.7071 ns`.
- At `r2/D`:
  - With borrowed arrival from `l/D` (`1.1130 ns` rise at `l/Q` + `0.1085 ns` fall through `u` =
    `1.2215 ns`): setup slack is `1.9 - 1.2215 = 0.6785 ns`.
  - Without borrowed arrival from `l/D` (only `l/G -> l/Q` rise `0.4100 ns` + `0.1085 ns` fall
    through `u` = `0.5185 ns`): setup slack is `1.9 - 0.5185 = 1.3815 ns`.
  - Early rise at `r2/D` is `0.359 + 0.136 = 0.4950 ns`: hold slack is `0.4950 - 0.05 = 0.4450 ns`.
- Under `mcp2_r1` (`set_multicycle_path 2 -setup -from [get_pins r1/CK]`):
  - Opening edge at `l/D` shifts by `(2 - 1)*2 = 2.0 ns` to `2.0 ns`. Since `0.8460 <= 2.0`, no
    time is borrowed and `l/D` setup slack is `2.0 - 0.8460 = 1.1540 ns`.
  - Hold relation at `l/D` shifts by `+2.0 ns` from `-1.0 ns` to `+1.0 ns`: hold slack is
    `0.7571 - (1.0 + 0.05) = -0.2929 ns`.
  - With zero borrowing at `l/D`, `r2/D` setup slack is `1.3815 ns`.

| Variant | `l/D` setup (ns) | `l/D` hold (ns) | `r2/D` setup (ns) | `r2/D` hold (ns) |
|---|---:|---:|---:|---:|
| `base` | `0.0000` | `1.7071` | `0.6785` | `0.4450` |
| `fp_from_r1` / `fp_to_ld` | `--` | `--` | `1.3815` | `0.4450` |
| `fp_from_lg` / `fp_from_l` | `0.0000` | `1.7071` | `--` | `--` |
| `fp_r1_r2` (`-from r1/CK -to r2/D`) | `0.0000` | `1.7071` | `0.6785` | `0.4450` |
| `mcp2_r1` / `mcp2_to_ld` | `1.1540` | `-0.2929` | `1.3815` | `0.4450` |
| `maxd_r1_ld` (`V=0.5`) | `-0.4460` | `1.7071` | `1.3815` | `0.4450` |
