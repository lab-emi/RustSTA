// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Expose existing native RC kernels for one explicitly selected library arc.
//! This is a read-only numerical diagnostic, not an independent timing oracle.
//! One corner, eight libraries/128 MiB source budget, 256 selected-cell arcs;
//! expected peak <512 MiB. Run under an OS memory/time cap. No graph or path search.

use std::error::Error;
use std::fs::File;
use std::io;
use std::path::PathBuf;

use clap::Parser;
use rusteda_compute::rc::{self, Pi};
use rusteda_workspace::Workspace;
use ruststa::engine::delay::driver::pi_drive;
use ruststa::engine::input::libview::TimingLib;
use ruststa_cli::ws_io::load_corner_libraries;
use serde_json::json;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    rev: String,
    #[arg(long)]
    corner: String,
    #[arg(long)]
    cell: String,
    #[arg(long)]
    from: String,
    #[arg(long)]
    to: String,
    #[arg(long)]
    input_slew_ns: f64,
    #[arg(long)]
    near_pf: f64,
    #[arg(long)]
    far_pf: f64,
    #[arg(long)]
    resistance_kohm: f64,
    #[arg(long)]
    fall: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if [&args.cell, &args.from, &args.to]
        .iter()
        .any(|s| s.len() > 4096)
        || !args.input_slew_ns.is_finite()
        || args.input_slew_ns < 0.0
    {
        return Err(io::Error::other("invalid diagnostic arc/slew").into());
    }
    let lock = File::open(args.workspace.join("workspace.lock"))?;
    lock.try_lock_shared()?;
    let ws = Workspace::open(args.workspace)?;
    let rev = ws.revision(ws.resolve_query(&args.rev)?)?;
    if rev.meta().libraries.len() > 8 {
        return Err(io::Error::other("diagnostic library-count budget").into());
    }
    let mut bytes = 0u64;
    for entry in &rev.meta().libraries {
        let n = std::fs::metadata(&entry.source)?.len();
        bytes = bytes.saturating_add(n);
        if n > 64 << 20 || bytes > 128 << 20 {
            return Err(io::Error::other("diagnostic library-byte budget").into());
        }
    }
    let mut diags = Vec::new();
    let owned = load_corner_libraries(&ws, &rev, std::slice::from_ref(&args.corner), &mut diags)?;
    let lib = TimingLib::from_corner(&args.corner, &owned[0].1)
        .map_err(|d| io::Error::other(format!("{d:?}")))?;
    let cell = lib
        .cell(&args.cell)
        .ok_or_else(|| io::Error::other("unknown cell"))?;
    if cell.arcs.len() > 256 {
        return Err(io::Error::other("diagnostic arc budget").into());
    }
    let ids: Vec<_> = cell
        .arcs
        .iter()
        .copied()
        .filter(|&id| {
            let arc = lib.arc(id);
            cell.pins[arc.from].name == args.from && cell.pins[arc.to].name == args.to
        })
        .collect();
    if ids.len() != 1 {
        return Err(
            io::Error::other("arc absent or ambiguous; do not guess a conditional arc").into(),
        );
    }
    let arc = lib.arc(ids[0]);
    let tr = usize::from(args.fall);
    let eval = |c| {
        (
            lib.lut_bank
                .eval(arc.delay[tr], [args.input_slew_ns, c, 0.0]),
            lib.lut_bank
                .eval(arc.slew[tr], [args.input_slew_ns, c, 0.0]),
        )
    };
    let pi = Pi {
        near: args.near_pf,
        far: args.far_pf,
        resistance: args.resistance_kohm,
    };
    let thresholds = cell.thresholds.rc(args.fall);
    let r = pi_drive(pi, thresholds, eval)?;
    let alternate = r.drive.map(|(res, _)| {
        rc::gate_detail_with_resistance(pi, thresholds, res, eval)
            .map(|g| json!({"delay_ns":g.delay,"slew_ns":g.slew,"capacitance_pf":g.capacitance}))
            .map_err(|e| format!("{e}"))
    });
    let result = json!({"schema":"ruststa.rc-arc-probe.v1","rev":args.rev,"corner":args.corner,
        "cell":args.cell,"from":args.from,"to":args.to,"arc_kind":format!("{:?}",arc.kind),
        "input_slew_ns":args.input_slew_ns,"pi":{"near_pf":pi.near,"far_pf":pi.far,"resistance_kohm":pi.resistance},
        "current_native":{"delay_ns":r.delay,"slew_ns":r.slew,"capacitance_pf":r.capacitance},
        "native_waveform":r.drive.map(|(res,duration)|json!({"resistance_kohm":res,"duration_ns":duration})),
        "converged_charge_diagnostic":alternate,"independent_oracle":false,
        "lut_range_reporting":"unchecked in scalar probe; full ruststa run/report retains extrapolation diagnostics"});
    serde_json::to_writer_pretty(io::stdout().lock(), &result)?;
    Ok(())
}
