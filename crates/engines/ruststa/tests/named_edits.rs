// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Named edits of the suite's edit files on timing sessions, and slack queries of candidate edits
//! (`owner: E11, X2`; `STA-REQ-0603`, `STA-REQ-0604`, `STA-REQ-0623` to `STA-REQ-0625`,
//! `WS-REQ-0146` to `WS-REQ-0152`).
//!
//! Oracles: after every edit the session is compared bit for bit with a full analysis of its
//! netlist ([`TimingSession::verify_against_full`]); the netlist a session edits is compared with
//! the netlist the shared edits module gives on a plain copy; and the values of a slack query are
//! compared with a full analysis ([`EngineAnalysis::analyze`]) of the edited netlist built apart
//! from any session.

mod support;

use std::fs;

use rusteda_core::job::Ctx;
use rusteda_core::liberty::{Direction, Library};
use rusteda_core::netlist::GateNetlist;
use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc::Sdc;
use rusteda_workspace::edits::{self, Candidate, Edit, EditFile, Sink};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::incr::named::{is_buffer, same_function, symmetric};
use ruststa::engine::incr::{SessionError, SessionOptions, TimingSession, revision_cell_pins};
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::session_api::{
    CandidateOutcome, CheckSummary, CornerReport, EndpointTarget, Evaluation, PathTarget,
    QueryTargets, evaluate, evaluate_candidates,
};
use ruststa::engine::store::canon_f64;
use ruststa_fixtures::liberty::{Variant, library};
use support::mutate::{LibCatalog, Rng, Shape, build_netlist, exception_sdc, parse_sdc};

fn lib(v: Variant) -> Library {
    read_text(&library(v), v.name()).expect("synthetic library")
}

const SHAPE: Shape = Shape {
    inputs: 6,
    regs: 8,
    comb: 24,
    outputs: 6,
    clocks: 2,
    latches: 2,
};

/// The cells of `lib` that the generated edits use.
struct Cells {
    /// Pairs of cells with the same function and pins, by first cell.
    same: Vec<(String, String)>,
    /// Non-inverting buffers.
    buffers: Vec<String>,
}

impl Cells {
    fn of(lib: &Library) -> Self {
        let usable: Vec<_> = lib
            .cells
            .iter()
            .filter(|c| {
                !c.dont_use
                    && c.problems.is_empty()
                    && c.pins
                        .iter()
                        .all(|p| matches!(p.direction, Direction::Input | Direction::Output))
            })
            .collect();
        let mut same = Vec::new();
        for a in &usable {
            for b in &usable {
                if a.name != b.name && same_function(a, b) {
                    same.push((a.name.clone(), b.name.clone()));
                }
            }
        }
        let buffers = usable
            .iter()
            .filter(|c| is_buffer(c))
            .map(|c| c.name.clone())
            .collect();
        Self { same, buffers }
    }
}

fn session(netlist: GateNetlist, l: &Library, sdc: &Sdc, backward: bool) -> TimingSession {
    TimingSession::with_options(
        netlist,
        vec![(String::from("default"), vec![l.clone()])],
        sdc.clone(),
        SessionOptions {
            analysis: AnalysisOptions::default(),
            intern_library_cells: false,
            backward,
        },
    )
    .expect("session")
}

fn pin_sink(i: &str, p: &str) -> Sink {
    Sink::Pin {
        instance: i.into(),
        pin: p.into(),
    }
}

/// A random valid named edit on the netlist of `s`, or `None` when the draw finds none.
fn random_edit(s: &TimingSession, cells: &Cells, rng: &mut Rng, uid: &mut usize) -> Option<Edit> {
    let g = s.netlist();
    let sym = g.symbols();
    let insts: Vec<_> = g.instances().collect();
    let (_, inst) = insts[rng.gen_range(insts.len())];
    let name = sym.resolve(inst.name).to_string();
    let cell = sym.resolve(g.cell(inst.cell).ok()?.name).to_string();
    let lib_cell = |n: &str| {
        s.corner_libs()[0].1[0]
            .cells
            .iter()
            .find(|c| c.name == n)
            .cloned()
    };
    let output_net = |inst: &rusteda_core::netlist::Instance| {
        inst.connected.iter().find_map(|&p| {
            let pin = g.pin(p).ok()?;
            let ct = g.cell(inst.cell).ok()?;
            (ct.pins[pin.cell_pin as usize].dir.drives())
                .then_some(pin.conn)
                .flatten()
        })
    };
    // The input-pin sinks of the net an instance drives, as edit sinks.
    let sinks_of = |net| -> Vec<Sink> {
        let Ok(n) = g.net(net) else { return Vec::new() };
        n.pins
            .iter()
            .filter_map(|&p| {
                let pin = g.pin(p).ok()?;
                let i = g.instance(pin.inst).ok()?;
                let cp = &g.cell(i.cell).ok()?.pins[pin.cell_pin as usize];
                (cp.dir == rusteda_core::netlist::PinDir::Input)
                    .then(|| pin_sink(sym.resolve(i.name), sym.resolve(cp.name)))
            })
            .collect()
    };
    *uid += 1;
    match rng.gen_range(5) {
        0 => {
            let alts: Vec<_> = cells.same.iter().filter(|(a, _)| *a == cell).collect();
            let (_, to) = alts.get(rng.gen_range(alts.len().max(1)))?;
            Some(Edit::Resize {
                instance: name,
                cell: to.clone(),
            })
        }
        1 => {
            let Some(rusteda_core::netlist::Conn::Net(net)) = output_net(inst) else {
                return None;
            };
            let net_name = g.net(net).ok()?.name.map(|n| sym.resolve(n).to_string())?;
            let mut sinks = sinks_of(net);
            sinks.truncate(1 + rng.gen_range(sinks.len().max(1)));
            // An output port on the net moves behind the buffer too, now and then.
            if g.find_port(&net_name).is_some() && (sinks.is_empty() || rng.gen_range(2) == 0) {
                sinks.push(Sink::Port {
                    port: net_name.clone(),
                });
            }
            if sinks.is_empty() || cells.buffers.is_empty() {
                return None;
            }
            Some(Edit::InsertBuffer {
                net: net_name,
                cell: cells.buffers[rng.gen_range(cells.buffers.len())].clone(),
                instance: format!("e_buf_{uid}"),
                net_out: format!("e_nbuf_{uid}"),
                sinks,
            })
        }
        2 => {
            let bufs: Vec<String> = insts
                .iter()
                .map(|(_, i)| sym.resolve(i.name).to_string())
                .filter(|n| n.starts_with("e_buf_"))
                .collect();
            let b = bufs.get(rng.gen_range(bufs.len().max(1)))?;
            Some(Edit::RemoveBuffer {
                instance: b.clone(),
            })
        }
        3 => {
            let lc = lib_cell(&cell)?;
            let ins: Vec<&str> = lc
                .pins
                .iter()
                .filter(|p| p.direction == Direction::Input)
                .map(|p| p.name.as_str())
                .collect();
            for a in &ins {
                for b in &ins {
                    if a < b && symmetric(&lc, a, b) && rng.gen_range(2) == 0 {
                        return Some(Edit::SwapPins {
                            instance: name,
                            pin_a: (*a).to_string(),
                            pin_b: (*b).to_string(),
                        });
                    }
                }
            }
            None
        }
        _ => {
            let lc = lib_cell(&cell)?;
            if !lc.sequentials.is_empty() || lc.outputs().count() != 1 {
                return None;
            }
            let Some(rusteda_core::netlist::Conn::Net(net)) = output_net(inst) else {
                return None;
            };
            let mut sinks = sinks_of(net);
            sinks.truncate(1 + rng.gen_range(sinks.len().max(1)));
            let net_name = g.net(net).ok()?.name.map(|n| sym.resolve(n).to_string())?;
            if g.find_port(&net_name).is_some() && (sinks.is_empty() || rng.gen_range(2) == 0) {
                sinks.push(Sink::Port { port: net_name });
            }
            if sinks.is_empty() {
                return None;
            }
            Some(Edit::Clone {
                instance: name,
                new_instance: format!("e_cln_{uid}"),
                new_net: format!("e_ncln_{uid}"),
                sinks,
            })
        }
    }
}

/// Applies `edits` to a plain copy of `netlist` with the shared edits module, as a tool that
/// accepted them would (`WS-REQ-0151`).
fn plain_apply(
    netlist: &GateNetlist,
    corners: &[(String, Vec<Library>)],
    edits: &[Edit],
) -> GateNetlist {
    let mut g = netlist.clone();
    for e in edits {
        edits::apply(&mut g, e, &|n| revision_cell_pins(corners, n)).expect("edit applies");
    }
    g
}

// STA-REQ-0603, WS-REQ-0146 to WS-REQ-0151: 300 random named edits of every kind on designs with
// exceptions, on both synthetic libraries; after every edit the session equals a full analysis,
// and its netlist is the netlist the shared edits module gives.
#[test]
fn random_named_edits_bitwise() {
    for (v, seed) in [
        (Variant::Full, 0xED17_0001u64),
        (Variant::Rich, 0xED17_0002),
    ] {
        let l = lib(v);
        let cells = Cells::of(&l);
        let netlist = build_netlist(&l, seed, &SHAPE);
        let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(&l)));
        let mut s = session(netlist.clone(), &l, &sdc, true);
        let corners = s.corner_libs().to_vec();
        let mut rng = Rng::new(seed ^ 0xF00D);
        let (mut uid, mut applied, mut by_kind) = (0usize, Vec::new(), [0usize; 5]);
        while applied.len() < 300 {
            let Some(e) = random_edit(&s, &cells, &mut rng, &mut uid) else {
                continue;
            };
            s.apply_named_edits(std::slice::from_ref(&e))
                .unwrap_or_else(|err| panic!("{}: {e:?}: {err}", v.name()));
            by_kind[[
                "resize",
                "insert_buffer",
                "remove_buffer",
                "swap_pins",
                "clone",
            ]
            .iter()
            .position(|k| *k == e.op())
            .unwrap_or(0)] += 1;
            applied.push(e);
            if let Err(m) = s.verify_against_full() {
                panic!(
                    "{}: after {} edits ({:?}): {m}",
                    v.name(),
                    applied.len(),
                    applied.last()
                );
            }
        }
        assert!(
            by_kind.iter().all(|&n| n > 0),
            "{}: edits by kind {by_kind:?}",
            v.name()
        );
        assert_eq!(
            s.netlist().freeze(),
            plain_apply(&netlist, &corners, &applied).freeze(),
            "{}: the session's netlist is the netlist of the shared edits",
            v.name()
        );
    }
}

// STA-REQ-0604: edits that would change the logic are rejected with their code, and the session
// stays equal to a full analysis.
#[test]
fn named_edit_rejections_have_codes() {
    let l = lib(Variant::Full);
    let netlist = build_netlist(&l, 0xED17_0003, &SHAPE);
    let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(&l)));
    let mut s = session(netlist, &l, &sdc, false);
    let g = s.netlist();
    let find = |prefix: &str| {
        g.instances()
            .find(|(_, i)| {
                g.symbols()
                    .resolve(g.cell(i.cell).unwrap().name)
                    .starts_with(prefix)
            })
            .map(|(_, i)| g.symbols().resolve(i.name).to_string())
    };
    let drv = String::from("u_out_drv_0");
    let drv_cell = g
        .symbols()
        .resolve(
            g.cell(g.instance(g.find_instance(&drv).unwrap()).unwrap().cell)
                .unwrap()
                .name,
        )
        .to_string();
    let other = if drv_cell.starts_with("wtf_inv") {
        "wtf_buf_1"
    } else {
        "wtf_inv_1"
    };
    let mut cases = vec![
        (
            Edit::Resize {
                instance: drv.clone(),
                cell: other.into(),
            },
            "T0102",
        ),
        (
            Edit::Resize {
                instance: drv.clone(),
                cell: "no_such_cell".into(),
            },
            "T0105",
        ),
        (
            Edit::InsertBuffer {
                net: "out0".into(),
                cell: "wtf_inv_1".into(),
                instance: "b_inv".into(),
                net_out: "n_inv".into(),
                sinks: vec![Sink::Port {
                    port: "out0".into(),
                }],
            },
            "T0103",
        ),
        (
            Edit::Resize {
                instance: "nobody".into(),
                cell: "wtf_buf_1".into(),
            },
            "W0305",
        ),
    ];
    if drv_cell.starts_with("wtf_inv") {
        cases.push((
            Edit::RemoveBuffer {
                instance: drv.clone(),
            },
            "T0103",
        ));
    }
    if let Some(aoi) = find("wtf_a21oi") {
        cases.push((
            Edit::SwapPins {
                instance: aoi,
                pin_a: "A1".into(),
                pin_b: "B1".into(),
            },
            "T0104",
        ));
    }
    for (e, code) in &cases {
        let err = s
            .apply_named_edits(std::slice::from_ref(e))
            .expect_err("rejected");
        assert_eq!(err.code().as_deref(), Some(*code), "{e:?}: {err}");
    }
    s.verify_against_full()
        .expect("unchanged and bitwise equal");
    assert!(matches!(
        s.apply_named_edits(&[Edit::RemoveBuffer {
            instance: "u_reg_0".into()
        }]),
        Err(SessionError::Edit(_) | SessionError::Diag(_))
    ));
}

/// The full analysis of `netlist` with `sdc`, as `ruststa run` analyzes a revision.
fn full_eval(
    netlist: &GateNetlist,
    corners: &[(String, Vec<Library>)],
    sdc: &Sdc,
    targets: &QueryTargets,
    names: &dyn Fn(&GateNetlist, &str) -> bool,
) -> Evaluation {
    let frozen = netlist.freeze();
    let refs: Vec<CornerLibs<'_>> = corners
        .iter()
        .map(|(n, l)| CornerLibs::new(n.as_str(), l.as_slice()))
        .collect();
    let compiled = CompiledConstraints::compile(sdc, &frozen, &refs).expect("constraints");
    let a = EngineAnalysis::analyze(
        &frozen,
        &refs,
        compiled,
        &AnalysisOptions::default(),
        Ctx::NONE,
    )
    .expect("full analysis");
    let node = |name: &str| a.find_node(name).filter(|_| names(netlist, name));
    let slack = |c: usize, name: &str, hold: bool| {
        let rec = a.corners[c].endpoint(node(name)?)?;
        let r = if hold {
            rec.hold.as_ref()
        } else {
            rec.setup.as_ref()
        };
        r.map(|r| canon_f64(r.slack))
    };
    // The values of a corner summary as the query specifies them (`STA-REQ-0624`).
    let check = |worst: Option<f64>, tns: f64, violating: u32| {
        let worst = worst.map(canon_f64);
        CheckSummary {
            wns_ns: worst.map(|w| if w < 0.0 { w } else { 0.0 }),
            tns_ns: worst.map(|_| canon_f64(tns)),
            violating_endpoints: violating,
            worst_slack_ns: worst,
        }
    };
    let corners_report = a
        .corners
        .iter()
        .map(|c| {
            let s = &c.endpoints.summary;
            CornerReport {
                corner: c.corner_name.clone(),
                setup: check(s.wns_setup_ns, s.tns_setup_ns, s.violating_setup),
                hold: check(s.wns_hold_ns, s.tns_hold_ns, s.violating_hold),
            }
        })
        .collect::<Vec<_>>();
    Evaluation {
        corners: corners_report,
        summary: a.sta_summary,
        endpoints: targets
            .endpoints
            .iter()
            .map(|t| {
                node(&t.name).map(|_| {
                    (0..a.corners.len())
                        .map(|c| ruststa::engine::session_api::EndpointSlack {
                            setup_ns: slack(c, &t.name, false),
                            hold_ns: slack(c, &t.name, true),
                        })
                        .collect()
                })
            })
            .collect(),
        paths: targets
            .paths
            .iter()
            .map(|p| slack(p.corner, &p.endpoint.name, p.is_hold_family()))
            .collect(),
        forward_nodes_evaluated: 0,
        // The pin limit counts of the full analysis (`STA-REQ-0807`).
        limits: a
            .corners
            .iter()
            .map(ruststa::engine::store::from_analysis::corner_limits_report)
            .collect(),
    }
}

/// Compares every reported value bitwise (`-0.0` and `NaN` differ from `0.0`).
fn assert_bitwise(label: &str, q: &Evaluation, full: &Evaluation) {
    let bits = |v: &Option<f64>| v.map(f64::to_bits);
    assert_eq!(
        format!("{:?}", q.limits),
        format!("{:?}", full.limits),
        "{label}: limit counts"
    );
    let s = (&q.summary, &full.summary);
    assert_eq!(
        bits(&s.0.wns_setup_ns),
        bits(&s.1.wns_setup_ns),
        "{label}: wns"
    );
    assert_eq!(
        s.0.tns_setup_ns.to_bits(),
        s.1.tns_setup_ns.to_bits(),
        "{label}: tns"
    );
    assert_eq!(
        bits(&s.0.wns_hold_ns),
        bits(&s.1.wns_hold_ns),
        "{label}: whs"
    );
    assert_eq!(
        s.0.tns_hold_ns.to_bits(),
        s.1.tns_hold_ns.to_bits(),
        "{label}: ths"
    );
    assert_eq!(s.0.violating_setup, s.1.violating_setup, "{label}");
    assert_eq!(s.0.violating_hold, s.1.violating_hold, "{label}");
    assert_eq!(q.corners.len(), full.corners.len(), "{label}");
    for (a, b) in q.corners.iter().zip(&full.corners) {
        for (x, y) in [(&a.setup, &b.setup), (&a.hold, &b.hold)] {
            assert_eq!(
                bits(&x.wns_ns),
                bits(&y.wns_ns),
                "{label}: {} wns",
                a.corner
            );
            assert_eq!(
                bits(&x.tns_ns),
                bits(&y.tns_ns),
                "{label}: {} tns",
                a.corner
            );
            assert_eq!(bits(&x.worst_slack_ns), bits(&y.worst_slack_ns), "{label}");
            assert_eq!(x.violating_endpoints, y.violating_endpoints, "{label}");
        }
    }
    for (k, (a, b)) in q.endpoints.iter().zip(&full.endpoints).enumerate() {
        match (a, b) {
            (Some(a), Some(b)) => {
                for (x, y) in a.iter().zip(b) {
                    assert_eq!(
                        bits(&x.setup_ns),
                        bits(&y.setup_ns),
                        "{label}: endpoint {k}"
                    );
                    assert_eq!(bits(&x.hold_ns), bits(&y.hold_ns), "{label}: endpoint {k}");
                }
            }
            (None, None) => {}
            _ => panic!("{label}: endpoint {k} present in one analysis only"),
        }
    }
    for (k, (a, b)) in q.paths.iter().zip(&full.paths).enumerate() {
        assert_eq!(bits(a), bits(b), "{label}: path {k}");
    }
}

/// A slack query of `n_candidates` random candidates on a design of `l`, every value compared
/// with a full analysis of the edited netlist (`STA-REQ-0623` to `STA-REQ-0625`).
fn query_equals_full_analyses(label: &str, l: &Library, seed: u64, n_candidates: usize) {
    let cells = Cells::of(l);
    let netlist = build_netlist(l, seed, &SHAPE);
    let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(l)));
    let mut base = session(netlist.clone(), l, &sdc, false);
    let corners = base.corner_libs().to_vec();
    let mut rng = Rng::new(seed ^ 0xBA5E);
    let mut uid = 0usize;
    let mut file = EditFile::default();
    while file.base.len() < 3 {
        if let Some(e) = random_edit(&base, &cells, &mut rng, &mut uid)
            && base.apply_named_edits(std::slice::from_ref(&e)).is_ok()
        {
            file.base.push(e);
        }
    }
    for k in 0..n_candidates {
        let mut edits = Vec::new();
        let mut probe = base.clone();
        while edits.len() < 1 + k % 3 {
            if let Some(e) = random_edit(&probe, &cells, &mut rng, &mut uid)
                && probe.apply_named_edits(std::slice::from_ref(&e)).is_ok()
            {
                edits.push(e);
            }
        }
        file.candidates.push(Candidate {
            id: format!("c{k}"),
            edits,
        });
    }
    // A candidate that changes the logic is rejected; the others are evaluated.
    file.candidates.push(Candidate {
        id: "bad".into(),
        edits: vec![Edit::Resize {
            instance: "u_out_drv_0".into(),
            cell: "no_such_cell".into(),
        }],
    });
    let mut targets = QueryTargets::default();
    for name in ["u_reg_1/D", "u_reg_3/D", "out0", "out1"] {
        let name = if name.contains('/') {
            let d = &LibCatalog::from_library(l).dff.2;
            name.replace("/D", &format!("/{d}"))
        } else {
            name.to_string()
        };
        let key = base.key_of_name(&name).expect("endpoint");
        targets.endpoints.push(EndpointTarget {
            label: name.clone(),
            name: name.clone(),
            key,
        });
        targets.paths.push(PathTarget {
            label: format!("path{}", targets.paths.len() + 1),
            endpoint: EndpointTarget {
                label: name.clone(),
                name,
                key,
            },
            corner: 0,
            check: if targets.paths.len() % 2 == 0 {
                "setup"
            } else {
                "hold"
            }
            .into(),
        });
    }
    let present = |g: &GateNetlist, name: &str| {
        // A pin of an instance that an edit removed is gone; ports stay.
        match name.split_once('/') {
            Some((i, _)) => g.find_instance(i).is_some(),
            None => true,
        }
    };
    let base_q = evaluate(&base, &targets);
    let base_full = full_eval(
        &plain_apply(&netlist, &corners, &file.base),
        &corners,
        &sdc,
        &targets,
        &present,
    );
    assert_bitwise(&format!("{label}/base"), &base_q, &base_full);
    let outcomes = evaluate_candidates(&base, &file, &targets, None);
    let mut evaluated = 0;
    for (c, o) in file.candidates.iter().zip(&outcomes) {
        match o {
            CandidateOutcome::Evaluated(q) => {
                evaluated += 1;
                let mut all = file.base.clone();
                all.extend(c.edits.iter().cloned());
                let full = full_eval(
                    &plain_apply(&netlist, &corners, &all),
                    &corners,
                    &sdc,
                    &targets,
                    &present,
                );
                assert_bitwise(&format!("{label}/{}", c.id), q, &full);
            }
            CandidateOutcome::Rejected(e) => {
                assert_eq!(c.id, "bad", "{label}: {e}");
                assert_eq!(e.code().as_deref(), Some("T0105"));
            }
            CandidateOutcome::NotEvaluated => panic!("no deadline"),
        }
    }
    assert_eq!(evaluated, n_candidates);
    // The base session is unchanged by the candidates.
    assert_bitwise(
        &format!("{label}/base again"),
        &evaluate(&base, &targets),
        &base_full,
    );
}

// STA-REQ-0623 to STA-REQ-0625: every value of a slack query equals a full analysis of the
// netlist with the base and candidate edits applied, on both synthetic libraries.
#[test]
fn query_values_equal_full_analyses() {
    query_equals_full_analyses("wt_full", &lib(Variant::Full), 0xC0DE_0001, 12);
    query_equals_full_analyses("wt_rich", &lib(Variant::Rich), 0xC0DE_0002, 12);
}

// STA-REQ-0603, STA-REQ-0625: named edits and slack queries on the `tt` library of each PDK.
// Invoked by `cargo xtask verify pdk`.
#[test]
#[ignore = "invoked by `cargo xtask verify pdk`"]
fn pdk_named_edits_and_queries_bitwise() {
    for (idx, (tag, path)) in support::pdk::campaign_libraries().iter().enumerate() {
        let text = fs::read_to_string(path).expect("read PDK liberty file");
        let name = path.file_name().expect("file name").to_string_lossy();
        let l = read_text(&text, &name).expect("parse PDK liberty file");
        query_equals_full_analyses(tag, &l, 0xC0DE_1000 + idx as u64, 8);
        let cells = Cells::of(&l);
        let netlist = build_netlist(&l, 0xED17_1000 + idx as u64, &SHAPE);
        let sdc = parse_sdc(&exception_sdc(&SHAPE, &LibCatalog::from_library(&l)));
        let mut s = session(netlist, &l, &sdc, true);
        let mut rng = Rng::new(0xED17_2000 + idx as u64);
        let (mut uid, mut n) = (0usize, 0);
        while n < 200 {
            let Some(e) = random_edit(&s, &cells, &mut rng, &mut uid) else {
                continue;
            };
            s.apply_named_edits(std::slice::from_ref(&e))
                .unwrap_or_else(|err| panic!("{tag}: {e:?}: {err}"));
            n += 1;
            s.verify_against_full()
                .unwrap_or_else(|m| panic!("{tag}: after {n} edits: {m}"));
        }
    }
}
