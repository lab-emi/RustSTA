# Import detailed parasitics

Import a signal-net extraction with its matching mapped netlist, Liberty corner
and constraints. The extraction corner is explicit; missing annotation never
falls back to ideal wires.

```sh
rusteda init --workspace work --format json
rusteda import --workspace work --base-rev rev0 \
  --lib cells.lib --corner tt --netlist routed.v --sdc timing.sdc \
  --spef routed.spef.gz --spef-corner tt --format json
ruststa run --workspace work --base-rev rev1 --corner tt \
  --precision f64 --max-paths 8 --max-state-gib 1 --format json
ruststa report --workspace work --rev rev2 --section summary \
  --section checks --section unconstrained --format json
ruststa check --workspace work --base-rev rev2 --method full --format json
```

Use each returned revision for the next mutation. Plain `.spef` and gzip input
are accepted. Gzip is detected by its bytes; CRC, truncation and decoded-size
errors reject the import. `PARASIT v1` stores the canonical decoded source and
its corner/budgets; source identity records the original input. `spef_compression`,
`spef_source_bytes` and `spef_decoded_bytes` describe the transport.

## Supported data

The reader supports detailed `D_NET`, `NAME_MAP`, port/pin connections, scalar
ground and coupling capacitances, and resistances. It converts NS/PS, PF/FF and
OHM/KOHM to the engine's ns, pF and kOhm. Dividers, bus delimiters and escaped
names are resolved against the netlist. Names, directions and all required
connections must agree.

Reduced nets, inductance, value triplets, included pin capacitances and unknown
semantic attributes are explicit errors. RC meshes and disconnected nodes are
rejected by the timing consumer; zero-resistance edges are contracted before
checking the tree. Symlinks, FIFOs and devices are not accepted as source files.

The default budgets are:

| Resource | Default |
|---|---:|
| Encoded and decoded source, each | 32 MiB |
| Nets | 100,000 |
| RC nodes / name-map entries | 500,000 |
| Elements | 1,000,000 |
| Name storage | 16 MiB |

The import options are `--max-spef-mib`, `--max-rc-nets`, `--max-rc-nodes`,
`--max-rc-elements` and `--max-rc-name-mib`. Each has an absolute ceiling of
eight times its default. Raising parser caps does not remove the independent
`ruststa run --max-state-gib` limit. Estimate actual memory before larger runs.

## Timing interpretation

The reported model is `wire_model=spef_elmore`, `parasitics=spef`,
`delay_calculator=nldm_lumped_elmore`. It includes physical pin and explicit
port capacitances, computes first-moment wire delay and grounds coupling at
factor 1. Interconnect slew is copied from the driver; this is not waveform or
crosstalk analysis. See [the model definition](../specs/sta/parasitics.md).

Explicit `set_propagated_clock` on primary clocks uses cell and wire delay in
the clock network. Generated clocks, unsupported internal clock sources and
clock meshes are not silently treated as ideal. Scope reports `clock_model`
and keeps CPPR off.

The independent checker has no extracted-RC implementation and therefore
reports `undecided` for that scope. `--method full` checks consistency with a
full engine run. The [verification report](../verification/spef-report.html)
separates nodal first-moment agreement, real-PDK input coverage and differences
against a reference tool's default waveform model. A successful import or run
does not mean the design meets setup/hold or signoff requirements.
