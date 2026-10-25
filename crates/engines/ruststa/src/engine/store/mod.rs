// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing table `sta.rbin` (STA v2) and its mapped queries (owner: E10).
//!
//! Engine section I. A deterministic writer and a zero-copy reader of the timing table in the
//! workspace's rbin container: per pin the slews, required times and slacks; per tag the arrivals
//! and predecessors; the endpoints, their order and slacks; the ranked paths with their chains and
//! points; name and instance hashes; the timing summary per corner and path group; the scope.
//! Opening a table validates its structure only; the content hash is checked at import, for
//! mutation inputs and on request. A summary store mode keeps only the endpoints, paths, chains,
//! names and summary. [`from_analysis`] builds the table of a revision from its analysis.

pub mod builder;
pub mod from_analysis;
pub mod hash;
pub mod layout;
pub mod query;
pub mod reader;
pub mod writer;

pub use builder::{
    CornerBuildInput, StaStoreBuildOptions, build_single_corner_store, build_sta_store,
};
pub use from_analysis::build_store_from_analysis;
pub use hash::{
    BUCKET_WORDS, INST_HASH_MAX_CELL_STR, INST_HASH_MAX_PIN_COUNT, InstHashBuilder, InstHashEntry,
    InstHashView, NameHashBuilder, NameHashView, StringTableBuilder, StringTableView,
    hash_node_name,
};
pub use layout::{
    ABSENT_EARLY_LATE, ABSENT_REQUIRED, ABSENT_SLACK, ClockRec, CompletenessReport,
    CornerLimitsReport, CornerRec, DisabledArcRec, EndpointKindTag, EndpointRec, EpSlackRec,
    FORMAT_VERSION, KIND, LimitKindReport, NO_INDEX, NO_PRED, NodeRec, NoteRec, NoteSeverity,
    PATH_CHAIN_HEADER_WORDS, PathHeaderRec, PathPointKind, PathPointRec, PathRankRec, ScopeReport,
    StaMeta, StoreMode, StorePrecision, SummaryRec, TABLE_NAME, TABLE_PATH, TagExtraRec, TagRec,
    UnconstrainedReasonTag, UnconstrainedRec, VERSIONS, canon_f32, canon_f64, canon_grid,
    pack_chain_step, pack_f64_words, section, unpack_chain_step, unpack_f64_words,
};
pub use query::{
    CompactEndpointEntry, InspectedInstance, InspectedInstancePin, InspectedNode,
    InspectedTagState, QueriedEndpoint, QueriedPath, QueriedPathPoint, QueriedSummary,
    QueriedSummaryRow,
};
pub use reader::{StaStore, StaStoreError, StaStoreView};
pub use writer::StaStoreData;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_elem_sizes_match_record_sizes() {
        assert_eq!(section::elem_size(section::STR_BYTES), Some(1));
        assert_eq!(section::elem_size(section::STR_OFFS), Some(4));
        assert_eq!(
            section::elem_size(section::META),
            Some(size_of::<StaMeta>() as u32)
        );
        assert_eq!(
            section::elem_size(section::CORNERS),
            Some(size_of::<CornerRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::CLOCKS),
            Some(size_of::<ClockRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::TAGS),
            Some(size_of::<TagRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::DISABLED_ARCS),
            Some(size_of::<DisabledArcRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::NODE_REC),
            Some(size_of::<NodeRec>() as u32)
        );
        assert_eq!(section::elem_size(section::TAG_EXTRA_PTR), Some(4));
        assert_eq!(
            section::elem_size(section::TAG_EXTRA),
            Some(size_of::<TagExtraRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::ENDPOINTS),
            Some(size_of::<EndpointRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::EP_SLACK),
            Some(size_of::<EpSlackRec>() as u32)
        );
        assert_eq!(section::elem_size(section::EP_ORDER), Some(4));
        assert_eq!(
            section::elem_size(section::PATH_RANK),
            Some(size_of::<PathRankRec>() as u32)
        );
        assert_eq!(section::elem_size(section::PATH_CHAIN_PTR), Some(8));
        assert_eq!(section::elem_size(section::PATH_CHAIN), Some(4));
        assert_eq!(
            section::elem_size(section::PATHS),
            Some(size_of::<PathHeaderRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::PATH_POINTS),
            Some(size_of::<PathPointRec>() as u32)
        );
        assert_eq!(section::elem_size(section::NAME_HASH), Some(4));
        assert_eq!(section::elem_size(section::INST_HASH), Some(4));
        assert_eq!(
            section::elem_size(section::SUMMARY),
            Some(size_of::<SummaryRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::UNCONSTRAINED),
            Some(size_of::<UnconstrainedRec>() as u32)
        );
        assert_eq!(
            section::elem_size(section::NOTES),
            Some(size_of::<NoteRec>() as u32)
        );
        assert_eq!(section::elem_size(section::SCOPE), Some(1));
    }

    #[test]
    fn neg_zero_canonicalizes_to_pos_zero() {
        assert_eq!(canon_f64(-0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(canon_f32(-0.0).to_bits(), 0.0f32.to_bits());
    }
}
