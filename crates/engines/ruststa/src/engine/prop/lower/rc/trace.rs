// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Private two-pin diagnostic replay; this file is absent from the qualified production ELF.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use rusteda_compute::rc::{self, Pi, Thresholds};
use rusteda_compute::timing::{ArcClass, CompiledLutBank, lut::lookup};
use serde::Serialize;

use super::{An, ArcKind, Diag, DriverLoad, GraphArc, NetTarget, NetlistView, NodeKind};
use super::{RcArcValue, TimingLib, Tr, WireModel};
use crate::engine::delay::driver::{LUMPED_RESISTANCE_RATIO, SENSITIVITY_STEP_PF};

const MAX_RECORDS: usize = 64;
const MAX_BYTES: usize = 1 << 20;
const MAX_ARC_VISITS: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 128;
const OUTPUTS: [&str; 2] = ["_12248_", "_12260_"];

#[derive(Debug)]
pub(super) struct Trace {
    path: PathBuf,
    arc_visits: usize,
    replays: Vec<Record>,
}

#[derive(Debug, Serialize)]
struct Record {
    source: String,
    output: String,
    cell: String,
    raw_source_id: u32,
    raw_destination_id: u32,
    csr_arc: usize,
    library_arc: u32,
    input_transition: usize,
    output_transition: usize,
    input_slew_ns: f64,
    pi_near_pf: f64,
    pi_far_pf: f64,
    pi_resistance_kohm: f64,
    thresholds: [f64; 5],
    table_at_total: [f64; 2],
    table_at_ceff: [f64; 2],
    sensitivity_ns_per_pf: f64,
    driver_resistance_kohm: f64,
    ceff_pf: f64,
    source_ramp_ns: Option<f64>,
    root_crossing_ns: Option<f64>,
    delay_ns: f64,
    slew_ns: f64,
    solver: Option<Solver>,
    diagnostic_fixed_lookup_callbacks: usize,
    original_response_bit_exact: bool,
}

#[derive(Debug, Serialize)]
struct Solver {
    iterations: usize,
    evaluations: usize,
    normalized_charge_residual: f64,
    condition: f64,
}

#[derive(Serialize)]
struct Report<'a> {
    schema: &'static str,
    diagnostic_only: bool,
    corner: &'static str,
    analysis_bound: &'static str,
    wire_model: &'static str,
    pins: [&'static str; 2],
    max_records: usize,
    max_output_bytes: usize,
    max_arc_selection_visits: usize,
    arc_selection_visits: usize,
    original_algorithm_work_counters_changed: bool,
    diagnostic_replay_solver_evaluations: usize,
    diagnostic_fixed_lookup_callbacks: usize,
    records: &'a [Record],
}

fn diagnostic(message: impl Into<String>) -> Diag {
    Diag::error("T0998", message)
}

impl Trace {
    pub(super) fn from_env(corner: &str) -> Result<Option<Self>, Diag> {
        let Some(path) = std::env::var_os("RUSTSTA_DIAG_RC_TRACE") else {
            return Ok(None);
        };
        if corner != "tt" {
            return Ok(None);
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() || path.as_os_str().len() > 4096 || path.exists() {
            return Err(diagnostic(
                "diagnostic trace requires a fresh absolute output path",
            ));
        }
        Ok(Some(Self {
            path,
            arc_visits: 0,
            replays: Vec::with_capacity(MAX_RECORDS),
        }))
    }

    pub(super) fn select(&mut self, view: &NetlistView<'_>, raw: &GraphArc) -> Result<bool, Diag> {
        self.arc_visits += 1;
        if self.arc_visits > MAX_ARC_VISITS {
            return Err(diagnostic("diagnostic trace arc selection budget exceeded"));
        }
        if raw.kind.is_net() {
            return Ok(false);
        }
        let NodeKind::InstancePin {
            inst,
            cell,
            cell_pin,
            ..
        } = view.node_kind(raw.dst)
        else {
            return Ok(false);
        };
        Ok(
            OUTPUTS.contains(&view.instance_name(inst))
                && view.cell_pin_name(cell, cell_pin) == "Y",
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record(
        &mut self,
        view: &NetlistView<'_>,
        wire: &dyn WireModel,
        lib: &TimingLib,
        luts: &CompiledLutBank<'_>,
        cls: &ArcClass,
        raw: &GraphArc,
        csr_arc: usize,
        ti: usize,
        to: usize,
        input_slew: f64,
        original: RcArcValue,
    ) -> Result<(), Diag> {
        if self.replays.len() >= MAX_RECORDS {
            return Err(diagnostic("diagnostic trace replay budget exceeded"));
        }
        let ArcKind::Comb { lib_arc, .. } = raw.kind else {
            return Err(diagnostic(
                "diagnostic trace selected a non-combinational arc",
            ));
        };
        let NodeKind::InstancePin { cell, .. } = view.node_kind(raw.dst) else {
            return Err(diagnostic(
                "diagnostic trace selected a non-instance output",
            ));
        };
        let NetTarget::Net(net) = view.node_net(raw.dst) else {
            return Err(diagnostic("diagnostic output has no net"));
        };
        let Some(DriverLoad::Pi { c_near, c_far, r }) =
            wire.physical_load(net, Tr::ALL[to], An::Late)
        else {
            return Err(diagnostic("diagnostic output has no extracted pi load"));
        };
        let pi = Pi {
            near: c_near,
            far: c_far,
            resistance: r,
        };
        let t = lib.node_thresholds(view, raw.dst).rc(to == 1);
        let eval = |c| {
            let args = [input_slew, c, c];
            (
                lookup(&luts.luts, &luts.data, cls.delay[to], args),
                lookup(&luts.luts, &luts.data, cls.slew[to], args),
            )
        };
        let total_values = eval(pi.total());
        let sensitivity =
            (eval(pi.total() + SENSITIVITY_STEP_PF).0 - total_values.0) / SENSITIVITY_STEP_PF;
        let resistance = sensitivity.max(0.0) * (-(1.0 - t.output).ln());
        // This is an isolated replay after the unchanged original arc evaluation. The default
        // charge bounds and original lookup order are retained; no graph counters are charged.
        let lumped = pi.resistance < LUMPED_RESISTANCE_RATIO * resistance && total_values.1 > 0.0;
        let solution = if lumped {
            None
        } else {
            Some(
                rc::charge::solve(pi, t, resistance, rc::charge::Budget::default(), eval)
                    .map_err(|e| diagnostic(format!("diagnostic replay: {e}")))?,
            )
        };
        let (delay, slew, ceff, drive) = match solution {
            Some(q) => {
                let q = q.response;
                (
                    q.delay,
                    q.slew,
                    q.capacitance,
                    (q.resistance > 0.0).then_some((q.resistance, q.duration)),
                )
            }
            None => (total_values.0, total_values.1, pi.total(), None),
        };
        if original.delay.to_bits() != delay.to_bits()
            || original.slew.to_bits() != slew.to_bits()
            || !same_drive(original.drive, drive)
            || original.tau.is_some()
        {
            return Err(diagnostic(
                "diagnostic replay differs from the original response",
            ));
        }
        let source = view.node_name_str(raw.src).into_owned();
        let output = view.node_name_str(raw.dst).into_owned();
        let cell = view.cell_name(cell).to_owned();
        if [&source, &output, &cell]
            .iter()
            .any(|s| s.len() > MAX_NAME_BYTES)
        {
            return Err(diagnostic("diagnostic trace name byte budget exceeded"));
        }
        let table_ceff = eval(ceff);
        self.replays.push(Record {
            source,
            output,
            cell,
            raw_source_id: raw.src.0,
            raw_destination_id: raw.dst.0,
            csr_arc,
            library_arc: lib_arc.0,
            input_transition: ti,
            output_transition: to,
            input_slew_ns: input_slew,
            pi_near_pf: c_near,
            pi_far_pf: c_far,
            pi_resistance_kohm: r,
            thresholds: threshold_array(t),
            table_at_total: [total_values.0, total_values.1],
            table_at_ceff: [table_ceff.0, table_ceff.1],
            sensitivity_ns_per_pf: sensitivity,
            driver_resistance_kohm: resistance,
            ceff_pf: ceff,
            source_ramp_ns: drive.map(|d| d.1),
            root_crossing_ns: solution.map(|q| q.response.root_crossing),
            delay_ns: delay,
            slew_ns: slew,
            solver: solution.map(|q| Solver {
                iterations: q.iterations,
                evaluations: q.evaluations,
                normalized_charge_residual: q.normalized_charge_residual,
                condition: q.condition,
            }),
            diagnostic_fixed_lookup_callbacks: 3,
            original_response_bit_exact: true,
        });
        Ok(())
    }

    pub(super) fn finish(self) -> Result<(), Diag> {
        if self.replays.is_empty()
            || OUTPUTS.iter().any(|instance| {
                !self
                    .replays
                    .iter()
                    .any(|r| r.output.strip_suffix("/Y") == Some(*instance))
            })
        {
            return Err(diagnostic(
                "diagnostic trace did not observe both selected outputs",
            ));
        }
        let report = Report {
            schema: "ruststa.two-pin-rc-diagnostic.v1",
            diagnostic_only: true,
            corner: "tt",
            analysis_bound: "late",
            wire_model: "spef_rc",
            pins: ["_12248_/Y", "_12260_/Y"],
            max_records: MAX_RECORDS,
            max_output_bytes: MAX_BYTES,
            max_arc_selection_visits: MAX_ARC_VISITS,
            arc_selection_visits: self.arc_visits,
            original_algorithm_work_counters_changed: false,
            diagnostic_replay_solver_evaluations: self
                .replays
                .iter()
                .filter_map(|r| r.solver.as_ref().map(|s| s.evaluations))
                .sum(),
            diagnostic_fixed_lookup_callbacks: self.replays.len() * 3,
            records: &self.replays,
        };
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
            .map_err(|e| diagnostic(format!("diagnostic trace output: {e}")))?;
        let mut output = LimitedWriter {
            file,
            remaining: MAX_BYTES,
        };
        serde_json::to_writer_pretty(&mut output, &report)
            .map_err(|e| diagnostic(format!("diagnostic trace serialization: {e}")))?;
        output
            .file
            .sync_all()
            .map_err(|e| diagnostic(format!("diagnostic trace sync: {e}")))
    }
}

fn threshold_array(t: Thresholds) -> [f64; 5] {
    [t.lower, t.upper, t.input, t.output, t.derate]
}

fn same_drive(a: Option<(f64, f64)>, b: Option<(f64, f64)>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.0.to_bits() == b.0.to_bits() && a.1.to_bits() == b.1.to_bits(),
        _ => false,
    }
}

struct LimitedWriter {
    file: File,
    remaining: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other(
                "diagnostic trace output byte budget exceeded",
            ));
        }
        let n = self.file.write(bytes)?;
        self.remaining -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
