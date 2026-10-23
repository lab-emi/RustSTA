// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing graph (STA-REQ-0100, STA-REQ-0102, STA-REQ-0401).

use crate::model::{ArcKind, Model};
use crate::netlist::{CONST1, Flat, NetId};

/// What a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    PortIn(u32),
    PortOut(u32),
    Pin { inst: u32, pin: u32 },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: NodeKind,
    pub net: NetId,
}

/// A propagation arc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcRef {
    Net,
    Cell { inst: u32, arc: u32 },
}

#[derive(Debug, Clone, Copy)]
pub struct Arc {
    pub from: u32,
    pub to: u32,
    pub kind: ArcRef,
}

/// An edge (launch) arc of an instance.
#[derive(Debug, Clone, Copy)]
pub struct Launch {
    pub inst: u32,
    pub arc: u32,
    pub clock_pin: u32,
    pub out: u32,
}

/// A timing check of an instance.
#[derive(Debug, Clone, Copy)]
pub struct Check {
    pub inst: u32,
    pub check: u32,
    pub data: u32,
    pub clock: u32,
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// Propagation arcs.
    pub arcs: Vec<Arc>,
    pub disabled: Vec<bool>,
    pub launches: Vec<Launch>,
    pub checks: Vec<Check>,
    pub inst_cell: Vec<u32>,
    /// Driver node of each net, if any.
    pub net_driver: Vec<Option<u32>>,
    /// Load nodes of each net.
    pub net_loads: Vec<Vec<u32>>,
    /// Pin capacitance of each node, `[tr][an]` (0 for ports and outputs).
    pub node_cap: Vec<[[f64; 2]; 2]>,
    /// Nodes in level order and the start of each level in it.
    pub order: Vec<u32>,
    pub level_start: Vec<u32>,
    /// Incoming enabled arcs per node, CSR indexed by node id.
    pub in_off: Vec<u32>,
    pub in_arcs: Vec<u32>,
    pub notes: Vec<String>,
}

impl Graph {
    pub fn incoming(&self, v: u32) -> &[u32] {
        &self.in_arcs[self.in_off[v as usize] as usize..self.in_off[v as usize + 1] as usize]
    }

    pub fn build(flat: &Flat, model: &Model) -> Result<Graph, String> {
        let mut nodes = Vec::new();
        for (i, p) in flat.ports.iter().enumerate() {
            nodes.push(Node {
                name: p.name.clone(),
                kind: if p.input {
                    NodeKind::PortIn(i as u32)
                } else {
                    NodeKind::PortOut(i as u32)
                },
                net: p.net,
            });
        }
        let mut inst_cell = Vec::with_capacity(flat.insts.len());
        // Node of every (instance, model pin).
        let mut pin_node: Vec<Vec<u32>> = Vec::with_capacity(flat.insts.len());
        let mut node_cap = vec![[[0.0; 2]; 2]; nodes.len()];
        for (ii, inst) in flat.insts.iter().enumerate() {
            let ci = *model
                .by_name
                .get(&inst.cell)
                .ok_or_else(|| format!("instance {}: unknown cell '{}'", inst.name, inst.cell))?;
            let cell = &model.cells[ci];
            inst_cell.push(ci as u32);
            let mut pn = vec![u32::MAX; cell.pins.len()];
            for (pin, net) in &inst.conns {
                let p = cell.pin(pin).ok_or_else(|| {
                    format!(
                        "instance {}: cell {} has no pin '{pin}'",
                        inst.name, inst.cell
                    )
                })?;
                pn[p] = nodes.len() as u32;
                node_cap.push(if cell.pins[p].input {
                    cell.pins[p].cap
                } else {
                    [[0.0; 2]; 2]
                });
                nodes.push(Node {
                    name: format!("{}/{}", inst.name, pin),
                    kind: NodeKind::Pin {
                        inst: ii as u32,
                        pin: p as u32,
                    },
                    net: *net,
                });
            }
            pin_node.push(pn);
        }
        let mut notes = Vec::new();
        let mut net_driver: Vec<Option<u32>> = vec![None; flat.num_nets];
        let mut net_loads: Vec<Vec<u32>> = vec![Vec::new(); flat.num_nets];
        for (v, n) in nodes.iter().enumerate() {
            let drives = match n.kind {
                NodeKind::PortIn(_) => true,
                NodeKind::PortOut(_) => false,
                NodeKind::Pin { inst, pin } => {
                    let c = &model.cells[inst_cell[inst as usize] as usize];
                    c.pins[pin as usize].output && !c.pins[pin as usize].input
                }
            };
            if n.net <= CONST1 {
                if drives {
                    return Err(format!("{} drives a constant net", n.name));
                }
                continue;
            }
            if drives {
                if let Some(d) = net_driver[n.net as usize] {
                    return Err(format!(
                        "net {} has two drivers: {} and {} (STA-REQ-0002)",
                        flat.net_names[n.net as usize], nodes[d as usize].name, n.name
                    ));
                }
                net_driver[n.net as usize] = Some(v as u32);
            } else {
                net_loads[n.net as usize].push(v as u32);
            }
        }
        let mut arcs = Vec::new();
        for (net, loads) in net_loads.iter().enumerate() {
            match net_driver[net] {
                Some(d) => {
                    for &l in loads {
                        arcs.push(Arc {
                            from: d,
                            to: l,
                            kind: ArcRef::Net,
                        });
                    }
                }
                None if !loads.is_empty() => notes.push(format!(
                    "net {} has no driver; its loads are unconstrained (STA-REQ-0002)",
                    flat.net_names[net]
                )),
                None => {}
            }
        }
        let mut launches = Vec::new();
        let mut checks = Vec::new();
        for (ii, pn) in pin_node.iter().enumerate() {
            let cell = &model.cells[inst_cell[ii] as usize];
            for (ai, a) in cell.arcs.iter().enumerate() {
                let (f, t) = (pn[a.from], pn[a.to]);
                if f == u32::MAX || t == u32::MAX {
                    continue;
                }
                match a.kind {
                    ArcKind::Comb { .. } => arcs.push(Arc {
                        from: f,
                        to: t,
                        kind: ArcRef::Cell {
                            inst: ii as u32,
                            arc: ai as u32,
                        },
                    }),
                    ArcKind::Edge(_) => launches.push(Launch {
                        inst: ii as u32,
                        arc: ai as u32,
                        clock_pin: f,
                        out: t,
                    }),
                }
            }
            for (ci, c) in cell.checks.iter().enumerate() {
                let (d, k) = (pn[c.data], pn[c.clock]);
                if d != u32::MAX && k != u32::MAX {
                    checks.push(Check {
                        inst: ii as u32,
                        check: ci as u32,
                        data: d,
                        clock: k,
                    });
                }
            }
        }
        let mut g = Graph {
            nodes,
            disabled: vec![false; arcs.len()],
            arcs,
            launches,
            checks,
            inst_cell,
            net_driver,
            net_loads,
            node_cap,
            order: Vec::new(),
            level_start: Vec::new(),
            in_off: Vec::new(),
            in_arcs: Vec::new(),
            notes,
        };
        g.break_cycles();
        g.levelize();
        Ok(g)
    }

    /// Depth-first search from the nodes in name order; arcs closing a cycle are disabled
    /// (STA-REQ-0102).
    fn break_cycles(&mut self) {
        let n = self.nodes.len();
        let mut out_off = vec![0u32; n + 1];
        for a in &self.arcs {
            out_off[a.from as usize + 1] += 1;
        }
        for i in 0..n {
            out_off[i + 1] += out_off[i];
        }
        let mut fill = out_off.clone();
        let mut out = vec![0u32; self.arcs.len()];
        for (ai, a) in self.arcs.iter().enumerate() {
            out[fill[a.from as usize] as usize] = ai as u32;
            fill[a.from as usize] += 1;
        }
        // Outgoing arcs in order of the name of their target (then arc index).
        for v in 0..n {
            let (lo, hi) = (out_off[v] as usize, out_off[v + 1] as usize);
            out[lo..hi].sort_by(|x, y| {
                let (a, b) = (&self.arcs[*x as usize], &self.arcs[*y as usize]);
                self.nodes[a.to as usize]
                    .name
                    .cmp(&self.nodes[b.to as usize].name)
                    .then(x.cmp(y))
            });
        }
        let mut start: Vec<u32> = (0..n as u32).collect();
        start.sort_by(|a, b| {
            self.nodes[*a as usize]
                .name
                .cmp(&self.nodes[*b as usize].name)
        });
        let mut color = vec![0u8; n];
        for s in start {
            if color[s as usize] != 0 {
                continue;
            }
            let mut stack: Vec<(u32, u32)> = vec![(s, out_off[s as usize])];
            color[s as usize] = 1;
            while let Some(&mut (v, ref mut next)) = stack.last_mut() {
                if *next < out_off[v as usize + 1] {
                    let ai = out[*next as usize];
                    *next += 1;
                    let w = self.arcs[ai as usize].to;
                    match color[w as usize] {
                        0 => {
                            color[w as usize] = 1;
                            stack.push((w, out_off[w as usize]));
                        }
                        1 => {
                            self.disabled[ai as usize] = true;
                            self.notes.push(format!(
                                "combinational cycle broken at arc {} -> {} (STA-REQ-0102)",
                                self.nodes[v as usize].name, self.nodes[w as usize].name
                            ));
                        }
                        _ => {}
                    }
                } else {
                    color[v as usize] = 2;
                    stack.pop();
                }
            }
        }
    }

    /// Longest-path levels, level order, and incoming CSR over enabled arcs.
    fn levelize(&mut self) {
        let n = self.nodes.len();
        let mut indeg = vec![0u32; n];
        let mut out: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (ai, a) in self.arcs.iter().enumerate() {
            if self.disabled[ai] {
                continue;
            }
            indeg[a.to as usize] += 1;
            out[a.from as usize].push(ai as u32);
        }
        let mut level = vec![0u32; n];
        let mut queue: Vec<u32> = (0..n as u32).filter(|v| indeg[*v as usize] == 0).collect();
        let mut head = 0;
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            for &ai in &out[v as usize] {
                let w = self.arcs[ai as usize].to;
                level[w as usize] = level[w as usize].max(level[v as usize] + 1);
                indeg[w as usize] -= 1;
                if indeg[w as usize] == 0 {
                    queue.push(w);
                }
            }
        }
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_by_key(|v| (level[*v as usize], *v));
        let max_level = order.last().map_or(0, |v| level[*v as usize]);
        let mut level_start = Vec::with_capacity(max_level as usize + 2);
        let mut cur = u32::MAX;
        for (i, v) in order.iter().enumerate() {
            if level[*v as usize] != cur {
                cur = level[*v as usize];
                level_start.push(i as u32);
            }
        }
        level_start.push(n as u32);
        let mut in_off = vec![0u32; n + 1];
        for (ai, a) in self.arcs.iter().enumerate() {
            if !self.disabled[ai] {
                in_off[a.to as usize + 1] += 1;
            }
        }
        for i in 0..n {
            in_off[i + 1] += in_off[i];
        }
        let mut fill = in_off.clone();
        let mut in_arcs = vec![0u32; in_off[n] as usize];
        for (ai, a) in self.arcs.iter().enumerate() {
            if !self.disabled[ai] {
                in_arcs[fill[a.to as usize] as usize] = ai as u32;
                fill[a.to as usize] += 1;
            }
        }
        self.order = order;
        self.level_start = level_start;
        self.in_off = in_off;
        self.in_arcs = in_arcs;
    }
}
