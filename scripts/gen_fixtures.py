#!/usr/bin/env python3
"""Generates PNG test fixtures with an independent (pure Python) encoder.

For every fixture `name.png` it also writes `name.rgba16`: the expected decoded
pixels as little-endian u16 RGBA on the 16-bit scale. The Rust tests compare the
decoder and every optimized output against these files.

Run from the repository root:  python3 scripts/gen_fixtures.py
"""
import os
import random
import struct
import zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "prismpx", "tests", "fixtures")
ADAM7 = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def filt(ft, cur, prev, bpp):
    out = bytearray()
    for i, x in enumerate(cur):
        a = cur[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        pred = [0, a, b, (a + b) // 2, paeth(a, b, c)][ft]
        out.append((x - pred) & 0xFF)
    return bytes(out)


def pack_row(samples, depth):
    if depth == 16:
        return b"".join(struct.pack(">H", s) for s in samples)
    if depth == 8:
        return bytes(samples)
    out = bytearray((len(samples) * depth + 7) // 8)
    for i, s in enumerate(samples):
        bit = i * depth
        out[bit // 8] |= s << (8 - depth - bit % 8)
    return bytes(out)


def encode(w, h, ct, depth, samples, interlace=False, plte=None, trns=None, extra_before=(), extra_after=(), rng=None):
    """samples[y][x] is a tuple of raw channel values for the file's color type."""
    ch = CHANNELS[ct]
    bpp = max(1, ch * depth // 8)
    passes = ADAM7 if interlace else [(0, 0, 1, 1)]
    raw = bytearray()
    for x0, y0, dx, dy in passes:
        xs = list(range(x0, w, dx))
        ys = list(range(y0, h, dy))
        if not xs or not ys:
            continue
        prev = bytes(len(pack_row([0] * (len(xs) * ch), depth)))
        for y in ys:
            row = pack_row([v for x in xs for v in samples[y][x]], depth)
            ft = rng.randrange(5)
            raw.append(ft)
            raw += filt(ft, row, prev, bpp)
            prev = row
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, ct, 0, 0, 1 if interlace else 0))
    for k, d in extra_before:
        png += chunk(k, d)
    if plte is not None:
        png += chunk(b"PLTE", bytes(c for e in plte for c in e))
    if trns is not None:
        png += chunk(b"tRNS", trns)
    # Deliberately weak compression and split IDATs, like many real-world encoders.
    data = zlib.compress(bytes(raw), 1)
    for i in range(0, len(data), 97):
        png += chunk(b"IDAT", data[i:i + 97])
    for k, d in extra_after:
        png += chunk(k, d)
    return png + chunk(b"IEND", b"")


def write(name, w, h, rgba16, png):
    with open(os.path.join(OUT, name + ".png"), "wb") as f:
        f.write(png)
    with open(os.path.join(OUT, name + ".rgba16"), "wb") as f:
        f.write(b"".join(struct.pack("<HHHH", *p) for row in rgba16 for p in row))


def main():
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        os.remove(os.path.join(OUT, f))
    rng = random.Random(20261101)
    W, H = 19, 13

    def grid(fn, w=W, h=H):
        return [[fn(x, y) for x in range(w)] for y in range(h)]

    def emit(name, ct, depth, raw, rgba16, w=W, h=H, **kw):
        for interlace in (False, True):
            n = name + ("_i" if interlace else "")
            write(n, w, h, rgba16, encode(w, h, ct, depth, raw, interlace=interlace, rng=rng, **kw))

    # Grayscale at every depth, with and without a tRNS key.
    for depth in (1, 2, 4, 8, 16):
        top = (1 << depth) - 1
        vals = grid(lambda x, y: rng.randrange(top + 1))
        to16 = lambda v: v * 65535 // top
        emit(f"gray{depth}", 0, depth, grid(lambda x, y: (vals[y][x],)),
             grid(lambda x, y: (to16(vals[y][x]),) * 3 + (65535,)))
        key = vals[0][0]
        emit(f"gray{depth}_trns", 0, depth, grid(lambda x, y: (vals[y][x],)),
             grid(lambda x, y: (to16(vals[y][x]),) * 3 + (0 if vals[y][x] == key else 65535,)),
             trns=struct.pack(">H", key))

    # Palette at every depth, with partial tRNS and unused/duplicate entries.
    for depth in (1, 2, 4, 8):
        n = 1 << depth
        plte = [(rng.randrange(256), rng.randrange(256), rng.randrange(256)) for _ in range(n)]
        if n >= 4:
            plte[3] = plte[2]  # duplicate color
        alpha = [rng.choice([0, 128, 255]) for _ in range(max(1, n // 2))]
        used = n - 1 if n > 2 else n  # leave one entry unused
        idx = grid(lambda x, y: rng.randrange(used))
        a_of = lambda i: alpha[i] if i < len(alpha) else 255
        emit(f"pal{depth}", 3, depth, grid(lambda x, y: (idx[y][x],)),
             grid(lambda x, y: tuple(c * 257 for c in plte[idx[y][x]]) + (a_of(idx[y][x]) * 257,)),
             plte=plte, trns=bytes(alpha))

    # Truecolor 8/16 with a tRNS key, gray+alpha, RGBA.
    for depth in (8, 16):
        top = (1 << depth) - 1
        s = 65535 // top
        rgb = grid(lambda x, y: tuple(rng.randrange(top + 1) for _ in range(3)))
        key = rgb[1][1]
        emit(f"rgb{depth}_trns", 2, depth, rgb,
             grid(lambda x, y: tuple(v * s for v in rgb[y][x]) + (0 if rgb[y][x] == key else 65535,)),
             trns=struct.pack(">HHH", *key))
        ga = grid(lambda x, y: (rng.randrange(top + 1), rng.randrange(top + 1)))
        emit(f"ga{depth}", 4, depth, ga, grid(lambda x, y: (ga[y][x][0] * s,) * 3 + (ga[y][x][1] * s,)))
        rgba = grid(lambda x, y: tuple(rng.randrange(top + 1) for _ in range(4)))
        emit(f"rgba{depth}", 6, depth, rgba, grid(lambda x, y: tuple(v * s for v in rgba[y][x])))

    # Reducible cases: wasteful encodings the optimizer should shrink.
    g4 = grid(lambda x, y: rng.randrange(16) * 17)
    emit("rgba8_is_gray4", 6, 8, grid(lambda x, y: (g4[y][x],) * 3 + (255,)),
         grid(lambda x, y: (g4[y][x] * 257,) * 3 + (65535,)))
    colors = [(255, 0, 0, 255), (0, 255, 0, 255), (0, 0, 255, 128), (10, 20, 30, 0), (200, 200, 0, 255)]
    pick = grid(lambda x, y: colors[rng.randrange(len(colors))])
    emit("rgba8_five_colors", 6, 8, pick, grid(lambda x, y: tuple(c * 257 for c in pick[y][x])))
    r16 = grid(lambda x, y: tuple(rng.randrange(256) for _ in range(3)))
    emit("rgb16_is_8bit", 2, 16, grid(lambda x, y: tuple(v * 257 for v in r16[y][x])),
         grid(lambda x, y: tuple(v * 257 for v in r16[y][x]) + (65535,)))
    binary = grid(lambda x, y: (0, 0, 0, 0) if (x + y) % 5 == 0 else tuple(rng.randrange(1, 256) for _ in range(3)) + (255,))
    emit("rgba8_binary_alpha", 6, 8, binary, grid(lambda x, y: tuple(c * 257 for c in binary[y][x])))

    # Larger smooth image: many colors, filters matter.
    GW, GH = 96, 64
    smooth = grid(lambda x, y: ((x * 255) // GW, (y * 255) // GH, ((x + y) * 3) % 256, 255 - (x // 3)), GW, GH)
    emit("rgba8_gradient", 6, 8, smooth, grid(lambda x, y: tuple(c * 257 for c in smooth[y][x]), GW, GH), w=GW, h=GH)

    # Metadata: text, time, color, physical size, exif, bKGD and unknown chunks.
    meta_rgb = grid(lambda x, y: (x * 13 % 256, y * 19 % 256, 77))
    before = [(b"gAMA", struct.pack(">I", 45455)), (b"pHYs", struct.pack(">IIB", 2835, 2835, 1)),
              (b"eXIf", b"MM\x00\x2a\x00\x00\x00\x08\x00\x00"), (b"bKGD", struct.pack(">HHH", 1, 2, 3)),
              (b"prVt", b"private-safe-to-copy"), (b"prVT", b"private-unsafe-to-copy")]
    after = [(b"tEXt", b"Comment\x00made by gen_fixtures"), (b"tIME", struct.pack(">HBBBBB", 2026, 11, 1, 12, 0, 0))]
    write("meta_rgb8", W, H, grid(lambda x, y: tuple(c * 257 for c in meta_rgb[y][x]) + (65535,)),
          encode(W, H, 2, 8, meta_rgb, extra_before=before, extra_after=after, rng=rng))

    print("fixtures written to", os.path.normpath(OUT), "-", len(os.listdir(OUT)) // 2, "images")


if __name__ == "__main__":
    main()
