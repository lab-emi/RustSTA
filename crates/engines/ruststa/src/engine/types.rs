// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Shared types of the engine: transitions, analysis bounds, indices and unsupported features
//! (owner: E5, from E0).
//!
//! Values per transition and bound are `[[T; 2]; 2]` arrays indexed `[tr][an]` with
//! [`Tr::index`] and [`An::index`] (STA-REQ-0101): rise 0, fall 1, early 0, late 1, the layout of
//! the legacy engine and of the propagation state of the compute timing task. Times are in ns,
//! capacitances in pF and resistances in kOhm.
//!
//! Other work items request changes to these types from the owner.

/// A signal transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tr {
    Rise = 0,
    Fall = 1,
}

impl Tr {
    /// Both transitions, in index order.
    pub const ALL: [Tr; 2] = [Tr::Rise, Tr::Fall];

    /// The index of the transition in per-transition arrays.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The other transition.
    pub fn opposite(self) -> Tr {
        match self {
            Tr::Rise => Tr::Fall,
            Tr::Fall => Tr::Rise,
        }
    }
}

/// An analysis bound: the early (minimum) or the late (maximum) values. Hold checks compare early
/// data arrivals, setup checks late ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum An {
    Early = 0,
    Late = 1,
}

impl An {
    /// Both bounds, in index order.
    pub const ALL: [An; 2] = [An::Early, An::Late];

    /// The index of the bound in per-bound arrays.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The other bound.
    pub fn opposite(self) -> An {
        match self {
            An::Early => An::Late,
            An::Late => An::Early,
        }
    }

    /// The sentinel value marking an absent arrival, required time, or slew for this bound
    /// (`+inf` for [`An::Early`], `-inf` for [`An::Late`]; `STA-REQ-0101`).
    #[inline]
    pub const fn absent(self) -> f64 {
        match self {
            An::Early => f64::INFINITY,
            An::Late => f64::NEG_INFINITY,
        }
    }

    /// True when `v` is a present (finite) value for this bound.
    #[inline]
    pub fn is_present(self, v: f64) -> bool {
        v.is_finite()
    }

    /// True when `a` is strictly tighter (more critical) than `b` for this bound (`a < b` for
    /// [`An::Early`], `a > b` for [`An::Late`]).
    #[inline]
    pub fn better(self, a: f64, b: f64) -> bool {
        match self {
            An::Early => a < b,
            An::Late => a > b,
        }
    }

    /// Merges two values for this bound (`min` for [`An::Early`], `max` for [`An::Late`]).
    #[inline]
    pub fn merge(self, a: f64, b: f64) -> f64 {
        if self.better(a, b) { a } else { b }
    }
}

/// Defines a `u32` index type with an `index()` accessor.
macro_rules! index_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            /// The index as a `usize`, for indexing tables.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index_type!(
    /// A node of the timing graph. The netlist's port bits come first, then its pin rows: with
    /// `P` port bits, node `n` is port bit `n` for `n < P` and pin row `n - P` otherwise.
    NodeId
);
index_type!(
    /// A net: its row in the `NETS` section of the netlist.
    NetRow
);
index_type!(
    /// A corner of the revision, in the order of the revision's corner list.
    CornerId
);
index_type!(
    /// A clock of the compiled constraints, in the order of definition.
    ClockId
);
index_type!(
    /// A propagation tag: a launching clock edge together with an exception state.
    TagId
);
index_type!(
    /// A hash-consed timing exception state carried by a propagation tag (`ExcStateId::NONE` = 0
    /// when no `-from` / `-through` exception is active).
    ExcStateId
);
index_type!(
    /// A timing arc of a cell in the library view of a corner, numbered by that view.
    LibArc
);

impl ExcStateId {
    /// The default exception state when no path exception has been entered (`0`).
    pub const NONE: ExcStateId = ExcStateId(0);
}

/// A delay and the slew at its end, in ns.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelaySlew {
    pub delay: f64,
    pub slew: f64,
}

/// A feature that the engine does not support yet: the error of a seam stub.
///
/// The work item that fills the seam replaces the stub. Until then, an analysis that needs the
/// feature fails with this error, which the caller reports as a diagnostic, so that the feature
/// is never silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{feature}: not supported yet (planned: {planned})")]
pub struct NotSupportedYet {
    /// The feature, for example `propagated clocks`.
    pub feature: &'static str,
    /// The roadmap id of the work item that implements it.
    pub planned: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    // STA-REQ-0101: values per transition (rise, fall) and bound (early, late); the indices are
    // those of the `[tr][an]` arrays.
    #[test]
    fn transition_and_bound_indices() {
        assert_eq!(Tr::ALL.map(Tr::index), [0, 1]);
        assert_eq!(An::ALL.map(An::index), [0, 1]);
        for tr in Tr::ALL {
            assert_ne!(tr.opposite(), tr);
            assert_eq!(tr.opposite().opposite(), tr);
        }
        for an in An::ALL {
            assert_ne!(an.opposite(), an);
            assert_eq!(an.opposite().opposite(), an);
            assert!(!an.is_present(an.absent()));
            assert!(an.is_present(0.0));
        }
        assert!(An::Early.better(1.0, 2.0));
        assert!(An::Late.better(2.0, 1.0));
        assert_eq!(An::Early.merge(1.0, An::Early.absent()), 1.0);
        assert_eq!(An::Late.merge(1.0, An::Late.absent()), 1.0);
        let mut cells = [[0u8; 2]; 2];
        for tr in Tr::ALL {
            for an in An::ALL {
                cells[tr.index()][an.index()] += 1;
            }
        }
        assert_eq!(cells, [[1, 1], [1, 1]]);
    }

    #[test]
    fn indices_and_unsupported_features() {
        assert_eq!(NodeId(7).index(), 7);
        assert_eq!(NetRow(u32::MAX).index(), u32::MAX as usize);
        assert_eq!(ExcStateId::NONE.index(), 0);
        assert!(ClockId(1) < ClockId(2));
        let e = NotSupportedYet {
            feature: "propagated clocks",
            planned: "E9",
        };
        assert_eq!(
            e.to_string(),
            "propagated clocks: not supported yet (planned: E9)"
        );
    }
}
