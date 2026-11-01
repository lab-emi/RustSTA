// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The dual-PDK suite (`docs/specs/sta/verification.md`): the fixture designs timed and checked
//! with pinned standard-cell libraries of two process design kits.
//!
//! - `pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]`: downloads and verifies the
//!   libraries of `verification/pdks.toml` ([`mod@fetch`]);
//! - `verify pdk [--dir DIR] [--profile pr|full] [--require] [--gpu] [--timeout SECS]`: runs the
//!   suite ([`run`]). Without the libraries it prints a SKIPPED banner and succeeds, unless
//!   `--require` is given.
//!
//! The libraries live in the library directory: `--dir`, else `$RUSTEDA_PDK_DIR`, else
//! `target/pdk` of the repository ([`fetch::library_dir`]). They are never tracked: design data
//! is tracked only as synthetic fixtures (AGENTS 5.6), and the files exceed the size limit.
//!
//! `fetch.rs`, `manifest.rs`, `json.rs` and `verification/pdks.toml` are shared verbatim with the
//! other RustEDA tool repositories; the command line (this file), the runner ([`run`]) and the
//! library scan ([`scan`]) belong to this repository.

mod fetch;
pub(crate) mod json;
pub(crate) mod manifest;
mod run;
mod scan;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default time limit of each `ruststa` invocation of `verify pdk`.
const TIMEOUT_S: u64 = 600;

/// Profile of `verify pdk`: `pr` checks the nominal `tt` corner per family; `full` checks all 9
/// `(family, corner)` libraries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Profile {
    #[default]
    Pr,
    Full,
}

/// Options of `pdk fetch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FetchOptions {
    pub(crate) dir: Option<PathBuf>,
    pub(crate) only: Vec<String>,
    pub(crate) offline: Option<PathBuf>,
}

/// Options of `verify pdk`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyOptions {
    pub(crate) dir: Option<PathBuf>,
    pub(crate) profile: Profile,
    /// Missing libraries fail the suite instead of skipping it.
    pub(crate) require: bool,
    /// Include the GPU backend when available.
    pub(crate) gpu: bool,
    /// Time limit of each `ruststa` invocation.
    pub(crate) timeout: Duration,
}

impl Default for VerifyOptions {
    fn default() -> VerifyOptions {
        VerifyOptions {
            dir: None,
            profile: Profile::Pr,
            require: false,
            gpu: false,
            timeout: Duration::from_secs(TIMEOUT_S),
        }
    }
}

/// The value of option `name` at `args[*i]` (`--name VALUE` or `--name=VALUE`), advancing `*i`
/// past a separate value; `None` when `args[*i]` is not that option.
fn option_value(args: &[String], i: &mut usize, name: &str) -> Option<Result<String, String>> {
    let a = args.get(*i)?;
    if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
        return Some(if v.is_empty() {
            Err(format!("{name} needs a value"))
        } else {
            Ok(v.to_string())
        });
    }
    if a != name {
        return None;
    }
    *i += 1;
    Some(match args.get(*i) {
        Some(v) if !v.is_empty() && !v.starts_with("--") => Ok(v.clone()),
        _ => Err(format!("{name} needs a value")),
    })
}

/// `pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]`: the arguments after `pdk`.
pub(crate) fn fetch_args(args: &[String]) -> Result<FetchOptions, String> {
    match args.first().map(String::as_str) {
        Some("fetch") => {}
        Some(other) => return Err(format!("unknown pdk command '{other}' (expected 'fetch')")),
        None => {
            return Err(
                "usage: cargo xtask pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]"
                    .into(),
            );
        }
    }
    let mut o = FetchOptions::default();
    let mut i = 1;
    while let Some(a) = args.get(i) {
        if let Some(v) = option_value(args, &mut i, "--dir") {
            o.dir = Some(PathBuf::from(v?));
        } else if let Some(v) = option_value(args, &mut i, "--only") {
            for item in v?.split(',').filter(|s| !s.is_empty()) {
                o.only.push(item.to_string());
            }
        } else if let Some(v) = option_value(args, &mut i, "--offline") {
            o.offline = Some(PathBuf::from(v?));
        } else if a.starts_with('-') {
            return Err(format!("unknown option '{a}'"));
        } else {
            return Err(format!("unexpected argument '{a}'"));
        }
        i += 1;
    }
    Ok(o)
}

/// `verify pdk [--dir DIR] [--profile pr|full] [--require] [--gpu] [--timeout SECS]`: the
/// arguments after `pdk`.
pub(crate) fn verify_args(args: &[String]) -> Result<VerifyOptions, String> {
    let mut o = VerifyOptions::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if let Some(v) = option_value(args, &mut i, "--dir") {
            o.dir = Some(PathBuf::from(v?));
        } else if let Some(v) = option_value(args, &mut i, "--profile") {
            let v = v?;
            o.profile = match v.as_str() {
                "pr" => Profile::Pr,
                "full" => Profile::Full,
                _ => {
                    return Err(format!("--profile must be 'pr' or 'full', not '{v}'"));
                }
            };
        } else if let Some(v) = option_value(args, &mut i, "--timeout") {
            let v = v?;
            o.timeout = match v.parse::<u64>() {
                Ok(s) if s > 0 => Duration::from_secs(s),
                _ => {
                    return Err(format!(
                        "--timeout needs a positive number of seconds, not '{v}'"
                    ));
                }
            };
        } else if a == "--require" {
            o.require = true;
        } else if a == "--gpu" {
            o.gpu = true;
        } else if a.starts_with('-') {
            return Err(format!("unknown option '{a}'"));
        } else {
            return Err(format!("unexpected argument '{a}'"));
        }
        i += 1;
    }
    Ok(o)
}

/// The library directory of `--dir` or the environment ([`fetch::library_dir`]).
fn library_dir(root: &Path, dir: Option<&Path>) -> PathBuf {
    fetch::library_dir(root, dir, std::env::var_os(fetch::DIR_VAR))
}

/// The `pdk fetch` command. The run outputs of `verify pdk` are no place for libraries: each run
/// deletes them.
pub(crate) fn fetch(root: &Path, o: &FetchOptions) -> Result<(), String> {
    let mut libs = manifest::load(root)?;
    if !o.only.is_empty() {
        for wanted in &o.only {
            if !libs
                .iter()
                .any(|l| l.name == *wanted || l.library == *wanted)
            {
                return Err(format!("pdk fetch: unknown library '{wanted}' in --only"));
            }
        }
        libs.retain(|l| o.only.iter().any(|w| *w == l.name || *w == l.library));
    }
    let dir = library_dir(root, o.dir.as_deref());
    if let Some(vendor) = &o.offline {
        let abs_vendor =
            std::path::absolute(vendor).map_err(|e| format!("{}: {e}", vendor.display()))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for lib in &mut libs {
            let fname = lib.file_name();
            let prebuilt = abs_vendor.join(&fname);
            if prebuilt.is_file() {
                let dest = dir.join(&fname);
                if prebuilt != dest {
                    std::fs::copy(&prebuilt, &dest)
                        .map_err(|e| format!("{}: {e}", dest.display()))?;
                }
            } else {
                let base = format!("file://{}/", abs_vendor.display());
                match &mut lib.source {
                    manifest::Source::File { url } => {
                        *url = format!("{base}{fname}");
                    }
                    manifest::Source::Assembled { base_url, .. } => {
                        *base_url = base;
                    }
                }
            }
        }
    }
    fetch::run(root, &libs, &dir, &[root.join(run::OUT)])
}

/// The `verify pdk` command.
pub(crate) fn verify(root: &Path, o: &VerifyOptions) -> Result<(), String> {
    let libs = manifest::load(root)?;
    let dir = library_dir(root, o.dir.as_deref());
    run::run(root, &libs, &dir, o)
}
