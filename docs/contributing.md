<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Contribute to RustSTA

Start with the repository's [contribution rules](../CONTRIBUTING.md).
Behavior belongs in specifications with requirement IDs, implementations cite
those IDs, and independent verification supports algorithm changes.

## Development checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask layout
cargo xtask coverage
cargo xtask shared check
cargo xtask fixtures check
cargo test --workspace
cargo xtask verify sta
```

Estimate peak memory and check `free -h` before builds and verification runs.
Use bounded workers, OS memory guards and wall-clock timeouts. Record peak RSS
for new suites and scale benchmarks; serialize runs estimated above 4 GiB.
The [performance guide](guide/performance.md) gives guard examples.

Share immutable parsed PDK libraries across tests rather than repeatedly cloning
multi-megabyte libraries. Compare engine updates with a full recomputation and
OpenSTA black-box results on both Sky130 and GF180. Keep checker code independent
of the engine's assumptions.

Shared RustEDA crates and specifications are tracked in
[the shared-file manifest](../verification/shared.sha256). Coordinate shared
changes across sibling repositories and run `cargo xtask shared check`.

## Documentation and design

Put walkthroughs in `docs/guide/`, lookup pages in `docs/reference/`, and exact
requirements in `docs/specs/`. Keep existing specification and manual URLs stable.
Run the [documentation checks](about/maintaining-docs.md) after edits.

Brand assets follow the shared [RustEDA visual identity](brand/README.md).
Marketing claims require measured evidence with explicit scope. Illustrative
artwork must stay distinguishable from tool output or verification receipts.

See the [Code of Conduct](../CODE_OF_CONDUCT.md) and
[security reporting policy](../SECURITY.md).
