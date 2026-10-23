<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- SPDX-FileCopyrightText: 2026 RustSTA contributors -->

# Maintain the documentation

The documentation is a static MkDocs Material site deployed to
[lab-emi.github.io/RustSTA](https://lab-emi.github.io/RustSTA/).
It follows the RustEDA identity shared with RustSys. The Rust tools do not
depend on Python or the site renderer.

## Install and build

From the repository root:

```bash
python3 -m venv .work/docs-venv
.work/docs-venv/bin/python -m pip install -r docs/requirements.txt
free -h
(
  ulimit -v 4194304
  timeout 60s .work/docs-venv/bin/python docs/_tools/check_markdown.py
  /usr/bin/time -v -o .work/docs-build.time timeout 180s \
    .work/docs-venv/bin/python -m mkdocs build --strict -f docs/mkdocs.yml
  timeout 60s .work/docs-venv/bin/python docs/_tools/check_site.py .work/docs-site
)
```

The source check validates README/docs links against the checkout. Strict MkDocs
checks navigation and site links. The final bounded checker resolves generated
HTML links, images, scripts and fragment IDs without network access.
Both link checkers cap page count, bytes and links per page.

The launch site contains fewer than 100 pages. Estimated build RSS is below
512 MiB, with a 4 GiB address-space guard and a 180-second wall-clock limit.
The GIF has a 1 MiB hard cap and is about 139 KiB. System fonts avoid external
font requests; no application server or client framework is required.

`requirements.in` pins direct tooling; `requirements.txt` locks transitives.
Refresh deliberately with `uv pip compile docs/requirements.in --output-file
docs/requirements.txt`.

## Preview and edit

```bash
.work/docs-venv/bin/python -m mkdocs serve -f docs/mkdocs.yml --dev-addr 127.0.0.1:8000
```

Open the printed `/RustSTA/` URL and stop the preview when finished. This is a
documentation authoring process, separate from RustSTA's daemonless CLI.

Put task walkthroughs in `docs/guide/`, lookup pages in `docs/reference/` and exact
contracts in `docs/specs/`. Preserve existing manual and specification paths.
Add primary pages to `nav` in `docs/mkdocs.yml`.

Keep Markdown links relative so they work in GitHub. The small site hook rewrites
links that leave `docs/` to the corresponding GitHub source location during the
site build. Shared specifications stay byte-identical to the shared-file manifest.
Do not edit shared sources just to accommodate a documentation renderer.

Run new command examples against the current binaries. Use matching synthetic
libraries and netlists: `net/chain_8.v` uses `lib/wt_15.lib`, whereas the
`netlists/` corpus uses `lib/wt_full.lib`. Keep illustrative diagrams separate
from command output and measured evidence.

## Publish to GitHub Pages

The `docs` GitHub Actions workflow checks pull requests and changes to `main`,
builds the site and uploads an artifact. Successful `main` documentation pushes
also deploy automatically. Pull requests only validate.

Set repository Pages publishing to **GitHub Actions**. The deploy job uses the
`github-pages` environment with scoped Pages/OIDC permissions. The build job has
read-only repository permissions and does not need a deploy token.

For a manual build, start the `docs` workflow on `main`. Enable **Publish to GitHub
Pages** to deploy after validation. Check the successful deployment and live URL.
Keep publication on the current `main` snapshot; do not create a separate history
of generated HTML in a publishing branch.
