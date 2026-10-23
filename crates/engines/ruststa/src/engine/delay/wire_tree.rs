// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Tree wire-load estimation, Phase 2b (owner: E14).
//!
//! Engine section B. The tree model of the library's wire-load tables behind the wire model seam
//! ([`WireModel`]): a pi load per net with the resistance R/N^2 for N loads, the Elmore delay to
//! each load, and slew degradation with the corner's slew thresholds and slew derate.

use super::wire::{DriverLoad, WireModel};
use crate::engine::types::{An, DelaySlew, NetRow, NodeId, NotSupportedYet, Tr};

/// Tree wire-load estimation. Not supported yet: the type has no values until work item E14
/// implements it.
#[derive(Debug)]
pub enum WireloadTree {}

impl WireloadTree {
    /// The tree wire-load model of a corner.
    pub fn new() -> Result<WireloadTree, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "tree wire-load estimation",
            planned: "E14",
        })
    }
}

impl WireModel for WireloadTree {
    fn scope_name(&self) -> &'static str {
        match *self {}
    }

    fn driver_load(&self, _net: NetRow, _an: An, _pin_load: f64) -> DriverLoad {
        match *self {}
    }

    fn load_delay(&self, _net: NetRow, _load: NodeId, _tr: Tr, _an: An, _slew: f64) -> DelaySlew {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_wire_load_is_not_supported_yet() {
        let e = WireloadTree::new().unwrap_err();
        assert_eq!(e.planned, "E14");
        assert_eq!(
            e.to_string(),
            "tree wire-load estimation: not supported yet (planned: E14)"
        );
    }
}
