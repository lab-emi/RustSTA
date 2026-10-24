// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The response of a cell output that drives an extracted pi load (`STA-REQ-0726`,
//! `STA-REQ-0727`).
//!
//! The Thevenin resistance of the finite-ramp driver is the forward delay sensitivity of the NLDM
//! table over one femtofarad above the total load, times `-ln(1 - output threshold)`. A pi whose
//! resistance is below one thousandth of that resistance is a lumped load: the delay and the
//! transition are the table values at the total capacitance, as for a load without resistance.
//! Every other pi goes through the bounded charge solve of `rusteda_compute::rc`.

use rusteda_compute::ComputeError;
use rusteda_compute::rc::{self, Pi, Thresholds};

/// The load step (pF) of the delay sensitivity that sets the Thevenin resistance: 1 fF.
pub const SENSITIVITY_STEP_PF: f64 = 1e-3;

/// A pi load whose resistance is below this fraction of the Thevenin resistance is lumped.
pub const LUMPED_RESISTANCE_RATIO: f64 = 1e-3;

/// Delay (ns), output transition (ns) and effective capacitance (pF) of a cell arc driving `pi`
/// with the trip points `t`; `eval(c)` is the arc's NLDM `(delay, transition)` at load `c` (pF).
pub fn pi_response(
    pi: Pi,
    t: Thresholds,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<(f64, f64, f64), ComputeError> {
    pi.validate()?;
    t.validate()?;
    if t.output <= t.lower || t.output >= t.upper {
        return Err(ComputeError::Invalid(
            "RC driver delay threshold must lie within slew trip points".into(),
        ));
    }
    let total = pi.total();
    let (d0, s0) = eval(total);
    if !d0.is_finite() || !s0.is_finite() {
        return Err(ComputeError::Invalid("RC NLDM nonfinite".into()));
    }
    let sensitivity = (eval(total + SENSITIVITY_STEP_PF).0 - d0) / SENSITIVITY_STEP_PF;
    let res = sensitivity.max(0.0) * (-(1.0 - t.output).ln());
    if pi.resistance < LUMPED_RESISTANCE_RATIO * res && s0 > 0.0 {
        return Ok((d0, s0, total));
    }
    let r = rc::gate_detail_with_resistance(pi, t, res, eval)?;
    Ok((r.delay, r.slew, r.capacitance))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trips() -> Thresholds {
        Thresholds {
            lower: 0.2,
            upper: 0.8,
            input: 0.5,
            output: 0.5,
            derate: 1.0,
        }
    }

    // STA-REQ-0727: below one thousandth of the Thevenin resistance the load is lumped (table
    // values at the total load); above it, the charge solve with the same resistance.
    #[test]
    fn lumped_below_one_thousandth_of_the_driver_resistance() {
        let t = trips();
        // delay slope 10 ns/pF: Thevenin resistance 10 * ln 2 kOhm, threshold 6.93 Ohm.
        let eval = |c: f64| (0.06 + 10.0 * c, 0.07 + 20.0 * c);
        let threshold = 1e-3 * 10.0 * std::f64::consts::LN_2;
        let below = Pi {
            near: 0.001,
            far: 0.003,
            resistance: threshold * 0.999,
        };
        let (d, s, c) = pi_response(below, t, eval).unwrap();
        assert_eq!((d, s, c), (eval(0.004).0, eval(0.004).1, 0.004));
        let above = Pi {
            resistance: threshold * 1.001,
            ..below
        };
        let (d, s, c) = pi_response(above, t, eval).unwrap();
        let r =
            rc::gate_detail_with_resistance(above, t, 10.0 * std::f64::consts::LN_2, eval).unwrap();
        for (x, y) in [(d, r.delay), (s, r.slew), (c, r.capacitance)] {
            assert!((x - y).abs() < 1e-12, "{x} {y}");
        }
        // The model's transition of a nearly lumped load is not the table transition: the
        // switch is a discontinuity of the declared model, not of the circuit.
        assert!(c < 0.004 && (s - eval(0.004).1).abs() > 1e-3);
    }

    // STA-REQ-0726: the sensitivity is the forward difference over 1 fF at the total load, so a
    // table breakpoint within 1 fF above the total load changes the resistance.
    #[test]
    fn forward_sensitivity_over_one_femtofarad() {
        let t = trips();
        let kink = |c: f64| {
            let d = if c <= 0.01 {
                10.0 * c
            } else {
                0.1 + 4.0 * (c - 0.01)
            };
            (0.06 + d, 0.07 + 20.0 * c)
        };
        let pi = Pi {
            near: 0.0003,
            far: 0.0092,
            resistance: 0.3,
        };
        // total 0.0095: half of the step lies above the breakpoint, slope 7 ns/pF.
        let (_, s, _) = pi_response(pi, t, kink).unwrap();
        let r = rc::gate_detail_with_resistance(pi, t, 7.0 * std::f64::consts::LN_2, kink).unwrap();
        assert!((s - r.slew).abs() < 1e-12, "{s} {}", r.slew);
        // lumped threshold with the same slope: 7 * ln 2 Ohm.
        let lumped = Pi {
            resistance: 0.99e-3 * 7.0 * std::f64::consts::LN_2,
            ..pi
        };
        assert_eq!(pi_response(lumped, t, kink).unwrap().2, 0.0095);
    }

    // STA-REQ-0726: invalid inputs keep their errors; a flat table has no resistance.
    #[test]
    fn invalid_models_are_errors() {
        let t = trips();
        let pi = Pi {
            near: 0.001,
            far: 0.002,
            resistance: 0.1,
        };
        assert!(pi_response(pi, t, |_| (f64::NAN, 0.1)).is_err());
        assert!(pi_response(pi, t, |_| (0.1, 0.1)).is_err());
        assert!(pi_response(Pi { near: -1.0, ..pi }, t, |c| (c, c)).is_err());
        let bad = Thresholds { derate: 0.0, ..t };
        assert!(pi_response(pi, bad, |c| (10.0 * c, 0.1 + 20.0 * c)).is_err());
        let tiny = Pi {
            resistance: 1e-9,
            ..pi
        };
        assert!(pi_response(tiny, bad, |c| (10.0 * c, 0.1 + 20.0 * c)).is_err());
    }
}
