# Mixed library and global OCV verification

`mixed_library_derates.py` compares the actual RustEDA import/STA CLI pipeline
with an external OpenSTA executable. It does not inspect reference source code.
The caller pins executable paths; every run records SHA-256 values, complete
stdout/stderr, original input hashes and each compared value. Missing tools,
missing paths and missing timing values fail rather than count as zero.

The input is a JSON list of cases with `name`, `libraries`, `netlist`, `sdc` and
`endpoints`. Optional `spef` exercises extracted RC. `driving_ports` names primary
ports driven by `set_driving_cell`; OpenSTA prints pre-driver input delay at these
ports, while RustSTA stores the arrived pad waveform. For zero-R cases the harness
compares the independently reported first sink arrival at that physical point.
With RC the primary-port arrival is explicitly listed as uncompared, and all
physical sink arrivals remain compared. Slews remain compared in both cases.

Use a new output directory, for example:

```
python3 verification/mixed_library_derates.py \
  --rusteda /absolute/pinned/rusteda --ruststa /absolute/pinned/ruststa \
  --reference /absolute/pinned/sta --cases /absolute/cases.json \
  --workspace /absolute/new-output-directory
```

The one-picosecond maximum tolerance accounts for the reference JSON report's
decimal precision. The gate includes both rise/fall and early/late. The SG13G2
campaign uses the original EZ130 and SRAM Liberty files, nominal and 0.95/1.05 OCV,
with a real SRAM clock/output path and data setup/hold endpoint. Sky130/GF180
controls use their original libraries. The Sky130 zero-wire control exposes its
internal wire as a port, because RustSTA automatically applies the library's
`Small` wire-load model to internal nets, while OpenSTA defaults to zero wires.
The configuration and actual scope are saved, so this difference cannot be
mistaken for a cell-delay error.

Both tools explicitly disable common-clock pessimism removal. The reference
executable must expose its corresponding control; an absent control fails.
Each case records the native analysis scope, including its actual wire model.

`tests/timing_derates.rs` also checks mixed-library incremental/full equality.
Its ignored `real_pdk_ocv_incremental_is_bitwise_full` test requires the original
three baseline PDK Liberty files in `RUSTEDA_PDK_DIR` and performs 128 bounded
resizes per library under global OCV. Run the suite in a memory-limited scope.
