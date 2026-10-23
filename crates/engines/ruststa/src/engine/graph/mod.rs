// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing graph: nodes, arcs, levels and constants (owner: E1).
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`build`] | Nodes and arcs from the netlist and library views; cycle breaking | E1 |
//! | [`levels`] | Exact longest-path levels and the canonical topological id | E1 |
//! | [`constants`] | Constant propagation, case analysis and disabled arcs | E7 |

pub mod build;
pub mod constants;
pub mod levels;

pub use build::{
    ArcKind, ArcSense, CheckArc, CheckKind, DisableReason, GraphArc, GraphBuildError,
    GraphDiagnostic, LaunchArc, TimingGraph,
};
pub use levels::Levels;
