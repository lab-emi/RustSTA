// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Propagated clocks, Phase 2b (owner: E9).
//!
//! Engine section G. The [`ClockModel`] of `set_propagated_clock`: the clock network is timed
//! like data, from the clock source through buffers, inverters and clock-gating cells; a clock
//! edge arrives at a pin at its waveform time plus the clock's source latency plus the propagated
//! network delay, which replaces the ideal network latency, with the propagated slew.

use super::{ClockArrival, ClockModel};
use crate::engine::types::{An, ClockId, NodeId, NotSupportedYet, Tr};

/// Propagated clocks. Not supported yet: the type has no values until work item E9 implements
/// it.
#[derive(Debug)]
pub enum PropagatedClocks {}

impl PropagatedClocks {
    /// The propagated clock network of a corner.
    pub fn new() -> Result<PropagatedClocks, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "propagated clocks",
            planned: "E9",
        })
    }
}

impl ClockModel for PropagatedClocks {
    fn scope_name(&self) -> &'static str {
        match *self {}
    }

    fn arrival(
        &self,
        _clock: ClockId,
        _edge: Tr,
        _pin: NodeId,
        _tr: Tr,
        _an: An,
    ) -> Option<ClockArrival> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagated_clocks_are_not_supported_yet() {
        let e = PropagatedClocks::new().unwrap_err();
        assert_eq!(e.planned, "E9");
        assert_eq!(
            e.to_string(),
            "propagated clocks: not supported yet (planned: E9)"
        );
    }
}
