// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The timing table of a revision from its analysis: the engine's builder
//! ([`ruststa::engine::store::from_analysis`]), which `ruststa run`, `ruststa update` and timing
//! sessions share so that their tables are byte-identical (`STA-REQ-0302`, `STA-REQ-0611`).

pub use ruststa::engine::store::from_analysis::build_store_from_analysis;
