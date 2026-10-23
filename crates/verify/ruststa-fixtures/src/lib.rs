// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Synthetic cell libraries, seeded netlist/SDC generators, and the gate-level netlist corpus for
//! RustSTA tests.

#[path = "gen/mod.rs"]
pub mod r#gen;
pub mod lib_gen;
pub mod liberty;
mod liberty_closed;
pub mod liberty_full;
pub mod net_gen;
pub mod netlists;
pub mod sdc_gen;

use std::path::Path;

use rusteda_formats::liberty::read_text;
use rusteda_formats::sdc;
use rusteda_formats::verilog::netlist::read_structural;

/// Relative path from repository root to the STA synthetic fixture directory.
pub const STA_FIXTURES_REL: &str = "verification/fixtures/synthetic/sta";

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn expected_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for v in liberty::Variant::ALL {
        out.push((format!("lib/{}.lib", v.name()), liberty::library(v)));
    }
    for f in r#gen::synthetic_netlists() {
        out.push((format!("net/{}.v", f.name), f.verilog));
    }
    for (name, text) in sdc_gen::synthetic_sdcs() {
        out.push((format!("sdc/{name}.sdc"), text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Regenerates `verification/fixtures/synthetic/sta/{lib,net,sdc}/` below `root`, leaving
/// `verification/fixtures/synthetic/sta/netlists/` untouched.
pub fn write_synthetic_fixtures(root: &Path) -> Result<(), String> {
    let base = root.join(STA_FIXTURES_REL);
    for sub in ["lib", "net", "sdc"] {
        let dir = base.join(sub);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    for (rel, content) in expected_files() {
        write_file(&base.join(rel), &content)?;
    }
    Ok(())
}

/// Checks that `verification/fixtures/synthetic/sta/{lib,net,sdc}/` below `root` matches the
/// deterministic generators byte-for-byte and that every generated `.lib`, `.v`, and `.sdc` file
/// parses cleanly with `rusteda-formats`.
pub fn check_synthetic_fixtures(root: &Path) -> Result<(), String> {
    let base = root.join(STA_FIXTURES_REL);
    let expected = expected_files();

    // Check that no extra files exist in lib/, net/, sdc/.
    let mut actual_rels = Vec::new();
    for sub in ["lib", "net", "sdc"] {
        let dir = base.join(sub);
        if !dir.is_dir() {
            return Err(format!("missing fixture directory {}", dir.display()));
        }
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file()
                && let Ok(rel) = p.strip_prefix(&base)
            {
                actual_rels.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    actual_rels.sort();
    let want_rels: Vec<String> = expected.iter().map(|(r, _)| r.clone()).collect();
    if actual_rels != want_rels {
        return Err(format!(
            "fixture file set mismatch: on disk {actual_rels:?}, expected {want_rels:?}"
        ));
    }

    let cat = r#gen::CellCatalog::default_wt15();
    for (rel, want_text) in &expected {
        let path = base.join(rel);
        let disk_text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if disk_text != *want_text {
            return Err(format!(
                "{}: content differs from generator; run `cargo xtask fixtures`",
                path.display()
            ));
        }
        if rel.ends_with(".lib") {
            read_text(&disk_text, rel).map_err(|e| format!("{rel}: {e}"))?;
        } else if rel.ends_with(".v") {
            read_structural(&disk_text, rel, &|c| cat.cell_pins(c))
                .map_err(|e| format!("{rel}: {e}"))?;
        } else if rel.ends_with(".sdc") {
            let (_ast, diags) = sdc::read_str(&disk_text);
            if let Some(d) = diags.iter().find(|d| d.severity == sdc::Severity::Error) {
                return Err(format!("{rel}: {}: {}", d.code, d.message));
            }
        }
    }
    Ok(())
}
