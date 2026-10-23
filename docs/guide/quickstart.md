<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Your first timing run

Analyze an eight-cell chain, inspect the result, and check the saved timing state.
All inputs below are included in the repository. Run from the checkout root after
[building the tools](installation.md).

The Liberty file is a synthetic test library, not a characterized manufacturing
process. The example has one corner and uses the default CPU f64 backend.

## Import the inputs

Choose a fresh workspace directory; `rusteda init` creates its initial `rev0`.

```bash
export PATH="$PWD/target/release:$PATH"
FIX=verification/fixtures/synthetic/sta
WS=.work/first-timing

rusteda init --workspace "$WS" --format json
rusteda import --workspace "$WS" --base-rev rev0 \
  --lib "$FIX/lib/wt_15.lib" \
  --netlist "$FIX/net/chain_8.v" \
  --sdc "$FIX/sdc/chain_8.sdc" --format json
```

The import creates `rev1`. It caches the verbatim Liberty input and compiles
netlist, library and constraint tables. Unsupported semantic constructs produce
explicit diagnostics; read stderr even when stdout is JSON.

## Analyze and inspect

```bash
ruststa run --workspace "$WS" --base-rev rev1 \
  --threads 2 --max-paths 5 --max-state-gib 1 --format json
ruststa report --workspace "$WS" --rev rev2 --section summary --format json
ruststa report --workspace "$WS" --rev rev2 --section paths --limit 5 --format json
ruststa report --workspace "$WS" --rev rev2 --section unconstrained --format json
```

The analysis creates `rev2` with `sta.rbin`. Reports query that revision directly.
Read the corner, wire model and constrained/unconstrained endpoint counts before
interpreting slack. A successful command indicates that the operation completed;
it does not establish that timing is met.

Path handles such as `path1` are scoped to the analyzed revision. Resolve a handle
returned by your path report:

```bash
ruststa path --workspace "$WS" --rev rev2 --target path1 --format json
```

## Check the saved result

```bash
ruststa check --workspace "$WS" --base-rev rev2 --method both --format json
rusteda show --workspace "$WS" --rev rev3 --format json
```

`both` compares the saved result with a full engine rerun and the independent
checker. The command commits `sta_check.json` in `rev3`; inspect the artifact and
diagnostics for the verification status. A resource limit or unsupported case
must not be interpreted as agreement.

The checker has a bounded scope, including known limitations on large exception
cases. See [checking](checking.md) and [verification scope](../verification/index.md).

## Next steps

- [Import your own design and multiple corners](timing.md).
- [Use JSON and revision guards in an agent loop](agents.md).
- [Understand supported constraints and wire models](../reference/scope.md).
