// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing engine on the RustEDA workspace (owner: E5, from E0).
//!
//! This tree is built beside the legacy engine of the crate root (`ruststa::{graph, prop, ...}`,
//! behind `ruststa sta`), which keeps its paths and its behaviour until work item Z1 removes it.
//! The engine reads one revision of a workspace: the frozen netlist, the compiled libraries of
//! every corner and the compiled constraints. It writes the timing table `sta.rbin` and the timing
//! summary and status of the revision. Its rules are specified in `docs/specs/sta/`.
//!
//! Work item E0 scaffolded every module. Each module names its owner, the work item (by its
//! roadmap id) that implements and maintains it: "owner: E2, then E12" when a later work item
//! takes the module over, "owner: E5, from E0" when the owner starts from content of E0. A seam
//! that a later work item fills is a trait or a type with an honest stub: an ideal model, or a
//! [`NotSupportedYet`](types::NotSupportedYet) error naming the planned work item. The work items
//! that call a seam wire it, so that the work item that fills it changes only the seam's file.
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`types`] | Shared types: transitions, analysis bounds, indices, unsupported features | E5, from E0 |
//! | [`analysis`] | The orchestrator: an analysis of every corner of a revision | E5, from E0 |
//! | [`scope`] | The analysis scope and the timing summary and status of a revision | E5, from E0 |
//! | [`input`] | Views of the revision's netlist, libraries and constraints | E1, E2, E3 |
//! | [`graph`] | The timing graph, its levels and constant propagation | E1, E7 |
//! | [`delay`] | Delay calculation, wire models and derates | E3, E4, E9, E14 |
//! | [`prop`] | Propagation: tags, start rows, relations, checks, clocks, exceptions | E5, E6, E8, E9, E13, C2 |
//! | [`paths`] | Worst paths and their handles | E6 |
//! | [`store`] | The timing table `sta.rbin` (STA v2) | E10 |
//! | [`incr`] | Incremental timing: timing sessions and the update of a revision | E11 |
//! | [`session_api`] | Slack queries on timing sessions for the optimizer (Phase 3) | X2 |

pub mod analysis;
pub mod delay;
pub mod graph;
pub mod incr;
pub mod input;
pub mod paths;
pub mod prop;
pub mod scope;
pub mod session_api;
pub mod store;
pub mod types;
