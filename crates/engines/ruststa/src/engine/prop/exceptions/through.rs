// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Timing exceptions with `-through`, Phase 2b (owner: E13).
//!
//! Engine section F. A path matches an exception with `-through` points when it passes them in
//! order. The exception state of a tag advances where a path passes the next point: a tag-mapping
//! arc of the compute timing task, under the tag budget.

use crate::engine::types::NotSupportedYet;

/// The `-through` points of the compiled exceptions. Not supported yet: the type has no values
/// until work item E13 implements it.
#[derive(Debug)]
pub enum ThroughPoints {}

impl ThroughPoints {
    /// The through-state machine of the compiled exceptions.
    pub fn new() -> Result<ThroughPoints, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "timing exceptions with -through",
            planned: "E13",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn through_exceptions_are_not_supported_yet() {
        let e = ThroughPoints::new().unwrap_err();
        assert_eq!(e.planned, "E13");
        assert_eq!(
            e.to_string(),
            "timing exceptions with -through: not supported yet (planned: E13)"
        );
    }
}
