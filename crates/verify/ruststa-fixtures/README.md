# ruststa-fixtures

Synthetic test inputs of the timing engine, the checker and the command line.

- `liberty`: toy cell libraries with our own cell names, functions and timing values
  (`wt_rich`, `wt_reset_only`, `wt_minimal`), and through `liberty_full` a library written in the
  full style of production Liberty files (`wt_full`); `library(Variant)` returns the text of one.
- `netlists`: gate-level netlists of 31 synthetic designs (`CORPUS`, from small control blocks to
  a pipelined processor core; `corpus(name)` finds one) and `LANES_K1`, one lane of the
  multiply-accumulate datapath of the backend benchmark; every cell they use is a cell of
  `wt_full`.

Everything under `verification/fixtures/synthetic/sta/lib/` is produced from this crate
(`cargo xtask fixtures`). The netlists under `verification/fixtures/synthetic/sta/netlists/` are
checked in and embedded by `netlists`; no tool of this repository regenerates them, and
`cargo xtask fixtures` leaves them as they are.
