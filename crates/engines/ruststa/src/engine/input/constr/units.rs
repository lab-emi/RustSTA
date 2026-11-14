// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The units SDC values are read in, and the unit agreement of the libraries (`STA-REQ-0018`).
//!
//! An SDC value without `set_units` is read in the unit of its quantity stated by the first
//! library of the corner, the main library. The libraries of one corner may state different
//! units: the library view converts each library with its own units
//! ([`LibraryUnits`](crate::engine::input::libview::LibraryUnits)), and a library whose time,
//! capacitance or resistance unit (the quantities of SDC values) differs from the first library's
//! gets warning `D0103`, naming both libraries and every unit that differs. The compiled
//! constraints are shared by all corners, so when the first libraries of two corners differ in the
//! time or capacitance unit and the SDC does not set that unit with `set_units`, one SDC value
//! would mean two physical values: that ambiguity is error `D0103`.

use rusteda_core::diag::Diag;
use rusteda_core::liberty::{Library, Unit, Units};
use rusteda_formats::sdc::Sdc;

use super::model::{CompiledConstraints, CornerLibs, CornerUnits};
use crate::engine::types::CornerId;

/// A unit attribute of a library: its name, its unit when absent, and its unit in `Units`.
struct UnitAttr {
    name: &'static str,
    internal: &'static str,
    unit: fn(&Units) -> &Unit,
}

/// The unit attributes in the order of [`Units`]; the first three are the quantities SDC values
/// are stated in.
const ATTRS: [UnitAttr; 6] = [
    UnitAttr {
        name: "time_unit",
        internal: "1ns",
        unit: |u| &u.time,
    },
    UnitAttr {
        name: "capacitive_load_unit",
        internal: "(1,pf)",
        unit: |u| &u.capacitance,
    },
    UnitAttr {
        name: "pulling_resistance_unit",
        internal: "1kohm",
        unit: |u| &u.resistance,
    },
    UnitAttr {
        name: "voltage_unit",
        internal: "1V",
        unit: |u| &u.voltage,
    },
    UnitAttr {
        name: "current_unit",
        internal: "1mA",
        unit: |u| &u.current,
    },
    UnitAttr {
        name: "leakage_power_unit",
        internal: "1nW",
        unit: |u| &u.leakage_power,
    },
];

/// The number of [`ATTRS`] that SDC values are stated in (time, capacitance, resistance).
const SDC_QUANTITIES: usize = 3;

/// The number of [`ATTRS`] whose disagreement between the corners' first libraries makes SDC
/// values ambiguous: time and capacitance, the quantities of the compiled SDC values.
const COMPILED_QUANTITIES: usize = 2;

/// The text of `unit` in a diagnostic: as written, or the absent unit's default.
fn unit_text(attr: &UnitAttr, unit: &Unit) -> String {
    match &unit.text {
        Some(text) if unit.given && attr.name == "capacitive_load_unit" => format!("({text})"),
        Some(text) if unit.given => text.clone(),
        _ => format!("default {}", attr.internal),
    }
}

/// The attributes among `attrs` whose units differ between `first` and `lib`, as
/// `name 'first' vs 'lib'`.
fn differences<'a>(
    first: &Library,
    lib: &Library,
    attrs: impl IntoIterator<Item = &'a UnitAttr>,
) -> Vec<String> {
    attrs
        .into_iter()
        .filter_map(|attr| {
            let (a, b) = ((attr.unit)(&first.units), (attr.unit)(&lib.units));
            (!scales_agree(a.scale, b.scale)).then(|| {
                format!(
                    "{} '{}' vs '{}'",
                    attr.name,
                    unit_text(attr, a),
                    unit_text(attr, b)
                )
            })
        })
        .collect()
}

/// Records the units of every corner in `out.corner_units`, reports the libraries whose units
/// differ from their corner's first library (warning `D0103`) and the corners whose first
/// libraries make SDC values ambiguous (error `D0103`), and returns the time, capacitance and
/// resistance scales of SDC values: the first corner's.
pub(super) fn resolve_units(
    sdc: &Sdc,
    corners: &[CornerLibs<'_>],
    out: &mut CompiledConstraints,
) -> (f64, f64, f64) {
    // SDC values are compiled once, with the units of the first corner's first library.
    let main = corners
        .first()
        .and_then(|c| c.libs.first().map(|l| (c.name, l)));
    for (ci, corner) in corners.iter().enumerate() {
        let first_lib = corner.libs.first();
        if let Some(first) = first_lib {
            for lib in &corner.libs[1..] {
                if !differences(first, lib, &ATTRS[..SDC_QUANTITIES]).is_empty() {
                    out.diags.push(Diag::warning(
                        "D0103",
                        format!(
                            "corner '{}' libraries '{}' and '{}' disagree on units ({}); each \
                             library is converted to internal units and SDC values without \
                             set_units are read in the units of the first library '{}'",
                            corner.name,
                            first.name,
                            lib.name,
                            differences(first, lib, &ATTRS).join(", "),
                            first.name
                        ),
                    ));
                }
            }
            if let Some((main_corner, main_lib)) = main
                && ci > 0
            {
                let unset = [
                    sdc.units.time_to_ns.is_none(),
                    sdc.units.cap_to_pf.is_none(),
                ];
                let attrs = ATTRS[..COMPILED_QUANTITIES]
                    .iter()
                    .zip(unset)
                    .filter_map(|(attr, unset)| unset.then_some(attr));
                let ambiguous = differences(main_lib, first, attrs);
                if !ambiguous.is_empty() {
                    out.diags.push(Diag::error(
                        "D0103",
                        format!(
                            "the first libraries '{}' of corner '{}' and '{}' of corner '{}' \
                             disagree on units ({}) and the SDC sets none of them with set_units: \
                             its values would mean different quantities in the two corners",
                            main_lib.name,
                            main_corner,
                            first.name,
                            corner.name,
                            ambiguous.join(", ")
                        ),
                    ));
                }
            }
        }

        let lib_t = first_lib.map_or(1.0, |l| l.units.time.scale);
        let lib_c = first_lib.map_or(1.0, |l| l.units.capacitance.scale);
        let lib_r = first_lib.map_or(1.0, |l| l.units.resistance.scale);
        let lib_v = first_lib.map_or(1.0, |l| l.units.voltage.scale);
        let lib_i = first_lib.map_or(1.0, |l| l.units.current.scale);
        let lib_p = first_lib.map_or(1.0, |l| l.units.leakage_power.scale);

        out.corner_units.push(CornerUnits {
            corner: CornerId(ci as u32),
            name: corner.name.to_string(),
            time_to_ns: sdc.units.time_to_ns.unwrap_or(lib_t),
            cap_to_pf: sdc.units.cap_to_pf.unwrap_or(lib_c),
            res_to_kohm: sdc.units.res_to_kohm.unwrap_or(lib_r),
            voltage_to_v: sdc.units.voltage_to_v.unwrap_or(lib_v),
            current_to_ma: sdc.units.current_to_ma.unwrap_or(lib_i),
            power_to_nw: sdc.units.power_to_nw.unwrap_or(lib_p),
        });
    }

    if out.corner_units.is_empty() {
        let cu = CornerUnits {
            corner: CornerId(0),
            name: "default".to_string(),
            time_to_ns: sdc.units.time_to_ns.unwrap_or(1.0),
            cap_to_pf: sdc.units.cap_to_pf.unwrap_or(1.0),
            res_to_kohm: sdc.units.res_to_kohm.unwrap_or(1.0),
            voltage_to_v: sdc.units.voltage_to_v.unwrap_or(1.0),
            current_to_ma: sdc.units.current_to_ma.unwrap_or(1.0),
            power_to_nw: sdc.units.power_to_nw.unwrap_or(1.0),
        };
        let scales = (cu.time_to_ns, cu.cap_to_pf, cu.res_to_kohm);
        out.corner_units.push(cu);
        scales
    } else {
        let cu = &out.corner_units[0];
        (cu.time_to_ns, cu.cap_to_pf, cu.res_to_kohm)
    }
}

fn scales_agree(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1.0)
}
