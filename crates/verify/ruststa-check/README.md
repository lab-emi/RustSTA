# ruststa-check

The independent timing checker (`docs/specs/sta/verification.md`, STA-REQ-0500): it recomputes
the timing of a revision or report from its own reading of the netlist, Liberty libraries and SDC
constraints. It shares no code with the timing engine; of the workspace crates it uses only
`rusteda-core`, `rusteda-formats` (the Liberty and general Verilog readers, never `sdc`) and
`rusteda-workspace`.

- `rev` checks workspace revisions (`check_revision`, `analyze_inputs`): it reads the verbatim
  `.lib` files from `cache/`, writes the revision's netlist to Verilog in memory and parses it with
  the general SystemVerilog front end, and reads the `CANON` section of `constraints.rbin` with its
  own canonical-SDC reader. It evaluates arrival times, slews, endpoint checks, required times and
  slacks on demand in `f64`, and compares per-pin quantities and endpoint roles against the engine's
  values (`Verdict::{Agree, Disagree, Undecided}`, `C0500`–`C0999` diagnostics, `mm<N>` handles).
- `design`, `cells`, `sdc`, `timing` and `checks` are the legacy report checker behind
  `ruststa check sta`; they keep their paths and behaviour until work item Z1 removes them.
