// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Clock reconvergence pessimism removal, Phase 2b (owner: R3).
//!
//! The checker enumerates the launching and the capturing clock paths of a check explicitly and
//! credits the difference of the late and the early arrival at their deepest shared node. With
//! ideal clocks the credit is 0, so the checker of Phase 2a needs no credit.

use super::Unsupported;

/// The credits of a revision's checks. Not supported yet: the type has no values until work item
/// R3 implements it.
#[derive(Debug)]
pub enum ClockPathCredit {}

impl ClockPathCredit {
    /// The credit computation of the timed clock network.
    pub fn new() -> Result<ClockPathCredit, Unsupported> {
        Err(Unsupported {
            feature: "clock reconvergence pessimism removal",
            planned: "R3",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_are_not_supported_yet() {
        let e = ClockPathCredit::new().unwrap_err();
        assert_eq!(e.planned, "R3");
        assert_eq!(
            e.to_string(),
            "clock reconvergence pessimism removal: not supported yet (planned: R3)"
        );
    }
}
