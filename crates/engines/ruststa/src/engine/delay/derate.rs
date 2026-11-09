// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Global delay scaling for static on-chip variation.
//!
//! Engine section G. A [`Derate`] scales each cell and net delay by a factor that depends on the
//! bound, the transition, and whether the delay lies on a clock or a data path.
//! [`TimingDerate`] implements global early/late factors; [`NoDerate`] leaves delays unchanged.

use crate::engine::types::{An, NodeId, Tr};
use rusteda_core::job::Diag;

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

/// Global early/late factors applied to cell and net delay, never to slew or checks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingDerate {
    factors: [f64; 2],
}

impl Default for TimingDerate {
    fn default() -> Self {
        Self { factors: [1.0; 2] }
    }
}

impl TimingDerate {
    /// Assigns the factor for one analysis bound.
    ///
    /// # Errors
    /// Returns `D0005` when the factor is not finite and positive.
    pub fn set(&mut self, bound: An, factor: f64) -> Result<(), Diag> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(Diag::error(
                "D0005",
                "timing derate must be finite and positive",
            ));
        }
        self.factors[bound.index()] = factor;
        Ok(())
    }

    /// True when both factors leave all delays unchanged.
    pub fn is_identity(self) -> bool {
        self.factors == [1.0; 2]
    }

    /// The factor of a global analysis bound.
    pub fn for_bound(self, bound: An) -> f64 {
        self.factors[bound.index()]
    }
}

impl Derate for TimingDerate {
    fn factor(&self, _kind: DelayKind, _path: PathKind, _at: NodeId, an: An, _tr: Tr) -> f64 {
        self.for_bound(an)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_derates_are_positive_and_do_not_depend_on_path() {
        let mut d = TimingDerate::default();
        assert!(d.is_identity());
        d.set(An::Early, 0.95).unwrap();
        d.set(An::Late, 1.05).unwrap();
        for kind in [DelayKind::Cell, DelayKind::Net] {
            for path in [PathKind::Clock, PathKind::Data] {
                for tr in Tr::ALL {
                    assert_eq!(d.factor(kind, path, NodeId(5), An::Early, tr), 0.95);
                    assert_eq!(d.factor(kind, path, NodeId(5), An::Late, tr), 1.05);
                    assert_eq!(NoDerate.factor(kind, path, NodeId(5), An::Early, tr), 1.0);
                }
            }
        }
        for factor in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(d.set(An::Early, factor).is_err());
        }
    }
}
