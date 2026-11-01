# prismpx

**Make images smaller without changing how they look.**

prismpx is a same-format image optimizer: a PNG or JPEG goes in, a smaller file of the same format comes out.

- **Lossless mode** (default): every pixel decodes to exactly the same value.
- **Visually-lossless mode** (`--lossy`): pixels may change, but only as far as a
  perceptual metric (SSIMULACRA 2) says is invisible at the chosen quality.

It is written in dependency-free Rust with a C ABI, so it can be wrapped for any language.

> Status: early. PNG and JPEG work in both modes. See the [roadmap](#roadmap).

## Results

20 real-world PNGs (photos, UI, textures), total size:

| Mode | Size | Saved |
|---|---|---|
| original | 2.86 MB | |
| lossless | 2.47 MB | 13.7% |
| `--lossy -q 90` (visually lossless) | 1.66 MB | 42.2% |
| `--lossy -q 85` (default; indistinguishable in a flip test) | 1.06 MB | 63.1% |
| `--lossy -q 80` (indistinguishable side by side) | 0.78 MB | 72.6% |

Every lossy output met its SSIMULACRA 2 target.

40 real-world JPEGs (3.63 MB, already web-compressed):

| Mode | Saved |
|---|---|
| lossless (pixel-identical under libjpeg) | 8.0% |
| `--lossy -q 90` | 11.0% |
| `--lossy -q 85` (default) | 21.6% |
| `--lossy -q 80` | 32.1% |

High-quality sources save more: five photos saved at JPEG quality 95 shrink by
31.4% at `-q 85`. JPEG scores compare against the decoded input, so an
already heavily compressed JPEG may have no headroom; it is then optimized
losslessly.

## What it does (PNG)

| Step | Technique |
|---|---|
| Color reduction | RGBA → RGB when fully opaque, RGB → gray when R=G=B, ≤256 colors → palette, 16 → 8 bit when exact, gray 8 → 4/2/1 bit when exact, a single transparent color → `tRNS` key instead of an alpha channel |
| Palette ordering | transparent entries first (short `tRNS`), then by frequency or brightness; both tried |
| Row filters | fixed filters plus adaptive per-row selection (min-sum and min-entropy heuristics), chosen by trial compression |
| DEFLATE | own encoder: hash-chain LZ77 with lazy matching, per-block stored/fixed/dynamic choice, optimal length-limited Huffman codes (package-merge) |
| Metadata | policy-based chunk filtering that keeps the file valid (color-type-dependent and unsafe-to-copy chunks are dropped when they would become wrong) |
| Interlacing | Adam7 input is accepted; output is non-interlaced |
| Safety | output is decoded and compared with the input before it is returned; the original is returned if nothing smaller is found |

Animated PNG (APNG) is detected and left untouched for now.

## What it does (JPEG, lossless)

| Step | Technique |
|---|---|
| Decoding | baseline, extended and progressive Huffman JPEGs (including successive approximation and restart intervals) are decoded to quantized DCT coefficients; nothing is ever dequantized or re-quantized |
| Huffman tables | optimal per-scan tables from the actual symbol statistics, length-limited to 16 bits with package-merge, with the all-ones code reserved as JPEG requires |
| Layout | a baseline file and a progressive file (spectral selection; each component's AC band split chosen by size from several candidates) are both built; the smaller wins. Restart markers are dropped |
| Metadata | JFIF, ICC profiles and the Adobe segment (which controls color decoding) are always kept. EXIF is reduced to its orientation tag unless `keep-all`. XMP, comments, IPTC and other APPn segments go unless `keep-all`. Multi-picture (MPF) extras after EOI are always dropped |
| Safety | the output is decoded again and its coefficients compared with the input's |

Arithmetic-coded, lossless (SOF3), hierarchical and 12-bit JPEGs are reported as unsupported.

## What it does (JPEG, visually lossless)

| Step | Technique |
|---|---|
| No pixel round trip | everything happens on the quantized DCT coefficients, so there is no second generation of blocking and ringing |
| Requantization | quantization steps are scaled up with a frequency weighting (larger for high frequencies, after Watson's DCTune), and every coefficient is re-rounded from its exact dequantized value. Chroma tables are scaled harder for 4:4:4 images |
| Rate-weighted zeroing | ±1 coefficients after long zero runs are dropped when their distortion is small relative to their estimated bit cost (a simplified rate-distortion decision in the spirit of trellis quantization). Plain requantization and requantization with zeroing are both searched |
| Quality control | the image is rendered (float IDCT, fancy upsampling, JFIF color) and scored with SSIMULACRA 2 against the rendered input; the strength is bisected to the largest value that meets the target |
| Encoding | the candidate goes through the lossless JPEG encoder (optimal Huffman tables, baseline or progressive); the lossless result is kept if it is smaller |

CMYK/YCCK JPEGs and files that redefine quantization tables mid-stream are optimized losslessly only.

## What it does (PNG, visually lossless)

| Step | Technique |
|---|---|
| Quality control | [SSIMULACRA 2](https://github.com/cloudinary/ssimulacra2) (Sneyers, 2023), ported from the libjxl reference implementation. Candidates below the target score are rejected. |
| Palette | variance-minimizing median cut (after Wu, 1991) in premultiplied OKLab + alpha, refined with weighted k-means |
| Dithering | serpentine Floyd–Steinberg whose strength fades out on edges and texture, so gradients stay smooth without dither noise that hurts compression; dithered and undithered remaps are both scored |
| Search | binary search over palette sizes (2–256) for the smallest palette meeting the target; 16-bit images also try plain 8-bit |
| Near-lossless | JPEG-LS-style bounded-error coding (Weinberger et al., LOCO-I, 2000) inside PNG's filter loop: each filter residual is quantized to a multiple of 2δ+1 while the encoder tracks the reconstruction, so every sample decodes within ±δ of the original and residuals compress far better. Alpha stays exact; color under fully transparent pixels is free. Binary search picks the largest δ that meets the target. Output is a standard PNG. |
| Encoding | every candidate goes through the lossless pipeline; the smallest file wins, and the lossless result is kept if nothing beats it |

## Usage

### CLI

```sh
cargo install --path crates/prismpx-cli

prismpx image.png                     # dry run: report savings only
prismpx -o small.png image.png        # write to a new file
prismpx --in-place -e max assets/*.png
prismpx --lossy -o small.png image.png        # visually lossless, quality 85
prismpx --lossy -q 80 -o smaller.png image.png
prismpx score original.png candidate.png      # SSIMULACRA 2 score
```

Options: `--lossy`, `--quality <score>`, `--effort fast|default|max`,
`--metadata safe|keep-all|color-only`, `--clean-transparent`, `--no-verify`.

### Rust

```rust
let input = std::fs::read("in.png")?;
let result = prismpx::optimize(&input, &prismpx::Options::default())?;
println!("{} -> {} bytes: {}", result.original_size, result.size(), result.strategy);
std::fs::write("out.png", &result.data)?;
```

### C (and other languages via FFI)

`cargo build --release -p prismpx-capi` produces `libprismpx.{so,dylib,dll}` and a
static library. The header is [`crates/prismpx-capi/include/prismpx.h`](crates/prismpx-capi/include/prismpx.h);
[`tests/smoke.c`](crates/prismpx-capi/tests/smoke.c) is a complete example.

## Options

| Option | Default | Meaning |
|---|---|---|
| `mode` | `Lossless` | `VisuallyLossless` allows changes that keep the SSIMULACRA 2 score at or above `quality` |
| `quality` | `85.0` | SSIMULACRA 2 target: 90 visually lossless, 85 excellent, 80 very high, 70 high |
| `effort` | `Default` | `Fast`, `Default`, `Max`: more filter strategies and longer LZ77 searches |
| `metadata` | `Safe` | `Safe` keeps color data (`iCCP`, `sRGB`, `gAMA`, `cHRM`, `cICP`, …), `pHYs` and `eXIf`; `ColorOnly` keeps only color data; `KeepAll` keeps everything that remains valid |
| `verify` | `true` | decode the output and compare it with the input |
| `clean_transparent` | `false` | zero the RGB of fully transparent pixels: invisible, but it changes stored values |

## Repository layout

```
crates/
  prismpx/        core library (no dependencies)
    src/deflate/  DEFLATE/zlib codec
    src/png/      PNG parser, decoder, optimizer, palette quantizer
    src/jpeg/     JPEG decoder (to DCT coefficients) and lossless re-encoder
    src/metrics/  SSIMULACRA 2
    tests/        fixture tests (fixtures generated by scripts/gen_fixtures.py)
  prismpx-capi/   C ABI (cdylib + staticlib) and header
  prismpx-cli/    `prismpx` command-line tool
scripts/          fixture generators and an independent libjpeg pixel check
```

## Testing

```sh
cargo test --workspace
python3 scripts/gen_fixtures.py        # regenerate PNG fixtures (Python stdlib only)
python3 scripts/gen_jpeg_fixtures.py   # regenerate JPEG fixtures (needs Pillow)
python3 scripts/check_jpeg_pixels.py target/release/prismpx some/*.jpg   # libjpeg pixel check
```

Fixtures cover every PNG color type and bit depth, `tRNS` variants, Adam7
interlacing, palette edge cases and metadata chunks. Expected pixels are produced
by an independent Python encoder, not by prismpx itself.

## Roadmap

- PNG: Zopfli-style optimal LZ77 parsing; faster perceptual search; rate-aware
  residual selection in near-lossless mode
- JPEG lossless: progressive scans with successive approximation (mozjpeg-style scan search)
- JPEG visually lossless: full trellis quantization with a real rate model; faster search
- WebP
- Bindings: Java, Python, Node, WASM, Swift/Kotlin

## License

SSIMULACRA 2 is ported from libjxl under the BSD 3-Clause license; see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Dependencies must be compatible with
this dual license; GPL code is not accepted.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work shall be dual licensed as above, without any additional
terms or conditions.
