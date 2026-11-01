//! Choosing and writing the smallest exact PNG representation.

use std::collections::HashMap;

use super::decode::{Chunk, Ihdr, Parsed};
use super::filter::{self, Strategy};
use super::{Image, SIGNATURE};
use crate::checksum::{crc32, crc32_update};
use crate::deflate::{zlib_compress, Level};
use crate::options::{Effort, MetadataPolicy, Options};

/// An output color representation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// `key`: tRNS transparent gray value at the target bit depth.
    Gray {
        depth: u8,
        key: Option<u16>,
    },
    GrayAlpha {
        depth: u8,
    },
    Rgb {
        depth: u8,
        key: Option<[u16; 3]>,
    },
    Rgba {
        depth: u8,
    },
    Palette {
        depth: u8,
        entries: Vec<[u8; 4]>,
    },
}

impl Target {
    fn color_type(&self) -> u8 {
        match self {
            Target::Gray { .. } => 0,
            Target::Rgb { .. } => 2,
            Target::Palette { .. } => 3,
            Target::GrayAlpha { .. } => 4,
            Target::Rgba { .. } => 6,
        }
    }

    fn depth(&self) -> u8 {
        match *self {
            Target::Gray { depth, .. }
            | Target::GrayAlpha { depth }
            | Target::Rgb { depth, .. }
            | Target::Rgba { depth }
            | Target::Palette { depth, .. } => depth,
        }
    }

    fn channels(&self) -> usize {
        match self {
            Target::Gray { .. } | Target::Palette { .. } => 1,
            Target::GrayAlpha { .. } => 2,
            Target::Rgb { .. } => 3,
            Target::Rgba { .. } => 4,
        }
    }

    fn describe(&self) -> String {
        let d = self.depth();
        match self {
            Target::Gray { key: None, .. } => format!("gray {d}-bit"),
            Target::Gray { key: Some(_), .. } => format!("gray {d}-bit + tRNS key"),
            Target::GrayAlpha { .. } => format!("gray+alpha {d}-bit"),
            Target::Rgb { key: None, .. } => format!("RGB {d}-bit"),
            Target::Rgb { key: Some(_), .. } => format!("RGB {d}-bit + tRNS key"),
            Target::Rgba { .. } => format!("RGBA {d}-bit"),
            Target::Palette { entries, .. } => {
                format!("palette {d}-bit ({} colors)", entries.len())
            }
        }
    }
}

fn describe_ihdr(h: &Ihdr) -> String {
    let kind = match h.color {
        0 => "gray",
        2 => "RGB",
        3 => "palette",
        4 => "gray+alpha",
        _ => "RGBA",
    };
    let interlace = if h.interlace == 1 { ", interlaced" } else { "" };
    format!("{kind} {}-bit{interlace}", h.depth)
}

/// Facts about the pixels that decide which exact representations exist.
struct Analysis {
    gray: bool,
    opaque: bool,
    /// Single fully transparent color usable as a tRNS key, if any.
    key: Option<[u16; 3]>,
    /// Distinct RGBA colors with counts, if 8-bit and at most 256.
    colors: Option<Vec<([u8; 4], u32)>>,
    /// Smallest gray bit depth that represents every value exactly.
    gray_depth: u8,
}

fn analyze(img: &Image) -> Analysis {
    let max = img.max_value();
    let (mut gray, mut opaque, mut binary_alpha) = (true, true, true);
    let mut transparent: Option<[u16; 3]> = None;
    let mut multi_transparent = false;
    let mut divisible = [true; 3]; // representable at 1, 2, 4 bits
    let mut counts: Option<HashMap<u32, u32>> = (!img.bit16).then(HashMap::new);
    let mut last: Option<(u32, u32)> = None; // run cache to avoid hashing every pixel

    for p in img.px.chunks_exact(4) {
        let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
        if gray && !(r == g && g == b) {
            gray = false;
        }
        if a != max {
            opaque = false;
            if a != 0 {
                binary_alpha = false;
            } else {
                match transparent {
                    None => transparent = Some([r, g, b]),
                    Some(c) if c != [r, g, b] => multi_transparent = true,
                    _ => {}
                }
            }
        }
        if gray && !img.bit16 {
            divisible[0] &= r % 255 == 0;
            divisible[1] &= r % 85 == 0;
            divisible[2] &= r % 17 == 0;
        }
        if let Some(map) = counts.as_mut() {
            let key = (r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | a as u32;
            match last {
                Some((k, ref mut n)) if k == key => *n += 1,
                _ => {
                    if let Some((k, n)) = last.take() {
                        *map.entry(k).or_insert(0) += n;
                    }
                    last = Some((key, 1));
                    if map.len() > 256 {
                        counts = None;
                        last = None;
                    }
                }
            }
        }
    }
    if let (Some(map), Some((k, n))) = (counts.as_mut(), last) {
        *map.entry(k).or_insert(0) += n;
    }
    let colors = counts.filter(|m| m.len() <= 256).map(|m| {
        m.into_iter()
            .map(|(k, n)| {
                (
                    [(k >> 24) as u8, (k >> 16) as u8, (k >> 8) as u8, k as u8],
                    n,
                )
            })
            .collect()
    });

    let key = match transparent {
        Some(c) if !opaque && binary_alpha && !multi_transparent => {
            let clash = img
                .px
                .chunks_exact(4)
                .any(|p| p[3] == max && [p[0], p[1], p[2]] == c);
            (!clash).then_some(c)
        }
        _ => None,
    };
    let gray_depth = if img.bit16 {
        16
    } else if divisible[0] {
        1
    } else if divisible[1] {
        2
    } else if divisible[2] {
        4
    } else {
        8
    };
    Analysis {
        gray,
        opaque,
        key,
        colors,
        gray_depth,
    }
}

/// Scale factor from canonical 8-bit gray to a lower bit depth.
fn gray_scale(depth: u8) -> u16 {
    match depth {
        1 => 255,
        2 => 85,
        4 => 17,
        _ => 1,
    }
}

fn palette_depth(n: usize) -> u8 {
    match n {
        0..=2 => 1,
        3..=4 => 2,
        5..=16 => 4,
        _ => 8,
    }
}

fn luma(c: &[u8; 4]) -> u32 {
    299 * c[0] as u32 + 587 * c[1] as u32 + 114 * c[2] as u32
}

/// Palette with transparent entries first (keeps tRNS short), then ordered
/// by frequency or by brightness.
fn build_palette(colors: &[([u8; 4], u32)], by_luma: bool) -> Vec<[u8; 4]> {
    let mut v = colors.to_vec();
    v.sort_by(|x, y| {
        let (tx, ty) = (x.0[3] < 255, y.0[3] < 255);
        ty.cmp(&tx)
            .then_with(|| {
                if by_luma {
                    luma(&x.0).cmp(&luma(&y.0))
                } else {
                    y.1.cmp(&x.1)
                }
            })
            .then(x.0.cmp(&y.0))
    });
    v.into_iter().map(|c| c.0).collect()
}

fn candidates(img: &Image, a: &Analysis, effort: Effort) -> Vec<Target> {
    let depth = if img.bit16 { 16 } else { 8 };
    let mut out = Vec::new();
    if a.gray {
        out.push(if a.opaque {
            Target::Gray {
                depth: a.gray_depth,
                key: None,
            }
        } else if let Some(k) = a.key {
            Target::Gray {
                depth: a.gray_depth,
                key: Some(k[0] / gray_scale(a.gray_depth)),
            }
        } else {
            Target::GrayAlpha { depth }
        });
    } else {
        out.push(if a.opaque {
            Target::Rgb { depth, key: None }
        } else if a.key.is_some() {
            Target::Rgb { depth, key: a.key }
        } else {
            Target::Rgba { depth }
        });
    }
    if let Some(colors) = &a.colors {
        let pd = palette_depth(colors.len());
        out.push(Target::Palette {
            depth: pd,
            entries: build_palette(colors, false),
        });
        if effort != Effort::Fast {
            let by_luma = build_palette(colors, true);
            if !out
                .iter()
                .any(|t| matches!(t, Target::Palette { entries, .. } if *entries == by_luma))
            {
                out.push(Target::Palette {
                    depth: pd,
                    entries: by_luma,
                });
            }
        }
    }
    out
}

/// Packs pixels into unfiltered scanlines. Returns (data, row length, filter bpp).
fn pack(img: &Image, t: &Target) -> (Vec<u8>, usize, usize) {
    let (w, h) = (img.width as usize, img.height as usize);
    let depth = t.depth() as usize;
    let bits_pp = depth * t.channels();
    let row_len = (w * bits_pp).div_ceil(8);
    let mut out = vec![0u8; row_len * h];
    let index: HashMap<[u8; 4], u8> = match t {
        Target::Palette { entries, .. } => entries
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i as u8))
            .collect(),
        _ => HashMap::new(),
    };
    let scale = gray_scale(t.depth());

    let put = |row: &mut [u8], bit: &mut usize, v: u16| {
        match depth {
            16 => {
                row[*bit / 8] = (v >> 8) as u8;
                row[*bit / 8 + 1] = v as u8;
            }
            8 => row[*bit / 8] = v as u8,
            d => row[*bit / 8] |= (v as u8) << (8 - d - *bit % 8),
        }
        *bit += depth;
    };

    for y in 0..h {
        let row = &mut out[y * row_len..(y + 1) * row_len];
        let mut bit = 0;
        for x in 0..w {
            let p = &img.px[(y * w + x) * 4..(y * w + x) * 4 + 4];
            match t {
                Target::Gray { .. } => put(row, &mut bit, p[0] / scale),
                Target::GrayAlpha { .. } => {
                    put(row, &mut bit, p[0]);
                    put(row, &mut bit, p[3]);
                }
                Target::Rgb { .. } => {
                    for &v in &p[..3] {
                        put(row, &mut bit, v);
                    }
                }
                Target::Rgba { .. } => {
                    for &v in p {
                        put(row, &mut bit, v);
                    }
                }
                Target::Palette { .. } => {
                    let c = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
                    put(row, &mut bit, index[&c] as u16);
                }
            }
        }
    }
    (out, row_len, bits_pp.div_ceil(8))
}

fn strategies(t: &Target, effort: Effort) -> Vec<Strategy> {
    let mut s = vec![Strategy::Fixed(0)];
    let filters_help = t.depth() >= 8 && !matches!(t, Target::Palette { .. });
    if filters_help || effort != Effort::Fast {
        s.push(Strategy::MinSum);
    }
    if filters_help && effort != Effort::Fast {
        s.push(Strategy::Entropy);
    }
    if effort == Effort::Max {
        s.extend((1..=4).map(Strategy::Fixed));
        if !filters_help {
            s.push(Strategy::Entropy);
        }
    }
    s
}

fn levels(effort: Effort) -> (Level, Level, &'static str) {
    match effort {
        Effort::Fast => (Level::FAST, Level::FAST, "fast"),
        Effort::Default => (Level::FAST, Level::DEFAULT, "default"),
        Effort::Max => (Level::DEFAULT, Level::MAX, "max"),
    }
}

/// Tries each filter strategy with a quick trial compression, then compresses
/// the winner at full effort.
fn compress(
    raw: &[u8],
    row_len: usize,
    bpp: usize,
    t: &Target,
    effort: Effort,
) -> (Strategy, Vec<u8>) {
    let (trial, final_level, _) = levels(effort);
    let mut best: Option<(Strategy, Vec<u8>, usize)> = None;
    for s in strategies(t, effort) {
        let filtered = filter::apply(raw, row_len, bpp, s);
        let size = zlib_compress(&filtered, &trial).len();
        if best.as_ref().is_none_or(|b| size < b.2) {
            best = Some((s, filtered, size));
        }
    }
    let (s, filtered, _) = best.expect("at least one strategy");
    (s, zlib_compress(&filtered, &final_level))
}

const COLOR_CHUNKS: [&[u8; 4]; 7] = [
    b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"cICP", b"mDCV", b"cLLI",
];
/// Chunks whose contents depend on the color type, bit depth or palette.
const TYPE_DEPENDENT: [&[u8; 4]; 3] = [b"bKGD", b"sBIT", b"hIST"];
const OTHER_KNOWN: [&[u8; 4]; 11] = [
    b"pHYs", b"sPLT", b"eXIf", b"tEXt", b"zTXt", b"iTXt", b"tIME", b"oFFs", b"pCAL", b"sCAL",
    b"sTER",
];

fn keep_chunk(kind: &[u8; 4], policy: MetadataPolicy, type_changed: bool) -> bool {
    if TYPE_DEPENDENT.contains(&kind) {
        return !type_changed && policy == MetadataPolicy::KeepAll;
    }
    let color = COLOR_CHUNKS.contains(&kind);
    match policy {
        MetadataPolicy::ColorOnly => color,
        // eXIf is kept because it may carry orientation, which changes how the image looks.
        MetadataPolicy::Safe => color || kind == b"pHYs" || kind == b"eXIf",
        MetadataPolicy::KeepAll => {
            // Unknown chunks may only be copied after edits if marked safe-to-copy.
            color || OTHER_KNOWN.contains(&kind) || kind[3].is_ascii_lowercase()
        }
    }
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32_update(crc32(kind), data).to_be_bytes());
}

fn assemble(img: &Image, t: &Target, idat: &[u8], chunks: &[&Chunk]) -> Vec<u8> {
    let mut out = Vec::with_capacity(idat.len() + 1024);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[t.depth(), t.color_type(), 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);

    let early = |c: &&&Chunk| {
        c.before_idat && (c.before_plte || COLOR_CHUNKS.contains(&&c.kind) || &c.kind == b"sBIT")
    };
    for c in chunks.iter().filter(early) {
        write_chunk(&mut out, &c.kind, &c.data);
    }

    match t {
        Target::Palette { entries, .. } => {
            let plte: Vec<u8> = entries.iter().flat_map(|e| [e[0], e[1], e[2]]).collect();
            write_chunk(&mut out, b"PLTE", &plte);
            if let Some(last) = entries.iter().rposition(|e| e[3] < 255) {
                let trns: Vec<u8> = entries[..=last].iter().map(|e| e[3]).collect();
                write_chunk(&mut out, b"tRNS", &trns);
            }
        }
        Target::Gray { key: Some(k), .. } => write_chunk(&mut out, b"tRNS", &k.to_be_bytes()),
        Target::Rgb { key: Some(k), .. } => {
            let d: Vec<u8> = k.iter().flat_map(|v| v.to_be_bytes()).collect();
            write_chunk(&mut out, b"tRNS", &d);
        }
        _ => {}
    }

    for c in chunks.iter().filter(|c| c.before_idat && !early(c)) {
        write_chunk(&mut out, &c.kind, &c.data);
    }
    write_chunk(&mut out, b"IDAT", idat);
    for c in chunks.iter().filter(|c| !c.before_idat) {
        write_chunk(&mut out, &c.kind, &c.data);
    }
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// Encodes every candidate representation and returns the smallest PNG with a
/// description of what was chosen.
pub(super) fn encode_best(img: &Image, parsed: &Parsed, opts: &Options) -> (Vec<u8>, String) {
    let analysis = analyze(img);
    let orig = &parsed.ihdr;
    let mut best: Option<(Vec<u8>, String)> = None;

    for t in candidates(img, &analysis, opts.effort) {
        let (raw, row_len, bpp) = pack(img, &t);
        let (strategy, idat) = compress(&raw, row_len, bpp, &t, opts.effort);
        let type_changed = matches!(t, Target::Palette { .. })
            || (t.color_type(), t.depth()) != (orig.color, orig.depth);
        let (kept, dropped): (Vec<&Chunk>, Vec<&Chunk>) = parsed
            .ancillary
            .iter()
            .partition(|c| keep_chunk(&c.kind, opts.metadata, type_changed));
        let png = assemble(img, &t, &idat, &kept);

        if best.as_ref().is_none_or(|b| png.len() < b.0.len()) {
            let mut desc = format!(
                "png: {} -> {}, filter={}, deflate={}",
                describe_ihdr(orig),
                t.describe(),
                strategy.name(),
                levels(opts.effort).2
            );
            if !dropped.is_empty() {
                let mut names: Vec<String> = dropped
                    .iter()
                    .map(|c| String::from_utf8_lossy(&c.kind).into_owned())
                    .collect();
                names.dedup();
                desc.push_str(&format!(", removed chunks: {}", names.join(" ")));
            }
            best = Some((png, desc));
        }
    }
    best.expect("at least one candidate")
}

// ---------------------------------------------------------------------------
// Near-lossless encoding
//
// JPEG-LS style bounded-error coding (Weinberger, Seroussi, Sapiro, "The
// LOCO-I lossless image compression algorithm", IEEE TIP 2000, near-lossless
// mode) adapted to PNG's filter loop: each filter residual is quantized to a
// multiple of 2·delta+1 while the encoder tracks the reconstructed pixels, so
// every sample decodes to within ±delta of the original and the residual
// alphabet shrinks dramatically, which DEFLATE rewards. Any standard PNG
// decoder reads the result; no special decoder is needed.
// ---------------------------------------------------------------------------

/// A near-lossless filtered image, ready to compress.
pub(super) struct NearLossless {
    target: Target,
    filtered: Vec<u8>,
    /// What a decoder will reconstruct.
    pub recon: Image,
    /// Human-readable description of the error bounds.
    pub label: String,
}

#[inline]
fn predict(ft: u8, a: u8, b: u8, c: u8) -> u8 {
    match ft {
        0 => 0,
        1 => a,
        2 => b,
        3 => ((a as u16 + b as u16) / 2) as u8,
        _ => filter::paeth(a, b, c),
    }
}

/// Quantizes one row with filter `ft`. `tol[i]` is the allowed error for byte i
/// (0 = exact, 255 = any value). Writes residual bytes and the reconstruction.
fn quantize_row(
    ft: u8,
    cur: &[u8],
    prev: &[u8],
    bpp: usize,
    tol: &[u8],
    res: &mut Vec<u8>,
    rec: &mut [u8],
) {
    res.clear();
    for i in 0..cur.len() {
        let a = if i >= bpp { rec[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let pred = predict(ft, a, b, c) as i32;
        let x = cur[i] as i32;
        let r = x - pred;
        let q = match tol[i] {
            0 => r,
            255 => 0,
            d => {
                let d = d as i32;
                let step = 2 * d + 1;
                let mut q = (r as f32 / step as f32).round() as i32 * step;
                if pred + q < 0 {
                    q += step;
                }
                if pred + q > 255 {
                    q -= step;
                }
                if (pred + q - x).abs() > d || !(0..=255).contains(&(pred + q)) {
                    r
                } else {
                    q
                }
            }
        };
        rec[i] = (pred + q) as u8;
        res.push(q as u8);
    }
}

fn minsum(row: &[u8]) -> u64 {
    row.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum()
}

/// Filters `img` (8-bit) with residual quantization: every color sample
/// decodes to within ±`delta`. Alpha is kept exact; color under fully
/// transparent pixels is free.
pub(super) fn near_lossless(img: &Image, delta: u8) -> NearLossless {
    debug_assert!(!img.bit16);
    let gray = img.px.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]);
    let opaque = img.px.chunks_exact(4).all(|p| p[3] == 255);
    let target = match (gray, opaque) {
        (true, true) => Target::Gray {
            depth: 8,
            key: None,
        },
        (true, false) => Target::GrayAlpha { depth: 8 },
        (false, true) => Target::Rgb {
            depth: 8,
            key: None,
        },
        (false, false) => Target::Rgba { depth: 8 },
    };
    let ch = target.channels();
    let (raw, row_len, bpp) = pack(img, &target);
    let (w, h) = (img.width as usize, img.height as usize);

    let mut filtered = Vec::with_capacity(h * (row_len + 1));
    let mut prev = vec![0u8; row_len];
    let mut tol = vec![0u8; row_len];
    let mut best_res = Vec::with_capacity(row_len);
    let mut best_rec = vec![0u8; row_len];
    let mut res = Vec::with_capacity(row_len);
    let mut rec = vec![0u8; row_len];
    let mut recon_rows = vec![0u8; h * row_len];

    for y in 0..h {
        let cur = &raw[y * row_len..(y + 1) * row_len];
        for x in 0..w {
            let alpha = if opaque { 255 } else { cur[x * ch + ch - 1] };
            for c in 0..ch {
                tol[x * ch + c] = if !opaque && c == ch - 1 {
                    0
                } else if alpha == 0 {
                    255
                } else {
                    delta.min(254)
                };
            }
        }
        let mut best_score = u64::MAX;
        let mut best_ft = 0;
        for ft in 0..5u8 {
            quantize_row(ft, cur, &prev, bpp, &tol, &mut res, &mut rec);
            let s = minsum(&res);
            if s < best_score {
                best_score = s;
                best_ft = ft;
                std::mem::swap(&mut best_res, &mut res);
                best_rec.copy_from_slice(&rec);
            }
        }
        filtered.push(best_ft);
        filtered.extend_from_slice(&best_res);
        recon_rows[y * row_len..(y + 1) * row_len].copy_from_slice(&best_rec);
        prev.copy_from_slice(&best_rec);
    }

    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let s = &recon_rows[y * row_len + x * ch..y * row_len + (x + 1) * ch];
            let v = |i: usize| s[i] as u16;
            px.extend_from_slice(&match ch {
                1 => [v(0), v(0), v(0), 255],
                2 => [v(0), v(0), v(0), v(1)],
                3 => [v(0), v(1), v(2), 255],
                _ => [v(0), v(1), v(2), v(3)],
            });
        }
    }
    NearLossless {
        target,
        filtered,
        recon: Image {
            width: img.width,
            height: img.height,
            bit16: false,
            px,
        },
        label: format!("max error ±{delta}"),
    }
}

/// Compresses and assembles a near-lossless image into a PNG.
pub(super) fn finish_near_lossless(
    nl: &NearLossless,
    parsed: &Parsed,
    opts: &Options,
) -> (Vec<u8>, String) {
    let (_, level, level_name) = levels(opts.effort);
    let idat = zlib_compress(&nl.filtered, &level);
    let orig = &parsed.ihdr;
    let type_changed = (nl.target.color_type(), nl.target.depth()) != (orig.color, orig.depth);
    let kept: Vec<&Chunk> = parsed
        .ancillary
        .iter()
        .filter(|c| keep_chunk(&c.kind, opts.metadata, type_changed))
        .collect();
    let png = assemble(&nl.recon, &nl.target, &idat, &kept);
    let desc = format!(
        "png: {} -> {}, near-lossless ({}), deflate={level_name}",
        describe_ihdr(orig),
        nl.target.describe(),
        nl.label
    );
    (png, desc)
}

#[cfg(test)]
mod near_lossless_tests {
    use super::*;

    fn test_image(w: u32, h: u32, alpha: bool) -> Image {
        let mut rng = 12345u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = (next() % 9) as i32 - 4;
                let base = [(x * 5 + y) as i32, (y * 7) as i32, ((x + y) * 3) as i32];
                let a = if !alpha {
                    255
                } else if (x + y) % 7 == 0 {
                    0
                } else {
                    (next() % 256) as u16
                };
                px.extend(base.iter().map(|&v| (v + n).clamp(0, 255) as u16));
                px.push(a);
            }
        }
        Image {
            width: w,
            height: h,
            bit16: false,
            px,
        }
    }

    #[test]
    fn error_is_bounded_and_alpha_exact() {
        for alpha in [false, true] {
            let img = test_image(41, 23, alpha);
            for delta in [1u8, 3, 8, 20] {
                let nl = near_lossless(&img, delta);
                for (o, r) in img.px.chunks_exact(4).zip(nl.recon.px.chunks_exact(4)) {
                    assert_eq!(o[3], r[3], "alpha must be exact");
                    if o[3] == 0 {
                        continue; // color under transparent pixels is free
                    }
                    for c in 0..3 {
                        let e = (o[c] as i32 - r[c] as i32).abs();
                        assert!(e <= delta as i32, "error {e} > {delta}");
                    }
                }
            }
        }
    }

    #[test]
    fn filtered_stream_reconstructs_exactly() {
        // Decode the filtered stream with the PNG unfilter routine and compare
        // with the encoder's own reconstruction.
        let img = test_image(33, 17, false);
        let nl = near_lossless(&img, 4);
        let row_len = 33 * 3;
        let mut prev = vec![0u8; row_len];
        for y in 0..17 {
            let start = y * (row_len + 1);
            let mut row = nl.filtered[start + 1..start + 1 + row_len].to_vec();
            assert!(filter::unfilter_row(nl.filtered[start], &mut row, &prev, 3));
            for x in 0..33 {
                let r = &nl.recon.px[(y * 33 + x) * 4..][..3];
                assert_eq!(
                    &row[x * 3..x * 3 + 3],
                    &[r[0] as u8, r[1] as u8, r[2] as u8]
                );
            }
            prev = row;
        }
    }
}
