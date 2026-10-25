// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tag-independent f64 waveform preparation (STA-REQ-0723).
use super::*;
use rusteda_compute::rc::{self, Pi};
use rusteda_compute::timing::{ArcValues, lut::lookup};

pub(super) fn prepare(
    lowered: &mut LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    max_bytes: u64,
) -> Result<(), Diag> {
    if wire.scope_name() != "spef_rc" {
        return Ok(());
    }
    let n = lowered.csr.nodes();
    let arcs = lowered.csr.in_arcs.len();
    let bytes = (n as u64)
        .saturating_mul(32 + 96)
        .saturating_add((arcs as u64).saturating_mul(192));
    if bytes > max_bytes {
        return Err(Diag::error("T0023", "RC arc state budget exceeded"));
    }
    let mut work = 0u64;
    for a in lowered.csr.in_arcs.iter() {
        let cls = lowered.luts.classes[a.class as usize];
        // A wire: three bracketed sink crossings (64 doublings, 100 bisections, two cascades
        // each) and one root crossing (STA-REQ-0729); a cell arc: the one-step correction with
        // one duration fit (56 x 48) and three root crossings (STA-REQ-0728).
        let evaluations = if cls.is_wire() {
            3 * (64 + 100) * 2 + 80
        } else {
            56 * 48 * 2 + 3 * 80 + 4
        };
        for ti in 0..2 {
            for to in 0..2 {
                if cls.allows(ti, to) {
                    work = work.saturating_add(2 * evaluations);
                }
            }
        }
    }
    if work > max_bytes.saturating_mul(8) {
        return Err(Diag::error(
            "T0078",
            format!(
                "RC numeric work budget exceeded: {work} > {}; --max-state-gib also bounds waveform work",
                max_bytes.saturating_mul(8)
            ),
        ));
    }
    let start = Instant::now();
    lowered.arc_values = vec![ArcValues::default(); arcs];
    lowered.wires = vec![WireRec::default(); arcs];
    let mut slews = vec![[absent(0), absent(1), absent(0), absent(1)]; n];
    // The driver resistance of the transition in `slews` (STA-REQ-0729).
    let mut drives = vec![[None; 4]; n];
    for (v, &node) in graph.order().iter().enumerate() {
        if let Some(k) = lowered.starts.find(v as u32) {
            slews[v].copy_from_slice(&lowered.starts.slew[k * 4..k * 4 + 4]);
            if let Some(d) = lowered.start_drives.get(&node) {
                drives[v] = *d;
            }
        }
        for k in lowered.csr.in_ptr[v] as usize..lowered.csr.in_ptr[v + 1] as usize {
            let in_arc = lowered.csr.in_arcs[k];
            let cls = lowered.luts.classes[in_arc.class as usize];
            let raw = lowered.csr_raw_arc(graph, k);
            debug_assert_eq!(raw.dst, node);
            for ti in 0..2 {
                for to in 0..2 {
                    if !cls.allows(ti, to) {
                        continue;
                    }
                    for an in 0..2 {
                        let s = slews[in_arc.src as usize][ti * 2 + an];
                        if s == absent(an) {
                            continue;
                        }
                        let value = arc_value(
                            &lowered.luts,
                            &cls,
                            &raw,
                            view,
                            wire,
                            lib,
                            lowered.load[v][to * 2 + an],
                            s,
                            to,
                            an,
                            drives[in_arc.src as usize][ti * 2 + an],
                        )?;
                        if let Some(tau) = value.tau {
                            lowered.wires[k].delay[to * 2 + an] = tau;
                        }
                        let j = (ti * 2 + to) * 2 + an;
                        let q = value.slew;
                        lowered.arc_values[k].delay[j] = value.delay;
                        lowered.arc_values[k].slew[j] = q;
                        let dst = &mut slews[v][to * 2 + an];
                        if an == 0 && q < *dst || an == 1 && q > *dst {
                            *dst = q;
                            drives[v][to * 2 + an] = value.drive;
                        }
                    }
                }
            }
        }
    }
    lowered.drives = drives;
    lowered.lower_s += start.elapsed().as_secs_f64();
    Ok(())
}

/// The value of one extracted arc for one input transition, output transition and bound.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RcArcValue {
    pub delay: f64,
    pub slew: f64,
    /// The extracted first moment of a net arc.
    pub tau: Option<f64>,
    /// The driver waveform (Thevenin resistance in kOhm, source ramp duration in ns) of a cell
    /// arc into a pi load outside the lumped regime; the net's wires follow it (`STA-REQ-0729`).
    pub drive: Option<(f64, f64)>,
}

/// The value of the arc `raw` with the class `cls` for the input transition `s`, the output
/// transition `to` and the bound `an` under the extracted wire model `wire` (`STA-REQ-0723`).
/// `lumped_load` is the load of the arc's destination when its net has no extracted driver load;
/// `drive` is the driver waveform of the transition `s` at a net arc's source. The only
/// evaluation of an extracted arc: the waveform preparation of the propagation and the
/// transitions of the design rules (`STA-REQ-0809`) both use it. A cell arc into a pi load
/// follows the driver rule of `STA-REQ-0728` and `STA-REQ-0727`; a net arc whose driver has such
/// a waveform follows `STA-REQ-0729`, any other net arc `STA-REQ-0722`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn arc_value(
    luts: &CompiledLutBank<'_>,
    cls: &rusteda_compute::timing::ArcClass,
    raw: &GraphArc,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    lumped_load: f64,
    s: f64,
    to: usize,
    an: usize,
    drive: Option<(f64, f64)>,
) -> Result<RcArcValue, Diag> {
    let t = lib.thresholds.rc(to == 1);
    let (tr, bound) = (Tr::ALL[to], An::ALL[an]);
    if let ArcKind::Net { net } = raw.kind {
        let tau = wire
            .elmore(raw.dst, tr, bound)
            .ok_or_else(|| Diag::error("T0077", "extracted first moment absent"))?;
        let primary = wire.primary_input(net);
        let pi = match (drive, wire.physical_load(net, tr, bound)) {
            (Some(drive), Some(DriverLoad::Pi { c_near, c_far, r })) if !primary && r > 0.0 => {
                Some((
                    drive,
                    Pi {
                        near: c_near,
                        far: c_far,
                        resistance: r,
                    },
                ))
            }
            _ => None,
        };
        let (d, q) = match pi {
            Some(((res, duration), pi)) => rc::wire_from_driver(pi, res, duration, tau, t),
            None => rc::wire(tau, s, t, primary),
        }
        .map_err(|e| Diag::error("T0077", e.to_string()))?;
        return Ok(RcArcValue {
            delay: d,
            slew: q,
            tau: Some(tau),
            drive: None,
        });
    }
    let load = match view.node_net(raw.dst) {
        NetTarget::Net(net) => wire.physical_load(net, tr, bound),
        _ => None,
    };
    let eval = |c| {
        let args = [s, c, c];
        (
            lookup(&luts.luts, &luts.data, cls.delay[to], args),
            lookup(&luts.luts, &luts.data, cls.slew[to], args),
        )
    };
    if let Some(DriverLoad::Pi { c_near, c_far, r }) = load {
        let p = crate::engine::delay::driver::pi_drive(
            Pi {
                near: c_near,
                far: c_far,
                resistance: r,
            },
            t,
            eval,
        )
        .map_err(|e| Diag::error("T0077", e.to_string()))?;
        Ok(RcArcValue {
            delay: p.delay,
            slew: p.slew,
            tau: None,
            drive: p.drive,
        })
    } else {
        let (d, q) = eval(lumped_load);
        Ok(RcArcValue {
            delay: d,
            slew: q.max(0.0),
            tau: None,
            drive: None,
        })
    }
}
