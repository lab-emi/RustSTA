// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Opt-in phase timing of the `ruststa` commands: with `RUSTSTA_PHASES=1` in the environment,
//! every finished phase of the command and of the timing engine is written to standard error
//! with its wall time and the resident and peak memory of the process after it (AGENTS Rule 8.3:
//! end-to-end measurements, Rule 9.4: peak RSS). Without the variable nothing is measured or
//! printed, and the engine runs without an observer.

use std::sync::OnceLock;
use std::time::Instant;

use rusteda_core::job::{Event, Observer};

/// The environment variable that enables the phase log.
pub const ENV: &str = "RUSTSTA_PHASES";

/// Writes one line per finished phase to standard error.
#[derive(Debug)]
pub struct PhaseLog {
    t0: Instant,
}

impl PhaseLog {
    /// The process-wide log, or `None` unless [`ENV`] is set to a non-empty value other than `0`.
    pub fn get() -> Option<&'static PhaseLog> {
        static LOG: OnceLock<Option<PhaseLog>> = OnceLock::new();
        LOG.get_or_init(|| {
            std::env::var_os(ENV)
                .filter(|v| !v.is_empty() && v != "0")
                .map(|_| PhaseLog { t0: Instant::now() })
        })
        .as_ref()
    }

    /// Runs `f` as phase `phase` of the command.
    pub fn time<T>(log: Option<&PhaseLog>, phase: &'static str, f: impl FnOnce() -> T) -> T {
        match log {
            None => f(),
            Some(log) => {
                let t = Instant::now();
                let out = f();
                log.report(phase, t.elapsed().as_secs_f64());
                out
            }
        }
    }

    fn report(&self, phase: &str, seconds: f64) {
        let (rss, peak) = memory_gib();
        eprintln!(
            "phase {phase:<20} {seconds:9.3} s   rss {rss:7.2} GiB   peak {peak:7.2} GiB   at {:9.3} s",
            self.t0.elapsed().as_secs_f64()
        );
    }
}

impl Observer for PhaseLog {
    fn event(&self, e: &Event) {
        if let Event::PhaseFinished { phase, seconds } = e {
            self.report(phase, *seconds);
        }
    }
}

/// `(VmRSS, VmHWM)` of this process in GiB from `/proc/self/status`, or zeros elsewhere.
fn memory_gib() -> (f64, f64) {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return (0.0, 0.0);
    };
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
            .map_or(0.0, |kib| kib / (1024.0 * 1024.0))
    };
    (field("VmRSS:"), field("VmHWM:"))
}
