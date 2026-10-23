<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Commands and exit codes

Two binaries share the disk workspace. Every workspace command takes
`-w` / `--workspace DIR`; use `--format json` for structured output.

| Binary | Commands | Purpose |
|---|---|---|
| `rusteda` | `init`, `import` | Create the workspace and compile design inputs |
| `rusteda` | `show`, `log`, `diff`, `export` | Inspect revisions and export a netlist |
| `rusteda` | `revert` | Commit a revision restoring earlier content |
| `ruststa` | `run`, `update` | Compute and persist timing |
| `ruststa` | `path`, `inspect`, `report` | Query a saved timing result |
| `ruststa` | `check` | Verify saved timing and commit a check artifact |
| Both | `completions` | Generate shell completions without a workspace |

Updates to an existing workspace take `--base-rev revN` and reject `HEAD`.
Queries take `--rev REV`, defaulting to `HEAD`. `rusteda init` creates `rev0`
without a base revision.

## Common timing controls

```bash
ruststa run --help
ruststa path --help
ruststa report --help
ruststa check --help
```

| Control | Use |
|---|---|
| `--corner NAME` | Select analysis/report corner; repeatable on `run` |
| `--backend cpu\|cuda\|gpu` | Select propagation backend |
| `--precision f64\|f32` | Select propagation precision |
| `--threads N` | Set worker count |
| `--max-paths N` | Bound ranked paths stored per corner |
| `--max-state-gib GIB` | Bound estimated per-corner propagation state |
| `--sta-store full\|summary` | Select timing persistence detail |
| `--section NAME` | Select summary, paths, endpoints, pins, clocks, checks, unconstrained or notes |
| `--filter EXPR`, `--sort KEY`, `--limit N` | Restrict report tables |
| `--method independent\|full\|both` | Choose verification method |

## Exit status

| Status | Meaning |
|---|---|
| `0` | Operation completed successfully; inspect timing/check status separately |
| `1` | Rejected input, unsupported operation or unavailable backend |
| `2` | Internal error or corrupt artifact |
| `3` | Verification mismatch |
| `4` | Revision conflict |
| `5` | Inconclusive / undecided verification |
| `130` | Cancelled |

The [complete CLI manual](../user/cli/README.md) and
[protocol specification](../specs/cli/protocol.md) define the detailed envelopes,
error codes and handle resolution rules.
