#!/usr/bin/env python3
"""Render a captured terminal session as a PNG.

The submission needs screenshots, and a screenshot of a *re-run* run is a claim.
This tool turns the raw capture of a real run into an image instead: pair it with
`script -q -c "<command>" capture.txt` so the image is a faithful transcript of
what the terminal actually printed, ANSI colours included.

Usage:
    script -q -c "npm run demo" demo.txt
    python3 tools/terminal-to-png.py --input demo.txt --output demo.png \
        --title "npm run demo — agent session against z:...:expense-guard"

Requires: python3 (stdlib only) and a chromium binary on PATH.
"""

from __future__ import annotations

import argparse
import glob
import html
import os
import re
import shutil
import subprocess
import sys
import tempfile

# Standard 16-colour palette, tuned for a dark background.
PALETTE = {
    30: "#3d4752", 31: "#e4687c", 32: "#7ec699", 33: "#e6c07b", 34: "#6cb6ff",
    35: "#c397d8", 36: "#63c6d6", 37: "#c8d1da",
    90: "#5c6a78", 91: "#ff8a9b", 92: "#a1e8b8", 93: "#ffd79a", 94: "#8ec8ff",
    95: "#d9b3ea", 96: "#8ee0ee", 97: "#eef3f8",
}
BACKGROUND_PALETTE = {
    40: "#10161d", 41: "#5a2430", 42: "#22402f", 43: "#4d3d1c", 44: "#1d3350",
    45: "#3d2a4a", 46: "#1d4046", 47: "#39424c",
}

SGR = re.compile(r"\x1b\[([0-9;]*)m")
CONTROL = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Za-z0-9]|[\x00-\x08\x0b\x0c\x0e-\x1f]")


def sgr_to_html(text: str) -> str:
    """Translate SGR escape sequences into spans. Everything else is escaped."""
    out: list[str] = []
    open_span = 0
    pos = 0
    for match in SGR.finditer(text):
        out.append(html.escape(CONTROL.sub("", text[pos:match.start()])))
        pos = match.end()
        codes = [int(c) if c else 0 for c in match.group(1).split(";")]
        styles: list[str] = []
        for code in codes:
            if code == 0:
                while open_span:
                    out.append("</span>")
                    open_span -= 1
            elif code == 1:
                styles.append("font-weight:600")
            elif code == 2:
                styles.append("opacity:.65")
            elif code in PALETTE:
                styles.append(f"color:{PALETTE[code]}")
            elif code in BACKGROUND_PALETTE:
                styles.append(f"background:{BACKGROUND_PALETTE[code]}")
        if styles:
            out.append(f'<span style="{";".join(styles)}">')
            open_span += 1
    out.append(html.escape(CONTROL.sub("", text[pos:])))
    out.append("</span>" * open_span)
    return "".join(out)


def build_html(body: str, title: str | None, cols: int, font_size: int) -> str:
    line_height = round(font_size * 1.5)
    header = (
        f'<div class="bar"><span class="dot r"></span><span class="dot y"></span>'
        f'<span class="dot g"></span><span class="title">{html.escape(title)}</span></div>'
        if title
        else ""
    )
    return f"""<!doctype html>
<meta charset="utf-8">
<style>
  html, body {{ margin:0; padding:0; background:#070a0e; }}
  .card {{ display:inline-block; background:#0b1017; border:1px solid #1d2530;
           border-radius:10px; margin:22px; box-shadow:0 18px 50px rgba(0,0,0,.55); }}
  .bar {{ display:flex; align-items:center; gap:8px; padding:11px 15px;
          border-bottom:1px solid #1d2530; background:#0e141c; border-radius:10px 10px 0 0; }}
  .dot {{ width:11px; height:11px; border-radius:50%; display:inline-block; }}
  .r {{ background:#ff5f57; }} .y {{ background:#febc2e; }} .g {{ background:#28c840; }}
  .title {{ color:#8b98a5; font:500 {font_size - 2}px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;
            margin-left:6px; }}
  pre {{ margin:0; padding:16px 18px; color:#c8d1da; background:transparent;
         font:{font_size}px/{line_height}px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
         white-space:pre; tab-size:4; }}
</style>
<div class="card">
  {header}
  <pre style="min-width:{cols}ch">{body}</pre>
</div>
"""


def find_chromium() -> str | None:
    """First usable chromium binary.

    The snap build is confined: it cannot read a scratch directory outside its
    allowed paths, and it fails as ERR_FILE_NOT_FOUND on the page itself. Prefer
    a plain build (playwright keeps one) and set CHROME_BIN to override.
    """
    home = os.path.expanduser("~")
    candidates: list[str | None] = [
        os.environ.get("CHROME_BIN"),
        *sorted(glob.glob(f"{home}/.cache/ms-playwright/chromium-*/chrome-linux64/chrome"), reverse=True),
        *sorted(glob.glob(f"{home}/.cache/ms-playwright/chromium-*/chrome-linux/chrome"), reverse=True),
        "/usr/lib/chromium/chromium",
        shutil.which("chromium"),
        shutil.which("chromium-browser"),
        shutil.which("google-chrome"),
    ]
    for candidate in candidates:
        if candidate and os.path.exists(candidate):
            return candidate
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, help="captured terminal output (may contain ANSI)")
    parser.add_argument("--output", required=True, help="PNG path to write")
    parser.add_argument("--title", default=None, help="fake window title bar text")
    parser.add_argument("--cols", type=int, default=0, help="minimum width in columns (default: longest line)")
    parser.add_argument("--font-size", type=int, default=15)
    parser.add_argument("--max-height", type=int, default=5000)
    args = parser.parse_args()

    with open(args.input, "r", encoding="utf-8", errors="replace") as handle:
        raw = handle.read()
    raw = raw.replace("\r\n", "\n").replace("\r", "")

    body = sgr_to_html(raw)
    plain = CONTROL.sub("", SGR.sub("", raw))
    longest = max((len(line) for line in plain.splitlines()), default=80)
    cols = args.cols or min(max(longest + 2, 72), 132)

    html_doc = build_html(body, args.title, cols, args.font_size)

    chromium = find_chromium()
    if not chromium:
        print("terminal-to-png: no chromium binary found (set CHROME_BIN)", file=sys.stderr)
        return 2

    width = int(cols * args.font_size * 0.62) + 90
    lines = plain.count("\n") + 1
    height = int(lines * args.font_size * 1.5) + (86 if args.title else 60)
    height = min(height, args.max_height)

    with tempfile.TemporaryDirectory(dir=os.path.dirname(os.path.abspath(args.output))) as tmp:
        html_path = os.path.join(tmp, "capture.html")
        with open(html_path, "w", encoding="utf-8") as handle:
            handle.write(html_doc)
        out_abs = os.path.abspath(args.output)
        os.makedirs(os.path.dirname(out_abs), exist_ok=True)
        result = subprocess.run(
            [
                chromium, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
                "--force-device-scale-factor=2", f"--window-size={width},{height}",
                f"--screenshot={out_abs}", f"file://{html_path}",
            ],
            capture_output=True,
            text=True,
            timeout=180,
        )
        if result.returncode != 0 or not os.path.exists(out_abs):
            print(f"terminal-to-png: chromium failed ({result.returncode})\n{result.stderr[-800:]}", file=sys.stderr)
            return 1

    print(f"{out_abs} — {os.path.getsize(out_abs)} bytes, {width}x{height} css px @2x, {lines} lines")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
