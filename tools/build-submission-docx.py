#!/usr/bin/env python3
"""Build docs/SUBMISSION.docx from docs/SUBMISSION.md.

Pandoc embeds an image at its native pixel size. The demo capture is
2634x6112, which lands in a Word file (and therefore in a Google Doc) as a
~32 inch tall picture. This wrapper runs the same pandoc conversion and then
resizes every inline image to a page-friendly width, preserving aspect ratio.

Usage:
    python3 tools/build-submission-docx.py

Requires: pandoc, python-docx, Pillow.
"""

from __future__ import annotations

import hashlib
import subprocess
import sys
from pathlib import Path

from PIL import Image
from docx import Document
from docx.shared import Inches

REPO = Path(__file__).resolve().parent.parent
SOURCE = REPO / "docs" / "SUBMISSION.md"
TARGET = REPO / "docs" / "SUBMISSION.docx"

# Long terminal captures are kept narrow so they fit one page; screenshots with
# a landscape ratio use the full text width.
TARGET_WIDTH_IN = {"demo.png": 3.5}
DEFAULT_WIDTH_IN = 6.0


def build_docx() -> None:
    # Pandoc resolves relative image paths against the working directory, not
    # against the input file, so point it at docs/ explicitly.
    subprocess.run(
        [
            "pandoc",
            str(SOURCE),
            "-o",
            str(TARGET),
            f"--resource-path={SOURCE.parent}",
        ],
        check=True,
        cwd=REPO,
    )


def resize_images() -> list[str]:
    document = Document(str(TARGET))
    report: list[str] = []

    # Pandoc stores every image as word/media/rIdNN.png, so the part name says
    # nothing about which capture it is. Match on content instead.
    by_digest = {
        hashlib.sha256(path.read_bytes()).hexdigest(): path.name
        for path in sorted((REPO / "docs" / "evidence").glob("*.png"))
    }

    for shape in document.inline_shapes:
        rid = shape._inline.graphic.graphicData.pic.blipFill.blip.embed
        blob = document.part.related_parts[rid].blob
        name = by_digest.get(hashlib.sha256(blob).hexdigest())
        if name is None:
            report.append("  unrecognised image: left at pandoc's size")
            continue

        with Image.open(REPO / "docs" / "evidence" / name) as image:
            ratio = image.height / image.width

        width_in = TARGET_WIDTH_IN.get(name, DEFAULT_WIDTH_IN)
        shape.width = Inches(width_in)
        shape.height = Inches(round(width_in * ratio, 3))
        report.append(
            f"  {name}: {width_in}in x {shape.height.inches:.2f}in "
            f"(ratio {ratio:.3f})"
        )

    document.save(str(TARGET))
    return report


def main() -> int:
    build_docx()
    report = resize_images()
    print(f"built {TARGET.relative_to(REPO)} ({TARGET.stat().st_size:,} bytes)")
    print(f"inline images resized: {len(report)}")
    for line in report:
        print(line)
    if not report:
        print("  WARNING: the document has no inline images", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
