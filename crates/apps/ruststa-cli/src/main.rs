// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! The `ruststa` command (`docs/specs/cli/README.md`).

fn main() -> std::process::ExitCode {
    // Before the thread pool starts: where the allocator places large blocks (`memory`).
    ruststa_cli::memory::configure();
    ruststa_cli::run(std::env::args_os().collect())
}
