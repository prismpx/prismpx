#!/usr/bin/env python3
"""Checks that prismpx's lossless JPEG output decodes (with libjpeg, via
Pillow) to exactly the same pixels as the input.

Usage: python3 scripts/check_jpeg_pixels.py <prismpx binary> <jpeg>...
"""
import subprocess
import sys
import tempfile

from PIL import Image


def pixels(path):
    im = Image.open(path)
    im.load()
    return im.mode, im.size, im.tobytes()


def main():
    binary, files = sys.argv[1], sys.argv[2:]
    failures = 0
    for f in files:
        with tempfile.NamedTemporaryFile(suffix=".jpg") as out:
            r = subprocess.run([binary, "--quiet", "-m", "keep-all", "-o", out.name, f], capture_output=True, text=True)
            if r.returncode != 0:
                print(f"FAIL {f}: prismpx error: {r.stderr.strip()}")
                failures += 1
                continue
            if pixels(f) != pixels(out.name):
                print(f"FAIL {f}: decoded pixels differ")
                failures += 1
            else:
                print(f"ok   {f}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
