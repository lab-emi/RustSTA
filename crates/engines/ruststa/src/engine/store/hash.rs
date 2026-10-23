// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Deterministic string table and open-addressed `NAME_HASH` / `INST_HASH` tables for `sta.rbin`
//! (`STA v2`, owner: E10; `STA-REQ-0315`, `WS-REQ-0030` to `WS-REQ-0037`).
//!
//! `NAME_HASH` (section 19) maps any port or instance-pin name (`"clk"`, `"bus[0]"`, `"u0/D"`) to
//! its [`NodeId`] in `O(1)` without scanning or allocating. Each bucket stores `(prefix_str,
//! leaf_str)` indices into `STR_OFFS` so that `"inst/pin"` names do not duplicate instance names
//! across pins in `STR_BYTES`, and any `NAME_HASH` bucket index can also format its canonical node
//! name in `O(1)`.
//!
//! `INST_HASH` (section 20) maps any instance name (`"u0"`) to its `(inst_str, cell_str,
//! pin_node_begin, pin_count)` in `O(1)` so `ruststa query instance <inst>` reads a single
//! contiguous slice of `NODE_REC`.

use rustc_hash::FxHashMap;
use xxhash_rust::xxh3::xxh3_64;

use crate::engine::store::layout::NO_INDEX;
use crate::engine::types::NodeId;

/// Number of `u32` words per bucket in `NAME_HASH` and `INST_HASH`.
pub const BUCKET_WORDS: usize = 4;

/// Largest cell-name string id an `INST_HASH` bucket packs (20 bits of its third word).
pub const INST_HASH_MAX_CELL_STR: u32 = 0x000F_FFFF;

/// Largest pin count of an instance an `INST_HASH` bucket packs (12 bits of its third word).
pub const INST_HASH_MAX_PIN_COUNT: u32 = 0x0FFF;

/// Builder for sections 1 (`STR_BYTES`) and 2 (`STR_OFFS`).
#[derive(Clone, Debug)]
pub struct StringTableBuilder {
    bytes: Vec<u8>,
    offs: Vec<u32>,
    dedup: FxHashMap<String, u32>,
}

impl Default for StringTableBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl StringTableBuilder {
    /// Creates a new string table builder with string `0` initialized to `""`.
    #[must_use]
    pub fn new() -> Self {
        let mut dedup = FxHashMap::default();
        dedup.insert(String::new(), 0);
        Self {
            bytes: Vec::new(),
            offs: vec![0, 0],
            dedup,
        }
    }

    /// Pre-allocates capacity for approximately `n_strings` and `n_bytes`.
    pub fn reserve(&mut self, n_strings: usize, n_bytes: usize) {
        self.offs.reserve(n_strings);
        self.bytes.reserve(n_bytes);
    }

    /// Interns `s` (deduplicating repeated strings) and returns its `u32` string index.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.dedup.get(s) {
            return id;
        }
        let id = self.append_raw(s);
        self.dedup.insert(s.to_owned(), id);
        id
    }

    /// Appends `s` directly without inserting into the deduplication map (used for unique
    /// instance or port names at 500k–1M cell scale).
    #[inline]
    pub fn append_raw(&mut self, s: &str) -> u32 {
        let id = (self.offs.len() - 1) as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.offs.push(self.bytes.len() as u32);
        id
    }

    /// Returns the UTF-8 slice for string index `id`.
    #[inline]
    #[must_use]
    pub fn get(&self, id: u32) -> Option<&str> {
        let idx = id as usize;
        let start = *self.offs.get(idx)? as usize;
        let end = *self.offs.get(idx + 1)? as usize;
        std::str::from_utf8(self.bytes.get(start..end)?).ok()
    }

    /// Consumes the builder and returns `(str_bytes, str_offs)`.
    #[must_use]
    pub fn finish(self) -> (Vec<u8>, Vec<u32>) {
        (self.bytes, self.offs)
    }
}

/// Zero-copy view over sections 1 (`STR_BYTES`) and 2 (`STR_OFFS`).
#[derive(Clone, Copy, Debug)]
pub struct StringTableView<'a> {
    bytes: &'a [u8],
    offs: &'a [u32],
}

impl<'a> StringTableView<'a> {
    /// Creates a view over `bytes` and `offs`, validating basic offset bounds in `O(1)`.
    #[inline]
    pub fn new(bytes: &'a [u8], offs: &'a [u32]) -> Result<Self, &'static str> {
        if offs.is_empty() {
            return Err("STR_OFFS is empty");
        }
        if offs[0] != 0 {
            return Err("STR_OFFS[0] must be 0");
        }
        let last = offs[offs.len() - 1] as usize;
        if last != bytes.len() {
            return Err("STR_OFFS last offset does not match STR_BYTES length");
        }
        Ok(Self { bytes, offs })
    }

    /// Number of strings in the table (`offs.len() - 1`).
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.offs.len().saturating_sub(1)
    }

    /// True if the table holds zero strings.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.offs.len() <= 1
    }

    /// Resolves string index `id` to `&'a str` in `O(1)`.
    #[inline]
    pub fn get(&self, id: u32) -> Result<&'a str, &'static str> {
        let idx = id as usize;
        let start = *self
            .offs
            .get(idx)
            .ok_or("string index out of bounds in STR_OFFS")? as usize;
        let end = *self
            .offs
            .get(idx + 1)
            .ok_or("string end offset out of bounds in STR_OFFS")? as usize;
        if start > end || end > self.bytes.len() {
            return Err("non-monotonic or out-of-bounds STR_OFFS range");
        }
        std::str::from_utf8(&self.bytes[start..end]).map_err(|_| "invalid UTF-8 in STR_BYTES")
    }
}

/// Computes `xxh3_64` of either a standalone name (`prefix == None`) or `"prefix/leaf"` without
/// heap allocation for names up to 256 bytes.
#[inline]
#[must_use]
pub fn hash_node_name(prefix: Option<&str>, leaf: &str) -> u64 {
    match prefix {
        None => xxh3_64(leaf.as_bytes()),
        Some(inst) => {
            let p = inst.as_bytes();
            let l = leaf.as_bytes();
            let total = p.len() + 1 + l.len();
            if total <= 256 {
                let mut buf = [0u8; 256];
                buf[..p.len()].copy_from_slice(p);
                buf[p.len()] = b'/';
                buf[p.len() + 1..total].copy_from_slice(l);
                xxh3_64(&buf[..total])
            } else {
                let mut v = Vec::with_capacity(total);
                v.extend_from_slice(p);
                v.push(b'/');
                v.extend_from_slice(l);
                xxh3_64(&v)
            }
        }
    }
}

#[inline]
fn next_table_capacity(count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    // Load factor <= 0.5 for fast O(1) linear probing.
    let needed = count.saturating_mul(2).max(16);
    needed.next_power_of_two()
}

/// Open-addressed hash table builder for section 19 (`NAME_HASH`).
#[derive(Clone, Debug)]
pub struct NameHashBuilder {
    words: Vec<u32>,
    mask: usize,
}

impl NameHashBuilder {
    /// Creates a new `NAME_HASH` table sized for `num_nodes` entries.
    #[must_use]
    pub fn with_capacity(num_nodes: usize) -> Self {
        let cap = next_table_capacity(num_nodes);
        let mut words = vec![0u32; cap * BUCKET_WORDS];
        for b in 0..cap {
            words[b * BUCKET_WORDS + 1] = NO_INDEX;
            words[b * BUCKET_WORDS + 2] = NO_INDEX;
            words[b * BUCKET_WORDS + 3] = NO_INDEX;
        }
        Self {
            words,
            mask: cap.saturating_sub(1),
        }
    }

    /// Inserts a node (`prefix_str = NO_INDEX` for a port bit or standalone name; `prefix_str =
    /// inst_str` for an instance pin `inst/pin`) and returns its bucket index `0..capacity`.
    #[inline]
    pub fn insert(&mut self, hash64: u64, node: NodeId, prefix_str: u32, leaf_str: u32) -> u32 {
        if self.words.is_empty() {
            return NO_INDEX;
        }
        let hash_lo = (hash64 as u32).max(1);
        let mut slot = ((hash64 >> 32) as usize) & self.mask;
        loop {
            let base = slot * BUCKET_WORDS;
            if self.words[base] == 0 {
                self.words[base] = hash_lo;
                self.words[base + 1] = node.0;
                self.words[base + 2] = prefix_str;
                self.words[base + 3] = leaf_str;
                return slot as u32;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    /// Consumes the builder and returns the raw `u32` words for section `NAME_HASH`.
    #[must_use]
    pub fn finish(self) -> Vec<u32> {
        self.words
    }
}

/// Zero-copy view over section 19 (`NAME_HASH`).
#[derive(Clone, Copy, Debug)]
pub struct NameHashView<'a> {
    words: &'a [u32],
    mask: usize,
}

impl<'a> NameHashView<'a> {
    /// Wraps a `NAME_HASH` slice, validating that its bucket count is zero or a power of two.
    #[inline]
    pub fn new(words: &'a [u32]) -> Result<Self, &'static str> {
        if !words.len().is_multiple_of(BUCKET_WORDS) {
            return Err("NAME_HASH word count is not a multiple of 4");
        }
        let cap = words.len() / BUCKET_WORDS;
        if cap != 0 && !cap.is_power_of_two() {
            return Err("NAME_HASH bucket count is not a power of two");
        }
        Ok(Self {
            words,
            mask: cap.saturating_sub(1),
        })
    }

    /// Looks up a port or instance-pin name in `O(1)` and returns `(NodeId, bucket_idx)`.
    #[inline]
    pub fn lookup(
        &self,
        name: &str,
        strings: &StringTableView<'_>,
    ) -> Result<Option<(NodeId, u32)>, &'static str> {
        if self.words.is_empty() {
            return Ok(None);
        }
        let h = xxh3_64(name.as_bytes());
        let hash_lo = (h as u32).max(1);
        let mut slot = ((h >> 32) as usize) & self.mask;
        let cap = self.words.len() / BUCKET_WORDS;
        for _ in 0..cap {
            let base = slot * BUCKET_WORDS;
            let w0 = self.words[base];
            if w0 == 0 {
                return Ok(None);
            }
            if w0 == hash_lo {
                let node_id = self.words[base + 1];
                let prefix_str = self.words[base + 2];
                let leaf_str = self.words[base + 3];
                let matches = if prefix_str == NO_INDEX {
                    strings.get(leaf_str)? == name
                } else {
                    let inst = strings.get(prefix_str)?;
                    let pin = strings.get(leaf_str)?;
                    name.len() == inst.len() + 1 + pin.len()
                        && name.starts_with(inst)
                        && name.as_bytes()[inst.len()] == b'/'
                        && &name[inst.len() + 1..] == pin
                };
                if matches {
                    return Ok(Some((NodeId(node_id), slot as u32)));
                }
            }
            slot = (slot + 1) & self.mask;
        }
        Ok(None)
    }

    /// Formats the canonical name of the node at `bucket_idx` in `O(1)`.
    #[inline]
    pub fn format_bucket(
        &self,
        bucket_idx: u32,
        strings: &StringTableView<'_>,
    ) -> Result<String, &'static str> {
        let slot = bucket_idx as usize;
        let base = slot
            .checked_mul(BUCKET_WORDS)
            .ok_or("NAME_HASH bucket overflow")?;
        if base + BUCKET_WORDS > self.words.len() || self.words[base] == 0 {
            return Err("NAME_HASH bucket index out of bounds or empty");
        }
        let prefix_str = self.words[base + 2];
        let leaf_str = self.words[base + 3];
        let leaf = strings.get(leaf_str)?;
        if prefix_str == NO_INDEX {
            Ok(leaf.to_owned())
        } else {
            let inst = strings.get(prefix_str)?;
            let mut out = String::with_capacity(inst.len() + 1 + leaf.len());
            out.push_str(inst);
            out.push('/');
            out.push_str(leaf);
            Ok(out)
        }
    }

    /// Returns the leaf name (`pin_name` for an instance pin, `port_name` for a port) of
    /// `bucket_idx` in `O(1)`.
    #[inline]
    pub fn bucket_leaf<'s>(
        &self,
        bucket_idx: u32,
        strings: &StringTableView<'s>,
    ) -> Result<&'s str, &'static str> {
        let slot = bucket_idx as usize;
        let base = slot
            .checked_mul(BUCKET_WORDS)
            .ok_or("NAME_HASH bucket overflow")?;
        if base + BUCKET_WORDS > self.words.len() || self.words[base] == 0 {
            return Err("NAME_HASH bucket index out of bounds or empty");
        }
        strings.get(self.words[base + 3])
    }

    /// Finds the bucket index of `node` by scanning `NAME_HASH` (used only as a fallback when a
    /// caller did not supply `bucket_idx`).
    pub fn find_node_bucket(&self, node: NodeId) -> Option<u32> {
        let cap = self.words.len() / BUCKET_WORDS;
        for slot in 0..cap {
            let base = slot * BUCKET_WORDS;
            if self.words[base] != 0 && self.words[base + 1] == node.0 {
                return Some(slot as u32);
            }
        }
        None
    }
}

/// Decoded instance entry from `INST_HASH` (section 20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstHashEntry {
    pub inst_str: u32,
    pub cell_str: u32,
    pub pin_node_begin: u32,
    pub pin_count: u32,
}

/// Open-addressed hash table builder for section 20 (`INST_HASH`).
#[derive(Clone, Debug)]
pub struct InstHashBuilder {
    words: Vec<u32>,
    mask: usize,
}

impl InstHashBuilder {
    /// Creates a new `INST_HASH` table sized for `num_instances` entries.
    #[must_use]
    pub fn with_capacity(num_instances: usize) -> Self {
        let cap = next_table_capacity(num_instances);
        let mut words = vec![0u32; cap * BUCKET_WORDS];
        for b in 0..cap {
            words[b * BUCKET_WORDS + 1] = NO_INDEX;
        }
        Self {
            words,
            mask: cap.saturating_sub(1),
        }
    }

    /// Inserts an instance into `INST_HASH`.
    #[inline]
    pub fn insert(
        &mut self,
        inst_name: &str,
        inst_str: u32,
        cell_str: u32,
        pin_node_begin: u32,
        pin_count: u32,
    ) {
        if self.words.is_empty() {
            return;
        }
        let h = xxh3_64(inst_name.as_bytes());
        let hash_lo = (h as u32).max(1);
        // `check_store_tables` rejects tables whose ids exceed these fields before building.
        debug_assert!(cell_str <= INST_HASH_MAX_CELL_STR && pin_count <= INST_HASH_MAX_PIN_COUNT);
        let packed_cell_count =
            (cell_str & INST_HASH_MAX_CELL_STR) | (pin_count.min(INST_HASH_MAX_PIN_COUNT) << 20);
        let mut slot = ((h >> 32) as usize) & self.mask;
        loop {
            let base = slot * BUCKET_WORDS;
            if self.words[base] == 0 {
                self.words[base] = hash_lo;
                self.words[base + 1] = inst_str;
                self.words[base + 2] = packed_cell_count;
                self.words[base + 3] = pin_node_begin;
                return;
            }
            slot = (slot + 1) & self.mask;
        }
    }

    /// Consumes the builder and returns the raw `u32` words for section `INST_HASH`.
    #[must_use]
    pub fn finish(self) -> Vec<u32> {
        self.words
    }
}

/// Zero-copy view over section 20 (`INST_HASH`).
#[derive(Clone, Copy, Debug)]
pub struct InstHashView<'a> {
    words: &'a [u32],
    mask: usize,
}

impl<'a> InstHashView<'a> {
    /// Wraps an `INST_HASH` slice, validating that its bucket count is zero or a power of two.
    #[inline]
    pub fn new(words: &'a [u32]) -> Result<Self, &'static str> {
        if !words.len().is_multiple_of(BUCKET_WORDS) {
            return Err("INST_HASH word count is not a multiple of 4");
        }
        let cap = words.len() / BUCKET_WORDS;
        if cap != 0 && !cap.is_power_of_two() {
            return Err("INST_HASH bucket count is not a power of two");
        }
        Ok(Self {
            words,
            mask: cap.saturating_sub(1),
        })
    }

    /// Looks up an instance by name in `O(1)`.
    #[inline]
    pub fn lookup(
        &self,
        inst_name: &str,
        strings: &StringTableView<'_>,
    ) -> Result<Option<InstHashEntry>, &'static str> {
        if self.words.is_empty() {
            return Ok(None);
        }
        let h = xxh3_64(inst_name.as_bytes());
        let hash_lo = (h as u32).max(1);
        let mut slot = ((h >> 32) as usize) & self.mask;
        let cap = self.words.len() / BUCKET_WORDS;
        for _ in 0..cap {
            let base = slot * BUCKET_WORDS;
            let w0 = self.words[base];
            if w0 == 0 {
                return Ok(None);
            }
            if w0 == hash_lo {
                let inst_str = self.words[base + 1];
                if strings.get(inst_str)? == inst_name {
                    let w2 = self.words[base + 2];
                    let cell_str = w2 & 0x000F_FFFF;
                    let pin_count = w2 >> 20;
                    let pin_node_begin = self.words[base + 3];
                    return Ok(Some(InstHashEntry {
                        inst_str,
                        cell_str,
                        pin_node_begin,
                        pin_count,
                    }));
                }
            }
            slot = (slot + 1) & self.mask;
        }
        Ok(None)
    }
}
