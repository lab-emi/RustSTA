// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The response of a cell output that drives an extracted pi load (`STA-REQ-0728`,
//! `STA-REQ-0727`).
//!
//! The Thevenin resistance of the finite-ramp driver is the forward delay sensitivity of the NLDM
//! table over one femtofarad above the total load, times `-ln(1 - output threshold)`. A pi whose
//! resistance is below one thousandth of that resistance is a lumped load: the delay and the
//! transition are the table values at the total capacitance, as for a load without resistance.
//! Every other pi takes its effective capacitance from one Newton-Raphson correction of the
//! driver system from the total load (`rusteda_compute::rc::gate_one_step`); the net's wires then
//! follow the driver waveform of that resistance (`STA-REQ-0729`).

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
    eval: impl FnMut(f64) -> (f64, f64),
) -> Result<(f64, f64, f64), ComputeError> {
    let r = pi_drive(pi, t, eval)?;
    Ok((r.delay, r.slew, r.capacitance))
}

/// The response of a cell arc into a pi load together with its driver waveform, which the wires
/// of the net then follow (`STA-REQ-0729`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiDrive {
    pub delay: f64,
    pub slew: f64,
    pub capacitance: f64,
    /// The Thevenin resistance (kOhm) and the source ramp duration (ns) of the driver waveform;
    /// `None` in the lumped regime.
    pub drive: Option<(f64, f64)>,
}

/// [`pi_response`] with the driver waveform.
pub fn pi_drive(
    pi: Pi,
    t: Thresholds,
    mut eval: impl FnMut(f64) -> (f64, f64),
) -> Result<PiDrive, ComputeError> {
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
        return Ok(PiDrive {
            delay: d0,
            slew: s0,
            capacitance: total,
            drive: None,
        });
    }
    let r = rc::gate_one_step(pi, t, res, eval)?;
    Ok(PiDrive {
        delay: r.delay,
        slew: r.slew,
        capacitance: r.capacitance,
        drive: (r.resistance > 0.0).then_some((r.resistance, r.duration)),
    })
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
    // values at the total load); above it, the one-step correction with the same resistance.
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
        let r = pi_drive(below, t, eval).unwrap();
        assert_eq!(
            (r.delay, r.slew, r.capacitance, r.drive),
            (eval(0.004).0, eval(0.004).1, 0.004, None)
        );
        let above = Pi {
            resistance: threshold * 1.001,
            ..below
        };
        let r = pi_drive(above, t, eval).unwrap();
        let g = rc::gate_one_step(above, t, 10.0 * std::f64::consts::LN_2, eval).unwrap();
        for (x, y) in [
            (r.delay, g.delay),
            (r.slew, g.slew),
            (r.capacitance, g.capacitance),
        ] {
            assert!((x - y).abs() < 1e-12, "{x} {y}");
        }
        let (res, duration) = r.drive.unwrap();
        assert!((res - 10.0 * std::f64::consts::LN_2).abs() < 1e-9);
        assert!(
            (duration - g.duration).abs() < 1e-12,
            "{duration} {}",
            g.duration
        );
        // The model's transition of a nearly lumped load is not the table transition: the
        // switch is a discontinuity of the declared model, not of the circuit.
        assert!(r.capacitance < 0.004 && (r.slew - eval(0.004).1).abs() > 1e-3);
    }

    // STA-REQ-0728: the sensitivity is the forward difference over 1 fF at the total load, so a
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
        let r = rc::gate_one_step(pi, t, 7.0 * std::f64::consts::LN_2, kink).unwrap();
        assert!((s - r.slew).abs() < 1e-12, "{s} {}", r.slew);
        // lumped threshold with the same slope: 7 * ln 2 Ohm.
        let lumped = Pi {
            resistance: 0.99e-3 * 7.0 * std::f64::consts::LN_2,
            ..pi
        };
        assert_eq!(pi_response(lumped, t, kink).unwrap().2, 0.0095);
    }

    // STA-REQ-0728: the driver transition is a continuous, non-decreasing function of the pi
    // resistance. Linear tables shaped like a sky130 `clkbuf_2` at the slow corner drive the pi of
    // an extracted clock net (0.8 fF near, 25 fF far) from 60 to 240 Ohm in 1 % steps. A reference
    // timing tool's driver transition steps by 0.3 to 1.2 ps between two resistances 1 % apart in
    // this range (its iteration count changes), while transistor-level transients of the real cell
    // move by less than 0.01 ps per step; the clock-pin pulse-width tables then amplify such a
    // step by their slope. The model must not acquire such steps.
    #[test]
    fn transition_is_continuous_in_the_pi_resistance() {
        let t = trips();
        let eval = |c: f64| (0.197 + 5.7 * c, 0.02 + 8.14 * c);
        let mut previous: Option<f64> = None;
        for i in 0..140 {
            let pi = Pi {
                near: 0.0008,
                far: 0.025,
                resistance: 0.06 * 1.01f64.powi(i),
            };
            let r = pi_drive(pi, t, eval).unwrap();
            assert!(r.drive.is_some());
            if let Some(p) = previous {
                let step = r.slew - p;
                assert!((0.0..2e-5).contains(&step), "{i}: {p} -> {}", r.slew);
            }
            previous = Some(r.slew);
        }
    }

    // STA-REQ-0728: the corrected capacitance of linear synthetic tables (delay 0.06 + S c,
    // transition 0.07 + T c at the 0.1 ns input transition), with the effective capacitance that a
    // reference timing tool reports for the same pi loads (black-box delay-calculation reports,
    // 20/80 % trip points without derate and 30/70 % with derate 0.5).
    #[test]
    fn one_step_capacitance_matches_measured_reference() {
        let s_in = 0.099_999_994;
        let cases = [
            // (lower, upper, derate, S, T, near, kOhm, far, reference Ceff)
            (
                0.2,
                0.8,
                1.0,
                10.0,
                20.0,
                0.01,
                0.664_610_684,
                0.01,
                0.019_872_354,
            ),
            (
                0.2,
                0.8,
                1.0,
                10.0,
                20.0,
                0.003,
                0.399_147_958,
                0.050_000_001,
                0.052_167_863,
            ),
            (
                0.2,
                0.8,
                1.0,
                10.0,
                20.0,
                0.003,
                0.221_632_689,
                0.050_000_001,
                0.052_539_434,
            ),
            (
                0.3,
                0.7,
                0.5,
                2.0,
                4.0,
                0.02,
                0.031_186_515,
                0.005,
                0.024_994_662,
            ),
            (
                0.3,
                0.7,
                0.5,
                2.0,
                4.0,
                0.01,
                0.010_816,
                0.01,
                0.019_991_899,
            ),
            (
                0.3,
                0.7,
                0.5,
                2.0,
                4.0,
                0.02,
                0.076_865_889,
                0.005,
                0.024_986_843,
            ),
        ];
        for (lower, upper, derate, sd, st, near, resistance, far, reference) in cases {
            let t = Thresholds {
                lower,
                upper,
                input: 0.5,
                output: 0.5,
                derate,
            };
            let eval = |c: f64| (0.05 + 0.1 * s_in + sd * c, 0.02 + 0.5 * s_in + st * c);
            let r = pi_drive(
                Pi {
                    near,
                    far,
                    resistance,
                },
                t,
                eval,
            )
            .unwrap();
            assert!(
                (r.capacitance - reference).abs() < 2e-8,
                "{} {reference}",
                r.capacitance
            );
            assert!(r.capacitance < near + far && r.drive.is_some());
        }
    }

    // STA-REQ-0728: invalid inputs keep their errors; a flat table has no resistance.
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
