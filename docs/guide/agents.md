<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Automate with an agent

RustSTA exposes file-based operations with explicit revisions. A shell script,
CI job or coding agent can analyze a workspace, inspect JSON, make a design
change through another tool, and import a new revision.

## Use a bounded observation loop

1. Read the current workspace revision with `rusteda show --format json`.
2. Submit a mutation with its exact `--base-rev revN`.
3. Parse the JSON envelope and retain `new_rev`.
4. Query that revision with explicit row limits and path handles.
5. Inspect diagnostics, analysis scope and unconstrained endpoints.
6. Make the next decision from the returned result and verification status.

```bash
ruststa run --workspace ./ws --base-rev rev1 \
  --threads 2 --max-paths 10 --max-state-gib 2 --format json \
  > run.json 2> run.stderr
ruststa report --workspace ./ws --rev rev2 --section endpoints \
  --filter 'slack<0' --limit 10 --format json \
  > endpoints.json 2> endpoints.stderr
```

`rev2` illustrates the `new_rev` returned by this run; use the actual returned
value in automation. Store stdout and stderr separately. Human progress output
and diagnostics must not be parsed as JSON.

## Revisions are the coordination boundary

Workspace mutations hold an exclusive `workspace.lock`; read-only reports hold a
shared lock. Changes to an existing workspace require an explicit base revision.
`HEAD` is allowed on queries, but rejected as a mutation base.

On `REVISION_CONFLICT` (exit 4), reread state and reevaluate the intended change.
Avoid retrying a stale decision blindly. Initialization is the exception: it
creates a new `rev0` and takes no base revision.

Handles such as `path1`, `pin17` and `inst102` are shell-safe and revision-scoped.
A handle from one analysis must not be reused against an unrelated revision.

## Treat success and timing status separately

Check the envelope's `ok`, diagnostics and error; then inspect the timing summary,
corner, wire model and endpoint completeness. Timing violations can be valid
analysis results. A checker budget limit or missing external reference tool is
unverified evidence, never an implicit pass.

Use [reports and artifacts](../reference/reports.md),
[commands and exit codes](../reference/cli.md), and the
[full protocol specification](../specs/cli/protocol.md) as the machine contract.
