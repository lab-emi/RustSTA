// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The clock model seam: how clock edges reach the clock pins (owner: E5).
//!
//! Engine sections C and G. A [`ClockModel`] gives the arrival of each clock edge at the pins of
//! the clock network. Phase 2a has ideal clocks ([`ideal`], work item E5): the edge arrives at its
//! waveform time plus the clock's source and network latency, with the clock's ideal transition.
//! Phase 2b adds propagated clocks ([`propagated`]), whose network latency is the delay through
//! the clock network, and generated clocks ([`generated`]), both by work item E9.
//!
//! | Module | Content | Owner |
//! |---|---|---|
//! | [`ideal`] | Ideal clocks | E5 |
//! | [`propagated`] | Propagated clocks (Phase 2b) | E9 |
//! | [`generated`] | Generated clocks (Phase 2b) | E9 |

pub mod generated;
pub mod ideal;
pub mod propagated;

use crate::engine::types::{An, ClockId, NodeId, Tr};

/// Positive clock sense bit (`bit 0`): a rising edge at the clock source arrives as [`Tr::Rise`]
/// at the pin, and a falling edge as [`Tr::Fall`].
pub const SENSE_POS: u8 = 1;

/// Negative clock sense bit (`bit 1`): a rising edge at the clock source arrives as [`Tr::Fall`]
/// at the pin, and a falling edge as [`Tr::Rise`].
pub const SENSE_NEG: u8 = 2;

/// Both positive and negative clock senses (`SENSE_POS | SENSE_NEG`).
pub const SENSE_BOTH: u8 = SENSE_POS | SENSE_NEG;

/// Bitmask of clock senses (`SENSE_POS`, `SENSE_NEG`) with which a clock reaches a node.
pub type SenseBits = u8;

/// Returns the source waveform edges (`Rise`, `Fall`) of a clock that produce transition `pin_tr`
/// at a pin reached with clock sense `bits` (`STA-REQ-0200`, `STA-REQ-0217`).
#[must_use]
pub fn source_edges_for_pin_tr(pin_tr: Tr, bits: SenseBits) -> Vec<Tr> {
    let mut out = Vec::with_capacity(2);
    if bits & SENSE_POS != 0 {
        out.push(pin_tr);
    }
    if bits & SENSE_NEG != 0 {
        out.push(pin_tr.opposite());
    }
    out
}

/// Returns the pin transitions (`Rise`, `Fall`) produced at a pin reached with clock sense `bits`
/// by source waveform edge `source_edge` (`STA-REQ-0200`, `STA-REQ-0217`).
#[must_use]
pub fn pin_trs_for_source_edge(source_edge: Tr, bits: SenseBits) -> Vec<Tr> {
    let mut out = Vec::with_capacity(2);
    if bits & SENSE_POS != 0 {
        out.push(source_edge);
    }
    if bits & SENSE_NEG != 0 {
        out.push(source_edge.opposite());
    }
    out
}

/// The arrival of a clock edge at a pin of the clock network.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockArrival {
    /// The time (ns) after the edge's time in the clock waveform.
    pub latency: f64,
    /// The slew (ns) of the clock at the pin.
    pub slew: f64,
}

/// A clock model (`STA-REQ-0200`, `STA-REQ-0217`).
pub trait ClockModel {
    /// The name of the model in the analysis scope (`clock_model`).
    fn scope_name(&self) -> &'static str;

    /// The arrival for bound `an` of the edge `edge` of `clock` (the rising or the falling edge
    /// of its waveform) at the clock pin `pin`, where it arrives as transition `tr`; `None` when
    /// that edge does not reach the pin as `tr`.
    fn arrival(
        &self,
        clock: ClockId,
        edge: Tr,
        pin: NodeId,
        tr: Tr,
        an: An,
    ) -> Option<ClockArrival>;

    /// The clocks and sense bits (`SENSE_POS`, `SENSE_NEG`) that reach `pin` in the clock network.
    fn pin_clocks(&self, pin: NodeId) -> &[(ClockId, SenseBits)] {
        let _ = pin;
        &[]
    }

    /// True when at least one clock reaches `pin` in the clock network.
    fn is_clock_network_pin(&self, pin: NodeId) -> bool {
        !self.pin_clocks(pin).is_empty()
    }
}
