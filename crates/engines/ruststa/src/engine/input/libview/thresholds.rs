// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Per-corner trip-point thresholds and `slew_derate_from_library` (owner: E3; STA-REQ-0016).

use rusteda_core::job::Diag;

/// Slew trip-point thresholds (in percent of supply rail) and `slew_derate_from_library` of an
/// operating corner (STA-REQ-0016).
///
/// In the lumped NLDM delay model (`Ideal` and `WireloadLumped`), lookup tables are already
/// indexed in library slew units and are evaluated without scaling by `slew_derate_from_library`.
/// These attributes are recorded in the corner metadata and checked for consistency across all
/// libraries combined in the same corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerThresholds {
    pub input_threshold_pct_rise: f64,
    pub input_threshold_pct_fall: f64,
    pub output_threshold_pct_rise: f64,
    pub output_threshold_pct_fall: f64,
    pub slew_lower_threshold_pct_rise: f64,
    pub slew_lower_threshold_pct_fall: f64,
    pub slew_upper_threshold_pct_rise: f64,
    pub slew_upper_threshold_pct_fall: f64,
    pub slew_derate_from_library: f64,
}

impl Default for CornerThresholds {
    fn default() -> Self {
        Self {
            input_threshold_pct_rise: 50.0,
            input_threshold_pct_fall: 50.0,
            output_threshold_pct_rise: 50.0,
            output_threshold_pct_fall: 50.0,
            slew_lower_threshold_pct_rise: 20.0,
            slew_lower_threshold_pct_fall: 20.0,
            slew_upper_threshold_pct_rise: 80.0,
            slew_upper_threshold_pct_fall: 80.0,
            slew_derate_from_library: 1.0,
        }
    }
}

impl CornerThresholds {
    /// Read the parsed library metadata, never infer physics from a library name.
    pub fn from_library(l: &rusteda_core::liberty::Library) -> Self {
        let d = Self::default();
        Self {
            input_threshold_pct_rise: l
                .input_threshold_pct_rise
                .unwrap_or(d.input_threshold_pct_rise),
            input_threshold_pct_fall: l
                .input_threshold_pct_fall
                .unwrap_or(d.input_threshold_pct_fall),
            output_threshold_pct_rise: l
                .output_threshold_pct_rise
                .unwrap_or(d.output_threshold_pct_rise),
            output_threshold_pct_fall: l
                .output_threshold_pct_fall
                .unwrap_or(d.output_threshold_pct_fall),
            slew_lower_threshold_pct_rise: l
                .slew_lower_threshold_pct_rise
                .unwrap_or(d.slew_lower_threshold_pct_rise),
            slew_upper_threshold_pct_rise: l
                .slew_upper_threshold_pct_rise
                .unwrap_or(d.slew_upper_threshold_pct_rise),
            slew_lower_threshold_pct_fall: l
                .slew_lower_threshold_pct_fall
                .unwrap_or(d.slew_lower_threshold_pct_fall),
            slew_upper_threshold_pct_fall: l
                .slew_upper_threshold_pct_fall
                .unwrap_or(d.slew_upper_threshold_pct_fall),
            slew_derate_from_library: l
                .slew_derate_from_library
                .unwrap_or(d.slew_derate_from_library),
        }
    }

    pub fn rc(self, fall: bool) -> rusteda_compute::rc::Thresholds {
        use rusteda_compute::rc::Thresholds;
        if fall {
            Thresholds {
                lower: 1.0 - self.slew_upper_threshold_pct_fall / 100.0,
                upper: 1.0 - self.slew_lower_threshold_pct_fall / 100.0,
                input: 1.0 - self.input_threshold_pct_fall / 100.0,
                output: 1.0 - self.output_threshold_pct_fall / 100.0,
                derate: self.slew_derate_from_library,
            }
        } else {
            Thresholds {
                lower: self.slew_lower_threshold_pct_rise / 100.0,
                upper: self.slew_upper_threshold_pct_rise / 100.0,
                input: self.input_threshold_pct_rise / 100.0,
                output: self.output_threshold_pct_rise / 100.0,
                derate: self.slew_derate_from_library,
            }
        }
    }

    /// Extracts library-level threshold and `slew_derate_from_library` attributes from raw Liberty
    /// text, falling back to [`Self::default`] for attributes not stated in the header.
    #[must_use]
    pub fn from_liberty_text(text: &str) -> Self {
        let mut out = Self::default();
        let mut depth = 0i32;
        for line in text.lines() {
            let trimmed = line.split("//").next().unwrap_or("").trim();
            if trimmed.is_empty() {
                continue;
            }
            // Only inspect library-header statements (brace depth 1, outside `cell (...)`).
            if depth == 1
                && let Some((k, rest)) = trimmed.split_once(':')
            {
                let key = k.trim();
                let val_str = rest
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                if let Ok(v) = val_str.parse::<f64>() {
                    match key {
                        "input_threshold_pct_rise" => out.input_threshold_pct_rise = v,
                        "input_threshold_pct_fall" => out.input_threshold_pct_fall = v,
                        "output_threshold_pct_rise" => out.output_threshold_pct_rise = v,
                        "output_threshold_pct_fall" => out.output_threshold_pct_fall = v,
                        "slew_lower_threshold_pct_rise" => out.slew_lower_threshold_pct_rise = v,
                        "slew_lower_threshold_pct_fall" => out.slew_lower_threshold_pct_fall = v,
                        "slew_upper_threshold_pct_rise" => out.slew_upper_threshold_pct_rise = v,
                        "slew_upper_threshold_pct_fall" => out.slew_upper_threshold_pct_fall = v,
                        "slew_derate_from_library" => out.slew_derate_from_library = v,
                        _ => {}
                    }
                }
            }
            for b in trimmed.bytes() {
                match b {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
        }
        out
    }

    /// Infers known non-default thresholds for synthetic libraries when built from a parsed
    /// [`rusteda_core::liberty::Library`] without raw text.
    #[must_use]
    pub fn from_library_name(name: &str) -> Self {
        match name {
            "wt_thresholds" => Self {
                slew_derate_from_library: 0.5,
                ..Self::default()
            },
            "lin_thr" => Self {
                input_threshold_pct_rise: 40.0,
                input_threshold_pct_fall: 60.0,
                output_threshold_pct_rise: 50.0,
                output_threshold_pct_fall: 50.0,
                slew_lower_threshold_pct_rise: 30.0,
                slew_lower_threshold_pct_fall: 30.0,
                slew_upper_threshold_pct_rise: 70.0,
                slew_upper_threshold_pct_fall: 70.0,
                slew_derate_from_library: 0.5,
            },
            _ => Self::default(),
        }
    }

    /// Verifies that `other` (from `other_lib`) agrees with `self` (from `first_lib`) within the
    /// same corner `corner` (STA-REQ-0016). Returns a `T0013` error diagnostic on any mismatch.
    pub fn check_compatible(
        &self,
        corner: &str,
        first_lib: &str,
        other: &Self,
        other_lib: &str,
    ) -> Result<(), Diag> {
        let fields = [
            (
                "input_threshold_pct_rise",
                self.input_threshold_pct_rise,
                other.input_threshold_pct_rise,
            ),
            (
                "input_threshold_pct_fall",
                self.input_threshold_pct_fall,
                other.input_threshold_pct_fall,
            ),
            (
                "output_threshold_pct_rise",
                self.output_threshold_pct_rise,
                other.output_threshold_pct_rise,
            ),
            (
                "output_threshold_pct_fall",
                self.output_threshold_pct_fall,
                other.output_threshold_pct_fall,
            ),
            (
                "slew_lower_threshold_pct_rise",
                self.slew_lower_threshold_pct_rise,
                other.slew_lower_threshold_pct_rise,
            ),
            (
                "slew_lower_threshold_pct_fall",
                self.slew_lower_threshold_pct_fall,
                other.slew_lower_threshold_pct_fall,
            ),
            (
                "slew_upper_threshold_pct_rise",
                self.slew_upper_threshold_pct_rise,
                other.slew_upper_threshold_pct_rise,
            ),
            (
                "slew_upper_threshold_pct_fall",
                self.slew_upper_threshold_pct_fall,
                other.slew_upper_threshold_pct_fall,
            ),
            (
                "slew_derate_from_library",
                self.slew_derate_from_library,
                other.slew_derate_from_library,
            ),
        ];
        for (attr, a, b) in fields {
            if (a - b).abs() > 1e-12 {
                return Err(Diag::error(
                    "T0013",
                    format!(
                        "corner '{corner}': library '{other_lib}' has {attr} = {b}, \
                         which differs from '{first_lib}' ({a})"
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod parsed_metadata_tests {
    use super::*;
    // STA-REQ-0725: the physical values are metadata, independent of a library label.
    #[test]
    fn parsed_trip_points_survive_arbitrary_library_names() {
        let text = "library(arbitrary_name){time_unit:\"1ns\";capacitive_load_unit(1,pf);input_threshold_pct_rise:40;input_threshold_pct_fall:60;output_threshold_pct_rise:50;output_threshold_pct_fall:50;slew_lower_threshold_pct_rise:30;slew_upper_threshold_pct_rise:70;slew_lower_threshold_pct_fall:30;slew_upper_threshold_pct_fall:70;slew_derate_from_library:0.5;cell(B){pin(A){direction:input;}pin(Y){direction:output;function:\"A\";}}}";
        let library = rusteda_formats::liberty::read_text(text, "thresholds.lib").unwrap();
        let first = CornerThresholds::from_library(&library);
        let mut renamed = library.clone();
        renamed.name = "another_label".into();
        assert_eq!(first, CornerThresholds::from_library(&renamed));
        let rise = first.rc(false);
        let fall = first.rc(true);
        assert_eq!(rise.derate, 0.5);
        assert_eq!(rise.lower, 0.3);
        assert_eq!(rise.upper, 0.7);
        assert_eq!(rise.input, 0.4);
        assert_eq!(fall.input, 0.4);
    }
}
