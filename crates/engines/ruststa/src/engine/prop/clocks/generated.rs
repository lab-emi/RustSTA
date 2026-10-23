// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Generated clocks, Phase 2b (owner: E9).
//!
//! Engine section G. The clocks of `create_generated_clock`: a waveform derived from a master
//! clock (division, multiplication, edge lists, inversion) at a pin of the master's network, with
//! the source latency of the path from the master's source to that pin.

use crate::engine::types::NotSupportedYet;

/// The generated clocks of the compiled constraints. Not supported yet: the type has no values
/// until work item E9 implements it.
#[derive(Debug)]
pub enum GeneratedClocks {}

impl GeneratedClocks {
    /// Derives the generated clocks from their master clocks.
    pub fn new() -> Result<GeneratedClocks, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "generated clocks",
            planned: "E9",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_clocks_are_not_supported_yet() {
        let e = GeneratedClocks::new().unwrap_err();
        assert_eq!(e.planned, "E9");
        assert_eq!(
            e.to_string(),
            "generated clocks: not supported yet (planned: E9)"
        );
    }
}
