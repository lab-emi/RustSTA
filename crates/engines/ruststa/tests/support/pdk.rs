// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The PDK libraries of the tests that `cargo xtask verify pdk` invokes.

use std::path::{Path, PathBuf};

/// The PDK directory: `RUSTEDA_PDK_DIR`, else the first `target/pdk` above this crate.
pub fn find_pdk_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTEDA_PDK_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .map(|d| d.join("target/pdk"))
                .find(|p| p.is_dir())
        })
}

/// `name,file,library,corner` of the `tt` libraries when `cargo xtask verify pdk` names none.
const DEFAULT_TT_LIBS: &str = "sky130hd_tt,sky130_fd_sc_hd__tt_025C_1v80.lib,,tt;\
    gf180mcu7t_tt,gf180mcu_fd_sc_mcu7t5v0__tt_025C_3v30.lib,,tt;\
    gf180mcu9t_tt,gf180mcu_fd_sc_mcu9t5v0__tt_025C_3v30.lib,,tt";

/// The `tt` libraries of a PDK campaign: those that `cargo xtask verify pdk` passes
/// (`RUSTSTA_PDK_VERIFY_DIR`, `RUSTSTA_PDK_VERIFY_LIBS`), else [`DEFAULT_TT_LIBS`] in the PDK
/// directory. Every library must exist, and both PDK families must be present (AGENTS rule 7.2).
pub fn campaign_libraries() -> Vec<(String, PathBuf)> {
    let (dir, spec) = match std::env::var("RUSTSTA_PDK_VERIFY_LIBS") {
        Ok(spec) if !spec.is_empty() => {
            let dir = std::env::var("RUSTSTA_PDK_VERIFY_DIR").expect("PDK directory");
            (PathBuf::from(dir), spec)
        }
        _ => (
            find_pdk_dir().expect("no PDK directory: set RUSTEDA_PDK_DIR or fetch the PDKs"),
            DEFAULT_TT_LIBS.to_string(),
        ),
    };
    let libs: Vec<(String, PathBuf)> = spec
        .split(';')
        .filter_map(|item| {
            let f: Vec<&str> = item.split(',').collect();
            (f.len() == 4 && f[3].starts_with("tt")).then(|| (f[0].to_string(), dir.join(f[1])))
        })
        .collect();
    for (tag, path) in &libs {
        assert!(path.is_file(), "{tag}: {} is missing", path.display());
    }
    let family = |prefix: &str| libs.iter().any(|(t, _)| t.starts_with(prefix));
    assert!(
        family("sky130") && family("gf180"),
        "need sky130 and gf180mcu: {libs:?}"
    );
    libs
}
