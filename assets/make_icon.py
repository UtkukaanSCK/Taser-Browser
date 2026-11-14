"""Draws the X-Browser icon and writes assets/x-browser.ico.

The glyph: a blue rounded square with a white, round-capped, symmetric X.
Our own geometry, no third-party art. Pure Python (no Pillow): every pixel is
supersampled from the shapes' exact outlines, so the output is the same on
every run. Sizes 16, 24, 32 and 48 are stored as 32-bit BMPs (what old shell
code expects), 256 as PNG.

    python assets/make_icon.py
"""

import os
import struct
import zlib

SIZES = (16, 24, 32, 48, 256)
BLUE = (0x2F, 0x62, 0xE9)
WHITE = (0xFF, 0xFF, 0xFF)

# Shapes on a unit square (0..1, y down).
MARGIN = 0.04  # around the rounded square
RADIUS = 0.22  # its corner radius
ARM = 0.30  # the X's arms run from (ARM, ARM) to (1-ARM, 1-ARM)
STROKE = 0.15  # the X's stroke width


def in_rounded_square(x, y):
    lo, hi = MARGIN, 1.0 - MARGIN
    if not (lo <= x <= hi and lo <= y <= hi):
        return False
    cx = min(max(x, lo + RADIUS), hi - RADIUS)
    cy = min(max(y, lo + RADIUS), hi - RADIUS)
    return (x - cx) ** 2 + (y - cy) ** 2 <= RADIUS**2


def near_segment(x, y, ax, ay, bx, by, r):
    dx, dy = bx - ax, by - ay
    t = ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)
    t = min(max(t, 0.0), 1.0)
    px, py = ax + t * dx, ay + t * dy
    return (x - px) ** 2 + (y - py) ** 2 <= r * r


def in_x(x, y):
    r = STROKE / 2
    a, b = ARM, 1.0 - ARM
    return near_segment(x, y, a, a, b, b, r) or near_segment(x, y, b, a, a, b, r)


def render(size):
    """RGBA rows (top first), straight alpha."""
    n = 8 if size < 64 else 4  # samples per axis
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0.0
            for sy in range(n):
                y = (py + (sy + 0.5) / n) / size
                for sx in range(n):
                    x = (px + (sx + 0.5) / n) / size
                    if not in_rounded_square(x, y):
                        continue
                    c = WHITE if in_x(x, y) else BLUE
                    r += c[0]
                    g += c[1]
                    b += c[2]
                    a += 1
            if a:
                row += bytes((round(r / a), round(g / a), round(b / a), round(255 * a / (n * n))))
            else:
                row += bytes(4)
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def bmp(size, rows):
    """A DIB as ICO stores it: header, BGRA bottom-up, then the AND mask."""
    xor = b"".join(bytes(px for i in range(0, len(row), 4) for px in (row[i + 2], row[i + 1], row[i], row[i + 3])) for row in reversed(rows))
    stride = (size + 31) // 32 * 4
    mask = bytearray()
    for row in reversed(rows):
        bits = bytearray(stride)
        for x in range(size):
            if row[4 * x + 3] == 0:
                bits[x // 8] |= 0x80 >> (x % 8)
        mask += bits
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(xor) + len(mask), 0, 0, 0, 0)
    return header + xor + bytes(mask)


def main():
    images = []
    for size in SIZES:
        rows = render(size)
        images.append((size, png(size, rows) if size >= 256 else bmp(size, rows)))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out += b"".join(data for _, data in images)
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "x-browser.ico")
    with open(path, "wb") as f:
        f.write(out)
    print(f"{path}: {len(out)} bytes, sizes {', '.join(map(str, SIZES))}")


if __name__ == "__main__":
    main()
