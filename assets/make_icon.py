"""Renders the Glance icon: a play triangle with a soft glow on a deep blue tile.

Writes glance.ico (16-256 px, PNG-compressed entries) and glance.png (256 px,
used for the window icon). Needs only numpy.

    python assets/make_icon.py
"""
import struct
import zlib
from pathlib import Path

import numpy as np

OUT = Path(__file__).parent


def hexrgb(h):
    return np.array([int(h[i:i + 2], 16) / 255 for i in (1, 3, 5)])


TOP, BOTTOM = hexrgb("#1f3b73"), hexrgb("#0a1230")
GLOW, PLAY_A, PLAY_B = hexrgb("#4fb0ff"), hexrgb("#7fe3ff"), hexrgb("#4f8bff")


def render(size, ss=4):
    n = size * ss
    y, x = (np.mgrid[0:n, 0:n] + 0.5) / n
    # Rounded square tile.
    r, m = 0.22, 0.04
    dx = np.maximum(np.abs(x - 0.5) - (0.5 - m - r), 0)
    dy = np.maximum(np.abs(y - 0.5) - (0.5 - m - r), 0)
    tile = np.clip((r - np.hypot(dx, dy)) * n, 0, 1)
    color = TOP + (BOTTOM - TOP) * y[..., None]
    # Play triangle (centroid slightly right of centre), via signed edge distances.
    pts = np.array([[0.38, 0.28], [0.74, 0.5], [0.38, 0.72]])
    d = np.full(x.shape, -np.inf)
    for i in range(3):
        a, b = pts[i], pts[(i + 1) % 3]
        e = b - a
        nrm = np.array([e[1], -e[0]]) / np.hypot(*e)
        d = np.maximum(d, (x - a[0]) * nrm[0] + (y - a[1]) * nrm[1])
    inside = np.clip(-d * n, 0, 1)  # edges oriented so the inside is negative
    glow = np.exp(-np.maximum(d, 0) * 14)[..., None] * 0.55
    color = color + (GLOW - color) * glow
    play = PLAY_A + (PLAY_B - PLAY_A) * ((y - 0.28) / 0.44)[..., None]
    color = color + (play - color) * inside[..., None]
    rgba = np.dstack([np.clip(color, 0, 1), tile])
    rgba = rgba.reshape(size, ss, size, ss, 4).mean(axis=(1, 3))
    return (rgba * 255 + 0.5).astype(np.uint8)


def png(img):
    h, w, _ = img.shape
    raw = b"".join(b"\0" + img[i].tobytes() for i in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


sizes = [16, 24, 32, 48, 64, 128, 256]
images = [png(render(s)) for s in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b""
for s, data in zip(sizes, images):
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
(OUT / "glance.ico").write_bytes(header + entries + b"".join(images))
(OUT / "glance.png").write_bytes(images[-1])
print("wrote glance.ico, glance.png")
