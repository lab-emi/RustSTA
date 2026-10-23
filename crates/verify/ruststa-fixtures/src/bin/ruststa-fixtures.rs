// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Command-line tool for writing and checking the synthetic fixtures under
//! `verification/fixtures/synthetic/sta/{lib,net,sdc}/`.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn default_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn run(args: &[String]) -> Result<(), String> {
    let mut mode = "write";
    let mut root = default_root();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "write" | "check" => mode = a.as_str(),
            "--root" => {
                i += 1;
                let v = args.get(i).ok_or("--root needs a path")?;
                root = PathBuf::from(v);
            }
            other => {
                return Err(format!(
                    "unknown argument '{other}' (usage: ruststa-fixtures [write|check] [--root DIR])"
                ));
            }
        }
        i += 1;
    }
    match mode {
        "write" => {
            ruststa_fixtures::write_synthetic_fixtures(&root)?;
            println!(
                "wrote {}",
                root.join(ruststa_fixtures::STA_FIXTURES_REL).display()
            );
            Ok(())
        }
        "check" => {
            ruststa_fixtures::check_synthetic_fixtures(&root)?;
            println!("fixtures ok");
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
