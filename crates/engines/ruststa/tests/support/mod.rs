// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Support modules of the engine's tests (owner: E1).
//!
//! A test file uses them with `mod support;`. The test target `support` (`main.rs`) tests them.
//!
//! | Module | Content |
//! |---|---|
//! | [`legacy_import`] | Netlists through the legacy flattener, as building steps of the shared netlist model |
//! | [`pdk`] | The PDK directory and the `tt` libraries of a PDK campaign |
//! | [`mutate`] | Random designs from a library's cells and random mutation campaigns of timing sessions |

#![allow(dead_code)]

pub mod legacy_import;
pub mod mutate;
pub mod pdk;
