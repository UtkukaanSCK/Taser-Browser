"""Draws the Taser icon and writes assets/taser.ico.

The glyph: a blue rounded square (a diagonal blue gradient) holding a stun
gun seen from the side, nose up, its body a yellow-to-amber gradient, with a
pale yellow lightning bolt in front of the two prongs and a soft glow behind it.
Our own geometry, no third-party art. Pure Python (no Pillow): every pixel is
supersampled from the shapes' exact outlines, so the output is the same on
every run. Sizes 16, 24, 32 and 48 are stored as 32-bit BMPs (what old shell
code expects), 256 as PNG.

    python assets/make_icon.py            # writes assets/taser.ico
    python assets/make_icon.py out.png    # also writes a 512 px PNG preview
"""

import math
import os
import struct
import sys
import zlib

SIZES = (16, 24, 32, 48, 256)

# Colors. BLUE is the brand accent (ui/input.css); the background runs from
# BLUE_HI (top left) to BLUE_LO (bottom right).
BLUE = (0x2F, 0x62, 0xE9)
BLUE_HI = (0x4C, 0x86, 0xFF)
BLUE_LO = (0x16, 0x34, 0xA8)
YELLOW_HI = (0xFF, 0xE9, 0x6A)  # the gun's top edge
YELLOW_LO = (0xF5, 0x9A, 0x0B)  # its bottom edge
HEAD = (0x1B, 0x23, 0x3D)  # the dark muzzle cap
PRONG = (0xE6, 0xEC, 0xF7)
SPARK_HI = (0xFF, 0xFC, 0xE0)  # the bolt's top
SPARK_LO = (0xFF, 0xD0, 0x2E)  # its tip
SHADOW = (0x0E, 0x22, 0x70)

# Shapes on a unit square (0..1, y down), drawn level, then scaled by SCALE
# and turned by TILT around the square's centre so the nose points up and to
# the right. SHIFT moves the level drawing so the turned gun sits centred.
MARGIN = 0.04  # around the rounded square
RADIUS = 0.22  # its corner radius
TILT = math.radians(-14)
SCALE = 0.86
SHIFT = (0.01, -0.04)

BODY = (0.12, 0.36, 0.64, 0.55, 0.09)  # x0, y0, x1, y1, corner radius
HEADBOX = (0.60, 0.335, 0.69, 0.575, 0.035)
GRIP = (0.27, 0.50, 0.18, 0.76, 0.085)  # capsule: from, to, radius
TRIGGER = (0.385, 0.55, 0.37, 0.625, 0.024)
PRONGS = ((0.66, 0.39, 0.75, 0.39, 0.022), (0.66, 0.52, 0.75, 0.52, 0.022))
# The spark: a filled lightning bolt in front of the prongs.
BOLT = ((0.83, 0.24), (0.92, 0.24), (0.875, 0.395), (0.935, 0.395), (0.80, 0.66), (0.835, 0.47), (0.775, 0.47))
GLOW = (0.85, 0.45, 0.27)  # centre and reach of the glow behind the spark
SHADOW_OFF = (0.0, 0.03)  # the gun's drop shadow


def lerp(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def in_rounded_box(x, y, box):
    x0, y0, x1, y1, r = box
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def seg_dist2(x, y, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)
    t = min(max(t, 0.0), 1.0)
    px, py = ax + t * dx, ay + t * dy
    return (x - px) ** 2 + (y - py) ** 2


def near_segment(x, y, seg):
    ax, ay, bx, by, r = seg
    return seg_dist2(x, y, ax, ay, bx, by) <= r * r


def near_bolt(x, y):
    """Inside the bolt polygon (even-odd ray casting)."""
    inside = False
    j = len(BOLT) - 1
    for i in range(len(BOLT)):
        (xi, yi), (xj, yj) = BOLT[i], BOLT[j]
        if (yi > y) != (yj > y) and x < xi + (y - yi) * (xj - xi) / (yj - yi):
            inside = not inside
        j = i
    return inside


_COS, _SIN = math.cos(-TILT), math.sin(-TILT)


def to_gun(x, y):
    """A point of the icon in the gun's own, level coordinates."""
    dx, dy = (x - 0.5) / SCALE, (y - 0.5) / SCALE
    return 0.5 + dx * _COS - dy * _SIN - SHIFT[0], 0.5 + dx * _SIN + dy * _COS - SHIFT[1]


def in_gun(gx, gy):
    return in_rounded_box(gx, gy, BODY) or near_segment(gx, gy, GRIP) or in_rounded_box(gx, gy, HEADBOX)


def sample(x, y):
    """The color at one point inside the rounded square."""
    gx, gy = to_gun(x, y)
    if near_bolt(gx, gy):
        return lerp(SPARK_HI, SPARK_LO, min(max((gy - BOLT[0][1]) / (BOLT[4][1] - BOLT[0][1]), 0.0), 1.0))
    if any(near_segment(gx, gy, p) for p in PRONGS):
        return PRONG
    if in_rounded_box(gx, gy, HEADBOX) or near_segment(gx, gy, TRIGGER):
        return HEAD
    if in_gun(gx, gy):
        # Top to bottom across the body and grip, yellow to amber.
        t = min(max((gy - BODY[1]) / (GRIP[3] - BODY[1]), 0.0), 1.0)
        return lerp(YELLOW_HI, YELLOW_LO, t)
    c = lerp(BLUE_HI, BLUE_LO, min(max((x + y - 2 * MARGIN) / (2 - 4 * MARGIN), 0.0), 1.0))
    if in_gun(gx - SHADOW_OFF[0], gy - SHADOW_OFF[1]):
        c = lerp(c, SHADOW, 0.45)
    # Warm glow around the spark, added on top of the blue.
    d = math.hypot(gx - GLOW[0], gy - GLOW[1]) / GLOW[2]
    if d < 1.0:
        k = (1.0 - d) ** 2 * 0.6
        c = lerp(c, (0xFF, 0xD8, 0x4A), k * 0.55)
        c = tuple(min(255.0, v + k * w) for v, w in zip(c, (60, 40, 0)))
    return c


def render(size):
    """RGBA rows (top first), straight alpha."""
    n = 8 if size < 64 else 4  # samples per axis
    lo, hi = MARGIN, 1.0 - MARGIN
    box = (lo, lo, hi, hi, RADIUS)
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0.0
            for sy in range(n):
                y = (py + (sy + 0.5) / n) / size
                for sx in range(n):
                    x = (px + (sx + 0.5) / n) / size
                    if not in_rounded_box(x, y, box):
                        continue
                    c = sample(x, y)
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
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "taser.ico")
    with open(path, "wb") as f:
        f.write(out)
    print(f"{path}: {len(out)} bytes, sizes {', '.join(map(str, SIZES))}")
    if len(sys.argv) > 1:
        with open(sys.argv[1], "wb") as f:
            f.write(png(512, render(512)))
        print(f"{sys.argv[1]}: 512 px preview")


if __name__ == "__main__":
    main()
