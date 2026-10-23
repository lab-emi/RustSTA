// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing constraints from SDC commands (STA-REQ-0020, STA-REQ-0021).

use rustc_hash::FxHashMap;

use crate::graph::{Graph, NodeKind};
use crate::old_sdc::{Command, Query, Word, glob_match};

/// A clock (times in ns).
#[derive(Debug, Clone)]
pub struct Clock {
    pub name: String,
    pub period: f64,
    /// Rise and fall edge times within the first period.
    pub edges: [f64; 2],
    pub sources: Vec<u32>,
    pub latency: [f64; 2],
    pub transition: f64,
    pub uncertainty_setup: f64,
    pub uncertainty_hold: f64,
}

/// Input or output delay: (early/min, late/max) values.
#[derive(Debug, Clone, Copy)]
pub struct PortDelay {
    pub node: u32,
    pub clock: u32,
    /// Edge of the clock: 0 rise, 1 fall.
    pub edge: usize,
    pub value: [f64; 2],
}

#[derive(Debug, Clone, Default)]
pub struct Constraints {
    pub clocks: Vec<Clock>,
    pub input_delays: Vec<PortDelay>,
    pub output_delays: Vec<PortDelay>,
    /// (min, max) load per port node.
    pub port_load: FxHashMap<u32, [f64; 2]>,
    /// (min, max) input transition per port node.
    pub input_transition: FxHashMap<u32, [f64; 2]>,
}

fn at(c: &Command) -> String {
    format!("SDC line {}", c.line)
}

impl Constraints {
    /// Reads constraints; `time` and `cap` convert SDC values (library units) to ns and pF.
    pub fn from_sdc(
        cmds: &[Command],
        g: &Graph,
        time: f64,
        cap: f64,
    ) -> Result<Constraints, String> {
        let mut k = Constraints::default();
        let ports_by_name: FxHashMap<&str, u32> = g
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| matches!(n.kind, NodeKind::PortIn(_) | NodeKind::PortOut(_)))
            .map(|(i, n)| (n.name.as_str(), i as u32))
            .collect();
        let port_names: Vec<(&str, u32)> = {
            let mut v: Vec<(&str, u32)> = ports_by_name.iter().map(|(a, b)| (*a, *b)).collect();
            v.sort();
            v
        };
        let num = |c: &Command, w: Option<&Word>, what: &str| -> Result<f64, String> {
            w.and_then(Word::number)
                .ok_or_else(|| format!("{}: {} expects a number for {what}", at(c), c.name))
        };
        for c in cmds {
            // Resolves port objects of a word (query or plain names).
            let ports_of = |k: &Constraints, w: &Word| -> Result<Vec<u32>, String> {
                let pats: Vec<String> = match w {
                    Word::Query(q) => match q.name.as_str() {
                        "get_ports" => q.patterns.clone(),
                        "all_inputs" => {
                            return Ok(port_names
                                .iter()
                                .filter(|(_, i)| {
                                    matches!(g.nodes[*i as usize].kind, NodeKind::PortIn(_))
                                })
                                .map(|x| x.1)
                                .filter(|i| !k.clocks.iter().any(|c| c.sources.contains(i)))
                                .collect());
                        }
                        "all_outputs" => {
                            return Ok(port_names
                                .iter()
                                .filter(|(_, i)| {
                                    matches!(g.nodes[*i as usize].kind, NodeKind::PortOut(_))
                                })
                                .map(|x| x.1)
                                .collect());
                        }
                        other => {
                            return Err(format!(
                                "{}: object query '{other}' is not supported here",
                                at(c)
                            ));
                        }
                    },
                    other => other.items(),
                };
                let mut out = Vec::new();
                for p in pats {
                    let mut hit = false;
                    for (name, i) in &port_names {
                        let base = name.split('[').next().unwrap_or(name);
                        if glob_match(&p, name) || glob_match(&p, base) {
                            out.push(*i);
                            hit = true;
                        }
                    }
                    if !hit {
                        return Err(format!("{}: no port matches '{p}'", at(c)));
                    }
                }
                out.sort_unstable();
                out.dedup();
                Ok(out)
            };
            let clocks_of = |k: &Constraints, w: &Word| -> Result<Vec<u32>, String> {
                let pats = match w {
                    Word::Query(Query { name, patterns, .. }) if name == "get_clocks" => {
                        patterns.clone()
                    }
                    Word::Query(Query { name, .. }) if name == "all_clocks" => {
                        return Ok((0..k.clocks.len() as u32).collect());
                    }
                    Word::Query(q) => {
                        return Err(format!("{}: '{}' does not name clocks", at(c), q.name));
                    }
                    other => other.items(),
                };
                let mut out = Vec::new();
                for p in pats {
                    let before = out.len();
                    for (i, ck) in k.clocks.iter().enumerate() {
                        if glob_match(&p, &ck.name) {
                            out.push(i as u32);
                        }
                    }
                    if out.len() == before {
                        return Err(format!("{}: no clock matches '{p}'", at(c)));
                    }
                }
                Ok(out)
            };
            let minmax = |c: &Command| -> [bool; 2] {
                match (c.flag("min"), c.flag("max")) {
                    (true, false) => [true, false],
                    (false, true) => [false, true],
                    _ => [true, true],
                }
            };
            match c.name.as_str() {
                "create_clock" => {
                    let period = num(c, c.value("period"), "-period")? * time;
                    if period <= 0.0 {
                        return Err(format!("{}: clock period must be positive", at(c)));
                    }
                    let edges = match c.value("waveform") {
                        Some(w) => {
                            let v: Vec<f64> = w
                                .items()
                                .iter()
                                .map(|x| x.parse::<f64>().map(|t| t * time))
                                .collect::<Result<_, _>>()
                                .map_err(|_| format!("{}: bad -waveform", at(c)))?;
                            if v.len() != 2
                                || !(0.0 <= v[0] && v[0] < v[1] && v[1] <= v[0] + period)
                            {
                                return Err(format!(
                                    "{}: -waveform needs two edges within one period",
                                    at(c)
                                ));
                            }
                            [v[0], v[1]]
                        }
                        None => [0.0, period / 2.0],
                    };
                    let sources = match c.args.first() {
                        Some(w) => ports_of(&k, w)?,
                        None => {
                            return Err(format!("{}: virtual clocks are not supported", at(c)));
                        }
                    };
                    let name = match c.value("name") {
                        Some(w) => w.items().join(" "),
                        None => sources
                            .first()
                            .map(|s| g.nodes[*s as usize].name.clone())
                            .unwrap_or_default(),
                    };
                    k.clocks.retain(|x| x.name != name);
                    k.clocks.push(Clock {
                        name,
                        period,
                        edges,
                        sources,
                        latency: [0.0, 0.0],
                        transition: 0.0,
                        uncertainty_setup: 0.0,
                        uncertainty_hold: 0.0,
                    });
                }
                "set_input_delay" | "set_output_delay" => {
                    let input = c.name == "set_input_delay";
                    let clock = match c.value("clock") {
                        Some(w) => {
                            let cs = clocks_of(&k, w)?;
                            if cs.len() != 1 {
                                return Err(format!("{}: -clock must name one clock", at(c)));
                            }
                            cs[0]
                        }
                        None => {
                            return Err(format!(
                                "{}: delays without -clock are not supported",
                                at(c)
                            ));
                        }
                    };
                    let value = num(c, c.args.first(), "the delay")? * time;
                    let objs = c
                        .args
                        .get(1)
                        .ok_or_else(|| format!("{}: {} needs ports", at(c), c.name))?;
                    let edge = usize::from(c.flag("clock_fall"));
                    let mm = minmax(c);
                    for node in ports_of(&k, objs)? {
                        let list = if input {
                            &mut k.input_delays
                        } else {
                            &mut k.output_delays
                        };
                        let e = match list
                            .iter_mut()
                            .find(|d| d.node == node && d.clock == clock && d.edge == edge)
                        {
                            Some(e) => e,
                            None => {
                                if !c.flag("add_delay") {
                                    list.retain(|d| d.node != node);
                                }
                                list.push(PortDelay {
                                    node,
                                    clock,
                                    edge,
                                    value: [value, value],
                                });
                                list.last_mut().expect("pushed")
                            }
                        };
                        for (i, on) in mm.iter().enumerate() {
                            if *on {
                                e.value[i] = value;
                            }
                        }
                    }
                }
                "set_load" | "set_input_transition" => {
                    let scale = if c.name == "set_load" { cap } else { time };
                    let value = num(c, c.args.first(), "the value")? * scale;
                    let objs = c
                        .args
                        .get(1)
                        .ok_or_else(|| format!("{}: {} needs ports", at(c), c.name))?;
                    let mm = minmax(c);
                    for node in ports_of(&k, objs)? {
                        let map = if c.name == "set_load" {
                            &mut k.port_load
                        } else {
                            &mut k.input_transition
                        };
                        let e = map.entry(node).or_insert([value, value]);
                        for (i, on) in mm.iter().enumerate() {
                            if *on {
                                e[i] = value;
                            }
                        }
                    }
                }
                "set_clock_uncertainty" | "set_clock_latency" | "set_clock_transition" => {
                    if c.value("from").is_some() || c.value("to").is_some() || c.flag("source") {
                        return Err(format!(
                            "{}: {} with -from/-to/-source is not supported",
                            at(c),
                            c.name
                        ));
                    }
                    let value = num(c, c.args.first(), "the value")? * time;
                    let objs = c
                        .args
                        .get(1)
                        .ok_or_else(|| format!("{}: {} needs clocks", at(c), c.name))?;
                    let cs = clocks_of(&k, objs)?;
                    let mm = minmax(c);
                    for ci in cs {
                        let ck = &mut k.clocks[ci as usize];
                        match c.name.as_str() {
                            "set_clock_uncertainty" => {
                                let (s, h) = match (c.flag("setup"), c.flag("hold")) {
                                    (true, false) => (true, false),
                                    (false, true) => (false, true),
                                    _ => (true, true),
                                };
                                if s {
                                    ck.uncertainty_setup = value;
                                }
                                if h {
                                    ck.uncertainty_hold = value;
                                }
                            }
                            "set_clock_latency" => {
                                for (i, on) in mm.iter().enumerate() {
                                    if *on {
                                        ck.latency[i] = value;
                                    }
                                }
                            }
                            _ => ck.transition = value,
                        }
                    }
                }
                "set_units" => {}
                other => {
                    return Err(format!(
                        "{}: '{other}' is not supported (STA-REQ-0020)",
                        at(c)
                    ));
                }
            }
        }
        Ok(k)
    }
}
