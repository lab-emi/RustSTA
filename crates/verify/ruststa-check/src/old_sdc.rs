// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The SDC word lexer of the legacy checker: a frozen copy (STA-REQ-0020).
//!
//! The legacy checker's constraint reader ([`crate::sdc`]) reads its constraints through this copy
//! of the word lexer of the shared SDC module of `rusteda-formats` in the restructured workspace,
//! so that the schema-driven SDC layer can replace that module without changing the legacy
//! results. The code is that lexer unchanged: commands, options and arguments as words, `{...}`
//! lists, quoted strings and `[...]` object queries left unevaluated; no variables, expressions or
//! control flow. Work item Z1 removes it with the legacy checker.

use std::fmt;

/// A parse error with a 1-based position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.msg)
    }
}

impl std::error::Error for Error {}

/// An unevaluated object query such as `[get_ports {a b*}]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub name: String,
    pub patterns: Vec<String>,
    pub opts: Vec<(String, Option<String>)>,
}

/// A command argument or option value.
#[derive(Debug, Clone, PartialEq)]
pub enum Word {
    /// A bare word or quoted string.
    Text(String),
    /// A `{...}` list, split into its elements.
    List(Vec<String>),
    /// A nested object query.
    Query(Query),
}

impl Word {
    /// The value as a number.
    pub fn number(&self) -> Option<f64> {
        match self {
            Word::Text(t) => t.parse::<f64>().ok(),
            Word::List(v) if v.len() == 1 => v[0].parse::<f64>().ok(),
            _ => None,
        }
    }

    /// The value as a list of words (a text is a one-element list).
    pub fn items(&self) -> Vec<String> {
        match self {
            Word::Text(t) => t.split_whitespace().map(str::to_string).collect(),
            Word::List(v) => v.clone(),
            Word::Query(_) => Vec::new(),
        }
    }
}

/// One command.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub name: String,
    /// Options in order: name without the dash, value for valued options.
    pub opts: Vec<(String, Option<Word>)>,
    pub args: Vec<Word>,
    pub line: u32,
    pub col: u32,
}

impl Command {
    pub fn flag(&self, name: &str) -> bool {
        self.opts.iter().any(|(n, _)| n == name)
    }

    pub fn value(&self, name: &str) -> Option<&Word> {
        self.opts
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_ref())
    }
}

/// Options that take a value, per command; every other option is a flag. Commands missing from
/// the table take no valued options.
fn valued(cmd: &str, opt: &str) -> bool {
    let list: &[&str] = match cmd {
        "create_clock" => &["name", "period", "waveform", "comment"],
        "create_generated_clock" => &[
            "name",
            "source",
            "master_clock",
            "divide_by",
            "multiply_by",
            "edges",
            "edge_shift",
            "duty_cycle",
            "comment",
        ],
        "set_input_delay" | "set_output_delay" => &["clock", "reference_pin"],
        "set_input_transition" => &["clock"],
        "set_clock_uncertainty"
        | "set_false_path"
        | "set_multicycle_path"
        | "set_max_delay"
        | "set_min_delay" => &[
            "from",
            "to",
            "through",
            "rise_from",
            "fall_from",
            "rise_to",
            "fall_to",
            "comment",
        ],
        "set_clock_latency" => &["clock"],
        "set_units" => &[
            "time",
            "capacitance",
            "resistance",
            "voltage",
            "current",
            "power",
        ],
        "set_driving_cell" => &[
            "lib_cell",
            "library",
            "pin",
            "from_pin",
            "input_transition_rise",
            "input_transition_fall",
            "clock",
        ],
        "get_ports" | "get_clocks" | "get_pins" | "get_cells" | "get_nets" => &["filter"],
        "all_inputs" | "all_outputs" | "all_registers" => &["clock"],
        _ => &[],
    };
    list.contains(&opt)
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
    depth: u32,
}

#[derive(Debug)]
enum Tok {
    Word(Word, u32, u32),
    End,
    Eof,
}

impl Lexer<'_> {
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, Error> {
        Err(Error {
            line: self.line,
            col: self.col,
            msg: msg.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// Skips blanks and continuations; newlines and `;` end a command unless `inside`.
    fn blanks(&mut self, inside: bool) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r') => {
                    self.bump();
                }
                Some(b'\n') if inside => {
                    self.bump();
                }
                Some(b'\\') if self.s.get(self.i + 1) == Some(&b'\n') => {
                    self.bump();
                    self.bump();
                }
                Some(b'\\') if self.s.get(self.i + 1) == Some(&b'\r') => {
                    self.bump();
                    self.bump();
                    if self.peek() == Some(b'\n') {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn braced(&mut self) -> Result<String, Error> {
        // After the opening brace.
        let mut depth = 1u32;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.bump() else {
                return self.err("unterminated '{'");
            };
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b'\\' if self.peek() == Some(b'\n') => {
                    self.bump();
                    out.push(b' ');
                    continue;
                }
                _ => {}
            }
            out.push(c);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn quoted(&mut self) -> Result<String, Error> {
        let mut out = Vec::new();
        loop {
            let Some(c) = self.bump() else {
                return self.err("unterminated string");
            };
            match c {
                b'"' => break,
                b'\\' => match self.bump() {
                    Some(b'\n') => out.push(b' '),
                    Some(x) => out.push(x),
                    None => return self.err("unterminated string"),
                },
                _ => out.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn next(&mut self, inside: bool) -> Result<Tok, Error> {
        self.blanks(inside);
        let (line, col) = (self.line, self.col);
        let Some(c) = self.peek() else {
            return Ok(Tok::Eof);
        };
        match c {
            b'\n' | b';' => {
                self.bump();
                Ok(Tok::End)
            }
            b']' if inside => Ok(Tok::End),
            b'{' => {
                self.bump();
                let t = self.braced()?;
                let items = t.split_whitespace().map(str::to_string).collect();
                Ok(Tok::Word(Word::List(items), line, col))
            }
            b'"' => {
                self.bump();
                Ok(Tok::Word(Word::Text(self.quoted()?), line, col))
            }
            b'[' => {
                self.bump();
                if self.depth >= 32 {
                    return self.err("nested commands too deep");
                }
                self.depth += 1;
                let cmd = self.command(true)?;
                self.depth -= 1;
                self.blanks(true);
                if self.bump() != Some(b']') {
                    return self.err("expected ']'");
                }
                let Some(cmd) = cmd else {
                    return self.err("empty nested command");
                };
                let q = Query {
                    patterns: cmd.args.iter().flat_map(Word::items).collect(),
                    opts: cmd
                        .opts
                        .iter()
                        .map(|(n, v)| (n.clone(), v.as_ref().map(|w| w.items().join(" "))))
                        .collect(),
                    name: cmd.name,
                };
                Ok(Tok::Word(Word::Query(q), line, col))
            }
            // The message of the shared lexer. The ID is split so that this copy does not cite
            // the shared requirement, whose text belongs to the shared SDC layer.
            b'$' => self.err(concat!("variables are not supported (SDC-REQ", "-0001)")),
            _ => {
                let mut out = Vec::new();
                while let Some(c) = self.peek() {
                    if matches!(
                        c,
                        b' ' | b'\t' | b'\r' | b'\n' | b';' | b'[' | b']' | b'{' | b'}' | b'"'
                    ) {
                        break;
                    }
                    if c == b'\\' && matches!(self.s.get(self.i + 1), Some(b'\n' | b'\r')) {
                        break;
                    }
                    out.push(c);
                    self.bump();
                }
                if out.is_empty() {
                    return self.err(format!("unexpected '{}'", c as char));
                }
                Ok(Tok::Word(
                    Word::Text(String::from_utf8_lossy(&out).into_owned()),
                    line,
                    col,
                ))
            }
        }
    }

    /// Reads one command; `None` for an empty line or a comment.
    fn command(&mut self, inside: bool) -> Result<Option<Command>, Error> {
        self.blanks(inside);
        if self.peek() == Some(b'#') && !inside {
            while let Some(c) = self.bump() {
                if c == b'\n' {
                    break;
                }
            }
            return Ok(None);
        }
        let mut words: Vec<(Word, u32, u32)> = Vec::new();
        while let Tok::Word(w, l, c) = self.next(inside)? {
            words.push((w, l, c));
        }
        let Some((first, line, col)) = words.first().cloned() else {
            return Ok(None);
        };
        let Word::Text(name) = first else {
            return Err(Error {
                line,
                col,
                msg: "a command must start with a name".into(),
            });
        };
        let mut cmd = Command {
            name,
            opts: Vec::new(),
            args: Vec::new(),
            line,
            col,
        };
        let mut it = words.into_iter().skip(1).peekable();
        while let Some((w, l, c)) = it.next() {
            match &w {
                Word::Text(t) if t.starts_with('-') && t.len() > 1 && t.parse::<f64>().is_err() => {
                    let o = t[1..].to_string();
                    if valued(&cmd.name, &o) {
                        let Some((v, _, _)) = it.next() else {
                            return Err(Error {
                                line: l,
                                col: c,
                                msg: format!("option -{o} needs a value"),
                            });
                        };
                        cmd.opts.push((o, Some(v)));
                    } else {
                        cmd.opts.push((o, None));
                    }
                }
                _ => cmd.args.push(w),
            }
        }
        Ok(Some(cmd))
    }
}

/// Parses constraint text into commands.
pub fn parse(text: &str) -> Result<Vec<Command>, Error> {
    let mut lx = Lexer {
        s: text.as_bytes(),
        i: 0,
        line: 1,
        col: 1,
        depth: 0,
    };
    let mut out = Vec::new();
    loop {
        lx.blanks(false);
        match lx.peek() {
            None => break,
            Some(b'\n' | b';') => {
                lx.bump();
            }
            Some(b']') => return lx.err("unexpected ']'"),
            Some(b'}') => return lx.err("unexpected '}'"),
            _ => {
                if let Some(c) = lx.command(false)? {
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

/// Glob match with `*` (any sequence) and `?` (any character).
pub fn glob_match(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut i, mut j) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(si) = star {
            i = si + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // STA-REQ-0020: the command syntax of the legacy constraint subset.
    #[test]
    fn parses_typical_constraints() {
        let text = r#"
# clocks
create_clock -name clk -period 10.0 -waveform {0 5} [get_ports clk_i]
set_input_delay -clock clk -max 2.5 [get_ports {a b* c[3]}] ; set_load 0.01 [all_outputs]
set_output_delay -clock [get_clocks clk] -clock_fall 1 \
    [get_ports "out_o"]
set_clock_uncertainty -setup 0.1 [get_clocks clk]
"#;
        let c = parse(text).unwrap();
        assert_eq!(c.len(), 5);
        assert_eq!(c[0].name, "create_clock");
        assert_eq!(c[0].value("period").and_then(Word::number), Some(10.0));
        assert_eq!(
            c[0].value("waveform").map(Word::items),
            Some(vec!["0".to_string(), "5".to_string()])
        );
        match &c[0].args[0] {
            Word::Query(q) => {
                assert_eq!(q.name, "get_ports");
                assert_eq!(q.patterns, vec!["clk_i"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(c[1].flag("max"));
        assert_eq!(c[1].args[0].number(), Some(2.5));
        match &c[1].args[1] {
            Word::Query(q) => assert_eq!(q.patterns, vec!["a", "b*", "c[3]"]),
            other => panic!("{other:?}"),
        }
        assert_eq!(c[2].name, "set_load");
        assert!(matches!(&c[2].args[1], Word::Query(q) if q.name == "all_outputs"));
        assert!(c[3].flag("clock_fall"));
        assert!(matches!(c[3].value("clock"), Some(Word::Query(q)) if q.name == "get_clocks"));
        assert_eq!(c[3].line, 5);
        assert!(c[4].flag("setup"));
    }

    // STA-REQ-0020: malformed constraints are errors with a position, never panics.
    #[test]
    fn reports_errors_without_panicking() {
        for bad in [
            "create_clock -period",
            "set_load 1 [get_ports a",
            "set_load {1",
            "set x $y",
            "]",
            "\"open",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(glob_match("a*b?c", "axxbyc"));
        assert!(!glob_match("a*b", "ac"));
        assert!(glob_match("*", ""));
        assert!(glob_match("data[*]", "data[12]"));
    }
}
