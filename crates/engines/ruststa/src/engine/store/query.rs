// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Zero-copy mapped queries on `StaStore` (`owner: E10`; `STA-REQ-0302`, `STA-REQ-0305`,
//! `STA-REQ-0312` to `STA-REQ-0315`, `WS-REQ-0037`).
//!
//! Provides `query_summary`, `query_scope`, `endpoint_table`, `query_endpoints`,
//! `query_path_handle` (`path<N>`), `reconstruct_path_from_chain` (with batched `NODE_REC`
//! cache-line prefetch along `PATH_CHAIN`), `query_path_to` (`--to <pin>`), `inspect_pin`,
//! `inspect_port`, and `inspect_instance`.

use serde::{Deserialize, Serialize};

use crate::engine::graph::CheckKind;
use crate::engine::store::layout::{
    ABSENT_EARLY_LATE, ABSENT_REQUIRED, ABSENT_SLACK, EndpointKindTag, EndpointRec, NO_INDEX,
    PATH_CHAIN_HEADER_WORDS, PATH_CHAIN_STEP_WORDS, PathPointKind, ScopeReport, StoreMode,
    StorePrecision, unpack_chain_step, unpack_f64_words,
};
use crate::engine::store::reader::{StaStore, StaStoreError, StaStoreView};
use crate::engine::types::{NodeId, Tr};

/// Compact 20-byte (`<= 22` bytes per `STA-REQ-0313`) endpoint entry in ascending `NodeId` order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompactEndpointEntry {
    pub setup_slack: f32,
    pub hold_slack: f32,
    pub node: NodeId,
    pub capture_clock: u32,
}

const _: () = assert!(size_of::<CompactEndpointEntry>() <= 22);

/// One point along a queried timing path.
#[derive(Clone, Debug, PartialEq)]
pub struct QueriedPathPoint {
    pub node: NodeId,
    pub name: String,
    pub tr: Tr,
    pub kind: PathPointKind,
    pub arrival: f64,
    pub incr_delay: f64,
    pub slew: f64,
}

/// A full timing path returned by `query_path_handle` (`path<N>`) or `query_path_to` (`--to`).
#[derive(Clone, Debug, PartialEq)]
pub struct QueriedPath {
    pub handle: String,
    pub rank_idx: usize,
    pub corner_idx: usize,
    pub corner_name: String,
    pub endpoint_idx: usize,
    pub endpoint_name: String,
    pub check_kind: String,
    pub path_group: String,
    pub launch_clock: String,
    pub capture_clock: String,
    pub slack: f64,
    pub arrival: f64,
    pub required: f64,
    pub check_value: f64,
    pub points: Vec<QueriedPathPoint>,
}

/// Per-tag arrival and predecessor state on an inspected pin or port.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedTagState {
    pub tag_idx: u32,
    pub clock_name: String,
    pub launch_edge: Option<Tr>,
    pub path_group: String,
    pub arrival: [[f64; 2]; 2],
    pub pred: [[u32; 2]; 2],
}

/// Timing state of a single pin or port returned by `inspect_pin` / `inspect_port`.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedNode {
    pub node: NodeId,
    pub name: String,
    pub is_port: bool,
    pub is_endpoint: bool,
    pub corner_idx: usize,
    pub corner_name: String,
    pub slew: [[f64; 2]; 2],
    pub required: [[f64; 2]; 2],
    pub slack: [[f64; 2]; 2],
    pub endpoint_setup_slack: Option<f64>,
    pub endpoint_hold_slack: Option<f64>,
    pub tags: Vec<InspectedTagState>,
}

/// Timing state of one pin belonging to an inspected cell instance.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedInstancePin {
    pub node: NodeId,
    pub pin_name: String,
    pub full_name: String,
    pub slew: [[f64; 2]; 2],
    pub arrival: [[f64; 2]; 2],
    pub required: [[f64; 2]; 2],
    pub slack: [[f64; 2]; 2],
}

/// Timing state of all pins on an instance returned by `inspect_instance`.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedInstance {
    pub inst_name: String,
    pub cell_name: String,
    pub corner_idx: usize,
    pub corner_name: String,
    pub pins: Vec<InspectedInstancePin>,
}

/// One row of the queried timing summary (`(corner, path_group)`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueriedSummaryRow {
    pub corner_idx: usize,
    pub corner_name: String,
    pub path_group: String,
    pub setup_wns: f64,
    pub setup_tns: f64,
    pub setup_violating: u32,
    pub setup_endpoints: u32,
    pub hold_wns: f64,
    pub hold_tns: f64,
    pub hold_violating: u32,
    pub hold_endpoints: u32,
    pub max_trans_violating: u32,
    pub max_cap_violating: u32,
    pub total_endpoints: u32,
    pub unconstrained_endpoints: u32,
}

/// Complete timing summary returned by [`StaStore::query_summary`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueriedSummary {
    pub num_nodes: u32,
    pub num_arcs: u32,
    pub num_cells: u32,
    pub num_endpoints: u32,
    pub num_unconstrained: u32,
    pub backend: String,
    pub precision: StorePrecision,
    pub store_mode: StoreMode,
    pub rows: Vec<QueriedSummaryRow>,
}

/// Endpoint slack record returned by [`StaStore::query_endpoints`].
#[derive(Clone, Debug, PartialEq)]
pub struct QueriedEndpoint {
    pub endpoint_idx: usize,
    pub data_node: NodeId,
    pub name: String,
    pub capture_clock: String,
    pub path_group: String,
    pub kind: EndpointKindTag,
    pub setup_slack: f64,
    pub hold_slack: f64,
    pub worst_setup_handle: Option<String>,
    pub worst_hold_handle: Option<String>,
}

impl StaStore {
    /// Reads the `SUMMARY`, `META`, and `CORNERS` sections (`< 32 KB`) without touching
    /// `NODE_REC`, `PATHS`, or `PATH_POINTS`.
    pub fn query_summary(&self) -> Result<QueriedSummary, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        let backend = v
            .strings
            .get(v.meta.backend_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();
        let precision = StorePrecision::from_u32(v.meta.precision).unwrap_or(StorePrecision::F64);
        let store_mode = StoreMode::from_u32(v.meta.store_mode).unwrap_or(StoreMode::Full);

        let mut rows = Vec::with_capacity(v.summary.len());
        for s in v.summary {
            let c_idx = s.corner_idx as usize;
            let corner_rec = v
                .corners
                .get(c_idx)
                .ok_or_else(|| StaStoreError::corrupt(path, "SUMMARY corner_idx out of bounds"))?;
            let corner_name = v
                .strings
                .get(corner_rec.name_str)
                .map_err(|r| StaStoreError::corrupt(path, r))?
                .to_owned();
            let path_group = v
                .strings
                .get(s.group_str)
                .map_err(|r| StaStoreError::corrupt(path, r))?
                .to_owned();
            rows.push(QueriedSummaryRow {
                corner_idx: c_idx,
                corner_name,
                path_group,
                setup_wns: s.setup_wns,
                setup_tns: s.setup_tns,
                setup_violating: s.setup_violating,
                setup_endpoints: s.setup_endpoints,
                hold_wns: s.hold_wns,
                hold_tns: s.hold_tns,
                hold_violating: s.hold_violating,
                hold_endpoints: s.hold_endpoints,
                max_trans_violating: s.max_trans_violating,
                max_cap_violating: s.max_cap_violating,
                total_endpoints: s.total_endpoints,
                unconstrained_endpoints: s.unconstrained_endpoints,
            });
        }

        Ok(QueriedSummary {
            num_nodes: v.meta.num_nodes,
            num_arcs: v.meta.num_arcs,
            num_cells: v.meta.num_cells,
            num_endpoints: v.meta.num_endpoints,
            num_unconstrained: v.unconstrained.len() as u32,
            backend,
            precision,
            store_mode,
            rows,
        })
    }

    /// Parses and returns the canonical JSON `SCOPE` section (`ScopeReport`).
    pub fn query_scope(&self) -> Result<ScopeReport, StaStoreError> {
        let v = self.view()?;
        serde_json::from_slice::<ScopeReport>(v.scope)
            .map_err(|e| StaStoreError::corrupt(self.path(), format!("invalid SCOPE JSON: {e}")))
    }

    /// Returns the compact `<= 22`-byte endpoint table in ascending `NodeId` order for
    /// `corner_idx` (`STA-REQ-0313`).
    pub fn endpoint_table(
        &self,
        corner_idx: usize,
    ) -> Result<Vec<CompactEndpointEntry>, StaStoreError> {
        let v = self.view()?;
        let n_corners = v.meta.num_corners as usize;
        if corner_idx >= n_corners {
            return Err(StaStoreError::CornerOutOfRange {
                corner: corner_idx,
                total: n_corners,
            });
        }
        let n_ep = v.endpoints.len();
        let slack_base = corner_idx * n_ep;
        let mut out = Vec::with_capacity(n_ep);
        for (i, ep) in v.endpoints.iter().enumerate() {
            let sl = &v.ep_slack[slack_base + i];
            out.push(CompactEndpointEntry {
                setup_slack: sl.setup_slack as f32,
                hold_slack: sl.hold_slack as f32,
                node: NodeId(ep.data_node),
                capture_clock: ep.capture_clock,
            });
        }
        Ok(out)
    }

    /// Queries endpoints ordered by ascending slack for `corner_idx` and `check_kind` (`Setup` by
    /// default when `None`), up to `limit` entries.
    pub fn query_endpoints(
        &self,
        corner_idx: usize,
        check_kind: Option<CheckKind>,
        limit: usize,
    ) -> Result<Vec<QueriedEndpoint>, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        let n_corners = v.meta.num_corners as usize;
        if corner_idx >= n_corners {
            return Err(StaStoreError::CornerOutOfRange {
                corner: corner_idx,
                total: n_corners,
            });
        }
        let n_ep = v.endpoints.len();
        let is_hold = usize::from(check_kind.is_some_and(CheckKind::is_hold));
        let order_base = (corner_idx * 2 + is_hold) * n_ep;
        let slack_base = corner_idx * n_ep;
        let count = limit.min(n_ep);
        let mut out = Vec::with_capacity(count);

        for &ep_idx_u32 in &v.ep_order[order_base..order_base + count] {
            let ep_idx = ep_idx_u32 as usize;
            let ep = v
                .endpoints
                .get(ep_idx)
                .ok_or_else(|| StaStoreError::corrupt(path, "EP_ORDER index out of bounds"))?;
            let sl = &v.ep_slack[slack_base + ep_idx];
            let name = self.resolve_endpoint_name(&v, ep)?;
            let capture_clock = self.resolve_clock_name(&v, ep.capture_clock)?;
            let path_group = v
                .strings
                .get(ep.path_group_str)
                .map_err(|r| StaStoreError::corrupt(path, r))?
                .to_owned();
            out.push(QueriedEndpoint {
                endpoint_idx: ep_idx,
                data_node: NodeId(ep.data_node),
                name,
                capture_clock,
                path_group,
                kind: ep.kind(),
                setup_slack: sl.setup_slack,
                hold_slack: sl.hold_slack,
                worst_setup_handle: (ep.worst_setup_rank != NO_INDEX)
                    .then(|| format!("path{}", ep.worst_setup_rank + 1)),
                worst_hold_handle: (ep.worst_hold_rank != NO_INDEX)
                    .then(|| format!("path{}", ep.worst_hold_rank + 1)),
            });
        }
        Ok(out)
    }

    /// Queries ranked path `path<rank_idx>` (`O(1)` lookup in `PATH_RANK`).
    pub fn query_path_handle(&self, rank_idx: usize) -> Result<QueriedPath, StaStoreError> {
        let v = self.view()?;
        if rank_idx >= v.path_rank.len() {
            return Err(StaStoreError::PathRankOutOfRange {
                rank: rank_idx,
                total: v.path_rank.len(),
            });
        }
        if rank_idx < v.paths.len() && v.paths[rank_idx].rank_idx as usize == rank_idx {
            return self.read_materialized_top_k_path(&v, rank_idx);
        }
        self.reconstruct_path_from_chain(rank_idx)
    }

    /// Reconstructs ranked path `rank_idx` directly from `PATH_CHAIN` + `NODE_REC` (+ `TAG_EXTRA`),
    /// batch-prefetching `NODE_REC` entries along the chain before assembling point records
    /// (`STA-REQ-0314`).
    pub fn reconstruct_path_from_chain(
        &self,
        rank_idx: usize,
    ) -> Result<QueriedPath, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        if rank_idx >= v.path_rank.len() {
            return Err(StaStoreError::PathRankOutOfRange {
                rank: rank_idx,
                total: v.path_rank.len(),
            });
        }

        let pr = &v.path_rank[rank_idx];
        let corner_idx = pr.corner_index();
        let ep_idx = pr.endpoint_index();
        let an = pr.bound();
        let is_hold = pr.is_hold();

        let corner_rec = v
            .corners
            .get(corner_idx)
            .ok_or_else(|| StaStoreError::corrupt(path, "PATH_RANK corner_idx out of bounds"))?;
        let corner_name = v
            .strings
            .get(corner_rec.name_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();
        let ep = v
            .endpoints
            .get(ep_idx)
            .ok_or_else(|| StaStoreError::corrupt(path, "PATH_RANK endpoint_idx out of bounds"))?;
        let endpoint_name = self.resolve_endpoint_name(&v, ep)?;
        let capture_clock = self.resolve_clock_name(&v, ep.capture_clock)?;
        let path_group = v
            .strings
            .get(ep.path_group_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();

        let c_start = v.path_chain_ptr[rank_idx] as usize;
        let c_end = v.path_chain_ptr[rank_idx + 1] as usize;
        if c_start + PATH_CHAIN_HEADER_WORDS > c_end || c_end > v.path_chain.len() {
            return Err(StaStoreError::corrupt(
                path,
                "PATH_CHAIN slice shorter than header or out of bounds",
            ));
        }
        let chain_words = &v.path_chain[c_start..c_end];
        let required = unpack_f64_words(chain_words[0], chain_words[1]);
        let check_value = unpack_f64_words(chain_words[2], chain_words[3]);
        let step_words = &chain_words[PATH_CHAIN_HEADER_WORDS..];
        if !step_words.len().is_multiple_of(PATH_CHAIN_STEP_WORDS) {
            return Err(StaStoreError::corrupt(
                path,
                "PATH_CHAIN step words count is not a multiple of 3",
            ));
        }
        let n_steps = step_words.len() / PATH_CHAIN_STEP_WORDS;

        let mode = StoreMode::from_u32(v.meta.store_mode).unwrap_or(StoreMode::Full);
        if mode == StoreMode::Summary {
            if rank_idx < v.paths.len() && v.paths[rank_idx].rank_idx as usize == rank_idx {
                return self.read_materialized_top_k_path(&v, rank_idx);
            }
            let mut points = Vec::with_capacity(n_steps);
            for k in 0..n_steps {
                let (node, tr, kind, _, name_bucket) = unpack_chain_step(chain_step(step_words, k));
                let name = v
                    .name_hash
                    .format_bucket(name_bucket, &v.strings)
                    .map_err(|r| StaStoreError::corrupt(path, r))?;
                points.push(QueriedPathPoint {
                    node,
                    name,
                    tr,
                    kind,
                    arrival: 0.0,
                    incr_delay: 0.0,
                    slew: 0.0,
                });
            }
            let slack = f64::from(pr.slack_ns);
            return Ok(QueriedPath {
                handle: format!("path{}", rank_idx + 1),
                rank_idx,
                corner_idx,
                corner_name,
                endpoint_idx: ep_idx,
                endpoint_name,
                check_kind: check_kind_str(pr.check_kind()).to_owned(),
                path_group,
                launch_clock: capture_clock.clone(),
                capture_clock,
                slack,
                arrival: if is_hold {
                    required + slack
                } else {
                    required - slack
                },
                required,
                check_value,
                points,
            });
        }

        let n_nodes = v.meta.num_nodes as usize;
        let corner_base = corner_idx * n_nodes;
        let mut node_indices = Vec::with_capacity(n_steps);
        for k in 0..n_steps {
            let (node, _, _, _, _) = unpack_chain_step(chain_step(step_words, k));
            let idx = node.index();
            if idx >= n_nodes {
                return Err(StaStoreError::corrupt(
                    path,
                    "PATH_CHAIN node index out of bounds",
                ));
            }
            node_indices.push(corner_base + idx);
        }

        // Batch-touch NODE_REC entries to prefetch cache lines before assembling points.
        let mut _prefetch_acc = 0u32;
        for &nr_idx in &node_indices {
            _prefetch_acc ^= v.node_rec[nr_idx].tag;
        }

        let mut points = Vec::with_capacity(n_steps);
        let mut prev_arrival = 0.0f64;
        let mut launch_clock_id = ep.capture_clock;

        for (k, &nr_idx) in node_indices.iter().enumerate() {
            let (node, tr, kind, tag_slot, name_bucket) =
                unpack_chain_step(chain_step(step_words, k));
            let nr = &v.node_rec[nr_idx];
            let tr_i = tr.index();
            let an_i = an.index();
            let slew = nr.slew[tr_i][an_i];

            let (arrival, tag_id) = if tag_slot == 0 {
                (nr.arrival[tr_i][an_i], nr.tag)
            } else {
                let extra_begin = v.tag_extra_ptr[nr_idx] as usize;
                let extra_end = v.tag_extra_ptr[nr_idx + 1] as usize;
                let extra_idx = extra_begin + (tag_slot - 1);
                if extra_idx >= extra_end || extra_idx >= v.tag_extra.len() {
                    return Err(StaStoreError::corrupt(
                        path,
                        "PATH_CHAIN tag_slot out of bounds in TAG_EXTRA",
                    ));
                }
                let te = &v.tag_extra[extra_idx];
                (te.arrival[tr_i][an_i], te.tag)
            };

            if k == 0
                && let Some(tag_rec) = v.tags.get(tag_id as usize)
            {
                launch_clock_id = tag_rec.clock_id;
            }

            let incr_delay = if k == 0 {
                arrival
            } else {
                arrival - prev_arrival
            };
            prev_arrival = arrival;

            let name = v
                .name_hash
                .format_bucket(name_bucket, &v.strings)
                .map_err(|r| StaStoreError::corrupt(path, r))?;

            points.push(QueriedPathPoint {
                node,
                name,
                tr,
                kind,
                arrival,
                incr_delay,
                slew,
            });
        }

        let final_arrival = points.last().map_or(0.0, |p| p.arrival);
        let slack = if is_hold {
            final_arrival - required
        } else {
            required - final_arrival
        };
        let launch_clock = self.resolve_clock_name(&v, launch_clock_id)?;

        Ok(QueriedPath {
            handle: format!("path{}", rank_idx + 1),
            rank_idx,
            corner_idx,
            corner_name,
            endpoint_idx: ep_idx,
            endpoint_name,
            check_kind: check_kind_str(pr.check_kind()).to_owned(),
            path_group,
            launch_clock,
            capture_clock,
            slack,
            arrival: final_arrival,
            required,
            check_value,
            points,
        })
    }

    /// Queries the worst path(s) ending at `pin_or_port` (`--to <pin>`): for each check family
    /// that `check_kind` selects (setup and recovery, or hold and removal), the worst path of the
    /// endpoint over all corners, or in corner `corner_idx` when given. A family without a path
    /// in that corner gives none (`STA-REQ-0305`, `STA-REQ-0314`, `STA-REQ-0814`).
    pub fn query_path_to(
        &self,
        pin_or_port: &str,
        corner_idx: Option<usize>,
        check_kind: Option<CheckKind>,
    ) -> Result<Vec<QueriedPath>, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        let Some((node, _)) = v
            .name_hash
            .lookup(pin_or_port, &v.strings)
            .map_err(|r| StaStoreError::corrupt(path, r))?
        else {
            return Err(StaStoreError::UnknownPin {
                name: pin_or_port.to_owned(),
            });
        };

        let ep_range = find_endpoints_for_node(v.endpoints, node);
        if ep_range.is_empty() {
            return Err(StaStoreError::NotAnEndpoint {
                name: pin_or_port.to_owned(),
            });
        }

        let mut ranks = Vec::new();
        for ep_idx in ep_range {
            let ep = &v.endpoints[ep_idx];
            for (hold, worst) in [(false, ep.worst_setup_rank), (true, ep.worst_hold_rank)] {
                let wanted = check_kind.is_none_or(|k| {
                    if hold {
                        CheckKind::is_hold(k)
                    } else {
                        CheckKind::is_setup(k)
                    }
                });
                if !wanted || worst == NO_INDEX || worst as usize >= v.path_rank.len() {
                    continue;
                }
                let worst = worst as usize;
                match corner_idx {
                    None => ranks.push(worst),
                    // The paths of a family are ranked by slack, the setup family first, so the
                    // first path of the endpoint in corner `c` from its worst rank on is its
                    // worst path in that corner (`STA-REQ-0814`).
                    Some(c) => ranks.extend(
                        (worst..v.path_rank.len())
                            .take_while(|&r| v.path_rank[r].is_hold() == hold)
                            .find(|&r| {
                                let pr = &v.path_rank[r];
                                pr.endpoint_index() == ep_idx && pr.corner_index() == c
                            }),
                    ),
                }
            }
        }

        let mut out = Vec::with_capacity(ranks.len());
        for r in ranks {
            out.push(self.query_path_handle(r)?);
        }
        Ok(out)
    }

    /// Inspects a pin by name (`"u0/D"`) in `O(1)` via `NAME_HASH` (`STA-REQ-0315`).
    pub fn inspect_pin(
        &self,
        pin_name: &str,
        corner_idx: usize,
    ) -> Result<InspectedNode, StaStoreError> {
        self.inspect_named_node(pin_name, corner_idx, false)
    }

    /// Inspects a top-level port by name (`"clk"`, `"in[0]"`) in `O(1)` via `NAME_HASH`.
    pub fn inspect_port(
        &self,
        port_name: &str,
        corner_idx: usize,
    ) -> Result<InspectedNode, StaStoreError> {
        self.inspect_named_node(port_name, corner_idx, true)
    }

    fn inspect_named_node(
        &self,
        name: &str,
        corner_idx: usize,
        expect_port: bool,
    ) -> Result<InspectedNode, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        let n_corners = v.meta.num_corners as usize;
        if corner_idx >= n_corners {
            return Err(StaStoreError::CornerOutOfRange {
                corner: corner_idx,
                total: n_corners,
            });
        }
        let Some((node, _)) = v
            .name_hash
            .lookup(name, &v.strings)
            .map_err(|r| StaStoreError::corrupt(path, r))?
        else {
            return Err(if expect_port {
                StaStoreError::UnknownPort {
                    name: name.to_owned(),
                }
            } else {
                StaStoreError::UnknownPin {
                    name: name.to_owned(),
                }
            });
        };

        let is_port = node.0 < v.meta.num_port_bits;
        if expect_port && !is_port {
            return Err(StaStoreError::UnknownPort {
                name: name.to_owned(),
            });
        }

        let corner_name = v
            .strings
            .get(v.corners[corner_idx].name_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();

        let ep_range = find_endpoints_for_node(v.endpoints, node);
        let is_endpoint = !ep_range.is_empty();
        let mut ep_setup_slack: Option<f64> = None;
        let mut ep_hold_slack: Option<f64> = None;
        if is_endpoint {
            let slack_base = corner_idx * v.endpoints.len();
            for ep_idx in ep_range {
                let sl = &v.ep_slack[slack_base + ep_idx];
                if !sl.setup_slack.is_nan() {
                    ep_setup_slack =
                        Some(ep_setup_slack.map_or(sl.setup_slack, |x| x.min(sl.setup_slack)));
                }
                if !sl.hold_slack.is_nan() {
                    ep_hold_slack =
                        Some(ep_hold_slack.map_or(sl.hold_slack, |x| x.min(sl.hold_slack)));
                }
            }
        }

        let mode = StoreMode::from_u32(v.meta.store_mode).unwrap_or(StoreMode::Full);
        if mode == StoreMode::Summary {
            if !is_endpoint {
                return Err(StaStoreError::SummaryModeUnavailable {
                    operation: "inspect non-endpoint pin or port",
                });
            }
            return Ok(InspectedNode {
                node,
                name: name.to_owned(),
                is_port,
                is_endpoint: true,
                corner_idx,
                corner_name,
                slew: [[0.0; 2]; 2],
                required: ABSENT_REQUIRED,
                slack: ABSENT_SLACK,
                endpoint_setup_slack: ep_setup_slack,
                endpoint_hold_slack: ep_hold_slack,
                tags: Vec::new(),
            });
        }

        let nr_idx = corner_idx * (v.meta.num_nodes as usize) + node.index();
        let nr = v
            .node_rec
            .get(nr_idx)
            .ok_or_else(|| StaStoreError::corrupt(path, "NODE_REC index out of bounds"))?;

        let mut tags = Vec::new();
        if nr.tag != NO_INDEX {
            tags.push(self.decode_tag_state(&v, nr.tag, nr.arrival, nr.pred)?);
        }
        let extra_begin = v.tag_extra_ptr[nr_idx] as usize;
        let extra_end = v.tag_extra_ptr[nr_idx + 1] as usize;
        for te in &v.tag_extra[extra_begin..extra_end] {
            tags.push(self.decode_tag_state(&v, te.tag, te.arrival, te.pred)?);
        }

        Ok(InspectedNode {
            node,
            name: name.to_owned(),
            is_port,
            is_endpoint,
            corner_idx,
            corner_name,
            slew: nr.slew,
            required: nr.required,
            slack: nr.slack,
            endpoint_setup_slack: ep_setup_slack,
            endpoint_hold_slack: ep_hold_slack,
            tags,
        })
    }

    /// Inspects an instance by name (`"u0"`) in `O(1)` via `INST_HASH`, reading a single
    /// contiguous slice of `NODE_REC` for all of its pins.
    pub fn inspect_instance(
        &self,
        inst_name: &str,
        corner_idx: usize,
    ) -> Result<InspectedInstance, StaStoreError> {
        let v = self.view()?;
        let path = self.path();
        let n_corners = v.meta.num_corners as usize;
        if corner_idx >= n_corners {
            return Err(StaStoreError::CornerOutOfRange {
                corner: corner_idx,
                total: n_corners,
            });
        }

        let mode = StoreMode::from_u32(v.meta.store_mode).unwrap_or(StoreMode::Full);
        if mode == StoreMode::Summary {
            return Err(StaStoreError::SummaryModeUnavailable {
                operation: "inspect instance pins",
            });
        }

        let Some(entry) = v
            .inst_hash
            .lookup(inst_name, &v.strings)
            .map_err(|r| StaStoreError::corrupt(path, r))?
        else {
            return Err(StaStoreError::UnknownInstance {
                name: inst_name.to_owned(),
            });
        };

        let cell_name = v
            .strings
            .get(entry.cell_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();
        let corner_name = v
            .strings
            .get(v.corners[corner_idx].name_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();

        let n_nodes = v.meta.num_nodes as usize;
        let pin_begin = entry.pin_node_begin as usize;
        let pin_end = pin_begin + entry.pin_count as usize;
        if pin_end > n_nodes {
            return Err(StaStoreError::corrupt(
                path,
                "INST_HASH pin_node range exceeds num_nodes",
            ));
        }

        let corner_base = corner_idx * n_nodes;
        let nr_slice = &v.node_rec[corner_base + pin_begin..corner_base + pin_end];
        let mut pins = Vec::with_capacity(nr_slice.len());

        for (offset, nr) in nr_slice.iter().enumerate() {
            let node = NodeId((pin_begin + offset) as u32);
            let pin_name = if nr.pad != NO_INDEX {
                v.name_hash
                    .bucket_leaf(nr.pad, &v.strings)
                    .map_err(|r| StaStoreError::corrupt(path, r))?
                    .to_owned()
            } else {
                format!("pin{offset}")
            };
            let full_name = format!("{inst_name}/{pin_name}");
            pins.push(InspectedInstancePin {
                node,
                pin_name,
                full_name,
                slew: nr.slew,
                arrival: if nr.tag != NO_INDEX {
                    nr.arrival
                } else {
                    ABSENT_EARLY_LATE
                },
                required: nr.required,
                slack: nr.slack,
            });
        }

        Ok(InspectedInstance {
            inst_name: inst_name.to_owned(),
            cell_name,
            corner_idx,
            corner_name,
            pins,
        })
    }

    fn read_materialized_top_k_path(
        &self,
        v: &StaStoreView<'_>,
        rank_idx: usize,
    ) -> Result<QueriedPath, StaStoreError> {
        let path = self.path();
        let pr = &v.path_rank[rank_idx];
        let ph = &v.paths[rank_idx];
        let corner_idx = pr.corner_index();
        let ep_idx = ph.endpoint_idx as usize;

        let corner_rec = v
            .corners
            .get(corner_idx)
            .ok_or_else(|| StaStoreError::corrupt(path, "PATH_RANK corner_idx out of bounds"))?;
        let corner_name = v
            .strings
            .get(corner_rec.name_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();
        let ep = v
            .endpoints
            .get(ep_idx)
            .ok_or_else(|| StaStoreError::corrupt(path, "PATHS endpoint_idx out of bounds"))?;
        let endpoint_name = self.resolve_endpoint_name(v, ep)?;
        let launch_clock = self.resolve_clock_name(v, ph.launch_clock)?;
        let capture_clock = self.resolve_clock_name(v, ph.capture_clock)?;
        let path_group = v
            .strings
            .get(ep.path_group_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();

        let p_begin = ph.point_begin as usize;
        let p_end = ph.point_end as usize;
        if p_begin > p_end || p_end > v.path_points.len() {
            return Err(StaStoreError::corrupt(
                path,
                "PATHS point_begin..point_end out of bounds",
            ));
        }

        let c_start = v.path_chain_ptr[rank_idx] as usize;
        let c_end = v.path_chain_ptr[rank_idx + 1] as usize;
        let step_words = v
            .path_chain
            .get(c_start + PATH_CHAIN_HEADER_WORDS..c_end)
            .unwrap_or(&[]);

        let mut points = Vec::with_capacity(p_end - p_begin);
        for (k, pp) in v.path_points[p_begin..p_end].iter().enumerate() {
            let node = NodeId(pp.node);
            let tr = if pp.tr == 0 { Tr::Rise } else { Tr::Fall };
            let kind = PathPointKind::from_u8(pp.kind);
            let name = if let Some(&nb) = step_words.get(k * PATH_CHAIN_STEP_WORDS + 2) {
                v.name_hash
                    .format_bucket(nb, &v.strings)
                    .map_err(|r| StaStoreError::corrupt(path, r))?
            } else {
                self.resolve_node_name_fallback(v, node)?
            };
            points.push(QueriedPathPoint {
                node,
                name,
                tr,
                kind,
                arrival: pp.arrival,
                incr_delay: pp.incr_delay,
                slew: pp.slew,
            });
        }

        Ok(QueriedPath {
            handle: format!("path{}", rank_idx + 1),
            rank_idx,
            corner_idx,
            corner_name,
            endpoint_idx: ep_idx,
            endpoint_name,
            check_kind: check_kind_str(pr.check_kind()).to_owned(),
            path_group,
            launch_clock,
            capture_clock,
            slack: ph.slack,
            arrival: ph.arrival,
            required: ph.required,
            check_value: ph.check_value,
            points,
        })
    }

    fn decode_tag_state(
        &self,
        v: &StaStoreView<'_>,
        tag_idx: u32,
        arrival: [[f64; 2]; 2],
        pred: [[u32; 2]; 2],
    ) -> Result<InspectedTagState, StaStoreError> {
        let path = self.path();
        let tag_rec = v
            .tags
            .get(tag_idx as usize)
            .ok_or_else(|| StaStoreError::corrupt(path, "tag index out of bounds in TAGS"))?;
        let clock_name = self.resolve_clock_name(v, tag_rec.clock_id)?;
        let launch_edge = match tag_rec.launch_edge {
            0 => Some(Tr::Rise),
            1 => Some(Tr::Fall),
            _ => None,
        };
        let path_group = v
            .strings
            .get(tag_rec.path_group_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?
            .to_owned();
        Ok(InspectedTagState {
            tag_idx,
            clock_name,
            launch_edge,
            path_group,
            arrival,
            pred,
        })
    }

    fn resolve_clock_name(
        &self,
        v: &StaStoreView<'_>,
        clock_id: u32,
    ) -> Result<String, StaStoreError> {
        if clock_id == NO_INDEX {
            return Ok(String::from("<unclocked>"));
        }
        let path = self.path();
        let clk = v
            .clocks
            .get(clock_id as usize)
            .ok_or_else(|| StaStoreError::corrupt(path, "clock_id out of bounds in CLOCKS"))?;
        v.strings
            .get(clk.name_str)
            .map(str::to_owned)
            .map_err(|r| StaStoreError::corrupt(path, r))
    }

    fn resolve_endpoint_name(
        &self,
        v: &StaStoreView<'_>,
        ep: &EndpointRec,
    ) -> Result<String, StaStoreError> {
        if let Some(nb) = ep.name_bucket() {
            return v
                .name_hash
                .format_bucket(nb, &v.strings)
                .map_err(|r| StaStoreError::corrupt(self.path(), r));
        }
        self.resolve_node_name_fallback(v, NodeId(ep.data_node))
    }

    fn resolve_node_name_fallback(
        &self,
        v: &StaStoreView<'_>,
        node: NodeId,
    ) -> Result<String, StaStoreError> {
        let path = self.path();
        if let Some(nr) = v.node_rec.get(node.index())
            && nr.pad != NO_INDEX
        {
            return v
                .name_hash
                .format_bucket(nr.pad, &v.strings)
                .map_err(|r| StaStoreError::corrupt(path, r));
        }
        if let Some(nb) = v.name_hash.find_node_bucket(node) {
            return v
                .name_hash
                .format_bucket(nb, &v.strings)
                .map_err(|r| StaStoreError::corrupt(path, r));
        }
        Ok(format!("node{}", node.0))
    }
}

fn check_kind_str(k: CheckKind) -> &'static str {
    match k {
        CheckKind::Setup => "setup",
        CheckKind::Hold => "hold",
        CheckKind::Recovery => "recovery",
        CheckKind::Removal => "removal",
    }
}

fn find_endpoints_for_node(endpoints: &[EndpointRec], node: NodeId) -> std::ops::Range<usize> {
    let target = node.0;
    let start = endpoints.partition_point(|ep| ep.data_node < target);
    let end = endpoints.partition_point(|ep| ep.data_node <= target);
    start..end
}

/// Step `k` of the step words of a `PATH_CHAIN` slice; the caller checked that the slice holds a
/// whole number of steps and that `k` is below the step count.
fn chain_step(step_words: &[u32], k: usize) -> &[u32; PATH_CHAIN_STEP_WORDS] {
    let w = &step_words[k * PATH_CHAIN_STEP_WORDS..(k + 1) * PATH_CHAIN_STEP_WORDS];
    w.try_into().expect("a PATH_CHAIN step has three words")
}
