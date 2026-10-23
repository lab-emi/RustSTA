# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
"""Keep repository-relative source links usable on GitHub and the static site."""

from pathlib import Path
from html import escape
import posixpath
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


def on_post_build(*, config):
    """Keep the standalone report's source links usable in the generated site."""
    docs = Path(config.docs_dir).resolve()
    source = docs / "verification/spef-report.html"
    if not source.is_file():
        return
    if source.stat().st_size > 1024 * 1024:
        raise ValueError("SPEF report byte budget exceeded")
    output = Path(config.site_dir).resolve() / source.relative_to(docs)

    def resolve(match):
        url = urlsplit(match.group(1))
        if url.scheme or url.netloc or not url.path:
            return match.group(0)
        target = (source.parent / unquote(url.path)).resolve()
        if not target.is_file():
            return match.group(0)
        if target.is_relative_to(docs) and target.suffix == ".md":
            rel = target.relative_to(docs).with_suffix("").as_posix()
            destination = rel + "/" if config.use_directory_urls else rel + ".html"
            path = posixpath.relpath(destination, source.parent.relative_to(docs).as_posix())
            if config.use_directory_urls:
                path += "/"
            link = urlunsplit(("", "", path, url.query, url.fragment))
        elif target.is_relative_to(docs.parent) and not target.is_relative_to(docs):
            path = quote(target.relative_to(docs.parent).as_posix(), safe="/")
            link = urlunsplit(("https", "github.com", f"/lab-emi/RustSTA/blob/main/{path}",
                               url.query, url.fragment))
        else:
            return match.group(0)
        return f'href="{escape(link, quote=True)}"'

    output.write_text(re.sub(r'href="([^"\s]+)"', resolve, source.read_text(encoding="utf-8")),
                      encoding="utf-8")
