# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Keep repository-relative source links usable on GitHub and the static site."""

from pathlib import Path
import re
from urllib.parse import quote, unquote, urlsplit, urlunsplit


def on_page_markdown(markdown, *, page, config, files):
    docs = Path(config.docs_dir).resolve()
    root = docs.parent
    source = Path(page.file.abs_src_path)

    def resolve(match):
        url = urlsplit(match.group(1))
        if url.scheme or url.netloc or not url.path:
            return match.group(0)
        target = (source.parent / unquote(url.path)).resolve()
        if target.is_relative_to(docs) or not target.is_relative_to(root):
            return match.group(0)
        if not target.exists():
            # Let strict validation diagnose missing source references.
            return match.group(0)
        kind = "tree" if target.is_dir() else "blob"
        path = quote(target.relative_to(root).as_posix(), safe="/")
        destination = urlunsplit(("https", "github.com", f"/lab-emi/RustSTA/{kind}/main/{path}",
                                 url.query, url.fragment))
        return f"]({destination})"

    # Do not rewrite code examples. Only Markdown link destinations are transformed.
    parts = re.split(r"(```.*?```|~~~.*?~~~)", markdown, flags=re.DOTALL)
    for i in range(0, len(parts), 2):
        parts[i] = re.sub(r"\]\(([^\s)]+)\)", resolve, parts[i])
    return "".join(parts)
