// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Atomic, bounded detailed-parasitic import (RCLI-REQ-0070, RCLI-REQ-0071, RCLI-REQ-0072).
use rusteda_formats::spef::{self, Limits, Metadata};
use rusteda_workspace::{DiagCode, Diagnostic, Error, Hash64, JsonMap, RbinWriter, Severity, Txn};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

pub(crate) fn import_spef(
    txn: &mut Txn,
    path: &Path,
    corner: &str,
    limits: Limits,
    metrics: &mut JsonMap,
    diags: &mut Vec<Diagnostic>,
) -> Result<(), Error> {
    limits
        .validate()
        .map_err(|e| Error::input_rejected(e.to_string()))?;
    if corner.is_empty()
        || corner.len() > 128
        || !corner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::usage(
            "--spef-corner must be a shell-safe corner handle",
        ));
    }
    if !txn.libraries().iter().any(|l| l.corner == corner) {
        return Err(Error::input_rejected(format!(
            "SPEF corner {corner} has no imported library"
        )));
    }
    let meta = fs::symlink_metadata(path)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(Error::input_rejected(
            "SPEF source must be a regular file, not a symlink/FIFO/device",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if !file
        .metadata()
        .map_err(|e| Error::input_rejected(e.to_string()))?
        .is_file()
    {
        return Err(Error::input_rejected("SPEF source is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limits.max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::input_rejected(format!("{}: {e}", path.display())))?;
    if bytes.len() > limits.max_bytes {
        return Err(Error::input_rejected(
            "T0072: SPEF source byte budget exceeded",
        ));
    }
    let mut reject = |e: spef::Error| {
        diags.push(Diagnostic::from_parts(
            Severity::Error,
            DiagCode::of(e.code),
            Some((path.display().to_string(), e.line.max(1) as u32, 1)),
            e.message.clone(),
            None,
        ));
        Error::input_rejected(e.to_string())
    };
    let decoded = spef::decode_source(&bytes, limits).map_err(&mut reject)?;
    let parsed = spef::read(&decoded.text, limits).map_err(&mut reject)?;
    let metadata = serde_json::to_vec(&Metadata {
        corner: corner.into(),
        limits,
    })
    .map_err(|e| Error::internal(e.to_string()))?;
    let mut writer = RbinWriter::new("PARASIT", 1)?;
    writer.section_bytes(1, 1, decoded.text.as_bytes())?;
    writer.section_bytes(2, 1, metadata)?;
    let upstream = txn.upstream(&["netlist"])?;
    txn.add_rbin("parasitics", "parasitics.rbin", &writer, upstream)?;
    txn.add_source("parasitics", path.display().to_string(), Hash64::of(&bytes))?;
    // A pre-extraction timing table cannot depend on a table that did not yet exist.
    for table in ["sta", "sta_check"] {
        if txn.table_hash(table).is_some() {
            txn.drop_table(table, "extracted parasitics imported")?;
        }
    }
    metrics.insert("spef_nets".into(), json!(parsed.nets.len()));
    metrics.insert("spef_nodes".into(), json!(parsed.node_count));
    metrics.insert("spef_elements".into(), json!(parsed.element_count));
    metrics.insert("spef_corner".into(), json!(corner));
    metrics.insert("spef_compression".into(), json!(decoded.encoding.as_str()));
    metrics.insert("spef_source_bytes".into(), json!(bytes.len()));
    metrics.insert("spef_decoded_bytes".into(), json!(decoded.text.len()));
    if parsed.coupling_records > 0 {
        diags.push(Diagnostic::new(
            Severity::Warning,
            DiagCode::of("T0074"),
            format!(
                "{} inter-net coupling records grounded at factor 1; crosstalk not analyzed",
                parsed.coupling_records
            ),
        ));
    }
    Ok(())
}
