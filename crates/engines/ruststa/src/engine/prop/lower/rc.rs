// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tag-independent f64 waveform preparation (STA-REQ-0723).
use super::*;
use rayon::prelude::*;
use rusteda_compute::rc::{self, Pi};
use rusteda_compute::timing::{ArcValues, lut::lookup};

/// The worst slew, its driver waveform and that waveform's root crossing at one node, per
/// [tr * 2 + an].
type NodeWave = ([f64; 4], [Option<(f64, f64)>; 4], [Option<DriveRoot>; 4]);

/// The root crossing of the output trip point of a driver waveform, with the pi load and the
/// output trip point it was solved for (`STA-REQ-0729`). A net arc of that driver whose pi and
/// output trip point are bit-identical takes it instead of solving the same crossing again.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DriveRoot {
    pi: Pi,
    output: f64,
    crossing: f64,
}

impl DriveRoot {
    /// Whether this crossing was solved for the pi `pi` and the output trip point `output`.
    fn solves(&self, pi: Pi, output: f64) -> bool {
        self.pi.near.to_bits() == pi.near.to_bits()
            && self.pi.far.to_bits() == pi.far.to_bits()
            && self.pi.resistance.to_bits() == pi.resistance.to_bits()
            && self.output.to_bits() == output.to_bits()
    }
}

pub(super) fn prepare(
    lowered: &mut LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    max_bytes: u64,
) -> Result<(), Diag> {
    let extracted = wire.scope_name() == "spef_rc";
    if !extracted && !lib.has_mixed_thresholds() && lowered.derates.is_identity() {
        return Ok(());
    }
    let n = lowered.csr.nodes();
    let arcs = lowered.csr.in_arcs.len();
    // The state of a level also holds one job and one result per node while it runs.
    let bytes = (n as u64)
        .saturating_mul(32 + 96 + 56 + 128)
        .saturating_add((arcs as u64).saturating_mul(192));
    if bytes > max_bytes {
        return Err(Diag::error("T0023", "RC arc state budget exceeded"));
    }
    let mut work = 0u64;
    let charge_evaluations = rc::charge::Budget::default().max_evaluations as u64;
    for a in lowered.csr.in_arcs.iter() {
        let cls = lowered.luts.classes[a.class as usize];
        // A wire: three bracketed sink crossings (64 doublings, 100 bisections, two cascades
        // each) and one root crossing (STA-REQ-0729); a cell arc: bounded charge callback work
        // (two table + five ramp evaluations each), one duration fit and three root crossings.
        // STA-REQ-0733, STA-REQ-0734. Keep the per-job work rejection before allocation.
        let evaluations = if !extracted {
            1
        } else if cls.is_wire() {
            3 * (64 + 100) * 2 + 80
        } else {
            7 * charge_evaluations + 56 * 48 * 2 + 3 * 80
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
    let mut arc_values = vec![ArcValues::default(); arcs];
    let mut wires = vec![WireRec::default(); arcs];
    let mut slews = vec![[absent(0), absent(1), absent(0), absent(1)]; n];
    // The driver resistance of the transition in `slews` (STA-REQ-0729).
    let mut drives = vec![[None; 4]; n];
    // The root crossings of the waveforms in `drives`, only while the waveforms are prepared:
    // `root_of[v]` indexes `roots` for a node with any.
    let mut root_of = vec![u32::MAX; n];
    let mut roots: Vec<[Option<DriveRoot>; 4]> = Vec::new();
    let shared: &LoweredCorner<'_> = lowered;
    let spans = independent_spans(shared, graph, n);
    for &(begin, end) in &spans {
        let in_ptr = &shared.csr.in_ptr;
        let (first, last) = (in_ptr[begin] as usize, in_ptr[end] as usize);
        let (mut values, mut records) = (&mut arc_values[first..last], &mut wires[first..last]);
        let mut jobs = Vec::with_capacity(end - begin);
        for v in begin..end {
            let count = (in_ptr[v + 1] - in_ptr[v]) as usize;
            let (value, rest) = std::mem::take(&mut values).split_at_mut(count);
            values = rest;
            let (record, rest) = std::mem::take(&mut records).split_at_mut(count);
            records = rest;
            jobs.push((v, value, record));
        }
        let (read_slews, read_drives) = (&slews, &drives);
        let read_roots = (root_of.as_slice(), roots.as_slice());
        let done: Vec<Result<NodeWave, Diag>> = jobs
            .into_par_iter()
            .with_min_len(16)
            .map(|(v, value, record)| {
                node_wave(
                    shared,
                    graph,
                    view,
                    wire,
                    lib,
                    v,
                    value,
                    record,
                    read_slews,
                    read_drives,
                    read_roots,
                )
            })
            .collect();
        // In order, so the first error of the topological sweep is the one reported.
        for (v, wave) in (begin..end).zip(done) {
            let root;
            (slews[v], drives[v], root) = wave?;
            if root.iter().any(Option::is_some) {
                root_of[v] = roots.len() as u32;
                roots.push(root);
            }
        }
    }
    drop((root_of, roots));
    lowered.arc_values = arc_values;
    lowered.wires = wires;
    lowered.drives = drives;
    lowered.lower_s += start.elapsed().as_secs_f64();
    Ok(())
}

/// Ranges of `0..n` whose nodes read only earlier ranges: the levels when every lowered arc
/// rises in level, otherwise one node per range, which is the topological sweep.
fn independent_spans(
    lowered: &LoweredCorner<'_>,
    graph: &TimingGraph,
    n: usize,
) -> Vec<(usize, usize)> {
    let starts = graph.levels().level_start();
    let spans: Vec<(usize, usize)> = starts
        .windows(2)
        .map(|w| (w[0] as usize, w[1] as usize))
        .collect();
    let in_ptr = &lowered.csr.in_ptr;
    let rising = starts.first() == Some(&0)
        && starts.last().map(|&end| end as usize) == Some(n)
        && spans.iter().all(|&(begin, end)| {
            (begin..end).all(|v| {
                lowered.csr.in_arcs[in_ptr[v] as usize..in_ptr[v + 1] as usize]
                    .iter()
                    .all(|arc| (arc.src as usize) < begin)
            })
        });
    if rising {
        spans
    } else {
        (0..n).map(|v| (v, v + 1)).collect()
    }
}

/// The arcs into the node `v`, from the finished state of its sources. Writes only this node's
/// rows of the arc tables and returns its wave; no other node reads them before this level ends.
#[allow(clippy::too_many_arguments)]
fn node_wave(
    lowered: &LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    v: usize,
    arc_values: &mut [ArcValues],
    wires: &mut [WireRec],
    slews: &[[f64; 4]],
    drives: &[[Option<(f64, f64)>; 4]],
    (root_of, roots): (&[u32], &[[Option<DriveRoot>; 4]]),
) -> Result<NodeWave, Diag> {
    let node = graph.order()[v];
    let mut own_slew = [absent(0), absent(1), absent(0), absent(1)];
    let mut own_drive = [None; 4];
    let mut own_root = [None; 4];
    if let Some(k) = lowered.starts.find(v as u32) {
        own_slew.copy_from_slice(&lowered.starts.slew[k * 4..k * 4 + 4]);
        if let Some(d) = lowered.start_drives.get(&node) {
            own_drive = *d;
        }
    }
    let first = lowered.csr.in_ptr[v] as usize;
    for (offset, (values, record)) in arc_values.iter_mut().zip(wires.iter_mut()).enumerate() {
        let k = first + offset;
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
                    let root = roots
                        .get(root_of[in_arc.src as usize] as usize)
                        .and_then(|r| r[ti * 2 + an]);
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
                        root,
                    )?;
                    if let Some(tau) = value.tau {
                        record.delay[to * 2 + an] = tau;
                    }
                    let j = (ti * 2 + to) * 2 + an;
                    let q = value.slew;
                    values.delay[j] = value.delay * lowered.derates.for_bound(An::ALL[an]);
                    values.slew[j] = q;
                    let dst = &mut own_slew[to * 2 + an];
                    if an == 0 && q < *dst || an == 1 && q > *dst {
                        *dst = q;
                        own_drive[to * 2 + an] = value.drive;
                        own_root[to * 2 + an] = value.root;
                    }
                }
            }
        }
    }
    Ok((own_slew, own_drive, own_root))
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
    /// The root crossing of `drive`.
    pub root: Option<DriveRoot>,
}

/// The value of the arc `raw` with the class `cls` for the input transition `s`, the output
/// transition `to` and the bound `an` under the extracted wire model `wire` (`STA-REQ-0723`).
/// `lumped_load` is the load of the arc's destination when its net has no extracted driver load;
/// `drive` is the driver waveform of the transition `s` at a net arc's source and `root`, when
/// known, the root crossing that waveform was solved with. The only
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
    root: Option<DriveRoot>,
) -> Result<RcArcValue, Diag> {
    let (tr, bound) = (Tr::ALL[to], An::ALL[an]);
    if let ArcKind::Net { net } = raw.kind {
        let source = lib.node_thresholds(view, raw.src);
        let destination = lib.node_thresholds(view, raw.dst);
        let converted_slew = source.convert_slew(destination, s, to == 1);
        if wire.scope_name() != "spef_rc" {
            let value = wire.load_delay(net, raw.dst, tr, bound, converted_slew);
            return Ok(RcArcValue {
                delay: value.delay + source.net_reference_shift(destination, s, to == 1),
                slew: value.slew,
                tau: None,
                drive: None,
                root: None,
            });
        }
        // RC keeps the driver waveform's slew convention. NLDM presents its
        // resulting abstract slew to the receiving library by interval/derate
        // conversion; remeasuring a curved RC waveform at the receiver's slew
        // thresholds would change the characterized-ramp model at a boundary.
        let mut t = source.rc(to == 1);
        t.input = destination.rc(to == 1).input;
        let tau = wire
            .elmore(raw.dst, tr, bound)
            .ok_or_else(|| Diag::error("T0077", "extracted first moment absent"))?;
        let primary = wire.primary_input(net);
        let pi = match (drive, wire.physical_load(net, tr, bound)) {
            (Some(drive), Some(DriverLoad::Pi { c_near, c_far, r })) if r > 0.0 => Some((
                drive,
                Pi {
                    near: c_near,
                    far: c_far,
                    resistance: r,
                },
            )),
            _ => None,
        };
        let (d, q) = match pi {
            // The driver solved this net's root crossing already when its pi and output trip
            // point are the same: the crossing is a function of them, the resistance and the
            // duration alone, so taking it changes no bit (`STA-REQ-0729`).
            Some(((res, duration), pi)) => match root {
                Some(r) if r.solves(pi, t.output) => {
                    rc::wire_from_driver_root(pi, res, duration, r.crossing, tau, t)
                }
                _ => rc::wire_from_driver(pi, res, duration, tau, t),
            },
            None => rc::wire(tau, s, t, primary),
        }
        .map_err(|e| Diag::error("T0077", e.to_string()))?;
        return Ok(RcArcValue {
            delay: d,
            slew: source.convert_slew(destination, q, to == 1),
            tau: Some(tau),
            drive: None,
            root: None,
        });
    }
    let t = lib.node_thresholds(view, raw.dst).rc(to == 1);
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
        let pi = Pi {
            near: c_near,
            far: c_far,
            resistance: r,
        };
        let p = crate::engine::delay::driver::pi_drive(pi, t, eval)
            .map_err(|e| Diag::error("T0077", e.to_string()))?;
        Ok(RcArcValue {
            delay: p.delay,
            slew: p.slew,
            tau: None,
            drive: p.drive,
            root: p.drive.map(|_| DriveRoot {
                pi,
                output: t.output,
                crossing: p.root_crossing,
            }),
        })
    } else {
        let (d, q) = eval(lumped_load);
        Ok(RcArcValue {
            delay: d,
            slew: q.max(0.0),
            tau: None,
            drive: None,
            root: None,
        })
    }
}
