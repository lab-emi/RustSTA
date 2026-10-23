// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Wires from the revision's parasitics, Phase 2b (owner: E14).
//!
//! Engine section B. The wire model ([`WireModel`]) of the parasitics table that `rusteda import
//! --spef` commits: per net, the pi load and the Elmore delay to each load of its extracted RC
//! network.

use super::wire::{DriverLoad, WireModel};
use crate::engine::types::{An, DelaySlew, NetRow, NodeId, NotSupportedYet, Tr};

/// Wires from parasitics. Not supported yet: the type has no values until work item E14
/// implements it.
#[derive(Debug)]
pub enum Spef {}

impl Spef {
    /// The wire model of the revision's parasitics.
    pub fn new() -> Result<Spef, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "wires from parasitics (SPEF)",
            planned: "E14",
        })
    }
}

impl WireModel for Spef {
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
    fn parasitics_are_not_supported_yet() {
        let e = Spef::new().unwrap_err();
        assert_eq!(e.planned, "E14");
        assert_eq!(
            e.to_string(),
            "wires from parasitics (SPEF): not supported yet (planned: E14)"
        );
    }
}
