# Dual-PDK suite and lab GPU verification

`cargo xtask verify pdk` verifies the standard-cell Liberty libraries of two open-source process
design kits (`sky130_fd_sc_hd` and `gf180mcu_fd_sc_mcu{7t,9t}5v0`), builds PDK-mapped netlist
topologies (`deep_chain`, `dag`, `two_clock_edges`, `latch_pipeline`, `icg_gating`, `tristate_bus`),
verifies engine timing invariants and mutation sequences (`crates/engines/ruststa/tests/invariants.rs`),
and checks engine-vs-checker agreement (`crates/apps/ruststa-cli/tests/checker_agreement.rs`):
every generated design of every library goes through the shipped commands, `ruststa run` and then
`ruststa check --method independent`, which must agree (a design the runner did not write is a
failure), and a 3-corner `sky130_fd_sc_hd` (`tt`, `ss`, `ff`) run is bitwise equal to three
single-corner runs.

The libraries are pinned in `verification/pdks.toml` and downloaded by `cargo xtask pdk fetch`.
They are never tracked in git: design data in the repository is tracked only as synthetic fixtures,
and each assembled PDK library is 8 to 21 MB.

## Coverage and profiles

| Name | Library | Corner | Profile | Source form |
|---|---|---|---|---|
| `sky130hd_tt` | `sky130_fd_sc_hd` | `tt_025C_1v80` | `pr`, `full` | one Liberty file |
| `sky130hd_ss` | `sky130_fd_sc_hd` | `ss_100C_1v60` | `pr`, `full` | one Liberty file |
| `sky130hd_ff` | `sky130_fd_sc_hd` | `ff_n40C_1v95` | `pr`, `full` | one Liberty file |
| `gf180mcu7t_tt` | `gf180mcu_fd_sc_mcu7t5v0` | `tt_025C_5v00` | `pr`, `full` | header and 229 cell fragments |
| `gf180mcu9t_tt` | `gf180mcu_fd_sc_mcu9t5v0` | `tt_025C_5v00` | `pr`, `full` | header and 229 cell fragments |
| `gf180mcu7t_ss` | `gf180mcu_fd_sc_mcu7t5v0` | `ss_125C_4v50` | `full` | header and 229 cell fragments |
| `gf180mcu7t_ff` | `gf180mcu_fd_sc_mcu7t5v0` | `ff_n40C_5v50` | `full` | header and 229 cell fragments |
| `gf180mcu9t_ss` | `gf180mcu_fd_sc_mcu9t5v0` | `ss_125C_4v50` | `full` | header and 229 cell fragments |
| `gf180mcu9t_ff` | `gf180mcu_fd_sc_mcu9t5v0` | `ff_n40C_5v50` | `full` | header and 229 cell fragments |

- `--profile pr` (default): runs the 5 PR libraries (`sky130hd_{tt,ss,ff}`, `gf180mcu7t_tt`,
  `gf180mcu9t_tt`), verifies zero Liberty reader diagnostics (`diags.is_empty()`), checks all 6
  PDK-mapped topologies per library, runs 100 netlist mutation steps per `tt` family, runs 1,000
  random sizing and buffering mutations on each `tt` library of both families with the incremental
  timing session, compared bit for bit with a full analysis after every step (STA-REQ-0455), and
  checks bitwise identity between a 3-corner `sky130_fd_sc_hd` analysis and 3 single-corner
  analyses.
  Completes within 20 minutes and 4 GB RSS on a hosted runner.
- `--profile full`: runs all 9 libraries in `verification/pdks.toml`, runs 1,000 netlist mutation
  steps per `tt` family, and runs the 500k-cell and 1M-cell `sta.rbin` binary store and query scale
  benchmarks on `tt` corners. Completes within 3 hours and 16 GB RSS on a workstation.

## Sources and licenses

Every file comes from the vendor repository at a pinned commit, through the raw-file address of
that commit. `verification/pdks.toml` records the commit, the license, the SHA-256 and the size of
each library file.

| Repository | Commit | License |
|---|---|---|
| <https://github.com/fossi-foundation/skywater-pdk-libs-sky130_fd_sc_hd> | `9cb2d7cb8ed4619094263614039a61b6b2d22a88` | Apache-2.0 |
| <https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu7t5v0> | `0ef8889a8df0acf959683fbe73013f342a2206a2` | Apache-2.0 |
| <https://github.com/fossi-foundation/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu9t5v0> | `02b7af3f8d88c0b014d9af3db6a17f02aa983305` | Apache-2.0 |

The sky130 libraries are the files `timing/<library>__<corner>.lib`. The gf180mcu repositories
hold no combined Liberty file: `liberty/<library>__<corner>.lib` is a header with the library
attributes, and each cell is a fragment `cells/<cell>/<library>__<cell>[_<drive>]__<corner>.lib`.
`cargo xtask pdk fetch` assembles them deterministically into `<library>__<corner>.lib` and
verifies the SHA-256 and byte size against `verification/pdks.toml`.

## Fetch

```sh
cargo xtask pdk fetch                    # into target/pdk
cargo xtask pdk fetch --dir ../pdk-libs  # or RUSTEDA_PDK_DIR=../pdk-libs
```

- The library directory is `--dir`, else `RUSTEDA_PDK_DIR`, else `target/pdk` of the repository
  (with `RUSTSTA_PDK_DIR` accepted as a fallback). Inside the repository only directories that git
  ignores and that hold no tracked file are accepted.
- `curl` downloads, `sha256sum` (or `shasum -a 256`) hashes; a missing program is an error that
  names it.
- A library file that is present with the recorded size and SHA-256 is kept without network
  access.

## Run

```sh
cargo xtask verify pdk                               # skips with a banner when libraries are missing
cargo xtask verify pdk --profile pr --require        # missing libraries fail the suite (exit 1)
cargo xtask verify pdk --profile full --require      # full 9-corner + 500k/1M scale + 1000 mutations
cargo xtask verify pdk --dir ../pdk-libs --timeout 600
```

1. Libraries missing from the library directory print a `SKIPPED` banner on stdout and stderr.
   Without `--require` the command runs the libraries that are present and exits `0` when none is;
   with `--require` it exits `1` before running tests. Present libraries must match their manifest
   size and SHA-256.
2. Outputs go to `target/verify-pdk/`: one directory per library and design with `netlist.v`,
   `constraints.sdc`, and `report.json`, plus `summary.json`, `summary.txt` (including the Liberty
   special-cell marks table for `is_isolation_cell`, `always_on`, `is_level_shifter`, and
   `sparecell`), and `runs.tsv`. The directory is replaced on each run.

## Lab GPU verification procedure

Builds do not require a CUDA toolkit at compile time (`cudarc` loads the CUDA driver and NVRTC
dynamically at run time). On a workstation or lab machine with a CUDA-capable GPU and NVIDIA driver:

```sh
# 1. Run all STA, SDC, and compute verification suites including GPU backend equality and
#    device self-tests (`STA-REQ-0403`, `STA-REQ-0406`, `STA-REQ-0407`, `STA-REQ-0408`):
cargo xtask verify sta --gpu
cargo xtask verify compute --gpu

# 2. Fetch PDK libraries (or set RUSTEDA_PDK_DIR to a pre-fetched cache) and run the full
#    dual-PDK matrix with CUDA f64 vs CPU f64 bitwise comparison enabled:
cargo xtask pdk fetch
cargo xtask verify pdk --profile full --gpu --require
```

- `cargo xtask verify sta --gpu` builds `ruststa-cli`, `ruststa`, and `rusteda-compute` with
  `--features cuda` and runs the ignored GPU tests (`backend_equality` and `rusteda-compute` device
  self-tests), asserting bitwise equality between CPU `f64` and CUDA `f64` arrivals and slews on all
  32 corpus netlists, latches and two-clock designs, and `<= 1e-5` relative + `1e-6 ns` absolute
  agreement for CUDA `f32` (`STA-REQ-0403`).
- `cargo xtask verify pdk --profile full --gpu --require` additionally checks that CUDA `f64` and
  CPU `f64` produce bit-identical worst setup slack (`wns_setup_ns.to_bits()`) across every
  PDK-mapped design and corner.
