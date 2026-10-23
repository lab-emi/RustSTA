// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Propagated and generated clocks and derates, Phase 2b (owner: R3).
//!
//! The checker times the clock network itself: propagated clock arrivals from the clock source
//! through the network, the waveforms and source latencies of generated clocks, and the delay
//! factors of `set_timing_derate`, written from the specification independently of the engine.

use super::Unsupported;

/// The timed clock network of a revision. Not supported yet: the type has no values until work
/// item R3 implements it.
#[derive(Debug)]
pub enum ClockNetwork {}

impl ClockNetwork {
    /// The clock network of the constraints' propagated and generated clocks.
    pub fn new() -> Result<ClockNetwork, Unsupported> {
        Err(Unsupported {
            feature: "propagated and generated clocks and derates",
            planned: "R3",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_networks_are_not_supported_yet() {
        let e = ClockNetwork::new().unwrap_err();
        assert_eq!(e.planned, "R3");
        assert_eq!(
            e.to_string(),
            "propagated and generated clocks and derates: not supported yet (planned: R3)"
        );
    }
}
