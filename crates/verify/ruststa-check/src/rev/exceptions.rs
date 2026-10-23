// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions and clock groups by explicit path matching (`STA-REQ-0250` to `0270`,
//! owner: `R2`).

use std::collections::{BTreeMap, BTreeSet};

use rusteda_core::liberty::Direction;

use super::canon_sdc::{
    ClockDef, ExceptionEndpointFilter, ExceptionKind, ExceptionSpec, ThroughHopFilter,
};
use super::checks_eval::{
    CheckKindEval, EndpointEval, EndpointStatus, PathEval, clock_relations_ns,
    gating_trailing_relation_ns, pulse_width_ns,
};
use super::constants::LogicVal;
use super::eval::{Tag, TimingSolver};
use super::lib_model::{Analysis, CheckArcKind, Trans};
use super::verilog::NodeKind;
use super::{DiagRecord, Unsupported};

/// Marker type for exception path matching in `ruststa-check::rev` (`R2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionPaths;

impl ExceptionPaths {
    /// Constructs the exception path matcher (`R2`).
    pub fn new() -> Result<Self, Unsupported> {
        Ok(Self)
    }
}

#[derive(Debug, Clone)]
struct ValidatedExc {
    idx: usize,
    spec: ExceptionSpec,
    vacuous: bool,
}

#[derive(Debug, Clone)]
struct RawPath {
    launch: Option<(String, Trans)>,
    hops: Vec<(usize, Trans)>,
    hop_arrs: Vec<(Option<f64>, Option<f64>)>,
    arr_late: Option<(f64, f64)>,
    arr_early: Option<(f64, f64)>,
}

impl RawPath {
    fn launch_clock(&self) -> Option<&str> {
        self.launch.as_ref().map(|x| x.0.as_str())
    }
    fn launch_edge(&self) -> Option<Trans> {
        self.launch.as_ref().map(|x| x.1)
    }
}

#[rustfmt::skip]
type BorrowedLaunch = (usize, usize, usize, Trans, Trans, Trans, String, Trans, f64, f64);
type BestBound = Option<(f64, f64, String, Option<Trans>)>;
type Cand<'a> = (u8, u8, bool, usize, &'a ValidatedExc);

#[derive(Debug, Clone, Copy)]
enum Winner {
    None,
    FalsePath(usize),
    Delay(usize, f64, bool),
    Multicycle(usize, i32, bool),
}

#[derive(Default)]
struct EvalAcc {
    best_setup: BestBound,
    best_hold: BestBound,
    max_borrow_ns: f64,
    was_disabled: bool,
    used_specs: BTreeSet<usize>,
    tie_conflicts: BTreeSet<(usize, usize)>,
}

impl EvalAcc {
    fn reset_for_path(&mut self) {
        self.best_setup = None;
        self.best_hold = None;
        self.max_borrow_ns = 0.0;
        self.was_disabled = false;
    }
}

#[rustfmt::skip]
impl TimingSolver<'_> {
    /// Applies timing exceptions and clock groups (`STA-REQ-0250` to `0270`) to all endpoints,
    /// paths, backward required times, diagnostics, and scope counters.
    pub(crate) fn apply_exceptions(&mut self) -> Result<(), Unsupported> {
        if self.sccs_broken > 0 && self.sdc.exceptions.iter().any(|e| !e.through_hops.is_empty()) {
            super::through::ThroughPaths::new()?;
        }
        for exc in &self.sdc.exceptions {
            *self.exception_counts.entry(exc.kind.kind_name().to_string()).or_insert(0) += 1;
        }
        if !self.sdc.clock_groups.is_empty() {
            self.exception_counts.insert("clock_groups".to_string(), self.sdc.clock_groups.len());
        }
        for cg in &self.sdc.clock_groups {
            if cg.allow_paths {
                let msg = "set_clock_groups -allow_paths separates no clocks";
                self.push_diag("D0114", "note", msg, Some(cg.line));
            }
        }

        let validated = self.validate_exceptions();
        let mut raw_paths = Vec::new();
        self.enumerate_stage1_raw_paths(&mut raw_paths);

        let mut acc = EvalAcc::default();
        let borrowed = self.evaluate_latch_borrowing(&raw_paths, &validated, &mut acc)?;
        self.enumerate_borrowed_raw_paths(&borrowed, &mut raw_paths);
        // The path budget of STA-REQ-0506 bounds memory; a truncated enumeration is undecided.
        if raw_paths.len() >= Self::MAX_ENUMERATED_PATHS { return Err(Unsupported { feature: "timing exceptions on designs with more than 200000 enumerated paths", planned: "a bounded per-tag exception pass" }); }
        self.evaluate_all_paths_and_endpoints(&raw_paths, &validated, &mut acc)?;

        for (c1, c2) in acc.tie_conflicts {
            let (l1, l2) = (self.sdc.exceptions[c1].line, self.sdc.exceptions[c2].line);
            let msg = format!("conflicting exceptions at lines {l1} and {l2} tie; line {l2} wins");
            self.push_diag("D0111", "warning", msg, Some(l2));
        }

        let mut unused_count = 0usize;
        for v in &validated {
            if !v.vacuous && !acc.used_specs.contains(&v.idx) {
                unused_count += 1;
                let msg = format!("{} at line {} is unused", v.spec.kind.kind_name(), v.spec.line);
                self.push_diag("D0112", "note", msg, Some(v.spec.line));
            }
        }
        self.unused_exceptions = unused_count;
        Ok(())
    }

    fn push_diag(&mut self, code: &str, sev: &str, msg: impl Into<String>, line: Option<usize>) {
        self.diagnostics.push(DiagRecord {
            code: code.to_string(), severity: sev.to_string(), message: msg.into(), line,
        });
    }

    fn validate_exceptions(&mut self) -> Vec<ValidatedExc> {
        let mut start_nodes: BTreeSet<usize> = self.design.input_ports.iter().copied().collect();
        let mut start_insts = BTreeSet::new();
        for (ii, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else { continue };
            for la in &cdef.launch_arcs {
                if let Some(&ck_n) = inst.pin_nodes.get(&la.clock_pin) {
                    start_nodes.insert(ck_n);
                    start_insts.insert(ii);
                }
            }
            for latch in &cdef.latches {
                if let Some(&g_n) = inst.pin_nodes.get(&latch.enable_pin) {
                    start_nodes.insert(g_n);
                    start_insts.insert(ii);
                }
            }
        }

        let mut end_nodes: BTreeSet<usize> = self.design.output_ports.iter().copied().collect();
        let mut end_insts = BTreeSet::new();
        for ep in &self.endpoints {
            end_nodes.insert(ep.node_idx);
            if let NodeKind::InstancePin { inst_idx, .. } = self.design.nodes[ep.node_idx].kind {
                end_insts.insert(inst_idx);
            }
        }

        let specs: Vec<ExceptionSpec> = self.sdc.exceptions.clone();
        specs.into_iter().enumerate().map(|(idx, mut exc)| {
            if exc.both_setup_and_hold_flags {
                let m = "set_multicycle_path with both -setup and -hold keeps hold multiplier 0";
                self.push_diag("D0113", "warning", m, Some(exc.line));
            }
            for q in &exc.empty_queries {
                self.push_diag("D0102", "warning", format!("query '{q}' matched nothing"), Some(exc.line));
            }
            if let Some(ref mut f) = exc.from {
                self.prune_endpoint_filter(f, &start_nodes, &start_insts, exc.line);
            }
            if let Some(ref mut t) = exc.to {
                self.prune_endpoint_filter(t, &end_nodes, &end_insts, exc.line);
            }
            for hop in &mut exc.through_hops {
                for cname in &hop.clocks {
                    let m = format!("clock '{cname}' is not valid in -through");
                    self.push_diag("D0102", "warning", m, Some(exc.line));
                }
                hop.clocks.clear();
            }

            let no_opts = !exc.has_from_flag && !exc.has_to_flag && exc.through_hops.is_empty();
            let empty_f = exc.has_from_flag && exc.from.as_ref().is_none_or(ExceptionEndpointFilter::is_empty);
            let empty_t = exc.has_to_flag && exc.to.as_ref().is_none_or(ExceptionEndpointFilter::is_empty);
            let empty_thr = exc.through_hops.iter().any(ThroughHopFilter::is_empty);
            let vacuous = no_opts || empty_f || empty_t || empty_thr;
            if vacuous {
                let m = format!("{} at line {} has no usable object", exc.kind.kind_name(), exc.line);
                self.push_diag("D0110", "warning", m, Some(exc.line));
            }
            ValidatedExc { idx, spec: exc, vacuous }
        }).collect()
    }

    fn prune_endpoint_filter(
        &mut self, flt: &mut ExceptionEndpointFilter, valid_nodes: &BTreeSet<usize>,
        valid_insts: &BTreeSet<usize>, line: usize,
    ) {
        for net_i in std::mem::take(&mut flt.nets) {
            let m = format!("net '{}' is not a path end", self.design.nets[net_i].name);
            self.push_diag("D0102", "warning", m, Some(line));
        }
        let bad_nodes: Vec<usize> = flt.nodes.keys().copied().filter(|n| !valid_nodes.contains(n)).collect();
        for ni in bad_nodes {
            flt.nodes.remove(&ni);
            let m = format!("'{}' is not a path end", self.design.nodes[ni].name);
            self.push_diag("D0102", "warning", m, Some(line));
        }
        let bad_insts: Vec<usize> = flt.insts.keys().copied().filter(|i| !valid_insts.contains(i)).collect();
        for ii in bad_insts {
            flt.insts.remove(&ii);
            let m = format!("cell '{}' has no path end", self.design.instances[ii].name);
            self.push_diag("D0102", "warning", m, Some(line));
        }
    }

    #[must_use]
    fn are_clocks_separated(&self, launch_clk: &str, cap_clk: &str) -> bool {
        self.sdc.clock_groups.iter().any(|cg| {
            if cg.allow_paths { return false; }
            if cg.groups.len() == 1 {
                let g0 = &cg.groups[0];
                !g0.is_empty() && (g0.contains(launch_clk) ^ g0.contains(cap_clk))
            } else {
                let has_l = cg.groups.iter().any(|g| g.contains(launch_clk));
                let has_c = cg.groups.iter().any(|g| g.contains(cap_clk));
                let same = cg.groups.iter().any(|g| g.contains(launch_clk) && g.contains(cap_clk));
                has_l && has_c && !same
            }
        })
    }

    fn enumerate_stage1_raw_paths(&self, out: &mut Vec<RawPath>) {
        let ep_nodes: BTreeSet<usize> = self.endpoints.iter().map(|e| e.node_idx).collect();
        let clk_srcs: BTreeSet<String> = self.sdc.clocks.values().flat_map(|c| c.sources.iter().cloned()).collect();

        for &pnode in &self.design.input_ports {
            if self.consts.pin_vals[pnode] != LogicVal::X { continue; }
            let pname = self.design.nodes[pnode].name.clone();
            if clk_srcs.contains(&pname) { continue; }
            let io_delays = self.sdc.input_delays_for_port(&pname);
            if io_delays.is_empty() {
                for tr in Trans::ALL {
                    let drv = |an| self.driving_cell_delay_and_slew(pnode, &pname, tr, an).map_or(0.0, |x| x.0);
                    let (dl, de) = (drv(Analysis::Late), drv(Analysis::Early));
                    let seed = RawPath {
                        launch: None, hops: vec![(pnode, tr)], hop_arrs: vec![(Some(dl), Some(de))],
                        arr_late: Some((dl, 0.0)), arr_early: Some((de, 0.0)),
                    };
                    self.collect_raw_paths(pnode, tr, seed, &mut vec![pnode], &ep_nodes, out);
                }
            } else {
                for id in io_delays {
                    let Some(cdef) = self.sdc.clocks.get(&id.clock) else { continue };
                    for tr in Trans::ALL {
                        let eval_io = |an: Analysis| {
                            let v = id.bound(tr, an)?;
                            let ls = if id.source_latency_included { 0.0 } else { cdef.source_latency(id.clock_edge, an) };
                            let ln = if id.network_latency_included { 0.0 } else { cdef.network_latency(id.clock_edge, an) };
                            let drv = self.driving_cell_delay_and_slew(pnode, &pname, tr, an).map_or(0.0, |x| x.0);
                            Some((v + ls + ln + drv, ls + ln))
                        };
                        let (al, ae) = (eval_io(Analysis::Late), eval_io(Analysis::Early));
                        if al.is_none() && ae.is_none() { continue; }
                        let seed = RawPath {
                            launch: Some((id.clock.clone(), id.clock_edge)), hops: vec![(pnode, tr)],
                            hop_arrs: vec![(al.map(|x| x.0), ae.map(|x| x.0))], arr_late: al, arr_early: ae,
                        };
                        self.collect_raw_paths(pnode, tr, seed, &mut vec![pnode], &ep_nodes, out);
                    }
                }
            }
        }

        for (inst_idx, inst) in self.design.instances.iter().enumerate() {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else { continue };
            for (la_idx, la) in cdef.launch_arcs.iter().enumerate() {
                if self.consts.disabled_launch_arcs.contains_key(&(inst_idx, la_idx)) { continue; }
                let (Some(&ck_n), Some(&q_n)) = (inst.pin_nodes.get(&la.clock_pin), inst.pin_nodes.get(&la.to_pin)) else {
                    continue;
                };
                if self.consts.pin_vals[ck_n] != LogicVal::X || self.consts.pin_vals[q_n] != LogicVal::X {
                    continue;
                }
                let Some(waves) = self.clk_waves.get(&ck_n) else { continue };
                for w in waves {
                    if w.pin_edge != la.clock_edge { continue; }
                    let Some(clk_def) = self.sdc.clocks.get(&w.clock) else { continue };
                    for out_tr in Trans::ALL {
                        let eval_la = |an: Analysis| {
                            let s = clk_def.transition(w.pin_edge, an);
                            let (d, _) = la.eval(out_tr, s, self.node_load_pf(q_n, out_tr, an))?;
                            let lat = clk_def.total_latency(w.source_edge, an);
                            Some((lat + d, lat))
                        };
                        let (al, ae) = (eval_la(Analysis::Late), eval_la(Analysis::Early));
                        if al.is_none() && ae.is_none() { continue; }
                        let seed = RawPath {
                            launch: Some((w.clock.clone(), w.source_edge)), hops: vec![(ck_n, w.pin_edge), (q_n, out_tr)],
                            hop_arrs: vec![(al.map(|x| x.1), ae.map(|x| x.1)), (al.map(|x| x.0), ae.map(|x| x.0))],
                            arr_late: al, arr_early: ae,
                        };
                        self.collect_raw_paths(q_n, out_tr, seed, &mut vec![q_n], &ep_nodes, out);
                    }
                }
            }
        }
    }

    const MAX_RAW_DFS_VISITS: usize = 512;
    const MAX_RAW_PATHS: usize = 4096;
    /// Budget of the paths enumerated for exception matching; beyond it the check is undecided.
    pub(crate) const MAX_ENUMERATED_PATHS: usize = 200_000;

    fn collect_raw_paths(
        &self, u: usize, u_tr: Trans, seed: RawPath, visited: &mut Vec<usize>,
        ep_nodes: &BTreeSet<usize>, out: &mut Vec<RawPath>,
    ) {
        if out.len() >= Self::MAX_ENUMERATED_PATHS { return; }
        let has_through = self.sdc.exceptions.iter().any(|e| !e.through_hops.is_empty());
        if (has_through || self.design.nodes.len() <= 64) && out.len() < Self::MAX_RAW_PATHS {
            let saved = out.len();
            let mut visits = 0usize;
            self.dfs_raw_paths(u, u_tr, seed.clone(), visited, ep_nodes, &mut visits, out);
            if visits <= Self::MAX_RAW_DFS_VISITS && out.len() < Self::MAX_RAW_PATHS {
                return;
            }
            out.truncate(saved);
        }
        self.dp_raw_paths_from_seed(u, u_tr, seed, ep_nodes, out);
    }

    fn dp_raw_paths_from_seed(
        &self, u: usize, u_tr: Trans, seed: RawPath, ep_nodes: &BTreeSet<usize>, out: &mut Vec<RawPath>,
    ) {
        let n = self.design.nodes.len();
        let mut bl: Vec<[Option<((f64, f64), Option<(usize, Trans)>)>; 2]> = vec![[None, None]; n];
        let mut be: Vec<[Option<((f64, f64), Option<(usize, Trans)>)>; 2]> = vec![[None, None]; n];
        bl[u][u_tr.idx()] = seed.arr_late.map(|a| (a, None));
        be[u][u_tr.idx()] = seed.arr_early.map(|a| (a, None));

        for &node in &self.topo_order {
            for ntr in Trans::ALL {
                let (cl, ce) = (bl[node][ntr.idx()].map(|x| x.0), be[node][ntr.idx()].map(|x| x.0));
                if cl.is_none() && ce.is_none() { continue; }
                if let Some(net_idx) = self.design.nodes[node].net_idx {
                    let net = &self.design.nets[net_idx];
                    if net.drivers.contains(&node) {
                        for &snk in &net.sinks {
                            if self.consts.pin_vals[snk] != LogicVal::X { continue; }
                            if let Some(al) = cl && bl[snk][ntr.idx()].is_none_or(|(p, _)| al.0 > p.0) {
                                bl[snk][ntr.idx()] = Some((al, Some((node, ntr))));
                            }
                            if let Some(ae) = ce && be[snk][ntr.idx()].is_none_or(|(p, _)| ae.0 < p.0) {
                                be[snk][ntr.idx()] = Some((ae, Some((node, ntr))));
                            }
                        }
                    }
                }
                if let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[node].kind {
                    let inst = &self.design.instances[inst_idx];
                    if let Some(cdef) = self.lib.cells.get(&inst.cell_type) {
                        for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
                            if ca.from_pin != *pin || !self.is_comb_arc_enabled(inst_idx, ca_idx) || cdef.is_latch_dq(&ca.from_pin, &ca.to_pin) {
                                continue;
                            }
                            let Some(&v) = inst.pin_nodes.get(&ca.to_pin) else { continue };
                            if self.consts.pin_vals[v] != LogicVal::X { continue; }
                            let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
                            for out_tr in Trans::ALL {
                                if !ca.can_propagate(&sense, ntr, out_tr) { continue; }
                                if let Some((a, lat)) = cl
                                    && let Some(s) = self.slews[node][ntr.idx()][Analysis::Late.idx()]
                                    && let Some((d, _)) = ca.eval(out_tr, s, self.node_load_pf(v, out_tr, Analysis::Late))
                                    && bl[v][out_tr.idx()].is_none_or(|(p, _)| a + d > p.0)
                                {
                                    bl[v][out_tr.idx()] = Some(((a + d, lat), Some((node, ntr))));
                                }
                                if let Some((a, lat)) = ce
                                    && let Some(s) = self.slews[node][ntr.idx()][Analysis::Early.idx()]
                                    && let Some((d, _)) = ca.eval(out_tr, s, self.node_load_pf(v, out_tr, Analysis::Early))
                                    && be[v][out_tr.idx()].is_none_or(|(p, _)| a + d < p.0)
                                {
                                    be[v][out_tr.idx()] = Some(((a + d, lat), Some((node, ntr))));
                                }
                            }
                        }
                    }
                }
            }
        }

        let recon = |tbl: &[[Option<((f64, f64), Option<(usize, Trans)>)>; 2]], ep_n: usize, etr: Trans, is_late: bool| {
            let (arr, _) = tbl[ep_n][etr.idx()]?;
            let mut rev_hops = Vec::new();
            let mut rev_arrs = Vec::new();
            let mut cur = (ep_n, etr);
            for _ in 0..=n {
                let a = tbl[cur.0][cur.1.idx()].map(|x| x.0.0);
                rev_hops.push(cur);
                rev_arrs.push(if is_late { (a, None) } else { (None, a) });
                let Some(prev) = tbl[cur.0][cur.1.idx()].and_then(|x| x.1) else { break };
                cur = prev;
            }
            let p_len = seed.hops.len().saturating_sub(1);
            let mut hops = seed.hops[..p_len].to_vec();
            let mut hop_arrs = seed.hop_arrs[..p_len].to_vec();
            rev_hops.reverse();
            rev_arrs.reverse();
            hops.extend(rev_hops);
            hop_arrs.extend(rev_arrs);
            Some(RawPath {
                launch: seed.launch.clone(), hops, hop_arrs,
                arr_late: if is_late { Some(arr) } else { None },
                arr_early: if is_late { None } else { Some(arr) },
            })
        };

        for &ep_n in ep_nodes {
            for etr in Trans::ALL {
                if out.len() >= Self::MAX_ENUMERATED_PATHS { return; }
                if let Some(rp_l) = recon(&bl, ep_n, etr, true) { out.push(rp_l); }
                if let Some(rp_e) = recon(&be, ep_n, etr, false) { out.push(rp_e); }
            }
        }
    }

    fn dfs_raw_paths(
        &self, u: usize, u_tr: Trans, mut cur: RawPath, visited: &mut Vec<usize>,
        ep_nodes: &BTreeSet<usize>, visits: &mut usize, out: &mut Vec<RawPath>,
    ) {
        *visits += 1;
        if *visits > Self::MAX_RAW_DFS_VISITS || out.len() >= Self::MAX_RAW_PATHS {
            return;
        }
        if ep_nodes.contains(&u) {
            out.push(cur.clone());
        }
        if let Some(net_idx) = self.design.nodes[u].net_idx {
            let net = &self.design.nets[net_idx];
            if net.drivers.contains(&u) {
                for &snk in &net.sinks {
                    if self.consts.pin_vals[snk] != LogicVal::X || visited.contains(&snk) { continue; }
                    cur.hops.push((snk, u_tr));
                    cur.hop_arrs.push((cur.arr_late.map(|x| x.0), cur.arr_early.map(|x| x.0)));
                    visited.push(snk);
                    self.dfs_raw_paths(snk, u_tr, cur.clone(), visited, ep_nodes, visits, out);
                    visited.pop();
                    cur.hop_arrs.pop();
                    cur.hops.pop();
                }
            }
        }

        let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind else { return };
        let inst = &self.design.instances[inst_idx];
        let Some(cdef) = self.lib.cells.get(&inst.cell_type) else { return };
        for (ca_idx, ca) in cdef.comb_arcs.iter().enumerate() {
            if ca.from_pin != *pin || !self.is_comb_arc_enabled(inst_idx, ca_idx) || cdef.is_latch_dq(&ca.from_pin, &ca.to_pin) {
                continue;
            }
            let Some(&v) = inst.pin_nodes.get(&ca.to_pin) else { continue };
            if self.consts.pin_vals[v] != LogicVal::X || visited.contains(&v) { continue; }
            let sense = self.effective_comb_sense(inst_idx, ca_idx, &ca.sense);
            for out_tr in Trans::ALL {
                if !ca.can_propagate(&sense, u_tr, out_tr) { continue; }
                let step = |arr: Option<(f64, f64)>, an: Analysis| {
                    let (a, lat) = arr?;
                    let s_in = self.slews[u][u_tr.idx()][an.idx()]?;
                    let (d, _) = ca.eval(out_tr, s_in, self.node_load_pf(v, out_tr, an))?;
                    Some((a + d, lat))
                };
                let (nl, ne) = (step(cur.arr_late, Analysis::Late), step(cur.arr_early, Analysis::Early));
                if nl.is_none() && ne.is_none() { continue; }
                let mut next_rp = cur.clone();
                next_rp.hops.push((v, out_tr));
                next_rp.hop_arrs.push((nl.map(|x| x.0), ne.map(|x| x.0)));
                next_rp.arr_late = nl;
                next_rp.arr_early = ne;
                visited.push(v);
                self.dfs_raw_paths(v, out_tr, next_rp, visited, ep_nodes, visits, out);
                visited.pop();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn latch_open_close_bounds(
        &self, rp: &RawPath, inst_name: &str, d_pin_name: &str, cap_clk: &ClockDef,
        close_src_edge: Trans, pulse_w: f64, lat_c: f64, s_val: f64, (multiplier, use_start): (i32, bool),
    ) -> Result<Option<(f64, f64, f64)>, Unsupported> {
        let Some((ref lc, le)) = rp.launch else { return Ok(None) };
        let Some(l_clk) = self.sdc.clocks.get(lc) else { return Ok(None) };
        let (d_close, _) = clock_relations_ns(l_clk, le, cap_clk, close_src_edge)?;
        let t_s = if use_start { l_clk.period_ns } else { cap_clk.period_ns };
        let d_close_sh = d_close + ((multiplier - 1) as f64) * t_s;
        let d_open_sh = d_close_sh - pulse_w;
        let u_s = self.sdc.clock_uncertainty(lc, &cap_clk.name, Analysis::Late);
        let r_open = d_open_sh + lat_c - u_s;
        let r_close_u = d_close_sh + lat_c - s_val - u_s;
        let r_close = self.sdc.max_time_borrow_for(inst_name, d_pin_name, &cap_clk.name).map_or(r_close_u, |b| r_close_u.min(r_open + b));
        Ok(Some((d_open_sh, r_open, r_close)))
    }

    fn evaluate_latch_borrowing(
        &self, stage1_paths: &[RawPath], validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<Vec<BorrowedLaunch>, Unsupported> {
        let mut launches = Vec::new();
        for inst in &self.design.instances {
            let Some(cdef) = self.lib.cells.get(&inst.cell_type) else { continue };
            for latch in &cdef.latches {
                let (Some(&d_n), Some(&g_n), Some(&q_n)) = (
                    inst.pin_nodes.get(&latch.data_pin), inst.pin_nodes.get(&latch.enable_pin), inst.pin_nodes.get(&latch.q_pin),
                ) else {
                    continue;
                };
                if [d_n, g_n, q_n].iter().any(|&n| self.consts.pin_vals[n] != LogicVal::X) { continue; }
                let Some(dq_arc) = cdef.comb_arcs.iter().find(|ca| ca.from_pin == latch.data_pin && ca.to_pin == latch.q_pin) else {
                    continue;
                };
                let Some(g_waves) = self.clk_waves.get(&g_n) else { continue };
                let (close_edge, open_edge) = (latch.close_edge(), latch.open_edge());
                let setup_chk = cdef.check_arcs.iter().find(|chk| {
                    chk.data_pin == latch.data_pin && chk.clock_pin == latch.enable_pin
                        && chk.kind == CheckArcKind::Setup && chk.clock_edge == close_edge
                });
                let d_pin_name = &self.design.nodes[d_n].name;

                for w_close in g_waves {
                    if w_close.pin_edge != close_edge { continue; }
                    let Some(w_open) = g_waves.iter().find(|w| {
                        w.clock == w_close.clock && w.source_port == w_close.source_port && w.pin_edge == open_edge
                    }) else {
                        continue;
                    };
                    let Some(cap_clk) = self.sdc.clocks.get(&w_close.clock) else { continue };
                    let pulse_w = pulse_width_ns(cap_clk, w_open.source_edge, w_close.source_edge);
                    let lat_c = cap_clk.total_latency(w_close.source_edge, Analysis::Early);

                    for d_tr in Trans::ALL {
                        let Some(s_d) = self.slews[d_n][d_tr.idx()][Analysis::Late.idx()] else { continue };
                        let s_g = cap_clk.transition(close_edge, Analysis::Late);
                        let s_val = setup_chk.and_then(|c| c.constraint_opt(d_tr, s_d, s_g, 0.0)).unwrap_or(0.0);
                        let mut max_borrowed: Option<f64> = None;

                        for rp in stage1_paths {
                            if rp.hops.last() != Some(&(d_n, d_tr)) { continue; }
                            let (Some((arr_late, _)), Some(lc)) = (rp.arr_late, rp.launch_clock()) else { continue };
                            if self.are_clocks_separated(lc, &w_close.clock) { continue; }
                            let (sw, _, _) = self.resolve_path_exceptions(
                                rp, d_n, d_tr, Some((&w_close.clock, w_close.source_edge)), validated, &mut acc.tie_conflicts,
                            );
                            let mcp = match sw {
                                Winner::FalsePath(idx) | Winner::Delay(idx, _, _) => {
                                    acc.used_specs.insert(idx);
                                    continue;
                                }
                                Winner::Multicycle(idx, m, us) => {
                                    acc.used_specs.insert(idx);
                                    (m, us)
                                }
                                Winner::None => (1, false),
                            };
                            let bnds = self.latch_open_close_bounds(
                                rp, &inst.name, d_pin_name, cap_clk, w_close.source_edge, pulse_w, lat_c, s_val, mcp,
                            )?;
                            if let Some((d_open_sh, r_open, r_close)) = bnds && arr_late > r_open {
                                let b_entry = arr_late.min(r_close) - d_open_sh;
                                max_borrowed = Some(max_borrowed.map_or(b_entry, |m| m.max(b_entry)));
                            }
                        }

                        if let Some(borrowed_entry) = max_borrowed {
                            for out_tr in Trans::ALL {
                                if !dq_arc.can_propagate(&dq_arc.sense, d_tr, out_tr) { continue; }
                                let c_q = self.node_load_pf(q_n, out_tr, Analysis::Late);
                                if let Some((dq_d, _)) = dq_arc.eval(out_tr, s_d, c_q) {
                                    let (clk, se) = (w_open.clock.clone(), w_open.source_edge);
                                    launches.push((g_n, d_n, q_n, open_edge, d_tr, out_tr, clk, se, borrowed_entry + dq_d, lat_c));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(launches)
    }

    fn enumerate_borrowed_raw_paths(&self, borrowed: &[BorrowedLaunch], out: &mut Vec<RawPath>) {
        let ep_nodes: BTreeSet<usize> = self.endpoints.iter().map(|e| e.node_idx).collect();
        for &(g_n, d_n, q_n, open_e, d_tr, q_tr, ref clk, src_e, arr_q, lat_g) in borrowed {
            let seed = RawPath {
                launch: Some((clk.clone(), src_e)), hops: vec![(g_n, open_e), (d_n, d_tr), (q_n, q_tr)],
                hop_arrs: vec![(Some(lat_g), None), (Some(arr_q), None), (Some(arr_q), None)],
                arr_late: Some((arr_q, lat_g)), arr_early: None,
            };
            self.collect_raw_paths(q_n, q_tr, seed, &mut vec![q_n], &ep_nodes, out);
        }
    }

    fn resolve_path_exceptions(
        &self, rp: &RawPath, end_node: usize, end_tr: Trans, cap: Option<(&str, Trans)>,
        validated: &[ValidatedExc], tie_conflicts: &mut BTreeSet<(usize, usize)>,
    ) -> (Winner, Winner, Option<(usize, i32, bool)>) {
        let both_clocked = rp.launch.is_some() && cap.is_some();
        let mut setup_cands: Vec<Cand<'_>> = Vec::new();
        let mut hold_cands: Vec<Cand<'_>> = Vec::new();
        let mut setup_mcp_cands: Vec<Cand<'_>> = Vec::new();

        for v in validated {
            if v.vacuous { continue; }
            let Some(spec) = self.match_exception_specificity(v, rp, end_node, end_tr, cap) else { continue };
            let (sa, ci) = (v.spec.single_analysis, v.spec.cmd_index);
            match v.spec.kind {
                ExceptionKind::FalsePath => {
                    if v.spec.setup { setup_cands.push((3, spec, sa, ci, v)); }
                    if v.spec.hold { hold_cands.push((3, spec, sa, ci, v)); }
                }
                ExceptionKind::MaxDelay { .. } if v.spec.setup => setup_cands.push((2, spec, sa, ci, v)),
                ExceptionKind::MinDelay { .. } if v.spec.hold => hold_cands.push((2, spec, sa, ci, v)),
                ExceptionKind::MulticyclePath { .. } if both_clocked => {
                    if v.spec.setup {
                        setup_cands.push((1, spec, sa, ci, v));
                        setup_mcp_cands.push((1, spec, sa, ci, v));
                    }
                    if v.spec.hold { hold_cands.push((1, spec, sa, ci, v)); }
                }
                _ => {}
            }
        }

        for cands in [&mut setup_cands, &mut hold_cands, &mut setup_mcp_cands] {
            cands.sort_by_key(|&(tr, sp, sa, ci, _)| (tr, sp, sa, ci));
        }
        record_tie_conflicts(&setup_cands, true, tie_conflicts);
        record_tie_conflicts(&hold_cands, false, tie_conflicts);
        record_tie_conflicts(&setup_mcp_cands, true, tie_conflicts);

        let setup_mcp = match pick_winner(&setup_mcp_cands, true) {
            Winner::Multicycle(idx, m, us) => Some((idx, m, us)),
            _ => None,
        };
        (pick_winner(&setup_cands, true), pick_winner(&hold_cands, false), setup_mcp)
    }

    fn match_exception_specificity(
        &self, v: &ValidatedExc, rp: &RawPath, end_node: usize, end_tr: Trans, cap: Option<(&str, Trans)>,
    ) -> Option<u8> {
        if v.spec.end_tr_filter.is_some_and(|req| req != end_tr) { return None; }
        let mut spec = 0u8;
        if let Some(ref f) = v.spec.from {
            let (sn, str_e) = rp.hops[0];
            spec += self.match_end_filter(f, sn, str_e, rp.launch_clock().zip(rp.launch_edge()), 16, 2)?;
        }
        if let Some(ref t) = v.spec.to {
            spec += self.match_end_filter(t, end_node, end_tr, cap, 8, 1)?;
        }
        if !v.spec.through_hops.is_empty() {
            let mut cur = 0usize;
            for thr in &v.spec.through_hops {
                cur = rp.hops.iter().enumerate().skip(cur).find(|&(_, &(ni, htr))| self.hop_matches_through(thr, ni, htr))?.0;
            }
            spec += 4;
        }
        Some(spec)
    }

    fn match_end_filter(
        &self, flt: &ExceptionEndpointFilter, node: usize, tr: Trans,
        clk_info: Option<(&str, Trans)>, obj_w: u8, clk_w: u8,
    ) -> Option<u8> {
        let mut obj_m = flt.nodes.get(&node).is_some_and(|t| t.is_none_or(|x| x == tr));
        if !obj_m && let NodeKind::InstancePin { inst_idx, .. } = self.design.nodes[node].kind {
            obj_m = flt.insts.get(&inst_idx).is_some_and(|t| t.is_none_or(|x| x == tr));
        }
        let clk_m = clk_info.is_some_and(|(c, e)| flt.clocks.get(c).is_some_and(|t| t.is_none_or(|x| x == e)));
        if obj_m { Some(obj_w) } else if clk_m { Some(clk_w) } else { None }
    }

    fn hop_matches_through(&self, thr: &ThroughHopFilter, node_idx: usize, hop_tr: Trans) -> bool {
        let ok = |opt: Option<&Option<Trans>>| opt.is_some_and(|t| t.is_none_or(|x| x == hop_tr));
        if ok(thr.nodes.get(&node_idx)) { return true; }
        if let Some(net_i) = self.design.nodes[node_idx].net_idx && ok(thr.nets.get(&net_i)) { return true; }
        if let NodeKind::InstancePin { inst_idx, .. } = self.design.nodes[node_idx].kind && ok(thr.insts.get(&inst_idx)) {
            return true;
        }
        false
    }

    fn evaluate_all_paths_and_endpoints(
        &mut self, all_raw_paths: &[RawPath], validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let clk_reaching = self.nodes_reaching_clock_pins();
        let mut ep_by_node: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (idx, ep) in self.endpoints.iter().enumerate() {
            ep_by_node.entry(ep.node_idx).or_default().push(idx);
        }

        let mut new_paths: Vec<PathEval> = Vec::new();
        let n_eps = self.endpoints.len();
        let mut ep_setup_slack: Vec<Option<f64>> = vec![None; n_eps];
        let mut ep_hold_slack: Vec<Option<f64>> = vec![None; n_eps];
        let mut ep_max_borrow: Vec<f64> = vec![0.0; n_eps];
        let mut ep_had_fp: Vec<bool> = vec![false; n_eps];
        let mut ep_tag_setup: Vec<BTreeMap<(Tag, Trans), f64>> = vec![BTreeMap::new(); n_eps];
        let mut ep_tag_hold: Vec<BTreeMap<(Tag, Trans), f64>> = vec![BTreeMap::new(); n_eps];

        let n_nodes = self.design.nodes.len();
        let mut node_best_late: Vec<[Option<(f64, f64)>; 2]> = vec![[None, None]; n_nodes];
        let mut node_best_early: Vec<[Option<(f64, f64)>; 2]> = vec![[None, None]; n_nodes];

        for rp in all_raw_paths {
            let Some(&(u, u_tr)) = rp.hops.last() else { continue };
            let Some(ep_indices) = ep_by_node.get(&u) else { continue };
            for &ep_idx in ep_indices {
                let ep = &self.endpoints[ep_idx];
                if matches!(ep.status, EndpointStatus::Disabled | EndpointStatus::Unclocked) { continue; }
                let pe_opt = self.evaluate_raw_path_at_endpoint(rp, ep, u, u_tr, &clk_reaching, validated, acc)?;
                if acc.was_disabled { ep_had_fp[ep_idx] = true; }
                ep_max_borrow[ep_idx] = ep_max_borrow[ep_idx].max(acc.max_borrow_ns);

                let Some(pe) = pe_opt else { continue };
                if let Some(s) = pe.setup_slack_ns {
                    ep_setup_slack[ep_idx] = Some(ep_setup_slack[ep_idx].map_or(s, |m| m.min(s)));
                    if let (Some(lc), Some(le)) = (&pe.launch_clock, pe.launch_edge) {
                        let tag = Tag { clock: lc.clone(), edge: le };
                        ep_tag_setup[ep_idx].entry((tag, u_tr)).and_modify(|v| *v = v.min(s)).or_insert(s);
                    }
                    for (&(hn, htr), &(hal, _)) in rp.hops.iter().zip(rp.hop_arrs.iter()) {
                        if let Some(al) = hal && node_best_late[hn][htr.idx()].is_none_or(|(_, ps)| s < ps) {
                            node_best_late[hn][htr.idx()] = Some((al + s, s));
                        }
                    }
                }
                if let Some(h) = pe.hold_slack_ns {
                    ep_hold_slack[ep_idx] = Some(ep_hold_slack[ep_idx].map_or(h, |m| m.min(h)));
                    if let (Some(lc), Some(le)) = (&pe.launch_clock, pe.launch_edge) {
                        let tag = Tag { clock: lc.clone(), edge: le };
                        ep_tag_hold[ep_idx].entry((tag, u_tr)).and_modify(|v| *v = v.min(h)).or_insert(h);
                    }
                    for (&(hn, htr), &(_, hae)) in rp.hops.iter().zip(rp.hop_arrs.iter()) {
                        if let Some(ae) = hae && node_best_early[hn][htr.idx()].is_none_or(|(_, ph)| h < ph) {
                            node_best_early[hn][htr.idx()] = Some((ae - h, h));
                        }
                    }
                }
                new_paths.push(pe);
            }
        }

        for (idx, ep) in self.endpoints.iter_mut().enumerate() {
            if matches!(ep.status, EndpointStatus::Disabled | EndpointStatus::Unclocked) { continue; }
            ep.setup_slack_ns = ep_setup_slack[idx];
            ep.hold_slack_ns = ep_hold_slack[idx];
            ep.max_borrow_ns = ep_max_borrow[idx];
            ep.tag_setup_slack = ep_tag_setup[idx].clone();
            ep.tag_hold_slack = ep_tag_hold[idx].clone();
            if ep.setup_slack_ns.is_some() || ep.hold_slack_ns.is_some() {
                ep.status = EndpointStatus::Constrained;
            } else if ep_had_fp[idx] || ep.status == EndpointStatus::Constrained {
                ep.status = EndpointStatus::FalsePathOnly;
            }
        }

        new_paths.sort_by(|a, b| {
            let (ae, be) = (a.setup_slack_ns.is_none(), b.setup_slack_ns.is_none());
            let ak = a.setup_slack_ns.or(a.hold_slack_ns).unwrap_or(f64::INFINITY);
            let bk = b.setup_slack_ns.or(b.hold_slack_ns).unwrap_or(f64::INFINITY);
            ae.cmp(&be).then_with(|| ak.total_cmp(&bk))
                .then_with(|| a.endpoint.cmp(&b.endpoint))
                .then_with(|| a.startpoint.cmp(&b.startpoint))
                .then_with(|| a.end_tr.cmp(&b.end_tr))
        });
        self.paths = new_paths;

        for u in 0..n_nodes {
            for tr in Trans::ALL {
                let (bl, be) = (node_best_late[u][tr.idx()], node_best_early[u][tr.idx()]);
                self.node_req_ns[u][tr.idx()][Analysis::Late.idx()] = bl.map(|x| x.0);
                self.node_slack_ns[u][tr.idx()][Analysis::Late.idx()] = bl.map(|x| x.1);
                self.node_req_ns[u][tr.idx()][Analysis::Early.idx()] = be.map(|x| x.0);
                self.node_slack_ns[u][tr.idx()][Analysis::Early.idx()] = be.map(|x| x.1);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_raw_path_at_endpoint(
        &self, rp: &RawPath, ep: &EndpointEval, u: usize, u_tr: Trans,
        clk_reaching: &[bool], validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<Option<PathEval>, Unsupported> {
        acc.reset_for_path();
        match ep.kind {
            CheckKindEval::SetupHold | CheckKindEval::RecoveryRemoval => {
                self.eval_path_setup_hold(rp, ep, u, u_tr, validated, acc)?;
            }
            CheckKindEval::LatchSetupHold => self.eval_path_latch(rp, ep, u, u_tr, validated, acc)?,
            CheckKindEval::ClockGating => self.eval_path_clock_gating(rp, u, u_tr, clk_reaching, validated, acc)?,
            CheckKindEval::OutputDelay => self.eval_path_output_delay(rp, ep, u, u_tr, validated, acc)?,
        }

        let setup_slack = acc.best_setup.as_ref().map(|(arr, req, _, _)| req - arr);
        let hold_slack = acc.best_hold.as_ref().map(|(arr, req, _, _)| arr - req);
        if setup_slack.is_none() && hold_slack.is_none() { return Ok(None); }
        let best_ref = acc.best_setup.as_ref().or(acc.best_hold.as_ref());
        let (start_n, start_tr) = rp.hops[0];

        Ok(Some(PathEval {
            startpoint: self.design.nodes[start_n].name.clone(),
            endpoint: ep.endpoint.clone(), start_tr, end_tr: u_tr,
            launch_clock: rp.launch_clock().map(str::to_string), launch_edge: rp.launch_edge(),
            capture_clock: best_ref.map(|x| x.2.clone()), capture_edge: best_ref.and_then(|x| x.3),
            kind: ep.kind, path_group: ep.path_group.clone(),
            pins: rp.hops.iter().map(|&(ni, tr)| (self.design.nodes[ni].name.clone(), tr)).collect(),
            arrival_late_ns: acc.best_setup.as_ref().map(|x| x.0).or(rp.arr_late.map(|x| x.0)),
            arrival_early_ns: acc.best_hold.as_ref().map(|x| x.0).or(rp.arr_early.map(|x| x.0)),
            setup_required_ns: acc.best_setup.as_ref().map(|x| x.1),
            hold_required_ns: acc.best_hold.as_ref().map(|x| x.1),
            setup_slack_ns: setup_slack, hold_slack_ns: hold_slack,
        }))
    }

    fn eval_path_setup_hold(
        &self, rp: &RawPath, ep: &EndpointEval, u: usize, u_tr: Trans,
        validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind else { return Ok(()) };
        let inst = &self.design.instances[inst_idx];
        let cdef = &self.lib.cells[&inst.cell_type];
        let is_async = ep.kind == CheckKindEval::RecoveryRemoval;
        let rel_out_cap = cdef.pins.iter().find(|(_, p)| p.direction == Direction::Output)
            .and_then(|(pname, _)| inst.pin_nodes.get(pname))
            .map_or(0.0, |&qn| self.node_load_pf(qn, Trans::Rise, Analysis::Late));

        for (chk_idx, chk) in cdef.check_arcs.iter().enumerate() {
            if chk.data_pin != *pin || chk.kind.is_async() != is_async || self.consts.disabled_check_arcs.contains_key(&(inst_idx, chk_idx)) {
                continue;
            }
            let Some(&ck_node) = inst.pin_nodes.get(&chk.clock_pin) else { continue };
            let Some(waves) = self.clk_waves.get(&ck_node) else { continue };
            for w in waves {
                if w.pin_edge != chk.clock_edge { continue; }
                let Some(cap_clk) = self.sdc.clocks.get(&w.clock) else { continue };
                if rp.launch_clock().is_some_and(|lc| self.are_clocks_separated(lc, &w.clock)) {
                    acc.was_disabled = true;
                    continue;
                }
                let (sw, hw, smcp) = self.resolve_path_exceptions(rp, u, u_tr, Some((&w.clock, w.source_edge)), validated, &mut acc.tie_conflicts);
                if chk.kind.is_setup_like() {
                    if let Some(s_d) = self.slews[u][u_tr.idx()][Analysis::Late.idx()]
                        && let Some(s_val) = chk.constraint_opt(u_tr, s_d, cap_clk.transition(w.pin_edge, Analysis::Late), rel_out_cap)
                    {
                        let lc = cap_clk.total_latency(w.source_edge, Analysis::Early);
                        self.apply_setup_winner(rp, cap_clk, w.source_edge, (lc, lc), s_val, &sw, acc)?;
                    }
                } else if let Some(s_d) = self.slews[u][u_tr.idx()][Analysis::Early.idx()]
                    && let Some(h_val) = chk.constraint_opt(u_tr, s_d, cap_clk.transition(w.pin_edge, Analysis::Early), rel_out_cap)
                {
                    let lc = cap_clk.total_latency(w.source_edge, Analysis::Late);
                    self.apply_hold_winner(rp, cap_clk, w.source_edge, (lc, lc), h_val, false, &hw, smcp, acc)?;
                }
            }
        }
        Ok(())
    }

    fn eval_path_latch(
        &self, rp: &RawPath, ep: &EndpointEval, u: usize, u_tr: Trans,
        validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let NodeKind::InstancePin { inst_idx, ref pin } = self.design.nodes[u].kind else { return Ok(()) };
        let inst = &self.design.instances[inst_idx];
        let cdef = &self.lib.cells[&inst.cell_type];
        let Some(latch) = cdef.latch_for_d(pin) else { return Ok(()) };
        let Some(&g_node) = inst.pin_nodes.get(&latch.enable_pin) else { return Ok(()) };
        let Some(g_waves) = self.clk_waves.get(&g_node) else { return Ok(()) };
        let (close_edge, open_edge) = (latch.close_edge(), latch.open_edge());
        let find_chk = |k| cdef.check_arcs.iter().find(|c| {
            c.data_pin == latch.data_pin && c.clock_pin == latch.enable_pin && c.kind == k && c.clock_edge == close_edge
        });
        let (setup_chk, hold_chk) = (find_chk(CheckArcKind::Setup), find_chk(CheckArcKind::Hold));

        for w_close in g_waves {
            if w_close.pin_edge != close_edge { continue; }
            let Some(w_open) = g_waves.iter().find(|w| {
                w.clock == w_close.clock && w.source_port == w_close.source_port && w.pin_edge == open_edge
            }) else {
                continue;
            };
            let Some(cap_clk) = self.sdc.clocks.get(&w_close.clock) else { continue };
            if rp.launch_clock().is_some_and(|lc| self.are_clocks_separated(lc, &w_close.clock)) {
                acc.was_disabled = true;
                continue;
            }
            let (sw, hw, smcp) = self.resolve_path_exceptions(rp, u, u_tr, Some((&w_close.clock, w_close.source_edge)), validated, &mut acc.tie_conflicts);
            let pulse_w = pulse_width_ns(cap_clk, w_open.source_edge, w_close.source_edge);

            if let (Some((arr_l, _)), Some(s_d)) = (rp.arr_late, self.slews[u][u_tr.idx()][Analysis::Late.idx()]) {
                let s_g = cap_clk.transition(close_edge, Analysis::Late);
                let s_val = setup_chk.and_then(|c| c.constraint_opt(u_tr, s_d, s_g, 0.0)).unwrap_or(0.0);
                let lc = cap_clk.total_latency(w_close.source_edge, Analysis::Early);

                let mcp_opt = match sw {
                    Winner::FalsePath(_) | Winner::Delay(_, _, _) => {
                        self.apply_setup_winner(rp, cap_clk, w_close.source_edge, (lc, lc), s_val, &sw, acc)?;
                        None
                    }
                    Winner::Multicycle(idx, m, us) => {
                        acc.used_specs.insert(idx);
                        Some((m, us))
                    }
                    Winner::None => Some((1, false)),
                };
                if let Some(mcp) = mcp_opt
                    && let Some((_, r_open, r_close)) = self.latch_open_close_bounds(rp, &inst.name, &ep.endpoint, cap_clk, w_close.source_edge, pulse_w, lc, s_val, mcp)?
                {
                    let eff_req = if arr_l <= r_open {
                        r_open
                    } else if arr_l <= r_close {
                        acc.max_borrow_ns = acc.max_borrow_ns.max(arr_l - r_open);
                        arr_l
                    } else {
                        acc.max_borrow_ns = acc.max_borrow_ns.max(r_close - r_open);
                        r_close
                    };
                    update_best_setup(&mut acc.best_setup, arr_l, eff_req, &w_close.clock, Some(w_close.source_edge));
                }
            }

            if let Some(s_d) = self.slews[u][u_tr.idx()][Analysis::Early.idx()] {
                let s_g = cap_clk.transition(close_edge, Analysis::Early);
                let h_val = hold_chk.and_then(|c| c.constraint_opt(u_tr, s_d, s_g, 0.0)).unwrap_or(0.0);
                let lc = cap_clk.total_latency(w_close.source_edge, Analysis::Late);
                self.apply_hold_winner(rp, cap_clk, w_close.source_edge, (lc, lc), h_val, false, &hw, smcp, acc)?;
            }
        }
        Ok(())
    }

    fn eval_path_clock_gating(
        &self, rp: &RawPath, u: usize, u_tr: Trans, clk_reaching: &[bool],
        validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let NodeKind::InstancePin { inst_idx, .. } = self.design.nodes[u].kind else { return Ok(()) };
        for cg in self.detect_clock_gates_on_inst(inst_idx, clk_reaching) {
            if cg.en_node != u { continue; }
            let Some(waves) = self.clk_waves.get(&cg.ck_node) else { continue };
            let (lead_edge, trail_edge) = if cg.active_low { (Trans::Fall, Trans::Rise) } else { (Trans::Rise, Trans::Fall) };
            for w_lead in waves {
                if w_lead.pin_edge != lead_edge { continue; }
                let Some(w_trail) = waves.iter().find(|w| {
                    w.clock == w_lead.clock && w.source_port == w_lead.source_port && w.pin_edge == trail_edge
                }) else {
                    continue;
                };
                let Some(cap_clk) = self.sdc.clocks.get(&w_lead.clock) else { continue };
                if rp.launch_clock().is_some_and(|lc| self.are_clocks_separated(lc, &w_lead.clock)) {
                    acc.was_disabled = true;
                    continue;
                }
                let (sw, _, _) = self.resolve_path_exceptions(rp, u, u_tr, Some((&w_lead.clock, w_lead.source_edge)), validated, &mut acc.tie_conflicts);
                let lc_s = cap_clk.total_latency(w_lead.source_edge, Analysis::Early);
                self.apply_setup_winner(rp, cap_clk, w_lead.source_edge, (lc_s, lc_s), 0.0, &sw, acc)?;

                let (_, hw, smcp) = self.resolve_path_exceptions(rp, u, u_tr, Some((&w_trail.clock, w_trail.source_edge)), validated, &mut acc.tie_conflicts);
                let lc_h = cap_clk.total_latency(w_trail.source_edge, Analysis::Late);
                self.apply_hold_winner(rp, cap_clk, w_trail.source_edge, (lc_h, lc_h), 0.0, true, &hw, smcp, acc)?;
            }
        }
        Ok(())
    }

    fn eval_path_output_delay(
        &self, rp: &RawPath, ep: &EndpointEval, u: usize, u_tr: Trans,
        validated: &[ValidatedExc], acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let ods = self.sdc.output_delays_for_port(&ep.endpoint);
        if ods.is_empty() {
            let (sw, hw, _) = self.resolve_path_exceptions(rp, u, u_tr, None, validated, &mut acc.tie_conflicts);
            if let (Some((arr_l, lat_l)), Winner::Delay(idx, d, ign)) = (rp.arr_late, sw) {
                acc.used_specs.insert(idx);
                let eff = if ign { arr_l - lat_l } else { arr_l };
                update_best_setup(&mut acc.best_setup, eff, d, "default", None);
            }
            if let (Some((arr_e, lat_e)), Winner::Delay(idx, d, ign)) = (rp.arr_early, hw) {
                acc.used_specs.insert(idx);
                let eff = if ign { arr_e - lat_e } else { arr_e };
                update_best_hold(&mut acc.best_hold, eff, d, "default", None);
            }
            return Ok(());
        }

        for od in ods {
            let Some(cap_clk) = self.sdc.clocks.get(&od.clock) else { continue };
            if rp.launch_clock().is_some_and(|lc| self.are_clocks_separated(lc, &od.clock)) {
                acc.was_disabled = true;
                continue;
            }
            let (sw, hw, smcp) = self.resolve_path_exceptions(rp, u, u_tr, Some((&od.clock, od.clock_edge)), validated, &mut acc.tie_conflicts);
            let od_lat = |an: Analysis| {
                let ls = if od.source_latency_included { 0.0 } else { cap_clk.source_latency(od.clock_edge, an) };
                let ln = if od.network_latency_included { 0.0 } else { cap_clk.network_latency(od.clock_edge, an) };
                ls + ln
            };
            if let Some(v_max) = od.bound(u_tr, Analysis::Late) {
                let lc = od_lat(Analysis::Early);
                self.apply_setup_winner(rp, cap_clk, od.clock_edge, (lc, 0.0), v_max, &sw, acc)?;
            }
            if let Some(v_min) = od.bound(u_tr, Analysis::Early) {
                let lc = od_lat(Analysis::Late);
                self.apply_hold_winner(rp, cap_clk, od.clock_edge, (lc, 0.0), -v_min, false, &hw, smcp, acc)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_setup_winner(
        &self, rp: &RawPath, cap_clk: &ClockDef, cap_edge: Trans, (lat_c, lat_c_maxd): (f64, f64),
        margin: f64, setup_win: &Winner, acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let Some((arr_l, lat_l)) = rp.arr_late else { return Ok(()) };
        let u_s = self.uncertainty_for(rp.launch_clock(), &cap_clk.name, Analysis::Late);
        let (spec_opt, n_mult, use_start) = match *setup_win {
            Winner::FalsePath(idx) => {
                if rp.launch.is_some() {
                    acc.used_specs.insert(idx);
                    acc.was_disabled = true;
                }
                return Ok(());
            }
            Winner::Delay(idx, d, ign) => {
                acc.used_specs.insert(idx);
                let eff = if ign { arr_l - lat_l } else { arr_l };
                let req = if ign { d - margin } else { d + lat_c_maxd - margin - u_s };
                update_best_setup(&mut acc.best_setup, eff, req, &cap_clk.name, Some(cap_edge));
                return Ok(());
            }
            Winner::Multicycle(idx, m, us) => (Some(idx), m, us),
            Winner::None => (None, 1, false),
        };
        if let Some((ref lc, le)) = rp.launch && let Some(l_clk) = self.sdc.clocks.get(lc) {
            if let Some(s_idx) = spec_opt { acc.used_specs.insert(s_idx); }
            let (d_s, _) = clock_relations_ns(l_clk, le, cap_clk, cap_edge)?;
            let t_s = if use_start { l_clk.period_ns } else { cap_clk.period_ns };
            let req = d_s + ((n_mult - 1) as f64) * t_s + lat_c - margin - u_s;
            update_best_setup(&mut acc.best_setup, arr_l, req, &cap_clk.name, Some(cap_edge));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_hold_winner(
        &self, rp: &RawPath, cap_clk: &ClockDef, cap_edge: Trans, (lat_c, lat_c_mind): (f64, f64),
        hold_adj: f64, is_cg: bool, hold_win: &Winner, setup_mcp: Option<(usize, i32, bool)>, acc: &mut EvalAcc,
    ) -> Result<(), Unsupported> {
        let Some((arr_e, lat_e)) = rp.arr_early else { return Ok(()) };
        let u_h = self.uncertainty_for(rp.launch_clock(), &cap_clk.name, Analysis::Early);
        let (spec_opt, m_mult, use_start_h) = match *hold_win {
            Winner::FalsePath(idx) => {
                if rp.launch.is_some() {
                    acc.used_specs.insert(idx);
                    acc.was_disabled = true;
                }
                return Ok(());
            }
            Winner::Delay(idx, d, ign) => {
                acc.used_specs.insert(idx);
                let eff = if ign { arr_e - lat_e } else { arr_e };
                let req = if ign { d + hold_adj } else { d + lat_c_mind + hold_adj + u_h };
                update_best_hold(&mut acc.best_hold, eff, req, &cap_clk.name, Some(cap_edge));
                return Ok(());
            }
            Winner::Multicycle(idx, m, us) => (Some(idx), m, us),
            Winner::None => (None, 0, true),
        };
        if let Some((ref lc, le)) = rp.launch && let Some(l_clk) = self.sdc.clocks.get(lc) {
            if let Some(s_idx) = spec_opt { acc.used_specs.insert(s_idx); }
            let (n_mult, use_start_s) = if let Some((s_idx, n, us)) = setup_mcp {
                acc.used_specs.insert(s_idx);
                (n, us)
            } else {
                (1, false)
            };
            let d_h = if is_cg {
                gating_trailing_relation_ns(l_clk, le, cap_clk, cap_edge)?
            } else {
                clock_relations_ns(l_clk, le, cap_clk, cap_edge)?.1
            };
            let t_s = if use_start_s { l_clk.period_ns } else { cap_clk.period_ns };
            let t_h = if use_start_h { l_clk.period_ns } else { cap_clk.period_ns };
            let req = d_h + ((n_mult - 1) as f64) * t_s - (m_mult as f64) * t_h + lat_c + hold_adj + u_h;
            update_best_hold(&mut acc.best_hold, arr_e, req, &cap_clk.name, Some(cap_edge));
        }
        Ok(())
    }

    fn uncertainty_for(&self, launch_clk: Option<&str>, cap_clk: &str, an: Analysis) -> f64 {
        self.sdc.clock_uncertainty(launch_clk.unwrap_or(cap_clk), cap_clk, an)
    }
}

#[rustfmt::skip]
fn pick_winner(cands: &[Cand<'_>], is_setup: bool) -> Winner {
    let Some(&(_, _, _, _, v)) = cands.last() else { return Winner::None };
    match v.spec.kind {
        ExceptionKind::FalsePath => Winner::FalsePath(v.idx),
        ExceptionKind::MaxDelay { delay_ns, ignore_clock_latency }
        | ExceptionKind::MinDelay { delay_ns, ignore_clock_latency } => Winner::Delay(v.idx, delay_ns, ignore_clock_latency),
        ExceptionKind::MulticyclePath { multiplier, is_hold, use_start } => {
            if is_setup || is_hold { Winner::Multicycle(v.idx, multiplier, use_start) } else { Winner::Multicycle(v.idx, 0, true) }
        }
    }
}

#[rustfmt::skip]
fn update_best_setup(best: &mut BestBound, eff_arr: f64, req: f64, clk: &str, edge: Option<Trans>) {
    if best.as_ref().is_none_or(|(pa, pr, _, _)| req - eff_arr < *pr - *pa) {
        *best = Some((eff_arr, req, clk.to_string(), edge));
    }
}

#[rustfmt::skip]
fn update_best_hold(best: &mut BestBound, eff_arr: f64, req: f64, clk: &str, edge: Option<Trans>) {
    if best.as_ref().is_none_or(|(pa, pr, _, _)| eff_arr - req < *pa - *pr) {
        *best = Some((eff_arr, req, clk.to_string(), edge));
    }
}

#[rustfmt::skip]
fn record_tie_conflicts(cands: &[Cand<'_>], is_setup: bool, out: &mut BTreeSet<(usize, usize)>) {
    let Some(&(win_tr, win_sp, win_sa, _, _)) = cands.last() else { return };
    let tied: Vec<&ValidatedExc> = cands.iter()
        .filter(|&&(tr, sp, sa, _, _)| (tr, sp, sa) == (win_tr, win_sp, win_sa))
        .map(|&(_, _, _, _, v)| v)
        .collect();
    for i in 0..tied.len() {
        for j in (i + 1)..tied.len() {
            let (a, b) = (tied[i], tied[j]);
            if effects_differ_in_analysis(&a.spec.kind, &b.spec.kind, is_setup) {
                out.insert((a.idx.min(b.idx), a.idx.max(b.idx)));
            }
        }
    }
}

#[rustfmt::skip]
fn effects_differ_in_analysis(a: &ExceptionKind, b: &ExceptionKind, is_setup: bool) -> bool {
    match (a, b) {
        (
            &ExceptionKind::MulticyclePath { multiplier: m1, is_hold: h1, use_start: s1 },
            &ExceptionKind::MulticyclePath { multiplier: m2, is_hold: h2, use_start: s2 },
        ) => {
            if is_setup { (m1, s1) != (m2, s2) } else {
                let e1 = if h1 { (m1, s1) } else { (0, true) };
                let e2 = if h2 { (m2, s2) } else { (0, true) };
                e1 != e2
            }
        }
        (
            &ExceptionKind::MaxDelay { delay_ns: d1, ignore_clock_latency: i1 },
            &ExceptionKind::MaxDelay { delay_ns: d2, ignore_clock_latency: i2 },
        ) | (
            &ExceptionKind::MinDelay { delay_ns: d1, ignore_clock_latency: i1 },
            &ExceptionKind::MinDelay { delay_ns: d2, ignore_clock_latency: i2 },
        ) => (d1 - d2).abs() > 1e-12 || i1 != i2,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_paths_constructs_cleanly() {
        assert_eq!(ExceptionPaths::new(), Ok(ExceptionPaths));
    }
}
