// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent reader for canonical SDC (`constraints/merged.sdc`) in the checker
//! (`STA-REQ-0200` to `0270`, `0500`, `0501`).
//!
//! Per Rule 8.2, this module never imports the format SDC parser; it parses the canonical
//! line-oriented SDC text directly from `constraints/merged.sdc`.

use std::collections::{BTreeMap, BTreeSet};

#[path = "sdc_query.rs"]
mod sdc_query;
pub use sdc_query::*;

use super::lib_model::{Analysis, CornerLib, Trans};
use super::verilog::FlatDesign;

/// Clock definition (`create_clock` + latency and transition specs).
#[derive(Debug, Clone)]
pub struct ClockDef {
    pub name: String,
    pub period_ns: f64,
    pub waveform_ns: (f64, f64),
    pub sources: Vec<String>,
    /// `source_latency[tr][an]` in `ns`.
    pub source_lat_ns: [[f64; 2]; 2],
    /// `network_latency[tr][an]` in `ns`.
    pub network_lat_ns: [[f64; 2]; 2],
    /// `transition[tr][an]` in `ns`.
    pub transition_ns: [[f64; 2]; 2],
}

impl ClockDef {
    #[must_use]
    pub const fn edge_time_ns(&self, edge: Trans) -> f64 {
        match edge {
            Trans::Rise => self.waveform_ns.0,
            Trans::Fall => self.waveform_ns.1,
        }
    }

    #[must_use]
    pub const fn source_latency(&self, edge: Trans, an: Analysis) -> f64 {
        self.source_lat_ns[edge.idx()][an.idx()]
    }

    #[must_use]
    pub const fn network_latency(&self, edge: Trans, an: Analysis) -> f64 {
        self.network_lat_ns[edge.idx()][an.idx()]
    }

    #[must_use]
    pub fn total_latency(&self, edge: Trans, an: Analysis) -> f64 {
        self.source_latency(edge, an) + self.network_latency(edge, an)
    }

    #[must_use]
    pub const fn transition(&self, edge: Trans, an: Analysis) -> f64 {
        self.transition_ns[edge.idx()][an.idx()]
    }
}

/// Per-(clock, edge) I/O delay entry on a primary port (`STA-REQ-0217` to `0219`).
#[derive(Debug, Clone)]
pub struct IoDelayEntry {
    pub clock: String,
    pub clock_edge: Trans,
    pub source_latency_included: bool,
    pub network_latency_included: bool,
    /// `bounds[tr][an]` in `ns`.
    pub bounds_ns: [[Option<f64>; 2]; 2],
}

impl IoDelayEntry {
    #[must_use]
    pub const fn bound(&self, tr: Trans, an: Analysis) -> Option<f64> {
        self.bounds_ns[tr.idx()][an.idx()]
    }
}

/// Driving cell specification on an input port (`STA-REQ-0218`).
#[derive(Debug, Clone)]
pub struct DrivingCellSpec {
    pub lib_cell: String,
    pub pin: String,
    pub from_pin: Option<String>,
    pub input_transition_rise: f64,
    pub input_transition_fall: f64,
}

/// Disable-timing rule from `set_disable_timing` (`STA-REQ-0155`).
#[derive(Debug, Clone)]
pub struct DisableTimingRule {
    pub cell_or_inst_or_port: String,
    pub is_lib_cell: bool,
    pub from_pin: Option<String>,
    pub to_pin: Option<String>,
}

/// Endpoint filter (`-from` / `-to`) of an exception command (`STA-REQ-0250` to `0265`).
#[derive(Debug, Clone, Default)]
pub struct ExceptionEndpointFilter {
    /// Clock name -> `None` (both edges) or `Some(edge)` (`-rise_from`/`-fall_from`).
    pub clocks: BTreeMap<String, Option<Trans>>,
    /// Node index (port or pin) -> `None` (both transitions) or `Some(tr)`.
    pub nodes: BTreeMap<usize, Option<Trans>>,
    /// Instance index (`[get_cells ...]`) -> `None` or `Some(tr)`.
    pub insts: BTreeMap<usize, Option<Trans>>,
    /// Nets passed to `-from` / `-to` (always ignored with `D0102`, `STA-REQ-0252`, `0253`).
    pub nets: Vec<usize>,
    /// Explicitly named instance pins (`[get_pins ...]`) for `D0102` startpoint/endpoint validation.
    pub explicit_pins: Vec<usize>,
}

impl ExceptionEndpointFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clocks.is_empty() && self.nodes.is_empty() && self.insts.is_empty()
    }
}

/// Through-hop filter (`-through` / `-rise_through` / `-fall_through`).
#[derive(Debug, Clone, Default)]
pub struct ThroughHopFilter {
    pub nodes: BTreeMap<usize, Option<Trans>>,
    pub nets: BTreeMap<usize, Option<Trans>>,
    pub insts: BTreeMap<usize, Option<Trans>>,
    pub clocks: Vec<String>,
}

impl ThroughHopFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.nets.is_empty() && self.insts.is_empty()
    }
}

/// Kind of timing exception (`STA-REQ-0250` to `0253`).
#[derive(Debug, Clone, PartialEq)]
pub enum ExceptionKind {
    FalsePath,
    MulticyclePath {
        multiplier: i32,
        is_hold: bool,
        use_start: bool,
    },
    MaxDelay {
        delay_ns: f64,
        ignore_clock_latency: bool,
    },
    MinDelay {
        delay_ns: f64,
        ignore_clock_latency: bool,
    },
}

impl ExceptionKind {
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::FalsePath => "false_path",
            Self::MulticyclePath { .. } => "multicycle_path",
            Self::MaxDelay { .. } => "max_delay",
            Self::MinDelay { .. } => "min_delay",
        }
    }
}

/// Parsed and resolved timing exception command (`STA-REQ-0250` to `0270`).
#[derive(Debug, Clone)]
pub struct ExceptionSpec {
    pub cmd_index: usize,
    pub line: usize,
    pub kind: ExceptionKind,
    pub setup: bool,
    pub hold: bool,
    pub single_analysis: bool,
    pub both_setup_and_hold_flags: bool,
    pub end_tr_filter: Option<Trans>,
    pub has_from_flag: bool,
    pub has_to_flag: bool,
    pub empty_queries: Vec<String>,
    pub from: Option<ExceptionEndpointFilter>,
    pub to: Option<ExceptionEndpointFilter>,
    pub through_hops: Vec<ThroughHopFilter>,
}

/// Kind of `set_clock_groups` relationship (`STA-REQ-0266`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockGroupKind {
    Asynchronous,
    LogicallyExclusive,
    PhysicallyExclusive,
}

/// Parsed and resolved `set_clock_groups` command (`STA-REQ-0266`).
#[derive(Debug, Clone)]
pub struct ClockGroupsSpec {
    pub cmd_index: usize,
    pub line: usize,
    pub name: Option<String>,
    pub kind: ClockGroupKind,
    pub allow_paths: bool,
    pub groups: Vec<BTreeSet<String>>,
    pub empty_queries: Vec<String>,
}

/// Canonical SDC model for a corner.
#[derive(Debug, Clone, Default)]
pub struct CanonSdc {
    pub time_unit_ns: f64,
    pub cap_unit_pf: f64,
    pub clocks: BTreeMap<String, ClockDef>,
    pub intra_uncertainty: BTreeMap<String, [Option<f64>; 2]>,
    pub inter_uncertainty: BTreeMap<(String, String), [Option<f64>; 2]>,
    pub input_delays: BTreeMap<String, Vec<IoDelayEntry>>,
    pub output_delays: BTreeMap<String, Vec<IoDelayEntry>>,
    pub driving_cells: BTreeMap<String, [[Option<DrivingCellSpec>; 2]; 2]>,
    pub input_transitions: BTreeMap<String, [[f64; 2]; 2]>,
    pub port_pin_loads: BTreeMap<String, [[f64; 2]; 2]>,
    pub port_wire_loads: BTreeMap<String, [[f64; 2]; 2]>,
    pub net_wire_loads: BTreeMap<String, [[Option<f64>; 2]; 2]>,
    pub wire_load_model: Option<String>,
    pub wire_load_selection: Option<String>,
    pub wire_load_mode: Option<String>,
    pub case_analysis: BTreeMap<String, bool>,
    pub disable_timing: Vec<DisableTimingRule>,
    pub max_time_borrow_clock: BTreeMap<String, f64>,
    pub max_time_borrow_inst: BTreeMap<String, f64>,
    pub max_time_borrow_pin: BTreeMap<String, f64>,
    pub exceptions: Vec<ExceptionSpec>,
    pub clock_groups: Vec<ClockGroupsSpec>,
    pub has_generated_clocks_or_sense: bool,
    pub has_propagated_clock: bool,
}

impl CanonSdc {
    #[must_use]
    pub fn has_exceptions_or_clock_groups(&self) -> bool {
        !self.exceptions.is_empty() || !self.clock_groups.is_empty()
    }

    #[must_use]
    pub fn port_pin_load(&self, port_name: &str, tr: Trans, an: Analysis) -> f64 {
        self.port_pin_loads
            .get(port_name)
            .map_or(0.0, |m| m[tr.idx()][an.idx()])
    }

    #[must_use]
    pub fn port_wire_load(&self, port_name: &str, tr: Trans, an: Analysis) -> f64 {
        self.port_wire_loads
            .get(port_name)
            .map_or(0.0, |m| m[tr.idx()][an.idx()])
    }

    #[must_use]
    pub fn net_wire_load(&self, net_name: &str, tr: Trans, an: Analysis) -> Option<f64> {
        self.net_wire_loads
            .get(net_name)
            .and_then(|m| m[tr.idx()][an.idx()])
    }

    #[must_use]
    pub fn input_transition(&self, port_name: &str, tr: Trans, an: Analysis) -> f64 {
        self.input_transitions
            .get(port_name)
            .map_or(0.0, |m| m[tr.idx()][an.idx()])
    }

    #[must_use]
    pub fn driving_cell(
        &self,
        port_name: &str,
        tr: Trans,
        an: Analysis,
    ) -> Option<&DrivingCellSpec> {
        self.driving_cells
            .get(port_name)
            .and_then(|m| m[tr.idx()][an.idx()].as_ref())
    }

    #[must_use]
    pub fn input_delays_for_port(&self, port_name: &str) -> &[IoDelayEntry] {
        self.input_delays.get(port_name).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn output_delays_for_port(&self, port_name: &str) -> &[IoDelayEntry] {
        self.output_delays.get(port_name).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn clock_uncertainty(&self, from_clk: &str, to_clk: &str, an: Analysis) -> f64 {
        if let Some(pair) = self
            .inter_uncertainty
            .get(&(from_clk.to_string(), to_clk.to_string()))
            && let Some(u) = pair[an.idx()]
        {
            return u;
        }
        if let Some(single) = self.intra_uncertainty.get(to_clk)
            && let Some(u) = single[an.idx()]
        {
            return u;
        }
        if from_clk == to_clk
            && let Some(single) = self.intra_uncertainty.get(from_clk)
            && let Some(u) = single[an.idx()]
        {
            return u;
        }
        0.0
    }

    /// Resolves `set_max_time_borrow` with pin > cell > clock specificity (`STA-REQ-0221`).
    #[must_use]
    pub fn max_time_borrow_for(
        &self,
        inst_name: &str,
        d_pin_name: &str,
        clk_name: &str,
    ) -> Option<f64> {
        self.max_time_borrow_pin
            .get(d_pin_name)
            .or_else(|| self.max_time_borrow_inst.get(inst_name))
            .or_else(|| self.max_time_borrow_clock.get(clk_name))
            .copied()
    }
}

/// Parses canonical SDC text and resolves object queries against `design` and `lib`.
pub fn parse_canon_sdc(
    text: &str,
    design: &FlatDesign,
    lib: Option<&CornerLib>,
) -> Result<CanonSdc, String> {
    let mut sdc = CanonSdc {
        time_unit_ns: lib.map_or(1.0, |l| l.time_unit_ns),
        cap_unit_pf: lib.map_or(1.0, |l| l.cap_unit_pf),
        ..CanonSdc::default()
    };

    let cmds = split_sdc_commands(text)?;
    let mut cmd_order = 0usize;

    for (line_no, words) in cmds {
        if words.is_empty() {
            continue;
        }
        cmd_order += 1;
        let cmd = words[0].as_str();
        let args = &words[1..];
        match cmd {
            "set_units" => parse_set_units(args, &mut sdc.time_unit_ns, &mut sdc.cap_unit_pf)?,
            "create_clock" => parse_create_clock(args, &mut sdc, design)?,
            "set_clock_latency" => parse_clock_latency(args, &mut sdc, design)?,
            "set_clock_transition" => parse_clock_transition(args, &mut sdc, design)?,
            "set_clock_uncertainty" => parse_clock_uncertainty(args, &mut sdc, design)?,
            "set_input_delay" => parse_io_delay(args, true, &mut sdc, design)?,
            "set_output_delay" => parse_io_delay(args, false, &mut sdc, design)?,
            "set_driving_cell" => parse_driving_cell(args, &mut sdc, design)?,
            "set_input_transition" => parse_input_transition(args, &mut sdc, design)?,
            "set_load" => parse_set_load(args, &mut sdc, design)?,
            "set_wire_load_model" => {
                let mut i = 0;
                while i < args.len() {
                    if args[i] == "-name" && i + 1 < args.len() {
                        sdc.wire_load_model = Some(unquote(&args[i + 1]));
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            "set_wire_load_selection_group" => {
                for a in args {
                    if !a.starts_with('-') && !a.starts_with('[') {
                        sdc.wire_load_selection = Some(unquote(a));
                        break;
                    }
                }
            }
            "set_wire_load_mode" => {
                if let Some(m) = args.first() {
                    sdc.wire_load_mode = Some(unquote(m));
                }
            }
            "set_case_analysis" => parse_case_analysis(args, &mut sdc, design)?,
            "set_logic_zero" => parse_logic_const(false, args, &mut sdc, design),
            "set_logic_one" => parse_logic_const(true, args, &mut sdc, design),
            "set_disable_timing" => parse_disable_timing(args, &mut sdc, design, lib),
            "set_max_time_borrow" => parse_max_time_borrow(args, &mut sdc, design, lib)?,
            "set_false_path" | "set_multicycle_path" | "set_max_delay" | "set_min_delay" => {
                parse_exception(cmd, args, cmd_order, line_no, &mut sdc, design, lib)?;
            }
            "set_clock_groups" => {
                parse_clock_groups(args, cmd_order, line_no, &mut sdc)?;
            }
            "create_generated_clock" | "set_clock_sense" => {
                sdc.has_generated_clocks_or_sense = true;
            }
            "set_propagated_clock" => {
                sdc.has_propagated_clock = true;
            }
            _ => {}
        }
    }

    Ok(sdc)
}

fn split_sdc_commands(text: &str) -> Result<Vec<(usize, Vec<String>)>, String> {
    let mut cmds = Vec::new();
    let mut cur_words: Vec<String> = Vec::new();
    let mut cur_tok = String::new();
    let mut brace_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut in_quotes = false;
    let mut line_no = 1usize;
    let mut cmd_start_line = 1usize;

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            if chars[i + 1] == '\n' {
                line_no += 1;
                i += 2;
                continue;
            }
            if chars[i + 1] == '\r' && i + 2 < chars.len() && chars[i + 2] == '\n' {
                line_no += 1;
                i += 3;
                continue;
            }
            cur_tok.push(c);
            cur_tok.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if !in_quotes && brace_depth == 0 && bracket_depth == 0 && c == '#' && cur_tok.is_empty() {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' && brace_depth == 0 {
            in_quotes = !in_quotes;
            cur_tok.push(c);
            i += 1;
            continue;
        }
        if !in_quotes {
            if c == '{' {
                brace_depth += 1;
            } else if c == '}' {
                brace_depth -= 1;
            } else if brace_depth == 0 && c == '[' {
                bracket_depth += 1;
            } else if brace_depth == 0 && c == ']' {
                bracket_depth -= 1;
            }
        }
        if !in_quotes && brace_depth == 0 && bracket_depth == 0 {
            if c == '\n' || c == ';' {
                if !cur_tok.is_empty() {
                    cur_words.push(std::mem::take(&mut cur_tok));
                }
                if !cur_words.is_empty() {
                    cmds.push((cmd_start_line, std::mem::take(&mut cur_words)));
                }
                if c == '\n' {
                    line_no += 1;
                }
                cmd_start_line = line_no;
                i += 1;
                continue;
            }
            if c.is_ascii_whitespace() {
                if !cur_tok.is_empty() {
                    if cur_words.is_empty() {
                        cmd_start_line = line_no;
                    }
                    cur_words.push(std::mem::take(&mut cur_tok));
                }
                i += 1;
                continue;
            }
        }
        if c == '\n' {
            line_no += 1;
        }
        if cur_words.is_empty() && cur_tok.is_empty() {
            cmd_start_line = line_no;
        }
        cur_tok.push(c);
        i += 1;
    }
    if brace_depth != 0 || bracket_depth != 0 || in_quotes {
        return Err("C0503: unbalanced quotes, braces, or brackets in SDC".into());
    }
    if !cur_tok.is_empty() {
        cur_words.push(cur_tok);
    }
    if !cur_words.is_empty() {
        cmds.push((cmd_start_line, cur_words));
    }
    Ok(cmds)
}

pub(crate) fn unquote(s: &str) -> String {
    let t = s.trim();
    let inner =
        if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('"') && t.ends_with('"')) {
            &t[1..t.len() - 1]
        } else {
            t
        };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(nx) = chars.next()
        {
            out.push(nx);
        } else {
            out.push(c);
        }
    }
    out
}

pub(crate) fn split_list_words(s: &str) -> Vec<String> {
    let t = s.trim();
    let inner =
        if (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('"') && t.ends_with('"')) {
            &t[1..t.len() - 1]
        } else {
            t
        };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut b_depth = 0i32;
    let mut k_depth = 0i32;
    let mut in_q = false;
    let chars: Vec<char> = inner.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            cur.push(c);
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '"' && b_depth == 0 {
            in_q = !in_q;
            cur.push(c);
            i += 1;
            continue;
        }
        if !in_q {
            match c {
                '{' => b_depth += 1,
                '}' => b_depth -= 1,
                '[' if b_depth == 0 => k_depth += 1,
                ']' if b_depth == 0 => k_depth -= 1,
                w if w.is_ascii_whitespace() && b_depth == 0 && k_depth == 0 => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        cur.push(c);
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(crate) fn is_num(s: &str) -> bool {
    s.parse::<f64>().is_ok()
}

pub(crate) fn parse_f64(s: &str) -> Result<f64, String> {
    unquote(s)
        .parse::<f64>()
        .map_err(|_| format!("C0503: invalid numeric value '{s}' in SDC"))
}

fn parse_set_units(args: &[String], tu: &mut f64, cu: &mut f64) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-time" if i + 1 < args.len() => {
                *tu = parse_unit_scale(&unquote(&args[i + 1]), "s", 1e9)?;
                i += 2;
            }
            "-capacitance" if i + 1 < args.len() => {
                *cu = parse_unit_scale(&unquote(&args[i + 1]), "f", 1e12)?;
                i += 2;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

fn parse_unit_scale(s: &str, suffix_char: &str, base_mult: f64) -> Result<f64, String> {
    let clean = s.trim().to_ascii_lowercase();
    let idx = clean
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(clean.len());
    let (num_s, unit_s) = clean.split_at(idx);
    let mult = if num_s.is_empty() {
        1.0
    } else {
        num_s
            .parse::<f64>()
            .map_err(|_| format!("C0503: invalid unit '{s}'"))?
    };
    let prefix_scale = match unit_s.strip_suffix(suffix_char).unwrap_or(unit_s) {
        "f" => 1e-15,
        "p" => 1e-12,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "" => 1.0,
        _ => return Err(format!("C0503: unsupported unit '{s}'")),
    };
    Ok(mult * prefix_scale * base_mult)
}

fn parse_create_clock(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut name: Option<String> = None;
    let mut period_ns: Option<f64> = None;
    let mut waveform_ns: Option<(f64, f64)> = None;
    let mut obj_words = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-name" if i + 1 < args.len() => {
                name = Some(unquote(&args[i + 1]));
                i += 2;
            }
            "-period" if i + 1 < args.len() => {
                period_ns = Some(parse_f64(&args[i + 1])? * tu);
                i += 2;
            }
            "-waveform" if i + 1 < args.len() => {
                let parts = split_list_words(&args[i + 1]);
                if parts.len() >= 2 {
                    waveform_ns = Some((parse_f64(&parts[0])? * tu, parse_f64(&parts[1])? * tu));
                }
                i += 2;
            }
            "-add" | "-comment" => i += 1,
            other => {
                obj_words.push(other.to_string());
                i += 1;
            }
        }
    }

    let p = period_ns.ok_or_else(|| "C0503: create_clock missing -period".to_string())?;
    let wf = waveform_ns.unwrap_or((0.0, p * 0.5));
    let sources: Vec<String> = resolve_port_indices(design, &obj_words, &[])
        .into_iter()
        .map(|pi| design.ports[pi].name.clone())
        .collect();
    let cname = name
        .or_else(|| sources.first().cloned())
        .ok_or_else(|| "C0503: create_clock missing name and source ports".to_string())?;

    sdc.clocks.insert(
        cname.clone(),
        ClockDef {
            name: cname,
            period_ns: p,
            waveform_ns: wf,
            sources,
            source_lat_ns: [[0.0; 2]; 2],
            network_lat_ns: [[0.0; 2]; 2],
            transition_ns: [[0.0; 2]; 2],
        },
    );
    Ok(())
}

fn parse_clock_latency(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut is_source = false;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut val: Option<f64> = None;
    let mut objs = Vec::new();

    for a in args {
        match a.as_str() {
            "-source" => is_source = true,
            "-rise" => rise = true,
            "-fall" => fall = true,
            "-min" | "-early" => min = true,
            "-max" | "-late" => max = true,
            other if val.is_none() && is_num(other) => val = Some(parse_f64(other)? * tu),
            other if !other.starts_with('-') => objs.push(other.to_string()),
            _ => {}
        }
    }
    let v = val.ok_or_else(|| "C0503: set_clock_latency missing value".to_string())?;
    let clks = resolve_clock_or_port_clocks(&sdc.clocks, design, &objs);
    for cname in clks {
        if let Some(cdef) = sdc.clocks.get_mut(&cname) {
            let slot = if is_source {
                &mut cdef.source_lat_ns
            } else {
                &mut cdef.network_lat_ns
            };
            apply_tr_an_f64(slot, rise, fall, min, max, v);
        }
    }
    Ok(())
}

fn parse_clock_transition(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut val: Option<f64> = None;
    let mut objs = Vec::new();

    for a in args {
        match a.as_str() {
            "-rise" => rise = true,
            "-fall" => fall = true,
            "-min" => min = true,
            "-max" => max = true,
            other if val.is_none() && is_num(other) => val = Some(parse_f64(other)? * tu),
            other if !other.starts_with('-') => objs.push(other.to_string()),
            _ => {}
        }
    }
    let v = val.ok_or_else(|| "C0503: set_clock_transition missing value".to_string())?;
    let clks = resolve_clock_or_port_clocks(&sdc.clocks, design, &objs);
    for cname in clks {
        if let Some(cdef) = sdc.clocks.get_mut(&cname) {
            apply_tr_an_f64(&mut cdef.transition_ns, rise, fall, min, max, v);
        }
    }
    Ok(())
}

fn parse_clock_uncertainty(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut setup = false;
    let mut hold = false;
    let mut from_objs = Vec::new();
    let mut to_objs = Vec::new();
    let mut objs = Vec::new();
    let mut val: Option<f64> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-setup" => {
                setup = true;
                i += 1;
            }
            "-hold" => {
                hold = true;
                i += 1;
            }
            "-from" | "-rise_from" | "-fall_from" if i + 1 < args.len() => {
                from_objs.push(args[i + 1].clone());
                i += 2;
            }
            "-to" | "-rise_to" | "-fall_to" if i + 1 < args.len() => {
                to_objs.push(args[i + 1].clone());
                i += 2;
            }
            other if is_num(other) => {
                val = Some(parse_f64(other)? * tu);
                i += 1;
            }
            other if !other.starts_with('-') => {
                objs.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let v = val.ok_or_else(|| "C0503: set_clock_uncertainty missing value".to_string())?;
    let apply_early = hold || !setup;
    let apply_late = setup || !hold;

    if !from_objs.is_empty() || !to_objs.is_empty() {
        let f_clks = if from_objs.is_empty() {
            sdc.clocks.keys().cloned().collect()
        } else {
            resolve_clock_or_port_clocks(&sdc.clocks, design, &from_objs)
        };
        let t_clks = if to_objs.is_empty() {
            sdc.clocks.keys().cloned().collect()
        } else {
            resolve_clock_or_port_clocks(&sdc.clocks, design, &to_objs)
        };
        for fc in &f_clks {
            for tc in &t_clks {
                let entry = sdc
                    .inter_uncertainty
                    .entry((fc.clone(), tc.clone()))
                    .or_insert([None, None]);
                if apply_early {
                    entry[Analysis::Early.idx()] = Some(v);
                }
                if apply_late {
                    entry[Analysis::Late.idx()] = Some(v);
                }
            }
        }
    } else {
        let clks = resolve_clock_or_port_clocks(&sdc.clocks, design, &objs);
        for c in clks {
            let entry = sdc.intra_uncertainty.entry(c).or_insert([None, None]);
            if apply_early {
                entry[Analysis::Early.idx()] = Some(v);
            }
            if apply_late {
                entry[Analysis::Late.idx()] = Some(v);
            }
        }
    }
    Ok(())
}

fn parse_io_delay(
    args: &[String],
    is_input: bool,
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut clock = String::new();
    let mut clock_edge = Trans::Rise;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut add_delay = false;
    let mut src_inc = false;
    let mut net_inc = false;
    let mut val: Option<f64> = None;
    let mut objs = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-clock" if i + 1 < args.len() => {
                clock = extract_first_name(&args[i + 1]);
                i += 2;
            }
            "-clock_fall" => {
                clock_edge = Trans::Fall;
                i += 1;
            }
            "-rise" => {
                rise = true;
                i += 1;
            }
            "-fall" => {
                fall = true;
                i += 1;
            }
            "-min" => {
                min = true;
                i += 1;
            }
            "-max" => {
                max = true;
                i += 1;
            }
            "-add_delay" => {
                add_delay = true;
                i += 1;
            }
            "-source_latency_included" => {
                src_inc = true;
                i += 1;
            }
            "-network_latency_included" => {
                net_inc = true;
                i += 1;
            }
            "-reference_pin" | "-level_sensitive" => {
                i += if args[i] == "-reference_pin" { 2 } else { 1 };
            }
            other if val.is_none() && is_num(other) => {
                val = Some(parse_f64(other)? * tu);
                i += 1;
            }
            other if !other.starts_with('-') => {
                objs.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }

    let v = val.ok_or_else(|| "C0503: set_input/output_delay missing value".to_string())?;
    let clk_port_names: Vec<String> = sdc
        .clocks
        .values()
        .flat_map(|c| c.sources.iter().cloned())
        .collect();
    let port_indices = resolve_port_indices(design, &objs, &clk_port_names);
    let apply_r = rise || !fall;
    let apply_f = fall || !rise;
    let apply_early = min || !max;
    let apply_late = max || !min;

    let table = if is_input {
        &mut sdc.input_delays
    } else {
        &mut sdc.output_delays
    };

    for pi in port_indices {
        let pname = design.ports[pi].name.clone();
        let list = table.entry(pname).or_default();
        if !add_delay {
            list.retain(|e| e.clock == clock && e.clock_edge == clock_edge);
        }
        let entry = if let Some(existing) = list
            .iter_mut()
            .find(|e| e.clock == clock && e.clock_edge == clock_edge)
        {
            existing.source_latency_included = src_inc;
            existing.network_latency_included = net_inc;
            existing
        } else {
            list.push(IoDelayEntry {
                clock: clock.clone(),
                clock_edge,
                source_latency_included: src_inc,
                network_latency_included: net_inc,
                bounds_ns: [[None; 2]; 2],
            });
            list.last_mut().unwrap()
        };
        for (tr_do, tr) in [(apply_r, Trans::Rise), (apply_f, Trans::Fall)] {
            if tr_do {
                if apply_early {
                    entry.bounds_ns[tr.idx()][Analysis::Early.idx()] = Some(v);
                }
                if apply_late {
                    entry.bounds_ns[tr.idx()][Analysis::Late.idx()] = Some(v);
                }
            }
        }
    }
    Ok(())
}

fn parse_driving_cell(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut lib_cell = String::new();
    let mut pin = None;
    let mut from_pin = None;
    let mut s_r = 0.0;
    let mut s_f = 0.0;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut objs = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-lib_cell" if i + 1 < args.len() => {
                lib_cell = unquote(&args[i + 1]);
                i += 2;
            }
            "-pin" if i + 1 < args.len() => {
                pin = Some(unquote(&args[i + 1]));
                i += 2;
            }
            "-from_pin" if i + 1 < args.len() => {
                from_pin = Some(unquote(&args[i + 1]));
                i += 2;
            }
            "-input_transition_rise" if i + 1 < args.len() => {
                s_r = parse_f64(&args[i + 1])? * tu;
                i += 2;
            }
            "-input_transition_fall" if i + 1 < args.len() => {
                s_f = parse_f64(&args[i + 1])? * tu;
                i += 2;
            }
            "-library" | "-multiply_by" | "-dont_scale" | "-no_design_rule" => {
                i += if matches!(args[i].as_str(), "-library" | "-multiply_by") {
                    2
                } else {
                    1
                };
            }
            "-rise" => {
                rise = true;
                i += 1;
            }
            "-fall" => {
                fall = true;
                i += 1;
            }
            "-min" => {
                min = true;
                i += 1;
            }
            "-max" => {
                max = true;
                i += 1;
            }
            other if !other.starts_with('-') => {
                objs.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let spec = DrivingCellSpec {
        lib_cell,
        pin: pin.unwrap_or_else(|| "Y".to_string()),
        from_pin,
        input_transition_rise: s_r,
        input_transition_fall: s_f,
    };
    let clk_port_names: Vec<String> = sdc
        .clocks
        .values()
        .flat_map(|c| c.sources.iter().cloned())
        .collect();
    for pi in resolve_port_indices(design, &objs, &clk_port_names) {
        let pname = design.ports[pi].name.clone();
        let mat = sdc.driving_cells.entry(pname).or_default();
        for (tr_do, tr) in [(rise || !fall, Trans::Rise), (fall || !rise, Trans::Fall)] {
            if tr_do {
                if min || !max {
                    mat[tr.idx()][Analysis::Early.idx()] = Some(spec.clone());
                }
                if max || !min {
                    mat[tr.idx()][Analysis::Late.idx()] = Some(spec.clone());
                }
            }
        }
    }
    Ok(())
}

fn parse_input_transition(
    args: &[String],
    sdc: &mut CanonSdc,
    design: &FlatDesign,
) -> Result<(), String> {
    let tu = sdc.time_unit_ns;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut val: Option<f64> = None;
    let mut objs = Vec::new();

    for a in args {
        match a.as_str() {
            "-rise" => rise = true,
            "-fall" => fall = true,
            "-min" => min = true,
            "-max" => max = true,
            other if val.is_none() && is_num(other) => val = Some(parse_f64(other)? * tu),
            other if !other.starts_with('-') => objs.push(other.to_string()),
            _ => {}
        }
    }
    let v = val.ok_or_else(|| "C0503: set_input_transition missing value".to_string())?;
    let clk_port_names: Vec<String> = sdc
        .clocks
        .values()
        .flat_map(|c| c.sources.iter().cloned())
        .collect();
    for pi in resolve_port_indices(design, &objs, &clk_port_names) {
        let pname = design.ports[pi].name.clone();
        let mat = sdc.input_transitions.entry(pname).or_insert([[0.0; 2]; 2]);
        apply_tr_an_f64(mat, rise, fall, min, max, v);
    }
    Ok(())
}

fn parse_set_load(args: &[String], sdc: &mut CanonSdc, design: &FlatDesign) -> Result<(), String> {
    let cu = sdc.cap_unit_pf;
    let mut rise = false;
    let mut fall = false;
    let mut min = false;
    let mut max = false;
    let mut is_wire = false;
    let mut val: Option<f64> = None;
    let mut objs = Vec::new();

    for a in args {
        match a.as_str() {
            "-rise" => rise = true,
            "-fall" => fall = true,
            "-min" => min = true,
            "-max" => max = true,
            "-wire_load" => is_wire = true,
            "-pin_load" => is_wire = false,
            other if val.is_none() && is_num(other) => val = Some(parse_f64(other)? * cu),
            other if !other.starts_with('-') => objs.push(other.to_string()),
            _ => {}
        }
    }
    let v = val.ok_or_else(|| "C0503: set_load missing value".to_string())?;
    let has_nets = objs.iter().any(|w| w.contains("get_nets"));
    if has_nets {
        for ni in resolve_net_indices(design, &objs) {
            let nname = design.nets[ni].name.clone();
            let mat = sdc.net_wire_loads.entry(nname).or_insert([[None; 2]; 2]);
            for (tr_do, tr) in [(rise || !fall, Trans::Rise), (fall || !rise, Trans::Fall)] {
                if tr_do {
                    if min || !max {
                        mat[tr.idx()][Analysis::Early.idx()] = Some(v);
                    }
                    if max || !min {
                        mat[tr.idx()][Analysis::Late.idx()] = Some(v);
                    }
                }
            }
        }
    } else {
        let clk_port_names: Vec<String> = sdc
            .clocks
            .values()
            .flat_map(|c| c.sources.iter().cloned())
            .collect();
        for pi in resolve_port_indices(design, &objs, &clk_port_names) {
            let pname = design.ports[pi].name.clone();
            let map = if is_wire {
                &mut sdc.port_wire_loads
            } else {
                &mut sdc.port_pin_loads
            };
            let mat = map.entry(pname).or_insert([[0.0; 2]; 2]);
            apply_tr_an_f64(mat, rise, fall, min, max, v);
        }
    }
    Ok(())
}
