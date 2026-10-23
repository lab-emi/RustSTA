// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Deterministic glob and regular-expression pattern matching for SDC object queries
//! (`SDC-REQ-0005`, `STA-REQ-0020`).
//!
//! Supports glob wildcards (`*`, `?`, `[...]`, `\`), bounded deterministic regular expressions
//! (`-regexp`: `^`, `$`, `.`, `*`, `+`, `?`, `{m,n}`, `[...]`, `(...)`, `|`, `\d`, `\w`, `\s`,
//! and escaped literals), case folding (`-nocase`), and hierarchy-aware `/` segment matching
//! (`-hierarchical`).

/// Matches a glob or regular-expression pattern against `candidate`.
///
/// When `regexp` is `false`, `pattern` is interpreted as an SDC glob pattern:
/// - If `hierarchical` is `true` or `pattern` contains `/`, `*` matches any character sequence.
/// - If `hierarchical` is `false` and `pattern` does not contain `/`, `*` and `?` do not match `/`
///   (so a non-hierarchical query without `/` matches only top-level names).
#[must_use]
pub fn matches_pattern(
    pattern: &str,
    candidate: &str,
    regexp: bool,
    nocase: bool,
    hierarchical: bool,
) -> bool {
    if regexp {
        regex_match(pattern, candidate, nocase)
    } else {
        let allow_slash = hierarchical || pattern.contains('/');
        glob_match(pattern, candidate, nocase, allow_slash)
    }
}

/// Glob match supporting `*`, `?`, `[abc]`, `[a-z]`, `[!...]`, `[^...]`, and `\` escapes.
#[must_use]
pub fn glob_match(pattern: &str, text: &str, nocase: bool, wildcard_matches_slash: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_slice(&p, &t, nocase, wildcard_matches_slash)
}

fn char_eq(a: char, b: char, nocase: bool) -> bool {
    if nocase {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn fold_char(c: char, nocase: bool) -> char {
    if nocase { c.to_ascii_lowercase() } else { c }
}

fn glob_slice(p: &[char], t: &[char], nocase: bool, slash_ok: bool) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        '*' => {
            let mut rest_p = &p[1..];
            while rest_p.first() == Some(&'*') {
                rest_p = &rest_p[1..];
            }
            if glob_slice(rest_p, t, nocase, slash_ok) {
                return true;
            }
            for i in 0..t.len() {
                if !slash_ok && t[i] == '/' {
                    break;
                }
                if glob_slice(rest_p, &t[i + 1..], nocase, slash_ok) {
                    return true;
                }
            }
            false
        }
        '?' => {
            let Some(&ch) = t.first() else {
                return false;
            };
            if !slash_ok && ch == '/' {
                return false;
            }
            glob_slice(&p[1..], &t[1..], nocase, slash_ok)
        }
        '[' => {
            // Only treat `[...]` as a glob character class when it is not a literal bus subscript
            // `[0]`, `[15]`, or when it contains ranges/multiple chars; for single-integer `[N]`
            // in netlist names like `a[0]`, matching `[` literally avoids surprising bus mismatches!
            if let Some((matched, close_idx)) = match_glob_bracket(p, t.first().copied(), nocase) {
                if matched && glob_slice(&p[close_idx + 1..], &t[1..], nocase, slash_ok) {
                    return true;
                }
            }
            if t.len() >= 3
                && t[0] == '['
                && t[2] == ']'
                && let Some((matched, close_idx)) = match_glob_bracket(p, Some(t[1]), nocase)
                && matched
                && glob_slice(&p[close_idx + 1..], &t[3..], nocase, slash_ok)
            {
                return true;
            }
            let Some(&ch) = t.first() else {
                return false;
            };
            char_eq('[', ch, nocase) && glob_slice(&p[1..], &t[1..], nocase, slash_ok)
        }
        '\\' => {
            let lit = if p.len() >= 2 { p[1] } else { '\\' };
            let step = if p.len() >= 2 { 2 } else { 1 };
            let Some(&ch) = t.first() else {
                return false;
            };
            char_eq(lit, ch, nocase) && glob_slice(&p[step..], &t[1..], nocase, slash_ok)
        }
        c => {
            let Some(&ch) = t.first() else {
                return false;
            };
            char_eq(c, ch, nocase) && glob_slice(&p[1..], &t[1..], nocase, slash_ok)
        }
    }
}

fn match_glob_bracket(p: &[char], target: Option<char>, nocase: bool) -> Option<(bool, usize)> {
    let mut i = 1;
    if i >= p.len() {
        return None;
    }
    let negated = matches!(p[i], '!' | '^');
    if negated {
        i += 1;
    }
    let start = i;
    while i < p.len() {
        if p[i] == '\\' && i + 1 < p.len() {
            i += 2;
            continue;
        }
        if p[i] == ']' && i > start {
            let class_chars = &p[start..i];
            if !negated && class_chars.iter().all(char::is_ascii_digit) {
                return None;
            }
            let Some(ch) = target else {
                return Some((false, i));
            };
            let hit = eval_char_class(class_chars, ch, nocase);
            return Some((if negated { !hit } else { hit }, i));
        }
        i += 1;
    }
    None
}

fn eval_char_class(spec: &[char], target: char, nocase: bool) -> bool {
    let t = fold_char(target, nocase);
    let mut k = 0;
    while k < spec.len() {
        let (c1, next_k) = if spec[k] == '\\' && k + 1 < spec.len() {
            (spec[k + 1], k + 2)
        } else {
            (spec[k], k + 1)
        };
        if next_k + 1 < spec.len() && spec[next_k] == '-' {
            let (c2, after_range) = if spec[next_k + 1] == '\\' && next_k + 2 < spec.len() {
                (spec[next_k + 2], next_k + 3)
            } else {
                (spec[next_k + 1], next_k + 2)
            };
            let lo = fold_char(c1, nocase);
            let hi = fold_char(c2, nocase);
            if lo <= t && t <= hi {
                return true;
            }
            k = after_range;
        } else {
            if fold_char(c1, nocase) == t {
                return true;
            }
            k = next_k;
        }
    }
    false
}

/// Deterministic regular-expression matcher (`-regexp`).
///
/// Unanchored by default (matching Tcl `regexp` / SDC `-regexp` semantics), honoring `^` and `$`
/// anchors when present.
#[must_use]
pub fn regex_match(pattern: &str, text: &str, nocase: bool) -> bool {
    let Ok(re) = parse_regex(pattern) else {
        return if nocase {
            text.to_ascii_lowercase()
                .contains(&pattern.to_ascii_lowercase())
        } else {
            text.contains(pattern)
        };
    };
    let chars: Vec<char> = text.chars().collect();
    for start in 0..=chars.len() {
        if eval_node(&re, &chars, start, nocase).is_some() {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
enum ReNode {
    Empty,
    AnchorStart,
    AnchorEnd,
    Any,
    Lit(char),
    Shorthand(char),
    Class {
        negated: bool,
        spec: Vec<char>,
    },
    Seq(Vec<ReNode>),
    Alt(Box<ReNode>, Box<ReNode>),
    Repeat {
        inner: Box<ReNode>,
        min: usize,
        max: Option<usize>,
    },
}

fn parse_regex(pattern: &str) -> Result<ReNode, ()> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut pos = 0;
    let node = parse_alt(&chars, &mut pos, false)?;
    if pos != chars.len() {
        return Err(());
    }
    Ok(node)
}

fn parse_alt(chars: &[char], pos: &mut usize, in_group: bool) -> Result<ReNode, ()> {
    let mut left = parse_seq(chars, pos, in_group)?;
    while *pos < chars.len() && chars[*pos] == '|' {
        *pos += 1;
        let right = parse_seq(chars, pos, in_group)?;
        left = ReNode::Alt(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_seq(chars: &[char], pos: &mut usize, in_group: bool) -> Result<ReNode, ()> {
    let mut items = Vec::new();
    while *pos < chars.len() {
        let c = chars[*pos];
        if c == '|' || (in_group && c == ')') {
            break;
        }
        if c == ')' {
            return Err(());
        }
        let atom = parse_atom(chars, pos)?;
        let quantified = parse_quantifier(atom, chars, pos)?;
        items.push(quantified);
    }
    if items.is_empty() {
        Ok(ReNode::Empty)
    } else if items.len() == 1 {
        Ok(items.into_iter().next().unwrap())
    } else {
        Ok(ReNode::Seq(items))
    }
}

fn parse_atom(chars: &[char], pos: &mut usize) -> Result<ReNode, ()> {
    let c = chars[*pos];
    *pos += 1;
    match c {
        '^' => Ok(ReNode::AnchorStart),
        '$' => Ok(ReNode::AnchorEnd),
        '.' => Ok(ReNode::Any),
        '(' => {
            if *pos + 1 < chars.len() && chars[*pos] == '?' && chars[*pos + 1] == ':' {
                *pos += 2;
            }
            let inner = parse_alt(chars, pos, true)?;
            if *pos >= chars.len() || chars[*pos] != ')' {
                return Err(());
            }
            *pos += 1;
            Ok(inner)
        }
        '[' => {
            if *pos >= chars.len() {
                return Err(());
            }
            let negated = chars[*pos] == '^';
            if negated {
                *pos += 1;
            }
            let start = *pos;
            while *pos < chars.len() {
                if chars[*pos] == '\\' && *pos + 1 < chars.len() {
                    *pos += 2;
                    continue;
                }
                if chars[*pos] == ']' && *pos > start {
                    let spec = chars[start..*pos].to_vec();
                    *pos += 1;
                    return Ok(ReNode::Class { negated, spec });
                }
                *pos += 1;
            }
            Err(())
        }
        '\\' => {
            if *pos >= chars.len() {
                return Ok(ReNode::Lit('\\'));
            }
            let esc = chars[*pos];
            *pos += 1;
            match esc {
                'd' | 'D' | 'w' | 'W' | 's' | 'S' => Ok(ReNode::Shorthand(esc)),
                'n' => Ok(ReNode::Lit('\n')),
                'r' => Ok(ReNode::Lit('\r')),
                't' => Ok(ReNode::Lit('\t')),
                other => Ok(ReNode::Lit(other)),
            }
        }
        other => Ok(ReNode::Lit(other)),
    }
}

fn parse_quantifier(atom: ReNode, chars: &[char], pos: &mut usize) -> Result<ReNode, ()> {
    if *pos >= chars.len() {
        return Ok(atom);
    }
    match chars[*pos] {
        '*' => {
            *pos += 1;
            Ok(ReNode::Repeat {
                inner: Box::new(atom),
                min: 0,
                max: None,
            })
        }
        '+' => {
            *pos += 1;
            Ok(ReNode::Repeat {
                inner: Box::new(atom),
                min: 1,
                max: None,
            })
        }
        '?' => {
            *pos += 1;
            Ok(ReNode::Repeat {
                inner: Box::new(atom),
                min: 0,
                max: Some(1),
            })
        }
        '{' => {
            let save = *pos;
            *pos += 1;
            if let Some((min, max)) = parse_brace_bounds(chars, pos) {
                Ok(ReNode::Repeat {
                    inner: Box::new(atom),
                    min,
                    max,
                })
            } else {
                *pos = save;
                Ok(atom)
            }
        }
        _ => Ok(atom),
    }
}

fn parse_brace_bounds(chars: &[char], pos: &mut usize) -> Option<(usize, Option<usize>)> {
    let start = *pos;
    while *pos < chars.len() && chars[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start {
        return None;
    }
    let min: usize = chars[start..*pos].iter().collect::<String>().parse().ok()?;
    if *pos >= chars.len() {
        return None;
    }
    if chars[*pos] == '}' {
        *pos += 1;
        return Some((min, Some(min)));
    }
    if chars[*pos] != ',' {
        return None;
    }
    *pos += 1;
    if *pos < chars.len() && chars[*pos] == '}' {
        *pos += 1;
        return Some((min, None));
    }
    let max_start = *pos;
    while *pos < chars.len() && chars[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == max_start || *pos >= chars.len() || chars[*pos] != '}' {
        return None;
    }
    let max: usize = chars[max_start..*pos]
        .iter()
        .collect::<String>()
        .parse()
        .ok()?;
    *pos += 1;
    if max < min {
        return None;
    }
    Some((min, Some(max)))
}

fn eval_node(node: &ReNode, text: &[char], idx: usize, nocase: bool) -> Option<usize> {
    eval_positions(node, text, idx, nocase).into_iter().next()
}

fn eval_positions(node: &ReNode, text: &[char], idx: usize, nocase: bool) -> Vec<usize> {
    match node {
        ReNode::Empty => vec![idx],
        ReNode::AnchorStart => {
            if idx == 0 {
                vec![0]
            } else {
                Vec::new()
            }
        }
        ReNode::AnchorEnd => {
            if idx == text.len() {
                vec![idx]
            } else {
                Vec::new()
            }
        }
        ReNode::Any => {
            if idx < text.len() {
                vec![idx + 1]
            } else {
                Vec::new()
            }
        }
        ReNode::Lit(c) => {
            if idx < text.len() && char_eq(*c, text[idx], nocase) {
                vec![idx + 1]
            } else {
                Vec::new()
            }
        }
        ReNode::Shorthand(s) => {
            if idx >= text.len() {
                return Vec::new();
            }
            let ch = text[idx];
            let ok = match s {
                'd' => ch.is_ascii_digit(),
                'D' => !ch.is_ascii_digit(),
                'w' => ch.is_ascii_alphanumeric() || ch == '_',
                'W' => !(ch.is_ascii_alphanumeric() || ch == '_'),
                's' => ch.is_ascii_whitespace(),
                'S' => !ch.is_ascii_whitespace(),
                _ => false,
            };
            if ok { vec![idx + 1] } else { Vec::new() }
        }
        ReNode::Class { negated, spec } => {
            if idx >= text.len() {
                return Vec::new();
            }
            let hit = eval_char_class(spec, text[idx], nocase);
            let ok = if *negated { !hit } else { hit };
            if ok { vec![idx + 1] } else { Vec::new() }
        }
        ReNode::Seq(items) => {
            let mut positions = vec![idx];
            for item in items {
                let mut next = Vec::new();
                for p in positions {
                    for np in eval_positions(item, text, p, nocase) {
                        if !next.contains(&np) {
                            next.push(np);
                        }
                    }
                }
                if next.is_empty() {
                    return Vec::new();
                }
                positions = next;
            }
            positions
        }
        ReNode::Alt(a, b) => {
            let mut out = eval_positions(a, text, idx, nocase);
            for np in eval_positions(b, text, idx, nocase) {
                if !out.contains(&np) {
                    out.push(np);
                }
            }
            out
        }
        ReNode::Repeat { inner, min, max } => {
            let mut by_count: Vec<Vec<usize>> = vec![vec![idx]];
            let limit = max.unwrap_or(text.len().saturating_sub(idx) + 1);
            for _ in 0..limit {
                let prev = by_count.last().unwrap();
                let mut step_next = Vec::new();
                for &p in prev {
                    for np in eval_positions(inner, text, p, nocase) {
                        // Prevent zero-width infinite repetition
                        if np > p && !step_next.contains(&np) {
                            step_next.push(np);
                        }
                    }
                }
                if step_next.is_empty() {
                    break;
                }
                by_count.push(step_next);
            }
            let mut out = Vec::new();
            if by_count.len() > *min {
                // Greedy first (longer matches first)
                for k in (*min..by_count.len()).rev() {
                    for &p in &by_count[k] {
                        if !out.contains(&p) {
                            out.push(p);
                        }
                    }
                }
            }
            out
        }
    }
}
