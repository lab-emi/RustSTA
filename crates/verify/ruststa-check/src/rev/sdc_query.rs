// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Canonical SDC object query resolution and exception/mode command parsers
//! (`STA-REQ-0150`, `0155`, `0221`, `0250` to `0270`).

use std::collections::{BTreeMap, BTreeSet};

use rusteda_core::liberty::Direction;
use rusteda_formats::verilog::ast::Direction as AstDir;

use super::{
    CanonSdc, ClockDef, ClockGroupKind, ClockGroupsSpec, DisableTimingRule,
    ExceptionEndpointFilter, ExceptionKind, ExceptionSpec, ThroughHopFilter, is_num, parse_f64,
    split_list_words, unquote,
};
use crate::rev::lib_model::{Analysis, CornerLib, Trans};
use crate::rev::verilog::{FlatDesign, NodeKind};

pub(super) fn parse_case_analysis(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    if args.is_empty() {
        return Ok(());
    }
    let val = match unquote(&args[0]).to_ascii_lowercase().as_str() {
        "0" | "zero" => false,
        "1" | "one" => true,
        // A transition case leaves the logic state of the pin unknown (STA-REQ-0150).
        "rising" | "rise" | "falling" | "fall" => return Ok(()),
        other => return Err(format!("C0503: invalid set_case_analysis value '{other}'")),
    };
    let objs = &args[1..];
    parse_logic_const(val, objs, sdc, design);
    Ok(())
}

pub(super) fn parse_logic_const(
    val: bool,
    objs: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) {
    for pi in resolve_port_indices(design, objs, &[]) {
        sdc.case_analysis.insert(design.ports[pi].name.clone(), val);
    }
    for ni in resolve_pin_nodes(design, objs) {
        sdc.case_analysis.insert(design.nodes[ni].name.clone(), val);
    }
}

pub(super) fn parse_disable_timing(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
) {
    let mut from_pin = None;
    let mut to_pin = None;
    let mut objs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-from" if i + 1 < args.len() => {
                from_pin = Some(unquote(&args[i + 1]));
                i += 2;
            }
            "-to" if i + 1 < args.len() => {
                to_pin = Some(unquote(&args[i + 1]));
                i += 2;
            }
            other if !other.starts_with('-') => {
                objs.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }

    for word in &objs {
        for expr in expand_obj_tokens(word) {
            if let Some(rest) = expr.strip_prefix("get_lib_cells ") {
                let (rx, nc, pats) = parse_query_args(rest);
                if let Some(clib) = lib {
                    for cname in clib.cells.keys() {
                        let short = cname.rsplit('/').next().unwrap_or(cname);
                        if pats
                            .iter()
                            .any(|p| pat_match(p, cname, rx, nc) || pat_match(p, short, rx, nc))
                        {
                            sdc.disable_timing.push(DisableTimingRule {
                                cell_or_inst_or_port: cname.clone(),
                                is_lib_cell: true,
                                from_pin: from_pin.clone(),
                                to_pin: to_pin.clone(),
                            });
                        }
                    }
                }
            } else if let Some(rest) = expr.strip_prefix("get_lib_pins ") {
                let (rx, nc, pats) = parse_query_args(rest);
                if let Some(clib) = lib {
                    for (cname, cdef) in &clib.cells {
                        for pname in cdef.pins.keys() {
                            let full = format!("{cname}/{pname}");
                            if pats.iter().any(|p| {
                                let short_pat = p.split_once('/').map_or(p.as_str(), |(_, r)| r);
                                pat_match(p, &full, rx, nc) || pat_match(short_pat, &full, rx, nc)
                            }) {
                                sdc.disable_timing.push(DisableTimingRule {
                                    cell_or_inst_or_port: full,
                                    is_lib_cell: true,
                                    from_pin: from_pin.clone(),
                                    to_pin: to_pin.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    for inst_idx in resolve_inst_indices(design, &objs) {
        sdc.disable_timing.push(DisableTimingRule {
            cell_or_inst_or_port: design.instances[inst_idx].name.clone(),
            is_lib_cell: false,
            from_pin: from_pin.clone(),
            to_pin: to_pin.clone(),
        });
    }
    for node_idx in resolve_pin_nodes(design, &objs) {
        sdc.disable_timing.push(DisableTimingRule {
            cell_or_inst_or_port: design.nodes[node_idx].name.clone(),
            is_lib_cell: false,
            from_pin: from_pin.clone(),
            to_pin: to_pin.clone(),
        });
    }
    for pi in resolve_port_indices(design, &objs, &[]) {
        sdc.disable_timing.push(DisableTimingRule {
            cell_or_inst_or_port: design.ports[pi].name.clone(),
            is_lib_cell: false,
            from_pin: from_pin.clone(),
            to_pin: to_pin.clone(),
        });
    }
}

pub(super) fn parse_max_time_borrow(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
) -> Result<(), String> {
    if args.is_empty() {
        return Ok(());
    }
    let val = parse_f64(&args[0])? * sdc.time_unit_ns;
    let objs = &args[1..];
    for cname in resolve_clock_names(&sdc.clocks, objs) {
        sdc.max_time_borrow_clock.insert(cname, val);
    }
    for ii in resolve_inst_indices(design, objs) {
        sdc.max_time_borrow_inst
            .insert(design.instances[ii].name.clone(), val);
    }
    for ni in resolve_pin_nodes(design, objs) {
        sdc.max_time_borrow_pin
            .insert(design.nodes[ni].name.clone(), val);
    }
    // Also support `[all_registers -level_sensitive ...]`
    for word in objs {
        for expr in expand_obj_tokens(word) {
            if let Some(rest) = expr.strip_prefix("all_registers") {
                let (insts, pins) = resolve_all_registers(rest, design, lib, &sdc.clocks);
                for ii in insts {
                    sdc.max_time_borrow_inst
                        .insert(design.instances[ii].name.clone(), val);
                }
                for ni in pins {
                    sdc.max_time_borrow_pin
                        .insert(design.nodes[ni].name.clone(), val);
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn parse_exception(
    cmd: &str,
    args: &[String],
    cmd_order: usize,
    line_no: usize,
    sdc: &mut CanonSdc,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut setup_flag = false;
    let mut hold_flag = false;
    let mut rise_flag = false;
    let mut fall_flag = false;
    let mut use_start = false;
    let mut use_end = false;
    let mut ignore_clock_latency = false;
    let mut has_from_flag = false;
    let mut has_to_flag = false;
    let mut from_filter = ExceptionEndpointFilter::default();
    let mut to_filter = ExceptionEndpointFilter::default();
    let mut through_hops = Vec::new();
    let mut empty_queries = Vec::new();
    let mut num_arg: Option<f64> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-setup" => {
                setup_flag = true;
                i += 1;
            }
            "-hold" => {
                hold_flag = true;
                i += 1;
            }
            "-rise" => {
                rise_flag = true;
                i += 1;
            }
            "-fall" => {
                fall_flag = true;
                i += 1;
            }
            "-start" => {
                use_start = true;
                i += 1;
            }
            "-end" => {
                use_end = true;
                i += 1;
            }
            "-ignore_clock_latency" => {
                ignore_clock_latency = true;
                i += 1;
            }
            "-reset_path" | "-comment" => {
                i += if args[i] == "-comment" { 2 } else { 1 };
            }
            f @ ("-from" | "-rise_from" | "-fall_from") if i + 1 < args.len() => {
                has_from_flag = true;
                let tr = match f {
                    "-rise_from" => Some(Trans::Rise),
                    "-fall_from" => Some(Trans::Fall),
                    _ => None,
                };
                resolve_exception_endpoint(
                    &args[i + 1],
                    tr,
                    design,
                    lib,
                    &sdc.clocks,
                    &mut from_filter,
                    &mut empty_queries,
                );
                i += 2;
            }
            f @ ("-to" | "-rise_to" | "-fall_to") if i + 1 < args.len() => {
                has_to_flag = true;
                let tr = match f {
                    "-rise_to" => Some(Trans::Rise),
                    "-fall_to" => Some(Trans::Fall),
                    _ => None,
                };
                resolve_exception_endpoint(
                    &args[i + 1],
                    tr,
                    design,
                    lib,
                    &sdc.clocks,
                    &mut to_filter,
                    &mut empty_queries,
                );
                i += 2;
            }
            f @ ("-through" | "-rise_through" | "-fall_through") if i + 1 < args.len() => {
                let tr = match f {
                    "-rise_through" => Some(Trans::Rise),
                    "-fall_through" => Some(Trans::Fall),
                    _ => None,
                };
                let mut hop = ThroughHopFilter::default();
                resolve_through_hop(
                    &args[i + 1],
                    tr,
                    design,
                    &sdc.clocks,
                    &mut hop,
                    &mut empty_queries,
                );
                through_hops.push(hop);
                i += 2;
            }
            other if num_arg.is_none() && is_num(other) => {
                num_arg = Some(parse_f64(other)?);
                i += 1;
            }
            _ => i += 1,
        }
    }

    let end_tr_filter = match (rise_flag, fall_flag) {
        (true, false) => Some(Trans::Rise),
        (false, true) => Some(Trans::Fall),
        _ => None,
    };

    let (kind, setup, hold, single_analysis, both_setup_and_hold_flags) = match cmd {
        "set_false_path" => {
            let s = setup_flag || !hold_flag;
            let h = hold_flag || !setup_flag;
            (
                ExceptionKind::FalsePath,
                s,
                h,
                setup_flag ^ hold_flag,
                false,
            )
        }
        "set_multicycle_path" => {
            let mult = num_arg.unwrap_or(1.0).round() as i32;
            let is_hold = hold_flag && !setup_flag;
            let single = setup_flag ^ hold_flag;
            let both_flags = setup_flag && hold_flag;
            let start = if use_start {
                true
            } else if use_end {
                false
            } else {
                is_hold
            };
            (
                ExceptionKind::MulticyclePath {
                    multiplier: mult,
                    is_hold,
                    use_start: start,
                },
                !is_hold,
                is_hold || !single,
                single,
                both_flags,
            )
        }
        "set_max_delay" => (
            ExceptionKind::MaxDelay {
                delay_ns: num_arg.unwrap_or(0.0) * tu,
                ignore_clock_latency,
            },
            true,
            false,
            true,
            false,
        ),
        "set_min_delay" => (
            ExceptionKind::MinDelay {
                delay_ns: num_arg.unwrap_or(0.0) * tu,
                ignore_clock_latency,
            },
            false,
            true,
            true,
            false,
        ),
        _ => unreachable!(),
    };

    sdc.exceptions.push(ExceptionSpec {
        cmd_index: cmd_order,
        line: line_no,
        kind,
        setup,
        hold,
        single_analysis,
        both_setup_and_hold_flags,
        end_tr_filter,
        has_from_flag,
        has_to_flag,
        empty_queries,
        from: has_from_flag.then_some(from_filter),
        to: has_to_flag.then_some(to_filter),
        through_hops,
    });
    Ok(())
}

pub(super) fn parse_clock_groups(
    args: &[String],
    cmd_order: usize,
    line_no: usize,
    sdc: &mut CanonSdc,
) -> Result<(), String> {
    let mut name = None;
    let mut kind = ClockGroupKind::Asynchronous;
    let mut allow_paths = false;
    let mut groups = Vec::new();
    let mut empty_queries = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-name" if i + 1 < args.len() => {
                name = Some(unquote(&args[i + 1]));
                i += 2;
            }
            "-asynchronous" => {
                kind = ClockGroupKind::Asynchronous;
                i += 1;
            }
            "-logically_exclusive" => {
                kind = ClockGroupKind::LogicallyExclusive;
                i += 1;
            }
            "-physically_exclusive" => {
                kind = ClockGroupKind::PhysicallyExclusive;
                i += 1;
            }
            "-allow_paths" => {
                allow_paths = true;
                i += 1;
            }
            "-group" if i + 1 < args.len() => {
                let clks: BTreeSet<String> =
                    resolve_clock_names(&sdc.clocks, std::slice::from_ref(&args[i + 1]))
                        .into_iter()
                        .collect();
                if clks.is_empty() {
                    empty_queries.push(args[i + 1].clone());
                }
                groups.push(clks);
                i += 2;
            }
            _ => i += 1,
        }
    }

    sdc.clock_groups.push(ClockGroupsSpec {
        cmd_index: cmd_order,
        line: line_no,
        name,
        kind,
        allow_paths,
        groups,
        empty_queries,
    });
    Ok(())
}

pub(super) fn apply_tr_an_f64(
    mat: &mut [[f64; 2]; 2],
    rise: bool,
    fall: bool,
    min: bool,
    max: bool,
    val: f64,
) {
    for (tr_do, tr) in [(rise || !fall, Trans::Rise), (fall || !rise, Trans::Fall)] {
        if tr_do {
            if min || !max {
                mat[tr.idx()][Analysis::Early.idx()] = val;
            }
            if max || !min {
                mat[tr.idx()][Analysis::Late.idx()] = val;
            }
        }
    }
}

pub(super) fn extract_first_name(arg: &str) -> String {
    for expr in expand_obj_tokens(arg) {
        if let Some(rest) = expr
            .strip_prefix("get_clocks ")
            .or_else(|| expr.strip_prefix("get_ports "))
        {
            let (_, _, pats) = parse_query_args(rest);
            if let Some(first) = pats.into_iter().next() {
                return first;
            }
        } else {
            return unquote(&expr);
        }
    }
    unquote(arg)
}

fn expand_obj_tokens(word: &str) -> Vec<String> {
    let t = word.trim();
    if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('"') && t.ends_with('"')) {
        let parts = split_list_words(t);
        if parts.len() > 1 || parts.first().is_some_and(|p| p.starts_with('[')) {
            return parts
                .into_iter()
                .flat_map(|p| expand_obj_tokens(&p))
                .collect();
        }
    }
    if t.starts_with('[') && t.ends_with(']') {
        let inner = t[1..t.len() - 1].trim();
        if let Some(rest) = inner.strip_prefix("list ") {
            return split_list_words(rest)
                .into_iter()
                .flat_map(|p| expand_obj_tokens(&p))
                .collect();
        }
        vec![inner.to_string()]
    } else {
        vec![unquote(t)]
    }
}

fn parse_query_args(rest: &str) -> (bool, bool, Vec<String>) {
    let words = split_list_words(rest);
    let mut regexp = false;
    let mut nocase = false;
    let mut pats = Vec::new();
    let mut i = 0;
    while i < words.len() {
        match words[i].as_str() {
            "-regexp" => {
                regexp = true;
                i += 1;
            }
            "-nocase" => {
                nocase = true;
                i += 1;
            }
            "-hierarchical" | "-quiet" => i += 1,
            "-filter" | "-of_objects" => i += 2,
            other if !other.starts_with('-') => {
                pats.extend(pattern_words(other));
                i += 1;
            }
            _ => i += 1,
        }
    }
    (regexp, nocase, pats)
}

/// The patterns of one query argument: a word, a braced or quoted list, or a `[list ...]`
/// command substitution such as `[get_ports [list a b]]` in canonical SDC (STA-REQ-0500).
fn pattern_words(word: &str) -> Vec<String> {
    let t = word.trim();
    if t.starts_with('[') && t.ends_with(']') {
        if let Some(rest) = t[1..t.len() - 1].trim().strip_prefix("list ") {
            return split_list_words(rest)
                .iter()
                .flat_map(|w| pattern_words(w))
                .collect();
        }
    }
    split_list_words(t).iter().map(|w| unquote(w)).collect()
}

#[must_use]
pub fn pat_match(pat: &str, text: &str, regexp: bool, nocase: bool) -> bool {
    let p_cow = if nocase {
        pat.to_ascii_lowercase()
    } else {
        pat.to_string()
    };
    let t_cow = if nocase {
        text.to_ascii_lowercase()
    } else {
        text.to_string()
    };
    if regexp {
        let p_trimmed = p_cow.strip_prefix('^').unwrap_or(&p_cow);
        let p_trimmed = p_trimmed.strip_suffix('$').unwrap_or(p_trimmed);
        simple_regex_match(p_trimmed.as_bytes(), t_cow.as_bytes())
    } else {
        glob_match(p_cow.as_bytes(), t_cow.as_bytes())
    }
}

fn simple_regex_match(p: &[u8], t: &[u8]) -> bool {
    // Handle top-level alternation `a|b`
    let mut depth = 0i32;
    let mut in_class = false;
    let mut escaped = false;
    for (i, &b) in p.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' {
            escaped = true;
            continue;
        }
        if b == b'[' {
            in_class = true;
        } else if b == b']' {
            in_class = false;
        } else if !in_class {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            } else if b == b'|' && depth == 0 {
                return simple_regex_match(&p[..i], t) || simple_regex_match(&p[i + 1..], t);
            }
        }
    }
    if p.is_empty() {
        return t.is_empty();
    }
    let (atom_end, match_fn): (usize, Box<dyn Fn(u8) -> bool>) = if p[0] == b'\\' && p.len() >= 2 {
        let c = p[1];
        (2, Box::new(move |b| b == c))
    } else if p[0] == b'.' {
        (1, Box::new(|_| true))
    } else if p[0] == b'[' {
        let mut end = 1;
        while end < p.len() && p[end] != b']' {
            if p[end] == b'\\' && end + 1 < p.len() {
                end += 2;
            } else {
                end += 1;
            }
        }
        let cls = p[1..end.min(p.len())].to_vec();
        let next = (end + 1).min(p.len());
        (
            next,
            Box::new(move |b| {
                let neg = cls.first() == Some(&b'^');
                let slice = if neg { &cls[1..] } else { &cls[..] };
                let mut matched = false;
                let mut j = 0;
                while j < slice.len() {
                    if j + 2 < slice.len() && slice[j + 1] == b'-' {
                        if b >= slice[j] && b <= slice[j + 2] {
                            matched = true;
                        }
                        j += 3;
                    } else {
                        if b == slice[j] {
                            matched = true;
                        }
                        j += 1;
                    }
                }
                if neg { !matched } else { matched }
            }),
        )
    } else {
        let c = p[0];
        (1, Box::new(move |b| b == c))
    };
    let quant = p.get(atom_end).copied();
    match quant {
        Some(b'*') => {
            let rest = &p[atom_end + 1..];
            if simple_regex_match(rest, t) {
                return true;
            }
            for k in 0..t.len() {
                if !match_fn(t[k]) {
                    break;
                }
                if simple_regex_match(rest, &t[k + 1..]) {
                    return true;
                }
            }
            false
        }
        Some(b'+') => {
            let rest = &p[atom_end + 1..];
            for k in 0..t.len() {
                if !match_fn(t[k]) {
                    break;
                }
                if simple_regex_match(rest, &t[k + 1..]) {
                    return true;
                }
            }
            false
        }
        Some(b'?') => {
            let rest = &p[atom_end + 1..];
            simple_regex_match(rest, t)
                || (!t.is_empty() && match_fn(t[0]) && simple_regex_match(rest, &t[1..]))
        }
        _ => {
            let rest = &p[atom_end..];
            !t.is_empty() && match_fn(t[0]) && simple_regex_match(rest, &t[1..])
        }
    }
}

fn glob_match(p: &[u8], t: &[u8]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'\\' && pi + 1 < p.len() {
            if p[pi + 1] == t[ti] {
                pi += 2;
                ti += 1;
                continue;
            }
        } else if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
            continue;
        } else if pi < p.len() && p[pi] == b'*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
            continue;
        }
        if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

pub(super) fn resolve_port_indices(
    design: &FlatDesign,
    words: &[String],
    clk_ports: &[String],
) -> Vec<usize> {
    let mut out = Vec::new();
    for w in words {
        for expr in expand_obj_tokens(w) {
            if expr == "all_inputs" || expr.starts_with("all_inputs ") {
                let no_clks = expr.contains("-no_clocks");
                for (i, p) in design.ports.iter().enumerate() {
                    if matches!(p.dir, AstDir::Input | AstDir::Inout)
                        && (!no_clks || !clk_ports.contains(&p.name))
                        && !out.contains(&i)
                    {
                        out.push(i);
                    }
                }
            } else if expr == "all_outputs" || expr.starts_with("all_outputs ") {
                for (i, p) in design.ports.iter().enumerate() {
                    if matches!(p.dir, AstDir::Output | AstDir::Inout) && !out.contains(&i) {
                        out.push(i);
                    }
                }
            } else {
                let rest = expr.strip_prefix("get_ports ").unwrap_or(&expr);
                let (rx, nc, pats) = parse_query_args(rest);
                for pat in &pats {
                    for (i, p) in design.ports.iter().enumerate() {
                        if (pat_match(pat, &p.name, rx, nc)
                            || p.bus_name.as_deref().is_some_and(|b| !rx && pat == b))
                            && !out.contains(&i)
                        {
                            out.push(i);
                        }
                    }
                }
            }
        }
    }
    out
}

fn resolve_pin_nodes(design: &FlatDesign, words: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    for w in words {
        for expr in expand_obj_tokens(w) {
            let rest = expr.strip_prefix("get_pins ").unwrap_or(&expr);
            let (rx, nc, pats) = parse_query_args(rest);
            for pat in &pats {
                for (ni, node) in design.nodes.iter().enumerate() {
                    if matches!(node.kind, NodeKind::InstancePin { .. })
                        && pat_match(pat, &node.name, rx, nc)
                        && !out.contains(&ni)
                    {
                        out.push(ni);
                    }
                }
            }
        }
    }
    out
}

fn resolve_inst_indices(design: &FlatDesign, words: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    for w in words {
        for expr in expand_obj_tokens(w) {
            let rest = expr.strip_prefix("get_cells ").unwrap_or(&expr);
            let (rx, nc, pats) = parse_query_args(rest);
            for pat in &pats {
                for (ii, inst) in design.instances.iter().enumerate() {
                    if pat_match(pat, &inst.name, rx, nc) && !out.contains(&ii) {
                        out.push(ii);
                    }
                }
            }
        }
    }
    out
}

pub(super) fn resolve_net_indices(design: &FlatDesign, words: &[String]) -> Vec<usize> {
    let mut out = Vec::new();
    for w in words {
        for expr in expand_obj_tokens(w) {
            let rest = expr.strip_prefix("get_nets ").unwrap_or(&expr);
            let (rx, nc, pats) = parse_query_args(rest);
            for pat in &pats {
                for (ni, net) in design.nets.iter().enumerate() {
                    if (pat_match(pat, &net.name, rx, nc)
                        || net.aliases.iter().any(|a| pat_match(pat, a, rx, nc)))
                        && !out.contains(&ni)
                    {
                        out.push(ni);
                    }
                }
            }
        }
    }
    out
}

fn resolve_clock_names(clocks: &BTreeMap<String, ClockDef>, words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for w in words {
        for expr in expand_obj_tokens(w) {
            if expr == "all_clocks" || expr.starts_with("all_clocks ") {
                for c in clocks.keys() {
                    if !out.contains(c) {
                        out.push(c.clone());
                    }
                }
            } else {
                let rest = expr.strip_prefix("get_clocks ").unwrap_or(&expr);
                let (rx, nc, pats) = parse_query_args(rest);
                for pat in &pats {
                    for c in clocks.keys() {
                        if pat_match(pat, c, rx, nc) && !out.contains(c) {
                            out.push(c.clone());
                        }
                    }
                }
            }
        }
    }
    out
}

pub(super) fn resolve_clock_or_port_clocks(
    clocks: &BTreeMap<String, ClockDef>,
    design: &FlatDesign,
    words: &[String],
) -> Vec<String> {
    let mut out = resolve_clock_names(clocks, words);
    let pis = resolve_port_indices(design, words, &[]);
    for pi in pis {
        let pname = &design.ports[pi].name;
        for (cname, cdef) in clocks {
            if cdef.sources.contains(pname) && !out.contains(cname) {
                out.push(cname.clone());
            }
        }
    }
    out
}

fn resolve_all_registers(
    rest: &str,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
    _clocks: &BTreeMap<String, ClockDef>,
) -> (Vec<usize>, Vec<usize>) {
    let Some(clib) = lib else {
        return (Vec::new(), Vec::new());
    };
    let words = split_list_words(rest);
    let mut data_pins = false;
    let mut clock_pins = false;
    let mut output_pins = false;
    let mut async_pins = false;
    let mut edge_only = false;
    let mut level_only = false;
    for w in &words {
        match w.as_str() {
            "-data_pins" => data_pins = true,
            "-clock_pins" => clock_pins = true,
            "-output_pins" => output_pins = true,
            "-async_pins" => async_pins = true,
            "-edge_triggered" => edge_only = true,
            "-level_sensitive" => level_only = true,
            _ => {}
        }
    }
    let want_pins = data_pins || clock_pins || output_pins || async_pins;
    let mut insts = Vec::new();
    let mut pins = Vec::new();

    for (ii, inst) in design.instances.iter().enumerate() {
        let Some(cdef) = clib.cells.get(&inst.cell_type) else {
            continue;
        };
        let is_latch = !cdef.latches.is_empty();
        let is_seq = is_latch || !cdef.launch_arcs.is_empty() || !cdef.check_arcs.is_empty();
        if !is_seq || (edge_only && is_latch) || (level_only && !is_latch) {
            continue;
        }
        if !want_pins {
            insts.push(ii);
            continue;
        }
        if data_pins {
            for chk in &cdef.check_arcs {
                if !chk.kind.is_async()
                    && let Some(&ni) = inst.pin_nodes.get(&chk.data_pin)
                    && !pins.contains(&ni)
                {
                    pins.push(ni);
                }
            }
        }
        if async_pins {
            for chk in &cdef.check_arcs {
                if chk.kind.is_async()
                    && let Some(&ni) = inst.pin_nodes.get(&chk.data_pin)
                    && !pins.contains(&ni)
                {
                    pins.push(ni);
                }
            }
        }
        if clock_pins {
            for (pname, pdef) in &cdef.pins {
                if (pdef.is_clock || cdef.latches.iter().any(|l| l.enable_pin == *pname))
                    && let Some(&ni) = inst.pin_nodes.get(pname)
                    && !pins.contains(&ni)
                {
                    pins.push(ni);
                }
            }
        }
        if output_pins {
            for (pname, pdef) in &cdef.pins {
                if pdef.direction == Direction::Output
                    && let Some(&ni) = inst.pin_nodes.get(pname)
                    && !pins.contains(&ni)
                {
                    pins.push(ni);
                }
            }
        }
    }
    (insts, pins)
}

fn resolve_exception_endpoint(
    word: &str,
    tr: Option<Trans>,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
    clocks: &BTreeMap<String, ClockDef>,
    filter: &mut ExceptionEndpointFilter,
    empty_queries: &mut Vec<String>,
) {
    let clk_port_names: Vec<String> = clocks
        .values()
        .flat_map(|c| c.sources.iter().cloned())
        .collect();
    for expr in expand_obj_tokens(word) {
        let mut matched = 0usize;
        if expr == "all_clocks"
            || expr.starts_with("all_clocks ")
            || expr.starts_with("get_clocks ")
        {
            for c in resolve_clock_names(clocks, std::slice::from_ref(&format!("[{expr}]"))) {
                filter.clocks.insert(c, tr);
                matched += 1;
            }
        } else if expr == "all_inputs"
            || expr.starts_with("all_inputs ")
            || expr == "all_outputs"
            || expr.starts_with("all_outputs ")
            || expr.starts_with("get_ports ")
        {
            for pi in resolve_port_indices(
                design,
                std::slice::from_ref(&format!("[{expr}]")),
                &clk_port_names,
            ) {
                filter.nodes.insert(design.ports[pi].node_idx, tr);
                matched += 1;
            }
        } else if expr.starts_with("get_pins ") {
            for ni in resolve_pin_nodes(design, std::slice::from_ref(&format!("[{expr}]"))) {
                filter.nodes.insert(ni, tr);
                if !filter.explicit_pins.contains(&ni) {
                    filter.explicit_pins.push(ni);
                }
                matched += 1;
            }
        } else if expr.starts_with("get_cells ") {
            for ii in resolve_inst_indices(design, std::slice::from_ref(&format!("[{expr}]"))) {
                filter.insts.insert(ii, tr);
                matched += 1;
            }
        } else if expr.starts_with("get_nets ") {
            for net_i in resolve_net_indices(design, std::slice::from_ref(&format!("[{expr}]"))) {
                if !filter.nets.contains(&net_i) {
                    filter.nets.push(net_i);
                }
                matched += 1;
            }
        } else if let Some(rest) = expr.strip_prefix("all_registers") {
            let (insts, pins) = resolve_all_registers(rest, design, lib, clocks);
            for ii in insts {
                filter.insts.insert(ii, tr);
                matched += 1;
            }
            for ni in pins {
                filter.nodes.insert(ni, tr);
                matched += 1;
            }
        } else {
            // Bare name fallback: check clocks, ports, pins, instances.
            for c in resolve_clock_names(clocks, std::slice::from_ref(&expr)) {
                filter.clocks.insert(c, tr);
                matched += 1;
            }
            for pi in resolve_port_indices(design, std::slice::from_ref(&expr), &clk_port_names) {
                filter.nodes.insert(design.ports[pi].node_idx, tr);
                matched += 1;
            }
            for ni in resolve_pin_nodes(design, std::slice::from_ref(&expr)) {
                filter.nodes.insert(ni, tr);
                if !filter.explicit_pins.contains(&ni) {
                    filter.explicit_pins.push(ni);
                }
                matched += 1;
            }
            for ii in resolve_inst_indices(design, std::slice::from_ref(&expr)) {
                filter.insts.insert(ii, tr);
                matched += 1;
            }
        }
        if matched == 0 {
            empty_queries.push(expr);
        }
    }
}

fn resolve_through_hop(
    word: &str,
    tr: Option<Trans>,
    design: &FlatDesign,
    clocks: &BTreeMap<String, ClockDef>,
    hop: &mut ThroughHopFilter,
    empty_queries: &mut Vec<String>,
) {
    let clk_port_names: Vec<String> = clocks
        .values()
        .flat_map(|c| c.sources.iter().cloned())
        .collect();
    for expr in expand_obj_tokens(word) {
        let mut matched = 0usize;
        if expr == "all_clocks"
            || expr.starts_with("all_clocks ")
            || expr.starts_with("get_clocks ")
        {
            for c in resolve_clock_names(clocks, std::slice::from_ref(&format!("[{expr}]"))) {
                if !hop.clocks.contains(&c) {
                    hop.clocks.push(c);
                }
                matched += 1;
            }
        } else if expr == "all_inputs"
            || expr.starts_with("all_inputs ")
            || expr == "all_outputs"
            || expr.starts_with("all_outputs ")
            || expr.starts_with("get_ports ")
        {
            for pi in resolve_port_indices(
                design,
                std::slice::from_ref(&format!("[{expr}]")),
                &clk_port_names,
            ) {
                hop.nodes.insert(design.ports[pi].node_idx, tr);
                matched += 1;
            }
        } else if expr.starts_with("get_pins ") {
            for ni in resolve_pin_nodes(design, std::slice::from_ref(&format!("[{expr}]"))) {
                hop.nodes.insert(ni, tr);
                matched += 1;
            }
        } else if expr.starts_with("get_nets ") {
            for net_i in resolve_net_indices(design, std::slice::from_ref(&format!("[{expr}]"))) {
                hop.nets.insert(net_i, tr);
                matched += 1;
            }
        } else if expr.starts_with("get_cells ") {
            for ii in resolve_inst_indices(design, std::slice::from_ref(&format!("[{expr}]"))) {
                hop.insts.insert(ii, tr);
                matched += 1;
            }
        } else {
            for ni in resolve_pin_nodes(design, std::slice::from_ref(&expr)) {
                hop.nodes.insert(ni, tr);
                matched += 1;
            }
            for pi in resolve_port_indices(design, std::slice::from_ref(&expr), &clk_port_names) {
                hop.nodes.insert(design.ports[pi].node_idx, tr);
                matched += 1;
            }
            for net_i in resolve_net_indices(design, std::slice::from_ref(&expr)) {
                hop.nets.insert(net_i, tr);
                matched += 1;
            }
            for ii in resolve_inst_indices(design, std::slice::from_ref(&expr)) {
                hop.insts.insert(ii, tr);
                matched += 1;
            }
        }
        if matched == 0 {
            empty_queries.push(expr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_query_args;

    // STA-REQ-0500: the canonical constraints write object lists as `[list ...]` command
    // substitutions; the checker reads every name of such a list.
    #[test]
    fn query_arguments_expand_list_substitutions() {
        let (_, _, pats) = parse_query_args("[list a b]");
        assert_eq!(pats, ["a", "b"]);
        let (_, _, pats) = parse_query_args("{c d} [list e [list f]]");
        assert_eq!(pats, ["c", "d", "e", "f"]);
        let (rx, _, pats) = parse_query_args("-regexp x.*");
        assert!(rx);
        assert_eq!(pats, ["x.*"]);
    }
}
