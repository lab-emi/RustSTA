// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Read-only diagnostic of at most 64 clock-network pins from one revision/corner.
//! This exposes existing native analysis state; it is not an independent oracle.
//! Bound: 100k instances, 500k nodes, four clocks, 2 GiB propagation state, two workers.
//! Estimated peak <1 GiB including immutable libraries and RC; run under an OS memory cap.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::io;
use std::path::PathBuf;

use clap::Parser;
use rusteda_core::job::Ctx;
use rusteda_workspace::Workspace;
use ruststa::engine::analysis::{AnalysisOptions, EngineAnalysis};
use ruststa::engine::delay::wire::DriverLoad;
use ruststa::engine::input::NetTarget;
use ruststa::engine::input::constr::CornerLibs;
use ruststa::engine::prop::clocks::ClockModel;
use ruststa::engine::types::{An, ClockId, Tr};
use ruststa_cli::ws_io::{
    compile_constraints, load_corner_libraries, load_parasitics, load_sdc_from_revision,
};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    rev: String,
    #[arg(long)]
    corner: String,
    #[arg(long, required = true)]
    pin: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.pin.len() > 64 || args.pin.iter().any(|p| p.len() > 4096) {
        return Err(io::Error::other("T0072: requested pin/name budget").into());
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build_global()?;
    let lock = File::open(args.workspace.join("workspace.lock"))?;
    lock.try_lock_shared()?;
    let ws = Workspace::open(args.workspace)?;
    let rev = ws.revision(ws.resolve_query(&args.rev)?)?;
    let rbin = rusteda_workspace::netlist::open_rev(&rev)?;
    let frozen = rusteda_workspace::netlist::decode(&rbin)?;
    if frozen.instances.len() > 100_000
        || frozen
            .pin_net
            .len()
            .saturating_add(frozen.port_bit_net.len())
            > 500_000
    {
        return Err(io::Error::other("T0023: diagnostic netlist budget").into());
    }
    let mut diags = Vec::new();
    let (sdc, _) = load_sdc_from_revision(&rev)?;
    let owned = load_corner_libraries(&ws, &rev, std::slice::from_ref(&args.corner), &mut diags)?;
    let refs: Vec<_> = owned
        .iter()
        .map(|(name, libs)| CornerLibs::new(name, libs))
        .collect();
    let compiled = compile_constraints(&sdc, &frozen, &refs, &mut diags)?;
    if compiled.clocks.len() > 4 {
        return Err(io::Error::other("T0023: diagnostic clock budget").into());
    }
    let opts = AnalysisOptions {
        parasitics: load_parasitics(&rev, std::slice::from_ref(&args.corner))?,
        max_state_bytes: 2 << 30,
        max_limit_rows: 100_000,
        ..AnalysisOptions::default()
    };
    let analysis = EngineAnalysis::analyze(&frozen, &refs, compiled, &opts, Ctx::NONE)
        .map_err(|d| io::Error::other(format!("{d:?}")))?;
    let ca = analysis.default_corner();
    if analysis.diags.len() > 1024 {
        return Err(io::Error::other("T0023: diagnostic warning budget").into());
    }
    let diagnostics: Vec<_> = analysis
        .diags
        .iter()
        .map(|d| {
            json!({"code":d.code,
        "severity":format!("{:?}",d.severity),"message":d.message})
        })
        .collect();
    let mut pins = BTreeMap::<String, Value>::new();
    for name in &args.pin {
        let node = analysis
            .find_node(name)
            .ok_or_else(|| io::Error::other("unknown pin: ".to_owned() + name))?;
        let mut clocks = Vec::with_capacity(32);
        for (index, clock) in analysis.constraints.clocks.iter().enumerate() {
            for edge in Tr::ALL {
                for tr in Tr::ALL {
                    for an in An::ALL {
                        if let Some(v) =
                            ca.clock_model
                                .arrival(ClockId(index as u32), edge, node, tr, an)
                        {
                            clocks.push(json!({"clock":clock.name,"edge":format!("{edge:?}"),
                                "transition":format!("{tr:?}"),"bound":format!("{an:?}"),
                                "arrival_ns":v.latency,"slew_ns":v.slew}));
                        }
                    }
                }
            }
        }
        let mut loads = Vec::with_capacity(4);
        if let NetTarget::Net(net) = analysis.netview.node_net(node) {
            for tr in Tr::ALL {
                for an in An::ALL {
                    let load = match ca.net_loads.physical_load(
                        net,
                        tr,
                        an,
                        ca.net_loads.total_cap(net, tr, an),
                    ) {
                        DriverLoad::Lumped { c } => json!({"kind":"lumped","cap_pf":c}),
                        DriverLoad::Pi { c_near, r, c_far } => {
                            json!({"kind":"pi", "near_pf":c_near,
                            "far_pf":c_far,"resistance_kohm":r})
                        }
                    };
                    loads.push(json!({"transition":format!("{tr:?}"),"bound":format!("{an:?}"),"load":load,
                        "arrival_ns":ca.worst_arrival(node,tr,an).map(|x| x.1),"slew_ns":ca.slew(node,tr,an)}));
                }
            }
        }
        pins.insert(
            name.clone(),
            json!({"clock_state":clocks,"data_state_and_load":loads}),
        );
    }
    let result = json!({"schema":"ruststa.clock-probe.v1","rev":args.rev,"corner":args.corner,
        "wire_model":ca.wire_model.scope().wire_model,"pins":pins,"independent_oracle":false,
        "analysis_scope":format!("{:?}",analysis.scope),"diagnostics":diagnostics});
    serde_json::to_writer_pretty(io::stdout().lock(), &result)?;
    Ok(())
}
