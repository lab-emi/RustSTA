// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustSTA contributors

//! Legacy GPU propagation stub (owner: C2, then Z1; STA-REQ-0400, STA-REQ-0404).
//!
//! GPU propagation now runs exclusively through [`rusteda_compute::timing`] via
//! [`crate::engine::prop::lower`]. The legacy `ruststa sta --backend cuda|gpu` entry point is
//! retired and returns an explicit input error directing callers to `ruststa run`.

use rusteda_compute::{GpuError, GpuRuntime};

use crate::graph::Graph;
use crate::model::Model;
use crate::prop::{Setup, Values};

/// No-op background preparation stub for the retired legacy GPU path (STA-REQ-0408).
pub fn prewarm(_want: Option<GpuRuntime>) {}

/// Rejects legacy GPU propagation (`ruststa sta --backend cuda|gpu`) with an explicit input error
/// naming `ruststa run` (STA-REQ-0400, STA-REQ-0404).
pub fn propagate_gpu(
    _g: &Graph,
    _m: &Model,
    _s: &Setup,
    _want: Option<GpuRuntime>,
) -> Result<(Values, GpuRuntime), GpuError> {
    Err(GpuError(
        "legacy `ruststa sta --backend cuda|gpu` has been retired; use `ruststa run` \
         (STA-REQ-0400, STA-REQ-0404)"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_gpu_prewarm_is_noop() {
        prewarm(None);
        prewarm(Some(GpuRuntime::Cuda));
    }
}
