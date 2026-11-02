#!/usr/bin/env python3
"""Builds the before/after images in docs/images/ for the README.

Sources are public domain:
  rocket.jpg             SpaceX launch photo (released into the public domain),
                         as shipped with scikit-image (skimage.data.rocket)
  hubble_deep_field.jpg  NASA / Hubble (public domain), skimage.data.hubble_deep_field
The UI screenshot is generated here.

Usage: python3 scripts/make_readme_images.py --prismpx target/release/prismpx --skimage-data DIR
"""
import argparse
import os
import random
import subprocess
import tempfile

from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
BOLD = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"
OUT = os.path.join(os.path.dirname(__file__), "..", "docs", "images")


def run(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def ui_screenshot(path):
    rng = random.Random(3)
    w, h = 800, 520
    im = Image.new("RGB", (w, h), (245, 246, 248))
    d = ImageDraw.Draw(im)
    font = ImageFont.truetype(FONT, 15)
    bold = ImageFont.truetype(BOLD, 18)
    for y in range(70):
        d.line([0, y, w, y], fill=(40 + y, 90 + y, 200))
    d.text((24, 22), "Dashboard", fill=(255, 255, 255), font=bold)
    for i in range(6):
        x0, y0 = 24 + (i % 3) * 256, 100 + (i // 3) * 200
        d.rounded_rectangle([x0, y0, x0 + 236, y0 + 180], radius=14, fill=(255, 255, 255), outline=(220, 222, 226))
        d.text((x0 + 16, y0 + 16), f"Project {i + 1}", fill=(30, 30, 30), font=bold)
        d.text((x0 + 16, y0 + 46), f"{rng.randrange(20, 99)} tasks · {rng.randrange(1, 9)} people", fill=(110, 110, 110), font=font)
        for k in range(5):
            v = rng.randrange(20, 90)
            d.rounded_rectangle([x0 + 16 + k * 42, y0 + 160 - v, x0 + 46 + k * 42, y0 + 160], radius=4,
                                fill=(60 + 30 * k, 120, 220 - 25 * k))
    im.save(path)


def compose(orig, new, box, scale, left_label, right_label, out):
    a = Image.open(orig).convert("RGB").crop(box)
    b = Image.open(new).convert("RGB").crop(box)
    w, h = a.size
    a = a.resize((w * scale, h * scale), Image.NEAREST)
    b = b.resize((w * scale, h * scale), Image.NEAREST)
    pad, head = 12, 54
    canvas = Image.new("RGB", (2 * w * scale + 3 * pad, h * scale + head + pad), (255, 255, 255))
    d = ImageDraw.Draw(canvas)
    f_bold = ImageFont.truetype(BOLD, 17)
    f = ImageFont.truetype(FONT, 14)
    for i, (img, (title, sub)) in enumerate([(a, left_label), (b, right_label)]):
        x = pad + i * (w * scale + pad)
        d.text((x, 8), title, fill=(20, 20, 20), font=f_bold)
        d.text((x, 30), sub, fill=(90, 90, 90), font=f)
        canvas.paste(img, (x, head))
    canvas.save(out)


def kb(path):
    return os.path.getsize(path) / 1024


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prismpx", required=True)
    ap.add_argument("--skimage-data", required=True)
    args = ap.parse_args()
    os.makedirs(OUT, exist_ok=True)
    tmp = tempfile.mkdtemp()
    p = args.prismpx

    def score(a, b):
        pa, pb = os.path.join(tmp, "sa.png"), os.path.join(tmp, "sb.png")
        Image.open(a).convert("RGB").save(pa)
        Image.open(b).convert("RGB").save(pb)
        return float(run([p, "score", pa, pb]))

    cases = []
    # 1. Photo saved as PNG
    photo = os.path.join(tmp, "photo.png")
    Image.open(os.path.join(args.skimage_data, "hubble_deep_field.jpg")).convert("RGB").save(photo)
    cases.append(("png-photo", photo, (380, 300, 620, 470), 2))
    # 2. UI screenshot PNG
    ui = os.path.join(tmp, "ui.png")
    ui_screenshot(ui)
    cases.append(("png-ui", ui, (16, 84, 270, 296), 2))
    # 3. JPEG photo
    jpeg = os.path.join(tmp, "rocket.jpg")
    Image.open(os.path.join(args.skimage_data, "rocket.jpg")).save(jpeg, quality=95)  # a high-quality source
    cases.append(("jpeg-photo", jpeg, (300, 60, 540, 230), 2))

    lines = []
    for name, src, box, scale in cases:
        ext = os.path.splitext(src)[1]
        lossless = os.path.join(tmp, name + "-lossless" + ext)
        lossy = os.path.join(tmp, name + "-lossy" + ext)
        run([p, "--quiet", "-o", lossless, src])
        run([p, "--quiet", "--lossy", "-o", lossy, src])
        s = score(src, lossy)
        o, ll, ly = kb(src), kb(lossless), kb(lossy)
        out = os.path.join(OUT, name + ".png")
        compose(src, lossy, box, scale,
                ("Original", f"{o:.0f} KB"),
                ("prismpx --lossy", f"{ly:.0f} KB  (−{100 * (1 - ly / o):.0f}%)   SSIMULACRA 2: {s:.1f}"),
                out)
        run([p, "--quiet", "--in-place", "-e", "max", out])  # dogfood: optimize the README images
        lines.append(f"{name}: original {o:.0f} KB, lossless {ll:.0f} KB, lossy {ly:.0f} KB, score {s:.1f}")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
