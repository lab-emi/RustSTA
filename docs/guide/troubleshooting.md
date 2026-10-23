<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Troubleshooting

| Symptom | Action |
|---|---|
| Cargo rejects the toolchain or edition | Install Rust with rustup and use the pinned `rust-toolchain.toml`; a distribution Cargo can be older. |
| Example input not found | Run from the repository root and use the complete fixture paths in the [quickstart](quickstart.md). |
| `REVISION_CONFLICT`, exit 4 | Read `rusteda show`/`log`, then reassess the change against the current explicit revision. |
| `HEAD` rejected as a mutation base | Pass the concrete `revN`; queries can use `HEAD`. |
| `BACKEND_UNAVAILABLE` / `A0501` | Check the compiled features, NVIDIA driver, NVRTC and device self-test; start with `--backend cpu`. |
| No path for a handle | Use a handle returned for the same revision and corner; check unconstrained endpoints and path limits. |
| Unsupported SDC diagnostic | Check [supported scope](../reference/scope.md) and the [SDC specification](../specs/sdc/sdc.md); remove or replace a constraint only if its semantics remain valid. |
| Old `sta.rbin` format rejected | Run timing again with the current engine; the current engine writes `STA v2`. |
| Checker is undecided or disagrees | Keep the artifact and diagnostics, reduce the case, and compare with a pinned black-box reference; see [checking](checking.md). |
| Memory budget reached | Reduce corners/workers/path limits, inspect tag growth, and use OS guards; a larger state budget requires a new estimate. |

For a reproducible report include the command, version, error code, input hashes,
corner, wire model, backend, precision, memory limits and smallest shareable
input. Follow the repository's [security policy](https://github.com/lab-emi/RustSTA/blob/main/SECURITY.md)
for vulnerabilities or confidential design data.
