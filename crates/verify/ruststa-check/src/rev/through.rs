// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions with `-through`, Phase 2b (owner: R3).
//!
//! A path matches an exception with `-through` points when it passes them in order; the checker
//! matches each enumerated path explicitly, independently of the engine's through states.

use super::Unsupported;

/// The `-through` points of a revision's exceptions. Not supported yet: the type has no values
/// until work item R3 implements it.
#[derive(Debug)]
pub enum ThroughPaths {}

impl ThroughPaths {
    /// The path matcher of the constraints' `-through` points.
    pub fn new() -> Result<ThroughPaths, Unsupported> {
        Err(Unsupported {
            feature: "timing exceptions with -through",
            planned: "R3",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn through_exceptions_are_not_supported_yet() {
        let e = ThroughPaths::new().unwrap_err();
        assert_eq!(e.planned, "R3");
        assert_eq!(
            e.to_string(),
            "timing exceptions with -through: not supported yet (planned: R3)"
        );
    }
}
