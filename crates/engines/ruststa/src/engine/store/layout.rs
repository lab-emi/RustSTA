// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Binary record layouts and section constants for `sta.rbin` (`STA v2`, owner: E10;
//! `WS-REQ-0030` to `WS-REQ-0038`, `STA-REQ-0302`, `STA-REQ-0313`, `STA-REQ-0401`).
//!
//! Every fixed-stride section payload in `sta.rbin` uses a `#[repr(C)]` record type implementing
//! [`bytemuck::Pod`] and [`bytemuck::Zeroable`] with alignment `<= 8` so that sections can be cast
//! in place (`&[T]`) from a read-only memory map or 8-byte aligned buffer without copying.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

use crate::engine::graph::{CheckKind, DisableReason};
use crate::engine::types::{An, NodeId, Tr};

/// The `rbin` container kind for timing tables (`"STA"`).
pub const KIND: &str = "STA";

/// The `STA` format version implemented by this build (`2`: 32-bit endpoint and node indices in
/// `PATH_RANK`, `ENDPOINTS` and `PATH_CHAIN`, 64-bit word offsets in `PATH_CHAIN_PTR`,
/// `STA-REQ-0302`).
pub const FORMAT_VERSION: u32 = 2;

/// Supported `STA` format versions (`2..=2`).
pub const VERSIONS: RangeInclusive<u32> = FORMAT_VERSION..=FORMAT_VERSION;

/// Canonical workspace table name for the timing table (`"sta"`).
pub const TABLE_NAME: &str = "sta";

/// Canonical filename inside a workspace revision directory (`"sta.rbin"`).
pub const TABLE_PATH: &str = "sta.rbin";

/// Sentinel for an absent 32-bit index (`u32::MAX`).
pub const NO_INDEX: u32 = u32::MAX;

/// Sentinel for an absent predecessor (`u32::MAX`).
pub const NO_PRED: u32 = u32::MAX;

/// Section IDs (`1..=24`) and element sizes of the `STA v2` (`sta.rbin`) container.
pub mod section {
    /// Concatenated UTF-8 strings (`elem_size = 1`).
    pub const STR_BYTES: u32 = 1;
    /// `u32` byte offsets into `STR_BYTES` (`N + 1` entries, `elem_size = 4`).
    pub const STR_OFFS: u32 = 2;
    /// Fixed 64-byte [`super::StaMeta`] record (`1` entry, `elem_size = 64`).
    pub const META: u32 = 3;
    /// Per-corner [`super::CornerRec`] descriptors (`elem_size = 16`).
    pub const CORNERS: u32 = 4;
    /// Per-clock [`super::ClockRec`] descriptors (`elem_size = 48`).
    pub const CLOCKS: u32 = 5;
    /// Per-tag [`super::TagRec`] descriptors (`elem_size = 16`).
    pub const TAGS: u32 = 6;
    /// Disabled arc records [`super::DisabledArcRec`] (`elem_size = 16`).
    pub const DISABLED_ARCS: u32 = 7;
    /// Per-`(corner, node)` [`super::NodeRec`] records (`elem_size = 152`).
    pub const NODE_REC: u32 = 8;
    /// CSR offsets (`num_corners * num_nodes + 1` `u32` words) into `TAG_EXTRA` (`elem_size = 4`).
    pub const TAG_EXTRA_PTR: u32 = 9;
    /// Overflow per-tag records [`super::TagExtraRec`] (`elem_size = 56`).
    pub const TAG_EXTRA: u32 = 10;
    /// Checked endpoint descriptors [`super::EndpointRec`] (`elem_size = 32`).
    pub const ENDPOINTS: u32 = 11;
    /// Per-`(corner, endpoint)` setup/hold slacks [`super::EpSlackRec`] (`elem_size = 16`).
    pub const EP_SLACK: u32 = 12;
    /// Per-`(corner, check_kind)` endpoint indices sorted by ascending slack (`elem_size = 4`).
    pub const EP_ORDER: u32 = 13;
    /// Ranked path handles [`super::PathRankRec`] (`elem_size = 8`).
    pub const PATH_RANK: u32 = 14;
    /// CSR offsets (`num_ranked_paths + 1` `u32` words) into `PATH_CHAIN` (`elem_size = 4`).
    pub const PATH_CHAIN_PTR: u32 = 15;
    /// Packed `u32` words storing header and `(node, tr, kind, tag_slot, name_bucket)` steps for
    /// every ranked path (`elem_size = 4`).
    pub const PATH_CHAIN: u32 = 16;
    /// Materialized top-K full path headers [`super::PathHeaderRec`] (`elem_size = 56`).
    pub const PATHS: u32 = 17;
    /// Point records [`super::PathPointRec`] for materialized top-K paths (`elem_size = 32`).
    pub const PATH_POINTS: u32 = 18;
    /// Open-addressed hash table mapping pin/port name -> `NodeId` (`elem_size = 4`).
    pub const NAME_HASH: u32 = 19;
    /// Open-addressed hash table mapping instance name -> `(inst_str, cell_str, pin_range)`
    /// (`elem_size = 4`).
    pub const INST_HASH: u32 = 20;
    /// Per-`(corner, path_group)` summary metrics [`super::SummaryRec`] (`elem_size = 80`).
    pub const SUMMARY: u32 = 21;
    /// Unconstrained endpoint records [`super::UnconstrainedRec`] (`elem_size = 12`).
    pub const UNCONSTRAINED: u32 = 22;
    /// Diagnostic notes and warnings [`super::NoteRec`] (`elem_size = 16`).
    pub const NOTES: u32 = 23;
    /// Canonical JSON bytes of the analysis scope and completeness (`elem_size = 1`).
    pub const SCOPE: u32 = 24;

    /// Total number of sections in `STA v2`.
    pub const COUNT: usize = 24;

    /// Required `elem_size` for section `id` (`1..=24`), or `None` if `id` is out of range.
    #[inline]
    #[must_use]
    pub const fn elem_size(id: u32) -> Option<u32> {
        match id {
            STR_BYTES => Some(1),
            STR_OFFS => Some(4),
            META => Some(64),
            CORNERS => Some(16),
            CLOCKS => Some(48),
            TAGS => Some(16),
            DISABLED_ARCS => Some(16),
            NODE_REC => Some(152),
            TAG_EXTRA_PTR => Some(4),
            TAG_EXTRA => Some(56),
            ENDPOINTS => Some(36),
            EP_SLACK => Some(16),
            EP_ORDER => Some(4),
            PATH_RANK => Some(12),
            PATH_CHAIN_PTR => Some(8),
            PATH_CHAIN => Some(4),
            PATHS => Some(56),
            PATH_POINTS => Some(32),
            NAME_HASH => Some(4),
            INST_HASH => Some(4),
            SUMMARY => Some(80),
            UNCONSTRAINED => Some(12),
            NOTES => Some(16),
            SCOPE => Some(1),
            _ => None,
        }
    }

    /// Human-readable section name for diagnostics.
    #[inline]
    #[must_use]
    pub const fn name(id: u32) -> &'static str {
        match id {
            STR_BYTES => "STR_BYTES",
            STR_OFFS => "STR_OFFS",
            META => "META",
            CORNERS => "CORNERS",
            CLOCKS => "CLOCKS",
            TAGS => "TAGS",
            DISABLED_ARCS => "DISABLED_ARCS",
            NODE_REC => "NODE_REC",
            TAG_EXTRA_PTR => "TAG_EXTRA_PTR",
            TAG_EXTRA => "TAG_EXTRA",
            ENDPOINTS => "ENDPOINTS",
            EP_SLACK => "EP_SLACK",
            EP_ORDER => "EP_ORDER",
            PATH_RANK => "PATH_RANK",
            PATH_CHAIN_PTR => "PATH_CHAIN_PTR",
            PATH_CHAIN => "PATH_CHAIN",
            PATHS => "PATHS",
            PATH_POINTS => "PATH_POINTS",
            NAME_HASH => "NAME_HASH",
            INST_HASH => "INST_HASH",
            SUMMARY => "SUMMARY",
            UNCONSTRAINED => "UNCONSTRAINED",
            NOTES => "NOTES",
            SCOPE => "SCOPE",
            _ => "UNKNOWN",
        }
    }
}

/// Storage fidelity mode of `sta.rbin` (`--store-mode full|summary`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreMode {
    /// Writes all 24 sections including `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA`.
    #[default]
    Full = 0,
    /// Omits `NODE_REC`, `TAG_EXTRA_PTR`, and `TAG_EXTRA` (`len = 0`) while retaining endpoints,
    /// ranked paths, path chains, top-K paths, name/instance hashes, summary, and scope.
    Summary = 1,
}

impl StoreMode {
    /// Decodes a `u32` discriminant (`0 = Full`, `1 = Summary`).
    #[inline]
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Full),
            1 => Some(Self::Summary),
            _ => None,
        }
    }

    /// Returns the `u32` discriminant stored in [`StaMeta::store_mode`].
    #[inline]
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Floating-point precision used during forward propagation (`0 = f64`, `1 = f32`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorePrecision {
    /// Double-precision (`f64`) propagation.
    #[default]
    F64 = 0,
    /// Single-precision (`f32`) propagation.
    F32 = 1,
}

impl StorePrecision {
    /// Decodes a `u32` discriminant (`0 = F64`, `1 = F32`).
    #[inline]
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::F64),
            1 => Some(Self::F32),
            _ => None,
        }
    }
}

/// Normalizes `-0.0` to `+0.0` and canonicalizes quiet `NaN` payloads so that written `f64`
/// values are bitwise deterministic across thread counts (`STA-REQ-0405`).
#[inline]
#[must_use]
pub fn canon_f64(x: f64) -> f64 {
    if x == 0.0 {
        0.0
    } else if x.is_nan() {
        f64::from_bits(0x7ff8_0000_0000_0000)
    } else {
        x
    }
}

/// Normalizes `-0.0f32` to `+0.0f32` and canonicalizes quiet `NaN` payloads.
#[inline]
#[must_use]
pub fn canon_f32(x: f32) -> f32 {
    if x == 0.0 {
        0.0
    } else if x.is_nan() {
        f32::from_bits(0x7fc0_0000)
    } else {
        x
    }
}

/// Canonicalizes a `[[f64; 2]; 2]` `[tr][an]` matrix via [`canon_f64`].
#[inline]
#[must_use]
pub fn canon_grid(g: [[f64; 2]; 2]) -> [[f64; 2]; 2] {
    [
        [canon_f64(g[0][0]), canon_f64(g[0][1])],
        [canon_f64(g[1][0]), canon_f64(g[1][1])],
    ]
}

/// Absent arrival/required sentinel grid (`+inf` for early, `-inf` for late).
pub const ABSENT_EARLY_LATE: [[f64; 2]; 2] = [
    [f64::INFINITY, f64::NEG_INFINITY],
    [f64::INFINITY, f64::NEG_INFINITY],
];

/// Absent required-time sentinel grid (`-inf` for early/hold, `+inf` for late/setup).
pub const ABSENT_REQUIRED: [[f64; 2]; 2] = [
    [f64::NEG_INFINITY, f64::INFINITY],
    [f64::NEG_INFINITY, f64::INFINITY],
];

/// Absent slack sentinel grid (`+inf` when unconstrained).
pub const ABSENT_SLACK: [[f64; 2]; 2] = [
    [f64::INFINITY, f64::INFINITY],
    [f64::INFINITY, f64::INFINITY],
];

/// Section 3 (`META`, 64 bytes): counts, backend, precision, and store mode of `sta.rbin`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct StaMeta {
    pub num_nodes: u32,
    pub num_port_bits: u32,
    pub num_arcs: u32,
    pub num_cells: u32,
    pub num_startpoints: u32,
    pub num_disabled_arcs: u32,
    pub num_corners: u32,
    pub num_clocks: u32,
    pub num_tags: u32,
    pub num_endpoints: u32,
    pub num_ranked_paths: u32,
    pub num_top_k_paths: u32,
    pub num_path_groups: u32,
    pub store_mode: u32,
    pub backend_str: u32,
    pub precision: u32,
}

const _: () = assert!(size_of::<StaMeta>() == 64);

/// Section 4 (`CORNERS`, 16 bytes): operating corner descriptor.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct CornerRec {
    pub name_str: u32,
    pub lib_count: u32,
    pub flags: u32,
    pub reserved: u32,
}

const _: () = assert!(size_of::<CornerRec>() == 16);

/// Section 5 (`CLOCKS`, 48 bytes): clock descriptor.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct ClockRec {
    pub name_str: u32,
    pub source_node: u32,
    pub master_clock: u32,
    pub flags: u32,
    pub period_ns: f64,
    pub rise_edge_ns: f64,
    pub fall_edge_ns: f64,
    pub uncertainty_setup_ns: f32,
    pub uncertainty_hold_ns: f32,
}

const _: () = assert!(size_of::<ClockRec>() == 48);

/// Section 6 (`TAGS`, 16 bytes): launch tag descriptor.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct TagRec {
    pub clock_id: u32,
    pub launch_edge: u8,
    pub flags: u8,
    pub exc_state: u16,
    pub launch_source_node: u32,
    pub path_group_str: u32,
}

const _: () = assert!(size_of::<TagRec>() == 16);

/// Section 7 (`DISABLED_ARCS`, 16 bytes): disabled timing arc record.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct DisabledArcRec {
    pub arc_idx: u32,
    pub src_node: u32,
    pub dst_node: u32,
    pub reason: u32,
}

const _: () = assert!(size_of::<DisabledArcRec>() == 16);

impl DisabledArcRec {
    /// Encodes a [`DisableReason`] into its `u32` discriminant.
    #[inline]
    #[must_use]
    pub const fn encode_reason(r: DisableReason) -> u32 {
        match r {
            DisableReason::LoopBreak => 0,
            DisableReason::Constant => 1,
            DisableReason::CaseAnalysis => 2,
            DisableReason::FalseWhen => 3,
            DisableReason::UserDisabled => 4,
        }
    }

    /// Decodes `self.reason` back into a [`DisableReason`].
    #[inline]
    #[must_use]
    pub const fn decode_reason(&self) -> Option<DisableReason> {
        match self.reason {
            0 => Some(DisableReason::LoopBreak),
            1 => Some(DisableReason::Constant),
            2 => Some(DisableReason::CaseAnalysis),
            3 => Some(DisableReason::FalseWhen),
            4 => Some(DisableReason::UserDisabled),
            _ => None,
        }
    }
}

/// Section 8 (`NODE_REC`, 152 bytes): per-`(corner, node)` timing state with one inline primary
/// tag (`tag`) so single-clock / single-tag nodes require no extra pointer chase.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct NodeRec {
    /// Per-pin transition times `slew[tr][an]` (`[Rise/Fall][Early/Late]`, 32 bytes).
    pub slew: [[f64; 2]; 2],
    /// Per-pin worst required times `required[tr][an]` (`[Rise/Fall][Early/Late]`, 32 bytes).
    pub required: [[f64; 2]; 2],
    /// Per-pin worst slacks `slack[tr][an]` (`[Rise/Fall][Early/Late]`, 32 bytes).
    pub slack: [[f64; 2]; 2],
    /// Primary tag index (`NO_INDEX` when no tag reached this node).
    pub tag: u32,
    /// Bucket index in `NAME_HASH` for `O(1)` reverse node-name resolution (`NO_INDEX` if unset).
    pub pad: u32,
    /// Inline primary tag arrival times `arrival[tr][an]` (32 bytes).
    pub arrival: [[f64; 2]; 2],
    /// Inline primary tag predecessors `pred[tr][an]` (`src_node | (src_tr << 31)` or `NO_PRED`).
    pub pred: [[u32; 2]; 2],
}

const _: () = assert!(size_of::<NodeRec>() == 152);

/// Section 10 (`TAG_EXTRA`, 56 bytes): overflow per-tag arrivals and predecessors on a node with
/// two or more active tags.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct TagExtraRec {
    pub tag: u32,
    pub pad: u32,
    pub arrival: [[f64; 2]; 2],
    pub pred: [[u32; 2]; 2],
}

const _: () = assert!(size_of::<TagExtraRec>() == 56);

/// Endpoint classification stored in the low byte of [`EndpointRec::kind_flags`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKindTag {
    FlipFlop = 0,
    Latch = 1,
    OutputPort = 2,
    AsyncRecoveryRemoval = 3,
    ClockGate = 4,
}

impl EndpointKindTag {
    /// Decodes the low 8 bits of `kind_flags`.
    #[inline]
    #[must_use]
    pub const fn from_flags(flags: u32) -> Self {
        match flags & 0xFF {
            0 => Self::FlipFlop,
            1 => Self::Latch,
            2 => Self::OutputPort,
            3 => Self::AsyncRecoveryRemoval,
            _ => Self::ClockGate,
        }
    }
}

/// Section 11 (`ENDPOINTS`, 36 bytes): checked timing endpoint descriptor, sorted by ascending
/// `(data_node, check_arc_idx, capture_clock)` (`STA-REQ-0313`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct EndpointRec {
    pub data_node: u32,
    pub clock_node: u32,
    pub capture_clock: u32,
    pub path_group_str: u32,
    pub check_arc_idx: u32,
    /// Bits `0..8`: [`EndpointKindTag`]; bits `8..10`: clock edge (`0 = Rise`, `1 = Fall`,
    /// `2 = None`); bits `10..32` are zero.
    pub kind_flags: u32,
    pub worst_setup_rank: u32,
    pub worst_hold_rank: u32,
    /// `NAME_HASH` bucket of `data_node` plus one, or `0` when unset (full 32-bit range; format
    /// version 1 packed it into 22 bits of `kind_flags` and dropped larger buckets).
    pub name_bucket_plus1: u32,
}

const _: () = assert!(size_of::<EndpointRec>() == 36);

impl EndpointRec {
    /// Packs `kind` and the optional clock `edge` into `kind_flags`.
    #[inline]
    #[must_use]
    pub const fn pack_kind_flags(kind: EndpointKindTag, edge: Option<Tr>) -> u32 {
        let edge_bits = match edge {
            Some(Tr::Rise) => 0u32,
            Some(Tr::Fall) => 1u32,
            None => 2u32,
        };
        (kind as u32) | (edge_bits << 8)
    }

    /// Encodes an optional `NAME_HASH` bucket as [`EndpointRec::name_bucket_plus1`]. A bucket is
    /// a `u32` slot index below the bucket count, so `b + 1` never overflows.
    #[inline]
    #[must_use]
    pub const fn pack_name_bucket(name_bucket: Option<u32>) -> u32 {
        match name_bucket {
            Some(b) => b + 1,
            None => 0,
        }
    }

    /// Returns the [`EndpointKindTag`] of this endpoint.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> EndpointKindTag {
        EndpointKindTag::from_flags(self.kind_flags)
    }

    /// Returns the active clock edge of this endpoint, if any.
    #[inline]
    #[must_use]
    pub const fn clock_edge(&self) -> Option<Tr> {
        match (self.kind_flags >> 8) & 0b11 {
            0 => Some(Tr::Rise),
            1 => Some(Tr::Fall),
            _ => None,
        }
    }

    /// Returns the `NAME_HASH` bucket index of `data_node`, if recorded.
    #[inline]
    #[must_use]
    pub const fn name_bucket(&self) -> Option<u32> {
        let nb = self.name_bucket_plus1;
        if nb == 0 { None } else { Some(nb - 1) }
    }
}

/// Section 12 (`EP_SLACK`, 16 bytes): worst setup and hold slack (in ns; `NaN` when absent) for a
/// `(corner, endpoint)` pair.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct EpSlackRec {
    pub setup_slack: f64,
    pub hold_slack: f64,
}

const _: () = assert!(size_of::<EpSlackRec>() == 16);

/// Section 14 (`PATH_RANK`, 12 bytes): compact ranked path record; row `i` is handle `path<i + 1>`.
///
/// Names an endpoint by its full `u32` index (format version 1 held only `20` bits) and a corner
/// by a `u16`; [`check_store_capacity`] rejects analyses beyond [`MAX_STORE_ENDPOINTS`] or
/// [`MAX_STORE_CORNERS`] before any record is packed.
/// Largest number of endpoints a timing table can index (`u32` indices, `u32::MAX` is the
/// `NO_INDEX` sentinel).
pub const MAX_STORE_ENDPOINTS: usize = (u32::MAX - 1) as usize;

/// Largest number of corners a timing table supports (`16`).
pub const MAX_STORE_CORNERS: usize = 16;

/// Rejects an analysis whose endpoints or corners a `PATH_RANK` record cannot name, with error
/// `T0022`, instead of packing truncated indices (AGENTS rule 9.2; `STA-REQ-0302`).
pub fn check_store_capacity(n_endpoints: usize, n_corners: usize) -> Result<(), String> {
    if n_endpoints > MAX_STORE_ENDPOINTS || n_corners > MAX_STORE_CORNERS {
        return Err(format!(
            "T0022: the timing table holds at most {MAX_STORE_ENDPOINTS} endpoints and \
             {MAX_STORE_CORNERS} corners; this analysis has {n_endpoints} endpoints and \
             {n_corners} corners"
        ));
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct PathRankRec {
    pub slack_ns: f32,
    pub endpoint_idx: u32,
    pub corner_idx: u16,
    pub flags: u8,
    pub pad: u8,
}

const _: () = assert!(size_of::<PathRankRec>() == 12);

impl PathRankRec {
    /// Creates a new [`PathRankRec`].
    #[inline]
    #[must_use]
    pub fn new(
        slack_ns: f32,
        endpoint_idx: usize,
        corner_idx: usize,
        check_kind: CheckKind,
        end_tr: Tr,
        in_top_k: bool,
    ) -> Self {
        debug_assert!(endpoint_idx <= MAX_STORE_ENDPOINTS && corner_idx < MAX_STORE_CORNERS);
        let is_hold = u8::from(check_kind.is_hold());
        let tr_bit = (end_tr.index() as u8) << 1;
        let kind_bits = (match check_kind {
            CheckKind::Setup => 0u8,
            CheckKind::Hold => 1u8,
            CheckKind::Recovery => 2u8,
            CheckKind::Removal => 3u8,
        }) << 2;
        let top_k_bit = if in_top_k { 0x80 } else { 0 };
        Self {
            slack_ns: canon_f32(slack_ns),
            endpoint_idx: endpoint_idx as u32,
            corner_idx: corner_idx as u16,
            flags: is_hold | tr_bit | kind_bits | top_k_bit,
            pad: 0,
        }
    }

    /// Endpoint index in `ENDPOINTS`.
    #[inline]
    #[must_use]
    pub const fn endpoint_index(&self) -> usize {
        self.endpoint_idx as usize
    }

    /// Corner index in `CORNERS`.
    #[inline]
    #[must_use]
    pub const fn corner_index(&self) -> usize {
        self.corner_idx as usize
    }

    /// True if this ranked path is a hold/removal (early/min) path.
    #[inline]
    #[must_use]
    pub const fn is_hold(&self) -> bool {
        (self.flags & 1) != 0
    }

    /// Analysis bound (`Early` for hold/removal, `Late` for setup/recovery).
    #[inline]
    #[must_use]
    pub const fn bound(&self) -> An {
        if self.is_hold() { An::Early } else { An::Late }
    }

    /// Ending transition at the endpoint data pin.
    #[inline]
    #[must_use]
    pub const fn end_tr(&self) -> Tr {
        if ((self.flags >> 1) & 1) == 0 {
            Tr::Rise
        } else {
            Tr::Fall
        }
    }

    /// Check kind (`Setup`, `Hold`, `Recovery`, `Removal`).
    #[inline]
    #[must_use]
    pub const fn check_kind(&self) -> CheckKind {
        match (self.flags >> 2) & 0b11 {
            0 => CheckKind::Setup,
            1 => CheckKind::Hold,
            2 => CheckKind::Recovery,
            _ => CheckKind::Removal,
        }
    }

    /// True if this ranked path is also materialized in section `PATHS` (`top_k`).
    #[inline]
    #[must_use]
    pub const fn in_top_k(&self) -> bool {
        (self.flags & 0x80) != 0
    }
}

/// Kind of a step along a timing path (`PATH_POINTS` and `PATH_CHAIN`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathPointKind {
    Port = 0,
    Net = 1,
    Cell = 2,
    ClockLaunch = 3,
}

impl PathPointKind {
    /// Decodes a `u8` tag (`0..=3`).
    #[inline]
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v & 0b11 {
            0 => Self::Port,
            1 => Self::Net,
            2 => Self::Cell,
            _ => Self::ClockLaunch,
        }
    }
}

/// Number of `u32` header words at the start of each `PATH_CHAIN` slice (`required: f64`,
/// `check_value: f64`).
pub const PATH_CHAIN_HEADER_WORDS: usize = 4;

/// Packs an `f64` into two little-endian `u32` words `[lo, hi]`.
#[inline]
#[must_use]
pub fn pack_f64_words(v: f64) -> [u32; 2] {
    let bits = canon_f64(v).to_bits();
    [bits as u32, (bits >> 32) as u32]
}

/// Unpacks two `u32` words `[lo, hi]` into an `f64`.
#[inline]
#[must_use]
pub fn unpack_f64_words(lo: u32, hi: u32) -> f64 {
    f64::from_bits(u64::from(lo) | (u64::from(hi) << 32))
}

/// Number of `u32` words of one `PATH_CHAIN` step: `[node, step_word, name_bucket]`.
pub const PATH_CHAIN_STEP_WORDS: usize = 3;

/// Largest tag slot a path-chain step can name (`29` bits).
pub const MAX_CHAIN_TAG_SLOT: usize = (1 << 29) - 1;

/// Packs one path-chain step into three `u32` words `[node, step_word, name_bucket]`.
///
/// `node` is the full `u32` node id (format version 1 kept only `24` bits). `step_word` layout:
/// - bit `0`: `tr` (`0 = Rise`, `1 = Fall`)
/// - bits `1..3`: [`PathPointKind`] (`0..=3`)
/// - bits `3..32`: `tag_slot` (`0` = primary `NodeRec.tag`, `k` = `TAG_EXTRA` offset `k - 1`)
#[inline]
#[must_use]
pub const fn pack_chain_step(
    node: NodeId,
    tr: Tr,
    kind: PathPointKind,
    tag_slot: usize,
    name_bucket: u32,
) -> [u32; PATH_CHAIN_STEP_WORDS] {
    debug_assert!(tag_slot <= MAX_CHAIN_TAG_SLOT);
    let tr_bit = match tr {
        Tr::Rise => 0u32,
        Tr::Fall => 1u32,
    };
    let w1 = tr_bit | ((kind as u32) << 1) | ((tag_slot as u32) << 3);
    [node.0, w1, name_bucket]
}

/// Unpacks a `[node, step_word, name_bucket]` step from `PATH_CHAIN`.
#[inline]
#[must_use]
pub const fn unpack_chain_step(
    w: &[u32; PATH_CHAIN_STEP_WORDS],
) -> (NodeId, Tr, PathPointKind, usize, u32) {
    let node = NodeId(w[0]);
    let tr = if (w[1] & 1) == 0 { Tr::Rise } else { Tr::Fall };
    let kind = PathPointKind::from_u8(((w[1] >> 1) & 0b11) as u8);
    let tag_slot = (w[1] >> 3) as usize;
    (node, tr, kind, tag_slot, w[2])
}

/// Section 17 (`PATHS`, 56 bytes): header of a materialized top-K path.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct PathHeaderRec {
    pub slack: f64,
    pub arrival: f64,
    pub required: f64,
    pub check_value: f64,
    pub rank_idx: u32,
    pub endpoint_idx: u32,
    pub launch_clock: u32,
    pub capture_clock: u32,
    pub point_begin: u32,
    pub point_end: u32,
}

const _: () = assert!(size_of::<PathHeaderRec>() == 56);

/// Section 18 (`PATH_POINTS`, 32 bytes): one point of a materialized top-K path.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct PathPointRec {
    pub arrival: f64,
    pub incr_delay: f64,
    pub slew: f64,
    pub node: u32,
    pub tr: u8,
    pub kind: u8,
    pub pad: u16,
}

const _: () = assert!(size_of::<PathPointRec>() == 32);

/// Section 21 (`SUMMARY`, 80 bytes): per-`(corner, path_group)` timing summary record.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SummaryRec {
    pub setup_wns: f64,
    pub setup_tns: f64,
    pub hold_wns: f64,
    pub hold_tns: f64,
    pub corner_idx: u32,
    pub group_str: u32,
    pub setup_violating: u32,
    pub setup_endpoints: u32,
    pub hold_violating: u32,
    pub hold_endpoints: u32,
    pub max_trans_violating: u32,
    pub max_cap_violating: u32,
    pub total_endpoints: u32,
    pub unconstrained_endpoints: u32,
    pub reserved0: u32,
    pub reserved1: u32,
}

const _: () = assert!(size_of::<SummaryRec>() == 80);

/// Why a potential timing endpoint is unconstrained (`UNCONSTRAINED` section).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnconstrainedReasonTag {
    NoClock = 0,
    NoArrival = 1,
    FalsePath = 2,
    ConstantPin = 3,
    UncheckedOutput = 4,
}

impl UnconstrainedReasonTag {
    /// Decodes a `u32` discriminant.
    #[inline]
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::NoClock),
            1 => Some(Self::NoArrival),
            2 => Some(Self::FalsePath),
            3 => Some(Self::ConstantPin),
            4 => Some(Self::UncheckedOutput),
            _ => None,
        }
    }
}

/// Section 22 (`UNCONSTRAINED`, 12 bytes): unconstrained endpoint record.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct UnconstrainedRec {
    pub node: u32,
    pub reason: u32,
    /// `NAME_HASH` bucket index for `O(1)` node-name lookup (`NO_INDEX` if unset).
    pub reserved: u32,
}

const _: () = assert!(size_of::<UnconstrainedRec>() == 12);

/// Severity of a diagnostic note in `NOTES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteSeverity {
    Note = 0,
    Warning = 1,
}

/// Section 23 (`NOTES`, 16 bytes): aggregated diagnostic note or warning.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct NoteRec {
    pub code_str: u32,
    pub message_str: u32,
    pub severity: u32,
    pub count: u32,
}

const _: () = assert!(size_of::<NoteRec>() == 16);

/// Scope and completeness metadata stored as canonical JSON in section 24 (`SCOPE`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeReport {
    pub top: String,
    pub corners: Vec<String>,
    pub wire_model: String,
    pub clock_model: String,
    pub cppr: String,
    pub backend: String,
    pub precision: StorePrecision,
    pub store_mode: StoreMode,
    pub exceptions_applied: Vec<String>,
    pub completeness: CompletenessReport,
    /// The counts of the pin limit checks of each corner, in corner order; absent in a table
    /// whose analysis did not check limits (`STA-REQ-0807`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Vec<CornerLimitsReport>>,
}

/// The counts of the pin limit checks of one corner inside [`ScopeReport`] (`STA-REQ-0807`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CornerLimitsReport {
    /// The corner.
    pub corner: String,
    /// Counts by check kind name (`max_transition`, `max_capacitance`, `max_fanout`,
    /// `min_pulse_width_high`, `min_pulse_width_low`, `min_period`).
    pub kinds: BTreeMap<String, LimitKindReport>,
    /// Instance pins with pulse checks reached by no clock.
    pub unclocked_pulse_pins: u64,
}

/// The counts of one check kind in one corner (`STA-REQ-0807`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitKindReport {
    /// Checked rows.
    pub checked: u64,
    /// Rows with a negative slack.
    pub violating: u64,
    /// Values without a limit (design rules).
    pub unlimited: u64,
    /// The worst slack, absent without a checked row.
    pub worst_slack_ns: Option<f64>,
}

/// Completeness breakdown inside [`ScopeReport`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletenessReport {
    pub analyzed_endpoints: usize,
    pub unconstrained_endpoints: usize,
    pub unconstrained_by_reason: BTreeMap<String, usize>,
    pub disabled_arcs_by_reason: BTreeMap<String, usize>,
    pub unanalyzed_arcs_by_type: BTreeMap<String, usize>,
}

#[cfg(test)]
mod capacity_tests {
    use super::{MAX_STORE_CORNERS, MAX_STORE_ENDPOINTS, PathRankRec, check_store_capacity};
    use crate::engine::graph::CheckKind;
    use crate::engine::types::Tr;

    // STA-REQ-0302: the largest endpoint and corner indices round-trip through `PATH_RANK`, and
    // one more is refused instead of being truncated. Format version 2 also names endpoints
    // beyond the 20-bit limit of version 1 (OpenC910 has more than four million).
    #[test]
    fn path_rank_capacity_is_checked() {
        let rec = PathRankRec::new(
            -1.0,
            MAX_STORE_ENDPOINTS,
            MAX_STORE_CORNERS - 1,
            CheckKind::Setup,
            Tr::Fall,
            true,
        );
        assert_eq!(rec.endpoint_index(), MAX_STORE_ENDPOINTS);
        assert_eq!(rec.corner_index(), MAX_STORE_CORNERS - 1);
        assert!(check_store_capacity(MAX_STORE_ENDPOINTS, MAX_STORE_CORNERS).is_ok());
        let err = check_store_capacity(MAX_STORE_ENDPOINTS + 1, 1).unwrap_err();
        assert!(err.starts_with("T0022"), "{err}");
        assert!(check_store_capacity(1, MAX_STORE_CORNERS + 1).is_err());
        let big = PathRankRec::new(0.5, 4_444_505, 0, CheckKind::Hold, Tr::Rise, false);
        assert_eq!(big.endpoint_index(), 4_444_505);
        assert!(check_store_capacity(4_444_505, 1).is_ok());
    }

    // STA-REQ-0302: a path-chain step keeps node ids above 2^24 and tag slots above 31.
    #[test]
    fn chain_steps_keep_full_node_ids_and_tag_slots() {
        use super::{PathPointKind, pack_chain_step, unpack_chain_step};
        use crate::engine::types::NodeId;
        let w = pack_chain_step(NodeId(20_000_123), Tr::Fall, PathPointKind::Cell, 40, 7);
        let (node, tr, kind, slot, nb) = unpack_chain_step(&w);
        assert_eq!(
            (node, tr, kind, slot, nb),
            (NodeId(20_000_123), Tr::Fall, PathPointKind::Cell, 40, 7)
        );
    }
}
