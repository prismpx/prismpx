#!/usr/bin/env python3
"""Generates JPEG test fixtures with Pillow (libjpeg / libjpeg-turbo).

Covers baseline and progressive coding, default and optimized Huffman
tables, 4:4:4 / 4:2:2 / 4:2:0, grayscale, CMYK (Adobe), restart intervals,
odd sizes, 16-bit quantization tables and metadata (EXIF with orientation,
ICC profile, XMP, comment).

Run from the repository root:  python3 scripts/gen_jpeg_fixtures.py
"""
import io
import os
import random

from PIL import Image, ImageCms, ImageDraw

OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "prismpx", "tests", "fixtures", "jpeg")


def scene(w, h, seed):
    rng = random.Random(seed)
    im = Image.new("RGB", (w, h))
    px = im.load()
    for y in range(h):
        for x in range(w):
            n = rng.randint(-12, 12)
            px[x, y] = (
                max(0, min(255, (x * 255) // max(1, w - 1) + n)),
                max(0, min(255, (y * 255) // max(1, h - 1) - n)),
                max(0, min(255, 128 + ((x * y) % 97) - 48 + n)),
            )
    d = ImageDraw.Draw(im)
    for i in range(6):
        x0, y0 = rng.randrange(w), rng.randrange(h)
        d.ellipse([x0, y0, x0 + rng.randrange(5, w // 2 + 6), y0 + rng.randrange(5, h // 2 + 6)],
                  fill=(rng.randrange(256), rng.randrange(256), rng.randrange(256)))
        d.line([rng.randrange(w), rng.randrange(h), rng.randrange(w), rng.randrange(h)], fill=(255, 255, 255), width=2)
    return im


def main():
    os.makedirs(OUT, exist_ok=True)
    for f in os.listdir(OUT):
        os.remove(os.path.join(OUT, f))
    big = scene(160, 120, 1)
    odd = scene(37, 23, 2)
    srgb = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
    exif = Image.Exif()
    exif[0x0112] = 6            # orientation: rotate 90 CW
    exif[0x010F] = "prismpx"    # Make
    exif[0x0131] = "fixture generator " * 20
    xmp = b'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF/></x:xmpmeta>'

    cases = {
        "baseline_420_q75": (big, dict(quality=75)),
        "baseline_420_optimized": (big, dict(quality=75, optimize=True)),
        "baseline_444_q92": (big, dict(quality=92, subsampling=0)),
        "baseline_422_q85": (big, dict(quality=85, subsampling=1)),
        "progressive_420_q80": (big, dict(quality=80, progressive=True)),
        "progressive_444_q95": (big, dict(quality=95, progressive=True, subsampling=0)),
        "baseline_odd_size": (odd, dict(quality=70)),
        "progressive_odd_size": (odd, dict(quality=70, progressive=True)),
        "gray_baseline": (big.convert("L"), dict(quality=80)),
        "gray_progressive": (big.convert("L"), dict(quality=80, progressive=True)),
        "cmyk_baseline": (big.convert("CMYK"), dict(quality=85)),
        "restart_rows": (big, dict(quality=80, restart_marker_rows=1)),
        "restart_blocks": (big, dict(quality=80, restart_marker_blocks=5)),
        "qtable_16bit": (big, dict(qtables=[[300 + i for i in range(64)], [400 + i for i in range(64)]])),
        "metadata_all": (big, dict(quality=85, exif=exif.tobytes(), icc_profile=srgb, xmp=xmp, comment=b"hello")),
        "tiny_8x8": (scene(8, 8, 3), dict(quality=90)),
        "q100_444": (big, dict(quality=100, subsampling=0)),
    }
    for name, (im, kw) in cases.items():
        buf = io.BytesIO()
        im.save(buf, "JPEG", **kw)
        with open(os.path.join(OUT, name + ".jpg"), "wb") as f:
            f.write(buf.getvalue())
    print("wrote", len(cases), "JPEG fixtures to", os.path.normpath(OUT))


if __name__ == "__main__":
    main()
