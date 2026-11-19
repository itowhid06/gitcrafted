#!/usr/bin/env python3
"""Renders the gitcrafted icon set from the two SVGs next to this file.

    pip install cairosvg pillow
    python3 assets/app-icon/render.py

gitcrafted.svg is the full logo; gitcrafted-small.svg is a simplified one (solid nodes, no
rings) that stays readable at 32 px and below, so the sizes up to 32 use it.

Writes, next to this script:
  hicolor/<n>x<n>/apps/gitcrafted.png   the sizes Linux icon themes look for
  hicolor/scalable/apps/gitcrafted.svg
  gitcrafted-256.png                    the window icon
  gitcrafted-1024.png                   the master macOS builds the .icns from
  gitcrafted.ico                        Windows: 16 to 256 px in one file
"""
import io
import shutil
from pathlib import Path

import cairosvg
from PIL import Image

HERE = Path(__file__).resolve().parent
FULL = HERE / "gitcrafted.svg"
SMALL = HERE / "gitcrafted-small.svg"
SIZES = [16, 22, 24, 32, 48, 64, 128, 256, 512]
SMALL_UP_TO = 32


def render(size: int) -> Image.Image:
    svg = SMALL if size <= SMALL_UP_TO else FULL
    png = cairosvg.svg2png(url=str(svg), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def main() -> None:
    for size in SIZES:
        folder = HERE / "hicolor" / f"{size}x{size}" / "apps"
        folder.mkdir(parents=True, exist_ok=True)
        render(size).save(folder / "gitcrafted.png", optimize=True)
    scalable = HERE / "hicolor" / "scalable" / "apps"
    scalable.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(FULL, scalable / "gitcrafted.svg")
    render(256).save(HERE / "gitcrafted-256.png", optimize=True)
    render(1024).save(HERE / "gitcrafted-1024.png", optimize=True)
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [render(s) for s in ico_sizes]
    frames[-1].save(
        HERE / "gitcrafted.ico",
        format="ICO",
        sizes=[(s, s) for s in ico_sizes],
        append_images=frames[:-1],
        bitmap_format="png",
    )


if __name__ == "__main__":
    main()
