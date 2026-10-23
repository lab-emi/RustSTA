// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! RustSTA: static timing analysis (`docs/specs/sta/`).
//!
//! The analysis on the RustEDA workspace is built in [`engine`]. The other modules are the legacy
//! engine behind `ruststa sta`; they keep their paths and their behaviour until the new engine
//! replaces them.

mod analysis;
pub mod checks;
pub mod constraints;
pub mod engine;
#[cfg(feature = "cuda")]
mod gpu;
pub mod graph;
pub mod model;
pub mod netlist;
pub mod old_sdc;
pub mod prop;
pub mod report;
pub mod view;

use std::path::PathBuf;
use std::time::Instant;

use rusteda_core::job::{Ctx, Diag, Event, JobError};
use rusteda_core::liberty::Library;
use rusteda_formats::verilog::{Diagnostic, PpOptions, SourceMap, read_sources, read_texts};

pub use analysis::ReportOptions;
pub use report::Report;

/// Value of `Report::format` (STA-REQ-0306).
pub const REPORT_FORMAT: &str = "ruststa-sta-report";

/// Netlist sources.
#[derive(Debug, Clone)]
pub enum Source {
    Files(Vec<PathBuf>),
    Texts(Vec<(String, String)>),
}

/// Propagation backend (STA-REQ-0400).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    /// The first GPU runtime of this build that passes its device self-test.
    Gpu,
    /// CUDA C kernels compiled by NVRTC (STA-REQ-0407).
    Cuda,
}

/// Analysis options (STA-REQ-0300).
#[derive(Debug, Clone)]
pub struct StaOptions {
    pub netlist: Source,
    pub top: Option<String>,
    /// Constraint text in SDC syntax.
    pub sdc: String,
    pub paths: usize,
    /// Endpoints whose worst setup and hold paths are reported in addition.
    pub path_to: Vec<String>,
    pub backend: Backend,
    /// Name of the constraint file in messages; empty: none (STA-REQ-0304).
    pub sdc_name: String,
}

impl Default for StaOptions {
    fn default() -> StaOptions {
        StaOptions {
            netlist: Source::Texts(Vec::new()),
            top: None,
            sdc: String::new(),
            paths: 5,
            path_to: Vec::new(),
            backend: Backend::Cpu,
            sdc_name: String::new(),
        }
    }
}

/// Failure with a message (input error) or an internal failure.
#[derive(Debug, Clone)]
pub struct StaError {
    pub message: String,
    pub internal: bool,
}

fn input_err(message: impl Into<String>) -> StaError {
    StaError {
        message: message.into(),
        internal: false,
    }
}

/// Everything computed by an analysis, for callers that need more than the report.
#[derive(Debug)]
pub struct Analysis {
    pub model: model::Model,
    pub flat: netlist::Flat,
    pub graph: graph::Graph,
    pub constraints: constraints::Constraints,
    pub setup: prop::Setup,
    pub values: prop::Values,
    pub endpoints: checks::Endpoints,
    /// Backend that computed `values`: `cpu` or `cuda`.
    pub backend: &'static str,
    /// Seconds per phase.
    pub phase_s: Vec<(&'static str, f64)>,
    /// Seconds of the whole run.
    pub run_s: f64,
}

/// Builds and propagates the timing graph. With a GPU backend, the runtime and its kernels are
/// prepared on a second thread while the netlist is read and the graph is built (STA-REQ-0408).
pub fn run(opts: &StaOptions, libs: &[Library]) -> Result<Analysis, StaError> {
    run_with(opts, libs, Ctx::NONE).map_err(|e| match e {
        JobError::Failed(e) => e,
        JobError::Cancelled => StaError {
            message: "error[T0901]: internal: cancelled without a cancel token".into(),
            internal: true,
        },
    })
}

/// `run` with a run context (STA-REQ-0310): phase, metric and message events, level progress of
/// the CPU propagation, and checkpoints between phases and levels.
pub fn run_with(
    opts: &StaOptions,
    libs: &[Library],
    ctx: Ctx<'_>,
) -> Result<Analysis, JobError<StaError>> {
    #[cfg(feature = "cuda")]
    if let Some(want) = gpu_runtime(opts.backend) {
        return std::thread::scope(|s| {
            // Without a thread, the preparation is skipped.
            let warm = std::thread::Builder::new()
                .name("ruststa-gpu-prepare".into())
                .spawn_scoped(s, move || gpu::prewarm(want))
                .ok();
            build(opts, libs, warm, ctx)
        });
    }
    build(opts, libs, None, ctx)
}

#[cfg(feature = "cuda")]
fn gpu_runtime(b: Backend) -> Option<Option<rusteda_compute::GpuRuntime>> {
    use rusteda_compute::GpuRuntime;
    match b {
        Backend::Cpu => None,
        Backend::Gpu => Some(None),
        Backend::Cuda => Some(Some(GpuRuntime::Cuda)),
    }
}

fn build(
    opts: &StaOptions,
    libs: &[Library],
    warm: Option<std::thread::ScopedJoinHandle<'_, ()>>,
    ctx: Ctx<'_>,
) -> Result<Analysis, JobError<StaError>> {
    let t0 = Instant::now();
    let mut phase_s = Vec::new();
    let mut t = Instant::now();
    let mut lap = |name: &'static str, next: Option<&'static str>| {
        let seconds = t.elapsed().as_secs_f64();
        phase_s.push((name, seconds));
        ctx.emit(|| Event::PhaseFinished {
            phase: name,
            seconds,
        });
        if let Some(phase) = next {
            ctx.emit(|| Event::PhaseStarted { phase });
        }
        t = Instant::now();
    };
    let failed = |e: StaError| JobError::Failed(e);
    ctx.emit(|| Event::PhaseStarted { phase: "read" });
    let model = model::Model::build(libs).map_err(|e| failed(input_err(e)))?;
    ctx.checkpoint()?;
    let mut sm = SourceMap::new();
    let (units, diags) = match &opts.netlist {
        Source::Files(f) => read_sources(&mut sm, f, PpOptions::default()),
        Source::Texts(t) => {
            let refs: Vec<(&str, &str)> = t.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            read_texts(&mut sm, &refs, PpOptions::default())
        }
    };
    if diags.iter().any(Diagnostic::is_error) {
        return Err(failed(input_err(
            diags
                .iter()
                .filter(|d| d.is_error())
                .map(|d| d.render(&sm))
                .collect::<Vec<_>>()
                .join("\n"),
        )));
    }
    ctx.checkpoint()?;
    let is_cell = |n: &str| model.by_name.contains_key(n);
    let flat = netlist::flatten(&units, opts.top.as_deref(), &is_cell)
        .map_err(|e| failed(input_err(e)))?;
    lap("read", Some("graph"));
    ctx.checkpoint()?;
    let graph = graph::Graph::build(&flat, &model).map_err(|e| failed(input_err(e)))?;
    ctx.emit(|| Event::Metric {
        name: "graph_nodes",
        value: graph.nodes.len() as f64,
    });
    ctx.emit(|| Event::Metric {
        name: "graph_arcs",
        value: graph.arcs.len() as f64,
    });
    ctx.emit(|| Event::Metric {
        name: "graph_levels",
        value: graph.level_start.len().saturating_sub(1) as f64,
    });
    lap("graph", Some("setup"));
    ctx.checkpoint()?;
    let cmds = old_sdc::parse(&opts.sdc).map_err(|e| {
        failed(input_err(if opts.sdc_name.is_empty() {
            format!("SDC {e}")
        } else {
            Diag::error("T0006", e.msg.clone())
                .at(opts.sdc_name.clone(), e.line, e.col)
                .render()
        }))
    })?;
    let constraints =
        constraints::Constraints::from_sdc(&cmds, &graph, model.time_unit, model.cap_unit)
            .map_err(|e| failed(input_err(e)))?;
    let setup =
        prop::Setup::build(&graph, &model, &constraints).map_err(|e| failed(input_err(e)))?;
    ctx.emit(|| Event::Metric {
        name: "clocks",
        value: constraints.clocks.len() as f64,
    });
    lap("setup", Some("propagate"));
    ctx.checkpoint()?;
    if let Some(w) = warm {
        // A panic of the preparation is reported by the propagation itself.
        let _ = w.join();
    }
    let (values, backend) = propagate(&graph, &model, &setup, opts.backend, ctx)?;
    lap("propagate", Some("checks"));
    ctx.checkpoint()?;
    let endpoints = checks::check_all(&graph, &model, &constraints, &setup, &values)
        .map_err(|e| failed(input_err(e)))?;
    lap("checks", None);
    let a = Analysis {
        model,
        flat,
        graph,
        constraints,
        setup,
        values,
        endpoints,
        backend,
        phase_s,
        run_s: t0.elapsed().as_secs_f64(),
    };
    // Notes in report order (STA-REQ-0310).
    if ctx.observed() {
        for n in a.notes() {
            ctx.diag(|| Diag::note("T0003", n));
        }
    }
    Ok(a)
}

#[cfg(feature = "cuda")]
fn propagate(
    g: &graph::Graph,
    m: &model::Model,
    s: &prop::Setup,
    b: Backend,
    ctx: Ctx<'_>,
) -> Result<(prop::Values, &'static str), JobError<StaError>> {
    let Some(want) = gpu_runtime(b) else {
        return Ok((prop::propagate_cpu_parallel_with(g, m, s, ctx)?, "cpu"));
    };
    let (v, rt) = gpu::propagate_gpu(g, m, s, want)
        .map_err(|e| JobError::Failed(input_err(format!("GPU backend: {e}"))))?;
    ctx.checkpoint()?;
    Ok((v, rt.name()))
}

#[cfg(not(feature = "cuda"))]
fn propagate(
    g: &graph::Graph,
    m: &model::Model,
    s: &prop::Setup,
    b: Backend,
    ctx: Ctx<'_>,
) -> Result<(prop::Values, &'static str), JobError<StaError>> {
    match b {
        Backend::Cpu => Ok((prop::propagate_cpu_parallel_with(g, m, s, ctx)?, "cpu")),
        _ => Err(JobError::Failed(input_err(
            "this build has no GPU runtime (enable the `cuda` feature)",
        ))),
    }
}

/// Runs the whole analysis and builds the report (STA-REQ-0302).
pub fn analyze(opts: &StaOptions, libs: &[Library]) -> Result<Report, StaError> {
    let t0 = Instant::now();
    let a = run(opts, libs)?;
    let mut rep = a.report(&ReportOptions {
        paths: opts.paths,
        path_to: opts.path_to.clone(),
    })?;
    rep.runtime_s = t0.elapsed().as_secs_f64();
    Ok(rep)
}
