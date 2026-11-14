// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The conversion of each library's values into the engine's internal units (`STA-REQ-0018`).
//!
//! The analysis computes in nanoseconds, picofarads, kilohms, volts, milliamperes and nanowatts.
//! Every library is converted with its own unit attributes, never with those of another library of
//! its corner, so the libraries of one corner may state different units and the same physical
//! library written in other units gives the same timing. A library unit that is the internal unit
//! divided by a whole number (`1ps`, `10fF`, `1ohm`) converts by dividing by that number instead of
//! multiplying by its inexact reciprocal, as SDC values do (`SDC-REQ-0016`): `123` in `1ps` becomes
//! the same `0.123` ns as the text `0.123` in `1ns`.

use rusteda_core::liberty::Library;

/// The conversion of a value stated in one library unit of a quantity into the internal unit of
/// that quantity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitScale(Op);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    /// The library unit is the internal unit.
    Identity,
    /// The library unit is the internal unit divided by this whole number.
    Divide(f64),
    /// The library unit is this many internal units.
    Multiply(f64),
}

impl UnitScale {
    /// The conversion of a library unit equal to the internal unit: values are unchanged.
    pub const IDENTITY: Self = Self(Op::Identity);

    /// The conversion of a library unit whose size in internal units is `scale` (a
    /// [`Unit::scale`](rusteda_core::liberty::Unit::scale)). A `scale` that is exactly the
    /// reciprocal of a whole number divides by that number.
    #[must_use]
    pub fn new(scale: f64) -> Self {
        if scale == 1.0 {
            return Self::IDENTITY;
        }
        if scale > 0.0 && scale < 1.0 {
            let divisor = (1.0 / scale).round();
            if 1.0 / divisor == scale {
                return Self(Op::Divide(divisor));
            }
        }
        Self(Op::Multiply(scale))
    }

    /// `value`, stated in the library unit, in the internal unit.
    #[inline]
    #[must_use]
    pub fn apply(self, value: f64) -> f64 {
        match self.0 {
            Op::Identity => value,
            Op::Divide(divisor) => value / divisor,
            Op::Multiply(scale) => value * scale,
        }
    }

    /// The size of one library unit in internal units.
    #[must_use]
    pub fn scale(self) -> f64 {
        match self.0 {
            Op::Identity => 1.0,
            Op::Divide(divisor) => 1.0 / divisor,
            Op::Multiply(scale) => scale,
        }
    }
}

/// The conversions of the values of one library into the internal units (`STA-REQ-0018`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LibraryUnits {
    /// `time_unit` into ns: tables, transition and pulse limits.
    pub time: UnitScale,
    /// `capacitive_load_unit` into pF: pin capacitances, capacitance limits, table axes and
    /// wire-load capacitances.
    pub capacitance: UnitScale,
    /// `pulling_resistance_unit` into kOhm.
    pub resistance: UnitScale,
    /// `voltage_unit` into V.
    pub voltage: UnitScale,
    /// `current_unit` into mA.
    pub current: UnitScale,
    /// `leakage_power_unit` into nW.
    pub power: UnitScale,
}

impl LibraryUnits {
    /// The conversions of `lib`, from its own unit attributes.
    #[must_use]
    pub fn of(lib: &Library) -> Self {
        Self {
            time: UnitScale::new(lib.time_unit_ns),
            capacitance: UnitScale::new(lib.cap_unit_pf),
            resistance: UnitScale::new(lib.units.resistance.scale),
            voltage: UnitScale::new(lib.units.voltage.scale),
            current: UnitScale::new(lib.units.current.scale),
            power: UnitScale::new(lib.units.leakage_power.scale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // STA-REQ-0018: the internal unit is kept as is, a whole fraction of it divides exactly, and
    // any other size multiplies.
    #[test]
    fn exact_conversions() {
        assert_eq!(UnitScale::new(1.0), UnitScale::IDENTITY);
        let v = -0.0f64;
        assert_eq!(UnitScale::IDENTITY.apply(v).to_bits(), v.to_bits());
        // 1ps, 10ps, 100ps, 1fs, 10fF and 1ohm as the Liberty reader states them.
        for (scale, divisor) in [
            (1e-3, 1e3),
            (10.0 * 1e-3, 1e2),
            (100.0 * 1e-3, 1e1),
            (1e-6, 1e6),
            (0.5, 2.0),
        ] {
            assert_eq!(
                UnitScale::new(scale),
                UnitScale(Op::Divide(divisor)),
                "{scale}"
            );
            assert_eq!(UnitScale::new(scale).scale(), scale);
        }
        assert_eq!(UnitScale::new(1e-3).apply(123.0), 0.123);
        assert_eq!(UnitScale::new(1e-3).apply(4.5), 0.0045);
        assert_eq!(UnitScale::new(0.01).apply(1.5), 0.015);
        assert_eq!(UnitScale::new(1e3).apply(0.25), 250.0);
        assert_eq!(UnitScale::new(1e6).scale(), 1e6);
        assert_eq!(UnitScale::new(0.3), UnitScale(Op::Multiply(0.3)));
        assert_eq!(UnitScale::new(1.5).apply(2.0), 3.0);
    }
}
