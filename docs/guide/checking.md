<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Check timing results

Keep three questions separate: did the command complete, did the design meet
its timing constraints, and was the computed timing independently checked?

After a run produces `rev2`, check its saved result:

```bash
ruststa check --workspace ./ws --base-rev rev2 --method both --format json
```

| Method | Comparison | What it establishes |
|---|---|---|
| `full` | Saved state vs full engine rerun | Consistency with recomputation |
| `independent` | Saved state vs `ruststa-check` | Agreement with a separate timing implementation within its supported scope |
| `both` | Both comparisons | Both forms of evidence, subject to their diagnostics and budgets |

The independent checker does not depend on the timing engine or the shared SDC
compiler. It rereads cached source Liberty, structural Verilog and canonical SDC,
then compares timing values and endpoint checks in f64. `sta_check.json` stores
the result in a new revision.

The independent checker is bounded. Large designs with exceptions may exhaust
its path budget, and some large-design conversions and timing cases remain
limited. Preserve `undecided` and mismatch outcomes for investigation. Do not
replace them with a success claim because a full rerun agrees.

For an external comparison, run a pinned OpenSTA executable as a black box on
the same netlist, Liberty, SDC and analysis assumptions. Record version, corner,
units, clocks, wire model, input hashes and numerical tolerance. See
[verification scope](../verification/index.md). Compilation and self-authored
unit tests alone do not establish independent timing accuracy.
