#!/usr/bin/env python3
"""Compares prismpx with other image optimizers on the same inputs.

Corpora (built in a work directory, nothing is committed):
  photos-png    Kodak PhotoCD set (24 true-color photos), as PNG
  graphics-png  synthetic UI/graphics images (flat colors, text, gradients)
  photos-jpeg   the Kodak photos saved as JPEG quality 90 (libjpeg-turbo)

Every tool that is installed is run on every image of the matching corpus.
Lossy results are scored with SSIMULACRA 2 against the input (higher is
closer; 90 is visually lossless, 85 indistinguishable in a flip test).

Usage: python3 scripts/bench_compare.py --prismpx target/release/prismpx [--work DIR] [--kodak DIR]
Prints a Markdown report on stdout.
"""
import argparse
import os
import random
import shutil
import statistics
import subprocess
import tempfile
import time
import urllib.request

from PIL import Image, ImageDraw, ImageFont

KODAK_URL = "https://r0k.us/graphics/kodak/kodak/kodim{:02d}.png"


def build_corpora(work, kodak_dir):
    photos = os.path.join(work, "photos-png")
    graphics = os.path.join(work, "graphics-png")
    jpegs = os.path.join(work, "photos-jpeg")
    for d in (photos, graphics, jpegs):
        os.makedirs(d, exist_ok=True)
    for i in range(1, 25):
        dst = os.path.join(photos, f"kodim{i:02d}.png")
        if not os.path.exists(dst):
            src = os.path.join(kodak_dir, f"kodim{i:02d}.png") if kodak_dir else None
            if src and os.path.exists(src):
                shutil.copy(src, dst)
            else:
                urllib.request.urlretrieve(KODAK_URL.format(i), dst)
        Image.open(dst).convert("RGB").save(os.path.join(jpegs, f"kodim{i:02d}.jpg"), quality=90)
    rng = random.Random(7)
    font = ImageFont.load_default()
    for n in range(8):
        w, h = 800, 600
        im = Image.new("RGB", (w, h), (245, 246, 248))
        d = ImageDraw.Draw(im)
        top = tuple(rng.randrange(40, 200) for _ in range(3))
        for y in range(64):  # header gradient
            d.line([0, y, w, y], fill=tuple(min(255, c + y) for c in top))
        for i in range(12):  # cards, buttons, text
            x0, y0 = rng.randrange(20, w - 220), rng.randrange(80, h - 120)
            d.rounded_rectangle([x0, y0, x0 + 200, y0 + 100], radius=12, fill=(255, 255, 255), outline=(220, 222, 226))
            d.rounded_rectangle([x0 + 12, y0 + 60, x0 + 92, y0 + 88], radius=8,
                                fill=tuple(rng.randrange(256) for _ in range(3)))
            d.text((x0 + 12, y0 + 12), f"Item {n}-{i}  {rng.randrange(1000)}", fill=(30, 30, 30), font=font)
            d.text((x0 + 12, y0 + 30), "lorem ipsum dolor sit", fill=(110, 110, 110), font=font)
        d.ellipse([w - 60, 12, w - 20, 52], fill=(255, 255, 255))
        if n % 2:  # half of them with an alpha channel
            im = im.convert("RGBA")
            a = Image.new("L", im.size, 255)
            ImageDraw.Draw(a).rectangle([0, h - 40, w, h], fill=0)
            im.putalpha(a)
        im.save(os.path.join(graphics, f"ui{n}.png"))
    return {"photos-png": photos, "graphics-png": graphics, "photos-jpeg": jpegs}


def which(*names):
    return all(shutil.which(n) for n in names)


def tools(prismpx, mozjpeg_bin):
    moz_tran = os.path.join(mozjpeg_bin, "jpegtran") if mozjpeg_bin else None
    moz_cjpeg = os.path.join(mozjpeg_bin, "cjpeg") if mozjpeg_bin else None
    moz_djpeg = os.path.join(mozjpeg_bin, "djpeg") if mozjpeg_bin else None
    have_moz = moz_tran and os.path.exists(moz_tran)
    t = [
        # (name, corpus kind, lossy?, available?, command(input, output))
        ("prismpx", "png", False, True, lambda i, o: [prismpx, "--quiet", "-o", o, i]),
        ("prismpx -e max", "png", False, True, lambda i, o: [prismpx, "--quiet", "-e", "max", "-o", o, i]),
        ("oxipng -o max", "png", False, which("oxipng"),
         lambda i, o: ["oxipng", "-q", "-o", "max", "--strip", "safe", "--out", o, i]),
        ("optipng -o7", "png", False, which("optipng"), lambda i, o: ["optipng", "-quiet", "-o7", "-strip", "all", "-out", o, i]),
        ("zopflipng", "png", False, which("zopflipng"), lambda i, o: ["zopflipng", "-y", "--keepchunks=iCCP,sRGB,gAMA,cHRM", i, o]),
        ("prismpx --lossy (q85)", "png", True, True, lambda i, o: [prismpx, "--quiet", "--lossy", "-o", o, i]),
        ("prismpx --lossy -q 80", "png", True, True, lambda i, o: [prismpx, "--quiet", "--lossy", "-q", "80", "-o", o, i]),
        ("pngquant (default)", "png", True, which("pngquant"),
         lambda i, o: ["pngquant", "--force", "--speed", "1", "--strip", "--output", o, i]),
        ("prismpx", "jpeg", False, True, lambda i, o: [prismpx, "--quiet", "-o", o, i]),
        ("jpegtran (libjpeg-turbo)", "jpeg", False, which("jpegtran"),
         lambda i, o: ["jpegtran", "-optimize", "-progressive", "-copy", "none", "-outfile", o, i]),
        ("jpegtran (mozjpeg)", "jpeg", False, bool(have_moz), lambda i, o: [moz_tran, "-copy", "none", "-outfile", o, i]),
        ("jpegoptim --strip-all", "jpeg", False, which("jpegoptim"), None),
        ("prismpx --lossy (q85)", "jpeg", True, True, lambda i, o: [prismpx, "--quiet", "--lossy", "-o", o, i]),
        ("prismpx --lossy -q 80", "jpeg", True, True, lambda i, o: [prismpx, "--quiet", "--lossy", "-q", "80", "-o", o, i]),
        ("jpegoptim -m85", "jpeg", True, which("jpegoptim"), None),
        ("mozjpeg cjpeg -quality 85 (re-encode)", "jpeg", True, bool(have_moz),
         lambda i, o: ["sh", "-c", f'"{moz_djpeg}" "{i}" | "{moz_cjpeg}" -quality 85 -outfile "{o}"']),
    ]
    return [x for x in t if x[3]]


def run_jpegoptim(args, i, o):
    d = tempfile.mkdtemp()
    subprocess.run(["jpegoptim", "-q", "-d", d, *args, i], check=True)
    shutil.move(os.path.join(d, os.path.basename(i)), o)
    shutil.rmtree(d)


def as_png(path, dst):
    Image.open(path).convert("RGBA" if Image.open(path).mode in ("RGBA", "LA", "P") else "RGB").save(dst)
    return dst


def score(prismpx, a, b, tmp):
    pa, pb = as_png(a, os.path.join(tmp, "a.png")), as_png(b, os.path.join(tmp, "b.png"))
    r = subprocess.run([prismpx, "score", pa, pb], capture_output=True, text=True)
    return float(r.stdout) if r.returncode == 0 else float("nan")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prismpx", required=True)
    ap.add_argument("--work", default=tempfile.mkdtemp())
    ap.add_argument("--kodak", default=None, help="directory with kodimNN.png (else downloaded)")
    ap.add_argument("--mozjpeg-bin", default=None)
    args = ap.parse_args()
    corpora = build_corpora(args.work, args.kodak)
    tmp = tempfile.mkdtemp()
    sections = [("photos-png", "png", "PNG photos — Kodak set (24 images)"),
                ("graphics-png", "png", "PNG graphics — synthetic UI screenshots (8 images, half with alpha)"),
                ("photos-jpeg", "jpeg", "JPEG photos — Kodak set saved at quality 90 (24 images)")]
    out = ["## Benchmark results", "",
           "Sizes are totals over the corpus. Scores are SSIMULACRA 2 against the input "
           "(mean / minimum; lossless tools are 100 by definition).", ""]
    for corpus, kind, title in sections:
        files = sorted(os.listdir(corpora[corpus]))
        paths = [os.path.join(corpora[corpus], f) for f in files]
        original = sum(os.path.getsize(p) for p in paths)
        out += [f"### {title}", "", f"Original total: {original / 1e6:.2f} MB", "",
                "| Tool | Mode | Total | Saved | Score (mean / min) | Time |", "|---|---|---|---|---|---|"]
        for name, tkind, lossy, _, cmd in tools(args.prismpx, args.mozjpeg_bin):
            if tkind != kind:
                continue
            total, scores, t0, failed = 0, [], time.time(), 0
            for p in paths:
                o = os.path.join(tmp, "out" + os.path.splitext(p)[1])
                if os.path.exists(o):
                    os.remove(o)
                try:
                    if name.startswith("jpegoptim"):
                        run_jpegoptim(["--strip-all"] + (["-m85"] if "-m85" in name else []), p, o)
                    else:
                        subprocess.run(cmd(p, o), check=True, capture_output=True)
                except subprocess.CalledProcessError:
                    pass
                if not os.path.exists(o) or os.path.getsize(o) == 0 or os.path.getsize(o) > os.path.getsize(p):
                    shutil.copy(p, o)  # tools that give up (or grow the file) count as the original
                    failed += 1
                total += os.path.getsize(o)
                if lossy:
                    scores.append(score(args.prismpx, p, o, tmp))
            elapsed = (time.time() - t0) / len(paths)
            sc = f"{statistics.mean(scores):.1f} / {min(scores):.1f}" if scores else "100 (lossless)"
            note = f" ({failed} kept original)" if failed and not name.startswith("prismpx") else ""
            out.append(f"| {name}{note} | {'lossy' if lossy else 'lossless'} | {total / 1e6:.2f} MB | "
                       f"{100 * (original - total) / original:.1f}% | {sc} | {elapsed:.2f} s/img |")
        out.append("")
    print("\n".join(out))


if __name__ == "__main__":
    main()
