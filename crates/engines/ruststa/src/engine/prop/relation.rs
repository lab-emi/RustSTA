// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Setup and hold relations between launching and capturing clock edges (owner: E5).
//!
//! Engine section C. Relations are computed in integer femtoseconds over the common period of the
//! two clocks: the setup relation is the smallest distance from a launching edge to the next
//! capturing edge after it, the hold relation the largest distance from a launching edge to the
//! last capturing edge at or before it.

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;

use crate::engine::input::constr::{CompiledClock, CompiledConstraints};
use crate::engine::types::{ClockId, Tr};

/// Maximum ratio `lcm(T_L, T_C) / max(T_L, T_C)` before a clock pair is rejected (`STA-REQ-0220`).
pub const MAX_COMMON_PERIOD_CYCLES: i128 = 1000;

/// Converts nanoseconds (`f64`) to integer femtoseconds (`1 ns = 10^6 fs`; `STA-REQ-0220`).
#[inline]
#[must_use]
pub fn ns_to_fs(ns: f64) -> i128 {
    (ns * 1.0e6).round() as i128
}

/// Converts integer femtoseconds back to nanoseconds (`f64`).
#[inline]
#[must_use]
pub fn fs_to_ns(fs: i128) -> f64 {
    let v = fs as f64 / 1.0e6;
    if v == 0.0 { 0.0 } else { v }
}

#[inline]
fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.abs().max(1)
}

/// Computes `(tl_fs, tc_fs, lcm_fs)` in integer femtoseconds for `(launch, capture)` and checks
/// that `lcm(T_L, T_C) <= 1000 * max(T_L, T_C)` (`STA-REQ-0220`).
pub fn common_period_fs(
    launch: &CompiledClock,
    capture: &CompiledClock,
) -> Result<(i128, i128, i128), Diag> {
    let tl = ns_to_fs(launch.period_ns);
    let tc = ns_to_fs(capture.period_ns);
    if tl <= 0 || tc <= 0 {
        return Err(Diag::error(
            "T0021",
            format!(
                "clocks '{}' and '{}' must have positive periods (got {} ns, {} ns)",
                launch.name, capture.name, launch.period_ns, capture.period_ns
            ),
        ));
    }
    let g = gcd(tl, tc);
    let lcm = (tl / g).saturating_mul(tc);
    if lcm > MAX_COMMON_PERIOD_CYCLES * tl.max(tc) {
        return Err(Diag::error(
            "T0021",
            format!(
                "clocks '{}' and '{}' have no common period within {} cycles (STA-REQ-0220)",
                launch.name, capture.name, MAX_COMMON_PERIOD_CYCLES
            ),
        ));
    }
    Ok((tl, tc, lcm))
}

/// Setup relation `D_s(L, e_L, C, e_C) > 0` in ns (`STA-REQ-0220`): the smallest strictly positive
/// distance `c_m - l_k > 0` from a launching edge `e_L` of `launch` to a capturing edge `e_C` of
/// `capture` over `[0, lcm(T_L, T_C))`.
pub fn setup_relation(
    launch: &CompiledClock,
    e_l: Tr,
    capture: &CompiledClock,
    e_c: Tr,
) -> Result<f64, Diag> {
    let (tl, tc, lcm) = common_period_fs(launch, capture)?;
    let el_t = ns_to_fs(launch.waveform_ns[e_l.index()]);
    let ec_t = ns_to_fs(capture.waveform_ns[e_c.index()]);
    let mut best = i128::MAX;
    let mut i = 0i128;
    while i * tl < lcm {
        let l_k = el_t + i * tl;
        let k0 = (l_k - ec_t).div_euclid(tc) + 1;
        let c_m = ec_t + k0 * tc;
        best = best.min(c_m - l_k);
        i += 1;
    }
    Ok(fs_to_ns(best))
}

/// Hold relation `D_h(L, e_L, C, e_C) <= 0` in ns (`STA-REQ-0220`): the largest non-positive
/// distance `c_m - l_k <= 0` from a launching edge `e_L` of `launch` to a capturing edge `e_C` of
/// `capture` at or before `l_k` over `[0, lcm(T_L, T_C))`.
pub fn hold_relation(
    launch: &CompiledClock,
    e_l: Tr,
    capture: &CompiledClock,
    e_c: Tr,
) -> Result<f64, Diag> {
    let (tl, tc, lcm) = common_period_fs(launch, capture)?;
    let el_t = ns_to_fs(launch.waveform_ns[e_l.index()]);
    let ec_t = ns_to_fs(capture.waveform_ns[e_c.index()]);
    let mut best = i128::MIN;
    let mut i = 0i128;
    while i * tl < lcm {
        let l_k = el_t + i * tl;
        let k0 = (l_k - ec_t).div_euclid(tc);
        let c_m = ec_t + k0 * tc;
        best = best.max(c_m - l_k);
        i += 1;
    }
    Ok(fs_to_ns(best))
}

/// Smallest non-negative difference `c_m - l_k >= 0` in ns from `e_L` of `launch` to `e_C` of
/// `capture` over `[0, lcm(T_L, T_C))` (`-D_h(C, e_C, L, e_L)`).
pub fn non_negative_relation(
    launch: &CompiledClock,
    e_l: Tr,
    capture: &CompiledClock,
    e_c: Tr,
) -> Result<f64, Diag> {
    let (tl, tc, lcm) = common_period_fs(launch, capture)?;
    let el_t = ns_to_fs(launch.waveform_ns[e_l.index()]);
    let ec_t = ns_to_fs(capture.waveform_ns[e_c.index()]);
    let mut best = i128::MAX;
    let mut i = 0i128;
    while i * tl < lcm {
        let l_k = el_t + i * tl;
        let mut k0 = (l_k - ec_t).div_euclid(tc);
        if ec_t + k0 * tc < l_k {
            k0 += 1;
        }
        let c_m = ec_t + k0 * tc;
        best = best.min(c_m - l_k);
        i += 1;
    }
    Ok(fs_to_ns(best))
}

/// Active enable pulse width `W_pulse in (0, T_C]` in ns from opening edge `e_open` to closing
/// edge `e_close` of `capture` (`STA-REQ-0221`).
#[must_use]
pub fn latch_pulse_width(capture: &CompiledClock, e_open: Tr, e_close: Tr) -> f64 {
    let tc = ns_to_fs(capture.period_ns).max(1);
    let t_open = ns_to_fs(capture.waveform_ns[e_open.index()]);
    let t_close = ns_to_fs(capture.waveform_ns[e_close.index()]);
    let mut w = (t_close - t_open).rem_euclid(tc);
    if w == 0 {
        w = tc;
    }
    fs_to_ns(w)
}

/// Latch opening and closing relations `(d_open, d_close, w_pulse)` in ns for a single enable
/// pulse of `capture` (`STA-REQ-0221`): `d_close = D_s(L, e_L, C, e_close) > 0` and
/// `d_open = d_close - w_pulse` is the opening edge of that same enable pulse.
pub fn latch_window_relations(
    launch: &CompiledClock,
    e_l: Tr,
    capture: &CompiledClock,
    e_open: Tr,
    e_close: Tr,
) -> Result<(f64, f64, f64), Diag> {
    let d_close = setup_relation(launch, e_l, capture, e_close)?;
    let w_pulse = latch_pulse_width(capture, e_open, e_close);
    let d_open = fs_to_ns(ns_to_fs(d_close) - ns_to_fs(w_pulse));
    Ok((d_open, d_close, w_pulse))
}

/// Pre-computed cache of setup and hold relations across `(ClockId, Tr, ClockId, Tr)` pairs.
#[derive(Debug, Clone, Default)]
pub struct RelationTable {
    setup: FxHashMap<(ClockId, Tr, ClockId, Tr), f64>,
    hold: FxHashMap<(ClockId, Tr, ClockId, Tr), f64>,
}

impl RelationTable {
    /// Creates an empty [`RelationTable`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(D_s, D_h)` for `(l, e_l, c, e_c)`, computing and caching on first access.
    pub fn get_or_compute(
        &mut self,
        constraints: &CompiledConstraints,
        l: ClockId,
        e_l: Tr,
        c: ClockId,
        e_c: Tr,
    ) -> Result<(f64, f64), Diag> {
        let key = (l, e_l, c, e_c);
        if let (Some(&ds), Some(&dh)) = (self.setup.get(&key), self.hold.get(&key)) {
            return Ok((ds, dh));
        }
        let ck_l = &constraints.clocks[l.index()];
        let ck_c = &constraints.clocks[c.index()];
        let ds = setup_relation(ck_l, e_l, ck_c, e_c)?;
        let dh = hold_relation(ck_l, e_l, ck_c, e_c)?;
        self.setup.insert(key, ds);
        self.hold.insert(key, dh);
        Ok((ds, dh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_clock(name: &str, period_ns: f64, rise_ns: f64, fall_ns: f64) -> CompiledClock {
        CompiledClock {
            id: ClockId(0),
            name: name.into(),
            period_ns,
            waveform_ns: [rise_ns, fall_ns],
            sources: Vec::new(),
            add: false,
            comment: None,
            source_latency_ns: [[[0.0; 2]; 2]; 2],
            network_latency_ns: [[0.0; 2]; 2],
            uncertainty_setup_ns: [0.0; 2],
            uncertainty_hold_ns: [0.0; 2],
            transition_ns: [[0.0; 2]; 2],
            location: None,
        }
    }

    // STA-REQ-0220: cross-clock setup and hold relations in integer femtoseconds.
    #[test]
    fn cross_clock_setup_and_hold_relations() {
        let cl = dummy_clock("CL", 6.0, 0.0, 3.0);
        let cc = dummy_clock("CC", 4.0, 0.0, 2.0);
        assert_eq!(setup_relation(&cl, Tr::Rise, &cc, Tr::Rise).unwrap(), 2.0);
        assert_eq!(hold_relation(&cl, Tr::Rise, &cc, Tr::Rise).unwrap(), 0.0);

        let ca = dummy_clock("CA", 6.0, 0.0, 3.0);
        let cb = dummy_clock("CB", 4.0, 1.0, 3.0);
        assert_eq!(setup_relation(&ca, Tr::Rise, &cb, Tr::Rise).unwrap(), 1.0);
        assert_eq!(hold_relation(&ca, Tr::Rise, &cb, Tr::Rise).unwrap(), -1.0);
    }

    // STA-REQ-0221: latch opening relation is taken from the same enable pulse as D_close.
    #[test]
    fn latch_same_pulse_opening_relation() {
        let ca = dummy_clock("CA", 2.0, 0.0, 1.0);
        let cb = dummy_clock("CB", 2.0, 0.5, 1.5);

        let (d_open1, d_close1, w1) =
            latch_window_relations(&cb, Tr::Rise, &ca, Tr::Rise, Tr::Fall).unwrap();
        assert_eq!((d_open1, d_close1, w1), (-0.5, 0.5, 1.0));

        let (d_open2, d_close2, w2) =
            latch_window_relations(&ca, Tr::Rise, &ca, Tr::Rise, Tr::Fall).unwrap();
        assert_eq!((d_open2, d_close2, w2), (0.0, 1.0, 1.0));

        let (d_open3, d_close3, w3) =
            latch_window_relations(&ca, Tr::Fall, &ca, Tr::Rise, Tr::Fall).unwrap();
        assert_eq!((d_open3, d_close3, w3), (1.0, 2.0, 1.0));
    }
}
