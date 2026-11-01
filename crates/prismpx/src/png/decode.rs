//! PNG parsing and decoding into a canonical RGBA image.

use super::filter::unfilter_row;
use super::{Image, SIGNATURE};
use crate::checksum::{crc32, crc32_update};
use crate::deflate::zlib_decompress;
use crate::error::{malformed, Error, Result};

/// Refuse images above this many pixels to bound memory use.
const MAX_PIXELS: u64 = 1 << 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ihdr {
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub color: u8,
    pub interlace: u8,
}

impl Ihdr {
    pub fn channels(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
    /// Position in the original file, used to keep valid chunk ordering.
    pub before_plte: bool,
    pub before_idat: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Parsed {
    pub ihdr: Ihdr,
    pub plte: Option<Vec<u8>>,
    pub trns: Option<Vec<u8>>,
    pub idat: Vec<u8>,
    pub ancillary: Vec<Chunk>,
}

pub(crate) fn parse(data: &[u8]) -> Result<Parsed> {
    if !data.starts_with(&SIGNATURE) {
        return Err(Error::UnsupportedFormat);
    }
    let mut pos = 8;
    let mut ihdr = None;
    let mut plte = None;
    let mut trns = None;
    let mut idat = Vec::new();
    let mut ancillary = Vec::new();
    let (mut seen_plte, mut seen_idat, mut seen_iend) = (false, false, false);

    while pos + 12 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        if len > i32::MAX as usize || pos + 12 + len > data.len() {
            return Err(malformed("chunk extends past end of file"));
        }
        let body = &data[pos + 8..pos + 8 + len];
        let stored_crc =
            u32::from_be_bytes(data[pos + 8 + len..pos + 12 + len].try_into().unwrap());
        if crc32_update(crc32(&kind), body) != stored_crc {
            return Err(malformed(format!(
                "CRC mismatch in {} chunk",
                String::from_utf8_lossy(&kind)
            )));
        }
        pos += 12 + len;

        if ihdr.is_none() && &kind != b"IHDR" {
            return Err(malformed("first chunk is not IHDR"));
        }
        match &kind {
            b"IHDR" => ihdr = Some(parse_ihdr(body)?),
            b"PLTE" => {
                if body.is_empty() || !body.len().is_multiple_of(3) || body.len() > 768 {
                    return Err(malformed("invalid PLTE length"));
                }
                plte = Some(body.to_vec());
                seen_plte = true;
            }
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => {
                idat.extend_from_slice(body);
                seen_idat = true;
            }
            b"IEND" => {
                seen_iend = true;
                break;
            }
            b"acTL" | b"fcTL" | b"fdAT" => {
                return Err(Error::Unsupported("animated PNG (APNG)".into()));
            }
            _ if kind[0].is_ascii_uppercase() => {
                return Err(Error::Unsupported(format!(
                    "unknown critical chunk {}",
                    String::from_utf8_lossy(&kind)
                )));
            }
            _ => ancillary.push(Chunk {
                kind,
                data: body.to_vec(),
                before_plte: !seen_plte && !seen_idat,
                before_idat: !seen_idat,
            }),
        }
    }
    let ihdr = ihdr.ok_or_else(|| malformed("missing IHDR"))?;
    if !seen_iend {
        return Err(malformed("missing IEND"));
    }
    if idat.is_empty() {
        return Err(malformed("missing IDAT"));
    }
    if ihdr.color == 3 && plte.is_none() {
        return Err(malformed("palette image without PLTE"));
    }
    Ok(Parsed {
        ihdr,
        plte,
        trns,
        idat,
        ancillary,
    })
}

fn parse_ihdr(b: &[u8]) -> Result<Ihdr> {
    if b.len() != 13 {
        return Err(malformed("invalid IHDR length"));
    }
    let h = Ihdr {
        width: u32::from_be_bytes(b[0..4].try_into().unwrap()),
        height: u32::from_be_bytes(b[4..8].try_into().unwrap()),
        depth: b[8],
        color: b[9],
        interlace: b[12],
    };
    let depth_ok = match h.color {
        0 => matches!(h.depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(h.depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(h.depth, 8 | 16),
        _ => false,
    };
    if !depth_ok {
        return Err(malformed("invalid color type / bit depth combination"));
    }
    if b[10] != 0 || b[11] != 0 || h.interlace > 1 {
        return Err(malformed("unknown compression, filter or interlace method"));
    }
    if h.width == 0 || h.height == 0 {
        return Err(malformed("zero image dimension"));
    }
    if h.width as u64 * h.height as u64 > MAX_PIXELS {
        return Err(Error::Unsupported("image too large".into()));
    }
    Ok(h)
}

/// (x start, y start, x step, y step) for the 7 Adam7 passes.
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn row_bytes(width: usize, bits_per_pixel: usize) -> usize {
    (width * bits_per_pixel).div_ceil(8)
}

/// Sub-images (pass width, pass height, x0, y0, dx, dy) making up the image.
fn passes(h: &Ihdr) -> Vec<(usize, usize, usize, usize, usize, usize)> {
    let (w, ht) = (h.width as usize, h.height as usize);
    if h.interlace == 0 {
        return vec![(w, ht, 0, 0, 1, 1)];
    }
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
            let ph = if ht > y0 { (ht - y0).div_ceil(dy) } else { 0 };
            (pw, ph, x0, y0, dx, dy)
        })
        .filter(|p| p.0 > 0 && p.1 > 0)
        .collect()
}

#[inline]
fn sample(row: &[u8], index: usize, depth: u8) -> u16 {
    match depth {
        16 => u16::from_be_bytes([row[2 * index], row[2 * index + 1]]),
        8 => row[index] as u16,
        d => {
            let bit = index * d as usize;
            let shift = 8 - d as usize - (bit % 8);
            ((row[bit / 8] >> shift) & ((1u8 << d) - 1)) as u16
        }
    }
}

pub(crate) fn decode_image(p: &Parsed) -> Result<Image> {
    let h = &p.ihdr;
    let channels = h.channels();
    let bits_pp = channels * h.depth as usize;
    let bpp = bits_pp.div_ceil(8);
    let passes = passes(h);
    let expected: usize = passes
        .iter()
        .map(|&(pw, ph, ..)| ph * (1 + row_bytes(pw, bits_pp)))
        .sum();
    let raw = zlib_decompress(&p.idat, expected)?;
    if raw.len() < expected {
        return Err(malformed("not enough image data"));
    }

    let bit16 = h.depth == 16;
    let max: u16 = if bit16 { 65535 } else { 255 };
    let gray_scale: u16 = match h.depth {
        1 => 255,
        2 => 85,
        4 => 17,
        _ => 1,
    };
    let palette: Vec<[u8; 4]> = match &p.plte {
        Some(pl) if h.color == 3 => pl
            .chunks(3)
            .enumerate()
            .map(|(i, c)| {
                let a = p
                    .trns
                    .as_ref()
                    .and_then(|t| t.get(i))
                    .copied()
                    .unwrap_or(255);
                [c[0], c[1], c[2], a]
            })
            .collect(),
        _ => Vec::new(),
    };
    let key: Option<[u16; 3]> = match (&p.trns, h.color) {
        (Some(t), 0) if t.len() >= 2 => {
            let g = u16::from_be_bytes([t[0], t[1]]);
            Some([g, g, g])
        }
        (Some(t), 2) if t.len() >= 6 => Some([
            u16::from_be_bytes([t[0], t[1]]),
            u16::from_be_bytes([t[2], t[3]]),
            u16::from_be_bytes([t[4], t[5]]),
        ]),
        _ => None,
    };

    let (w, ht) = (h.width as usize, h.height as usize);
    let mut px = vec![0u16; w * ht * 4];
    let mut offset = 0;
    for &(pw, ph, x0, y0, dx, dy) in &passes {
        let rb = row_bytes(pw, bits_pp);
        let mut prev = vec![0u8; rb];
        let mut cur = vec![0u8; rb];
        for py in 0..ph {
            let ft = raw[offset];
            cur.copy_from_slice(&raw[offset + 1..offset + 1 + rb]);
            offset += 1 + rb;
            if !unfilter_row(ft, &mut cur, &prev, bpp) {
                return Err(malformed("invalid filter type"));
            }
            let y = y0 + py * dy;
            for pxi in 0..pw {
                let x = x0 + pxi * dx;
                let o = (y * w + x) * 4;
                let s = |c: usize| sample(&cur, pxi * channels + c, h.depth);
                let rgba = match h.color {
                    0 => {
                        let v = s(0);
                        let g = v * gray_scale;
                        let a = if key.is_some_and(|k| k[0] == v) {
                            0
                        } else {
                            max
                        };
                        [g, g, g, a]
                    }
                    2 => {
                        let (r, g, b) = (s(0), s(1), s(2));
                        let a = if key == Some([r, g, b]) { 0 } else { max };
                        [r, g, b, a]
                    }
                    3 => {
                        let e = palette
                            .get(s(0) as usize)
                            .ok_or_else(|| malformed("palette index out of range"))?;
                        [e[0] as u16, e[1] as u16, e[2] as u16, e[3] as u16]
                    }
                    4 => {
                        let g = s(0);
                        [g, g, g, s(1)]
                    }
                    _ => [s(0), s(1), s(2), s(3)],
                };
                px[o..o + 4].copy_from_slice(&rgba);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
    }
    Ok(Image {
        width: h.width,
        height: h.height,
        bit16,
        px,
    })
}

/// Decodes a PNG into canonical RGBA.
pub fn decode(data: &[u8]) -> Result<Image> {
    decode_image(&parse(data)?)
}
