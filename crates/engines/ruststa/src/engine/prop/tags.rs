// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Propagation tags: a launching clock edge with an exception state (owner: E5).
//!
//! Engine section C. Each node carries arrivals per tag ([`TagId`](super::super::types::TagId)),
//! stored densely per node. The number of tags is bounded by a budget; exceeding it is an error,
//! never a silent merge.

use rustc_hash::FxHashMap;
use rusteda_core::job::Diag;

use crate::engine::types::{ClockId, ExcStateId, TagId, Tr};

/// Default upper bound on the number of propagation tags in a single analysis.
pub const DEFAULT_TAG_BUDGET: usize = 4096;

/// A propagation tag: a launching clock edge paired with a timing-exception state
/// (`STA-REQ-0201`, `STA-REQ-0217`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag {
    /// The launching clock.
    pub clock: ClockId,
    /// The launching edge of `clock`'s waveform (`Rise` or `Fall`).
    pub edge: Tr,
    /// The active timing-exception state (`ExcStateId::NONE` when no path exception applies).
    pub exc_state: ExcStateId,
}

impl Tag {
    /// Creates a base tag `(clock, edge, ExcStateId::NONE)`.
    #[inline]
    #[must_use]
    pub const fn base(clock: ClockId, edge: Tr) -> Self {
        Self {
            clock,
            edge,
            exc_state: ExcStateId::NONE,
        }
    }

    /// Creates a tag `(clock, edge, exc_state)`.
    #[inline]
    #[must_use]
    pub const fn with_exc(clock: ClockId, edge: Tr, exc_state: ExcStateId) -> Self {
        Self {
            clock,
            edge,
            exc_state,
        }
    }
}

/// Dense hash-consed table of propagation [`Tag`]s with a hard budget (`STA-REQ-0201`).
#[derive(Debug, Clone)]
pub struct TagTable {
    tags: Vec<Tag>,
    by_tag: FxHashMap<Tag, TagId>,
    budget: usize,
}

impl Default for TagTable {
    fn default() -> Self {
        Self::with_budget(DEFAULT_TAG_BUDGET)
    }
}

impl TagTable {
    /// Creates an empty [`TagTable`] with the given maximum tag budget (`budget >= 1`).
    #[must_use]
    pub fn with_budget(budget: usize) -> Self {
        let budget = budget.max(1);
        Self {
            tags: Vec::new(),
            by_tag: FxHashMap::default(),
            budget,
        }
    }

    /// Builds a [`TagTable`] pre-populated with the `2 * num_clocks` base tags
    /// `(ClockId(c), Tr::Rise, ExcStateId::NONE)` and `(ClockId(c), Tr::Fall, ExcStateId::NONE)`
    /// in clock definition order (`tag = 2 * c + edge.index()`).
    pub fn for_clocks(num_clocks: usize) -> Result<Self, Diag> {
        Self::for_clocks_with_budget(num_clocks, DEFAULT_TAG_BUDGET)
    }

    /// Builds a [`TagTable`] for `num_clocks` clocks with an explicit `budget`.
    pub fn for_clocks_with_budget(num_clocks: usize, budget: usize) -> Result<Self, Diag> {
        let mut table = Self::with_budget(budget);
        for c in 0..num_clocks {
            let cid = ClockId(c as u32);
            for edge in Tr::ALL {
                table.intern(Tag::base(cid, edge))?;
            }
        }
        Ok(table)
    }

    /// Returns the tag budget.
    #[inline]
    #[must_use]
    pub const fn budget(&self) -> usize {
        self.budget
    }

    /// Number of interned tags.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// True when no tags have been interned.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Number of tag slots to allocate in the compute timing task (`>= 1`).
    #[inline]
    #[must_use]
    pub fn ntags_for_compute(&self) -> u32 {
        self.tags.len().max(1) as u32
    }

    /// Interns `tag`, returning its existing [`TagId`] or allocating a new one if within budget.
    /// Exceeding the budget returns an error (`T0020`) rather than silently merging tags.
    pub fn intern(&mut self, tag: Tag) -> Result<TagId, Diag> {
        if let Some(&id) = self.by_tag.get(&tag) {
            return Ok(id);
        }
        if self.tags.len() >= self.budget {
            return Err(Diag::error(
                "T0020",
                format!(
                    "propagation tag budget exceeded ({} tags, limit {})",
                    self.tags.len() + 1,
                    self.budget
                ),
            ));
        }
        let id = TagId(self.tags.len() as u32);
        self.tags.push(tag);
        self.by_tag.insert(tag, id);
        Ok(id)
    }

    /// Looks up `tag` without inserting it.
    #[inline]
    #[must_use]
    pub fn find(&self, tag: Tag) -> Option<TagId> {
        self.by_tag.get(&tag).copied()
    }

    /// Looks up the base tag `(clock, edge, ExcStateId::NONE)`.
    #[inline]
    #[must_use]
    pub fn base_tag(&self, clock: ClockId, edge: Tr) -> Option<TagId> {
        self.find(Tag::base(clock, edge))
    }

    /// Returns the [`Tag`] for `id`, if `id` is in range.
    #[inline]
    #[must_use]
    pub fn get(&self, id: TagId) -> Option<Tag> {
        self.tags.get(id.index()).copied()
    }

    /// Returns the [`Tag`] for `id`.
    #[inline]
    #[must_use]
    pub fn tag(&self, id: TagId) -> Tag {
        self.tags[id.index()]
    }

    /// Slice of all interned tags in [`TagId`] order.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[Tag] {
        &self.tags
    }

    /// Iterates over all `(TagId, Tag)` pairs in [`TagId`] order.
    pub fn iter(&self) -> impl Iterator<Item = (TagId, Tag)> + '_ {
        self.tags
            .iter()
            .copied()
            .enumerate()
            .map(|(i, t)| (TagId(i as u32), t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_interning_and_budget() {
        let mut table = TagTable::for_clocks_with_budget(2, 5).expect("4 base tags fit in 5");
        assert_eq!(table.len(), 4);
        assert_eq!(table.base_tag(ClockId(0), Tr::Rise), Some(TagId(0)));
        assert_eq!(table.base_tag(ClockId(0), Tr::Fall), Some(TagId(1)));
        assert_eq!(table.base_tag(ClockId(1), Tr::Rise), Some(TagId(2)));
        assert_eq!(table.base_tag(ClockId(1), Tr::Fall), Some(TagId(3)));

        // Re-interning an existing tag returns the same TagId without consuming budget.
        assert_eq!(
            table.intern(Tag::base(ClockId(1), Tr::Rise)).unwrap(),
            TagId(2)
        );

        // Interning a 5th tag succeeds.
        let t4 = table
            .intern(Tag::with_exc(ClockId(0), Tr::Rise, ExcStateId(1)))
            .unwrap();
        assert_eq!(t4, TagId(4));

        // Interning a 6th tag fails with T0020.
        let err = table
            .intern(Tag::with_exc(ClockId(0), Tr::Fall, ExcStateId(1)))
            .unwrap_err();
        assert_eq!(err.code, "T0020");
    }
}
