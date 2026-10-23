// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Zero-copy `STA v2` (`sta.rbin`) reader, structure-only open validator, and content-hash
//! verifier (`owner: E10`; `WS-REQ-0030` to `WS-REQ-0038`, `STA-REQ-0302`).
//!
//! Opening a timing table (`StaStore::open_rev`, `StaStore::read_path`, `StaStore::from_rbin`)
//! validates the 64-byte `rbin` header, the 24-entry section directory, and the `O(1)` section
//! count invariants against [`StaMeta`] without hashing `[header_len..EOF)`. Full content-hash
//! verification (`xxh3-64`) runs on [`StaStore::verify_hash`], [`StaStore::open_verified_path`],
//! and [`StaStore::open_for_mutation_input`].

use std::path::{Path, PathBuf};

use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::{Corruption, Error as WsError, ErrorCode, Revision, Workspace};

use crate::engine::store::hash::{InstHashView, NameHashView, StringTableView};
use crate::engine::store::layout::{
    ClockRec, CornerRec, DisabledArcRec, EndpointRec, EpSlackRec, KIND, NodeRec, NoteRec,
    PathHeaderRec, PathPointRec, PathRankRec, StaMeta, StoreMode, StorePrecision, SummaryRec,
    TABLE_NAME, TagExtraRec, TagRec, UnconstrainedRec, VERSIONS, section,
};
use crate::engine::store::writer::StaStoreData;

/// Errors returned when opening, verifying, or querying `sta.rbin`.
#[derive(Debug, thiserror::Error)]
pub enum StaStoreError {
    /// Underlying workspace / `rbin` error (`CORRUPT_TABLE`, `SCHEMA_TOO_NEW`, `IO`, etc.).
    #[error(transparent)]
    Workspace(#[from] WsError),

    /// Structural or semantic corruption in `sta.rbin` (`CORRUPT_TABLE`).
    #[error("corrupt STA table at {}: {reason}", path.display())]
    CorruptTable { path: PathBuf, reason: String },

    /// An operation requiring `NODE_REC` / `TAG_EXTRA` was requested on a `Summary`-mode table.
    #[error(
        "cannot {operation}: sta.rbin was written in Summary mode (store_mode = summary), which omits per-node NODE_REC and TAG_EXTRA sections; re-run with --store-mode full"
    )]
    SummaryModeUnavailable { operation: &'static str },

    /// Named pin was not found in `NAME_HASH`.
    #[error("unknown pin '{name}'")]
    UnknownPin { name: String },

    /// Named port was not found in `NAME_HASH`.
    #[error("unknown port '{name}'")]
    UnknownPort { name: String },

    /// Named instance was not found in `INST_HASH`.
    #[error("unknown instance '{name}'")]
    UnknownInstance { name: String },

    /// Named pin or port is not a checked timing endpoint.
    #[error("pin or port '{name}' is not a checked timing endpoint")]
    NotAnEndpoint { name: String },

    /// Ranked path handle `path<N>` is out of range; `rank` is the 0-based row (`N - 1`).
    #[error("path handle path{} is out of range (table has {total} ranked paths)", .rank + 1)]
    PathRankOutOfRange { rank: usize, total: usize },

    /// Corner index is out of range.
    #[error("corner index {corner} is out of range (table has {total} corners)")]
    CornerOutOfRange { corner: usize, total: usize },
}

impl StaStoreError {
    /// Maps this error to its canonical workspace [`ErrorCode`].
    #[must_use]
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::Workspace(e) => e.code(),
            Self::CorruptTable { .. } => ErrorCode::CorruptTable,
            Self::SummaryModeUnavailable { .. } | Self::NotAnEndpoint { .. } => {
                ErrorCode::InputRejected
            }
            Self::UnknownPin { .. }
            | Self::UnknownPort { .. }
            | Self::UnknownInstance { .. }
            | Self::PathRankOutOfRange { .. }
            | Self::CornerOutOfRange { .. } => ErrorCode::UnknownHandle,
        }
    }

    pub(crate) fn corrupt(path: &Path, reason: impl Into<String>) -> Self {
        Self::CorruptTable {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }
}

/// Zero-copy borrowed slices of all 24 sections of an open `sta.rbin`.
#[derive(Clone, Copy, Debug)]
pub struct StaStoreView<'a> {
    pub str_bytes: &'a [u8],
    pub str_offs: &'a [u32],
    pub strings: StringTableView<'a>,
    pub meta: &'a StaMeta,
    pub corners: &'a [CornerRec],
    pub clocks: &'a [ClockRec],
    pub tags: &'a [TagRec],
    pub disabled_arcs: &'a [DisabledArcRec],
    pub node_rec: &'a [NodeRec],
    pub tag_extra_ptr: &'a [u32],
    pub tag_extra: &'a [TagExtraRec],
    pub endpoints: &'a [EndpointRec],
    pub ep_slack: &'a [EpSlackRec],
    pub ep_order: &'a [u32],
    pub path_rank: &'a [PathRankRec],
    pub path_chain_ptr: &'a [u64],
    pub path_chain: &'a [u32],
    pub paths: &'a [PathHeaderRec],
    pub path_points: &'a [PathPointRec],
    pub name_hash_words: &'a [u32],
    pub name_hash: NameHashView<'a>,
    pub inst_hash_words: &'a [u32],
    pub inst_hash: InstHashView<'a>,
    pub summary: &'a [SummaryRec],
    pub unconstrained: &'a [UnconstrainedRec],
    pub notes: &'a [NoteRec],
    pub scope: &'a [u8],
}

/// Opened `sta.rbin` table backed by a read-only memory map (via `Revision::open_rbin`) or an
/// 8-byte aligned buffer (`Rbin::read` / `Rbin::from_bytes`).
#[derive(Debug)]
pub struct StaStore {
    backing: Backing,
}

/// What a [`StaStore`] reads its sections from.
#[derive(Debug)]
enum Backing {
    /// A `sta.rbin` container: mapped, read, or bytes in memory.
    Rbin(Rbin),
    /// The in-memory table of an analysis before it is written; `path` names it in errors.
    Data {
        data: Box<StaStoreData>,
        path: PathBuf,
    },
}

/// The raw sections of a timing table, before their contents are validated.
struct SectionParts<'a> {
    str_bytes: &'a [u8],
    str_offs: &'a [u32],
    metas: &'a [StaMeta],
    corners: &'a [CornerRec],
    clocks: &'a [ClockRec],
    tags: &'a [TagRec],
    disabled_arcs: &'a [DisabledArcRec],
    node_rec: &'a [NodeRec],
    tag_extra_ptr: &'a [u32],
    tag_extra: &'a [TagExtraRec],
    endpoints: &'a [EndpointRec],
    ep_slack: &'a [EpSlackRec],
    ep_order: &'a [u32],
    path_rank: &'a [PathRankRec],
    path_chain_ptr: &'a [u64],
    path_chain: &'a [u32],
    paths: &'a [PathHeaderRec],
    path_points: &'a [PathPointRec],
    name_hash_words: &'a [u32],
    inst_hash_words: &'a [u32],
    summary: &'a [SummaryRec],
    unconstrained: &'a [UnconstrainedRec],
    notes: &'a [NoteRec],
    scope: &'a [u8],
}

impl<'a> SectionParts<'a> {
    fn of_data(d: &'a StaStoreData) -> Self {
        Self {
            str_bytes: &d.str_bytes,
            str_offs: &d.str_offs,
            metas: &d.meta,
            corners: &d.corners,
            clocks: &d.clocks,
            tags: &d.tags,
            disabled_arcs: &d.disabled_arcs,
            node_rec: &d.node_rec,
            tag_extra_ptr: &d.tag_extra_ptr,
            tag_extra: &d.tag_extra,
            endpoints: &d.endpoints,
            ep_slack: &d.ep_slack,
            ep_order: &d.ep_order,
            path_rank: &d.path_rank,
            path_chain_ptr: &d.path_chain_ptr,
            path_chain: &d.path_chain,
            paths: &d.paths,
            path_points: &d.path_points,
            name_hash_words: &d.name_hash,
            inst_hash_words: &d.inst_hash,
            summary: &d.summary,
            unconstrained: &d.unconstrained,
            notes: &d.notes,
            scope: &d.scope,
        }
    }

    fn of_rbin(rbin: &'a Rbin) -> Result<Self, StaStoreError> {
        Ok(Self {
            str_bytes: rbin.section::<u8>(section::STR_BYTES)?,
            str_offs: rbin.section::<u32>(section::STR_OFFS)?,
            metas: rbin.section::<StaMeta>(section::META)?,
            corners: rbin.section::<CornerRec>(section::CORNERS)?,
            clocks: rbin.section::<ClockRec>(section::CLOCKS)?,
            tags: rbin.section::<TagRec>(section::TAGS)?,
            disabled_arcs: rbin.section::<DisabledArcRec>(section::DISABLED_ARCS)?,
            node_rec: rbin.section::<NodeRec>(section::NODE_REC)?,
            tag_extra_ptr: rbin.section::<u32>(section::TAG_EXTRA_PTR)?,
            tag_extra: rbin.section::<TagExtraRec>(section::TAG_EXTRA)?,
            endpoints: rbin.section::<EndpointRec>(section::ENDPOINTS)?,
            ep_slack: rbin.section::<EpSlackRec>(section::EP_SLACK)?,
            ep_order: rbin.section::<u32>(section::EP_ORDER)?,
            path_rank: rbin.section::<PathRankRec>(section::PATH_RANK)?,
            path_chain_ptr: rbin.section::<u64>(section::PATH_CHAIN_PTR)?,
            path_chain: rbin.section::<u32>(section::PATH_CHAIN)?,
            paths: rbin.section::<PathHeaderRec>(section::PATHS)?,
            path_points: rbin.section::<PathPointRec>(section::PATH_POINTS)?,
            name_hash_words: rbin.section::<u32>(section::NAME_HASH)?,
            inst_hash_words: rbin.section::<u32>(section::INST_HASH)?,
            summary: rbin.section::<SummaryRec>(section::SUMMARY)?,
            unconstrained: rbin.section::<UnconstrainedRec>(section::UNCONSTRAINED)?,
            notes: rbin.section::<NoteRec>(section::NOTES)?,
            scope: rbin.section::<u8>(section::SCOPE)?,
        })
    }
}

impl StaStore {
    /// Opens a `sta.rbin` from an already-opened [`Rbin`], performing **structure-only** `O(1)`
    /// validation without hashing `[header_len..EOF)` (`WS-REQ-0036`, Decision 6).
    pub fn from_rbin(rbin: Rbin) -> Result<Self, StaStoreError> {
        let store = Self {
            backing: Backing::Rbin(rbin),
        };
        let _ = store.view()?;
        Ok(store)
    }

    /// Wraps the in-memory table `data` of an analysis, validated like a read table, so that
    /// a command answers its queries before (or without) writing `sta.rbin`: no bytes are
    /// encoded or copied (`STA-REQ-0302`).
    pub fn from_data(data: StaStoreData) -> Result<Self, StaStoreError> {
        let store = Self {
            backing: Backing::Data {
                data: Box::new(data),
                path: PathBuf::from(format!("<in-memory {TABLE_NAME} table>")),
            },
        };
        let _ = store.view()?;
        Ok(store)
    }

    /// The in-memory table of a store made by [`Self::from_data`], given back without a copy.
    #[must_use]
    pub fn into_data(self) -> Option<StaStoreData> {
        match self.backing {
            Backing::Data { data, .. } => Some(*data),
            Backing::Rbin(_) => None,
        }
    }

    /// The in-memory table of a store made by [`Self::from_data`].
    #[must_use]
    pub fn data(&self) -> Option<&StaStoreData> {
        match &self.backing {
            Backing::Data { data, .. } => Some(data),
            Backing::Rbin(_) => None,
        }
    }

    /// The file of the table, or a placeholder naming an in-memory table, for messages.
    #[must_use]
    pub fn path(&self) -> &Path {
        match &self.backing {
            Backing::Rbin(rbin) => rbin.path(),
            Backing::Data { path, .. } => path,
        }
    }

    /// Reads `path` into an 8-byte aligned [`Rbin`] and validates structure only (`O(1)`).
    pub fn read_path(path: &Path) -> Result<Self, StaStoreError> {
        let rbin = Rbin::read(path, KIND, VERSIONS)?;
        Self::from_rbin(rbin)
    }

    /// Reads `path`, validates structure, and verifies the `xxh3-64` content hash of
    /// `[header_len..EOF)` (used for external imports and explicit verification).
    pub fn open_verified_path(path: &Path) -> Result<Self, StaStoreError> {
        let store = Self::read_path(path)?;
        store.verify_hash()?;
        Ok(store)
    }

    /// Opens in-memory `.rbin` bytes and validates structure only (`O(1)`).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, StaStoreError> {
        let rbin = Rbin::from_bytes(bytes, KIND, VERSIONS)?;
        Self::from_rbin(rbin)
    }

    /// Maps the committed `"sta"` (`sta.rbin`) table of workspace revision `rev` read-only via
    /// [`Revision::open_rbin`] and validates structure only (`O(1)`, `WS-REQ-0037`).
    pub fn open_rev(rev: &Revision) -> Result<Self, StaStoreError> {
        let rbin = rev.open_rbin(TABLE_NAME, KIND, VERSIONS)?;
        Self::from_rbin(rbin)
    }

    /// Maps the committed `"sta"` (`sta.rbin`) table of `rev` and verifies its `xxh3-64` content
    /// hash before using it as a mutation input (Decision 6).
    pub fn open_for_mutation_input(rev: &Revision) -> Result<Self, StaStoreError> {
        let store = Self::open_rev(rev)?;
        store.verify_hash()?;
        Ok(store)
    }

    /// Maps the committed `"sta"` (`sta.rbin`) table of the workspace's `HEAD` revision.
    pub fn open_head(ws: &Workspace) -> Result<(Revision, Self), StaStoreError> {
        let rev = ws.revision(ws.head()?)?;
        let store = Self::open_rev(&rev)?;
        Ok((rev, store))
    }

    /// Recomputes and verifies the `xxh3-64` content hash of `[header_len..EOF)` (`WS-REQ-0035`,
    /// `WS-REQ-0036`). Returns `CORRUPT_TABLE` on any byte modification.
    /// An in-memory table has no stored hash and verifies trivially.
    pub fn verify_hash(&self) -> Result<(), StaStoreError> {
        if let Backing::Rbin(rbin) = &self.backing {
            rbin.verify_hash()?;
        }
        Ok(())
    }

    /// Underlying [`Rbin`] container, `None` for an in-memory table.
    #[inline]
    #[must_use]
    pub const fn rbin(&self) -> Option<&Rbin> {
        match &self.backing {
            Backing::Rbin(rbin) => Some(rbin),
            Backing::Data { .. } => None,
        }
    }

    /// Storage mode (`Full` or `Summary`) of this table.
    pub fn store_mode(&self) -> Result<StoreMode, StaStoreError> {
        let v = self.view()?;
        StoreMode::from_u32(v.meta.store_mode)
            .ok_or_else(|| StaStoreError::corrupt(self.path(), "invalid store_mode"))
    }

    /// Verifies that the table was written in [`StoreMode::Full`] before restarting an
    /// incremental timing session from disk.
    pub fn require_full_mode_for_incremental(&self) -> Result<(), StaStoreError> {
        if self.store_mode()? == StoreMode::Summary {
            return Err(StaStoreError::SummaryModeUnavailable {
                operation: "restart incremental timing session from disk",
            });
        }
        Ok(())
    }

    /// Validates all 24 section headers and `O(1)` count invariants and returns zero-copy typed
    /// slices over the table.
    pub fn view(&self) -> Result<StaStoreView<'_>, StaStoreError> {
        match &self.backing {
            Backing::Rbin(rbin) => {
                Self::check_container(rbin)?;
                view_of_parts(rbin.path(), SectionParts::of_rbin(rbin)?)
            }
            Backing::Data { data, path } => view_of_parts(path, SectionParts::of_data(data)),
        }
    }

    /// The container checks of a `sta.rbin`: kind, version, the 24 sections, element sizes
    /// and file length.
    fn check_container(rbin: &Rbin) -> Result<(), StaStoreError> {
        let path = rbin.path();

        if rbin.kind() != KIND {
            return Err(StaStoreError::Workspace(WsError::corrupt(
                path,
                Corruption::WrongKind {
                    expected: KIND.to_string(),
                    found: rbin.kind().to_string(),
                },
            )));
        }
        let fv = rbin.format_version();
        if fv > *VERSIONS.end() {
            return Err(StaStoreError::Workspace(WsError::FormatTooNew {
                path: path.to_path_buf(),
                kind: KIND.to_string(),
                found: fv,
                min: *VERSIONS.start(),
                max: *VERSIONS.end(),
            }));
        }
        if !VERSIONS.contains(&fv) {
            return Err(StaStoreError::Workspace(WsError::corrupt(
                path,
                Corruption::UnsupportedVersion {
                    found: fv,
                    min: *VERSIONS.start(),
                    max: *VERSIONS.end(),
                },
            )));
        }
        if rbin.sections().len() != section::COUNT {
            return Err(StaStoreError::corrupt(
                path,
                format!(
                    "expected {} sections (1..={}), found {}",
                    section::COUNT,
                    section::COUNT,
                    rbin.sections().len()
                ),
            ));
        }

        for (idx, sec) in rbin.sections().iter().enumerate() {
            let expected_id = (idx + 1) as u32;
            if sec.id != expected_id {
                return Err(StaStoreError::corrupt(
                    path,
                    format!("expected section ID {expected_id}, found {}", sec.id),
                ));
            }
            let expected_elem = section::elem_size(expected_id).unwrap_or(0);
            if sec.elem_size != expected_elem {
                return Err(StaStoreError::corrupt(
                    path,
                    format!(
                        "section {} ({}) has elem_size {}, expected {expected_elem}",
                        expected_id,
                        section::name(expected_id),
                        sec.elem_size
                    ),
                ));
            }
        }
        if let Some(last) = rbin.sections().last() {
            let expected_len = (last.offset + last.len + 7) & !7;
            if rbin.file_len() != expected_len {
                return Err(StaStoreError::corrupt(
                    path,
                    format!(
                        "file length {} does not match 8-byte aligned end of sections ({expected_len})",
                        rbin.file_len()
                    ),
                ));
            }
        }

        Ok(())
    }
}

/// The content checks of a timing table's sections and its zero-copy view (`STA-REQ-0302`).
fn view_of_parts<'a>(path: &Path, p: SectionParts<'a>) -> Result<StaStoreView<'a>, StaStoreError> {
    {
        let SectionParts {
            str_bytes,
            str_offs,
            metas,
            corners,
            clocks,
            tags,
            disabled_arcs,
            node_rec,
            tag_extra_ptr,
            tag_extra,
            endpoints,
            ep_slack,
            ep_order,
            path_rank,
            path_chain_ptr,
            path_chain,
            paths,
            path_points,
            name_hash_words,
            inst_hash_words,
            summary,
            unconstrained,
            notes,
            scope,
        } = p;
        let strings = StringTableView::new(str_bytes, str_offs)
            .map_err(|r| StaStoreError::corrupt(path, r))?;

        if metas.len() != 1 {
            return Err(StaStoreError::corrupt(
                path,
                format!("META section must have 1 element, found {}", metas.len()),
            ));
        }
        let meta = &metas[0];
        let mode = StoreMode::from_u32(meta.store_mode).ok_or_else(|| {
            StaStoreError::corrupt(path, format!("invalid store_mode {}", meta.store_mode))
        })?;
        if StorePrecision::from_u32(meta.precision).is_none() {
            return Err(StaStoreError::corrupt(
                path,
                format!("invalid precision {}", meta.precision),
            ));
        }
        if meta.num_port_bits > meta.num_nodes {
            return Err(StaStoreError::corrupt(
                path,
                format!(
                    "num_port_bits ({}) exceeds num_nodes ({})",
                    meta.num_port_bits, meta.num_nodes
                ),
            ));
        }
        let _ = strings
            .get(meta.backend_str)
            .map_err(|r| StaStoreError::corrupt(path, r))?;

        let n_corners = meta.num_corners as usize;
        let n_nodes = meta.num_nodes as usize;
        let n_endpoints = meta.num_endpoints as usize;
        let n_ranked = meta.num_ranked_paths as usize;
        let n_top_k = meta.num_top_k_paths as usize;
        let n_groups = meta.num_path_groups as usize;

        if corners.len() != n_corners
            || clocks.len() != meta.num_clocks as usize
            || tags.len() != meta.num_tags as usize
            || disabled_arcs.len() != meta.num_disabled_arcs as usize
        {
            return Err(StaStoreError::corrupt(
                path,
                "CORNERS, CLOCKS, TAGS, or DISABLED_ARCS count disagrees with META",
            ));
        }

        match mode {
            StoreMode::Full => {
                let total_cn = n_corners.checked_mul(n_nodes).ok_or_else(|| {
                    StaStoreError::corrupt(path, "num_corners * num_nodes overflow")
                })?;
                if node_rec.len() != total_cn
                    || tag_extra_ptr.len() != total_cn + 1
                    || tag_extra_ptr[0] != 0
                    || tag_extra_ptr[total_cn] as usize != tag_extra.len()
                {
                    return Err(StaStoreError::corrupt(
                        path,
                        "NODE_REC or TAG_EXTRA_PTR count/bounds invalid in Full mode",
                    ));
                }
            }
            StoreMode::Summary => {
                if !node_rec.is_empty() || !tag_extra_ptr.is_empty() || !tag_extra.is_empty() {
                    return Err(StaStoreError::corrupt(
                        path,
                        "Summary mode table must have empty NODE_REC, TAG_EXTRA_PTR, and TAG_EXTRA",
                    ));
                }
            }
        }

        let expected_ep_slack = n_corners
            .checked_mul(n_endpoints)
            .ok_or_else(|| StaStoreError::corrupt(path, "EP_SLACK count overflow"))?;
        let expected_ep_order = expected_ep_slack
            .checked_mul(2)
            .ok_or_else(|| StaStoreError::corrupt(path, "EP_ORDER count overflow"))?;
        if endpoints.len() != n_endpoints
            || ep_slack.len() != expected_ep_slack
            || ep_order.len() != expected_ep_order
        {
            return Err(StaStoreError::corrupt(
                path,
                "ENDPOINTS, EP_SLACK, or EP_ORDER count disagrees with META",
            ));
        }

        if path_rank.len() != n_ranked
            || path_chain_ptr.len() != n_ranked + 1
            || path_chain_ptr[0] != 0
            || path_chain_ptr[n_ranked] as usize != path_chain.len()
            || paths.len() != n_top_k
            || n_top_k > n_ranked
        {
            return Err(StaStoreError::corrupt(
                path,
                "PATH_RANK, PATH_CHAIN_PTR, or PATHS count/bounds invalid",
            ));
        }
        if let Some(last_path) = paths.last()
            && last_path.point_end as usize > path_points.len()
        {
            return Err(StaStoreError::corrupt(
                path,
                "PATHS last point_end exceeds PATH_POINTS length",
            ));
        }

        let name_hash =
            NameHashView::new(name_hash_words).map_err(|r| StaStoreError::corrupt(path, r))?;
        let inst_hash =
            InstHashView::new(inst_hash_words).map_err(|r| StaStoreError::corrupt(path, r))?;

        let expected_summary = n_corners
            .checked_mul(n_groups)
            .ok_or_else(|| StaStoreError::corrupt(path, "SUMMARY count overflow"))?;
        if summary.len() != expected_summary || scope.len() < 2 {
            return Err(StaStoreError::corrupt(
                path,
                "SUMMARY or SCOPE section count/length invalid",
            ));
        }

        Ok(StaStoreView {
            str_bytes,
            str_offs,
            strings,
            meta,
            corners,
            clocks,
            tags,
            disabled_arcs,
            node_rec,
            tag_extra_ptr,
            tag_extra,
            endpoints,
            ep_slack,
            ep_order,
            path_rank,
            path_chain_ptr,
            path_chain,
            paths,
            path_points,
            name_hash_words,
            name_hash,
            inst_hash_words,
            inst_hash,
            summary,
            unconstrained,
            notes,
            scope,
        })
    }
}

impl StaStore {
    /// Copies all 24 borrowed sections back into an owned [`StaStoreData`] for round-trip
    /// verification.
    pub fn to_store_data(&self) -> Result<StaStoreData, StaStoreError> {
        let v = self.view()?;
        Ok(StaStoreData {
            str_bytes: v.str_bytes.to_vec(),
            str_offs: v.str_offs.to_vec(),
            meta: [*v.meta],
            corners: v.corners.to_vec(),
            clocks: v.clocks.to_vec(),
            tags: v.tags.to_vec(),
            disabled_arcs: v.disabled_arcs.to_vec(),
            node_rec: v.node_rec.to_vec(),
            tag_extra_ptr: v.tag_extra_ptr.to_vec(),
            tag_extra: v.tag_extra.to_vec(),
            endpoints: v.endpoints.to_vec(),
            ep_slack: v.ep_slack.to_vec(),
            ep_order: v.ep_order.to_vec(),
            path_rank: v.path_rank.to_vec(),
            path_chain_ptr: v.path_chain_ptr.to_vec(),
            path_chain: v.path_chain.to_vec(),
            paths: v.paths.to_vec(),
            path_points: v.path_points.to_vec(),
            name_hash: v.name_hash_words.to_vec(),
            inst_hash: v.inst_hash_words.to_vec(),
            summary: v.summary.to_vec(),
            unconstrained: v.unconstrained.to_vec(),
            notes: v.notes.to_vec(),
            scope: v.scope.to_vec(),
        })
    }
}
