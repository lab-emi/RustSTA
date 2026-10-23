# rusteda-formats

Readers and writers of the RustEDA tools, shared by every tool of the suite.

- `verilog` (`docs/specs/verilog/`): SystemVerilog reader and structural netlist writer.
  `SourceMap` holds source texts and `Span` locates tokens and syntax nodes;
  `Preprocessor::run` preprocesses one file (macros, conditional compilation, includes) and
  returns its tokens, with macro definitions that persist across the files one `Preprocessor`
  reads; `parse_tokens` builds the syntax tree in `ast`. `read_sources` combines both for a list
  of files, `read_texts` for in-memory texts, `parse_expression` reads one expression;
  `number::Literal` converts integer literals to bit patterns; `write::write_netlist` emits the
  structural netlist subset; `netlist` converts between `NlModule` and `rusteda_core::netlist::GateNetlist`
  and reads flat structural modules (`read_structural`).
- `liberty` (`docs/specs/liberty/`): Liberty library reader. `read_file` and `read_text` build
  the library model of `rusteda_core::liberty` (re-exported here) from a file or a text,
  `parse_function` reads a Boolean function; errors carry file, line and column.
- `sdc` (`docs/specs/sdc/`): reader, canonical writer, and `CONSTR v1` binary section codec for
  timing constraints in SDC syntax. `read` and `read_str` evaluate a bounded, deterministic Tcl
  subset (`set`, `unset`, `$v`, `${v}`, `$::env(X)`, `{*}`, `[expr]`, `if`/`elseif`/`else`,
  `info exists`, `foreach` over plain lists, `puts`, `list`, `concat`, `llength`, `lindex`,
  `lappend`, `split`, `join`, `string`, `add_to_collection`, `remove_from_collection`, `lsearch`,
  `lreplace`, and relative `source`) under explicit step and depth `Limits`, validate every command
  against its static `CmdSchema`, scale numeric values to canonical internal units (`ns`, `pF`,
  `kOhm`, `V`, `mA`, `nW`), and build typed `Cmd` entries and symbolic `Obj` / `Filter` query
  trees with structured `D0001`..`D0040` diagnostics. `write` emits canonical, variable-free SDC
  text (`read(write(s)) == s`); `Sdc::to_sections` and `Sdc::from_sections` encode and decode the
  13 `CONSTR v1` binary sections; `glob_match` and `glob_match_nocase` implement `*`/`?` matching.
