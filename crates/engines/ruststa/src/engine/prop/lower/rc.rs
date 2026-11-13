// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tag-independent f64 waveform preparation (STA-REQ-0723).
use super::*;
use rusteda_compute::rc::{self, Pi};
use rusteda_compute::timing::{ArcClass, ArcValues, lut::lookup};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

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

/// [`rc::wire_from_driver`] of a net wire after the driver waveform `(res, duration)` into `pi`,
/// taking the driver's root crossing `root` when it was solved for the same pi and output trip
/// point: the crossing is a function of them, the resistance and the duration alone, so taking it
/// changes no bit (`STA-REQ-0729`). Any other root is solved again.
fn wire_after_driver(
    pi: Pi,
    res: f64,
    duration: f64,
    root: Option<DriveRoot>,
    tau: f64,
    t: rc::Thresholds,
) -> Result<(f64, f64), ComputeError> {
    match root {
        Some(r) if r.solves(pi, t.output) => {
            rc::wire_from_driver_root(pi, res, duration, r.crossing, tau, t)
        }
        _ => rc::wire_from_driver(pi, res, duration, tau, t),
    }
}

/// The tag-independent RC values of every arc of `lowered` under `wire` (`STA-REQ-0723`), and,
/// with `wire_records`, the first moment of every extracted net arc in its wire record.
pub(super) fn prepare(
    lowered: &mut LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    max_bytes: u64,
    wire_records: bool,
) -> Result<(), Diag> {
    let extracted = wire.scope_name() == "spef_rc";
    if !extracted && !lib.has_mixed_thresholds() && lowered.derates.is_identity() {
        return Ok(());
    }
    let n = lowered.csr.nodes();
    let arcs = lowered.csr.in_arcs.len();
    // The arcs that a cone lowering left out are charged as if they were prepared, so that the
    // budgets decide, and report, as for the whole graph (STA-REQ-0713).
    let charged_arcs = (arcs as u64).saturating_add(lowered.pruned.arcs());
    // The sweep also holds, per node, its rows, its count of unfinished sources and its finished
    // wave, and one load entry per arc, while it runs.
    let bytes = (n as u64)
        .saturating_mul(32 + 96 + 56 + 128)
        .saturating_add(charged_arcs.saturating_mul(192));
    if bytes > max_bytes {
        return Err(Diag::error("T0023", "RC arc state budget exceeded"));
    }
    let charge_evaluations = rc::charge::Budget::default().max_evaluations as u64;
    let arc_work = |cls: &ArcClass| {
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
        let mut work = 0u64;
        for ti in 0..2 {
            for to in 0..2 {
                if cls.allows(ti, to) {
                    work = work.saturating_add(2 * evaluations);
                }
            }
        }
        work
    };
    // Every term is non-negative, so the saturating sum is the same in any order.
    let mut work = 0u64;
    for a in lowered.csr.in_arcs.iter() {
        work = work.saturating_add(arc_work(&lowered.luts.classes[a.class as usize]));
    }
    for &(flags, count) in lowered.pruned.by_flags() {
        let cls = ArcClass {
            flags,
            ..ArcClass::default()
        };
        work = work.saturating_add(arc_work(&cls).saturating_mul(count));
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
    // The preparation is a function of the CSR, the tables, the loads and the starts alone: a
    // previous preparation of this problem (the clock propagation prepares it again for every
    // clock and launch stage) is released before the next one is allocated, so that the two
    // never coexist (AGENTS rule 9).
    lowered.arc_values = Vec::new();
    lowered.wires = Vec::new();
    lowered.drives = Vec::new();
    let mut arc_values = vec![ArcValues::default(); arcs];
    // The arc values hold every delay and transition; the wire records only keep the first
    // moments for the callers that compare or report them.
    let mut wires = vec![WireRec::default(); if wire_records { arcs } else { 0 }];
    let shared: &LoweredCorner<'_> = lowered;
    let drives = if sources_precede(shared) {
        sweep_ready(shared, graph, view, wire, lib, &mut arc_values, &mut wires)?
    } else {
        sweep_in_order(shared, graph, view, wire, lib, &mut arc_values, &mut wires)?
    };
    lowered.arc_values = arc_values;
    lowered.wires = wires;
    lowered.drives = drives;
    lowered.lower_s += start.elapsed().as_secs_f64();
    Ok(())
}

/// Whether every lowered arc reads a node that precedes its destination in the topological order,
/// so that the finished state of a node's sources is final when the node is prepared.
fn sources_precede(lowered: &LoweredCorner<'_>) -> bool {
    let in_ptr = &lowered.csr.in_ptr;
    (0..lowered.csr.nodes()).all(|v| {
        lowered.csr.in_arcs[in_ptr[v] as usize..in_ptr[v + 1] as usize]
            .iter()
            .all(|arc| (arc.src as usize) < v)
    })
}

/// The finished waves of the sources that [`node_wave`] reads.
trait Sources {
    /// The worst slew, its driver waveform and that waveform's root crossing at `src` for
    /// [tr * 2 + an] = `j`.
    fn wave(&self, src: usize, j: usize) -> (f64, Option<(f64, f64)>, Option<DriveRoot>);
}

/// The waves of an in-order sweep: a node not yet swept reads as absent.
struct Swept<'s> {
    slews: &'s [[f64; 4]],
    drives: &'s [[Option<(f64, f64)>; 4]],
    /// `root_of[v]` indexes `roots` for a node with any root crossing.
    root_of: &'s [u32],
    roots: &'s [[Option<DriveRoot>; 4]],
}

impl Sources for Swept<'_> {
    fn wave(&self, src: usize, j: usize) -> (f64, Option<(f64, f64)>, Option<DriveRoot>) {
        let root = self
            .roots
            .get(self.root_of[src] as usize)
            .and_then(|r| r[j]);
        (self.slews[src][j], self.drives[src][j], root)
    }
}

/// The finished wave of one node in a dependency-ordered sweep.
#[derive(Debug)]
struct Wave {
    slew: [f64; 4],
    drive: [Option<(f64, f64)>; 4],
    /// The root crossings, only for a node with any.
    root: Option<Box<[Option<DriveRoot>; 4]>>,
}

impl Sources for [OnceLock<Wave>] {
    fn wave(&self, src: usize, j: usize) -> (f64, Option<(f64, f64)>, Option<DriveRoot>) {
        // A node starts only once all of its sources have finished (`sweep_ready`).
        let w = self[src]
            .get()
            .expect("the sources of a node finish before it starts");
        (w.slew[j], w.drive[j], w.root.as_ref().and_then(|r| r[j]))
    }
}

/// [`node_wave`] of every node in topological order, one at a time: the sweep when an arc reads a
/// node that does not precede its destination. Returns the driver waveforms of the nodes, or the
/// first error in that order.
fn sweep_in_order(
    lowered: &LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    arc_values: &mut [ArcValues],
    wires: &mut [WireRec],
) -> Result<Vec<[Option<(f64, f64)>; 4]>, Diag> {
    let n = lowered.csr.nodes();
    let in_ptr = &lowered.csr.in_ptr;
    let mut slews = vec![[absent(0), absent(1), absent(0), absent(1)]; n];
    // The driver resistance of the transition in `slews` (STA-REQ-0729).
    let mut drives = vec![[None; 4]; n];
    // The root crossings of the waveforms in `drives`, only while the waveforms are prepared:
    // `root_of[v]` indexes `roots` for a node with any.
    let mut root_of = vec![u32::MAX; n];
    let mut roots: Vec<[Option<DriveRoot>; 4]> = Vec::new();
    for v in 0..n {
        let (first, last) = (in_ptr[v] as usize, in_ptr[v + 1] as usize);
        let record = wires.get_mut(first..last).unwrap_or_default();
        let swept = Swept {
            slews: &slews,
            drives: &drives,
            root_of: &root_of,
            roots: &roots,
        };
        let root;
        (slews[v], drives[v], root) = node_wave(
            lowered,
            graph,
            view,
            wire,
            lib,
            v,
            &mut arc_values[first..last],
            record,
            &swept,
        )?;
        if root.iter().any(Option::is_some) {
            root_of[v] = roots.len() as u32;
            roots.push(root);
        }
    }
    Ok(drives)
}

/// What every task of [`sweep_ready`] shares.
struct Ready<'s, 'a> {
    lowered: &'s LoweredCorner<'a>,
    graph: &'s TimingGraph,
    view: &'s NetlistView<'s>,
    wire: &'s dyn WireModel,
    lib: &'s TimingLib,
    /// The rows of the arc tables of each node, taken by the one task that prepares it.
    rows: Vec<Mutex<(&'s mut [ArcValues], &'s mut [WireRec])>>,
    /// The number of arcs into each node whose source has not finished.
    pending: Vec<AtomicU32>,
    /// CSR offsets (`n + 1`) and destinations of the arcs out of each node.
    load_ptr: Vec<u32>,
    loads: Vec<u32>,
    waves: Vec<OnceLock<Wave>>,
    /// The failed node earliest in topological order, and its error.
    failed: Mutex<Option<(usize, Diag)>>,
}

impl Ready<'_, '_> {
    /// Prepares `v`, then each node that it leaves without unfinished sources: one in this task,
    /// the others in new tasks of `scope`.
    fn run<'scope>(&'scope self, scope: &rayon::Scope<'scope>, mut v: usize) {
        loop {
            let wave = {
                let mut rows = self.rows[v].lock().unwrap_or_else(PoisonError::into_inner);
                let (values, records) = &mut *rows;
                node_wave(
                    self.lowered,
                    self.graph,
                    self.view,
                    self.wire,
                    self.lib,
                    v,
                    values,
                    records,
                    self.waves.as_slice(),
                )
            };
            match wave {
                Ok((slew, drive, root)) => {
                    let root = root.iter().any(Option::is_some).then(|| Box::new(root));
                    let _ = self.waves[v].set(Wave { slew, drive, root });
                }
                Err(e) => {
                    // The nodes that read this one never start.
                    let mut failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
                    if failed.as_ref().is_none_or(|(first, _)| v < *first) {
                        *failed = Some((v, e));
                    }
                    return;
                }
            }
            let mut next = None;
            for &w in &self.loads[self.load_ptr[v] as usize..self.load_ptr[v + 1] as usize] {
                if self.pending[w as usize].fetch_sub(1, Ordering::AcqRel) == 1 {
                    if let Some(other) = next.replace(w as usize) {
                        scope.spawn(move |scope| self.run(scope, other));
                    }
                }
            }
            match next {
                Some(w) => v = w,
                None => return,
            }
        }
    }
}

/// [`node_wave`] of every node, each as soon as the last of its sources has finished, on the
/// rayon pool (`sources_precede` holds). A node reads only the final state of its sources, so the
/// values are those of the in-order sweep in every bit, whatever the schedule; there are no level
/// barriers at which threads wait for the slowest node of a level. Returns the driver waveforms
/// of the nodes, or the error of the failed node earliest in topological order: the error that
/// the in-order sweep returns, since every node before it, and every source of it, succeeds.
fn sweep_ready(
    lowered: &LoweredCorner<'_>,
    graph: &TimingGraph,
    view: &NetlistView<'_>,
    wire: &dyn WireModel,
    lib: &TimingLib,
    arc_values: &mut [ArcValues],
    wires: &mut [WireRec],
) -> Result<Vec<[Option<(f64, f64)>; 4]>, Diag> {
    let n = lowered.csr.nodes();
    let in_ptr = &lowered.csr.in_ptr;
    let in_arcs = &lowered.csr.in_arcs;
    let mut rows = Vec::with_capacity(n);
    let (mut values, mut records) = (arc_values, wires);
    for v in 0..n {
        let count = (in_ptr[v + 1] - in_ptr[v]) as usize;
        let (value, rest) = std::mem::take(&mut values).split_at_mut(count);
        values = rest;
        let (record, rest) = std::mem::take(&mut records).split_at_mut(count.min(records.len()));
        records = rest;
        rows.push(Mutex::new((value, record)));
    }
    let mut load_ptr = vec![0u32; n + 1];
    for arc in in_arcs.iter() {
        load_ptr[arc.src as usize + 1] += 1;
    }
    for v in 0..n {
        load_ptr[v + 1] += load_ptr[v];
    }
    let mut fill = load_ptr.clone();
    let mut loads = vec![0u32; in_arcs.len()];
    for v in 0..n {
        for arc in &in_arcs[in_ptr[v] as usize..in_ptr[v + 1] as usize] {
            let at = &mut fill[arc.src as usize];
            loads[*at as usize] = v as u32;
            *at += 1;
        }
    }
    drop(fill);
    let ready = Ready {
        lowered,
        graph,
        view,
        wire,
        lib,
        rows,
        pending: (0..n)
            .map(|v| AtomicU32::new(in_ptr[v + 1] - in_ptr[v]))
            .collect(),
        load_ptr,
        loads,
        waves: (0..n).map(|_| OnceLock::new()).collect(),
        failed: Mutex::new(None),
    };
    let starts: Vec<usize> = (0..n).filter(|&v| in_ptr[v] == in_ptr[v + 1]).collect();
    rayon::scope(|scope| {
        let ready = &ready;
        for chunk in starts.chunks(64) {
            scope.spawn(move |scope| {
                for &v in chunk {
                    ready.run(scope, v);
                }
            });
        }
    });
    let Ready { waves, failed, .. } = ready;
    if let Some((_, e)) = failed.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(e);
    }
    Ok(waves
        .into_iter()
        .map(|w| {
            w.into_inner()
                .expect("every node finishes when none fails")
                .drive
        })
        .collect())
}

/// The arcs into the node `v`, from the finished state of its sources. Writes only this node's
/// rows of the arc tables (`wires` empty without wire records) and returns its wave; no other
/// node reads them before this level ends.
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
    sources: &(impl Sources + ?Sized),
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
    for (offset, values) in arc_values.iter_mut().enumerate() {
        let k = first + offset;
        let mut record = wires.get_mut(offset);
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
                    let (s, drive, root) = sources.wave(in_arc.src as usize, ti * 2 + an);
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
                        drive,
                        root,
                    )?;
                    if let (Some(tau), Some(record)) = (value.tau, record.as_deref_mut()) {
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
    cls: &ArcClass,
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
            // point are the same.
            Some(((res, duration), pi)) => wire_after_driver(pi, res, duration, root, tau, t),
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
            root: p.root_crossing.map(|crossing| DriveRoot {
                pi,
                output: t.output,
                crossing,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn trips(output: f64) -> rc::Thresholds {
        rc::Thresholds {
            lower: 0.2,
            upper: 0.8,
            input: 0.5,
            output,
            derate: 1.0,
        }
    }

    // STA-REQ-0729: a net wire takes its driver's root crossing only when that crossing was
    // solved for its own pi and output trip point; for any other pi or trip point it solves the
    // crossing itself, so its value is the plain solve's bit for bit.
    #[test]
    fn a_wire_takes_only_the_root_crossing_of_its_own_pi() {
        let pi = Pi {
            near: 0.004,
            far: 0.011,
            resistance: 0.7,
        };
        let t = trips(0.5);
        // The driver waveform of a cell arc into `pi` and its root crossing, as the cell arc's
        // value records them.
        let gate = rc::charge::gate(pi, t, 1.3, |c| (0.02 + 2.0 * c, 0.03 + 4.0 * c)).unwrap();
        let (res, duration, tau) = (gate.resistance, gate.duration, 0.006);
        assert!(res > 0.0 && duration > 0.0);
        let own = DriveRoot {
            pi,
            output: t.output,
            crossing: gate.root_crossing,
        };
        let bits = |v: (f64, f64)| (v.0.to_bits(), v.1.to_bits());
        let plain = |pi: Pi, t: rc::Thresholds| {
            bits(rc::wire_from_driver(pi, res, duration, tau, t).unwrap())
        };
        let given = |pi: Pi, root: Option<DriveRoot>, t: rc::Thresholds| {
            bits(wire_after_driver(pi, res, duration, root, tau, t).unwrap())
        };
        assert_eq!(given(pi, Some(own), t), plain(pi, t));
        assert_eq!(given(pi, None, t), plain(pi, t));
        // A crossing of another pi, or of another trip point, is not taken.
        let other = DriveRoot {
            crossing: own.crossing * 1.5,
            ..own
        };
        let near = Pi {
            near: f64::from_bits(pi.near.to_bits() + 1),
            ..pi
        };
        for p in [
            near,
            Pi { far: 0.012, ..pi },
            Pi {
                resistance: 0.71,
                ..pi
            },
        ] {
            assert!(!other.solves(p, t.output));
            assert_eq!(given(p, Some(other), t), plain(p, t));
        }
        let t2 = trips(0.45);
        assert!(!other.solves(pi, t2.output));
        assert_eq!(given(pi, Some(other), t2), plain(pi, t2));
        // Taken, the wrong crossing would show: the check is what keeps the bits.
        assert_ne!(
            bits(rc::wire_from_driver_root(pi, res, duration, other.crossing, tau, t).unwrap()),
            plain(pi, t)
        );
    }
}
