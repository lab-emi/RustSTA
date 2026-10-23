// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Repository automation. Run with `cargo xtask <command>`.
//!
//! - `fixtures [write|check]`: regenerates or checks the synthetic Liberty libraries (`lib/`),
//!   netlists (`net/`) and SDC files (`sdc/`) of `verification/fixtures/synthetic/sta/` from
//!   `ruststa-fixtures`; the checked-in netlists in `netlists/` are left as they are.
//! - `layout`: checks the repository layout rules on tracked (and untracked, not ignored) files:
//!   locations, crates, SPDX headers, sizes and dependency edges ([`layout`]).
//! - `coverage`: writes the requirement-to-test matrix to `docs/verification/requirements.md`.
//! - `verify [sta|sdc|compute|all] [--gpu]`: runs the verification suites ([`verify`]).
//! - `verify pdk [--dir DIR] [--profile pr|full] [--require] [--gpu] [--timeout SECS]`: verifies
//!   the fetched PDK libraries ([`pdk`]).
//! - `pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]`: downloads the pinned
//!   standard-cell libraries of `verification/pdks.toml` and verifies their hashes ([`pdk`]).
//! - `shared <check|update|diff OTHER>`: checks or rewrites the manifest of the files shared with
//!   the other RustEDA tool repositories, or compares them with another repository ([`shared`]).

#![allow(clippy::print_stdout)]

mod coverage;
mod guard;
mod layout;
mod pdk;
mod shared;
#[cfg(test)]
mod testutil;
mod verify;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// First line every Rust source must have (assembled so that license scanners do not read it as
/// a tag of this file).
pub(crate) const SPDX_HEADER: &str = concat!("// SPDX-", "License-Identifier: Apache-2.0");

/// The workspace root (the parent of this crate's directory).
pub(crate) fn root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map_or(manifest.clone(), Path::to_path_buf)
}

pub(crate) fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The `fixtures [write|check]` command: rewrites or checks `lib/`, `net/` and `sdc/` of
/// `verification/fixtures/synthetic/sta/`. The netlists of `netlists/` were synthesized outside
/// this repository, so they are never touched.
fn fixtures(root: &Path, args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        None | Some("write") if args.len() <= 1 => {
            ruststa_fixtures::write_synthetic_fixtures(root)?;
            println!(
                "wrote {}",
                root.join(ruststa_fixtures::STA_FIXTURES_REL).display()
            );
            Ok(())
        }
        Some("check") if args.len() == 1 => {
            ruststa_fixtures::check_synthetic_fixtures(root)?;
            println!("fixtures ok");
            Ok(())
        }
        _ => Err("usage: cargo xtask fixtures [write|check]".into()),
    }
}

/// Files to check: tracked files and untracked files that git does not ignore, without the
/// local-only files that are never part of the product tree (agent notes, the scratch, tool and
/// build directories). Other files of a local setup belong in the clone's `.git/info/exclude`,
/// which git applies like `.gitignore`.
fn files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root())
        .output();
    let mut v: Vec<String> = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let local = |p: &str| {
        let lower = p.to_ascii_lowercase();
        lower == "agents.md"
            || lower == "claude.md"
            || lower.starts_with(".work/")
            || lower.starts_with("target/")
            || lower.starts_with(".claude/")
    };
    v.retain(|p| !local(p));
    // Files deleted from the working tree but still in the index are not checked.
    let r = root();
    v.retain(|p| r.join(p).exists());
    v.sort();
    v.dedup();
    v
}

/// The commands, printed for a missing or unknown command.
const USAGE: &str = "usage: cargo xtask <command>
  fixtures [write|check]
  layout
  coverage
  verify [sta|sdc|compute|all] [--gpu]
  verify pdk [--dir DIR] [--profile pr|full] [--require] [--gpu] [--timeout SECS]
  pdk fetch [--dir DIR] [--only NAME...] [--offline VENDOR_DIR]
  shared <check|update|diff OTHER>";

/// `verify [product] [--gpu]`; the product defaults to `sta`.
fn verify_args(args: &[String]) -> Result<(String, verify::Options), String> {
    let mut product = None;
    let mut o = verify::Options::default();
    for a in args {
        match a.as_str() {
            "--gpu" => o.gpu = true,
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s if product.is_none() => product = Some(s.to_string()),
            s => return Err(format!("unexpected argument '{s}'")),
        }
    }
    Ok((product.unwrap_or_else(|| "sta".into()), o))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("fixtures") => fixtures(&root(), args.get(1..).unwrap_or_default()),
        Some("layout") => layout::run(&root(), &files()),
        Some("coverage") => coverage::run(&root()),
        Some("pdk") => {
            pdk::fetch_args(args.get(1..).unwrap_or_default()).and_then(|o| pdk::fetch(&root(), &o))
        }
        Some("verify") if args.get(1).is_some_and(|a| a == "pdk") => {
            pdk::verify_args(args.get(2..).unwrap_or_default())
                .and_then(|o| pdk::verify(&root(), &o))
        }
        Some("verify") => verify_args(args.get(1..).unwrap_or_default())
            .and_then(|(p, o)| verify::run(&root(), &p, o)),
        Some("shared") => shared::run(&root(), args.get(1..).unwrap_or_default()),
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Tree;
    use ruststa_fixtures::liberty::{Variant, library};

    // `verify sta --gpu` parses and the product defaults to `sta`; unknown options and a second
    // product fail.
    #[test]
    fn verify_arguments() {
        let a = |s: &[&str]| verify_args(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(
            a(&["sta", "--gpu"]),
            Ok(("sta".to_string(), verify::Options { gpu: true }))
        );
        assert_eq!(
            a(&["--gpu"]),
            Ok(("sta".to_string(), verify::Options { gpu: true }))
        );
        assert_eq!(a(&[]), Ok(("sta".to_string(), verify::Options::default())));
        assert!(a(&["sta", "--fast"]).is_err());
        assert!(a(&["sta", "check"]).is_err());
    }

    // `fixtures` rewrites `lib/`, `net/` and `sdc/` while leaving `netlists/` untouched, and
    // `fixtures check` verifies byte-for-byte identity and parsing.
    #[test]
    fn fixtures_keep_the_netlists_and_check_cleanly() {
        let t = Tree::new("fixtures");
        let netlist = "module counter(input wire clk_i);\nendmodule\n";
        t.file(
            "verification/fixtures/synthetic/sta/netlists/counter.v",
            netlist,
        )
        .file("verification/fixtures/synthetic/sta/lib/old.lib", "x");
        fixtures(t.root(), &[]).unwrap_or_else(|e| panic!("{e}"));
        fixtures(t.root(), &["check".to_string()]).unwrap_or_else(|e| panic!("{e}"));

        let read = |rel: &str| std::fs::read_to_string(t.root().join(rel)).unwrap_or_default();
        assert_eq!(
            read("verification/fixtures/synthetic/sta/netlists/counter.v"),
            netlist
        );
        assert!(
            !t.root()
                .join("verification/fixtures/synthetic/sta/lib/old.lib")
                .exists()
        );
        for v in Variant::ALL {
            assert_eq!(
                read(&format!(
                    "verification/fixtures/synthetic/sta/lib/{}.lib",
                    v.name()
                )),
                library(v),
                "{}",
                v.name()
            );
        }
    }
}
