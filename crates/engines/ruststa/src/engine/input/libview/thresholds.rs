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
