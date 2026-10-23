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
        .saturating_mul(32)
        .saturating_add((arcs as u64).saturating_mul(192));
    if bytes > max_bytes {
        return Err(Diag::error("T0023", "RC arc state budget exceeded"));
    }
    let mut work = 0u64;
    for a in lowered.csr.in_arcs.iter() {
        let cls = lowered.luts.classes[a.class as usize];
        let evaluations = if cls.is_wire() {
            3 * 80
        } else {
            64 * 56 * 48 * 2
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
    for (v, &node) in graph.order().iter().enumerate() {
        if let Some(k) = lowered.starts.find(v as u32) {
            slews[v].copy_from_slice(&lowered.starts.slew[k * 4..k * 4 + 4]);
        }
        for k in lowered.csr.in_ptr[v] as usize..lowered.csr.in_ptr[v + 1] as usize {
            let in_arc = lowered.csr.in_arcs[k];
            let cls = lowered.luts.classes[in_arc.class as usize];
            let raw = graph.arcs()[lowered.csr_arc_to_graph_arc[k] as usize];
            for ti in 0..2 {
                for to in 0..2 {
                    if !cls.allows(ti, to) {
                        continue;
                    }
                    let t = lib.thresholds.rc(to == 1);
                    for an in 0..2 {
                        let s = slews[in_arc.src as usize][ti * 2 + an];
                        if s == absent(an) {
                            continue;
                        }
                        let (d, q) = if let ArcKind::Net { net } = raw.kind {
                            let tau = wire.elmore(raw.dst, Tr::ALL[to], An::ALL[an]).ok_or_else(
                                || Diag::error("T0077", "extracted first moment absent"),
                            )?;
                            lowered.wires[k].delay[to * 2 + an] = tau;
                            rc::wire(tau, s, t, wire.primary_input(net))
                                .map_err(|e| Diag::error("T0077", e.to_string()))?
                        } else {
                            let load = match view.node_net(node) {
                                NetTarget::Net(net) => {
                                    wire.physical_load(net, Tr::ALL[to], An::ALL[an])
                                }
                                _ => None,
                            };
                            let eval = |c| {
                                let args = [s, c, c];
                                (
                                    lookup(
                                        &lowered.luts.luts,
                                        &lowered.luts.data,
                                        cls.delay[to],
                                        args,
                                    ),
                                    lookup(
                                        &lowered.luts.luts,
                                        &lowered.luts.data,
                                        cls.slew[to],
                                        args,
                                    ),
                                )
                            };
                            if let Some(DriverLoad::Pi { c_near, c_far, r }) = load {
                                let (d, q, _) = rc::gate(
                                    Pi {
                                        near: c_near,
                                        far: c_far,
                                        resistance: r,
                                    },
                                    t,
                                    eval,
                                )
                                .map_err(|e| Diag::error("T0077", e.to_string()))?;
                                (d, q)
                            } else {
                                let (d, q) = eval(lowered.load[v][to * 2 + an]);
                                (d, q.max(0.0))
                            }
                        };
                        let j = (ti * 2 + to) * 2 + an;
                        lowered.arc_values[k].delay[j] = d;
                        lowered.arc_values[k].slew[j] = q;
                        let dst = &mut slews[v][to * 2 + an];
                        if an == 0 && q < *dst || an == 1 && q > *dst {
                            *dst = q;
                        }
                    }
                }
            }
        }
    }
    lowered.lower_s += start.elapsed().as_secs_f64();
    Ok(())
}
