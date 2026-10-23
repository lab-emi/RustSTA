// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The derate seam: delay scaling for on-chip variation, Phase 2b (owner: E9).
//!
//! Engine section G. A [`Derate`] scales each cell and net delay by a factor that depends on the
//! bound, the transition, and whether the delay lies on a clock or a data path. Phase 2a has no
//! derates ([`NoDerate`]: every factor is 1); the factors of `set_timing_derate`
//! ([`TimingDerate`]) follow in Phase 2b.

use crate::engine::types::{An, NodeId, NotSupportedYet, Tr};

/// The delays that a derate factor scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DelayKind {
    /// The delay of a cell arc.
    Cell,
    /// The delay of a net from its driver to a load.
    Net,
}

/// Whether a delay lies on a clock path or on a data path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    Clock,
    Data,
}

/// Delay derating.
pub trait Derate {
    /// The factor that scales a delay of `kind` that ends at node `at` on a `path` path, for the
    /// bound `an` and the transition `tr` at `at`.
    fn factor(&self, kind: DelayKind, path: PathKind, at: NodeId, an: An, tr: Tr) -> f64;
}

/// No derating: every factor is 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDerate;

impl Derate for NoDerate {
    fn factor(&self, _kind: DelayKind, _path: PathKind, _at: NodeId, _an: An, _tr: Tr) -> f64 {
        1.0
    }
}

/// The derate factors of `set_timing_derate`. Not supported yet: the type has no values until
/// work item E9 implements it.
#[derive(Debug)]
pub enum TimingDerate {}

impl TimingDerate {
    /// The derates of the compiled constraints.
    pub fn new() -> Result<TimingDerate, NotSupportedYet> {
        Err(NotSupportedYet {
            feature: "timing derates (set_timing_derate)",
            planned: "E9",
        })
    }
}

impl Derate for TimingDerate {
    fn factor(&self, _kind: DelayKind, _path: PathKind, _at: NodeId, _an: An, _tr: Tr) -> f64 {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_derate_and_derate_stub() {
        let d: &dyn Derate = &NoDerate;
        for kind in [DelayKind::Cell, DelayKind::Net] {
            for path in [PathKind::Clock, PathKind::Data] {
                for an in An::ALL {
                    for tr in Tr::ALL {
                        assert_eq!(d.factor(kind, path, NodeId(5), an, tr), 1.0);
                    }
                }
            }
        }
        let e = TimingDerate::new().unwrap_err();
        assert_eq!(e.planned, "E9");
        assert_eq!(
            e.to_string(),
            "timing derates (set_timing_derate): not supported yet (planned: E9)"
        );
    }
}
