// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent Liberty library and corner timing model for workspace revisions
//! (STA-REQ-0010, 0012 to 0016, 0112 to 0114, 0120 to 0125).

use std::collections::BTreeMap;

use rusteda_core::liberty::{
    Cell, Direction, Func, Library, Pin, SeqKind, Table, TableVariable, Template, TimingArc,
    TimingSense, TimingType, WireLoad, WireLoadMode, WireLoadSelection,
};

/// Signal transition direction (`rise` or `fall`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Trans {
    Rise = 0,
    Fall = 1,
}

impl Trans {
    pub const ALL: [Trans; 2] = [Trans::Rise, Trans::Fall];

    #[must_use]
    pub const fn idx(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn flip(self) -> Trans {
        match self {
            Trans::Rise => Trans::Fall,
            Trans::Fall => Trans::Rise,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Trans::Rise => "rise",
            Trans::Fall => "fall",
        }
    }
}

/// Analysis bound (`early` / min or `late` / max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Analysis {
    Early = 0,
    Late = 1,
}

impl Analysis {
    pub const ALL: [Analysis; 2] = [Analysis::Early, Analysis::Late];

    #[must_use]
    pub const fn idx(self) -> usize {
        self as usize
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Analysis::Early => "early",
            Analysis::Late => "late",
        }
    }
}

/// Axis variable role for a 1D, 2D or 3D NLDM lookup table (STA-REQ-0014, 0114).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisVar {
    InputSlew,
    OutputCap,
    WireLength,
    ConstrainedSlew,
    RelatedSlew,
    RelatedOutCap,
}

/// Independent 1D/2D/3D multilinear lookup table in canonical units (`ns`, `pF`).
#[derive(Debug, Clone)]
pub struct Lut {
    pub vars: [Option<AxisVar>; 3],
    pub index_1: Vec<f64>,
    pub index_2: Vec<f64>,
    pub index_3: Vec<f64>,
    pub values: Vec<f64>,
    pub time_scale: f64,
    pub cap_scale: f64,
}

fn axis_weights(idx: &[f64], x: f64) -> (usize, usize, f64) {
    match idx.len() {
        0 | 1 => (0, 0, 0.0),
        n => {
            let mut k = 0;
            while k + 2 < n && x > idx[k + 1] {
                k += 1;
            }
            let (x0, x1) = (idx[k], idx[k + 1]);
            let t = if x1 == x0 { 0.0 } else { (x - x0) / (x1 - x0) };
            (k, k + 1, t)
        }
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn strictly_increasing(idx: &[f64]) -> bool {
    idx.windows(2).all(|w| w[0] < w[1])
}

impl Lut {
    /// Builds a delay/transition table (`is_constraint = false`) or constraint table
    /// (`is_constraint = true`), validating strictly increasing index vectors (STA-REQ-0114).
    pub fn from_table(
        t: &Table,
        templates: &BTreeMap<String, Template>,
        time_scale: f64,
        cap_scale: f64,
        is_constraint: bool,
    ) -> Result<Self, String> {
        let tpl = templates.get(&t.template);
        let idx1 = if !t.index_1.is_empty() {
            t.index_1.clone()
        } else {
            tpl.map(|tp| tp.index_1.clone()).unwrap_or_default()
        };
        let idx2 = if !t.index_2.is_empty() {
            t.index_2.clone()
        } else {
            tpl.map(|tp| tp.index_2.clone()).unwrap_or_default()
        };
        let idx3 = if !t.index_3.is_empty() {
            t.index_3.clone()
        } else {
            tpl.map(|tp| tp.index_3.clone()).unwrap_or_default()
        };

        if !strictly_increasing(&idx1) || !strictly_increasing(&idx2) || !strictly_increasing(&idx3)
        {
            return Err(format!(
                "lookup table '{}' has non-increasing index points",
                t.template
            ));
        }

        let raw_var = |axis: usize| -> Option<TableVariable> {
            t.variable(axis)
                .or_else(|| tpl.and_then(|tp| tp.variable(axis)))
        };

        let map_var = |v: Option<TableVariable>, default: Option<AxisVar>| -> Option<AxisVar> {
            match v {
                Some(TableVariable::InputNetTransition | TableVariable::InputTransitionTime) => {
                    Some(if is_constraint {
                        AxisVar::RelatedSlew
                    } else {
                        AxisVar::InputSlew
                    })
                }
                Some(TableVariable::RelatedPinTransition) => Some(if is_constraint {
                    AxisVar::RelatedSlew
                } else {
                    AxisVar::InputSlew
                }),
                Some(TableVariable::ConstrainedPinTransition) => Some(AxisVar::ConstrainedSlew),
                Some(
                    TableVariable::TotalOutputNetCapacitance
                    | TableVariable::EqualOrOppositeOutputNetCapacitance
                    | TableVariable::OutputNetWireCap
                    | TableVariable::OutputNetPinCap,
                ) => Some(AxisVar::OutputCap),
                Some(TableVariable::RelatedOutTotalOutputNetCapacitance) => {
                    Some(if is_constraint {
                        AxisVar::RelatedOutCap
                    } else {
                        AxisVar::OutputCap
                    })
                }
                Some(TableVariable::OutputNetLength | TableVariable::RelatedOutOutputNetLength) => {
                    Some(AxisVar::WireLength)
                }
                _ => default,
            }
        };

        let v1_raw = raw_var(1);
        let v2_raw = raw_var(2);
        let v3_raw = raw_var(3);

        let vars = if is_constraint {
            let def1 = AxisVar::ConstrainedSlew;
            let def2 = AxisVar::RelatedSlew;
            let a1 = map_var(v1_raw.clone(), Some(def1));
            let a2 = if idx2.is_empty() && v2_raw.is_none() {
                if a1 == Some(AxisVar::RelatedSlew) {
                    Some(AxisVar::ConstrainedSlew)
                } else {
                    Some(def2)
                }
            } else {
                let d2 = if a1 == Some(AxisVar::RelatedSlew) {
                    AxisVar::ConstrainedSlew
                } else {
                    def2
                };
                map_var(v2_raw, Some(d2))
            };
            let a3 = if idx3.is_empty() && v3_raw.is_none() {
                None
            } else {
                map_var(v3_raw, Some(AxisVar::RelatedOutCap))
            };
            [a1, a2, a3]
        } else {
            let a1 = map_var(v1_raw, Some(AxisVar::InputSlew));
            let d2 = if a1 == Some(AxisVar::OutputCap) {
                AxisVar::InputSlew
            } else {
                AxisVar::OutputCap
            };
            let a2 = map_var(v2_raw, Some(d2));
            let a3 = if idx3.is_empty() && v3_raw.is_none() {
                None
            } else {
                map_var(v3_raw, Some(AxisVar::WireLength))
            };
            [a1, a2, a3]
        };

        Ok(Self {
            vars,
            index_1: idx1,
            index_2: idx2,
            index_3: idx3,
            values: t.values.clone(),
            time_scale,
            cap_scale,
        })
    }

    fn at3(&self, i: usize, j: usize, k: usize) -> f64 {
        if self.values.len() == 1 {
            return self.values[0];
        }
        let n2 = self.index_2.len().max(1);
        let n3 = self.index_3.len().max(1);
        self.values
            .get((i * n2 + j) * n3 + k)
            .copied()
            .unwrap_or(0.0)
    }

    fn eval_raw(&self, x1: f64, x2: f64, x3: f64) -> f64 {
        let n1 = self.index_1.len().max(1);
        let n2 = self.index_2.len().max(1);
        let n3 = self.index_3.len().max(1);
        if self.values.is_empty() || (self.values.len() != 1 && self.values.len() != n1 * n2 * n3) {
            return 0.0;
        }
        let (i0, i1, t) = axis_weights(&self.index_1, x1);
        let (j0, j1, u) = axis_weights(&self.index_2, x2);
        let (k0, k1, w) = axis_weights(&self.index_3, x3);
        let along3 = |i: usize, j: usize| lerp(self.at3(i, j, k0), self.at3(i, j, k1), w);
        let a = lerp(along3(i0, j0), along3(i0, j1), u);
        let b = lerp(along3(i1, j0), along3(i1, j1), u);
        lerp(a, b, t)
    }

    fn arg_for(&self, v: Option<AxisVar>, slew_ns: f64, load_pf: f64, wire_len: f64) -> f64 {
        match v {
            Some(AxisVar::InputSlew | AxisVar::RelatedSlew | AxisVar::ConstrainedSlew) | None => {
                slew_ns / self.time_scale
            }
            Some(AxisVar::OutputCap | AxisVar::RelatedOutCap) => load_pf / self.cap_scale,
            Some(AxisVar::WireLength) => wire_len,
        }
    }

    /// Evaluates a cell delay table in `ns` (never clamped at zero, STA-REQ-0120).
    #[must_use]
    pub fn lookup_delay(&self, slew_ns: f64, load_pf: f64, wire_len: f64) -> f64 {
        let x1 = self.arg_for(self.vars[0], slew_ns, load_pf, wire_len);
        let x2 = self.arg_for(self.vars[1], slew_ns, load_pf, wire_len);
        let x3 = self.arg_for(self.vars[2], slew_ns, load_pf, wire_len);
        self.eval_raw(x1, x2, x3) * self.time_scale
    }

    /// Evaluates an output transition table in `ns`, clamping negative extrapolated slews to
    /// `0.0` (STA-REQ-0120).
    #[must_use]
    pub fn lookup_slew(&self, slew_ns: f64, load_pf: f64, wire_len: f64) -> f64 {
        self.lookup_delay(slew_ns, load_pf, wire_len).max(0.0)
    }

    /// Evaluates a timing constraint table in `ns` (never clamped at zero, STA-REQ-0120).
    #[must_use]
    pub fn lookup_constraint(
        &self,
        data_slew_ns: f64,
        clk_slew_ns: f64,
        related_out_cap_pf: f64,
    ) -> f64 {
        let map = |v: Option<AxisVar>| match v {
            Some(AxisVar::RelatedSlew) => clk_slew_ns / self.time_scale,
            Some(AxisVar::RelatedOutCap | AxisVar::OutputCap) => {
                related_out_cap_pf / self.cap_scale
            }
            Some(AxisVar::ConstrainedSlew | AxisVar::InputSlew) | None => {
                data_slew_ns / self.time_scale
            }
            Some(AxisVar::WireLength) => 0.0,
        };
        self.eval_raw(map(self.vars[0]), map(self.vars[1]), map(self.vars[2])) * self.time_scale
    }
}

/// Combinational or three-state arc sub-kind (STA-REQ-0013, 0223).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombArcKind {
    Combinational,
    CombinationalRise,
    CombinationalFall,
    ThreeStateEnable,
    ThreeStateEnableRise,
    ThreeStateEnableFall,
    ThreeStateDisable,
    ThreeStateDisableRise,
    ThreeStateDisableFall,
}

impl CombArcKind {
    #[must_use]
    pub const fn allows_out(self, tr_out: Trans) -> bool {
        match self {
            Self::Combinational | Self::ThreeStateEnable | Self::ThreeStateDisable => true,
            Self::CombinationalRise | Self::ThreeStateEnableRise | Self::ThreeStateDisableRise => {
                matches!(tr_out, Trans::Rise)
            }
            Self::CombinationalFall | Self::ThreeStateEnableFall | Self::ThreeStateDisableFall => {
                matches!(tr_out, Trans::Fall)
            }
        }
    }

    #[must_use]
    pub const fn is_three_state(self) -> bool {
        matches!(
            self,
            Self::ThreeStateEnable
                | Self::ThreeStateEnableRise
                | Self::ThreeStateEnableFall
                | Self::ThreeStateDisable
                | Self::ThreeStateDisableRise
                | Self::ThreeStateDisableFall
        )
    }

    #[must_use]
    pub const fn is_three_state_disable(self) -> bool {
        matches!(
            self,
            Self::ThreeStateDisable | Self::ThreeStateDisableRise | Self::ThreeStateDisableFall
        )
    }
}

/// Combinational or three-state propagation arc (`from_pin -> to_pin`).
#[derive(Debug, Clone)]
pub struct CombArc {
    pub from_pin: String,
    pub to_pin: String,
    pub kind: CombArcKind,
    pub sense: TimingSense,
    pub when: Option<Func>,
    pub cell_rise: Option<Lut>,
    pub cell_fall: Option<Lut>,
    pub rise_transition: Option<Lut>,
    pub fall_transition: Option<Lut>,
}

impl CombArc {
    #[must_use]
    pub fn can_propagate(&self, sense: &TimingSense, in_tr: Trans, out_tr: Trans) -> bool {
        if !self.kind.allows_out(out_tr) {
            return false;
        }
        let has_tbl = match out_tr {
            Trans::Rise => self.cell_rise.is_some(),
            Trans::Fall => self.cell_fall.is_some(),
        };
        if !has_tbl {
            return false;
        }
        if self.kind.is_three_state() {
            return match sense {
                TimingSense::PositiveUnate => matches!(in_tr, Trans::Rise),
                TimingSense::NegativeUnate => matches!(in_tr, Trans::Fall),
                _ => true,
            };
        }
        match sense {
            TimingSense::PositiveUnate => in_tr == out_tr,
            TimingSense::NegativeUnate => in_tr != out_tr,
            _ => true,
        }
    }

    #[must_use]
    pub fn eval(&self, tr_out: Trans, slew_in: f64, load_pf: f64) -> Option<(f64, f64)> {
        if !self.kind.allows_out(tr_out) {
            return None;
        }
        let (d_lut, s_lut) = match tr_out {
            Trans::Rise => (&self.cell_rise, &self.rise_transition),
            Trans::Fall => (&self.cell_fall, &self.fall_transition),
        };
        let d = d_lut.as_ref()?.lookup_delay(slew_in, load_pf, 0.0);
        let s = s_lut
            .as_ref()
            .map_or(0.0, |lu| lu.lookup_slew(slew_in, load_pf, 0.0));
        Some((d, s))
    }

    #[must_use]
    pub fn delay(&self, tr_out: Trans, slew_in: f64, load_pf: f64, wire_len: f64) -> f64 {
        let t = match tr_out {
            Trans::Rise => &self.cell_rise,
            Trans::Fall => &self.cell_fall,
        };
        t.as_ref()
            .map_or(0.0, |lu| lu.lookup_delay(slew_in, load_pf, wire_len))
    }

    #[must_use]
    pub fn slew(&self, tr_out: Trans, slew_in: f64, load_pf: f64, wire_len: f64) -> f64 {
        let t = match tr_out {
            Trans::Rise => &self.rise_transition,
            Trans::Fall => &self.fall_transition,
        };
        t.as_ref()
            .map_or(0.0, |lu| lu.lookup_slew(slew_in, load_pf, wire_len))
    }
}

/// Sequential edge launch arc (`rising_edge` / `falling_edge`).
#[derive(Debug, Clone)]
pub struct LaunchArc {
    pub clock_pin: String,
    pub to_pin: String,
    pub clock_edge: Trans,
    pub when: Option<Func>,
    pub cell_rise: Option<Lut>,
    pub cell_fall: Option<Lut>,
    pub rise_transition: Option<Lut>,
    pub fall_transition: Option<Lut>,
}

impl LaunchArc {
    #[must_use]
    pub fn eval(&self, tr_out: Trans, slew_in: f64, load_pf: f64) -> Option<(f64, f64)> {
        let (d_lut, s_lut) = match tr_out {
            Trans::Rise => (&self.cell_rise, &self.rise_transition),
            Trans::Fall => (&self.cell_fall, &self.fall_transition),
        };
        let d = d_lut.as_ref()?.lookup_delay(slew_in, load_pf, 0.0);
        let s = s_lut
            .as_ref()
            .map_or(0.0, |lu| lu.lookup_slew(slew_in, load_pf, 0.0));
        Some((d, s))
    }

    #[must_use]
    pub fn delay(&self, tr_out: Trans, slew_in: f64, load_pf: f64, wire_len: f64) -> f64 {
        let t = match tr_out {
            Trans::Rise => &self.cell_rise,
            Trans::Fall => &self.cell_fall,
        };
        t.as_ref()
            .map_or(0.0, |lu| lu.lookup_delay(slew_in, load_pf, wire_len))
    }

    #[must_use]
    pub fn slew(&self, tr_out: Trans, slew_in: f64, load_pf: f64, wire_len: f64) -> f64 {
        let t = match tr_out {
            Trans::Rise => &self.rise_transition,
            Trans::Fall => &self.fall_transition,
        };
        t.as_ref()
            .map_or(0.0, |lu| lu.lookup_slew(slew_in, load_pf, wire_len))
    }
}

/// Kind of a sequential timing check arc (STA-REQ-0013, 0211, 0224).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckArcKind {
    Setup,
    Hold,
    Recovery,
    Removal,
}

impl CheckArcKind {
    #[must_use]
    pub const fn is_setup_like(self) -> bool {
        matches!(self, Self::Setup | Self::Recovery)
    }

    #[must_use]
    pub const fn is_async(self) -> bool {
        matches!(self, Self::Recovery | Self::Removal)
    }
}

/// Sequential timing check arc (`setup_*`, `hold_*`, `recovery_*`, `removal_*`).
#[derive(Debug, Clone)]
pub struct CheckArc {
    pub data_pin: String,
    pub clock_pin: String,
    pub clock_edge: Trans,
    pub kind: CheckArcKind,
    pub when: Option<Func>,
    pub rise_constraint: Option<Lut>,
    pub fall_constraint: Option<Lut>,
}

impl CheckArc {
    #[must_use]
    pub fn constraint_opt(
        &self,
        data_tr: Trans,
        data_slew_ns: f64,
        clk_slew_ns: f64,
        related_out_cap_pf: f64,
    ) -> Option<f64> {
        let t = match data_tr {
            Trans::Rise => &self.rise_constraint,
            Trans::Fall => &self.fall_constraint,
        };
        t.as_ref()
            .map(|lu| lu.lookup_constraint(data_slew_ns, clk_slew_ns, related_out_cap_pf))
    }

    #[must_use]
    pub fn constraint(
        &self,
        data_tr: Trans,
        data_slew_ns: f64,
        clk_slew_ns: f64,
        related_out_cap_pf: f64,
    ) -> f64 {
        self.constraint_opt(data_tr, data_slew_ns, clk_slew_ns, related_out_cap_pf)
            .unwrap_or(0.0)
    }
}

/// Transparent latch descriptor (STA-REQ-0221, 0222).
#[derive(Debug, Clone)]
pub struct LatchDef {
    pub data_pin: String,
    pub enable_pin: String,
    pub q_pin: String,
    /// Active level of `enable_pin` (`Trans::Rise` for active-high, `Trans::Fall` for active-low).
    pub active_level: Trans,
}

impl LatchDef {
    #[must_use]
    pub const fn open_edge(&self) -> Trans {
        self.active_level
    }

    #[must_use]
    pub const fn close_edge(&self) -> Trans {
        self.active_level.flip()
    }
}

/// Per-pin definition in canonical units (`pF`).
#[derive(Debug, Clone)]
pub struct PinDef {
    pub name: String,
    pub direction: Direction,
    /// Capacitance `[early, late]` for `Rise` and `Fall` in `pF` (STA-REQ-0110).
    pub cap_pf: [[f64; 2]; 2],
    pub function: Option<Func>,
    pub three_state: Option<Func>,
    pub is_clock: bool,
    pub clock_gate_clock_pin: bool,
    pub clock_gate_enable_pin: bool,
    pub clock_gate_out_pin: bool,
}

impl PinDef {
    #[must_use]
    pub const fn is_three_state(&self) -> bool {
        self.three_state.is_some()
    }

    #[must_use]
    pub const fn cap(&self, tr: Trans, an: Analysis) -> f64 {
        self.cap_pf[tr.idx()][an.idx()]
    }
}

/// Compiled cell definition for one corner.
#[derive(Debug, Clone)]
pub struct CellDef {
    pub name: String,
    pub area: f64,
    pub pins: BTreeMap<String, PinDef>,
    pub comb_arcs: Vec<CombArc>,
    pub launch_arcs: Vec<LaunchArc>,
    pub check_arcs: Vec<CheckArc>,
    pub latches: Vec<LatchDef>,
    pub is_clock_gating: bool,
    pub unanalyzed_arcs: usize,
}

impl CellDef {
    #[must_use]
    pub fn latch_for_d(&self, d_pin: &str) -> Option<&LatchDef> {
        self.latches.iter().find(|l| l.data_pin == d_pin)
    }

    #[must_use]
    pub fn latch_for_q(&self, q_pin: &str) -> Option<&LatchDef> {
        self.latches.iter().find(|l| l.q_pin == q_pin)
    }

    #[must_use]
    pub fn is_latch_dq(&self, from_pin: &str, to_pin: &str) -> bool {
        self.latches
            .iter()
            .any(|l| l.data_pin == from_pin && l.q_pin == to_pin)
    }
}

/// Trip-point thresholds and slew derate of a corner (STA-REQ-0016).
#[derive(Debug, Clone, PartialEq)]
pub struct CornerThresholds {
    pub input_rise: f64,
    pub input_fall: f64,
    pub output_rise: f64,
    pub output_fall: f64,
    pub slew_lower_rise: f64,
    pub slew_upper_rise: f64,
    pub slew_lower_fall: f64,
    pub slew_upper_fall: f64,
    pub slew_derate: f64,
}

impl CornerThresholds {
    #[must_use]
    pub fn from_lib(lib: &Library) -> Self {
        Self {
            input_rise: lib.input_threshold_pct_rise.unwrap_or(50.0),
            input_fall: lib.input_threshold_pct_fall.unwrap_or(50.0),
            output_rise: lib.output_threshold_pct_rise.unwrap_or(50.0),
            output_fall: lib.output_threshold_pct_fall.unwrap_or(50.0),
            slew_lower_rise: lib.slew_lower_threshold_pct_rise.unwrap_or(20.0),
            slew_upper_rise: lib.slew_upper_threshold_pct_rise.unwrap_or(80.0),
            slew_lower_fall: lib.slew_lower_threshold_pct_fall.unwrap_or(20.0),
            slew_upper_fall: lib.slew_upper_threshold_pct_fall.unwrap_or(80.0),
            slew_derate: lib.slew_derate_from_library.unwrap_or(1.0),
        }
    }
}

/// Compiled corner library view (STA-REQ-0015, 0016).
#[derive(Debug, Clone)]
pub struct CornerLib {
    pub corner: String,
    pub time_unit_ns: f64,
    pub cap_unit_pf: f64,
    pub thresholds: CornerThresholds,
    pub cells: BTreeMap<String, CellDef>,
    pub wire_loads: BTreeMap<String, WireLoad>,
    pub wire_load_selections: BTreeMap<String, WireLoadSelection>,
    pub default_wire_load: Option<String>,
    pub default_wire_load_selection: Option<String>,
    pub default_wire_load_mode: Option<WireLoadMode>,
}

/// Derives unateness of `func` with respect to `var` by Shannon cofactors (STA-REQ-0012).
#[must_use]
pub fn sense_from_func(func: Option<&Func>, var: &str) -> TimingSense {
    let Some(f) = func else {
        return TimingSense::NonUnate;
    };
    let mut vars = Vec::new();
    collect_vars(f, &mut vars);
    if !vars.iter().any(|v| v == var) || vars.len() > 10 {
        return TimingSense::NonUnate;
    }
    let others: Vec<&str> = vars
        .iter()
        .map(String::as_str)
        .filter(|&v| v != var)
        .collect();
    let mut pos = false;
    let mut neg = false;
    for mask in 0..(1usize << others.len()) {
        let eval_with = |val: bool| {
            f.eval(&|name| {
                if name == var {
                    Some(val)
                } else {
                    let idx = others.iter().position(|&o| o == name)?;
                    Some(((mask >> idx) & 1) != 0)
                }
            })
        };
        if let (Some(v0), Some(v1)) = (eval_with(false), eval_with(true)) {
            if !v0 && v1 {
                pos = true;
            } else if v0 && !v1 {
                neg = true;
            }
        } else {
            return TimingSense::NonUnate;
        }
    }
    match (pos, neg) {
        (true, false) => TimingSense::PositiveUnate,
        (false, true) => TimingSense::NegativeUnate,
        _ => TimingSense::NonUnate,
    }
}

pub(crate) fn collect_vars(f: &Func, out: &mut Vec<String>) {
    match f {
        Func::Const(_) => {}
        Func::Var(s) => {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
        Func::Not(a) => collect_vars(a, out),
        Func::And(a, b) | Func::Or(a, b) | Func::Xor(a, b) => {
            collect_vars(a, out);
            collect_vars(b, out);
        }
    }
}

fn pin_caps_pf(lib: &Library, p: &Pin, cu: f64) -> [[f64; 2]; 2] {
    let base = lib.pin_capacitance(p);
    let r_nom = p.rise_capacitance.or(p.fall_capacitance).unwrap_or(base) * cu;
    let f_nom = p.fall_capacitance.or(p.rise_capacitance).unwrap_or(base) * cu;
    let r = match p.rise_capacitance_range {
        Some((lo, hi)) => [lo * cu, hi * cu],
        None => [r_nom, r_nom],
    };
    let f = match p.fall_capacitance_range {
        Some((lo, hi)) => [lo * cu, hi * cu],
        None => [f_nom, f_nom],
    };
    [r, f]
}

fn build_lut_opt(
    t: &Option<Table>,
    templates: &BTreeMap<String, Template>,
    tu: f64,
    cu: f64,
    is_constraint: bool,
) -> Result<Option<Lut>, String> {
    t.as_ref()
        .map(|tbl| Lut::from_table(tbl, templates, tu, cu, is_constraint))
        .transpose()
}

fn build_cell(lib: &Library, c: &Cell, tu: f64, cu: f64) -> Result<CellDef, String> {
    let mut pins = BTreeMap::new();
    for p in &c.pins {
        pins.insert(
            p.name.clone(),
            PinDef {
                name: p.name.clone(),
                direction: p.direction,
                cap_pf: pin_caps_pf(lib, p, cu),
                function: p.function.clone(),
                three_state: p.three_state_function.clone().or_else(|| {
                    p.three_state
                        .as_ref()
                        .map(|s| Func::Var(s.trim().to_string()))
                }),
                is_clock: p.is_clock,
                clock_gate_clock_pin: p.clock_gate_clock_pin,
                clock_gate_enable_pin: p.clock_gate_enable_pin,
                clock_gate_out_pin: p.clock_gate_out_pin,
            },
        );
    }

    let mut comb_arcs = Vec::new();
    let mut launch_arcs = Vec::new();
    let mut check_arcs = Vec::new();
    let mut unanalyzed_arcs = 0usize;

    for p in &c.pins {
        for a in &p.arcs {
            if a.related_pin.is_empty() {
                continue;
            }
            classify_arc(
                lib,
                p,
                a,
                tu,
                cu,
                &mut comb_arcs,
                &mut launch_arcs,
                &mut check_arcs,
                &mut unanalyzed_arcs,
            )?;
        }
    }

    let latches = extract_latches(c, &comb_arcs, &launch_arcs, &check_arcs);

    Ok(CellDef {
        name: c.name.clone(),
        area: c.area,
        pins,
        comb_arcs,
        launch_arcs,
        check_arcs,
        latches,
        is_clock_gating: c.is_clock_gating,
        unanalyzed_arcs,
    })
}

#[allow(clippy::too_many_arguments)]
fn classify_arc(
    lib: &Library,
    p: &Pin,
    a: &TimingArc,
    tu: f64,
    cu: f64,
    comb_arcs: &mut Vec<CombArc>,
    launch_arcs: &mut Vec<LaunchArc>,
    check_arcs: &mut Vec<CheckArc>,
    unanalyzed: &mut usize,
) -> Result<(), String> {
    let kind = if a.timing_type.trim().is_empty() {
        TimingType::Combinational
    } else {
        a.kind()
    };

    let comb_kind = match kind {
        TimingType::Combinational => Some(CombArcKind::Combinational),
        TimingType::CombinationalRise => Some(CombArcKind::CombinationalRise),
        TimingType::CombinationalFall => Some(CombArcKind::CombinationalFall),
        TimingType::ThreeStateEnable => Some(CombArcKind::ThreeStateEnable),
        TimingType::ThreeStateEnableRise => Some(CombArcKind::ThreeStateEnableRise),
        TimingType::ThreeStateEnableFall => Some(CombArcKind::ThreeStateEnableFall),
        TimingType::ThreeStateDisable => Some(CombArcKind::ThreeStateDisable),
        TimingType::ThreeStateDisableRise => Some(CombArcKind::ThreeStateDisableRise),
        TimingType::ThreeStateDisableFall => Some(CombArcKind::ThreeStateDisableFall),
        _ => None,
    };

    if let Some(ck) = comb_kind {
        if ck.is_three_state() && p.three_state.is_none() && p.three_state_function.is_none() {
            return Ok(());
        }
        let sense = match a.sense() {
            Some(TimingSense::PositiveUnate) => TimingSense::PositiveUnate,
            Some(TimingSense::NegativeUnate) => TimingSense::NegativeUnate,
            Some(_) => TimingSense::NonUnate,
            None => {
                if ck.is_three_state() {
                    let ts_s = sense_from_func(p.three_state_function.as_ref(), &a.related_pin);
                    if ck.is_three_state_disable() {
                        ts_s
                    } else {
                        match ts_s {
                            TimingSense::PositiveUnate => TimingSense::NegativeUnate,
                            TimingSense::NegativeUnate => TimingSense::PositiveUnate,
                            other => other,
                        }
                    }
                } else {
                    sense_from_func(p.function.as_ref(), &a.related_pin)
                }
            }
        };
        comb_arcs.push(CombArc {
            from_pin: a.related_pin.clone(),
            to_pin: p.name.clone(),
            kind: ck,
            sense,
            when: a.when.clone(),
            cell_rise: build_lut_opt(&a.cell_rise, &lib.templates, tu, cu, false)?,
            cell_fall: build_lut_opt(&a.cell_fall, &lib.templates, tu, cu, false)?,
            rise_transition: build_lut_opt(&a.rise_transition, &lib.templates, tu, cu, false)?,
            fall_transition: build_lut_opt(&a.fall_transition, &lib.templates, tu, cu, false)?,
        });
        return Ok(());
    }

    match kind {
        TimingType::RisingEdge | TimingType::FallingEdge => {
            let clock_edge = if kind == TimingType::RisingEdge {
                Trans::Rise
            } else {
                Trans::Fall
            };
            launch_arcs.push(LaunchArc {
                clock_pin: a.related_pin.clone(),
                to_pin: p.name.clone(),
                clock_edge,
                when: a.when.clone(),
                cell_rise: build_lut_opt(&a.cell_rise, &lib.templates, tu, cu, false)?,
                cell_fall: build_lut_opt(&a.cell_fall, &lib.templates, tu, cu, false)?,
                rise_transition: build_lut_opt(&a.rise_transition, &lib.templates, tu, cu, false)?,
                fall_transition: build_lut_opt(&a.fall_transition, &lib.templates, tu, cu, false)?,
            });
        }
        TimingType::SetupRising
        | TimingType::SetupFalling
        | TimingType::HoldRising
        | TimingType::HoldFalling
        | TimingType::RecoveryRising
        | TimingType::RecoveryFalling
        | TimingType::RemovalRising
        | TimingType::RemovalFalling => {
            let (chk_kind, clock_edge) = match kind {
                TimingType::SetupRising => (CheckArcKind::Setup, Trans::Rise),
                TimingType::SetupFalling => (CheckArcKind::Setup, Trans::Fall),
                TimingType::HoldRising => (CheckArcKind::Hold, Trans::Rise),
                TimingType::HoldFalling => (CheckArcKind::Hold, Trans::Fall),
                TimingType::RecoveryRising => (CheckArcKind::Recovery, Trans::Rise),
                TimingType::RecoveryFalling => (CheckArcKind::Recovery, Trans::Fall),
                TimingType::RemovalRising => (CheckArcKind::Removal, Trans::Rise),
                TimingType::RemovalFalling => (CheckArcKind::Removal, Trans::Fall),
                _ => unreachable!(),
            };
            check_arcs.push(CheckArc {
                data_pin: p.name.clone(),
                clock_pin: a.related_pin.clone(),
                clock_edge,
                kind: chk_kind,
                when: a.when.clone(),
                rise_constraint: build_lut_opt(&a.rise_constraint, &lib.templates, tu, cu, true)?,
                fall_constraint: build_lut_opt(&a.fall_constraint, &lib.templates, tu, cu, true)?,
            });
        }
        TimingType::Preset | TimingType::Clear => {
            // STA-REQ-0013: async preset/clear arcs are retained and not propagated as data.
        }
        _ => {
            *unanalyzed += 1;
        }
    }
    Ok(())
}

fn extract_latches(
    c: &Cell,
    comb_arcs: &[CombArc],
    launch_arcs: &[LaunchArc],
    check_arcs: &[CheckArc],
) -> Vec<LatchDef> {
    let mut out = Vec::new();
    for seq in &c.sequentials {
        if seq.kind != SeqKind::Latch {
            continue;
        }
        let mut d_vars = Vec::new();
        collect_vars(&seq.data, &mut d_vars);
        let mut g_vars = Vec::new();
        collect_vars(&seq.clock, &mut g_vars);
        let (Some(d_pin), Some(g_pin)) = (d_vars.first(), g_vars.first()) else {
            continue;
        };
        let active_level = match sense_from_func(Some(&seq.clock), g_pin) {
            TimingSense::NegativeUnate => Trans::Fall,
            _ => Trans::Rise,
        };
        for p in &c.pins {
            if p.direction == Direction::Output {
                let mut f_vars = Vec::new();
                if let Some(f) = &p.function {
                    collect_vars(f, &mut f_vars);
                }
                if f_vars.iter().any(|v| v == &seq.state)
                    || comb_arcs
                        .iter()
                        .any(|ca| ca.from_pin == *d_pin && ca.to_pin == p.name)
                {
                    out.push(LatchDef {
                        data_pin: d_pin.clone(),
                        enable_pin: g_pin.clone(),
                        q_pin: p.name.clone(),
                        active_level,
                    });
                }
            }
        }
    }
    if out.is_empty() {
        for ca in comb_arcs {
            for chk in check_arcs {
                if chk.kind == CheckArcKind::Setup && chk.data_pin == ca.from_pin {
                    let has_launch = launch_arcs.iter().any(|la| {
                        la.clock_pin == chk.clock_pin
                            && la.to_pin == ca.to_pin
                            && la.clock_edge != chk.clock_edge
                    });
                    if has_launch {
                        out.push(LatchDef {
                            data_pin: ca.from_pin.clone(),
                            enable_pin: chk.clock_pin.clone(),
                            q_pin: ca.to_pin.clone(),
                            active_level: chk.clock_edge.flip(),
                        });
                    }
                }
            }
        }
    }
    out
}

impl CornerLib {
    /// Builds a corner library view from the Liberty libraries of `corner`, validating duplicate
    /// cells (STA-REQ-0015) and threshold / slew-derate consistency (STA-REQ-0016).
    pub fn build(corner: impl Into<String>, libs: &[Library]) -> Result<Self, String> {
        let corner = corner.into();
        let Some(first) = libs.first() else {
            return Err(format!("corner '{corner}' has no libraries"));
        };
        let thresholds = CornerThresholds::from_lib(first);
        let mut cells = BTreeMap::new();
        let mut wire_loads = BTreeMap::new();
        let mut wire_load_selections = BTreeMap::new();
        let mut default_wire_load = None;
        let mut default_wire_load_selection = None;
        let mut default_wire_load_mode = None;

        for lib in libs {
            let thr = CornerThresholds::from_lib(lib);
            if thr != thresholds {
                return Err(format!(
                    "C0501: library '{}' in corner '{corner}' has conflicting trip-point thresholds or slew_derate_from_library (STA-REQ-0016)",
                    lib.name
                ));
            }
            let tu = if lib.time_unit_ns > 0.0 {
                lib.time_unit_ns
            } else {
                1.0
            };
            let cu = if lib.cap_unit_pf > 0.0 {
                lib.cap_unit_pf
            } else {
                1.0
            };
            for c in &lib.cells {
                if cells.contains_key(&c.name) {
                    return Err(format!(
                        "C0501: duplicate cell '{}' in corner '{corner}' (STA-REQ-0015)",
                        c.name
                    ));
                }
                cells.insert(c.name.clone(), build_cell(lib, c, tu, cu)?);
            }
            for (name, wl) in &lib.wire_loads {
                let mut scaled = wl.clone();
                scaled.capacitance *= cu;
                wire_loads.entry(name.clone()).or_insert(scaled);
            }
            for (name, sel) in &lib.wire_load_selections {
                wire_load_selections
                    .entry(name.clone())
                    .or_insert_with(|| sel.clone());
            }
            if default_wire_load.is_none() {
                default_wire_load.clone_from(&lib.default_wire_load);
            }
            if default_wire_load_selection.is_none() {
                default_wire_load_selection.clone_from(&lib.default_wire_load_selection);
            }
            if default_wire_load_mode.is_none() {
                default_wire_load_mode = lib.default_wire_load_mode.clone();
            }
        }

        let tu = if first.time_unit_ns > 0.0 {
            first.time_unit_ns
        } else {
            1.0
        };
        let cu = if first.cap_unit_pf > 0.0 {
            first.cap_unit_pf
        } else {
            1.0
        };

        Ok(Self {
            corner,
            time_unit_ns: tu,
            cap_unit_pf: cu,
            thresholds,
            cells,
            wire_loads,
            wire_load_selections,
            default_wire_load,
            default_wire_load_selection,
            default_wire_load_mode,
        })
    }

    /// Selects the active `WireLoad` model for `total_area` under optional SDC overrides
    /// (STA-REQ-0125).
    #[must_use]
    pub fn select_wire_load(
        &self,
        sdc_model: Option<&str>,
        sdc_selection: Option<&str>,
        total_area: f64,
    ) -> Option<&WireLoad> {
        if let Some(m) = sdc_model {
            return self.wire_loads.get(m);
        }
        if let Some(sel_name) = sdc_selection
            && let Some(sel) = self.wire_load_selections.get(sel_name)
            && let Some(wl_name) = sel.select(total_area)
        {
            return self.wire_loads.get(wl_name);
        }
        if let Some(sel_name) = &self.default_wire_load_selection
            && let Some(sel) = self.wire_load_selections.get(sel_name)
            && let Some(wl_name) = sel.select(total_area)
        {
            return self.wire_loads.get(wl_name);
        }
        if let Some(wl_name) = &self.default_wire_load {
            return self.wire_loads.get(wl_name);
        }
        None
    }
}
