// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Constraints for the checker (STA-REQ-0020, STA-REQ-0021).

use std::collections::HashMap;

use crate::old_sdc::{Command, Word, glob_match, parse};

#[derive(Debug, Clone)]
pub struct Clk {
    pub name: String,
    pub period: f64,
    pub rise: f64,
    pub fall: f64,
    pub sources: Vec<usize>,
    /// (min, max)
    pub latency: [f64; 2],
    pub transition: f64,
    pub setup_unc: f64,
    pub hold_unc: f64,
}

/// A port delay: pin, clock, edge (0 rise, 1 fall), (min, max) value.
#[derive(Debug, Clone, Copy)]
pub struct Delay {
    pub pin: usize,
    pub clock: usize,
    pub edge: usize,
    pub value: [f64; 2],
}

#[derive(Debug, Clone, Default)]
pub struct Sdc {
    pub clocks: Vec<Clk>,
    pub inputs: Vec<Delay>,
    pub outputs: Vec<Delay>,
    pub port_load: HashMap<usize, [f64; 2]>,
    pub input_tr: HashMap<usize, [f64; 2]>,
}

fn which(c: &Command) -> (bool, bool) {
    match (c.flag("min"), c.flag("max")) {
        (true, false) => (true, false),
        (false, true) => (false, true),
        _ => (true, true),
    }
}

impl Sdc {
    pub fn read(
        text: &str,
        names: &[String],
        port_in: &[bool],
        owner: &[Option<(usize, usize)>],
        tu: f64,
        cu: f64,
    ) -> Result<Sdc, String> {
        let cmds = parse(text).map_err(|e| e.to_string())?;
        let ports: Vec<usize> = (0..names.len()).filter(|i| owner[*i].is_none()).collect();
        let mut s = Sdc::default();
        for c in &cmds {
            let resolve_ports = |s: &Sdc, w: &Word| -> Result<Vec<usize>, String> {
                let (pats, filter): (Vec<String>, Option<bool>) = match w {
                    Word::Query(q) if q.name == "get_ports" => (q.patterns.clone(), None),
                    Word::Query(q) if q.name == "all_inputs" => (vec!["*".into()], Some(true)),
                    Word::Query(q) if q.name == "all_outputs" => (vec!["*".into()], Some(false)),
                    Word::Query(q) => return Err(format!("unsupported query {}", q.name)),
                    other => (other.items(), None),
                };
                let mut out = Vec::new();
                for pat in &pats {
                    let before = out.len();
                    for &p in &ports {
                        if let Some(inp) = filter {
                            if port_in[p] != inp {
                                continue;
                            }
                            if inp && s.clocks.iter().any(|k| k.sources.contains(&p)) {
                                continue;
                            }
                        }
                        let base = names[p].split('[').next().unwrap_or("");
                        if glob_match(pat, &names[p]) || glob_match(pat, base) {
                            out.push(p);
                        }
                    }
                    if out.len() == before && filter.is_none() {
                        return Err(format!("line {}: no port {pat}", c.line));
                    }
                }
                out.sort_unstable();
                out.dedup();
                Ok(out)
            };
            let resolve_clocks = |s: &Sdc, w: &Word| -> Result<Vec<usize>, String> {
                let pats = match w {
                    Word::Query(q) if q.name == "get_clocks" => q.patterns.clone(),
                    Word::Query(q) if q.name == "all_clocks" => vec!["*".into()],
                    Word::Query(q) => return Err(format!("unsupported query {}", q.name)),
                    other => other.items(),
                };
                let v: Vec<usize> = (0..s.clocks.len())
                    .filter(|i| pats.iter().any(|p| glob_match(p, &s.clocks[*i].name)))
                    .collect();
                if v.is_empty() {
                    return Err(format!("line {}: no clock", c.line));
                }
                Ok(v)
            };
            let arg_num = |k: usize| -> Result<f64, String> {
                c.args
                    .get(k)
                    .and_then(Word::number)
                    .ok_or_else(|| format!("line {}: number expected", c.line))
            };
            match c.name.as_str() {
                "create_clock" => {
                    let period = c
                        .value("period")
                        .and_then(Word::number)
                        .ok_or("create_clock needs -period")?
                        * tu;
                    let (rise, fall) = match c.value("waveform") {
                        Some(w) => {
                            let v: Vec<f64> = w
                                .items()
                                .iter()
                                .filter_map(|x| x.parse::<f64>().ok())
                                .collect();
                            if v.len() != 2 {
                                return Err("bad -waveform".into());
                            }
                            (v[0] * tu, v[1] * tu)
                        }
                        None => (0.0, period / 2.0),
                    };
                    let sources = resolve_ports(&s, c.args.first().ok_or("virtual clock")?)?;
                    let name = c
                        .value("name")
                        .map(|w| w.items().join(" "))
                        .unwrap_or_else(|| names[sources[0]].clone());
                    s.clocks.retain(|k| k.name != name);
                    s.clocks.push(Clk {
                        name,
                        period,
                        rise,
                        fall,
                        sources,
                        latency: [0.0, 0.0],
                        transition: 0.0,
                        setup_unc: 0.0,
                        hold_unc: 0.0,
                    });
                }
                "set_input_delay" | "set_output_delay" => {
                    let clk = resolve_clocks(&s, c.value("clock").ok_or("-clock needed")?)?[0];
                    let v = arg_num(0)? * tu;
                    let pins = resolve_ports(&s, c.args.get(1).ok_or("ports needed")?)?;
                    let edge = usize::from(c.flag("clock_fall"));
                    let (mn, mx) = which(c);
                    let add = c.flag("add_delay");
                    let list = if c.name == "set_input_delay" {
                        &mut s.inputs
                    } else {
                        &mut s.outputs
                    };
                    for p in pins {
                        let pos = list
                            .iter()
                            .position(|d| d.pin == p && d.clock == clk && d.edge == edge);
                        let i = match pos {
                            Some(i) => i,
                            None => {
                                if !add {
                                    list.retain(|d| d.pin != p);
                                }
                                list.push(Delay {
                                    pin: p,
                                    clock: clk,
                                    edge,
                                    value: [v, v],
                                });
                                list.len() - 1
                            }
                        };
                        if mn {
                            list[i].value[0] = v;
                        }
                        if mx {
                            list[i].value[1] = v;
                        }
                    }
                }
                "set_load" | "set_input_transition" => {
                    let is_load = c.name == "set_load";
                    let v = arg_num(0)? * if is_load { cu } else { tu };
                    let pins = resolve_ports(&s, c.args.get(1).ok_or("ports needed")?)?;
                    let (mn, mx) = which(c);
                    for p in pins {
                        let m = if is_load {
                            &mut s.port_load
                        } else {
                            &mut s.input_tr
                        };
                        let e = m.entry(p).or_insert([v, v]);
                        if mn {
                            e[0] = v;
                        }
                        if mx {
                            e[1] = v;
                        }
                    }
                }
                "set_clock_uncertainty" | "set_clock_latency" | "set_clock_transition" => {
                    let v = arg_num(0)? * tu;
                    let cs = resolve_clocks(&s, c.args.get(1).ok_or("clocks needed")?)?;
                    let (mn, mx) = which(c);
                    for k in cs {
                        let ck = &mut s.clocks[k];
                        match c.name.as_str() {
                            "set_clock_uncertainty" => {
                                let (st, ho) = match (c.flag("setup"), c.flag("hold")) {
                                    (true, false) => (true, false),
                                    (false, true) => (false, true),
                                    _ => (true, true),
                                };
                                if st {
                                    ck.setup_unc = v;
                                }
                                if ho {
                                    ck.hold_unc = v;
                                }
                            }
                            "set_clock_latency" => {
                                if mn {
                                    ck.latency[0] = v;
                                }
                                if mx {
                                    ck.latency[1] = v;
                                }
                            }
                            _ => ck.transition = v,
                        }
                    }
                }
                "set_units" => {}
                other => return Err(format!("line {}: unsupported command {other}", c.line)),
            }
        }
        Ok(s)
    }
}
