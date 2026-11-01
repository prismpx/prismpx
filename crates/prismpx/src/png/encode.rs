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
