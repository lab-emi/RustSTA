// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Resource guard of the verification runs (AGENTS rule 9.3): every `cargo test` that `verify`
//! and `verify pdk` start runs with at most four test threads (unless `RUST_TEST_THREADS` says
//! otherwise), under a wall-clock timeout and, where the user's service manager accepts transient
//! scopes, inside one with a memory limit, so that a runaway allocation or a hang ends that run
//! instead of the host.
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `RUSTEDA_VERIFY_MEMORY_MAX` | `MemoryMax` of the scope | `16G` |
//! | `RUSTEDA_VERIFY_TIMEOUT_S` | Timeout of one step in seconds | `3600` |
//! | `RUSTEDA_VERIFY_NO_SCOPE` | When set, no scope (timeout and threads still apply) | unset |

use std::process::{Command, Stdio};

/// How the verification steps are confined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Guard {
    /// Run each step in a transient scope of the user's service manager.
    pub(crate) scope: bool,
    /// `MemoryMax` of the scope, in the manager's syntax (`16G`).
    pub(crate) memory_max: String,
    /// Wall-clock limit of one step in seconds.
    pub(crate) timeout_s: u64,
}

impl Guard {
    /// The guard of this machine and environment.
    pub(crate) fn detect() -> Guard {
        let memory_max =
            std::env::var("RUSTEDA_VERIFY_MEMORY_MAX").unwrap_or_else(|_| "16G".to_string());
        let timeout_s = std::env::var("RUSTEDA_VERIFY_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);
        let scope = std::env::var_os("RUSTEDA_VERIFY_NO_SCOPE").is_none()
            && Command::new("systemd-run")
                .args(["--user", "--scope", "--quiet", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        Guard {
            scope,
            memory_max,
            timeout_s,
        }
    }

    /// The command that runs `cargo` with `args` under this guard.
    pub(crate) fn cargo(&self, cargo: &str, args: &[String]) -> Command {
        let timeout = format!("{}s", self.timeout_s);
        let mut cmd = if self.scope {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "-p"])
                .arg(format!("MemoryMax={}", self.memory_max))
                .args(["-p", "MemorySwapMax=4G", "timeout", &timeout, cargo]);
            c
        } else {
            let mut c = Command::new("timeout");
            c.args([timeout.as_str(), cargo]);
            c
        };
        cmd.args(args);
        if std::env::var_os("RUST_TEST_THREADS").is_none() {
            cmd.env("RUST_TEST_THREADS", "4");
        }
        cmd
    }

    /// One line that says how the steps are confined.
    pub(crate) fn describe(&self) -> String {
        if self.scope {
            format!(
                "steps run in a user scope with MemoryMax={} and a {} s timeout",
                self.memory_max, self.timeout_s
            )
        } else {
            format!(
                "steps run with a {} s timeout and no memory scope",
                self.timeout_s
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Guard;

    fn args(cmd: &std::process::Command) -> Vec<String> {
        std::iter::once(cmd.get_program())
            .chain(cmd.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn scoped_and_unscoped_commands() {
        let step = vec!["test".to_string(), "--release".to_string()];
        let scoped = Guard {
            scope: true,
            memory_max: "8G".to_string(),
            timeout_s: 60,
        };
        assert_eq!(
            args(&scoped.cargo("cargo", &step)),
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "-p",
                "MemoryMax=8G",
                "-p",
                "MemorySwapMax=4G",
                "timeout",
                "60s",
                "cargo",
                "test",
                "--release"
            ]
        );
        let plain = Guard {
            scope: false,
            ..scoped
        };
        assert_eq!(
            args(&plain.cargo("cargo", &step)),
            ["timeout", "60s", "cargo", "test", "--release"]
        );
        assert!(plain.describe().contains("60 s"));
    }
}
