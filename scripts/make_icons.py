#!/usr/bin/env python3
"""Generates the ScreenRec application icon (ICO + raw RGBA for egui).

The script only uses the Python standard library so it can be re-run on any
machine without extra tooling. Run it from the project root:

    python3 scripts/make_icons.py
"""

from __future__ import annotations

import math
import os
import struct
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "resources")
SIZES = [16, 24, 32, 48, 64, 128, 256]
SS = 4  # supersampling factor

INDIGO_TOP = (79, 124, 255)
INDIGO_BOTTOM = (37, 66, 168)
WHITE = (255, 255, 255)
RED = (229, 72, 77)
RED_DARK = (178, 38, 44)


def smoothstep(edge0: float, edge1: float, x: float) -> float:
    if edge0 == edge1:
        return 1.0 if x >= edge1 else 0.0
    t = max(0.0, min(1.0, (x - edge0) / (edge1 - edge0)))
    return t * t * (3.0 - 2.0 * t)


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def mix(c1, c2, t: float):
    return tuple(lerp(c1[i], c2[i], t) for i in range(3))


def rounded_rect_coverage(px: float, py: float, size: float) -> float:
    margin = size * 0.035
    radius = size * 0.225
    cx = cy = size * 0.5
    hw = hh = size * 0.5 - margin
    dx = abs(px - cx) - (hw - radius)
    dy = abs(py - cy) - (hh - radius)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    inside = min(max(dx, dy), 0.0)
    dist = outside + inside - radius
    return 1.0 - smoothstep(-0.5, 0.5, dist)


def render_hires(size: int):
    """Returns a list of straight-alpha RGBA tuples at size*SS resolution."""
    hi = size * SS
    cx = cy = hi * 0.5
    ring_outer = hi * 0.30
    ring_inner = hi * 0.30 - max(1.0, hi * 0.055)
    dot_radius = hi * 0.155

    pixels = []
    for y in range(hi):
        for x in range(hi):
            px = x + 0.5
            py = y + 0.5

            bg = rounded_rect_coverage(px, py, hi)
            if bg <= 0.0:
                pixels.append((0, 0, 0, 0))
                continue

            ny = py / hi
            base = mix(INDIGO_TOP, INDIGO_BOTTOM, ny)

            # Soft diagonal sheen in the upper-left corner.
            sheen = max(0.0, 1.0 - math.hypot(px / hi, py / hi) * 1.6)
            base = mix(base, (255, 255, 255), sheen * 0.10)

            col = base
            d = math.hypot(px - cx, py - cy)

            # Record button: white ring with a red centre.
            if ring_inner <= d <= ring_outer:
                col = WHITE
            if d <= dot_radius:
                dot_t = max(0.0, min(1.0, d / max(dot_radius, 1.0)))
                col = mix(RED, RED_DARK, dot_t * 0.55)

            r, g, b = (int(round(v)) for v in col)
            pixels.append((r, g, b, int(round(bg * 255))))
    return pixels, hi


def downsample(pixels, hi: int, size: int):
    out = []
    scale = hi // size
    inv = 1.0 / (scale * scale)
    for y in range(size):
        for x in range(size):
            ar = ag = ab = aa = 0.0
            for sy in range(scale):
                row = (y * scale + sy) * hi
                for sx in range(scale):
                    r, g, b, a = pixels[row + x * scale + sx]
                    af = a / 255.0
                    ar += r * af
                    ag += g * af
                    ab += b * af
                    aa += af
            if aa <= 0.0001:
                out.append((0, 0, 0, 0))
                continue
            out.append(
                (
                    int(round(ar / aa)),
                    int(round(ag / aa)),
                    int(round(ab / aa)),
                    int(round(aa * inv * 255.0)),
                )
            )
    return out


def ico_bitmap_bits(rgba, size: int):
    header = struct.pack(
        "<IiiHHIIiiII",
        40,
        size,
        size * 2,
        1,
        32,
        0,
        size * size * 4,
        0,
        0,
        0,
        0,
    )
    body = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            r, g, b, a = rgba[y * size + x]
            body += bytes((b, g, r, a))
    mask_row = ((size + 31) // 32) * 4
    mask = bytearray()
    for y in range(size - 1, -1, -1):
        row = bytearray(mask_row)
        for x in range(size):
            if rgba[y * size + x][3] < 128:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row
    return header + bytes(body) + bytes(mask)


def build_ico(path: str):
    images = []
    for size in SIZES:
        pixels, hi = render_hires(size)
        rgba = downsample(pixels, hi, size)
        images.append((size, ico_bitmap_bits(rgba, size)))

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = bytearray()
    data = bytearray()
    for size, bits in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,
            0,
            1,
            32,
            len(bits),
            offset,
        )
        data += bits
        offset += len(bits)

    with open(path, "wb") as fh:
        fh.write(header)
        fh.write(entries)
        fh.write(data)


def write_raw_rgba(path: str, size: int):
    pixels, hi = render_hires(size)
    rgba = downsample(pixels, hi, size)
    with open(path, "wb") as fh:
        for r, g, b, a in rgba:
            fh.write(bytes((r, g, b, a)))


def write_png(path: str, size: int):
    pixels, hi = render_hires(size)
    rgba = downsample(pixels, hi, size)
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            raw += bytes(rgba[y * size + x])

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as fh:
        fh.write(png)


def main():
    os.makedirs(RES, exist_ok=True)
    build_ico(os.path.join(RES, "ScreenRec.ico"))
    write_raw_rgba(os.path.join(RES, "icon_256.rgba"), 256)
    write_png(os.path.join(RES, "icon_256.png"), 256)
    print("Generated resources/ScreenRec.ico, icon_256.rgba, icon_256.png")


if __name__ == "__main__":
    main()
