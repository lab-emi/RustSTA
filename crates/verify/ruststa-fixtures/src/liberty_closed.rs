// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Closed-form synthetic Liberty library variants with exact dyadic affine lookup tables for
//! analytical delay, transition, check, wire-load, conditional-arc, tri-state, threshold, and
//! diagnostic verification.

use std::fmt::Write;

/// Affine 2x2 table parameters: `val(slew, load) = d0 + k_slew * (slew - s0) + k_load * (load - c0)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineLut2x2 {
    pub s0: f64,
    pub s1: f64,
    pub c0: f64,
    pub c1: f64,
    pub d0: f64,
    pub k_slew: f64,
    pub k_load: f64,
}

impl AffineLut2x2 {
    pub const DEFAULT_DELAY: Self = Self {
        s0: 0.1,
        s1: 0.5,
        c0: 0.01,
        c1: 0.05,
        d0: 0.05,
        k_slew: 0.25,
        k_load: 2.0,
    };

    pub const DEFAULT_SLEW: Self = Self {
        s0: 0.1,
        s1: 0.5,
        c0: 0.01,
        c1: 0.05,
        d0: 0.04,
        k_slew: 0.5,
        k_load: 1.5,
    };

    /// Evaluates the exact affine formula at `(slew, load)` (matches bilinear interpolation and
    /// linear extrapolation everywhere).
    #[must_use]
    pub fn eval(&self, slew: f64, load: f64) -> f64 {
        self.d0 + self.k_slew * (slew - self.s0) + self.k_load * (load - self.c0)
    }

    /// Formats the 4 corner values `(v00, v01, v10, v11)` scaled by `scale`.
    fn format_values(&self, scale: f64) -> String {
        let v00 = self.eval(self.s0, self.c0) * scale;
        let v01 = self.eval(self.s0, self.c1) * scale;
        let v10 = self.eval(self.s1, self.c0) * scale;
        let v11 = self.eval(self.s1, self.c1) * scale;
        format!("\"{v00:.4}, {v01:.4}\", \"{v10:.4}, {v11:.4}\"")
    }
}

/// Evaluates the closed-form `wl_small` wire-load capacitance (in pF) for a given `fanout`.
#[must_use]
pub fn eval_wireload_small_cap(fanout: usize) -> f64 {
    let length = match fanout {
        0 => 0.0,
        1 => 10.0,
        2 => 25.0,
        3 | 4 => 25.0 + (fanout as f64 - 2.0) * 17.5,
        _ => 60.0 + (fanout as f64 - 4.0) * 20.0,
    };
    length * 0.002
}

/// Evaluates the closed-form `wl_small` wire-load resistance (in kohm) for a given `fanout`.
#[must_use]
pub fn eval_wireload_small_res(fanout: usize) -> f64 {
    let length = match fanout {
        0 => 0.0,
        1 => 10.0,
        2 => 25.0,
        3 | 4 => 25.0 + (fanout as f64 - 2.0) * 17.5,
        _ => 60.0 + (fanout as f64 - 4.0) * 20.0,
    };
    length * 0.005
}

fn push_comb_timing(
    out: &mut String,
    tpl: &str,
    related: &str,
    sense: &str,
    d_lut: AffineLut2x2,
    s_lut: AffineLut2x2,
    scale: f64,
) {
    let dv = d_lut.format_values(scale);
    let sv = s_lut.format_values(scale);
    let _ = writeln!(out, "      timing () {{");
    let _ = writeln!(out, "        related_pin : \"{related}\";");
    let _ = writeln!(out, "        timing_sense : {sense};");
    let _ = writeln!(out, "        timing_type : combinational;");
    let _ = writeln!(out, "        cell_rise ({tpl}) {{ values ({dv}); }}");
    let _ = writeln!(out, "        cell_fall ({tpl}) {{ values ({dv}); }}");
    let _ = writeln!(out, "        rise_transition ({tpl}) {{ values ({sv}); }}");
    let _ = writeln!(out, "        fall_transition ({tpl}) {{ values ({sv}); }}");
    let _ = writeln!(out, "      }}");
}

fn push_base_cells(out: &mut String, time_scale: f64, cap_scale: f64, out_cap: f64) {
    let d_lut = AffineLut2x2::DEFAULT_DELAY;
    let s_lut = AffineLut2x2::DEFAULT_SLEW;
    let in_cap = 0.01 * cap_scale;
    let y_cap = out_cap * cap_scale;

    // 1. wt_inv_1
    let _ = writeln!(
        out,
        "  cell (wt_inv_1) {{\n    area : 1.0;\n    pin (A) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (Y) {{\n      direction : output;\n      capacitance : {y_cap:.4};\n      function : \"!A\";"
    );
    push_comb_timing(
        out,
        "lut_2x2",
        "A",
        "negative_unate",
        d_lut,
        s_lut,
        time_scale,
    );
    let _ = writeln!(out, "    }}\n  }}");

    // 2. wt_buf_1
    let _ = writeln!(
        out,
        "  cell (wt_buf_1) {{\n    area : 1.5;\n    pin (A) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (Y) {{\n      direction : output;\n      capacitance : {y_cap:.4};\n      function : \"A\";"
    );
    push_comb_timing(
        out,
        "lut_2x2",
        "A",
        "positive_unate",
        d_lut,
        s_lut,
        time_scale,
    );
    let _ = writeln!(out, "    }}\n  }}");

    // 3. wt_buf_2 (faster buffer)
    let fast_d = AffineLut2x2 {
        d0: 0.03,
        k_load: 1.0,
        ..d_lut
    };
    let fast_s = AffineLut2x2 {
        d0: 0.025,
        k_load: 0.75,
        ..s_lut
    };
    let _ = writeln!(
        out,
        "  cell (wt_buf_2) {{\n    area : 2.5;\n    pin (A) {{ direction : input; capacitance : {:.4}; }}\n    pin (Y) {{\n      direction : output;\n      capacitance : {y_cap:.4};\n      function : \"A\";",
        in_cap * 1.5
    );
    push_comb_timing(
        out,
        "lut_2x2",
        "A",
        "positive_unate",
        fast_d,
        fast_s,
        time_scale,
    );
    let _ = writeln!(out, "    }}\n  }}");

    // 4..8. 2-input gates: wt_and2_1, wt_or2_1, wt_nand2_1, wt_nor2_1, wt_xor2_1
    for (name, func, sense, area) in [
        ("wt_and2_1", "A & B", "positive_unate", 2.0),
        ("wt_or2_1", "A | B", "positive_unate", 2.0),
        ("wt_nand2_1", "!(A & B)", "negative_unate", 1.5),
        ("wt_nor2_1", "!(A | B)", "negative_unate", 1.5),
        ("wt_xor2_1", "A ^ B", "non_unate", 3.0),
    ] {
        let _ = writeln!(
            out,
            "  cell ({name}) {{\n    area : {area:.1};\n    pin (A) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (B) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (Y) {{\n      direction : output;\n      capacitance : {y_cap:.4};\n      function : \"{func}\";"
        );
        push_comb_timing(out, "lut_2x2", "A", sense, d_lut, s_lut, time_scale);
        push_comb_timing(out, "lut_2x2", "B", sense, d_lut, s_lut, time_scale);
        let _ = writeln!(out, "    }}\n  }}");
    }

    // 9. wt_mux2_1
    let _ = writeln!(
        out,
        "  cell (wt_mux2_1) {{\n    area : 3.5;\n    pin (A) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (B) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (S) {{ direction : input; capacitance : {in_cap:.4}; }}\n    pin (Y) {{\n      direction : output;\n      capacitance : {y_cap:.4};\n      function : \"(A & !S) | (B & S)\";"
    );
    push_comb_timing(
        out,
        "lut_2x2",
        "A",
        "positive_unate",
        d_lut,
        s_lut,
        time_scale,
    );
    push_comb_timing(
        out,
        "lut_2x2",
        "B",
        "positive_unate",
        d_lut,
        s_lut,
        time_scale,
    );
    push_comb_timing(out, "lut_2x2", "S", "non_unate", d_lut, s_lut, time_scale);
    let _ = writeln!(out, "    }}\n  }}");

    // 10. wt_dff_p (posedge DFF)
    let su = 0.04 * time_scale;
    let hd = 0.02 * time_scale;
    let c2q_d = d_lut.format_values(time_scale);
    let c2q_s = s_lut.format_values(time_scale);
    let _ = writeln!(
        out,
        "  cell (wt_dff_p) {{
    area : 5.0;
    ff (IQ, IQN) {{ clocked_on : \"CLK\"; next_state : \"D\"; }}
    pin (CLK) {{ direction : input; clock : true; capacitance : {in_cap:.4}; }}
    pin (D) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"CLK\";
        timing_type : setup_rising;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"CLK\";
        timing_type : hold_rising;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (Q) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"IQ\";
      timing () {{
        related_pin : \"CLK\";
        timing_sense : non_unate;
        timing_type : rising_edge;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 11. wt_dffr_n (negedge DFF with active-low async reset)
    let _ = writeln!(
        out,
        "  cell (wt_dffr_n) {{
    area : 5.5;
    ff (IQ, IQN) {{ clocked_on : \"!CLK\"; next_state : \"D\"; clear : \"!RST_N\"; }}
    pin (CLK) {{ direction : input; clock : true; capacitance : {in_cap:.4}; }}
    pin (D) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"CLK\";
        timing_type : setup_falling;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"CLK\";
        timing_type : hold_falling;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (RST_N) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"CLK\";
        timing_type : recovery_falling;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"CLK\";
        timing_type : removal_falling;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (Q) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"IQ\";
      timing () {{
        related_pin : \"CLK\";
        timing_sense : non_unate;
        timing_type : falling_edge;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
      timing () {{
        related_pin : \"RST_N\";
        timing_sense : positive_unate;
        timing_type : clear;
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 12. wt_dlatch_p (active-high latch)
    let _ = writeln!(
        out,
        "  cell (wt_dlatch_p) {{
    area : 4.0;
    latch (IQ, IQN) {{ enable : \"G\"; data_in : \"D\"; }}
    pin (G) {{ direction : input; capacitance : {in_cap:.4}; }}
    pin (D) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"G\";
        timing_type : setup_falling;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"G\";
        timing_type : hold_falling;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (Q) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"IQ\";
      timing () {{
        related_pin : \"D\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
      timing () {{
        related_pin : \"G\";
        timing_sense : non_unate;
        timing_type : rising_edge;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 13. wt_dlatch_n (active-low latch)
    let _ = writeln!(
        out,
        "  cell (wt_dlatch_n) {{
    area : 4.0;
    latch (IQ, IQN) {{ enable : \"!GN\"; data_in : \"D\"; }}
    pin (GN) {{ direction : input; capacitance : {in_cap:.4}; }}
    pin (D) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"GN\";
        timing_type : setup_rising;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"GN\";
        timing_type : hold_rising;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (Q) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"IQ\";
      timing () {{
        related_pin : \"D\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
      timing () {{
        related_pin : \"GN\";
        timing_sense : non_unate;
        timing_type : falling_edge;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 14. wt_icg_p (integrated posedge clock gating cell)
    let _ = writeln!(
        out,
        "  cell (wt_icg_p) {{
    area : 4.5;
    clock_gating_integrated_cell : \"latch_posedge\";
    pin (CLK) {{ direction : input; clock : true; capacitance : {in_cap:.4}; }}
    pin (EN) {{
      direction : input;
      capacitance : {in_cap:.4};
      timing () {{
        related_pin : \"CLK\";
        timing_type : setup_rising;
        rise_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{su:.4}, {su:.4}\", \"{su:.4}, {su:.4}\"); }}
      }}
      timing () {{
        related_pin : \"CLK\";
        timing_type : hold_rising;
        rise_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
        fall_constraint (chk_2x2) {{ values (\"{hd:.4}, {hd:.4}\", \"{hd:.4}, {hd:.4}\"); }}
      }}
    }}
    pin (GCLK) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"CLK & EN\";
      timing () {{
        related_pin : \"CLK\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 15. wt_tbuf_1 (tri-state buffer)
    let _ = writeln!(
        out,
        "  cell (wt_tbuf_1) {{
    area : 2.5;
    pin (A) {{ direction : input; capacitance : {in_cap:.4}; }}
    pin (OE) {{ direction : input; capacitance : {in_cap:.4}; }}
    pin (Y) {{
      direction : output;
      capacitance : {y_cap:.4};
      function : \"A\";
      three_state : \"!OE\";
      timing () {{
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
      timing () {{
        related_pin : \"OE\";
        timing_sense : positive_unate;
        timing_type : three_state_enable;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
      timing () {{
        related_pin : \"OE\";
        timing_sense : negative_unate;
        timing_type : three_state_disable;
        cell_rise (lut_2x2) {{ values ({c2q_d}); }}
        cell_fall (lut_2x2) {{ values ({c2q_d}); }}
        rise_transition (lut_2x2) {{ values ({c2q_s}); }}
        fall_transition (lut_2x2) {{ values ({c2q_s}); }}
      }}
    }}
  }}"
    );

    // 16 & 17. wt_tiehi and wt_tielo (constant tie cells)
    let _ = writeln!(
        out,
        "  cell (wt_tiehi) {{
    area : 0.5;
    pin (Y) {{ direction : output; capacitance : {y_cap:.4}; function : \"1\"; }}
  }}
  cell (wt_tielo) {{
    area : 0.5;
    pin (Y) {{ direction : output; capacitance : {y_cap:.4}; function : \"0\"; }}
  }}"
    );
}

fn header_with_templates(
    lib_name: &str,
    extra_header: &str,
    s0: f64,
    s1: f64,
    c0: f64,
    c1: f64,
) -> String {
    format!(
        "library ({lib_name}) {{
  delay_model : table_lookup;
{extra_header}  lu_table_template (lut_2x2) {{
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1 (\"{s0:.4}, {s1:.4}\");
    index_2 (\"{c0:.4}, {c1:.4}\");
  }}
  lu_table_template (chk_2x2) {{
    variable_1 : constrained_pin_transition;
    variable_2 : related_pin_transition;
    index_1 (\"{s0:.4}, {s1:.4}\");
    index_2 (\"{s0:.4}, {s1:.4}\");
  }}
"
    )
}

/// `wt_15`: 15+ cell closed-form library with 1ns / 1pF / 1kohm units and affine 2x2 tables.
#[must_use]
pub fn wt_15() -> String {
    let mut out = header_with_templates(
        "wt_15",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    out.push_str("}\n");
    out
}

/// `wt_units`: closed-form library with `100ps`, `10fF`, `10kohm` units (scaled so physical values
/// match `wt_15`).
#[must_use]
pub fn wt_units() -> String {
    let mut out = header_with_templates(
        "wt_units",
        "  time_unit : \"100ps\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"10kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (10, ff);\n",
        1.0,
        5.0,
        1.0,
        5.0,
    );
    push_base_cells(&mut out, 10.0, 100.0, 0.0);
    out.push_str("}\n");
    out
}

/// `wt_thresholds`: closed-form library with explicit 20/80 slew trip points and `0.5` slew derate.
#[must_use]
pub fn wt_thresholds() -> String {
    let mut out = header_with_templates(
        "wt_thresholds",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n  slew_lower_threshold_pct_rise : 20.0;\n  slew_upper_threshold_pct_rise : 80.0;\n  slew_lower_threshold_pct_fall : 20.0;\n  slew_upper_threshold_pct_fall : 80.0;\n  slew_derate_from_library : 0.5;\n  input_threshold_pct_rise : 50.0;\n  input_threshold_pct_fall : 50.0;\n  output_threshold_pct_rise : 50.0;\n  output_threshold_pct_fall : 50.0;\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    out.push_str("}\n");
    out
}

/// `wt_wireload`: closed-form library with `wire_load` groups, piecewise-linear `fanout_length`,
/// and `wire_load_selection`.
#[must_use]
pub fn wt_wireload() -> String {
    let extra = "  time_unit : \"1ns\";
  voltage_unit : \"1V\";
  current_unit : \"1mA\";
  pulling_resistance_unit : \"1kohm\";
  leakage_power_unit : \"1nW\";
  capacitive_load_unit (1, pf);
  wire_load (\"wl_small\") {
    resistance : 0.005;
    capacitance : 0.002;
    area : 0.1;
    slope : 20.0;
    fanout_length (1, 10.0);
    fanout_length (2, 25.0);
    fanout_length (4, 60.0);
  }
  wire_load (\"wl_medium\") {
    resistance : 0.008;
    capacitance : 0.004;
    area : 0.2;
    slope : 30.0;
    fanout_length (1, 15.0);
    fanout_length (2, 40.0);
    fanout_length (4, 90.0);
  }
  wire_load (\"wl_large\") {
    resistance : 0.010;
    capacitance : 0.006;
    area : 0.4;
    slope : 50.0;
    fanout_length (1, 25.0);
    fanout_length (2, 60.0);
    fanout_length (4, 140.0);
  }
  wire_load_selection (\"sel_area\") {
    wire_load_from_area (0.0, 500.0, \"wl_small\");
    wire_load_from_area (500.0, 2000.0, \"wl_medium\");
    wire_load_from_area (2000.0, 100000.0, \"wl_large\");
  }
  default_wire_load : \"wl_small\";
  default_wire_load_selection : \"sel_area\";
  default_wire_load_mode : \"top\";
";
    let mut out = header_with_templates("wt_wireload", extra, 0.1, 0.5, 0.01, 0.05);
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    out.push_str("}\n");
    out
}

/// `wt_cond`: closed-form library with conditional (`when`) timing groups alongside unconditional
/// default timing groups.
#[must_use]
pub fn wt_cond() -> String {
    let mut out = header_with_templates(
        "wt_cond",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    let d_fast = AffineLut2x2 {
        d0: 0.04,
        ..AffineLut2x2::DEFAULT_DELAY
    }
    .format_values(1.0);
    let d_slow = AffineLut2x2 {
        d0: 0.08,
        ..AffineLut2x2::DEFAULT_DELAY
    }
    .format_values(1.0);
    let s_val = AffineLut2x2::DEFAULT_SLEW.format_values(1.0);
    let _ = writeln!(
        out,
        "  cell (wt_xor2_cond) {{
    area : 3.0;
    pin (A) {{ direction : input; capacitance : 0.0100; }}
    pin (B) {{ direction : input; capacitance : 0.0100; }}
    pin (Y) {{
      direction : output;
      function : \"A ^ B\";
      timing () {{
        related_pin : \"A\";
        when : \"!B\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({d_fast}); }}
        cell_fall (lut_2x2) {{ values ({d_fast}); }}
        rise_transition (lut_2x2) {{ values ({s_val}); }}
        fall_transition (lut_2x2) {{ values ({s_val}); }}
      }}
      timing () {{
        related_pin : \"A\";
        when : \"B\";
        timing_sense : negative_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({d_slow}); }}
        cell_fall (lut_2x2) {{ values ({d_slow}); }}
        rise_transition (lut_2x2) {{ values ({s_val}); }}
        fall_transition (lut_2x2) {{ values ({s_val}); }}
      }}
      timing () {{
        related_pin : \"A\";
        timing_sense : non_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({d_slow}); }}
        cell_fall (lut_2x2) {{ values ({d_slow}); }}
        rise_transition (lut_2x2) {{ values ({s_val}); }}
        fall_transition (lut_2x2) {{ values ({s_val}); }}
      }}
    }}
  }}"
    );
    out.push_str("}\n");
    out
}

/// `wt_tristate`: closed-form library with active-high and active-low tri-state drivers and bus
/// keeper cell.
#[must_use]
pub fn wt_tristate() -> String {
    let mut out = header_with_templates(
        "wt_tristate",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    let dv = AffineLut2x2::DEFAULT_DELAY.format_values(1.0);
    let sv = AffineLut2x2::DEFAULT_SLEW.format_values(1.0);
    let _ = writeln!(
        out,
        "  cell (wt_tbuf_n) {{
    area : 2.5;
    pin (A) {{ direction : input; capacitance : 0.0100; }}
    pin (OE_N) {{ direction : input; capacitance : 0.0100; }}
    pin (Y) {{
      direction : output;
      function : \"A\";
      three_state : \"OE_N\";
      timing () {{
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({dv}); }}
        cell_fall (lut_2x2) {{ values ({dv}); }}
        rise_transition (lut_2x2) {{ values ({sv}); }}
        fall_transition (lut_2x2) {{ values ({sv}); }}
      }}
      timing () {{
        related_pin : \"OE_N\";
        timing_sense : negative_unate;
        timing_type : three_state_enable;
        cell_rise (lut_2x2) {{ values ({dv}); }}
        cell_fall (lut_2x2) {{ values ({dv}); }}
        rise_transition (lut_2x2) {{ values ({sv}); }}
        fall_transition (lut_2x2) {{ values ({sv}); }}
      }}
    }}
  }}
  cell (wt_keeper) {{
    area : 1.0;
    pin (A) {{ direction : input; capacitance : 0.0050; }}
    pin (Y) {{
      direction : output;
      function : \"A\";
      timing () {{
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{ values ({dv}); }}
        cell_fall (lut_2x2) {{ values ({dv}); }}
        rise_transition (lut_2x2) {{ values ({sv}); }}
        fall_transition (lut_2x2) {{ values ({sv}); }}
      }}
    }}
  }}"
    );
    out.push_str("}\n");
    out
}

/// `wt_outcap`: closed-form library with nonzero output-pin self-capacitance (`0.005 pF`).
#[must_use]
pub fn wt_outcap() -> String {
    let mut out = header_with_templates(
        "wt_outcap",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.005);
    out.push_str("}\n");
    out
}

/// `wt_3d`: closed-form library including a 3-D NLDM lookup table (`variable_3 :
/// related_out_total_output_net_capacitance`).
#[must_use]
pub fn wt_3d() -> String {
    let extra = "  time_unit : \"1ns\";
  voltage_unit : \"1V\";
  current_unit : \"1mA\";
  pulling_resistance_unit : \"1kohm\";
  leakage_power_unit : \"1nW\";
  capacitive_load_unit (1, pf);
  lu_table_template (lut_3d) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    variable_3 : related_out_total_output_net_capacitance;
    index_1 (\"0.1000, 0.5000\");
    index_2 (\"0.0100, 0.0500\");
    index_3 (\"0.0100, 0.0500\");
  }
";
    let mut out = header_with_templates("wt_3d", extra, 0.1, 0.5, 0.01, 0.05);
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    let _ = writeln!(
        out,
        "  cell (wt_buf_3d) {{
    area : 2.0;
    pin (A) {{ direction : input; capacitance : 0.0100; }}
    pin (Y) {{
      direction : output;
      function : \"A\";
      timing () {{
        related_pin : \"A\";
        timing_sense : positive_unate;
        timing_type : combinational;
        cell_rise (lut_3d) {{
          values (
            \"0.0500, 0.1300\", \"0.1500, 0.2300\",
            \"0.0700, 0.1500\", \"0.1700, 0.2500\"
          );
        }}
        cell_fall (lut_3d) {{
          values (
            \"0.0500, 0.1300\", \"0.1500, 0.2300\",
            \"0.0700, 0.1500\", \"0.1700, 0.2500\"
          );
        }}
      }}
    }}
  }}"
    );
    out.push_str("}\n");
    out
}

/// `wt_nonmono`: closed-form library including a cell with a non-monotonic `index_1`.
#[must_use]
pub fn wt_nonmono() -> String {
    let mut out = header_with_templates(
        "wt_nonmono",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.1,
        0.5,
        0.01,
        0.05,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    let _ = writeln!(
        out,
        "  cell (wt_inv_nonmono) {{
    area : 1.0;
    pin (A) {{ direction : input; capacitance : 0.0100; }}
    pin (Y) {{
      direction : output;
      function : \"!A\";
      timing () {{
        related_pin : \"A\";
        timing_sense : negative_unate;
        timing_type : combinational;
        cell_rise (lut_2x2) {{
          index_1 (\"0.5000, 0.1000\");
          values (\"0.0500, 0.1300\", \"0.1500, 0.2300\");
        }}
      }}
    }}
  }}"
    );
    out.push_str("}\n");
    out
}

/// `wt_extrap`: closed-form library with tight breakpoints (`[0.10, 0.20]` x `[0.01, 0.02]`) so
/// slews and loads below or above the table axes exercise linear extrapolation.
#[must_use]
pub fn wt_extrap() -> String {
    let mut out = header_with_templates(
        "wt_extrap",
        "  time_unit : \"1ns\";\n  voltage_unit : \"1V\";\n  current_unit : \"1mA\";\n  pulling_resistance_unit : \"1kohm\";\n  leakage_power_unit : \"1nW\";\n  capacitive_load_unit (1, pf);\n",
        0.10,
        0.20,
        0.01,
        0.02,
    );
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    out.push_str("}\n");
    out
}

/// `wt_gfconv`: closed-form library exercising gf180mcu conventions (`pg_pin`, `leakage_power`,
/// `internal_power`, `dont_use`, `is_filler_cell`, `always_on`, `is_isolation_cell`).
#[must_use]
pub fn wt_gfconv() -> String {
    let extra = "  time_unit : \"1ns\";
  voltage_unit : \"1V\";
  current_unit : \"1mA\";
  pulling_resistance_unit : \"1kohm\";
  leakage_power_unit : \"1nW\";
  capacitive_load_unit (1, pf);
  power_lut_template (pwr_2x2) {
    variable_1 : input_transition_time;
    variable_2 : total_output_net_capacitance;
    index_1 (\"0.1000, 0.5000\");
    index_2 (\"0.0100, 0.0500\");
  }
";
    let mut out = header_with_templates("wt_gfconv", extra, 0.1, 0.5, 0.01, 0.05);
    push_base_cells(&mut out, 1.0, 1.0, 0.0);
    let _ = writeln!(
        out,
        "  cell (wt_fill_1) {{
    area : 1.0;
    is_filler_cell : true;
    dont_use : true;
    pg_pin (VDD) {{ pg_type : primary_power; voltage_name : \"VDD\"; }}
    pg_pin (VSS) {{ pg_type : primary_ground; voltage_name : \"VSS\"; }}
  }}
  cell (wt_aon_buf) {{
    area : 2.0;
    always_on : true;
    is_isolation_cell : true;
    pg_pin (VDD) {{ pg_type : primary_power; voltage_name : \"VDD\"; }}
    pg_pin (VSS) {{ pg_type : primary_ground; voltage_name : \"VSS\"; }}
    leakage_power () {{ when : \"!A\"; value : 0.25; }}
    pin (A) {{ direction : input; capacitance : 0.0100; }}
    pin (Y) {{
      direction : output;
      function : \"A\";
      internal_power () {{
        related_pin : \"A\";
        rise_power (pwr_2x2) {{ values (\"0.01, 0.02\", \"0.03, 0.04\"); }}
        fall_power (pwr_2x2) {{ values (\"0.01, 0.02\", \"0.03, 0.04\"); }}
      }}
    }}
  }}"
    );
    out.push_str("}\n");
    out
}
