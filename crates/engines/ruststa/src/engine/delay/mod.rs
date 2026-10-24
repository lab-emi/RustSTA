// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Delay calculation: table lookup, net loads, gate delays, wire models and derates (owner: E3).
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`lut`] | Table lookup with up to three axes; the clamp rule | E3 |
//! | [`load`] | Net loads per transition and bound | E3 |
//! | [`gate`] | The gate delay seam [`GateDelay`](gate::GateDelay) | E3 |
//! | [`driver`] | The driver of an extracted pi load: Thevenin resistance, lumped regime | E3 |
//! | [`wire`] | The wire model seam [`WireModel`](wire::WireModel): ideal and lumped wire load | E4 |
//! | [`wire_tree`] | Tree wire-load estimation (Phase 2b) | E14 |
//! | [`spef`] | Parasitics from the revision's SPEF table (Phase 2b) | E14 |
//! | [`derate`] | The derate seam [`Derate`](derate::Derate) (Phase 2b) | E9 |

pub mod derate;
pub mod driver;
pub mod gate;
pub mod load;
pub mod lut;
pub mod spef;
pub mod wire;
pub mod wire_tree;
