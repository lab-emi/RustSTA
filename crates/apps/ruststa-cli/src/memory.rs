// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! How the `ruststa` program hands the memory that it frees back to the system (AGENTS rule 9.4:
//! peak RSS).
//!
//! The C allocator of glibc keeps the pages of a freed block for reuse. A block of at least its
//! mmap threshold has a mapping of its own, which is unmapped when the block is freed, but the
//! threshold follows the size of every such block that is freed, up to 32 MiB; after that, blocks
//! of up to that size are cut from the arena of the allocating thread, and an arena gives pages
//! back only at its top. Each rayon worker of a run allocates from its own arena, so the pages
//! that a phase frees in one arena (the parsed extraction once every corner is bound, a corner's
//! preparation once it is propagated) stay resident while the phases that follow allocate in
//! others. On the Ibex benchmark (its two corners at once with four threads) such pages were 25 to
//! 50 MiB of the peak resident memory of `ruststa run`, a different amount in every run.
//!
//! [`configure`] fixes the threshold at [`MMAP_THRESHOLD`], and [`ReleaseAtPhaseEnds`] returns the
//! free pages of every arena to the system at the end of each phase of the analysis. Neither
//! changes a value: only which freed pages stay resident.
//!
//! The two calls into the C library are the only unsafe code of this crate (CONTRIBUTING rule
//! 5.4); with another C library both functions do nothing.

#![allow(unsafe_code)]

use rusteda_core::job::{Event, Observer};

/// The fixed mmap threshold, in bytes: every block of at least this size has a mapping of its own,
/// which is returned to the system when the block is freed. Of 128 KiB to 32 MiB it left the least
/// resident on the Ibex benchmark: 128 and 256 KiB about 18 MiB more at two threads, 512 KiB and
/// 2 MiB up to 9 MiB more in some runs at four.
pub const MMAP_THRESHOLD: usize = 1 << 20;

/// Fixes the mmap threshold of the C allocator at [`MMAP_THRESHOLD`], unless the environment
/// already sets it (`MALLOC_MMAP_THRESHOLD_`, or `glibc.malloc.mmap_threshold` in
/// `GLIBC_TUNABLES`). Returns whether the threshold was set. The program calls it once, first,
/// before any thread is started.
pub fn configure() -> bool {
    let set_by_env = std::env::var_os("MALLOC_MMAP_THRESHOLD_").is_some()
        || std::env::var_os("GLIBC_TUNABLES")
            .is_some_and(|t| t.to_string_lossy().contains("glibc.malloc.mmap_threshold"));
    !set_by_env && sys::set_mmap_threshold(MMAP_THRESHOLD)
}

/// Returns the free pages of every arena of the C allocator to the system. Returns whether any
/// memory was returned.
pub fn release_free_pages() -> bool {
    sys::release_free_pages()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod sys {
    pub(super) fn set_mmap_threshold(bytes: usize) -> bool {
        let Ok(value) = libc::c_int::try_from(bytes) else {
            return false;
        };
        // SAFETY: `mallopt` takes two integers and changes one parameter of glibc's allocator
        // under the allocator's own lock; it reads and writes no memory of the caller, and the
        // threshold decides only where later blocks are placed.
        unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, value) == 1 }
    }

    pub(super) fn release_free_pages() -> bool {
        // SAFETY: `malloc_trim` takes an integer and, under the allocator's locks, gives the
        // whole pages inside free blocks back to the system (they read as zero when reused);
        // no allocated block is touched, so no memory that the program references changes.
        unsafe { libc::malloc_trim(0) == 1 }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod sys {
    pub(super) fn set_mmap_threshold(_bytes: usize) -> bool {
        false
    }

    pub(super) fn release_free_pages() -> bool {
        false
    }
}

/// An observer of a run that returns the allocator's free pages to the system
/// ([`release_free_pages`]) whenever a phase ends, then passes every event on to `inner`.
pub struct ReleaseAtPhaseEnds<'a> {
    inner: Option<&'a dyn Observer>,
}

impl<'a> ReleaseAtPhaseEnds<'a> {
    /// The observer that passes the events on to `inner` (none: the events end here).
    pub fn new(inner: Option<&'a dyn Observer>) -> Self {
        Self { inner }
    }
}

impl std::fmt::Debug for ReleaseAtPhaseEnds<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReleaseAtPhaseEnds")
            .field("inner", &self.inner.is_some())
            .finish()
    }
}

impl Observer for ReleaseAtPhaseEnds<'_> {
    fn event(&self, e: &Event) {
        if matches!(e, Event::PhaseFinished { .. }) {
            release_free_pages();
        }
        if let Some(inner) = self.inner {
            inner.event(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Observer for Recorder {
        fn event(&self, e: &Event) {
            let line = match e {
                Event::PhaseStarted { phase } => format!("start {phase}"),
                Event::PhaseFinished { phase, .. } => format!("finish {phase}"),
                Event::Metric { name, .. } => format!("metric {name}"),
                _ => "other".to_string(),
            };
            self.0.lock().unwrap().push(line);
        }
    }

    #[test]
    fn passes_every_event_on_in_order() {
        let rec = Recorder::default();
        let obs = ReleaseAtPhaseEnds::new(Some(&rec));
        obs.event(&Event::PhaseStarted { phase: "a" });
        obs.event(&Event::Metric {
            name: "nodes",
            value: 3.0,
        });
        obs.event(&Event::PhaseFinished {
            phase: "a",
            seconds: 0.5,
        });
        assert_eq!(
            *rec.0.lock().unwrap(),
            ["start a", "metric nodes", "finish a"]
        );
    }

    #[test]
    fn without_an_inner_observer_the_events_end_here() {
        let obs = ReleaseAtPhaseEnds::new(None);
        obs.event(&Event::PhaseFinished {
            phase: "a",
            seconds: 0.0,
        });
        assert!(format!("{obs:?}").contains("inner: false"));
    }

    #[test]
    fn the_threshold_is_an_int_of_the_c_library() {
        assert!(i32::try_from(MMAP_THRESHOLD).is_ok());
        assert!(MMAP_THRESHOLD.is_power_of_two());
    }
}
