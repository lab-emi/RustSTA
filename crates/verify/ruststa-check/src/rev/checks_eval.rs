// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Independent timing checks, common-period clock relations, endpoint status taxonomy, backward
//! required-time propagation, and exhaustive path enumeration (`STA-REQ-0210` to `0228`, `0502`).

use std::collections::BTreeMap;

use rusteda_formats::liberty::Direction;

use super::Unsupported;
use super::canon_sdc::ClockDef;
use super::constants::{DisableReason, LogicVal};
use super::eval::{Tag, TimingSolver};
use super::lib_model::{Analysis, CheckArcKind, LatchDef, Trans};

const FS_PER_NS: f64 = 1_000_000.0;

/// Kind of endpoint check (`STA-REQ-0211`, `0212`, `0216`, `0221`, `0224`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckKindEval {
    SetupHold,
    RecoveryRemoval,
    LatchSetupHold,
    ClockGating,
    OutputDelay,
}

impl CheckKindEval {
    #[must_use]
    pub const fn setup_role_str(self) -> &'static str {
        match self {
            Self::SetupHold => "setup",
            Self::RecoveryRemoval => "recovery",
            Self::LatchSetupHold => "latch_setup",
            Self::ClockGating => "clock_gating_setup",
            Self::OutputDelay => "output_setup",
        }
    }

    #[must_use]
    pub const fn hold_role_str(self) -> &'static str {
        match self {
            Self::SetupHold => "hold",
            Self::RecoveryRemoval => "removal",
            Self::LatchSetupHold => "latch_hold",
            Self::ClockGating => "clock_gating_hold",
            Self::OutputDelay => "output_hold",
        }
    }
}

/// Endpoint completeness status taxonomy (`STA-REQ-0226`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EndpointStatus {
    Constrained,
    Unclocked,
    NoDataArrival,
    FalsePathOnly,
    UnconstrainedOutput,
    Disabled,
}

impl EndpointStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constrained => "constrained",
            Self::Unclocked => "unclocked",
            Self::NoDataArrival => "no_data_arrival",
            Self::FalsePathOnly => "false_path_only",
            Self::UnconstrainedOutput => "unconstrained_output",
            Self::Disabled => "disabled",
        }
    }
}

/// Evaluated endpoint record.
#[derive(Debug, Clone)]
pub struct EndpointEval {
    pub node_idx: usize,
    pub endpoint: String,
    pub kind: CheckKindEval,
    pub path_group: String,
    pub status: EndpointStatus,
    pub setup_slack_ns: Option<f64>,
    pub hold_slack_ns: Option<f64>,
    pub max_borrow_ns: f64,
    /// Per-`(tag, tr)` worst setup slack at this endpoint (for backward required times).
    pub tag_setup_slack: BTreeMap<(Tag, Trans), f64>,
    /// Per-`(tag, tr)` worst hold slack at this endpoint (for backward required times).
    pub tag_hold_slack: BTreeMap<(Tag, Trans), f64>,
}

/// Exhaustively enumerated path record (`STA-REQ-0502`, `0215`, `0228`).
#[derive(Debug, Clone)]
pub struct PathEval {
    pub startpoint: String,
    pub endpoint: String,
    pub start_tr: Trans,
    pub end_tr: Trans,
    pub launch_clock: Option<String>,
    pub launch_edge: Option<Trans>,
    pub capture_clock: Option<String>,
    pub capture_edge: Option<Trans>,
    pub kind: CheckKindEval,
    pub path_group: String,
    /// Ordered sequence of `(pin_or_port_name, transition)` along the data path.
    pub pins: Vec<(String, Trans)>,
    pub arrival_late_ns: Option<f64>,
    pub arrival_early_ns: Option<f64>,
    pub setup_required_ns: Option<f64>,
    pub hold_required_ns: Option<f64>,
    pub setup_slack_ns: Option<f64>,
    pub hold_slack_ns: Option<f64>,
}

fn ns_to_fs(ns: f64) -> i64 {
    (ns * FS_PER_NS).round() as i64
}

fn fs_to_ns(fs: i64) -> f64 {
    (fs as f64) / FS_PER_NS
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a.max(1)
}

fn lcm_i64(a: i64, b: i64) -> i64 {
    if a == 0 || b == 0 {
        return 0;
    }
    (a / gcd_i64(a, b)).saturating_mul(b)
}

/// Computes the setup (`D_s > 0`) and hold (`D_h <= 0`) edge relations in `ns` over the integer-fs
/// common period `P = lcm(T_L, T_C)` (`STA-REQ-0220`).
pub(crate) fn clock_relations_ns(
    launch_clk: &ClockDef,
    launch_edge: Trans,
    cap_clk: &ClockDef,
    cap_edge: Trans,
) -> Result<(f64, f64), Unsupported> {
    let t_l = ns_to_fs(launch_clk.period_ns);
    let t_c = ns_to_fs(cap_clk.period_ns);
    if t_l <= 0 || t_c <= 0 {
        return Err(Unsupported {
            feature: "non-positive clock period",
            planned: "R1",
        });
    }
    let p = lcm_i64(t_l, t_c);
    if p > 1000 * t_l.max(t_c) {
        return Err(Unsupported {
            feature: "non-harmonically-related clocks exceeding 1000x max period",
            planned: "R1",
        });
    }
    let l0 = ns_to_fs(launch_clk.edge_time_ns(launch_edge));
    let c0 = ns_to_fs(cap_clk.edge_time_ns(cap_edge));
    let n_l = (p / t_l).max(1);
    let mut min_ds = i64::MAX;
    let mut max_dh = i64::MIN;

    for k in 0..n_l {
        let l = l0 + k * t_l;
        // Smallest capture edge `c = c0 + j * t_c > l`.
        let diff = l - c0;
        let j_after = if diff >= 0 {
            diff / t_c + 1
        } else {
            -((-diff - 1) / t_c)
        };
        let c_after = c0 + j_after * t_c;
        let ds = c_after - l;
        debug_assert!(ds > 0);
        min_ds = min_ds.min(ds);

        // Largest capture edge `c = c0 + j * t_c <= l`.
        let c_before = c_after - t_c;
        let dh = c_before - l;
        debug_assert!(dh <= 0);
        max_dh = max_dh.max(dh);
    }

    Ok((fs_to_ns(min_ds), fs_to_ns(max_dh)))
}

/// Computes the transparent pulse width `W` in `ns` between `open_edge` and `close_edge` of `clk`
/// (`STA-REQ-0221`).
#[must_use]
pub(crate) fn pulse_width_ns(clk: &ClockDef, open_edge: Trans, close_edge: Trans) -> f64 {
    let t_open = clk.edge_time_ns(open_edge);
    let t_close = clk.edge_time_ns(close_edge);
    if t_close > t_open {
        t_close - t_open
    } else {
        clk.period_ns - (t_open - t_close)
    }
}

/// Computes the trailing-edge clock-gating hold relation `h(trailing) = min { c_trail >= l } - l`
/// over the common period (`STA-REQ-0225`).
pub(crate) fn gating_trailing_relation_ns(
    launch_clk: &ClockDef,
    launch_edge: Trans,
    cap_clk: &ClockDef,
    trail_edge: Trans,
) -> Result<f64, Unsupported> {
    let t_l = ns_to_fs(launch_clk.period_ns);
    let t_c = ns_to_fs(cap_clk.period_ns);
    let p = lcm_i64(t_l, t_c);
    if p <= 0 || p > 1000 * t_l.max(t_c) {
        return Err(Unsupported {
            feature: "clock-gating non-harmonic period ratio",
            planned: "R1",
        });
    }
    let l0 = ns_to_fs(launch_clk.edge_time_ns(launch_edge));
    let c0 = ns_to_fs(cap_clk.edge_time_ns(trail_edge));
    let n_l = (p / t_l).max(1);
    let mut min_h = i64::MAX;

    for k in 0..n_l {
        let l = l0 + k * t_l;
        let diff = l - c0;
        let j = if diff <= 0 {
            -(diff.abs() / t_c)
        } else {
            (diff + t_c - 1) / t_c
        };
        let mut c = c0 + j * t_c;
        while c < l {
            c += t_c;
        }
        while c - t_c >= l {
            c -= t_c;
        }
        min_h = min_h.min(c - l);
    }

    Ok(fs_to_ns(min_h))
}

/// Information about a combinational clock-gating check at an enable pin (`STA-REQ-0216`, `0225`).
#[derive(Debug, Clone)]
pub(crate) struct ClockGateCheckInfo {
    pub(crate) en_node: usize,
    pub(crate) ck_node: usize,
    /// `false` (`0`) for active-high AND-like gating, `true` (`1`) for active-low OR-like gating.
    pub(crate) active_low: bool,
}

impl TimingSolver<'_> {
    /// Evaluates all potential endpoints in canonical order (`STA-REQ-0211`, `0212`, `0216`,
    /// `0221`, `0224`, `0225`, `0226`).
    pub(crate) fn evaluate_endpoints(&mut self) -> Result<(), Unsupported> {
        let mut endpoints = Vec::new();
        let clk_reaching = self.nodes_reaching_clock_pins();

        // 1. Sequential check pins and combinational clock-gating enable pins across instances.
        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else {
                continue;
            };

            // Group check arcs by `(data_pin, is_async)` so `D` (SetupHold / LatchSetupHold) and
            // `RN` (RecoveryRemoval) on the same cell are separate endpoints.
            let mut data_pins: Vec<(String, bool)> = Vec::new();
            for chk in &cdef.check_arcs {
                let key = (chk.data_pin.clone(), chk.kind.is_async());
                if !data_pins.contains(&key) {
                    data_pins.push(key);
                }
            }

            for (data_pin, is_async) in data_pins {
                let Some(&d_node) = inst.pin_nodes.get(&data_pin) else {
                    continue;
                };
                if let Some(latch) = cdef.latch_for_d(&data_pin)
                    && !is_async
                {
                    let ep = self.eval_latch_endpoint(inst_idx, latch, d_node)?;
                    endpoints.push(ep);
                } else {
                    let ep = self.eval_seq_check_endpoint(inst_idx, &data_pin, is_async, d_node)?;
                    endpoints.push(ep);
                }
            }

            // Check for combinational clock-gating enable pins (`STA-REQ-0216`, `0225`).
            for cg in self.detect_clock_gates_on_inst(inst_idx, &clk_reaching) {
                let ep = self.eval_clock_gate_endpoint(&cg)?;
                endpoints.push(ep);
            }
        }

        // 2. Primary output ports (`STA-REQ-0212`, `0218`, `0219`, `0226`).
        for &port_node in &self.design.output_ports {
            let ep = self.eval_output_port_endpoint(port_node)?;
            endpoints.push(ep);
        }

        endpoints.sort_by(|a, b| {
            a.endpoint
                .cmp(&b.endpoint)
                .then_with(|| a.kind.cmp(&b.kind))
        });
        self.endpoints = endpoints;
        Ok(())
    }

    fn eval_seq_check_endpoint(
        &self,
        inst_idx: usize,
        data_pin: &str,
        is_async: bool,
        d_node: usize,
    ) -> Result<EndpointEval, Unsupported> {
        let inst = &self.design.instances[inst_idx];
        let cdef = &self.lib.cells[&inst.cell_type];
        let ep_name = self.design.nodes[d_node].name.clone();
        let kind = if is_async {
            CheckKindEval::RecoveryRemoval
        } else {
            CheckKindEval::SetupHold
        };

        let matching_arcs: Vec<(usize, &super::lib_model::CheckArc)> = cdef
            .check_arcs
            .iter()
            .enumerate()
            .filter(|(_, chk)| chk.data_pin == data_pin && chk.kind.is_async() == is_async)
            .collect();

        // Check whether any related clock pin is in a clock network with the active edge.
        let mut has_active_clock = false;
        let mut default_group = if is_async {
            "async_default".to_string()
        } else {
            "unclocked".to_string()
        };
        let mut all_user_or_when_disabled = true;

        for &(chk_idx, chk) in &matching_arcs {
            let dis = self.consts.disabled_check_arcs.get(&(inst_idx, chk_idx));
            if !matches!(
                dis,
                Some(
                    DisableReason::UserDisabled
                        | DisableReason::WhenFalse
                        | DisableReason::CaseAnalysis
                )
            ) {
                all_user_or_when_disabled = false;
            }
            if let Some(&ck_node) = inst.pin_nodes.get(&chk.clock_pin)
                && self.consts.pin_vals[ck_node] == LogicVal::X
                && let Some(waves) = self.clk_waves.get(&ck_node)
            {
                for w in waves {
                    if w.pin_edge == chk.clock_edge {
                        has_active_clock = true;
                        if !is_async {
                            default_group.clone_from(&w.clock);
                        }
                    }
                }
            }
        }

        if !has_active_clock {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind,
                path_group: default_group,
                status: EndpointStatus::Unclocked,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        if all_user_or_when_disabled {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind,
                path_group: default_group,
                status: EndpointStatus::Disabled,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let has_any_arrival = self.consts.pin_vals[d_node] == LogicVal::X
            && Trans::ALL.iter().any(|&tr| {
                Analysis::ALL
                    .iter()
                    .any(|&an| !self.arrivals[d_node][tr.idx()][an.idx()].is_empty())
            });
        if !has_any_arrival {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind,
                path_group: default_group,
                status: EndpointStatus::NoDataArrival,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let mut min_setup: Option<f64> = None;
        let mut min_hold: Option<f64> = None;
        let mut tag_setup_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();
        let mut tag_hold_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();

        for &(chk_idx, chk) in &matching_arcs {
            if self
                .consts
                .disabled_check_arcs
                .contains_key(&(inst_idx, chk_idx))
            {
                continue;
            }
            let Some(&ck_node) = inst.pin_nodes.get(&chk.clock_pin) else {
                continue;
            };
            let Some(waves) = self.clk_waves.get(&ck_node) else {
                continue;
            };
            let rel_out_cap = cdef
                .pins
                .iter()
                .find(|(_, p)| p.direction == Direction::Output)
                .and_then(|(pname, _)| inst.pin_nodes.get(pname))
                .map_or(0.0, |&qn| {
                    self.node_load_pf(qn, Trans::Rise, Analysis::Late)
                });

            for w in waves {
                if w.pin_edge != chk.clock_edge {
                    continue;
                }
                let Some(cap_clk) = self.sdc.clocks.get(&w.clock) else {
                    continue;
                };
                for data_tr in Trans::ALL {
                    if chk.kind.is_setup_like() {
                        let Some(s_d) = self.slews[d_node][data_tr.idx()][Analysis::Late.idx()]
                        else {
                            continue;
                        };
                        let s_ck = cap_clk.transition(w.pin_edge, Analysis::Late);
                        let Some(s_val) = chk.constraint_opt(data_tr, s_d, s_ck, rel_out_cap)
                        else {
                            continue;
                        };
                        let lat_c = cap_clk.total_latency(w.source_edge, Analysis::Early);
                        for entry in &self.arrivals[d_node][data_tr.idx()][Analysis::Late.idx()] {
                            let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                                continue;
                            };
                            let (d_s, _) = clock_relations_ns(
                                launch_clk,
                                entry.tag.edge,
                                cap_clk,
                                w.source_edge,
                            )?;
                            let u_s = self.sdc.clock_uncertainty(
                                &entry.tag.clock,
                                &w.clock,
                                Analysis::Late,
                            );
                            let req = d_s + lat_c - s_val - u_s;
                            let slack = req - entry.arrival_ns;
                            min_setup = Some(min_setup.map_or(slack, |m| m.min(slack)));
                            tag_setup_slack
                                .entry((entry.tag.clone(), data_tr))
                                .and_modify(|v| *v = v.min(slack))
                                .or_insert(slack);
                        }
                    } else {
                        let Some(s_d) = self.slews[d_node][data_tr.idx()][Analysis::Early.idx()]
                        else {
                            continue;
                        };
                        let s_ck = cap_clk.transition(w.pin_edge, Analysis::Early);
                        let Some(h_val) = chk.constraint_opt(data_tr, s_d, s_ck, rel_out_cap)
                        else {
                            continue;
                        };
                        let lat_c = cap_clk.total_latency(w.source_edge, Analysis::Late);
                        for entry in &self.arrivals[d_node][data_tr.idx()][Analysis::Early.idx()] {
                            let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                                continue;
                            };
                            let (_, d_h) = clock_relations_ns(
                                launch_clk,
                                entry.tag.edge,
                                cap_clk,
                                w.source_edge,
                            )?;
                            let u_h = self.sdc.clock_uncertainty(
                                &entry.tag.clock,
                                &w.clock,
                                Analysis::Early,
                            );
                            let req = d_h + lat_c + h_val + u_h;
                            let slack = entry.arrival_ns - req;
                            min_hold = Some(min_hold.map_or(slack, |m| m.min(slack)));
                            tag_hold_slack
                                .entry((entry.tag.clone(), data_tr))
                                .and_modify(|v| *v = v.min(slack))
                                .or_insert(slack);
                        }
                    }
                }
            }
        }

        let status = if min_setup.is_some() || min_hold.is_some() {
            EndpointStatus::Constrained
        } else {
            EndpointStatus::NoDataArrival
        };

        Ok(EndpointEval {
            node_idx: d_node,
            endpoint: ep_name,
            kind,
            path_group: default_group,
            status,
            setup_slack_ns: min_setup,
            hold_slack_ns: min_hold,
            max_borrow_ns: 0.0,
            tag_setup_slack,
            tag_hold_slack,
        })
    }

    fn eval_latch_endpoint(
        &self,
        inst_idx: usize,
        latch: &LatchDef,
        d_node: usize,
    ) -> Result<EndpointEval, Unsupported> {
        let inst = &self.design.instances[inst_idx];
        let cdef = &self.lib.cells[&inst.cell_type];
        let ep_name = self.design.nodes[d_node].name.clone();
        let close_edge = latch.close_edge();
        let open_edge = latch.open_edge();

        let Some(&g_node) = inst.pin_nodes.get(&latch.enable_pin) else {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind: CheckKindEval::LatchSetupHold,
                path_group: "unclocked".to_string(),
                status: EndpointStatus::Unclocked,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        };

        let g_waves = self
            .clk_waves
            .get(&g_node)
            .filter(|_| self.consts.pin_vals[g_node] == LogicVal::X)
            .cloned()
            .unwrap_or_default();

        let has_close = g_waves.iter().any(|w| w.pin_edge == close_edge);
        if !has_close {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind: CheckKindEval::LatchSetupHold,
                path_group: "unclocked".to_string(),
                status: EndpointStatus::Unclocked,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let path_group = g_waves
            .iter()
            .find(|w| w.pin_edge == close_edge)
            .map_or_else(|| "unclocked".to_string(), |w| w.clock.clone());

        let has_any_arrival = self.consts.pin_vals[d_node] == LogicVal::X
            && Trans::ALL.iter().any(|&tr| {
                Analysis::ALL
                    .iter()
                    .any(|&an| !self.arrivals[d_node][tr.idx()][an.idx()].is_empty())
            });
        if !has_any_arrival {
            return Ok(EndpointEval {
                node_idx: d_node,
                endpoint: ep_name,
                kind: CheckKindEval::LatchSetupHold,
                path_group,
                status: EndpointStatus::NoDataArrival,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let setup_chk = cdef.check_arcs.iter().find(|chk| {
            chk.data_pin == latch.data_pin
                && chk.clock_pin == latch.enable_pin
                && chk.kind == CheckArcKind::Setup
                && chk.clock_edge == close_edge
        });
        let hold_chk = cdef.check_arcs.iter().find(|chk| {
            chk.data_pin == latch.data_pin
                && chk.clock_pin == latch.enable_pin
                && chk.kind == CheckArcKind::Hold
                && chk.clock_edge == close_edge
        });

        let mut min_setup: Option<f64> = None;
        let mut min_hold: Option<f64> = None;
        let mut max_borrow: f64 = 0.0;
        let mut tag_setup_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();
        let mut tag_hold_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();

        for w_close in &g_waves {
            if w_close.pin_edge != close_edge {
                continue;
            }
            let Some(w_open) = g_waves.iter().find(|w| {
                w.clock == w_close.clock
                    && w.source_port == w_close.source_port
                    && w.pin_edge == open_edge
            }) else {
                continue;
            };
            let Some(cap_clk) = self.sdc.clocks.get(&w_close.clock) else {
                continue;
            };
            let pulse_w = pulse_width_ns(cap_clk, w_open.source_edge, w_close.source_edge);

            for data_tr in Trans::ALL {
                // Latch setup check with transparent window `[R_open, R_close]` (STA-REQ-0221).
                if let Some(s_d) = self.slews[d_node][data_tr.idx()][Analysis::Late.idx()] {
                    let s_g = cap_clk.transition(close_edge, Analysis::Late);
                    let s_val = setup_chk
                        .and_then(|chk| chk.constraint_opt(data_tr, s_d, s_g, 0.0))
                        .unwrap_or(0.0);
                    let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Early);
                    for entry in &self.arrivals[d_node][data_tr.idx()][Analysis::Late.idx()] {
                        let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                            continue;
                        };
                        let (d_close, _) = clock_relations_ns(
                            launch_clk,
                            entry.tag.edge,
                            cap_clk,
                            w_close.source_edge,
                        )?;
                        let d_open = d_close - pulse_w;
                        let u_s = self.sdc.clock_uncertainty(
                            &entry.tag.clock,
                            &w_close.clock,
                            Analysis::Late,
                        );
                        let r_open = d_open + lat_c - u_s;
                        let r_close_unclamped = d_close + lat_c - s_val - u_s;
                        let r_close =
                            match self
                                .sdc
                                .max_time_borrow_for(&inst.name, &ep_name, &w_close.clock)
                            {
                                Some(b) => r_close_unclamped.min(r_open + b),
                                None => r_close_unclamped,
                            };
                        let a = entry.arrival_ns;
                        let (slack, borrow) = if a <= r_open {
                            (r_open - a, 0.0)
                        } else if a <= r_close {
                            (0.0, a - r_open)
                        } else {
                            (r_close - a, (r_close - r_open).max(0.0))
                        };
                        max_borrow = max_borrow.max(borrow);
                        min_setup = Some(min_setup.map_or(slack, |m| m.min(slack)));
                        tag_setup_slack
                            .entry((entry.tag.clone(), data_tr))
                            .and_modify(|v| *v = v.min(slack))
                            .or_insert(slack);
                    }
                }

                // Latch hold check against closing edge `D_h((L, e_L) -> (C, e_close))` (STA-REQ-0221).
                if let Some(s_d) = self.slews[d_node][data_tr.idx()][Analysis::Early.idx()] {
                    let s_g = cap_clk.transition(close_edge, Analysis::Early);
                    let h_val = hold_chk
                        .and_then(|chk| chk.constraint_opt(data_tr, s_d, s_g, 0.0))
                        .unwrap_or(0.0);
                    let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Late);
                    for entry in &self.arrivals[d_node][data_tr.idx()][Analysis::Early.idx()] {
                        let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                            continue;
                        };
                        let (_, d_h) = clock_relations_ns(
                            launch_clk,
                            entry.tag.edge,
                            cap_clk,
                            w_close.source_edge,
                        )?;
                        let u_h = self.sdc.clock_uncertainty(
                            &entry.tag.clock,
                            &w_close.clock,
                            Analysis::Early,
                        );
                        let req = d_h + lat_c + h_val + u_h;
                        let slack = entry.arrival_ns - req;
                        min_hold = Some(min_hold.map_or(slack, |m| m.min(slack)));
                        tag_hold_slack
                            .entry((entry.tag.clone(), data_tr))
                            .and_modify(|v| *v = v.min(slack))
                            .or_insert(slack);
                    }
                }
            }
        }

        let status = if min_setup.is_some() || min_hold.is_some() {
            EndpointStatus::Constrained
        } else {
            EndpointStatus::NoDataArrival
        };

        Ok(EndpointEval {
            node_idx: d_node,
            endpoint: ep_name,
            kind: CheckKindEval::LatchSetupHold,
            path_group,
            status,
            setup_slack_ns: min_setup,
            hold_slack_ns: min_hold,
            max_borrow_ns: max_borrow,
            tag_setup_slack,
            tag_hold_slack,
        })
    }

    fn eval_clock_gate_endpoint(
        &self,
        cg: &ClockGateCheckInfo,
    ) -> Result<EndpointEval, Unsupported> {
        let ep_name = self.design.nodes[cg.en_node].name.clone();
        let waves = self.clk_waves.get(&cg.ck_node).cloned().unwrap_or_default();
        let path_group = waves
            .first()
            .map_or_else(|| "unclocked".to_string(), |w| w.clock.clone());

        let has_any_arrival = self.consts.pin_vals[cg.en_node] == LogicVal::X
            && Trans::ALL.iter().any(|&tr| {
                Analysis::ALL
                    .iter()
                    .any(|&an| !self.arrivals[cg.en_node][tr.idx()][an.idx()].is_empty())
            });
        if !has_any_arrival {
            return Ok(EndpointEval {
                node_idx: cg.en_node,
                endpoint: ep_name,
                kind: CheckKindEval::ClockGating,
                path_group,
                status: EndpointStatus::NoDataArrival,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        // Active-high (AND, `v = 0`): leading pin edge is `Rise`, trailing pin edge is `Fall`.
        // Active-low (OR, `v = 1`): leading pin edge is `Fall`, trailing pin edge is `Rise`
        // (STA-REQ-0216, 0225).
        let (lead_pin_edge, trail_pin_edge) = if cg.active_low {
            (Trans::Fall, Trans::Rise)
        } else {
            (Trans::Rise, Trans::Fall)
        };

        let mut min_setup: Option<f64> = None;
        let mut min_hold: Option<f64> = None;
        let mut tag_setup_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();
        let mut tag_hold_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();

        for w_lead in &waves {
            if w_lead.pin_edge != lead_pin_edge {
                continue;
            }
            let Some(w_trail) = waves.iter().find(|w| {
                w.clock == w_lead.clock
                    && w.source_port == w_lead.source_port
                    && w.pin_edge == trail_pin_edge
            }) else {
                continue;
            };
            let Some(cap_clk) = self.sdc.clocks.get(&w_lead.clock) else {
                continue;
            };

            for data_tr in Trans::ALL {
                let lat_c_early = cap_clk.total_latency(w_lead.source_edge, Analysis::Early);
                for entry in &self.arrivals[cg.en_node][data_tr.idx()][Analysis::Late.idx()] {
                    let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                        continue;
                    };
                    let (d_s, _) = clock_relations_ns(
                        launch_clk,
                        entry.tag.edge,
                        cap_clk,
                        w_lead.source_edge,
                    )?;
                    let u_s =
                        self.sdc
                            .clock_uncertainty(&entry.tag.clock, &w_lead.clock, Analysis::Late);
                    let req = d_s + lat_c_early - u_s;
                    let slack = req - entry.arrival_ns;
                    min_setup = Some(min_setup.map_or(slack, |m| m.min(slack)));
                    tag_setup_slack
                        .entry((entry.tag.clone(), data_tr))
                        .and_modify(|v| *v = v.min(slack))
                        .or_insert(slack);
                }

                let lat_c_late = cap_clk.total_latency(w_trail.source_edge, Analysis::Late);
                for entry in &self.arrivals[cg.en_node][data_tr.idx()][Analysis::Early.idx()] {
                    let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                        continue;
                    };
                    let h_trail = gating_trailing_relation_ns(
                        launch_clk,
                        entry.tag.edge,
                        cap_clk,
                        w_trail.source_edge,
                    )?;
                    let u_h = self.sdc.clock_uncertainty(
                        &entry.tag.clock,
                        &w_trail.clock,
                        Analysis::Early,
                    );
                    let req = h_trail + lat_c_late + u_h;
                    let slack = entry.arrival_ns - req;
                    min_hold = Some(min_hold.map_or(slack, |m| m.min(slack)));
                    tag_hold_slack
                        .entry((entry.tag.clone(), data_tr))
                        .and_modify(|v| *v = v.min(slack))
                        .or_insert(slack);
                }
            }
        }

        let status = if min_setup.is_some() || min_hold.is_some() {
            EndpointStatus::Constrained
        } else {
            EndpointStatus::NoDataArrival
        };

        Ok(EndpointEval {
            node_idx: cg.en_node,
            endpoint: ep_name,
            kind: CheckKindEval::ClockGating,
            path_group,
            status,
            setup_slack_ns: min_setup,
            hold_slack_ns: min_hold,
            max_borrow_ns: 0.0,
            tag_setup_slack,
            tag_hold_slack,
        })
    }

    fn eval_output_port_endpoint(&self, port_node: usize) -> Result<EndpointEval, Unsupported> {
        let port_name = self.design.nodes[port_node].name.clone();
        let odelays = self.sdc.output_delays_for_port(&port_name);
        if odelays.is_empty() {
            return Ok(EndpointEval {
                node_idx: port_node,
                endpoint: port_name,
                kind: CheckKindEval::OutputDelay,
                path_group: "unconstrained".to_string(),
                status: EndpointStatus::UnconstrainedOutput,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let valid_clks: Vec<_> = odelays
            .iter()
            .filter(|od| self.sdc.clocks.contains_key(&od.clock))
            .collect();
        if valid_clks.is_empty() {
            return Ok(EndpointEval {
                node_idx: port_node,
                endpoint: port_name,
                kind: CheckKindEval::OutputDelay,
                path_group: "unclocked".to_string(),
                status: EndpointStatus::Unclocked,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let path_group = valid_clks[0].clock.clone();
        let has_any_arrival = self.consts.pin_vals[port_node] == LogicVal::X
            && Trans::ALL.iter().any(|&tr| {
                Analysis::ALL
                    .iter()
                    .any(|&an| !self.arrivals[port_node][tr.idx()][an.idx()].is_empty())
            });
        if !has_any_arrival {
            return Ok(EndpointEval {
                node_idx: port_node,
                endpoint: port_name,
                kind: CheckKindEval::OutputDelay,
                path_group,
                status: EndpointStatus::NoDataArrival,
                setup_slack_ns: None,
                hold_slack_ns: None,
                max_borrow_ns: 0.0,
                tag_setup_slack: BTreeMap::new(),
                tag_hold_slack: BTreeMap::new(),
            });
        }

        let mut min_setup: Option<f64> = None;
        let mut min_hold: Option<f64> = None;
        let mut tag_setup_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();
        let mut tag_hold_slack: BTreeMap<(Tag, Trans), f64> = BTreeMap::new();

        for od in valid_clks {
            let cap_clk = &self.sdc.clocks[&od.clock];
            for data_tr in Trans::ALL {
                // Output setup check (`-max`, STA-REQ-0212, 0218, 0219).
                if let Some(v_max) = od.bound(data_tr, Analysis::Late) {
                    let lat_src = if od.source_latency_included {
                        0.0
                    } else {
                        cap_clk.source_latency(od.clock_edge, Analysis::Early)
                    };
                    let lat_net = if od.network_latency_included {
                        0.0
                    } else {
                        cap_clk.network_latency(od.clock_edge, Analysis::Early)
                    };
                    let lat_c = lat_src + lat_net;
                    for entry in &self.arrivals[port_node][data_tr.idx()][Analysis::Late.idx()] {
                        let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                            continue;
                        };
                        let (d_s, _) =
                            clock_relations_ns(launch_clk, entry.tag.edge, cap_clk, od.clock_edge)?;
                        let u_s =
                            self.sdc
                                .clock_uncertainty(&entry.tag.clock, &od.clock, Analysis::Late);
                        let req = d_s + lat_c - v_max - u_s;
                        let slack = req - entry.arrival_ns;
                        min_setup = Some(min_setup.map_or(slack, |m| m.min(slack)));
                        tag_setup_slack
                            .entry((entry.tag.clone(), data_tr))
                            .and_modify(|v| *v = v.min(slack))
                            .or_insert(slack);
                    }
                }

                // Output hold check (`-min`, STA-REQ-0212, 0218, 0219).
                if let Some(v_min) = od.bound(data_tr, Analysis::Early) {
                    let lat_src = if od.source_latency_included {
                        0.0
                    } else {
                        cap_clk.source_latency(od.clock_edge, Analysis::Late)
                    };
                    let lat_net = if od.network_latency_included {
                        0.0
                    } else {
                        cap_clk.network_latency(od.clock_edge, Analysis::Late)
                    };
                    let lat_c = lat_src + lat_net;
                    for entry in &self.arrivals[port_node][data_tr.idx()][Analysis::Early.idx()] {
                        let Some(launch_clk) = self.sdc.clocks.get(&entry.tag.clock) else {
                            continue;
                        };
                        let (_, d_h) =
                            clock_relations_ns(launch_clk, entry.tag.edge, cap_clk, od.clock_edge)?;
                        let u_h = self.sdc.clock_uncertainty(
                            &entry.tag.clock,
                            &od.clock,
                            Analysis::Early,
                        );
                        let req = d_h + lat_c - v_min + u_h;
                        let slack = entry.arrival_ns - req;
                        min_hold = Some(min_hold.map_or(slack, |m| m.min(slack)));
                        tag_hold_slack
                            .entry((entry.tag.clone(), data_tr))
                            .and_modify(|v| *v = v.min(slack))
                            .or_insert(slack);
                    }
                }
            }
        }

        let status = if min_setup.is_some() || min_hold.is_some() {
            EndpointStatus::Constrained
        } else {
            EndpointStatus::NoDataArrival
        };

        Ok(EndpointEval {
            node_idx: port_node,
            endpoint: port_name,
            kind: CheckKindEval::OutputDelay,
            path_group,
            status,
            setup_slack_ns: min_setup,
            hold_slack_ns: min_hold,
            max_borrow_ns: 0.0,
            tag_setup_slack,
            tag_hold_slack,
        })
    }
}
