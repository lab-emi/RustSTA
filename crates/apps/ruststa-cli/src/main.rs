// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa` command (`docs/specs/cli/README.md`).

fn main() -> std::process::ExitCode {
    ruststa_cli::run(std::env::args_os().collect())
}
