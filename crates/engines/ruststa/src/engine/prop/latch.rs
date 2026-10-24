// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Latches: the data-to-output arc as a tag-mapping arc (owner: E5).
//!
//! Engine section C. Data passing a transparent latch re-launches in the tag of the enable's
//! opening edge with the time it borrowed, the opening and closing edges taken from the same
//! enable cycle, the setup uncertainty applied at the opening edge, and the limit of
//! `set_max_time_borrow`.

use rustc_hash::FxHashMap;
use rusteda_compute::timing::{ArcClass, LutDesc, NONE, TagMapRec};
use rusteda_core::job::Diag;

use super::clocks::ideal::IdealClockModel;
use super::clocks::{ClockModel, SENSE_NEG, SENSE_POS};
use super::cppr::Cppr;
use super::exceptions::{AnalysisKind, ExceptionContext, PathAnalysisResolution};
use super::lower::{ArcTagMaps, LoweredCorner};
use super::relation::latch_window_relations;
use super::tags::{Tag, TagTable};
use crate::engine::delay::load::NetLoads;
use crate::engine::delay::wire::WireModel;
use crate::engine::graph::{ArcKind, CheckArc, TimingGraph};
use crate::engine::input::NetlistView;
use crate::engine::input::constr::{CompiledConstraints, MaxTimeBorrowTarget};
use crate::engine::input::libview::TimingLib;
use crate::engine::types::{An, ClockId, NodeId, Tr};

/// Value of `TagMapRec::open` that rejects all arrivals for a bound (`< f64::INFINITY` and finite
/// in `f32` for `COMPUTE-REQ-0020`).
const CLOSED_WINDOW_OPEN: f64 = 1.0e30;

/// Evaluated transparency window of a latch for one `(launch_tag, capture_clock, sense)` pair
/// (`STA-REQ-0221`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatchWindow {
    /// Opening edge of the capture clock's waveform (`Rise` or `Fall`).
    pub e_open: Tr,
    /// Closing edge of the capture clock's waveform (`Rise` or `Fall`).
    pub e_close: Tr,
    /// Opening relation `D_open = D_close - W_pulse` (ns).
    pub d_open: f64,
    /// Closing setup relation `D_close = D_s(L, e_L, C, e_close) > 0` (ns).
    pub d_close: f64,
    /// Active enable pulse width `W_pulse in (0, T_C]` (ns).
    pub w_pulse: f64,
    /// Earliest opening window boundary `R_open = D_open + lat(C, e_open, Early) - u_s + cppr` (ns).
    pub r_open: f64,
    /// Closing boundary before subtracting the setup check table value:
    /// `D_close + lat(C, e_close, Early) - u_s + cppr` (ns).
    pub r_close_unsetup: f64,
    /// Clock slew at the closing edge in `An::Early` (ns).
    pub close_slew_early: f64,
    /// Clock slew at the closing edge in `An::Late` (ns), used as `related_pin_transition` of the
    /// latch setup check table (`STA-REQ-0229`).
    pub close_slew_late: f64,
    /// Optional `set_max_time_borrow` limit `B_max` (ns).
    pub max_borrow: Option<f64>,
}

impl LatchWindow {
    /// The window with its opening and closing edges moved by `shift_ns`: a multicycle path that
    /// wins the setup analysis of the latch data pin moves both, and with them the borrowing
    /// (`STA-REQ-0267`).
    #[must_use]
    pub fn shifted(self, shift_ns: f64) -> Self {
        Self {
            d_open: self.d_open + shift_ns,
            d_close: self.d_close + shift_ns,
            r_open: self.r_open + shift_ns,
            r_close_unsetup: self.r_close_unsetup + shift_ns,
            ..self
        }
    }

    /// Computes the closing window boundary `R_close` (ns) after subtracting the setup check table
    /// value `setup_time_ns` and clamping to `R_open + B_max` when `set_max_time_borrow` applies
    /// (`STA-REQ-0221`).
    #[inline]
    #[must_use]
    pub fn r_close(&self, setup_time_ns: f64) -> f64 {
        let raw = self.r_close_unsetup - setup_time_ns;
        match self.max_borrow {
            Some(b_max) => raw.min(self.r_open + b_max),
            None => raw,
        }
    }

    /// Evaluates `(required_ns, slack_ns, borrowed_ns)` for a late data arrival `arr_ns` and setup
    /// check table value `setup_time_ns` under the time-borrowing convention (`STA-REQ-0221`).
    #[inline]
    #[must_use]
    pub fn eval_setup(&self, arr_ns: f64, setup_time_ns: f64) -> (f64, f64, f64) {
        let r_open = self.r_open;
        let r_close = self.r_close(setup_time_ns);
        let max_b = (r_close - r_open).max(0.0);
        let borrowed = (arr_ns - r_open).max(0.0).min(max_b);
        if arr_ns <= r_open {
            (r_open, r_open - arr_ns, 0.0)
        } else if arr_ns <= r_close {
            (arr_ns, 0.0, borrowed)
        } else {
            (r_close, r_close - arr_ns, borrowed)
        }
    }
}

/// Finds the opening pin transition on `ch.clock_pin` for a latch setup check `ch` if `ch` belongs
/// to a transparent latch (`STA-REQ-0221`, `STA-REQ-0222`).
#[must_use]
pub fn latch_open_pin_edge(graph: &TimingGraph, check_idx: usize, ch: &CheckArc) -> Option<Tr> {
    if !ch.kind.is_setup() {
        return None;
    }
    let has_latch_data = graph.arcs().iter().any(|a| {
        matches!(
            a.kind,
            ArcKind::LatchData {
                inst,
                setup_check,
                ..
            } if inst == ch.inst && setup_check as usize == check_idx
        )
    });
    if !has_latch_data {
        return None;
    }
    let edge = graph
        .launches()
        .iter()
        .find(|l| l.inst == ch.inst && l.clock_pin == ch.clock_pin)
        .map_or_else(|| ch.edge.opposite(), |l| l.edge);
    Some(edge)
}

/// Resolves the `set_max_time_borrow` limit (ns) for a latch instance / pin / capture clock,
/// if any (`STA-REQ-0221`).
#[must_use]
pub fn resolve_max_time_borrow(
    constraints: &CompiledConstraints,
    inst: u32,
    data_pin: NodeId,
    clock_pin: NodeId,
    out_pin: Option<NodeId>,
    capture_clock: ClockId,
) -> Option<f64> {
    let mut matched = constraints.latch_max_borrow_ns.get(&inst).copied();
    for spec in &constraints.max_time_borrow {
        let applies = match spec.target {
            MaxTimeBorrowTarget::Clock(cid) => cid == capture_clock,
            MaxTimeBorrowTarget::Cell(i) => i == inst,
            MaxTimeBorrowTarget::Pin(n) => {
                n == data_pin || n == clock_pin || out_pin.is_some_and(|q| n == q)
            }
        };
        if applies {
            matched = Some(spec.max_borrow_ns);
        }
    }
    matched
}

/// Computes the [`LatchWindow`] for a latch setup check `ch` with opening pin transition
/// `open_pin_edge`, launch tag `launch_tag`, capture clock `capture_clock`, and clock network
/// polarity `negative_sense` (`false` for positive sense, `true` for negative sense;
/// `STA-REQ-0221`).
#[allow(clippy::too_many_arguments)]
pub fn compute_latch_window<C: Cppr>(
    constraints: &CompiledConstraints,
    clock_model: &IdealClockModel,
    cppr_model: &C,
    ch: &CheckArc,
    open_pin_edge: Tr,
    out_pin: Option<NodeId>,
    launch_tag: Tag,
    capture_clock: ClockId,
    negative_sense: bool,
) -> Result<Option<LatchWindow>, Diag> {
    let e_close = if negative_sense {
        ch.edge.opposite()
    } else {
        ch.edge
    };
    let e_open = if negative_sense {
        open_pin_edge.opposite()
    } else {
        open_pin_edge
    };
    let Some(open_arr) = clock_model.arrival(
        capture_clock,
        e_open,
        ch.clock_pin,
        open_pin_edge,
        An::Early,
    ) else {
        return Ok(None);
    };
    let Some(close_arr) =
        clock_model.arrival(capture_clock, e_close, ch.clock_pin, ch.edge, An::Early)
    else {
        return Ok(None);
    };
    let close_slew_late = clock_model
        .arrival(capture_clock, e_close, ch.clock_pin, ch.edge, An::Late)
        .map_or(close_arr.slew, |a| a.slew);

    let ck_l = &constraints.clocks[launch_tag.clock.index()];
    let ck_c = &constraints.clocks[capture_clock.index()];
    let (d_open, d_close, w_pulse) =
        latch_window_relations(ck_l, launch_tag.edge, ck_c, e_open, e_close)?;

    let u_s = clock_model.uncertainty(
        constraints,
        launch_tag.clock,
        launch_tag.edge,
        capture_clock,
        e_close,
        Some(ch.data_pin),
        true,
    );
    let cppr = if launch_tag.clock == capture_clock {
        cppr_model.credit(capture_clock, ch.clock_pin, ch.clock_pin)
    } else {
        0.0
    };

    let r_open = d_open + open_arr.latency - u_s + cppr;
    let r_close_unsetup = d_close + close_arr.latency - u_s + cppr;
    let max_borrow = resolve_max_time_borrow(
        constraints,
        ch.inst,
        ch.data_pin,
        ch.clock_pin,
        out_pin,
        capture_clock,
    );

    Ok(Some(LatchWindow {
        e_open,
        e_close,
        d_open,
        d_close,
        w_pulse,
        r_open,
        r_close_unsetup,
        close_slew_early: close_arr.slew,
        close_slew_late,
        max_borrow,
    }))
}

/// Builds the [`ArcTagMaps`] for all enabled latch `D -> Q` arcs in `graph` (`STA-REQ-0221`,
/// `STA-REQ-0222`).
///
/// Each record maps `src_tag = (L, e_L, exc)` at `D` to `dst_tag = (C, e_open, exc')` at `Q` with:
/// - `shift = -D_open` (converting the time origin from `t(e_L)` to `t(e_open)`),
/// - `open = [CLOSED_WINDOW_OPEN, (R_open + shift).next_up()]` so only `Late` arrivals that
///   strictly borrow (`a(D, Late) > R_open`) traverse `D -> Q`,
/// - `close = [f64::INFINITY, r_close_unsetup + shift]` with `close_slew = [0.0, slew(C, e_close, Late)]`,
/// - `limit = [f64::INFINITY, R_open + B_max + shift]` when `set_max_time_borrow` applies.
///
/// With timing exceptions (`STA-REQ-0267`), a data arrival passes `D -> Q` only in the setup
/// analysis it is timed in: a false path or a clock group passes none, a multicycle path moves the
/// window, and a maximum delay checks the pin like a flip-flop pin and passes no borrowed time. The
/// relaunched paths start at the enable pin, so `exc'` is the `-from` state of that pin.
pub fn build_latch_tag_maps<C: Cppr>(
    graph: &TimingGraph,
    constraints: &CompiledConstraints,
    clock_model: &IdealClockModel,
    cppr_model: &C,
    tags: &TagTable,
    exceptions: &mut ExceptionContext,
) -> Result<ArcTagMaps, Diag> {
    let mut tag_maps = ArcTagMaps::new();
    if tags.is_empty() {
        return Ok(tag_maps);
    }
    let g_arcs = graph.arcs();
    let g_checks = graph.checks();
    let g_launches = graph.launches();

    for (ai, arc) in g_arcs.iter().enumerate() {
        if graph.is_arc_disabled(ai) {
            continue;
        }
        let ArcKind::LatchData {
            inst, setup_check, ..
        } = arc.kind
        else {
            continue;
        };
        let Some(ch) = g_checks.get(setup_check as usize) else {
            continue;
        };
        let open_pin_edge = g_launches
            .iter()
            .find(|l| l.inst == inst && l.clock_pin == ch.clock_pin && l.out_pin == arc.dst)
            .map_or_else(|| ch.edge.opposite(), |l| l.edge);

        let mut recs = Vec::new();
        for &(capture_clock, bits) in clock_model.pin_clocks(ch.clock_pin) {
            for (sense_bit, neg_sense) in [(SENSE_POS, false), (SENSE_NEG, true)] {
                if bits & sense_bit == 0 {
                    continue;
                }
                for (src_tid, src_tag) in tags.iter() {
                    let Some(win) = compute_latch_window(
                        constraints,
                        clock_model,
                        cppr_model,
                        ch,
                        open_pin_edge,
                        Some(arc.dst),
                        src_tag,
                        capture_clock,
                        neg_sense,
                    )?
                    else {
                        continue;
                    };
                    let win = if exceptions.is_active() {
                        let res = Tr::ALL.map(|tr| {
                            exceptions.resolve(
                                src_tag,
                                ch.data_pin,
                                tr,
                                (capture_clock, win.e_close),
                                AnalysisKind::Setup,
                            )
                        });
                        if res[0] != res[1] {
                            return Err(Diag::error(
                                "T0073",
                                format!(
                                    "timing exceptions resolve differently for the rising and the \
                                     falling data at latch data pin node {}; this version does not \
                                     analyse such latch paths (STA-REQ-0267)",
                                    ch.data_pin.0
                                ),
                            ));
                        }
                        if matches!(res[0], PathAnalysisResolution::TimedMaxDelay { .. }) {
                            continue;
                        }
                        let Some(shift) = ExceptionContext::relation_shift(
                            res[0],
                            win.d_close,
                            constraints.clocks[src_tag.clock.index()].period_ns,
                            constraints.clocks[capture_clock.index()].period_ns,
                        ) else {
                            continue;
                        };
                        if shift != 0.0 {
                            win.shifted(shift)
                        } else {
                            win
                        }
                    } else {
                        win
                    };
                    let Some(dst_tid) = exceptions.start_tag(
                        tags,
                        ch.clock_pin,
                        open_pin_edge,
                        capture_clock,
                        win.e_open,
                    ) else {
                        continue;
                    };
                    let shift = -win.d_open;
                    let open_late = (win.r_open + shift).next_up();
                    let close_late = win.r_close_unsetup + shift;
                    let limit_late = win
                        .max_borrow
                        .map_or(f64::INFINITY, |b_max| win.r_open + b_max + shift);

                    recs.push(TagMapRec {
                        src_tag: src_tid.0,
                        dst_tag: dst_tid.0,
                        shift,
                        open: [CLOSED_WINDOW_OPEN, open_late],
                        close: [f64::INFINITY, close_late],
                        close_slew: [0.0, win.close_slew_early],
                        limit: [f64::INFINITY, limit_late],
                    });
                }
            }
        }
        if !recs.is_empty() {
            tag_maps.insert(ai as u32, recs);
        } else {
            // Unclocked latch enable: block D -> Q from passing arrivals.
            tag_maps.insert(
                ai as u32,
                vec![TagMapRec {
                    src_tag: 0,
                    dst_tag: 0,
                    shift: 0.0,
                    open: [CLOSED_WINDOW_OPEN, CLOSED_WINDOW_OPEN],
                    close: [f64::INFINITY, f64::INFINITY],
                    close_slew: [0.0, 0.0],
                    limit: [f64::INFINITY, f64::INFINITY],
                }],
            );
        }
    }

    // A latch that a constant active enable holds transparent passes the late arrivals of every
    // tag through `D -> Q` and no early one: no hold path passes a latch (`STA-REQ-0161`).
    for &ai in graph.transparent_latch_arcs() {
        if graph.is_arc_disabled(ai as usize) {
            continue;
        }
        let recs = (0..tags.len() as u32)
            .map(|t| TagMapRec {
                src_tag: t,
                dst_tag: t,
                shift: 0.0,
                open: [CLOSED_WINDOW_OPEN, f64::NEG_INFINITY],
                close: [f64::INFINITY, f64::INFINITY],
                close_slew: [0.0, 0.0],
                limit: [f64::INFINITY, f64::INFINITY],
            })
            .collect();
        tag_maps.insert(ai, recs);
    }

    Ok(tag_maps)
}

/// Lowers the problem of one corner onto the compute timing task: the graph with its start rows
/// and latch tag maps, the latch `G -> Q` transitions pinned on the latch `D -> Q` arcs
/// ([`pin_latch_gq_slews`]) and the prepared wire model (`STA-REQ-0222`, `STA-REQ-0401`).
///
/// The analysis and the backward pass of the timing table lower a corner only through this
/// function, so that a table built from a kept lowered problem and one built from a problem
/// lowered again read the same arc values and are byte-identical (`STA-REQ-0806`).
#[allow(clippy::too_many_arguments)]
pub fn lower_corner<'l>(
    netview: &NetlistView<'_>,
    graph: &TimingGraph,
    lib: &'l TimingLib,
    net_loads: &NetLoads,
    ntags: u32,
    starts: rusteda_compute::timing::StartRows,
    launch_slews: &FxHashMap<NodeId, [f64; 4]>,
    latch_maps: &ArcTagMaps,
    wire_model: &dyn WireModel,
    max_state_bytes: u64,
) -> Result<LoweredCorner<'l>, Diag> {
    let mut lowered =
        LoweredCorner::lower(netview, graph, lib, net_loads, ntags, starts, latch_maps)?;
    pin_latch_gq_slews(graph, launch_slews, &mut lowered);
    lowered.set_wire_model(graph, netview, wire_model, lib, max_state_bytes)?;
    Ok(lowered)
}

/// Pins the output slew tables of enabled latch `D -> Q` arcs in `lowered` to the latch output
/// `Q`'s `G -> Q` slews whenever `G -> Q` is present on that latch (`STA-REQ-0222`).
///
/// Because `Q` is initialized by `StartRows` with `[s_RE, s_RL, s_FE, s_FL]` from `G -> Q` where
/// `s_RE <= s_RL` and `s_FE <= s_FL`, replacing `ArcClass::slew[to]` on the `D -> Q` arc with a
/// scalar LUT of value `s_Early(to)` ensures that neither `Early` (`q < s_RE`) nor `Late`
/// (`q > s_RL`) overwrites `Q`'s `G -> Q` slew during level propagation on CPU or GPU.
pub fn pin_latch_gq_slews(
    graph: &TimingGraph,
    launch_slews: &FxHashMap<NodeId, [f64; 4]>,
    lowered: &mut LoweredCorner<'_>,
) {
    let g_arcs = graph.arcs();
    for (ai, arc) in g_arcs.iter().enumerate() {
        if !arc.kind.is_latch_data() || graph.is_arc_disabled(ai) {
            continue;
        }
        let csr_idx = lowered
            .graph_arc_to_csr_arc
            .get(ai)
            .copied()
            .unwrap_or(NONE);
        if csr_idx == NONE {
            continue;
        }
        let Some(&gq_slew) = launch_slews.get(&arc.dst) else {
            continue;
        };
        let old_class_idx = lowered.csr.in_arcs[csr_idx as usize].class as usize;
        let mut new_class: ArcClass = lowered.luts.classes[old_class_idx];
        let mut modified = false;
        for to in 0..2 {
            let s_early = gq_slew[to * 2];
            let s_late = gq_slew[to * 2 + 1];
            if s_early.is_finite() && s_late.is_finite() && s_early <= s_late {
                let voff = lowered.luts.data.len() as u32;
                lowered.luts.data.to_mut().push(s_early);
                let lut_idx = lowered.luts.luts.len() as u32;
                lowered.luts.luts.to_mut().push(LutDesc::scalar(voff));
                new_class.slew[to] = lut_idx;
                modified = true;
            }
        }
        if modified {
            let new_class_idx = lowered.luts.classes.len() as u32;
            lowered.luts.classes.to_mut().push(new_class);
            lowered.csr.in_arcs.to_mut()[csr_idx as usize].class = new_class_idx;
        }
    }
}
