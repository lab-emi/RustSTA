// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The report of an analysis and its queries: endpoint slacks, worst paths on request and nodes
//! by name (STA-REQ-0302, STA-REQ-0312 to STA-REQ-0315).

use std::time::Instant;

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use rusteda_core::job::{Ctx, Event};

use crate::view::{EndpointTable, Kind, NameIndex};
use crate::{Analysis, Report, StaError, checks, input_err, prop, report};

/// What the report contains besides the summaries and endpoints.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// Worst paths per check kind.
    pub paths: usize,
    /// Endpoints whose worst paths are reported in addition.
    pub path_to: Vec<String>,
}

impl Analysis {
    /// The report of the analysis (STA-REQ-0312); the same as `analyze` for the same options.
    pub fn report(&self, ro: &ReportOptions) -> Result<Report, StaError> {
        self.report_with(ro, Ctx::NONE)
    }

    /// `report` with a run context: the phase `report` and progress per reported path.
    pub fn report_with(&self, ro: &ReportOptions, ctx: Ctx<'_>) -> Result<Report, StaError> {
        ctx.emit(|| Event::PhaseStarted { phase: "report" });
        let t_report = Instant::now();
        let setup = summary(self, &self.endpoints.setup);
        let hold = summary(self, &self.endpoints.hold);
        let mut eps: Vec<u32> = self
            .endpoints
            .setup
            .keys()
            .chain(self.endpoints.hold.keys())
            .copied()
            .collect();
        eps.sort_unstable();
        eps.dedup();
        let endpoints = eps
            .iter()
            .map(|v| report::EndpointReport {
                pin: self.graph.nodes[*v as usize].name.clone(),
                setup_slack: self.endpoints.setup.get(v).map(|s| s.slack),
                hold_slack: self.endpoints.hold.get(v).map(|s| s.slack),
            })
            .collect();
        let mut paths = Vec::new();
        for (map, an) in [
            (&self.endpoints.setup, prop::LATE),
            (&self.endpoints.hold, prop::EARLY),
        ] {
            let mut worst: Vec<(&u32, &checks::Slack)> = map.iter().collect();
            worst.sort_by(|x, y| x.1.slack.total_cmp(&y.1.slack).then(x.0.cmp(y.0)));
            for (v, sl) in worst.into_iter().take(ro.paths) {
                paths.push(checks::path(
                    &self.graph,
                    &self.model,
                    &self.constraints,
                    &self.setup,
                    &self.values,
                    *v,
                    sl,
                    an,
                ));
            }
        }
        for name in &ro.path_to {
            let v = self
                .graph
                .nodes
                .iter()
                .position(|n| n.name == *name)
                .ok_or_else(|| input_err(format!("--path-to: no pin named '{name}'")))?
                as u32;
            for (map, an) in [
                (&self.endpoints.setup, prop::LATE),
                (&self.endpoints.hold, prop::EARLY),
            ] {
                if let Some(sl) = map.get(&v) {
                    paths.push(checks::path(
                        &self.graph,
                        &self.model,
                        &self.constraints,
                        &self.setup,
                        &self.values,
                        v,
                        sl,
                        an,
                    ));
                }
            }
        }
        let notes = self.notes();
        let report_s = t_report.elapsed().as_secs_f64();
        ctx.emit(|| Event::PhaseFinished {
            phase: "report",
            seconds: report_s,
        });
        Ok(Report {
            format: crate::REPORT_FORMAT.into(),
            format_version: 1,
            top: self.flat.top.clone(),
            backend: self.backend.into(),
            clocks: self
                .constraints
                .clocks
                .iter()
                .map(|c| report::ClockReport {
                    name: c.name.clone(),
                    period: c.period,
                    waveform: c.edges,
                })
                .collect(),
            setup,
            hold,
            unconstrained_endpoints: self.endpoints.unconstrained,
            endpoints,
            paths,
            counts: report::Counts {
                nodes: self.graph.nodes.len(),
                arcs: self.graph.arcs.len(),
                cells: self.flat.insts.len(),
                levels: self.graph.level_start.len().saturating_sub(1),
                startpoints: self.setup.is_start.iter().filter(|x| **x).count(),
                disabled_arcs: self.graph.disabled.iter().filter(|x| **x).count(),
            },
            notes,
            runtime_s: self.run_s + report_s,
            phase_s: self
                .phase_s
                .iter()
                .map(|(n, s)| (n.to_string(), *s))
                .chain([("report".to_string(), report_s)])
                .collect(),
        })
    }

    /// Graph notes, then cell notes: the `notes` of the report.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = self.graph.notes.clone();
        for c in &self.model.cells {
            for n in &c.notes {
                notes.push(format!("cell {}: {n}", c.name));
            }
        }
        notes
    }

    /// Summary of one check kind.
    pub fn summary(&self, kind: Kind) -> report::Summary {
        summary(self, self.slacks(kind))
    }

    fn slacks(&self, kind: Kind) -> &FxHashMap<u32, checks::Slack> {
        match kind {
            Kind::Setup => &self.endpoints.setup,
            Kind::Hold => &self.endpoints.hold,
        }
    }

    pub fn node_count(&self) -> usize {
        self.graph.nodes.len()
    }

    pub fn node_name(&self, v: u32) -> &str {
        &self.graph.nodes[v as usize].name
    }

    pub fn clock_name(&self, c: u32) -> &str {
        self.constraints
            .clocks
            .get(c as usize)
            .map_or("", |k| k.name.as_str())
    }

    /// Worst slack of endpoint `v` for `kind`.
    pub fn endpoint_slack(&self, v: u32, kind: Kind) -> Option<f64> {
        self.slacks(kind).get(&v).map(|s| s.slack)
    }

    /// Every endpoint in ascending node order with its worst slacks (STA-REQ-0313).
    pub fn endpoint_table(&self) -> EndpointTable {
        let mut node: Vec<u32> = self
            .endpoints
            .setup
            .keys()
            .chain(self.endpoints.hold.keys())
            .copied()
            .collect();
        node.par_sort_unstable();
        node.dedup();
        node.shrink_to_fit();
        fn get(m: &FxHashMap<u32, checks::Slack>, v: u32) -> Option<&checks::Slack> {
            m.get(&v)
        }
        let setup = node
            .par_iter()
            .map(|&v| get(&self.endpoints.setup, v).map_or(f64::NAN, |s| s.slack))
            .collect();
        let hold = node
            .par_iter()
            .map(|&v| get(&self.endpoints.hold, v).map_or(f64::NAN, |s| s.slack))
            .collect();
        let capture = node
            .par_iter()
            .map(|&v| {
                get(&self.endpoints.setup, v)
                    .or_else(|| get(&self.endpoints.hold, v))
                    .map_or(u16::MAX, |s| u16::try_from(s.capture).unwrap_or(u16::MAX))
            })
            .collect();
        EndpointTable {
            node,
            setup,
            hold,
            capture,
        }
    }

    /// Nodes ordered by name, for lookups by name and prefix (STA-REQ-0315).
    pub fn name_index(&self) -> NameIndex {
        let mut order: Vec<u32> = (0..self.graph.nodes.len() as u32).collect();
        order.par_sort_unstable_by(|&a, &b| {
            self.node_name(a).cmp(self.node_name(b)).then(a.cmp(&b))
        });
        NameIndex { order }
    }

    /// The worst path of endpoint `v` for `kind`, or `None` when `v` is no endpoint of that
    /// kind (STA-REQ-0314).
    pub fn worst_path(&self, v: u32, kind: Kind) -> Option<report::PathReport> {
        let an = match kind {
            Kind::Setup => prop::LATE,
            Kind::Hold => prop::EARLY,
        };
        self.slacks(kind).get(&v).map(|sl| {
            checks::path(
                &self.graph,
                &self.model,
                &self.constraints,
                &self.setup,
                &self.values,
                v,
                sl,
                an,
            )
        })
    }
}

fn summary(a: &Analysis, map: &FxHashMap<u32, checks::Slack>) -> report::Summary {
    let mut s = report::Summary {
        endpoints: map.len(),
        ..report::Summary::default()
    };
    let mut groups: Vec<report::GroupSummary> = a
        .constraints
        .clocks
        .iter()
        .map(|c| report::GroupSummary {
            clock: c.name.clone(),
            ..report::GroupSummary::default()
        })
        .collect();
    for sl in map.values() {
        let g = &mut groups[sl.capture as usize];
        g.endpoints += 1;
        if sl.slack < 0.0 {
            s.violating += 1;
            s.tns += sl.slack;
            s.wns = s.wns.min(sl.slack);
            g.violating += 1;
            g.tns += sl.slack;
            g.wns = g.wns.min(sl.slack);
        }
    }
    s.per_clock = groups;
    s
}
