# Synthetic timing fixtures

Inputs of the RustSTA tests and of the backend benchmark. Everything in this directory is
synthetic: the cell libraries and the designs were made up for testing and model no real process
or product.

## `lib/`: cell libraries

Liberty libraries, one per variant of `ruststa_fixtures::liberty::Variant`. Cell names, functions,
areas and timing values are invented.

| File | Contents |
|---|---|
| `wt_full.lib` | production-style library: power pins and power tables, timing checks, several drive strengths, and special cells such as scan, clock-gating and physical-only cells |
| `wt_rich.lib` | many combinational cells, flip-flops of every kind, latches and tie cells |
| `wt_reset_only.lib` | few gates; flip-flops with an active-low clear only, no tie cells |
| `wt_minimal.lib` | an inverter, a two-input NAND and a flip-flop with a clear |

The files are generated: `cargo xtask fixtures` rewrites this directory from the
`ruststa-fixtures` crate. Do not edit them by hand; change the generator and run the command
again. Continuous integration checks that the committed files equal the generated ones.

## `netlists/`: gate-level netlists

Flat structural Verilog netlists of one module each, checked in:

- `<design>.v`: the 31 designs of the test corpus (`ruststa_fixtures::netlists::CORPUS`), from
  small control and datapath blocks such as `counter`, `fifo` and `uart_tx` up to the pipelined
  processor core `rv_core`. The module has the name of the file.
- `lanes_k1.v`: one lane of the datapath of the backend benchmark
  (`ruststa_fixtures::netlists::LANES_K1`, module `lanes`): a 16-by-16-bit multiply-accumulate
  into a 32-bit register and a comparison of its upper half.

The netlists were synthesized from synthetic RTL with the `wt_full` library, so every cell they
instantiate is a cell of `lib/wt_full.lib`. This repository contains no synthesizer: the netlists
are not regenerated here, and `cargo xtask fixtures` leaves them unchanged.

## Use in tests

Tests read these inputs through `ruststa-fixtures`, which generates the libraries in memory and
embeds the netlists, so a test never depends on the working directory. The files here serve runs
of the `ruststa` command by hand and let a reader see exactly what the tests analyze.
