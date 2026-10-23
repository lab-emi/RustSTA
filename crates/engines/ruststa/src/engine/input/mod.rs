// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Views of the revision's netlist, libraries and constraints (owner: E1).
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`netview`] | The frozen netlist: drivers, loads, names on demand | E1 |
//! | [`libview`] | The libraries of each corner: typed arcs, pins, tables | E3 |
//! | [`constr`] | The compiled constraints: object queries resolved against the netlist | E2, then E12 |

pub mod constr;
pub mod libview;
pub mod netview;

pub use netview::{
    NetName, NetTarget, NetlistDiagnostic, NetlistView, NetlistViewError, NodeKind, NodeName,
};
