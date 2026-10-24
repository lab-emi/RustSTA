# RustSTA known-answer cases (STA-REQ-0501)

Each case is a small netlist over the linear synthetic library (`crates/apps/ruststa-cli/tests/sta_common/mod.rs`
and its Phase 2a extensions): every table has two index points per axis (`[0, 1]`), so bilinear
interpolation and linear extrapolation reproduce the linear cell models (`delay = d0 + ks * s +
kc * C`, `slew = max(0, s0 + ms * s + mc * C)`) exactly, and the expected arrivals, transitions
and slacks follow by hand [@hitchcock1982timing; @liberty2017; @sdc2016; @bhasker2009static;
@sapatnekar2004timing; @sakallah1990checktc]. Tests evaluate the derivations below and compare the
engine and the independent checker with them within `1e-9 ns`.

## Linear synthetic library parameters

All input pins have capacitance `CIN = 0.01 pF`; `BUFO/Y` has output capacitance `COUT_O = 0.004 pF`;
`BUFZ/Z` has output capacitance `COUT_Z = 0.005 pF`. Flip-flop and latch setup time is `0.10 ns`,
hold time `0.05 ns`; `DFFR/RN` recovery time is `0.12 ns` (rise), removal time `0.06 ns` (rise).

| Model | Cells / arcs | `d0` | `ks` | `kc` | `s0` | `ms` | `mc` |
|---|---|---|---|---|---|---|---|
| `INV_R` / `INV_F` | `INV` (`!A`, negative unate) | `0.10` / `0.08` | `0.20` / `0.15` | `2.0` / `1.5` | `0.05` / `0.04` | `0.0` / `0.0` | `1.5` / `1.2` |
| `BUF_R` / `BUF_F` | `BUF`, `BUFO`, `AND2`, `OR2`, `MUX2` (`A0`, `A1`), `BUFZ` (`A -> Z`) | `0.20` / `0.18` | `0.10` / `0.10` | `1.0` / `1.2` | `0.06` / `0.05` | `0.0` / `0.0` | `1.0` / `1.1` |
| `XOR_R` / `XOR_F` | `XOR2` (`A^B`), `MUX2` (`S -> Y`), non-unate | `0.30` / `0.28` | `0.25` / `0.20` | `2.5` / `2.2` | `0.07` / `0.06` | `0.0` / `0.0` | `1.8` / `1.6` |
| `CKQ_R` / `CKQ_F` | `DFF`, `DFFN`, `DFFR` (`CK -> Q`), `LAT`, `LATS` (`G -> Q`) | `0.40` / `0.35` | `0.0` / `0.0` | `1.0` / `0.9` | `0.08` / `0.07` | `0.0` / `0.0` | `1.0` / `1.0` |
| `DQ_R` / `DQ_F` | `LAT` (`D -> Q`, positive unate) | `0.25` / `0.22` | `0.10` / `0.10` | `1.0` / `1.0` | `0.06` / `0.05` | `0.0` / `0.0` | `1.0` / `1.0` |
| `DQS_R` / `DQS_F` | `LATS` (`D -> Q`, larger transition than `G -> Q`) | `0.25` / `0.22` | `0.10` / `0.10` | `1.0` / `1.0` | `0.12` / `0.11` | `0.0` / `0.0` | `1.0` / `1.0` |
| `TSE_R` / `TSE_F` | `BUFZ` (`EN -> Z`, `three_state_enable`, positive unate) | `0.15` / `0.13` | `0.10` / `0.10` | `1.0` / `1.0` | `0.06` / `0.05` | `0.0` / `0.0` | `1.0` / `1.0` |
| `TSD_R` / `TSD_F` | `BUFZ` (`EN -> Z`, `three_state_disable`, negative unate) | `0.09` / `0.07` | `0.05` / `0.05` | `0.0` / `0.0` | `0.0` / `0.0` | `0.0` / `0.0` | `0.0` / `0.0` |
| `XTR_R` / `XTR_F` | `XTR` (`A -> Y`, negative slope extrapolation) | `0.05` / `0.05` | `-0.04` / `-0.04` | `1.0` / `1.0` | `0.10` / `0.10` | `-0.08` / `-0.08` | `0.5` / `0.5` |

## Phase 2a derivations

| Case | Circuit and rule | Requirements |
|---|---|---|
| [`tristate_bus`](tristate_bus/DERIVATION.md) | Two `BUFZ` drivers on a shared bus with `three_state_enable` and `three_state_disable` arcs | STA-REQ-0013, 0014, 0223 |
| [`two_corners`](two_corners/DERIVATION.md) | Two-register buffer path across `fast` (`k = 1`) and `slow` (`k = 2`) corners | STA-REQ-0015 |
| [`thresholds`](thresholds/DERIVATION.md) | Inverter chain with `40/60` thresholds, `30/70` slew trip points and `slew_derate_from_library : 0.5` | STA-REQ-0016 |
| [`per_pin_slew`](per_pin_slew/DERIVATION.md) | Per-pin worst slew at a `MUX2` output when `S` carries an untagged input transition | STA-REQ-0108, 0109 |
| [`slew_clamp`](slew_clamp/DERIVATION.md) | Negative extrapolated transition clamped to `0.0`, negative extrapolated delay unclamped | STA-REQ-0120 |
| [`driver_pin_cap`](driver_pin_cap/DERIVATION.md) | Driver output-pin capacitance (`BUFO/Y`) added to net load | STA-REQ-0121 |
| [`wireload_lumped`](wireload_lumped/DERIVATION.md) | Lumped wire-load interpolation, slope extrapolation, and port-connected net exclusion | STA-REQ-0122 to 0125 |
| [`const_tielo_and2`](const_tielo_and2/DERIVATION.md) | `TIELO` constant propagation disabling an `AND2` branch into an `OR2` | STA-REQ-0150 to 0152 |
| [`const_xor_tiehi`](const_xor_tiehi/DERIVATION.md) | `XOR2` with `TIEHI` input refined from `non_unate` to `negative_unate` | STA-REQ-0153 |
| [`const_flop_d`](const_flop_d/DERIVATION.md) | Flip-flop `D` tied constant (`CK -> Q` still launches) and `CK` tied constant (`unclocked`) | STA-REQ-0161 |
| [`const_mux_select`](const_mux_select/DERIVATION.md) | `MUX2` select tied to `TIELO` disabling unselected data and select arcs | STA-REQ-0155 |
| [`case_analysis`](case_analysis/DERIVATION.md) | `set_case_analysis`, `set_logic_zero`, and `set_disable_timing` on a `MUX2` | STA-REQ-0156, 0157 |
| [`io_delay_latency`](io_delay_latency/DERIVATION.md) | Input/output delay clock latencies, `-source_latency_included`, `-network_latency_included`, and `set_driving_cell` | STA-REQ-0217, 0218 |
| [`one_sided_delays`](one_sided_delays/DERIVATION.md) | One-sided `-max` and `-min` input and output delays | STA-REQ-0219 |
| [`two_clocks_hold`](two_clocks_hold/DERIVATION.md) | Setup (`D_s`) and hold (`D_h`) relations over the common period (`3:2`, `2:3`, and offset waveforms) | STA-REQ-0220 |
| [`latch_open_uncertainty`](latch_open_uncertainty/DERIVATION.md) | Latch transparency window, opening-edge setup uncertainty, and `set_max_time_borrow` | STA-REQ-0221 |
| [`latch_relaunch`](latch_relaunch/DERIVATION.md) | Latch `D -> Q` re-launch in the opening-edge tag, `Q` slew from `G -> Q`, and backward required times | STA-REQ-0222, 0227 |
| [`recovery_removal`](recovery_removal/DERIVATION.md) | Asynchronous clear `recovery`/`removal` checks (`async_default`) and setup-first handle ranking | STA-REQ-0224, 0228 |
| [`clock_gating`](clock_gating/DERIVATION.md) | Active-high (`AND2`) and active-low (`OR2`) combinational clock-gating checks | STA-REQ-0216, 0225 |
| [`endpoint_status`](endpoint_status/DERIVATION.md) | Endpoint status taxonomy (`constrained`, `unclocked`, `no_data_arrival`, `unconstrained_output`) | STA-REQ-0226 |

## Baseline and legacy cases

| Case | Circuit | Requirements |
|---|---|---|
| [`inverter_chain`](inverter_chain/DERIVATION.md) | Two inverters between flip-flops, an input delay, an output delay and a port load | STA-REQ-0110 to 0114, 0211, 0212 |
| [`reconvergent_xor`](reconvergent_xor/DERIVATION.md) | A flip-flop output reaching a non-unate XOR directly and through a buffer | STA-REQ-0012, 0112 |
| [`falling_edge_capture`](falling_edge_capture/DERIVATION.md) | Capture by a falling-edge flip-flop | STA-REQ-0229 |
| [`latch_borrowing`](latch_borrowing/DERIVATION.md) | Superseded Phase 1 latch borrowing (legacy engine, removed by Z1) | STA-REQ-0213 ([legacy.md](../../../docs/specs/sta/legacy.md)) |
| [`two_clocks`](two_clocks/DERIVATION.md) | Superseded Phase 1 two-clock hold relation `D_s - T_C` (legacy engine, removed by Z1) | STA-REQ-0210 ([legacy.md](../../../docs/specs/sta/legacy.md)) |
