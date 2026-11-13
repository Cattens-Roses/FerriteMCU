#!/usr/bin/env python3
"""Render the FerriteMCU SVG brand source into the native app icon formats.

Requires Pillow and CairoSVG: ``python -m pip install pillow cairosvg``.
Run from any directory with ``python scripts/generate-brand-assets.py``.
"""

from __future__ import annotations

import io
import struct
from pathlib import Path

import cairosvg
from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / "assets" / "branding"
MARK = BRAND / "ferriteMCU-mark.svg"
BACKGROUND = "#26333D"
SIZE = 1024
CORNER_RADIUS = 185
TILE_INSET = 100
TILE_SIZE = SIZE - TILE_INSET * 2


def render_mark(*, light_ring: bool = False) -> Image.Image:
    source = MARK.read_text(encoding="utf-8")
    if light_ring:
        # The supplied graphite ring disappears on the dark tile used by macOS/Linux.
        source = source.replace('stroke="#26333D"', 'stroke="#B8C3CB"', 1)
    data = cairosvg.svg2png(
        bytestring=source.encode("utf-8"), output_width=SIZE, output_height=SIZE
    )
    return Image.open(io.BytesIO(data)).convert("RGBA")


def icns_bytes(image: Image.Image) -> bytes:
    """Write modern PNG-backed ICNS chunks at each standard macOS size."""
    chunk_types = {
        16: b"icp4",
        32: b"icp5",
        64: b"icp6",
        128: b"ic07",
        256: b"ic08",
        512: b"ic09",
        1024: b"ic10",
    }
    chunks = bytearray()
    for size, chunk_type in chunk_types.items():
        png = io.BytesIO()
        image.resize((size, size), Image.Resampling.LANCZOS).save(png, format="PNG")
        payload = png.getvalue()
        chunks.extend(chunk_type + struct.pack(">I", len(payload) + 8) + payload)
    return b"icns" + struct.pack(">I", len(chunks) + 8) + chunks


def main() -> None:
    # Windows uses the actual mark on transparency. Keep this full-resolution PNG as
    # the source for all ICO sizes so the alpha edges stay clean at high DPI.
    windows_mark = render_mark()
    windows_mark.save(ROOT / "assets" / "AppIcon.png", optimize=True)
    windows_mark.save(
        ROOT / "assets" / "AppIcon.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )

    mark = render_mark(light_ring=True)
    tile = Image.new("RGBA", (SIZE, SIZE), BACKGROUND)
    tile.alpha_composite(mark)

    # The scalable Linux desktop icon uses the supplied vector mark on its dark tile.
    svg = MARK.read_text(encoding="utf-8").replace(
        'stroke="#26333D"', 'stroke="#B8C3CB"', 1
    )
    svg = svg.replace(
        "  <defs>",
        f'  <rect width="128" height="128" fill="{BACKGROUND}"/>\n  <defs>',
        1,
    )
    (ROOT / "assets" / "logo-icon.svg").write_text(svg, encoding="utf-8")

    tile.save(ROOT / "assets" / "logo-square.png", optimize=True)
    body = tile.resize((TILE_SIZE, TILE_SIZE), Image.Resampling.LANCZOS)
    mask = Image.new("L", body.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, TILE_SIZE - 1, TILE_SIZE - 1), radius=CORNER_RADIUS, fill=255
    )
    rounded = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    rounded.paste(body, (TILE_INSET, TILE_INSET), mask)
    rounded.save(ROOT / "assets" / "logo.png", optimize=True)
    # Keep local and release builds visually consistent; branding should not be inverted.
    rounded.save(ROOT / "assets" / "logo-dev.png", optimize=True)
    (ROOT / "assets" / "AppIcon.icns").write_bytes(icns_bytes(rounded))

    print("Generated Windows, macOS, Linux, runtime, and development FerriteMCU icons.")


if __name__ == "__main__":
    main()
