// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Minimum pulse width and minimum period of clock pins (`STA-REQ-0800` to `STA-REQ-0802`).
//!
//! A clock `C` reaches a pin `p` with sense bits; its edge `e` of waveform time `t(e)` arrives at
//! `p` as transition `tr` with latency `L(e, tr, an)` and transition time `s(e, tr, an)`
//! ([`ClockModel::arrival`]). The high pulse at `p` opens with a rising transition, from the source
//! edge `e_o` that produces it, and closes with the following falling transition, from the next
//! source edge `e_c = opposite(e_o)` (`t(e_c)` is moved one period later when it does not follow
//! `t(e_o)`). Its width is taken with the latest opening and the earliest closing arrival,
//! `W = (t(e_c) + L(e_c, fall, early)) - (t(e_o) + L(e_o, rise, late))`; the low pulse swaps rise and
//! fall. The required width is the largest `rise_constraint` (high) or `fall_constraint` (low) of
//! the pin's `min_pulse_width` groups at the opening transition time `s(e_o, ., late)`; the slack
//! is `W - required`, the worst over the opening edges. The period check compares the clock period
//! with the largest constraint of the pin's `minimum_period` groups (each table at the transition
//! time of its own transition) and its slack is `period - required`. Clock uncertainty and CPPR do
//! not enter these checks.

use crate::engine::delay::lut::RangeTracker;
use crate::engine::input::libview::{PulseCheck, PulseCheckKind, TimingCell};
use crate::engine::prop::clocks::{ClockModel, source_edges_for_pin_tr};
use crate::engine::types::{An, ClockId, NodeId, Tr};

use super::{CornerLimits, LimitInputs, LimitKind, LimitRow, LimitSource, RowCollector};

/// The pulse checks of each pin of a cell, by cell pin of the netlist cell.
type CellPulseChecks<'l> = Option<(&'l TimingCell, Vec<Vec<&'l PulseCheck>>)>;

pub(super) fn check_pulses(
    inputs: &LimitInputs<'_>,
    tracker: &mut RangeTracker,
    rows: &mut RowCollector,
    out: &mut CornerLimits,
) {
    let nv = inputs.netview;
    let frozen = nv.frozen();
    // The pulse checks of every pin of every netlist cell, looked up once per cell.
    let per_cell: Vec<CellPulseChecks<'_>> = (0..frozen.cells.len() as u32)
        .map(|cell_row| {
            let tc = inputs.lib.cell(nv.cell_name(cell_row))?;
            if tc.pulse_checks.is_empty() {
                return None;
            }
            let n_cp = frozen.cell_pins(cell_row).len() as u32;
            let pins = (0..n_cp)
                .map(|cp| {
                    tc.pin(nv.cell_pin_name(cell_row, cp))
                        .map(|lib_pin| {
                            tc.pulse_checks
                                .iter()
                                .filter(|c| c.pin == lib_pin)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect();
            Some((tc, pins))
        })
        .collect();

    for inst in 0..frozen.instances.len() as u32 {
        let cell_row = frozen.instances[inst as usize].cell as usize;
        let Some((_, pins)) = &per_cell[cell_row] else {
            continue;
        };
        for pin_row in frozen.instance_pins(inst) {
            let cp = frozen.pin_cellpin[pin_row] as usize;
            let Some(checks) = pins.get(cp).filter(|c| !c.is_empty()) else {
                continue;
            };
            let node = nv.pin_row_node(pin_row as u32);
            let clocks = inputs.clock_model.pin_clocks(node);
            if clocks.is_empty() {
                out.unclocked_pulse_pins += 1;
                continue;
            }
            out.conditional_pulse_groups += checks.iter().filter(|c| c.conditional).count() as u64;
            if clocks.iter().any(|&(clock, _)| {
                inputs
                    .constraints
                    .clocks
                    .get(clock.index())
                    .is_some_and(|ck| !ck.propagated)
            }) {
                out.ideal_clock_pulse_pins += 1;
            }
            for &(clock, bits) in clocks {
                let Some(ck) = inputs.constraints.clocks.get(clock.index()) else {
                    continue;
                };
                for pulse_tr in Tr::ALL {
                    if let Some(row) =
                        width_row(inputs, tracker, checks, node, clock, bits, pulse_tr, ck)
                    {
                        rows.push(row);
                    }
                }
                if let Some(row) = period_row(inputs, tracker, checks, node, clock, bits, ck) {
                    rows.push(row);
                }
            }
        }
    }
}

/// The source edges of `ck` that produce transition `tr` at a pin reached with `bits`.
fn opening_edges(tr: Tr, bits: u8) -> Vec<Tr> {
    source_edges_for_pin_tr(tr, bits)
}

/// The largest constraint of the `kind` checks for `tr` at transition time `slew`, if any.
fn required(
    inputs: &LimitInputs<'_>,
    tracker: &mut RangeTracker,
    checks: &[&PulseCheck],
    kind: PulseCheckKind,
    tr: Tr,
    slew: f64,
) -> Option<f64> {
    checks
        .iter()
        .filter(|c| c.kind == kind)
        .filter_map(|c| inputs.lib.eval_pulse(c, tr, slew, tracker))
        .reduce(f64::max)
}

#[allow(clippy::too_many_arguments)]
fn width_row(
    inputs: &LimitInputs<'_>,
    tracker: &mut RangeTracker,
    checks: &[&PulseCheck],
    node: NodeId,
    clock: ClockId,
    bits: u8,
    pulse_tr: Tr,
    ck: &crate::engine::input::constr::CompiledClock,
) -> Option<LimitRow> {
    let period = ck.period_ns;
    let wave = |e: Tr| ck.waveform_ns[e.index()];
    let mut best: Option<LimitRow> = None;
    for e_open in opening_edges(pulse_tr, bits) {
        let e_close = e_open.opposite();
        let Some(open) = inputs
            .clock_model
            .arrival(clock, e_open, node, pulse_tr, An::Late)
        else {
            continue;
        };
        let Some(close) =
            inputs
                .clock_model
                .arrival(clock, e_close, node, pulse_tr.opposite(), An::Early)
        else {
            continue;
        };
        let Some(req) = required(
            inputs,
            tracker,
            checks,
            PulseCheckKind::Width,
            pulse_tr,
            open.slew,
        ) else {
            continue;
        };
        let t_open = wave(e_open);
        let mut t_close = wave(e_close);
        if t_close <= t_open {
            t_close += period;
        }
        let width = (t_close + close.latency) - (t_open + open.latency);
        let row = LimitRow {
            node,
            kind: if pulse_tr == Tr::Rise {
                LimitKind::MinPulseWidthHigh
            } else {
                LimitKind::MinPulseWidthLow
            },
            tr: Some(pulse_tr),
            clock: Some(clock),
            value: width,
            limit: req,
            slack: width - req,
            source: LimitSource::Liberty,
        };
        if best.is_none_or(|b| row.slack < b.slack) {
            best = Some(row);
        }
    }
    best
}

fn period_row(
    inputs: &LimitInputs<'_>,
    tracker: &mut RangeTracker,
    checks: &[&PulseCheck],
    node: NodeId,
    clock: ClockId,
    bits: u8,
    ck: &crate::engine::input::constr::CompiledClock,
) -> Option<LimitRow> {
    if !checks.iter().any(|c| c.kind == PulseCheckKind::Period) {
        return None;
    }
    let mut worst: Option<(f64, Tr)> = None;
    for tr in Tr::ALL {
        // The transition time of the pin's `tr` edges: the latest over the source edges that
        // produce them.
        let slew = opening_edges(tr, bits)
            .into_iter()
            .filter_map(|e| inputs.clock_model.arrival(clock, e, node, tr, An::Late))
            .map(|a| a.slew)
            .reduce(f64::max);
        let Some(slew) = slew else {
            continue;
        };
        if let Some(req) = required(inputs, tracker, checks, PulseCheckKind::Period, tr, slew)
            && worst.is_none_or(|(w, _)| req > w)
        {
            worst = Some((req, tr));
        }
    }
    let (req, tr) = worst?;
    Some(LimitRow {
        node,
        kind: LimitKind::MinPeriod,
        tr: Some(tr),
        clock: Some(clock),
        value: ck.period_ns,
        limit: req,
        slack: ck.period_ns - req,
        source: LimitSource::Liberty,
    })
}
