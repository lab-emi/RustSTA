// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The CPPR seam: clock reconvergence pessimism removal, Phase 2b (owner: E9).
//!
//! Engine section G. When the launching and the capturing clock paths of a check share a part of
//! the clock network, the check is credited the difference of the late and the early arrival at
//! the deepest shared node, for the same transition. With ideal clocks every arrival on the clock
//! network is the same for both bounds, so the credit is 0 ([`NoCppr`]); the exact credit for
//! propagated clocks ([`ExactCppr`]) follows in Phase 2b.

use crate::engine::types::{ClockId, NodeId, NotSupportedYet};

/// Clock reconvergence pessimism removal.
pub trait Cppr {
    /// The credit (ns, never negative) of a check whose launching clock pin `launch` and
    /// capturing clock pin `capture` are both reached by `clock`.
    fn credit(&self, clock: ClockId, launch: NodeId, capture: NodeId) -> f64;
}

/// No pessimism to remove: the credit of every check is 0, as with ideal clocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCppr;

impl Cppr for NoCppr {
    fn credit(&self, _clock: ClockId, _launch: NodeId, _capture: NodeId) -> f64 {
        0.0
    }
}

/// The exact credit of propagated clocks. Not supported yet: the type has no values until work
/// item E9 implements it.
#[derive(Debug)]
pub enum ExactCppr {}

impl ExactCppr {
    /// Clock reconvergence pessimism removal for a corner's propagated clocks.
    pub fn new() -> Result<ExactCppr, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "clock reconvergence pessimism removal",
            planned: "E9",
        })
    }
}

impl Cppr for ExactCppr {
    fn credit(&self, _clock: ClockId, _launch: NodeId, _capture: NodeId) -> f64 {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_credit_with_ideal_clocks_and_exact_stub() {
        let c: &dyn Cppr = &NoCppr;
        assert_eq!(c.credit(ClockId(0), NodeId(4), NodeId(11)), 0.0);
        let e = ExactCppr::new().unwrap_err();
        assert_eq!(e.planned, "E9");
        assert_eq!(
            e.to_string(),
            "clock reconvergence pessimism removal: not supported yet (planned: E9)"
        );
    }
}
