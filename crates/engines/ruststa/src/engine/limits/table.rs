// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `limits` table of a revision (`limits.rbin`, kind `LIMITS`, version 1; `STA-REQ-0806`).
//!
//! The table holds, per corner, the counts of every check kind and the kept rows of the pin limit
//! checks with the names of their pin, net and clock, so that it is read without the timing
//! table. It is written by the command that writes `sta.rbin`, from the same analysis, and
//! depends on the same tables.
//!
//! | ID | Section | Record | Content |
//! |---:|---|---:|---|
//! | 1 | `STR_BYTES` | 1 | UTF-8 strings (string 0 is empty) |
//! | 2 | `STR_OFFS` | 4 | `u32` offsets, one more than the strings |
//! | 3 | `META` | 16 | [`LimitsMeta`] |
//! | 4 | `CORNERS` | 80 | [`LimitsCornerRec`] per corner |
//! | 5 | `KINDS` | 56 | [`LimitsKindRec`] per corner and kind (`corner * 6 + kind`) |
//! | 6 | `ROWS` | 48 | [`LimitsRowRec`] by corner, kind, slack, node |

use std::ops::RangeInclusive;

use bytemuck::{Pod, Zeroable};
use rusteda_workspace::rbin::Rbin;
use rusteda_workspace::{Corruption, Error, Hash64, RbinWriter, Result, Revision, Txn, Upstream};
use serde_json::json;

use crate::engine::analysis::EngineAnalysis;
use crate::engine::input::NetTarget;
use crate::engine::store::hash::{StringTableBuilder, StringTableView};
use crate::engine::types::{NodeId, Tr};

use super::{
    ClockLimitScope, CornerLimits, KindSummary, LimitKind, LimitRow, LimitSource, PulseRules,
};

/// The `rbin` container kind of the table.
pub const KIND: &str = "LIMITS";
/// The format version of the table.
pub const FORMAT_VERSION: u32 = 1;
/// Supported format versions.
pub const VERSIONS: RangeInclusive<u32> = FORMAT_VERSION..=FORMAT_VERSION;
/// The workspace table name.
pub const TABLE_NAME: &str = "limits";
/// The file name of the table in a revision.
pub const TABLE_PATH: &str = "limits.rbin";

mod section {
    pub const STR_BYTES: u32 = 1;
    pub const STR_OFFS: u32 = 2;
    pub const META: u32 = 3;
    pub const CORNERS: u32 = 4;
    pub const KINDS: u32 = 5;
    pub const ROWS: u32 = 6;
}

const NUM_KINDS: usize = LimitKind::ALL.len();

/// The counts of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct LimitsMeta {
    /// Corners.
    pub num_corners: u32,
    /// Kept rows of all corners.
    pub num_rows: u32,
    /// The row budget per corner and kind of the analysis.
    pub max_rows: u32,
    /// Zero.
    pub reserved: u32,
}

/// One corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct LimitsCornerRec {
    /// The corner name.
    pub name_str: u32,
    /// The rules of the corner's checks: the pulse rules ([`PulseRules::bits`], `STA-REQ-0816`,
    /// `STA-REQ-0817`) and the clock limit scope of the design rules
    /// ([`ClockLimitScope::bits`], `STA-REQ-0808`).
    pub pulse_rules: u32,
    /// Instance pins with pulse checks that no clock reaches.
    pub unclocked_pulse_pins: u64,
    /// Conditional pulse groups on the checked pins.
    pub conditional_pulse_groups: u64,
    /// Clocked instance pins with pulse constraints that pass their clock on, not checked under
    /// the scope `clock-ends` (`STA-REQ-0817`).
    pub pass_through_pulse_pins: u64,
    /// Drivers of constant nets, not checked for capacitance and fanout (`STA-REQ-0809`).
    pub constant_drivers: u64,
    /// Pins checked for transition with a static transition (`STA-REQ-0809`).
    pub static_transition_pins: u64,
    /// Pins and drivers of ideal clock networks, not checked for the design rules
    /// (`STA-REQ-0822`).
    pub ideal_clock_pins: u64,
    /// Pins without any transition, whose transition check is not evaluated (`STA-REQ-0809`).
    pub untimed_pins: u64,
    /// Checked pins of clock networks whose limit the clock limit scope decides
    /// (`STA-REQ-0808`).
    pub clock_scope_pins: u64,
    /// Unconnected instance output pins, not checked (`STA-REQ-0819`).
    pub unconnected_output_pins: u64,
}

/// The counts of one kind in one corner.
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct LimitsKindRec {
    /// Checked rows.
    pub checked: u64,
    /// Rows with a negative slack.
    pub violating: u64,
    /// Values without a limit.
    pub unlimited: u64,
    /// Kept rows.
    pub stored: u64,
    /// The index of the first kept row in `ROWS`.
    pub row_begin: u64,
    /// The worst slack, NaN without a checked row.
    pub worst_slack: f64,
    /// Checks that were not evaluated (a value, limit or slack that is not finite).
    pub unevaluated: u64,
}

/// One kept row.
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct LimitsRowRec {
    /// The checked value.
    pub value: f64,
    /// The limit.
    pub limit: f64,
    /// The slack.
    pub slack: f64,
    /// The node in the timing graph numbering (ports, then instance pins).
    pub node: u32,
    /// The pin or port name.
    pub pin_str: u32,
    /// The net of the pin (string 0 when the pin is on a constant).
    pub net_str: u32,
    /// The clock of a pulse or period check (string 0 otherwise).
    pub clock_str: u32,
    /// The corner.
    pub corner: u16,
    /// The [`LimitKind`].
    pub kind: u8,
    /// Bits 0 and 1: transition (0 rise, 1 fall, 2 none); bits 2 and 3: [`LimitSource`].
    pub flags: u8,
    /// Zero.
    pub reserved: u32,
}

/// The encoded table of an analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsData {
    str_bytes: Vec<u8>,
    str_offs: Vec<u32>,
    meta: [LimitsMeta; 1],
    corners: Vec<LimitsCornerRec>,
    kinds: Vec<LimitsKindRec>,
    rows: Vec<LimitsRowRec>,
}

fn encode_flags(row: &LimitRow) -> u8 {
    let tr = match row.tr {
        Some(Tr::Rise) => 0,
        Some(Tr::Fall) => 1,
        None => 2,
    };
    tr | ((row.source as u8) << 2)
}

impl LimitsData {
    /// The table of `analysis` with the row budget `max_rows` it ran with.
    #[must_use]
    pub fn from_analysis(analysis: &EngineAnalysis<'_>, max_rows: usize) -> Self {
        let nv = &analysis.netview;
        let mut strings = StringTableBuilder::new();
        let mut corners = Vec::with_capacity(analysis.corners.len());
        let mut kinds = Vec::with_capacity(analysis.corners.len() * NUM_KINDS);
        let mut rows = Vec::new();
        for (c_idx, ca) in analysis.corners.iter().enumerate() {
            let l = &ca.limits;
            corners.push(LimitsCornerRec {
                name_str: strings.intern(&ca.corner_name),
                pulse_rules: l.rules.bits() | l.clock_limits.bits(),
                unclocked_pulse_pins: l.unclocked_pulse_pins,
                conditional_pulse_groups: l.conditional_pulse_groups,
                pass_through_pulse_pins: l.pass_through_pulse_pins,
                constant_drivers: l.constant_drivers,
                static_transition_pins: l.static_transition_pins,
                ideal_clock_pins: l.ideal_clock_pins,
                untimed_pins: l.untimed_pins,
                clock_scope_pins: l.clock_scope_pins,
                unconnected_output_pins: l.unconnected_output_pins,
            });
            for kind in LimitKind::ALL {
                let k = l.kind(kind);
                let row_begin = rows.len() as u64;
                for r in l.rows_of(kind) {
                    let net_str = match nv.node_net(r.node) {
                        NetTarget::Net(net) => strings.intern(&nv.net_name_str(net)),
                        NetTarget::Const(_) => 0,
                    };
                    let clock_str = r.clock.map_or(0, |c| {
                        analysis
                            .constraints
                            .clocks
                            .get(c.index())
                            .map_or(0, |ck| strings.intern(&ck.name))
                    });
                    rows.push(LimitsRowRec {
                        value: r.value,
                        limit: r.limit,
                        slack: r.slack,
                        node: r.node.0,
                        pin_str: strings.intern(&nv.node_name_str(r.node)),
                        net_str,
                        clock_str,
                        corner: c_idx as u16,
                        kind: kind as u8,
                        flags: encode_flags(r),
                        reserved: 0,
                    });
                }
                kinds.push(LimitsKindRec {
                    checked: k.checked,
                    violating: k.violating,
                    unlimited: k.unlimited,
                    stored: k.stored,
                    row_begin,
                    worst_slack: k.worst_slack.unwrap_or(f64::NAN),
                    unevaluated: k.unevaluated,
                });
            }
        }
        let (str_bytes, str_offs) = strings.finish();
        Self {
            str_bytes,
            str_offs,
            meta: [LimitsMeta {
                num_corners: corners.len() as u32,
                num_rows: rows.len() as u32,
                max_rows: max_rows.min(u32::MAX as usize) as u32,
                reserved: 0,
            }],
            corners,
            kinds,
            rows,
        }
    }

    /// The `rbin` writer of the table.
    pub fn writer(&self) -> Result<RbinWriter<'_>> {
        let mut w = RbinWriter::new(KIND, FORMAT_VERSION)?;
        w.section_bytes(section::STR_BYTES, 1, &self.str_bytes)?;
        w.section::<u32>(section::STR_OFFS, &self.str_offs)?;
        w.section::<LimitsMeta>(section::META, &self.meta)?;
        w.section::<LimitsCornerRec>(section::CORNERS, &self.corners)?;
        w.section::<LimitsKindRec>(section::KINDS, &self.kinds)?;
        w.section::<LimitsRowRec>(section::ROWS, &self.rows)?;
        Ok(w)
    }

    /// The deterministic bytes of the table without upstream entries.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(self.writer()?.to_bytes())
    }

    /// Stages the table as `limits` (`limits.rbin`) with `upstream`, and its summary values
    /// `limit_rows` and `limit_rows_truncated` (`STA-REQ-0806`).
    pub fn stage_to_txn(&self, txn: &mut Txn, upstream: Upstream) -> Result<Hash64> {
        let w = self.writer()?;
        let hash = txn.add_rbin(TABLE_NAME, TABLE_PATH, &w, upstream)?;
        let truncated: u64 = self.kinds.iter().map(|k| k.checked - k.stored).sum();
        txn.set_summary(TABLE_NAME, "limit_rows", json!(self.rows.len()))?;
        txn.set_summary(TABLE_NAME, "limit_rows_truncated", json!(truncated))?;
        Ok(hash)
    }
}

/// The counts of every kind over the corners: the worst corner of each count
/// (`STA-REQ-0807`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LimitTotals {
    /// Per kind, [`LimitKind::index`]: checked rows of the corner with the most.
    pub checked: [u64; NUM_KINDS],
    /// Per kind: violating rows of the worst corner.
    pub violating: [u64; NUM_KINDS],
    /// Per kind: the worst slack over the corners.
    pub worst_slack: [Option<f64>; NUM_KINDS],
    /// Per kind: values without a limit, of the corner with the most.
    pub unlimited: [u64; NUM_KINDS],
    /// Instance pins with pulse checks reached by no clock, of the corner with the most.
    pub unclocked_pulse_pins: u64,
}

impl LimitTotals {
    /// The totals of `corners`.
    #[must_use]
    pub fn of<'a>(corners: impl IntoIterator<Item = &'a CornerLimits>) -> Self {
        let mut t = Self::default();
        for c in corners {
            for kind in LimitKind::ALL {
                let i = kind.index();
                let k: &KindSummary = c.kind(kind);
                t.checked[i] = t.checked[i].max(k.checked);
                t.violating[i] = t.violating[i].max(k.violating);
                t.unlimited[i] = t.unlimited[i].max(k.unlimited);
                if let Some(w) = k.worst_slack {
                    t.worst_slack[i] = Some(t.worst_slack[i].map_or(w, |p: f64| p.min(w)));
                }
            }
            t.unclocked_pulse_pins = t.unclocked_pulse_pins.max(c.unclocked_pulse_pins);
        }
        t
    }

    /// Violating rows of the design rules (transition, capacitance and fanout).
    #[must_use]
    pub fn drv_violating(&self) -> u64 {
        LimitKind::ALL
            .iter()
            .filter(|k| k.is_drv())
            .map(|k| self.violating[k.index()])
            .sum()
    }

    /// Violating pulse width and period rows.
    #[must_use]
    pub fn pulse_violating(&self) -> u64 {
        LimitKind::ALL
            .iter()
            .filter(|k| !k.is_drv())
            .map(|k| self.violating[k.index()])
            .sum()
    }
}

/// One row read from the table.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitRowView<'a> {
    /// The corner name.
    pub corner: &'a str,
    /// The check.
    pub kind: LimitKind,
    /// The pin or port.
    pub pin: &'a str,
    /// The node of the pin.
    pub node: NodeId,
    /// The net of the pin, `None` on a constant.
    pub net: Option<&'a str>,
    /// The clock of a pulse or period check.
    pub clock: Option<&'a str>,
    /// The transition of the worst slack.
    pub tr: Option<Tr>,
    /// The checked value.
    pub value: f64,
    /// The limit.
    pub limit: f64,
    /// The slack.
    pub slack: f64,
    /// Where the limit comes from.
    pub source: LimitSource,
}

/// The counts of one kind in one corner, read from the table.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitKindView<'a> {
    /// The corner name.
    pub corner: &'a str,
    /// The check.
    pub kind: LimitKind,
    /// The counts.
    pub summary: KindSummary,
}

/// A validated `limits` table.
#[derive(Debug)]
pub struct LimitsTable {
    rbin: Rbin,
}

fn corrupt(rbin: &Rbin, reason: &str) -> Error {
    Error::corrupt(
        rbin.path(),
        Corruption::Json {
            reason: format!("limits table: {reason}"),
        },
    )
}

impl LimitsTable {
    /// The `limits` table of `rev`, `None` when the revision has none.
    pub fn open_rev(rev: &Revision) -> Result<Option<Self>> {
        if !rev.has_table(TABLE_NAME) {
            return Ok(None);
        }
        let rbin = rev.open_rbin(TABLE_NAME, KIND, VERSIONS)?;
        Self::from_rbin(rbin).map(Some)
    }

    /// Validates `rbin` as a `limits` table.
    pub fn from_rbin(rbin: Rbin) -> Result<Self> {
        let t = Self { rbin };
        let strings = t.strings()?;
        let meta = t.meta()?;
        let corners = t.section::<LimitsCornerRec>(section::CORNERS)?;
        let kinds = t.section::<LimitsKindRec>(section::KINDS)?;
        let rows = t.section::<LimitsRowRec>(section::ROWS)?;
        let n_str = strings.len() as u32;
        if corners.len() != meta.num_corners as usize
            || kinds.len() != corners.len() * NUM_KINDS
            || rows.len() != meta.num_rows as usize
        {
            return Err(corrupt(&t.rbin, "section lengths disagree with META"));
        }
        if corners.iter().any(|c| c.name_str >= n_str) {
            return Err(corrupt(&t.rbin, "corner name out of range"));
        }
        if corners
            .iter()
            .any(|c| PulseRules::from_bits(c.pulse_rules).is_none())
        {
            return Err(corrupt(&t.rbin, "unknown rule bits"));
        }
        let mut next = 0u64;
        for k in kinds {
            if k.row_begin != next || k.stored > k.checked || k.violating > k.checked {
                return Err(corrupt(
                    &t.rbin,
                    "kind counts or row ranges are inconsistent",
                ));
            }
            next += k.stored;
        }
        if next != rows.len() as u64 {
            return Err(corrupt(&t.rbin, "rows not covered by the kind ranges"));
        }
        for (i, r) in rows.iter().enumerate() {
            let range_ok = (r.corner as usize) < corners.len()
                && LimitKind::from_u8(r.kind).is_some()
                && (r.flags & 3) <= 2
                && LimitSource::from_u8(r.flags >> 2).is_some()
                && r.pin_str < n_str
                && r.net_str < n_str
                && r.clock_str < n_str;
            let k = &kinds[r.corner as usize * NUM_KINDS + r.kind as usize];
            let in_range = (i as u64) >= k.row_begin && (i as u64) < k.row_begin + k.stored;
            if !range_ok || !in_range {
                return Err(corrupt(&t.rbin, "row field out of range"));
            }
        }
        Ok(t)
    }

    fn section<T: Pod>(&self, id: u32) -> Result<&[T]> {
        self.rbin.section::<T>(id)
    }

    fn strings(&self) -> Result<StringTableView<'_>> {
        let bytes = self.rbin.section_bytes(section::STR_BYTES)?;
        let offs = self.section::<u32>(section::STR_OFFS)?;
        StringTableView::new(bytes, offs).map_err(|e| corrupt(&self.rbin, e))
    }

    fn str(&self, id: u32) -> Result<&str> {
        self.strings()?.get(id).map_err(|e| corrupt(&self.rbin, e))
    }

    fn meta(&self) -> Result<LimitsMeta> {
        self.section::<LimitsMeta>(section::META)?
            .first()
            .copied()
            .ok_or_else(|| corrupt(&self.rbin, "no META record"))
    }

    /// The row budget per corner and kind of the analysis.
    pub fn max_rows(&self) -> Result<u32> {
        Ok(self.meta()?.max_rows)
    }

    /// The corner names in table order.
    pub fn corners(&self) -> Result<Vec<&str>> {
        self.section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| self.str(c.name_str))
            .collect()
    }

    /// Instance pins with pulse checks reached by no clock, per corner.
    pub fn unclocked_pulse_pins(&self) -> Result<Vec<u64>> {
        Ok(self
            .section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| c.unclocked_pulse_pins)
            .collect())
    }

    /// The pulse rules of each corner's checks (`STA-REQ-0816`, `STA-REQ-0817`).
    pub fn pulse_rules(&self) -> Result<Vec<PulseRules>> {
        self.section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| {
                PulseRules::from_bits(c.pulse_rules)
                    .ok_or_else(|| corrupt(&self.rbin, "unknown pulse rule bits"))
            })
            .collect()
    }

    /// The clock limit scope of each corner's design rules (`STA-REQ-0808`).
    pub fn clock_limits(&self) -> Result<Vec<ClockLimitScope>> {
        Ok(self
            .section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| ClockLimitScope::from_bits(c.pulse_rules))
            .collect())
    }

    /// Per corner, the drivers of constant nets left out of the capacitance and fanout checks,
    /// the pins checked with a static transition, the pins of ideal clock networks, the untimed
    /// pins, the clock network pins whose limit the clock limit scope decides and the
    /// unconnected instance output pins (`STA-REQ-0809`, `STA-REQ-0822`, `STA-REQ-0808`,
    /// `STA-REQ-0819`).
    pub fn drv_pin_counts(&self) -> Result<Vec<[u64; 6]>> {
        Ok(self
            .section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| {
                [
                    c.constant_drivers,
                    c.static_transition_pins,
                    c.ideal_clock_pins,
                    c.untimed_pins,
                    c.clock_scope_pins,
                    c.unconnected_output_pins,
                ]
            })
            .collect())
    }

    /// Clocked pins with pulse constraints left out by the scope `clock-ends`, per corner
    /// (`STA-REQ-0817`).
    pub fn pass_through_pulse_pins(&self) -> Result<Vec<u64>> {
        Ok(self
            .section::<LimitsCornerRec>(section::CORNERS)?
            .iter()
            .map(|c| c.pass_through_pulse_pins)
            .collect())
    }

    /// The counts of every kind of every corner (`corner` only, when given).
    pub fn kinds(&self, corner: Option<usize>) -> Result<Vec<LimitKindView<'_>>> {
        let corners = self.section::<LimitsCornerRec>(section::CORNERS)?;
        let kinds = self.section::<LimitsKindRec>(section::KINDS)?;
        let mut out = Vec::new();
        for (c_idx, c) in corners.iter().enumerate() {
            if corner.is_some_and(|w| w != c_idx) {
                continue;
            }
            let name = self.str(c.name_str)?;
            for kind in LimitKind::ALL {
                let k = &kinds[c_idx * NUM_KINDS + kind.index()];
                out.push(LimitKindView {
                    corner: name,
                    kind,
                    summary: KindSummary {
                        checked: k.checked,
                        violating: k.violating,
                        worst_slack: (!k.worst_slack.is_nan()).then_some(k.worst_slack),
                        unlimited: k.unlimited,
                        stored: k.stored,
                        unevaluated: k.unevaluated,
                    },
                });
            }
        }
        Ok(out)
    }

    /// The kept rows of `kind` in `corner`, worst slack first.
    pub fn rows(&self, corner: usize, kind: LimitKind) -> Result<Vec<LimitRowView<'_>>> {
        let corners = self.section::<LimitsCornerRec>(section::CORNERS)?;
        let kinds = self.section::<LimitsKindRec>(section::KINDS)?;
        let rows = self.section::<LimitsRowRec>(section::ROWS)?;
        let Some(c) = corners.get(corner) else {
            return Ok(Vec::new());
        };
        let k = &kinds[corner * NUM_KINDS + kind.index()];
        let corner_name = self.str(c.name_str)?;
        let begin = k.row_begin as usize;
        let end = begin + k.stored as usize;
        rows[begin..end]
            .iter()
            .map(|r| {
                let opt = |id: u32| -> Result<Option<&str>> {
                    if id == 0 {
                        Ok(None)
                    } else {
                        self.str(id).map(Some)
                    }
                };
                Ok(LimitRowView {
                    corner: corner_name,
                    kind,
                    pin: self.str(r.pin_str)?,
                    node: NodeId(r.node),
                    net: opt(r.net_str)?,
                    clock: opt(r.clock_str)?,
                    tr: match r.flags & 3 {
                        0 => Some(Tr::Rise),
                        1 => Some(Tr::Fall),
                        _ => None,
                    },
                    value: r.value,
                    limit: r.limit,
                    slack: r.slack,
                    source: LimitSource::from_u8(r.flags >> 2).unwrap_or(LimitSource::Liberty),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_sizes_are_fixed() {
        assert_eq!(size_of::<LimitsMeta>(), 16);
        assert_eq!(size_of::<LimitsCornerRec>(), 80);
        assert_eq!(size_of::<LimitsKindRec>(), 56);
        assert_eq!(size_of::<LimitsRowRec>(), 48);
    }
}
