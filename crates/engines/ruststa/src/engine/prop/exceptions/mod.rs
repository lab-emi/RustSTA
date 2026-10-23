// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions (owner: E8).
//!
//! Engine section F (`STA-REQ-0250` to `STA-REQ-0270`). `set_false_path`, `set_multicycle_path`,
//! `set_max_delay` and `set_min_delay` with `-from` and `-to` (and their `-rise_`/`-fall_` forms),
//! and `set_clock_groups`. Exception states are hash-consed and carried in the tags. Precedence:
//! clock groups, then false path, then maximum and minimum delay, then multicycle path, then the
//! more specific exception, then single-analysis over both-analyses, then later statement order;
//! a conflict of equal priority gets warning `D0111`. Multicycle paths shift the capturing edge
//! (`-end` by default for setup, `-start` for hold, and the hold check follows the setup
//! multiplier unless `-hold` is given); maximum and minimum delays replace the relation; clock
//! groups exclude the pairs of their clocks. Exceptions with `-through` are in [`through`]
//! (Phase 2b, owner: E13).
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`state`] | Hash-consed exception states, tag interning, and node state budget (`STA-REQ-0269`) | E8 |
//! | [`resolve`] | Matching, precedence, specificity, ties, multicycle/delay relations, clock groups, diagnostics and scope (`STA-REQ-0250`..`0270`) | E8 |
//! | [`apply`] | Exceptions in the corner analysis: startpoint tags and check resolution (`STA-REQ-0271`) | E8 |
//! | [`eval`] | Exception-aware corner propagation and endpoint slack evaluator | E8 |
//! | [`through`] | Exceptions with `-through` (Phase 2b) | E13 |

pub mod apply;
mod check;
pub mod eval;
mod helpers;
pub mod resolve;
pub mod state;
pub mod through;

pub use apply::ExceptionContext;
pub use eval::{
    ExceptionEvalOptions, ExceptionEvaluation, evaluate_compiled_exceptions, evaluate_exceptions,
};
pub use helpers::{hold_relation_ns, open_relation_ns, setup_relation_ns};
pub use resolve::{
    AnalysisKind, ClockGroupsIndex, EndpointExceptionResult, EndpointStatus, ExceptionRule,
    ExceptionRuleKind, ExceptionScopeSummary, ExceptionTable, ExceptionTypeRank,
    HoldMulticycleWinner, PathAnalysisResolution, SetupMulticycleWinner, specificity,
};
pub use state::{
    DEFAULT_NODE_EXCEPTION_STATE_BUDGET, ExceptionId, ExceptionState, ExceptionStateData,
    ExceptionStateTable, PropagationTagKey, PropagationTagTable, StartLaunchKey, StartMatchEntry,
    check_node_exception_state_budget,
};
