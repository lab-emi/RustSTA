#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSTA contributors
# Shared frame and rendering approach: 2026 RustSys contributors, Apache-2.0.
"""Render the RustSTA Signal Forge identity from bounded, original SVG geometry.

Documentation authoring only. No Python dependency enters the Rust runtime.
84 indexed frames use 38.5 MiB; estimated peak RSS including scratch < 256 MiB.
Run under a 1 GiB address-space guard and a 180-second timeout.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import io
import json
from pathlib import Path

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "docs" / "assets"
INK, PANEL, BORDER = "#101923", "#17242F", "#304451"
COPPER, TEAL, PAPER, MUTED = "#EF8B52", "#57D6BE", "#F4F1E9", "#A1B0BA"
WIDTH, HEIGHT, FRAMES, FRAME_MS = 1200, 400, 84, 100
MAX_PIXELS, MAX_FRAMES, MAX_GIF_BYTES = 480_000, 96, 1024 * 1024


def text(x: float, y: float, value: str, size: int = 16, fill: str = PAPER,
         weight: int = 400, mono: bool = False, anchor: str = "start") -> str:
    family = "DejaVu Sans Mono,monospace" if mono else "DejaVu Sans,sans-serif"
    return (f'<text x="{x}" y="{y}" fill="{fill}" font-family="{family}" '
            f'font-size="{size}" font-weight="{weight}" text-anchor="{anchor}">'
            f'{html.escape(value)}</text>')


def svg(width: int, height: int, body: str, title: str, description: str) -> str:
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">'
            f'<title id="title">{html.escape(title)}</title>'
            f'<desc id="desc">{html.escape(description)}</desc>{body}</svg>\n')


def mark() -> str:
    return (f'<rect width="256" height="256" rx="54" fill="{INK}"/>'
            '<g fill="none" stroke-width="12" stroke-linecap="round" stroke-linejoin="round">'
            f'<path d="M79 50H50V79M50 177V206H79" stroke="{COPPER}"/>'
            f'<path d="M177 50H206V79M206 177V206H177" stroke="{TEAL}"/>'
            f'<path d="M69 128H83" stroke="{COPPER}"/>'
            f'<circle cx="128" cy="128" r="43" stroke="{PAPER}" stroke-width="8"/>'
            f'<path d="M128 128V99" stroke="{COPPER}" stroke-width="9"/>'
            f'<path d="M128 128L151 141M173 128H189" stroke="{TEAL}" stroke-width="9"/>'
            f'</g><circle cx="128" cy="128" r="6" fill="{PAPER}"/>')


def wordmark() -> str:
    body = f'<rect width="760" height="176" rx="28" fill="{INK}"/>'
    body += f'<g transform="translate(20 20) scale(.53125)">{mark()}</g>'
    body += text(180, 89, "RustSTA", 58, weight=700)
    body += text(184, 127, "From netlists to timing insight.", 19, MUTED)
    return svg(760, 176, body, "RustSTA wordmark", "A RustEDA tool for static timing analysis.")


def wire(path: str, color: str = BORDER, width: int = 2) -> str:
    return (f'<path d="{path}" fill="none" stroke="{color}" stroke-width="{width}" '
            'stroke-linecap="round" stroke-linejoin="round"/>')


def pulse(a: tuple[float, float], b: tuple[float, float], progress: float,
          color: str = TEAL) -> str:
    if not 0 <= progress <= 1:
        return ""
    x, y = a[0] + (b[0] - a[0]) * progress, a[1] + (b[1] - a[1]) * progress
    return (f'<circle cx="{x:.2f}" cy="{y:.2f}" r="9" fill="{color}" opacity=".13"/>'
            f'<circle cx="{x:.2f}" cy="{y:.2f}" r="3.5" fill="{color}"/>')


def banner(frame: int) -> str:
    """Fixed-size DAG illustration; no timing measurement or sign-off claim."""
    t = frame / FRAMES
    stage = min(3, int(t * 4))
    body = f'<rect width="1200" height="400" rx="24" fill="{INK}"/>'
    for x in range(24, 1200, 24):
        for y in range(96, 336, 24):
            body += f'<circle cx="{x}" cy="{y}" r=".7" fill="{MUTED}" opacity=".14"/>'
    body += f'<g transform="translate(32 20) scale(.21)">{mark()}</g>'
    body += text(98, 57, "RustSTA", 29, weight=700)
    body += text(1164, 43, "NETLIST  →  TIMING  →  CRITICAL PATH", 13, MUTED, 700, anchor="end")
    body += text(1164, 66, "CLI first. Ready for agents.", 14, TEAL, anchor="end")
    for x, width, number, label in [(32, 286, "01", "DESIGN INPUTS"),
                                   (396, 410, "02", "PROPAGATE TIMING"),
                                   (884, 284, "03", "INSPECT THE PATH")]:
        body += (f'<rect x="{x}" y="100" width="{width}" height="218" rx="14" '
                 f'fill="{PANEL}" stroke="{BORDER}"/>')
        body += text(x + 18, 126, number, 11, COPPER, mono=True)
        body += text(x + 49, 126, label, 12, MUTED, 700)

    for i, (name, label) in enumerate([("gates.v", "gate-level netlist"),
                                      ("cells.lib", "Liberty timing tables"),
                                      ("design.sdc", "clocks + constraints")]):
        y = 158 + 47 * i
        color = COPPER if i == stage or stage >= 1 else MUTED
        body += wire(f'M52 {y-10}H65L72 {y-3}V{y+12}H52Z', color)
        body += text(85, y, name, 16, PAPER, mono=True)
        body += text(85, y + 18, label, 11, MUTED)
    body += text(51, 298, "Explicit files. Reproducible revisions.", 11, TEAL)

    for start, end, color in [(329, 385, COPPER), (817, 873, TEAL)]:
        body += wire(f'M{start} 207H{end-8}', color)
        body += wire(f'M{end-14} 201L{end-6} 207L{end-14} 213', color)
        body += pulse((start, 207), (end-10, 207), (t * 3) % 1, color)

    # Two reconvergent branches. Only the top path is highlighted, never enumerated.
    critical = [((461, 206), (516, 167)), ((516, 167), (604, 206)),
                ((604, 206), (691, 167)), ((691, 167), (745, 206))]
    other = [((461, 206), (516, 254)), ((516, 254), (604, 206)),
             ((604, 206), (691, 254)), ((691, 254), (745, 206))]
    active = t >= .25
    for a, b in other:
        body += wire(f'M{a[0]} {a[1]}L{b[0]} {b[1]}')
    for i, (a, b) in enumerate(critical):
        color = TEAL if active else BORDER
        body += wire(f'M{a[0]} {a[1]}L{b[0]} {b[1]}', color, 3)
        if active:
            body += pulse(a, b, ((t - .25) * 4 - i * .23) % 1)
    for x, y in [(516, 167), (516, 254), (604, 206), (691, 167), (691, 254)]:
        color = TEAL if active and y != 254 else BORDER
        body += f'<circle cx="{x}" cy="{y}" r="8" fill="{INK}" stroke="{color}" stroke-width="2"/>'
    for x, name in [(425, "r0/Q"), (745, "r1/D")]:
        body += f'<rect x="{x}" y="189" width="36" height="34" rx="4" fill="{INK}" stroke="{PAPER}"/>'
        body += wire(f'M{x} 213L{x+6} 217L{x} 221', MUTED, 1)
        body += text(x + 18, 242, name, 11, MUTED, mono=True, anchor="middle")
    body += text(443, 171, "launch", 10, COPPER, anchor="middle")
    body += text(763, 171, "capture", 10, TEAL, anchor="middle")
    body += wire('M428 282H461V272H498V282H536V272H574V282H612V272H650V282H688V272H772', COPPER)
    body += text(601, 302, ["READ INPUTS", "ARRIVAL → REQUIRED", "RANK BOUNDED PATHS", "TRACE THE CRITICAL PATH"][stage],
                 11, TEAL, 700, anchor="middle")

    body += text(903, 162, "path1", 23, TEAL, 700, mono=True)
    body += text(1149, 161, "setup", 12, COPPER, mono=True, anchor="end")
    for i, (key, value) in enumerate([("corner", "default"), ("wire model", "ideal"),
                                      ("from", "r0/Q"), ("to", "r1/D")]):
        y = 190 + i * 24
        body += text(904, y, key, 12, MUTED, mono=True)
        body += text(1148, y, value, 13, PAPER, mono=True, anchor="end")
    body += text(904, 298, "Trace → inspect → iterate", 12, TEAL)
    body += f'<rect x="32" y="336" width="1136" height="40" rx="9" fill="#0B1219" stroke="{BORDER}"/>'
    body += text(48, 361, "$", 14, TEAL, mono=True)
    body += text(68, 361, "ruststa run --workspace ./ws --base-rev rev1 --max-paths 5 --format json", 13, PAPER, mono=True)
    body += text(1151, 361, "netlist → insight", 12, COPPER, mono=True, anchor="end")
    return svg(WIDTH, HEIGHT, body, "RustSTA: from netlists to timing insight",
               "Netlist, Liberty and SDC inputs feed a reconvergent timing graph. Signals move along a "
               "highlighted launch-to-capture path, then a revision-scoped path report identifies its endpoints. "
               "This is an illustration, not a tool run, benchmark or sign-off result.")


def raster(source: str, width: int = WIDTH, height: int = HEIGHT) -> Image.Image:
    data = cairosvg.svg2png(bytestring=source.encode(), output_width=width, output_height=height)
    with Image.open(io.BytesIO(data)) as source_image:
        return source_image.convert("RGB")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--static-only", action="store_true")
    args = parser.parse_args()
    if WIDTH * HEIGHT > MAX_PIXELS or not 1 <= FRAMES <= MAX_FRAMES:
        raise SystemExit("ASSET_BUDGET_EXCEEDED: dimensions or frame count")
    ASSETS.mkdir(parents=True, exist_ok=True)
    entries = {}
    for name, source in {
        "ruststa-logo.svg": svg(256, 256, mark(), "RustSTA timing mark", "A clock in the shared RustEDA chip and terminal frame."),
        "favicon.svg": svg(256, 256, mark(), "RustSTA", "RustSTA static timing analysis."),
        "ruststa-wordmark.svg": wordmark(),
        "ruststa-banner.svg": banner(65),
    }.items():
        (ASSETS / name).write_text(source)
        entries[name] = {"bytes": len(source.encode()), "sha256": hashlib.sha256(source.encode()).hexdigest()}
    if args.static_only:
        print(json.dumps(entries, indent=2))
        return
    frames, samples = [], []
    palette = raster(banner(65)).quantize(colors=96, method=Image.Quantize.MEDIANCUT)
    for frame in range(FRAMES):
        image = raster(banner(frame))
        frames.append(image.quantize(palette=palette, dither=Image.Dither.NONE))
        if frame in (0, 21, 42, 65):
            samples.append(image.resize((600, 200)))
    output = ASSETS / "ruststa-banner.gif"
    frames[0].save(output, save_all=True, append_images=frames[1:], duration=FRAME_MS,
                   loop=0, optimize=True, disposal=1)
    if output.stat().st_size > MAX_GIF_BYTES:
        raise SystemExit("ASSET_BUDGET_EXCEEDED: GIF exceeds 1 MiB")
    qa = ROOT / ".work" / "brand"
    qa.mkdir(parents=True, exist_ok=True)
    contact = Image.new("RGB", (1200, 400), INK)
    for i, sample in enumerate(samples):
        contact.paste(sample, ((i % 2) * 600, (i // 2) * 200))
    contact.save(qa / "ruststa-banner-contact-sheet.png")
    raster(banner(65)).save(qa / "ruststa-banner-poster.png")
    raster(wordmark(), 760, 176).save(qa / "ruststa-wordmark.png")
    with Image.open(output) as gif:
        entries[output.name] = {
            "bytes": output.stat().st_size, "width": gif.width, "height": gif.height,
            "frames": gif.n_frames, "duration_ms": sum((gif.seek(i), gif.info.get("duration", 0))[1]
                                                     for i in range(gif.n_frames)),
            "loop": gif.info.get("loop"), "sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        }
    (qa / "assets-manifest.json").write_text(json.dumps(entries, indent=2) + "\n")
    print(json.dumps(entries, indent=2))


if __name__ == "__main__":
    main()
