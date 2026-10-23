// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! What front ends use of the analysis (STA-REQ-0311 to STA-REQ-0315, JOB-REQ-0005,
//! JOB-REQ-0006), on corpus netlists.

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusteda_core::job::{CancelToken, Ctx, Event, JobError, Observer};
use ruststa::view::{Kind, PHASES, ReportOptions};
use ruststa::{Backend, Source, StaOptions, analyze, run, run_with};
use ruststa_fixtures::liberty::Variant;
use ruststa_fixtures::netlists::corpus;

/// Netlists of clocked corpus designs with a tight clock.
fn cases() -> Vec<(String, StaOptions)> {
    ["counter", "fifo", "uart_tx", "rv_core"]
        .iter()
        .map(|name| {
            let d = corpus(name).expect("design");
            let sdc = "create_clock -name clk -period 3 [get_ports clk_i]\n\
                       set_output_delay 0.5 -clock clk [all_outputs]\n"
                .to_string();
            (
                name.to_string(),
                StaOptions {
                    netlist: Source::Texts(vec![("n.v".into(), d.text.into())]),
                    top: None,
                    sdc,
                    paths: 4,
                    path_to: Vec::new(),
                    backend: Backend::Cpu,
                    sdc_name: String::new(),
                },
            )
        })
        .collect()
}

fn timeless(r: &ruststa::Report) -> serde_json::Value {
    let mut v = serde_json::to_value(r).expect("json");
    for k in ["runtime_s", "phase_s"] {
        v.as_object_mut().expect("object").remove(k);
    }
    v
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl Observer for Recorder {
    fn event(&self, e: &Event) {
        let e = match e {
            Event::PhaseFinished { phase, .. } => Event::PhaseFinished {
                phase,
                seconds: 0.0,
            },
            e => e.clone(),
        };
        self.0.lock().expect("lock").push(e);
    }
}

fn report_options(o: &StaOptions) -> ReportOptions {
    ReportOptions {
        paths: o.paths,
        path_to: o.path_to.clone(),
    }
}

// STA-REQ-0311, JOB-REQ-0005: observing a run or passing a token that is never set changes no
// value; the events are the same every time; STA-REQ-0312: the analysis's report is the report
// of `analyze`.
#[test]
fn observation_changes_nothing() {
    let lib = common::lib(Variant::Full);
    for (name, o) in cases() {
        let libs = std::slice::from_ref(&lib);
        let plain = analyze(&o, libs).expect("analysis");
        let mut seqs = Vec::new();
        for _ in 0..2 {
            let rec = Recorder::default();
            let token = CancelToken::new();
            let a = run_with(&o, libs, Ctx::new(Some(&token), Some(&rec)))
                .unwrap_or_else(|_| panic!("{name}: observed run failed"));
            let r = a
                .report_with(&report_options(&o), Ctx::new(Some(&token), Some(&rec)))
                .expect("report");
            assert_eq!(timeless(&r), timeless(&plain), "{name}");
            seqs.push(rec.0.into_inner().expect("lock"));
        }
        assert_eq!(seqs[0], seqs[1], "{name}: events differ between runs");
        let started: Vec<&str> = seqs[0]
            .iter()
            .filter_map(|e| match e {
                Event::PhaseStarted { phase } => Some(*phase),
                _ => None,
            })
            .collect();
        assert_eq!(started, PHASES.to_vec(), "{name}");
        let a = run(&o, libs).expect("analysis");
        assert_eq!(
            timeless(&a.report(&report_options(&o)).expect("report")),
            timeless(&plain),
            "{name}"
        );
    }
}

struct CancelAt<'a> {
    token: &'a CancelToken,
    k: usize,
    seen: AtomicUsize,
}

impl Observer for CancelAt<'_> {
    fn event(&self, _e: &Event) {
        if self.seen.fetch_add(1, Ordering::Relaxed) + 1 == self.k {
            self.token.cancel();
        }
    }
}

// JOB-REQ-0006: a timing run cancelled at any point returns `Cancelled`.
#[test]
fn cancel_at_every_point() {
    let lib = common::lib(Variant::Full);
    let (_, o) = cases().pop().expect("case");
    let libs = std::slice::from_ref(&lib);
    let rec = Recorder::default();
    run_with(&o, libs, Ctx::new(None, Some(&rec))).expect("analysis");
    let n = rec.0.into_inner().expect("lock").len();
    assert!(n > 5, "{n} events");
    for k in 1..n {
        let token = CancelToken::new();
        let obs = CancelAt {
            token: &token,
            k,
            seen: AtomicUsize::new(0),
        };
        let r = run_with(&o, libs, Ctx::new(Some(&token), Some(&obs)));
        assert!(matches!(r, Err(JobError::Cancelled)), "event {k} of {n}");
    }
}

// STA-REQ-0313, STA-REQ-0314, STA-REQ-0315: the endpoint table matches the report's endpoints
// in 22 bytes per endpoint; names are found exactly and by prefix; the worst path on request
// equals the reported path of the same endpoint.
#[test]
fn endpoint_table_names_and_paths() {
    let lib = common::lib(Variant::Full);
    for (name, o) in cases() {
        let libs = std::slice::from_ref(&lib);
        let a = run(&o, libs).expect("analysis");
        let rep = a.report(&report_options(&o)).expect("report");
        let t = a.endpoint_table();
        assert_eq!(t.len(), rep.endpoints.len(), "{name}");
        assert!(t.node.windows(2).all(|w| w[0] < w[1]), "{name}: node order");
        assert!(
            t.heap_bytes() <= 22 * t.len(),
            "{name}: {} bytes",
            t.heap_bytes()
        );
        let same = |x: f64, y: Option<f64>| match y {
            Some(y) => x.to_bits() == y.to_bits(),
            None => x.is_nan(),
        };
        for (i, e) in rep.endpoints.iter().enumerate() {
            assert_eq!(a.node_name(t.node[i]), e.pin, "{name}");
            assert!(same(t.setup[i], e.setup_slack), "{name}: {}", e.pin);
            assert!(same(t.hold[i], e.hold_slack), "{name}: {}", e.pin);
        }
        let idx = a.name_index();
        assert_eq!(idx.len(), a.node_count());
        for e in &rep.endpoints {
            let v = idx.get(&a, &e.pin).expect("endpoint by name");
            assert_eq!(a.node_name(v), e.pin);
        }
        assert!(idx.get(&a, "no such pin").is_none());
        let first = &rep.endpoints[0].pin;
        let prefix: String = first.chars().take(2).collect();
        let found: Vec<u32> = idx.with_prefix(&a, &prefix).collect();
        assert!(!found.is_empty());
        assert!(
            found
                .iter()
                .all(|&v| a.node_name(v).starts_with(prefix.as_str()))
        );
        assert!(
            found
                .windows(2)
                .all(|w| a.node_name(w[0]) <= a.node_name(w[1])),
            "{name}: name order"
        );
        let all = (0..a.node_count() as u32)
            .filter(|&v| a.node_name(v).starts_with(prefix.as_str()))
            .count();
        assert_eq!(found.len(), all, "{name}");
        for p in &rep.paths {
            let kind = if p.kind == "setup" {
                Kind::Setup
            } else {
                Kind::Hold
            };
            let v = idx.get(&a, &p.endpoint).expect("endpoint");
            let w = a.worst_path(v, kind).expect("path on request");
            assert_eq!(
                serde_json::to_value(&w).expect("json"),
                serde_json::to_value(p).expect("json"),
                "{name}: {} {}",
                p.kind,
                p.endpoint
            );
        }
        // A node that is no endpoint has no path.
        let not_end = (0..a.node_count() as u32)
            .find(|v| a.endpoint_slack(*v, Kind::Setup).is_none())
            .expect("a node that is no endpoint");
        assert!(a.worst_path(not_end, Kind::Setup).is_none());
    }
}
