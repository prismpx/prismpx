//! Palette quantization with edge-aware error diffusion.
//!
//! 1. Colors are mapped to premultiplied OKLab + alpha, a space where
//!    Euclidean distance tracks perceived difference and fully transparent
//!    colors collapse to one point.
//! 2. An initial palette is built by variance-minimizing median cut: the box
//!    with the largest weighted squared error is split at the point along its
//!    widest axis that minimizes the summed error of the halves (in the spirit
//!    of Wu, "Efficient Statistical Computations for Optimal Color
//!    Quantization", Graphics Gems II, 1991).
//! 3. The palette is refined with weighted k-means (Lloyd iterations).
//! 4. Pixels are remapped with serpentine Floyd–Steinberg error diffusion
//!    whose strength fades out on edges and texture, where quantization error
//!    is masked anyway. This keeps smooth gradients free of banding while
//!    avoiding dither noise that would hurt DEFLATE.

// Index loops over small fixed-size channel arrays read more clearly here.
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;

use super::Image;
use crate::color::{linear_to_oklab, linear_to_srgb, oklab_to_linear, srgb8_to_linear_table};

/// Relative importance of alpha errors versus OKLab lightness errors.
const ALPHA_WEIGHT: f32 = 1.0;
/// Above this many distinct colors, colors are bucketed before clustering.
const MAX_HISTOGRAM: usize = 1 << 17;

type Feature = [f32; 4];

struct Converter {
    lin: [f32; 256],
}

impl Converter {
    fn new() -> Converter {
        Converter {
            lin: srgb8_to_linear_table(),
        }
    }

    fn feature(&self, c: [u8; 4]) -> Feature {
        let a = c[3] as f32 / 255.0;
        let lab = linear_to_oklab(
            self.lin[c[0] as usize],
            self.lin[c[1] as usize],
            self.lin[c[2] as usize],
        );
        [lab[0] * a, lab[1] * a, lab[2] * a, a * ALPHA_WEIGHT]
    }

    fn color(&self, f: Feature) -> [u8; 4] {
        let a = (f[3] / ALPHA_WEIGHT).clamp(0.0, 1.0);
        let a8 = (a * 255.0).round() as u8;
        if a8 == 0 {
            return [0, 0, 0, 0];
        }
        let lin = oklab_to_linear([f[0] / a, f[1] / a, f[2] / a]);
        let enc = |v: f32| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
        [enc(lin[0]), enc(lin[1]), enc(lin[2]), a8]
    }
}

#[inline]
fn dist2(a: &Feature, b: &Feature) -> f32 {
    let d0 = a[0] - b[0];
    let d1 = a[1] - b[1];
    let d2 = a[2] - b[2];
    let d3 = a[3] - b[3];
    d0 * d0 + d1 * d1 + d2 * d2 + d3 * d3
}

#[derive(Clone, Copy)]
struct Entry {
    f: Feature,
    w: f32,
}

/// Weighted color histogram in feature space.
fn histogram(img: &Image, conv: &Converter) -> Vec<Entry> {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for p in img.px.chunks_exact(4) {
        let key = (p[0] as u32) << 24 | (p[1] as u32) << 16 | (p[2] as u32) << 8 | p[3] as u32;
        *counts.entry(key).or_insert(0) += 1;
    }
    let unpack = |k: u32| [(k >> 24) as u8, (k >> 16) as u8, (k >> 8) as u8, k as u8];
    if counts.len() <= MAX_HISTOGRAM {
        return counts
            .into_iter()
            .map(|(k, n)| Entry {
                f: conv.feature(unpack(k)),
                w: n as f32,
            })
            .collect();
    }
    // Too many colors: average within 6-bit-per-channel buckets.
    let mut buckets: HashMap<u32, (Feature, f32)> = HashMap::new();
    for (k, n) in counts {
        let f = conv.feature(unpack(k));
        let e = buckets.entry(k & 0xFCFC_FCFC).or_insert(([0.0; 4], 0.0));
        for d in 0..4 {
            e.0[d] += f[d] * n as f32;
        }
        e.1 += n as f32;
    }
    buckets
        .into_values()
        .map(|(s, w)| Entry {
            f: [s[0] / w, s[1] / w, s[2] / w, s[3] / w],
            w,
        })
        .collect()
}

struct Cell {
    start: usize,
    end: usize,
    sse: f64,
}

fn moments(e: &[Entry]) -> (f64, [f64; 4], f64) {
    let (mut w, mut s, mut q) = (0.0f64, [0.0f64; 4], 0.0f64);
    for x in e {
        let ww = x.w as f64;
        w += ww;
        for d in 0..4 {
            s[d] += ww * x.f[d] as f64;
            q += ww * (x.f[d] as f64).powi(2);
        }
    }
    (w, s, q)
}

fn sse_of(w: f64, s: &[f64; 4], q: f64) -> f64 {
    if w <= 0.0 {
        return 0.0;
    }
    (q - s.iter().map(|v| v * v).sum::<f64>() / w).max(0.0)
}

/// Splits the cell at the position that minimizes total error; returns the split index.
fn best_split(e: &mut [Entry]) -> Option<usize> {
    if e.len() < 2 {
        return None;
    }
    let (w, s, q) = moments(e);
    let mean: Vec<f64> = s.iter().map(|v| v / w).collect();
    let mut var = [0.0f64; 4];
    for x in e.iter() {
        for d in 0..4 {
            var[d] += x.w as f64 * (x.f[d] as f64 - mean[d]).powi(2);
        }
    }
    let axis = (0..4).max_by(|&a, &b| var[a].total_cmp(&var[b])).unwrap();
    e.sort_unstable_by(|a, b| a.f[axis].total_cmp(&b.f[axis]));

    let (mut lw, mut ls, mut lq) = (0.0f64, [0.0f64; 4], 0.0f64);
    let mut best: Option<(usize, f64)> = None;
    for i in 0..e.len() - 1 {
        let x = &e[i];
        let ww = x.w as f64;
        lw += ww;
        for d in 0..4 {
            ls[d] += ww * x.f[d] as f64;
            lq += ww * (x.f[d] as f64).powi(2);
        }
        if e[i].f[axis] == e[i + 1].f[axis] {
            continue;
        }
        let rs = [s[0] - ls[0], s[1] - ls[1], s[2] - ls[2], s[3] - ls[3]];
        let total = sse_of(lw, &ls, lq) + sse_of(w - lw, &rs, q - lq);
        if best.is_none_or(|b| total < b.1) {
            best = Some((i + 1, total));
        }
    }
    best.map(|b| b.0)
}

fn median_cut(entries: &mut [Entry], colors: usize) -> Vec<Feature> {
    let (w, s, q) = moments(entries);
    let mut cells = vec![Cell {
        start: 0,
        end: entries.len(),
        sse: sse_of(w, &s, q),
    }];
    while cells.len() < colors {
        let Some(idx) = (0..cells.len())
            .filter(|&i| cells[i].end - cells[i].start > 1 && cells[i].sse > 0.0)
            .max_by(|&a, &b| cells[a].sse.total_cmp(&cells[b].sse))
        else {
            break;
        };
        let (start, end) = (cells[idx].start, cells[idx].end);
        let Some(split) = best_split(&mut entries[start..end]) else {
            cells[idx].sse = 0.0;
            continue;
        };
        let mid = start + split;
        let (lw, ls, lq) = moments(&entries[start..mid]);
        let (rw, rs, rq) = moments(&entries[mid..end]);
        cells[idx] = Cell {
            start,
            end: mid,
            sse: sse_of(lw, &ls, lq),
        };
        cells.push(Cell {
            start: mid,
            end,
            sse: sse_of(rw, &rs, rq),
        });
    }
    cells
        .iter()
        .map(|c| {
            let (w, s, _) = moments(&entries[c.start..c.end]);
            [
                (s[0] / w) as f32,
                (s[1] / w) as f32,
                (s[2] / w) as f32,
                (s[3] / w) as f32,
            ]
        })
        .collect()
}

fn nearest(palette: &[Feature], f: &Feature) -> usize {
    let mut best = 0;
    let mut bd = f32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let d = dist2(p, f);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

fn kmeans(entries: &[Entry], palette: &mut [Feature], iterations: usize) {
    let mut assign = vec![0usize; entries.len()];
    for _ in 0..iterations {
        let mut sums = vec![[0.0f64; 4]; palette.len()];
        let mut weights = vec![0.0f64; palette.len()];
        let mut changed = 0usize;
        for (i, e) in entries.iter().enumerate() {
            let k = nearest(palette, &e.f);
            if k != assign[i] {
                changed += 1;
                assign[i] = k;
            }
            weights[k] += e.w as f64;
            for d in 0..4 {
                sums[k][d] += e.w as f64 * e.f[d] as f64;
            }
        }
        for (k, p) in palette.iter_mut().enumerate() {
            if weights[k] > 0.0 {
                for d in 0..4 {
                    p[d] = (sums[k][d] / weights[k]) as f32;
                }
            }
        }
        if changed * 1000 < entries.len() {
            break;
        }
    }
}

/// How strongly to dither each pixel: 1 in smooth areas, fading to 0 on edges.
fn dither_map(img: &Image, features: &[Feature]) -> Vec<f32> {
    let (w, h) = (img.width as usize, img.height as usize);
    let lum: Vec<f32> = features.iter().map(|f| f[0] + 0.5 * f[3]).collect();
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = lum[y * w + x];
            let mut m = 0.0f32;
            if x > 0 {
                m = m.max((c - lum[y * w + x - 1]).abs());
            }
            if x + 1 < w {
                m = m.max((c - lum[y * w + x + 1]).abs());
            }
            if y > 0 {
                m = m.max((c - lum[(y - 1) * w + x]).abs());
            }
            if y + 1 < h {
                m = m.max((c - lum[(y + 1) * w + x]).abs());
            }
            // Gradients step by <0.02 per pixel; edges and texture are larger.
            out[y * w + x] = (1.0 - (m - 0.02) / 0.06).clamp(0.0, 1.0);
        }
    }
    out
}

/// Per-image data reused across palette sizes and dither settings.
pub(crate) struct Prepared<'a> {
    img: &'a Image,
    conv: Converter,
    entries: Vec<Entry>,
    /// Feature of every pixel, in raster order.
    features: Vec<Feature>,
    /// Dither strength per pixel (1 in smooth areas, 0 on edges).
    strength: Vec<f32>,
}

/// A palette as RGBA colors plus their exact features.
pub(crate) struct Palette {
    rgba: Vec<[u8; 4]>,
    feats: Vec<Feature>,
}

impl<'a> Prepared<'a> {
    pub fn new(img: &'a Image) -> Prepared<'a> {
        debug_assert!(!img.bit16);
        let conv = Converter::new();
        let entries = histogram(img, &conv);
        let mut cache: HashMap<[u8; 4], Feature> = HashMap::new();
        let features: Vec<Feature> = img
            .px
            .chunks_exact(4)
            .map(|p| {
                let c = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
                *cache.entry(c).or_insert_with(|| conv.feature(c))
            })
            .collect();
        let strength = dither_map(img, &features);
        Prepared {
            img,
            conv,
            entries,
            features,
            strength,
        }
    }

    pub fn palette(&self, colors: usize) -> Palette {
        let mut entries = self.entries.clone();
        let mut palette = median_cut(&mut entries, colors.clamp(2, 256));
        let iterations = if entries.len() > 50_000 { 3 } else { 8 };
        kmeans(&entries, &mut palette, iterations);
        // Snap to representable colors and recompute features so errors are exact.
        let mut rgba: Vec<[u8; 4]> = palette.iter().map(|&f| self.conv.color(f)).collect();
        rgba.sort_unstable();
        rgba.dedup();
        let feats = rgba.iter().map(|&c| self.conv.feature(c)).collect();
        Palette { rgba, feats }
    }

    /// Maps every pixel to the palette. `dither` in 0..=1 scales error diffusion.
    pub fn remap(&self, pal: &Palette, dither: f32) -> Image {
        let img = self.img;
        let (w, h) = (img.width as usize, img.height as usize);
        let mut out = Image {
            width: img.width,
            height: img.height,
            bit16: false,
            px: vec![0; w * h * 4],
        };
        let mut cache: HashMap<[u8; 4], usize> = HashMap::new();
        let mut err_cur = vec![[0.0f32; 4]; w + 2];
        let mut err_next = vec![[0.0f32; 4]; w + 2];
        const MAX_ERR: f32 = 0.08;

        for y in 0..h {
            let ltr = y % 2 == 0;
            for i in 0..w {
                let x = if ltr { i } else { w - 1 - i };
                let idx = y * w + x;
                let p = &img.px[idx * 4..idx * 4 + 4];
                let c = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
                let f0 = self.features[idx];
                let e = err_cur[x + 1];
                let k = if e.iter().any(|v| *v != 0.0) {
                    let f = [f0[0] + e[0], f0[1] + e[1], f0[2] + e[2], f0[3] + e[3]];
                    nearest(&pal.feats, &f)
                } else {
                    *cache.entry(c).or_insert_with(|| nearest(&pal.feats, &f0))
                };
                let q = pal.rgba[k];
                out.px[idx * 4..idx * 4 + 4].copy_from_slice(&[
                    q[0] as u16,
                    q[1] as u16,
                    q[2] as u16,
                    q[3] as u16,
                ]);

                let s = dither * self.strength[idx];
                if s > 0.0 {
                    let mut d = [0.0f32; 4];
                    for j in 0..4 {
                        d[j] = ((f0[j] + e[j] - pal.feats[k][j]) * s).clamp(-MAX_ERR, MAX_ERR);
                    }
                    // Floyd–Steinberg weights, mirrored on right-to-left rows.
                    let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
                    for j in 0..4 {
                        err_cur[fwd][j] += d[j] * 7.0 / 16.0;
                        err_next[back][j] += d[j] * 3.0 / 16.0;
                        err_next[x + 1][j] += d[j] * 5.0 / 16.0;
                        err_next[fwd][j] += d[j] / 16.0;
                    }
                }
            }
            std::mem::swap(&mut err_cur, &mut err_next);
            err_next.iter_mut().for_each(|v| *v = [0.0; 4]);
        }
        out
    }
}

/// Reduces an 8-bit RGBA image to at most `colors` colors.
#[cfg(test)]
pub(crate) fn quantize(img: &Image, colors: usize, dither: f32) -> Image {
    let prep = Prepared::new(img);
    let pal = prep.palette(colors);
    prep.remap(&pal, dither)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Image {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[(x * 255 / w) as u16, (y * 255 / h) as u16, 128, 255]);
            }
        }
        Image {
            width: w,
            height: h,
            bit16: false,
            px,
        }
    }

    fn distinct(img: &Image) -> usize {
        let mut v: Vec<&[u16]> = img.px.chunks_exact(4).collect();
        v.sort();
        v.dedup();
        v.len()
    }

    #[test]
    fn respects_color_budget() {
        let img = gradient(120, 80);
        for n in [2, 7, 16, 64, 256] {
            for dither in [0.0, 1.0] {
                let q = quantize(&img, n, dither);
                assert!(distinct(&q) <= n, "{n}: {}", distinct(&q));
            }
        }
    }

    #[test]
    fn exact_when_few_colors() {
        let mut img = gradient(16, 16);
        for (i, p) in img.px.chunks_exact_mut(4).enumerate() {
            let v = [[255, 0, 0, 255], [0, 0, 255, 128], [0, 0, 0, 0]][i % 3];
            p.copy_from_slice(&v);
        }
        let q = quantize(&img, 8, 1.0);
        assert_eq!(q, img);
    }
}
