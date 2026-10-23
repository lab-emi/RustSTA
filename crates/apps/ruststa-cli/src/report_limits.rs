// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The pin limit sections of `ruststa report`: `limits`, `pulse_width`, `min_period`, `drv`,
//! `max_transition`, `max_capacitance` and `max_fanout` (`STA-REQ-0811`).
//!
//! The rows come from the `limits` table of the revision. Without `--corner` every corner is
//! reported; rows are ordered by slack (worst first), then corner and pin, unless `--sort`
//! asks for `pin`/`name` or `value` (aliases `slew`, `cap`, `fanout`, `period`).

use rusteda_workspace::{Error, JsonObject};
use ruststa::engine::limits::LimitKind;
use ruststa::engine::limits::table::{LimitRowView, LimitsTable};
use ruststa::engine::store::canon_f64;
use ruststa::engine::types::Tr;

use crate::ws_io::node_handle_string;

/// A filter of limit rows (`STA-REQ-0811`).
#[derive(Debug, Clone)]
pub(crate) enum LimitFilter {
    /// Negative slack.
    Violating,
    /// Slack below the value.
    SlackLt(f64),
    /// Pin, port or net name contains the text.
    NameContains(String),
    /// The clock of a pulse row is the name.
    ClockEq(String),
}

impl LimitFilter {
    fn keep(&self, r: &LimitRowView<'_>) -> bool {
        match self {
            Self::Violating => r.slack < 0.0,
            Self::SlackLt(t) => r.slack < *t,
            Self::NameContains(s) => {
                r.pin.contains(s.as_str()) || r.net.is_some_and(|n| n.contains(s.as_str()))
            }
            Self::ClockEq(c) => r.clock == Some(c.as_str()),
        }
    }
}

fn unit(kind: LimitKind) -> &'static str {
    match kind {
        LimitKind::MaxCapacitance => "pF",
        LimitKind::MaxFanout => "fanout_load",
        _ => "ns",
    }
}

fn tr_name(tr: Option<Tr>) -> Option<&'static str> {
    tr.map(|t| match t {
        Tr::Rise => "rise",
        Tr::Fall => "fall",
    })
}

/// The `limits` section: the counts per corner and kind.
pub(crate) fn limits_section(
    table: &LimitsTable,
    corner: Option<usize>,
) -> Result<JsonObject, Error> {
    let kinds = table.kinds(corner)?;
    let total = kinds.len();
    let mut rows = Vec::with_capacity(total);
    for k in &kinds {
        let s = &k.summary;
        let mut o = JsonObject::new();
        o.insert("corner", &k.corner)?;
        o.insert("kind", &k.kind.name())?;
        o.insert("checked", &s.checked)?;
        o.insert("violating", &s.violating)?;
        o.insert("worst_slack", &s.worst_slack.map(canon_f64))?;
        o.insert("unit", &unit(k.kind))?;
        o.insert("unlimited", &s.unlimited)?;
        o.insert("stored", &s.stored)?;
        o.insert("truncated", &s.truncated())?;
        rows.push(o);
    }
    crate::report::section_envelope_obj(
        &[
            "corner",
            "kind",
            "checked",
            "violating",
            "worst_slack",
            "unit",
            "unlimited",
            "stored",
            "truncated",
        ],
        total,
        total,
        rows,
    )
}

/// A row section over `kinds` (`pulse_width`, `min_period`, `drv` and one design rule).
#[allow(clippy::too_many_arguments)]
pub(crate) fn rows_section(
    table: &LimitsTable,
    kinds: &[LimitKind],
    corner: Option<usize>,
    num_port_bits: u32,
    filter: Option<&LimitFilter>,
    sort_key: Option<&str>,
    reverse: bool,
    limit: usize,
) -> Result<JsonObject, Error> {
    let n_corners = table.corners()?.len();
    let mut rows: Vec<LimitRowView<'_>> = Vec::new();
    let mut total = 0usize;
    for c in 0..n_corners {
        if corner.is_some_and(|w| w != c) {
            continue;
        }
        for &kind in kinds {
            let kept = table.rows(c, kind)?;
            total += kept.len();
            rows.extend(
                kept.into_iter()
                    .filter(|r| filter.is_none_or(|f| f.keep(r))),
            );
        }
    }
    match sort_key {
        Some("pin" | "name" | "endpoint") => rows.sort_by(|a, b| {
            a.pin
                .cmp(b.pin)
                .then_with(|| a.corner.cmp(b.corner))
                .then_with(|| a.kind.cmp(&b.kind))
        }),
        Some("value" | "slew" | "cap" | "fanout" | "period") => rows.sort_by(|a, b| {
            b.value
                .total_cmp(&a.value)
                .then_with(|| a.pin.cmp(b.pin))
                .then_with(|| a.corner.cmp(b.corner))
        }),
        _ => rows.sort_by(|a, b| {
            a.slack
                .total_cmp(&b.slack)
                .then_with(|| a.corner.cmp(b.corner))
                .then_with(|| a.pin.cmp(b.pin))
                .then_with(|| a.kind.cmp(&b.kind))
        }),
    }
    if reverse {
        rows.reverse();
    }
    let matched = rows.len();
    if limit > 0 && rows.len() > limit {
        rows.truncate(limit);
    }
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut o = JsonObject::new();
        o.insert("pin", &r.pin)?;
        o.insert("handle", &node_handle_string(r.node, num_port_bits))?;
        o.insert("corner", &r.corner)?;
        o.insert("kind", &r.kind.name())?;
        o.insert("net", &r.net)?;
        o.insert("clock", &r.clock)?;
        o.insert("transition", &tr_name(r.tr))?;
        o.insert("value", &canon_f64(r.value))?;
        o.insert("limit", &canon_f64(r.limit))?;
        o.insert("slack", &canon_f64(r.slack))?;
        o.insert("unit", &unit(r.kind))?;
        o.insert("source", &r.source.name())?;
        out.push(o);
    }
    crate::report::section_envelope_obj(
        &[
            "pin",
            "handle",
            "corner",
            "kind",
            "net",
            "clock",
            "transition",
            "value",
            "limit",
            "slack",
            "unit",
            "source",
        ],
        total,
        matched,
        out,
    )
}
