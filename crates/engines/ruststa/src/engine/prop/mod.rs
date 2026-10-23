// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Propagation: tags, start rows, relations, checks, clocks, exceptions and required times
//! (owner: E5).
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`tags`] | Propagation tags: clock edge and exception state | E5 |
//! | [`starts`] | Start rows: input delays, clock sources, clock-as-data sources | E5 |
//! | [`relation`] | Setup and hold relations between clock edges | E5 |
//! | [`latch`] | Latch re-launch as a tag-mapping arc | E5 |
//! | [`checks`] | Timing checks of every kind | E5 |
//! | [`gating`] | Clock-gating checks | E5 |
//! | [`endpoints`] | Endpoint status, path groups and completeness | E5 |
//! | [`clocks`] | The clock model seam; ideal, propagated and generated clocks | E5, E9 |
//! | [`cppr`] | The CPPR seam (Phase 2b) | E9 |
//! | [`exceptions`] | Timing exceptions; `-through` (Phase 2b) | E8, E13 |
//! | [`lower`] | Lowering onto the compute timing task | C2 |
//! | [`backward`] | Required times and slack per pin | E6 |

pub mod backward;
pub mod checks;
pub mod clocks;
pub mod cppr;
pub mod endpoints;
pub mod exceptions;
pub mod gating;
pub mod latch;
pub mod lower;
pub mod relation;
pub mod starts;
pub mod tags;

pub use checks::{ASYNC_DEFAULT_GROUP, EndpointCheckKind, SlackRecord, evaluate_all_checks};
pub use clocks::ideal::IdealClockModel;
pub use clocks::{ClockArrival, ClockModel, SENSE_BOTH, SENSE_NEG, SENSE_POS, SenseBits};
pub use endpoints::{CornerEndpoints, EndpointRecord, EndpointStatus, PathGroupSummary};
pub use gating::{ClockGatingSite, discover_gating_sites, evaluate_gating_sites};
pub use latch::{LatchWindow, build_latch_tag_maps, compute_latch_window, pin_latch_gq_slews};
pub use lower::{ArcTagMaps, GraphStartBuilder, LoweredCorner, LoweredResult};
pub use relation::{
    MAX_COMMON_PERIOD_CYCLES, RelationTable, common_period_fs, fs_to_ns, hold_relation,
    latch_pulse_width, latch_window_relations, non_negative_relation, ns_to_fs, setup_relation,
};
pub use starts::{DriveCellEffect, SeededStarts, build_start_rows};
pub use tags::{DEFAULT_TAG_BUDGET, Tag, TagTable};
