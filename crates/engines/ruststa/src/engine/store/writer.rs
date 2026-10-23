// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Deterministic `STA v2` (`sta.rbin`) writer and workspace revision staging (`owner: E10`;
//! `WS-REQ-0030` to `WS-REQ-0038`, `WS-REQ-0045`, `STA-REQ-0302`, `STA-REQ-0405`).
//!
//! [`StaStoreData`] holds the owned vectors for the 24 sections of `sta.rbin`, canonicalizes every
//! floating-point value (`-0.0 -> +0.0`, quiet `NaN`), applies [`StoreMode`] (`Full` vs
//! `Summary`), encodes the deterministic [`RbinWriter`], and populates `meta.json`'s
//! `summary.sta` and `status.sta` when staging into a workspace [`Txn`].

use std::path::Path;

use rusteda_workspace::rbin::RbinWriter;
use rusteda_workspace::schema::{JsonMap, Upstream};
use rusteda_workspace::{Error, Hash64, Result, Txn};
use serde_json::{Value, json};

use crate::engine::store::layout::{
    ClockRec, CornerRec, DisabledArcRec, EndpointRec, EpSlackRec, FORMAT_VERSION, KIND, NodeRec,
    NoteRec, PathHeaderRec, PathPointRec, PathRankRec, ScopeReport, StaMeta, StoreMode, SummaryRec,
    TABLE_NAME, TABLE_PATH, TagExtraRec, TagRec, UnconstrainedRec, canon_f32, canon_f64,
    canon_grid, section,
};

/// Complete in-memory payload for the 24 sections of an `STA v2` (`sta.rbin`) table.
#[derive(Clone, Debug, PartialEq)]
pub struct StaStoreData {
    pub str_bytes: Vec<u8>,
    pub str_offs: Vec<u32>,
    pub meta: [StaMeta; 1],
    pub corners: Vec<CornerRec>,
    pub clocks: Vec<ClockRec>,
    pub tags: Vec<TagRec>,
    pub disabled_arcs: Vec<DisabledArcRec>,
    pub node_rec: Vec<NodeRec>,
    pub tag_extra_ptr: Vec<u32>,
    pub tag_extra: Vec<TagExtraRec>,
    pub endpoints: Vec<EndpointRec>,
    pub ep_slack: Vec<EpSlackRec>,
    pub ep_order: Vec<u32>,
    pub path_rank: Vec<PathRankRec>,
    pub path_chain_ptr: Vec<u64>,
    pub path_chain: Vec<u32>,
    pub paths: Vec<PathHeaderRec>,
    pub path_points: Vec<PathPointRec>,
    pub name_hash: Vec<u32>,
    pub inst_hash: Vec<u32>,
    pub summary: Vec<SummaryRec>,
    pub unconstrained: Vec<UnconstrainedRec>,
    pub notes: Vec<NoteRec>,
    pub scope: Vec<u8>,
}

impl StaStoreData {
    /// Normalizes all floating-point fields (`-0.0 -> +0.0`, canonical quiet `NaN`) and enforces
    /// [`StoreMode::Summary`] truncation (`NODE_REC`, `TAG_EXTRA_PTR`, `TAG_EXTRA` cleared) so
    /// the encoded `.rbin` is bitwise identical across thread counts (`STA-REQ-0405`).
    pub fn canonicalize(&mut self) {
        let mode = StoreMode::from_u32(self.meta[0].store_mode).unwrap_or(StoreMode::Full);
        if mode == StoreMode::Summary {
            self.node_rec.clear();
            self.tag_extra_ptr.clear();
            self.tag_extra.clear();
        }
        for clk in &mut self.clocks {
            clk.period_ns = canon_f64(clk.period_ns);
            clk.rise_edge_ns = canon_f64(clk.rise_edge_ns);
            clk.fall_edge_ns = canon_f64(clk.fall_edge_ns);
            clk.uncertainty_setup_ns = canon_f32(clk.uncertainty_setup_ns);
            clk.uncertainty_hold_ns = canon_f32(clk.uncertainty_hold_ns);
        }
        for nr in &mut self.node_rec {
            nr.slew = canon_grid(nr.slew);
            nr.required = canon_grid(nr.required);
            nr.slack = canon_grid(nr.slack);
            nr.arrival = canon_grid(nr.arrival);
        }
        for te in &mut self.tag_extra {
            te.arrival = canon_grid(te.arrival);
        }
        for es in &mut self.ep_slack {
            es.setup_slack = canon_f64(es.setup_slack);
            es.hold_slack = canon_f64(es.hold_slack);
        }
        for pr in &mut self.path_rank {
            pr.slack_ns = canon_f32(pr.slack_ns);
        }
        for ph in &mut self.paths {
            ph.slack = canon_f64(ph.slack);
            ph.arrival = canon_f64(ph.arrival);
            ph.required = canon_f64(ph.required);
            ph.check_value = canon_f64(ph.check_value);
        }
        for pp in &mut self.path_points {
            pp.arrival = canon_f64(pp.arrival);
            pp.incr_delay = canon_f64(pp.incr_delay);
            pp.slew = canon_f64(pp.slew);
        }
        for s in &mut self.summary {
            s.setup_wns = canon_f64(s.setup_wns);
            s.setup_tns = canon_f64(s.setup_tns);
            s.hold_wns = canon_f64(s.hold_wns);
            s.hold_tns = canon_f64(s.hold_tns);
        }
    }

    /// Converts a `Full` store payload into `Summary` mode (`store_mode = 1`), dropping
    /// `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA` while keeping all endpoints, slacks, ranked
    /// paths, path chains, top-K paths, name/instance hashes, summary, and scope.
    #[must_use]
    pub fn into_summary_mode(mut self) -> Self {
        self.meta[0].store_mode = StoreMode::Summary.as_u32();
        if let Ok(mut scope) = serde_json::from_slice::<ScopeReport>(&self.scope) {
            scope.store_mode = StoreMode::Summary;
            if let Ok(bytes) = serde_json::to_vec(&scope) {
                self.scope = bytes;
            }
        }
        self.canonicalize();
        self
    }

    /// Builds an [`RbinWriter`] borrowing all 24 sections of `self` in ascending section ID order
    /// (`1..=24`).
    pub fn writer<'a>(&'a self) -> Result<RbinWriter<'a>> {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION)?;
        w.section_bytes(section::STR_BYTES, 1, &self.str_bytes)?;
        w.section::<u32>(section::STR_OFFS, &self.str_offs)?;
        w.section::<StaMeta>(section::META, &self.meta)?;
        w.section::<CornerRec>(section::CORNERS, &self.corners)?;
        w.section::<ClockRec>(section::CLOCKS, &self.clocks)?;
        w.section::<TagRec>(section::TAGS, &self.tags)?;
        w.section::<DisabledArcRec>(section::DISABLED_ARCS, &self.disabled_arcs)?;
        w.section::<NodeRec>(section::NODE_REC, &self.node_rec)?;
        w.section::<u32>(section::TAG_EXTRA_PTR, &self.tag_extra_ptr)?;
        w.section::<TagExtraRec>(section::TAG_EXTRA, &self.tag_extra)?;
        w.section::<EndpointRec>(section::ENDPOINTS, &self.endpoints)?;
        w.section::<EpSlackRec>(section::EP_SLACK, &self.ep_slack)?;
        w.section::<u32>(section::EP_ORDER, &self.ep_order)?;
        w.section::<PathRankRec>(section::PATH_RANK, &self.path_rank)?;
        w.section::<u64>(section::PATH_CHAIN_PTR, &self.path_chain_ptr)?;
        w.section::<u32>(section::PATH_CHAIN, &self.path_chain)?;
        w.section::<PathHeaderRec>(section::PATHS, &self.paths)?;
        w.section::<PathPointRec>(section::PATH_POINTS, &self.path_points)?;
        w.section::<u32>(section::NAME_HASH, &self.name_hash)?;
        w.section::<u32>(section::INST_HASH, &self.inst_hash)?;
        w.section::<SummaryRec>(section::SUMMARY, &self.summary)?;
        w.section::<UnconstrainedRec>(section::UNCONSTRAINED, &self.unconstrained)?;
        w.section::<NoteRec>(section::NOTES, &self.notes)?;
        w.section_bytes(section::SCOPE, 1, &self.scope)?;
        Ok(w)
    }

    /// Encodes `self` into deterministic `STA v2` `.rbin` bytes without upstream entries.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(self.writer()?.to_bytes())
    }

    /// Encodes `self` into deterministic `STA v2` `.rbin` bytes recording `upstream` in the
    /// `rbin` header (`WS-REQ-0038`).
    pub fn to_bytes_with_upstream(&self, upstream: &Upstream) -> Result<Vec<u8>> {
        let mut w = self.writer()?;
        w.upstream_tables(upstream)?;
        Ok(w.to_bytes())
    }

    /// Writes `self` to `path` and returns the `xxh3-64` content hash of `[header_len..EOF)`.
    pub fn write_file(&self, path: &Path) -> Result<Hash64> {
        self.writer()?.write_to(path)
    }

    /// Writes `self` with `upstream` to `path` and returns the `xxh3-64` content hash.
    pub fn write_file_with_upstream(&self, path: &Path, upstream: &Upstream) -> Result<Hash64> {
        let mut w = self.writer()?;
        w.upstream_tables(upstream)?;
        w.write_to(path)
    }

    /// Computes the revision-level `summary.sta` and `status.sta` maps (`meta.json` contract §3,
    /// `CA-STATUS`, `WS-REQ-0045`, `STA-REQ-0807`).
    ///
    /// Aggregates worst-over-corners `setup_wns_ns`, `setup_tns_ns`, `setup_violating_endpoints`,
    /// `hold_wns_ns`, `hold_tns_ns`, `hold_violating_endpoints`, the design-rule counts
    /// `max_trans_violating`, `max_cap_violating` and `max_fanout_violating` with `drv_status`,
    /// the pulse counts `min_pulse_width_violating` and `min_period_violating` with
    /// `pulse_width_status`, `total_endpoints`, `unconstrained_endpoints`, and `corners`. A table
    /// whose `SCOPE` has no limit counts reports the design-rule and pulse counts as `null` and
    /// their status as `unchecked`, never as zero (AGENTS rule 4.2).
    #[must_use]
    pub fn revision_summary_and_status(&self) -> (JsonMap, JsonMap) {
        let meta = &self.meta[0];
        let num_corners = meta.num_corners as usize;
        let num_groups = meta.num_path_groups as usize;

        // Per corner, the overall summary record is at group index 0 ("**" / overall) when
        // present, or aggregated across groups.
        let mut setup_wns = f64::INFINITY;
        let mut setup_tns = 0.0f64;
        let mut setup_violating = 0u32;
        let mut hold_wns = f64::INFINITY;
        let mut hold_tns = 0.0f64;
        let mut hold_violating = 0u32;

        if num_corners > 0 && num_groups > 0 && self.summary.len() == num_corners * num_groups {
            for c in 0..num_corners {
                // Group 0 is the corner-wide aggregate row.
                let s = &self.summary[c * num_groups];
                if s.setup_wns.is_finite() && s.setup_wns < setup_wns {
                    setup_wns = s.setup_wns;
                }
                if s.setup_tns.is_finite() && s.setup_tns < setup_tns {
                    setup_tns = s.setup_tns;
                }
                setup_violating = setup_violating.max(s.setup_violating);
                if s.hold_wns.is_finite() && s.hold_wns < hold_wns {
                    hold_wns = s.hold_wns;
                }
                if s.hold_tns.is_finite() && s.hold_tns < hold_tns {
                    hold_tns = s.hold_tns;
                }
                hold_violating = hold_violating.max(s.hold_violating);
            }
        }

        // Without a timed check there is no worst slack: `null`, never a fabricated `0.0`
        // (AGENTS rule 4.2).
        let setup_wns_val = if setup_wns.is_finite() {
            json!(canon_f64(setup_wns))
        } else {
            Value::Null
        };
        let hold_wns_val = if hold_wns.is_finite() {
            json!(canon_f64(hold_wns))
        } else {
            Value::Null
        };

        // The limit counts of the analysis, the worst corner of each (`STA-REQ-0807`).
        let limits = serde_json::from_slice::<ScopeReport>(&self.scope)
            .ok()
            .and_then(|s| s.limits);
        let worst = |kinds: &[&str]| -> Option<u64> {
            limits.as_ref().map(|corners| {
                corners
                    .iter()
                    .map(|c| {
                        kinds
                            .iter()
                            .filter_map(|k| c.kinds.get(*k))
                            .map(|k| k.violating)
                            .sum::<u64>()
                    })
                    .max()
                    .unwrap_or(0)
            })
        };
        let max_trans_viol = worst(&["max_transition"]);
        let max_cap_viol = worst(&["max_capacitance"]);
        let max_fanout_viol = worst(&["max_fanout"]);
        let mpw_viol = worst(&["min_pulse_width_high", "min_pulse_width_low"]);
        let period_viol = worst(&["min_period"]);
        let status_of = |counts: &[Option<u64>]| -> &'static str {
            if counts.iter().any(Option::is_none) {
                "unchecked"
            } else if counts.iter().any(|c| c.is_some_and(|v| v > 0)) {
                "violated"
            } else {
                "met"
            }
        };
        let drv_status = status_of(&[max_trans_viol, max_cap_viol, max_fanout_viol]);
        let pulse_status = status_of(&[mpw_viol, period_viol]);

        let mut summary = JsonMap::new();
        summary.insert("setup_wns_ns".into(), setup_wns_val);
        summary.insert("setup_tns_ns".into(), json!(canon_f64(setup_tns)));
        summary.insert("setup_violating_endpoints".into(), json!(setup_violating));
        summary.insert("hold_wns_ns".into(), hold_wns_val);
        summary.insert("hold_tns_ns".into(), json!(canon_f64(hold_tns)));
        summary.insert("hold_violating_endpoints".into(), json!(hold_violating));
        summary.insert("max_trans_violating".into(), json!(max_trans_viol));
        summary.insert("max_cap_violating".into(), json!(max_cap_viol));
        summary.insert("max_fanout_violating".into(), json!(max_fanout_viol));
        summary.insert("drv_status".into(), json!(drv_status));
        summary.insert("min_pulse_width_violating".into(), json!(mpw_viol));
        summary.insert("min_period_violating".into(), json!(period_viol));
        summary.insert("pulse_width_status".into(), json!(pulse_status));
        summary.insert("total_endpoints".into(), json!(meta.num_endpoints));
        summary.insert(
            "unconstrained_endpoints".into(),
            json!(self.unconstrained.len()),
        );
        summary.insert("corners".into(), json!(meta.num_corners));

        // Pulse width and period are timing checks; the design rules have their own status.
        let violated = setup_violating > 0 || hold_violating > 0 || pulse_status == "violated";
        let mut status = JsonMap::new();
        status.insert(
            "timing".into(),
            json!(if violated { "violated" } else { "met" }),
        );
        status.insert("drv".into(), json!(drv_status));
        status.insert("pulse_width".into(), json!(pulse_status));

        (summary, status)
    }

    /// Stages `self` into `txn` as table `"sta"` (`"sta.rbin"`) with `upstream`, and populates
    /// `summary.sta` and `status.sta` on `txn` (`WS-REQ-0038`, `WS-REQ-0045`).
    pub fn stage_to_txn(&self, txn: &mut Txn, upstream: Upstream) -> Result<Hash64> {
        let w = self.writer().map_err(|e| {
            Error::internal(format!("failed to construct the STA v2 RbinWriter: {e}"))
        })?;
        let hash = txn.add_rbin(TABLE_NAME, TABLE_PATH, &w, upstream)?;
        let (summary, status) = self.revision_summary_and_status();
        for (k, v) in summary {
            txn.set_summary(TABLE_NAME, k, v)?;
        }
        for (k, v) in status {
            txn.set_status(TABLE_NAME, k, v)?;
        }
        Ok(hash)
    }
}
