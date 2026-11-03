// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Corners analyzed at once and corner by corner: the same tables bit for bit, within the
//! propagation memory budget of every corner together (`STA-REQ-0024`); the kept lowered problems
//! of the timing table; and extracted parasitics released once bound (`STA-REQ-0820`).

use std::sync::{Arc, Mutex};

use rusteda_core::job::{Ctx, Event, Observer};
use rusteda_core::liberty::Library;
use rusteda_core::netlist::{Conn, FrozenNetlist, GateNetlist, PinDir, PortDir};
use rusteda_formats::{liberty, spef};
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis, KeepLowered, Propagation};
use ruststa::engine::delay::spef::ParasiticInput;
use ruststa::engine::input::constr::{CompiledConstraints, CornerLibs};
use ruststa::engine::limits::table::LimitsData;
use ruststa::engine::store::from_analysis::{
    build_store_from_analysis_releasing, build_store_from_analysis_with_ctx,
};
use ruststa::engine::store::layout::{StoreMode, StorePrecision};
use ruststa_fixtures::liberty::{Variant, library};

/// Two buffers in series from one input net to each output port, so that every level is wide.
const WIDTH: usize = 24;

const SDC: &str = "create_clock -name vclk -period 4\n\
    set_input_delay -clock vclk -max 0.3 [get_ports a]\nset_input_delay -clock vclk -min 0.1 [get_ports a]\n\
    set_output_delay -clock vclk -max 0.2 [all_outputs]\nset_output_delay -clock vclk -min 0.05 [all_outputs]\n\
    set_input_transition 0.1 [get_ports a]\nset_load 0.004 [all_outputs]\n\
    set_max_capacitance 0.06 [current_design]\nset_max_transition 0.4 [current_design]\n";

fn lib() -> Library {
    liberty::read_text(&library(Variant::Wt15), "wt_15.lib").unwrap()
}

fn netlist() -> FrozenNetlist<'static> {
    let mut g = GateNetlist::new("top").unwrap();
    let pins = [("A", PinDir::Input), ("Y", PinDir::Output)];
    let buf = g.add_cell_type("wt_buf_1", &pins).unwrap();
    g.add_port("a", PortDir::Input, None).unwrap();
    let a = g.find_net("a", None).unwrap();
    for i in 0..WIDTH {
        g.add_port(&format!("y{i}"), PortDir::Output, None).unwrap();
        let d = g.add_net(Some(&format!("d{i}")), None).unwrap();
        let y = g.find_net(&format!("y{i}"), None).unwrap();
        let b = g.add_instance(&format!("b{i}"), buf).unwrap();
        g.connect(b, "A", Conn::Net(a)).unwrap();
        g.connect(b, "Y", Conn::Net(d)).unwrap();
        let c = g.add_instance(&format!("c{i}"), buf).unwrap();
        g.connect(c, "A", Conn::Net(d)).unwrap();
        g.connect(c, "Y", Conn::Net(y)).unwrap();
    }
    g.freeze()
}

/// The extraction of [`netlist`], its capacitances scaled by `cap` and its resistances by `res`.
/// Every branch differs, so that a value of the wrong net or corner cannot match.
fn spef_text(cap: f64, res: f64) -> String {
    let mut s = String::from(
        "*SPEF \"ieee 1481-1999\"\n*DESIGN top\n*DIVIDER /\n*DELIMITER :\n*BUS_DELIMITER []\n\
         *T_UNIT 1 NS\n*C_UNIT 1 PF\n*R_UNIT 1 KOHM\n",
    );
    // One star net from the driver node `root` (`*P` or `*I`) to one node per sink, with the
    // node capacitances `root_cap` and `sink_cap` and the branch resistances `r(i)`.
    let mut star = |name: &str,
                    root: &str,
                    sinks: &[String],
                    (root_cap, sink_cap): (f64, f64),
                    r: &dyn Fn(usize) -> f64| {
        let (mut conn, mut caps, mut ress) = (String::new(), String::new(), String::new());
        caps += &format!("1 {root} {}\n", root_cap * cap);
        let mut total = root_cap * cap;
        for (i, sink) in sinks.iter().enumerate() {
            let c = sink_cap * cap;
            total += c;
            conn += &if sink.contains(':') {
                format!("*I {sink} I\n")
            } else {
                format!("*P {sink} O\n")
            };
            caps += &format!("{} {sink} {c}\n", i + 2);
            ress += &format!("{} {root} {sink} {}\n", i + 1, r(i) * res);
        }
        let head = if root.contains(':') {
            format!("*I {root} O\n")
        } else {
            format!("*P {root} I\n")
        };
        s += &format!("*D_NET {name} {total}\n*CONN\n{head}{conn}*CAP\n{caps}*RES\n{ress}*END\n");
    };
    let a_sinks: Vec<String> = (0..WIDTH).map(|i| format!("b{i}:A")).collect();
    star("a", "a", &a_sinks, (0.001, 0.001), &|i| {
        0.5 + i as f64 / 16.0
    });
    for i in 0..WIDTH {
        let r = 1.0 + i as f64 / 8.0;
        let (d, b, c, y) = (
            format!("d{i}"),
            format!("b{i}:Y"),
            format!("c{i}:A"),
            format!("y{i}"),
        );
        star(&d, &b, &[c], (0.01, 0.02), &|_| r);
        star(
            &y,
            &format!("c{i}:Y"),
            std::slice::from_ref(&y),
            (0.01, 0.03),
            &|_| 2.0 * r,
        );
    }
    s
}

fn extraction(cap: f64, res: f64) -> Arc<spef::Spef> {
    Arc::new(spef::read(&spef_text(cap, res), spef::Limits::default()).unwrap())
}

/// Two corners with their own extractions.
fn options(max_state_bytes: u64) -> AnalysisOptions {
    AnalysisOptions {
        parasitics: vec![
            Arc::new(ParasiticInput::new("fast", extraction(1.0, 1.0))),
            Arc::new(ParasiticInput::new("slow", extraction(2.5, 1.5))),
        ],
        max_state_bytes,
        ..AnalysisOptions::default()
    }
}

/// The phases an analysis starts.
#[derive(Default)]
struct Phases(Mutex<Vec<&'static str>>);

impl Observer for Phases {
    fn event(&self, e: &Event) {
        if let Event::PhaseStarted { phase } = e {
            self.0.lock().unwrap().push(phase);
        }
    }
}

/// The tables and diagnostics of one analysis, whether its corners ran at once and how many of
/// them waited for the others.
#[derive(Debug, PartialEq)]
struct Tables {
    store: Vec<u8>,
    limits: Vec<u8>,
    diags: Vec<(String, String)>,
    lowered: usize,
    concurrent: bool,
    deferred: usize,
}

/// The lowered problems the timing table is built from.
#[derive(Clone, Copy)]
enum Kept {
    /// From the whole kept lowered problems.
    Full,
    /// From the problems kept for the table, each released after its required times, as
    /// `ruststa run` builds it.
    ForTable,
    /// Without kept problems: every corner lowered again.
    Relowered,
}

fn analyze(
    workers: usize,
    options: &AnalysisOptions,
    table: Kept,
) -> Result<Tables, Vec<(String, String)>> {
    let lib = lib();
    let frozen = netlist();
    let libs = [lib];
    let corners = [
        CornerLibs::new("fast", &libs),
        CornerLibs::new("slow", &libs),
    ];
    let owned = vec![
        ("fast".to_string(), libs.to_vec()),
        ("slow".to_string(), libs.to_vec()),
    ];
    let phases = Phases::default();
    let codes = |d: &[rusteda_core::job::Diag]| {
        d.iter()
            .map(|d| (d.code.to_string(), d.message.clone()))
            .collect::<Vec<_>>()
    };
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .unwrap()
        .install(|| {
            let constraints =
                CompiledConstraints::from_sdc_str(SDC, "top.sdc", &frozen, &corners).unwrap();
            let keep = match table {
                Kept::Full => KeepLowered::Full,
                Kept::ForTable => KeepLowered::ForTable,
                Kept::Relowered => KeepLowered::No,
            };
            let (analysis, record) = EngineAnalysis::analyze_with(
                &frozen,
                &corners,
                None,
                constraints,
                options,
                Ctx::new(None, Some(&phases)),
                Propagation::Full,
                keep,
            )
            .map_err(|d| codes(&d))?;
            let lowered = record.lowered.len();
            let store = match table {
                Kept::Full => build_store_from_analysis_with_ctx(
                    &analysis,
                    Some(&record.lowered),
                    &owned,
                    "cpu",
                    StorePrecision::F64,
                    StoreMode::Full,
                    100,
                    Ctx::NONE,
                ),
                Kept::ForTable => build_store_from_analysis_releasing(
                    &analysis,
                    record.lowered,
                    &owned,
                    "cpu",
                    StorePrecision::F64,
                    StoreMode::Full,
                    100,
                    Ctx::NONE,
                ),
                Kept::Relowered => build_store_from_analysis_with_ctx(
                    &analysis,
                    None,
                    &owned,
                    "cpu",
                    StorePrecision::F64,
                    StoreMode::Full,
                    100,
                    Ctx::NONE,
                ),
            };
            let phases = phases.0.lock().unwrap();
            Ok(Tables {
                store: store.to_bytes().unwrap(),
                limits: LimitsData::from_analysis(&analysis, 1000)
                    .to_bytes()
                    .unwrap(),
                diags: codes(&analysis.diags),
                lowered,
                concurrent: phases.contains(&"concurrent_corners"),
                deferred: phases.iter().filter(|p| **p == "deferred_corner").count(),
            })
        })
}

// STA-REQ-0024, STA-REQ-0820: with two or more worker threads per corner the two corners run at
// once; the tables, the diagnostics and the kept problems are those of the corner-by-corner
// analysis bit for bit, whichever lowered problems the timing table is built from.
#[test]
fn corners_at_once_equal_corner_by_corner() {
    let options = options(1 << 30);
    let serial = analyze(1, &options, Kept::Full).unwrap();
    assert!(!serial.concurrent);
    assert_eq!(serial.deferred, 0);
    assert_eq!(serial.lowered, 2);
    // The corners differ, or this comparison proves nothing.
    assert!(serial.store.len() > 1000);
    for workers in [4, 5] {
        let at_once = analyze(workers, &options, Kept::Full).unwrap();
        assert!(at_once.concurrent, "{workers} workers");
        assert_eq!(at_once.deferred, 0, "{workers} workers");
        assert_eq!(
            Tables {
                concurrent: false,
                ..at_once
            },
            serial,
            "{workers} workers"
        );
    }
    for workers in [1, 4] {
        for table in [Kept::ForTable, Kept::Relowered] {
            let other = analyze(workers, &options, table).unwrap();
            assert_eq!(other.concurrent, workers == 4);
            assert_eq!(other.store, serial.store, "{workers} workers");
            assert_eq!(other.limits, serial.limits, "{workers} workers");
            assert_eq!(other.diags, serial.diags, "{workers} workers");
        }
    }
}

// STA-REQ-0024: the corners run at once only as far as their propagation states fit the limit
// together. At the smallest limit that the corner-by-corner analysis passes, one corner fits and
// two do not: one corner waits until the other is analyzed, with the same tables; at twice that
// limit both run at once. Below it, both analyses fail with the corner's T0023.
#[test]
fn corners_run_at_once_only_within_the_budget() {
    let passes = |max: u64| analyze(1, &options(max), Kept::ForTable).is_ok();
    let (mut lo, mut hi) = (0u64, 1u64 << 30);
    assert!(passes(hi));
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if passes(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let least = hi;
    let serial = analyze(1, &options(least), Kept::ForTable).unwrap();
    let tight = analyze(4, &options(least), Kept::ForTable).unwrap();
    assert_eq!(tight.deferred, 1);
    let roomy = analyze(4, &options(2 * least), Kept::ForTable).unwrap();
    assert!(roomy.concurrent);
    assert_eq!(roomy.deferred, 0);
    for at_once in [tight, roomy] {
        assert_eq!(
            Tables {
                concurrent: false,
                deferred: 0,
                ..at_once
            },
            serial
        );
    }
    for workers in [1, 4] {
        let err = analyze(workers, &options(least - 1), Kept::ForTable).unwrap_err();
        assert!(
            err.iter()
                .any(|(code, m)| code == "T0023" && m.contains("propagation state of corner")),
            "{workers} workers: {err:?}"
        );
    }
}

// STA-REQ-0820: an extraction released once bound serves one analysis: it is given up after
// that analysis has bound its corner, keeps its size estimate and coupling count, and a second
// analysis with it is error T0070. Corners sharing one parsed extraction bind it both.
#[test]
fn released_extraction_serves_one_analysis() {
    let fast = extraction(1.0, 1.0);
    let slow = extraction(2.5, 1.5);
    let estimate = fast.estimated_state_bytes();
    let released = |corner: &str, data: &Arc<spef::Spef>| {
        Arc::new(ParasiticInput::released_after_bind(
            corner,
            Arc::clone(data),
        ))
    };
    let kept = analyze(1, &options(1 << 30), Kept::ForTable).unwrap();
    for workers in [1, 4] {
        let options = AnalysisOptions {
            parasitics: vec![released("fast", &fast), released("slow", &slow)],
            max_state_bytes: 1 << 30,
            ..AnalysisOptions::default()
        };
        let once = analyze(workers, &options, Kept::ForTable).unwrap();
        assert_eq!(
            Tables {
                concurrent: false,
                ..once
            },
            kept,
            "{workers} workers"
        );
        for input in &options.parasitics {
            assert!(input.data().is_none());
        }
        assert_eq!(options.parasitics[0].estimated_state_bytes(), estimate);
        assert_eq!(options.parasitics[0].coupling_records(), 0);
        let err = analyze(workers, &options, Kept::ForTable).unwrap_err();
        assert!(
            err.iter()
                .any(|(code, m)| code == "T0070" && m.contains("released by an earlier analysis")),
            "{workers} workers: {err:?}"
        );
    }
    // Two corners of one parsed extraction: both bind it, then it is released.
    let same = AnalysisOptions {
        parasitics: vec![
            Arc::new(ParasiticInput::new("fast", Arc::clone(&fast))),
            Arc::new(ParasiticInput::new("slow", Arc::clone(&fast))),
        ],
        max_state_bytes: 1 << 30,
        ..AnalysisOptions::default()
    };
    for workers in [1, 4] {
        let shared = AnalysisOptions {
            parasitics: vec![released("fast", &fast), released("slow", &fast)],
            max_state_bytes: 1 << 30,
            ..AnalysisOptions::default()
        };
        assert_eq!(
            analyze(workers, &shared, Kept::ForTable).unwrap(),
            analyze(workers, &same, Kept::ForTable).unwrap(),
            "{workers} workers"
        );
        assert!(shared.parasitics.iter().all(|p| p.data().is_none()));
    }
}
