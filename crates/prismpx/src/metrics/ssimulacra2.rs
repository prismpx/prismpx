//! SSIMULACRA 2 perceptual metric.
//!
//! A port of the reference implementation in libjxl (`tools/ssimulacra2.cc`
//! and `tools/gauss_blur.cc`, commit a16d32d0), by Jon Sneyers / the JPEG XL
//! Project Authors, used under the BSD 3-Clause license (see
//! THIRD_PARTY_NOTICES.md). The algorithm, constants and the 108 tuned weights
//! are theirs; any bugs in this port are ours.
//!
//! Score scale (from the reference tool's documentation):
//! 90+ visually lossless, 85 excellent (indistinguishable in a flip test at 1:1),
//! 80 very high (indistinguishable side by side), 70 high, 50 medium, 30 low.

// Index loops over small fixed-size channel arrays read more clearly here.
#![allow(clippy::needless_range_loop)]

use crate::color::{srgb8_to_linear_table, srgb_to_linear};
use crate::error::{Error, Result};
use crate::png::Image;

const C2: f32 = 0.0009;
const NUM_SCALES: usize = 6;

/// Computes the SSIMULACRA 2 score of `distorted` against `original`.
///
/// Both images must have the same size, at least 8x8 pixels. Images with
/// transparency are composited on a dark and a bright background and the
/// worse score is returned, as the reference tool does.
pub fn ssimulacra2(original: &Image, distorted: &Image) -> Result<f64> {
    Reference::new(original)?.score(distorted)
}

fn translucent(img: &Image) -> bool {
    let max = img.max_value();
    img.px.chunks_exact(4).any(|p| p[3] != max)
}

/// Precomputed data for scoring many distorted versions of one original.
pub struct Reference {
    original: Image,
    width: u32,
    height: u32,
    translucent: bool,
    /// Per background (none, or dark and bright): per-scale original data.
    backgrounds: Vec<(Option<f32>, Vec<OrigScale>)>,
}

struct OrigScale {
    xyb: Planes,
    mu: Planes,
    sigma_sq: Planes,
}

impl Reference {
    pub fn new(original: &Image) -> Result<Reference> {
        if original.width < 8 || original.height < 8 {
            return Err(Error::Unsupported(
                "SSIMULACRA2 needs at least 8x8 pixels".into(),
            ));
        }
        let translucent = translucent(original);
        let backgrounds = Self::prepare(original, translucent);
        Ok(Reference {
            original: original.clone(),
            width: original.width,
            height: original.height,
            translucent,
            backgrounds,
        })
    }

    fn prepare(original: &Image, translucent: bool) -> Vec<(Option<f32>, Vec<OrigScale>)> {
        let bgs: Vec<Option<f32>> = if translucent {
            vec![Some(0.1), Some(0.9)]
        } else {
            vec![None]
        };
        par_map(&bgs, |&bg| {
            let gauss = RecursiveGaussian::new(1.5);
            let mut lin = to_linear(original, bg);
            let mut scales = Vec::new();
            for scale in 0..NUM_SCALES {
                if scale > 0 {
                    lin = downsample(&lin);
                }
                if lin.w < 8 || lin.h < 8 {
                    break;
                }
                let xyb = to_positive_xyb(&lin);
                let sigma_sq = gauss.blur_planes(&product(&xyb, &xyb));
                let mu = gauss.blur_planes(&xyb);
                scales.push(OrigScale { xyb, mu, sigma_sq });
            }
            (bg, scales)
        })
    }

    pub fn score(&self, distorted: &Image) -> Result<f64> {
        if self.width != distorted.width || self.height != distorted.height {
            return Err(Error::Unsupported("image sizes differ".into()));
        }
        if !self.translucent && translucent(distorted) {
            // Opaque original, translucent result: both need compositing.
            let backgrounds = Self::prepare(&self.original, true);
            let scores = par_map(&backgrounds, |(bg, orig)| {
                score_against(orig, distorted, *bg)
            });
            return Ok(scores.into_iter().fold(f64::INFINITY, f64::min));
        }
        let scores = par_map(&self.backgrounds, |(bg, orig)| {
            score_against(orig, distorted, *bg)
        });
        Ok(scores.into_iter().fold(f64::INFINITY, f64::min))
    }
}

/// Maps `f` over `items`, one thread per item where threads are available.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    #[cfg(not(target_family = "wasm"))]
    {
        if items.len() > 1 {
            return std::thread::scope(|s| {
                let handles: Vec<_> = items.iter().map(|it| s.spawn(|| f(it))).collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("worker panicked"))
                    .collect()
            });
        }
    }
    items.iter().map(f).collect()
}

/// Planar float image.
#[derive(Clone)]
struct Planes {
    w: usize,
    h: usize,
    p: [Vec<f32>; 3],
}

impl Planes {
    fn new(w: usize, h: usize) -> Planes {
        Planes {
            w,
            h,
            p: [vec![0.0; w * h], vec![0.0; w * h], vec![0.0; w * h]],
        }
    }
}

/// Linear RGB planes, alpha-blended (in sRGB space) against `bg` if given.
fn to_linear(img: &Image, bg: Option<f32>) -> Planes {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Planes::new(w, h);
    let scale = 1.0 / img.max_value() as f32;
    let lut = (!img.bit16).then(srgb8_to_linear_table);
    for (i, px) in img.px.chunks_exact(4).enumerate() {
        if let Some(lut) = &lut {
            if bg.is_none() || px[3] == 255 {
                for c in 0..3 {
                    out.p[c][i] = lut[px[c] as usize];
                }
                continue;
            }
        }
        let a = px[3] as f32 * scale;
        for c in 0..3 {
            let mut v = px[c] as f32 * scale;
            if let Some(bg) = bg {
                v = a * v + (1.0 - a) * bg;
            }
            out.p[c][i] = srgb_to_linear(v);
        }
    }
    out
}

// Opsin absorbance (lib/jxl/cms/opsin_params.h).
const M: [[f32; 3]; 3] = [
    [0.30, 0.622, 0.078],
    [0.23, 0.692, 0.078],
    [
        0.243_422_69,
        0.204_767_44,
        1.0 - 0.243_422_69 - 0.204_767_44,
    ],
];
const BIAS: f32 = 0.003_793_073_3;

/// Linear RGB to XYB, rescaled to roughly 0..1 ("MakePositiveXYB").
fn to_positive_xyb(lin: &Planes) -> Planes {
    let mut out = Planes::new(lin.w, lin.h);
    let neg_bias_cbrt = -BIAS.cbrt();
    for i in 0..lin.w * lin.h {
        let (r, g, b) = (lin.p[0][i], lin.p[1][i], lin.p[2][i]);
        let mut lms = [0.0f32; 3];
        for (k, v) in lms.iter_mut().enumerate() {
            let mixed = (M[k][0] * r + M[k][1] * g + M[k][2] * b + BIAS).max(0.0);
            *v = mixed.cbrt() + neg_bias_cbrt;
        }
        let x = 0.5 * (lms[0] - lms[1]);
        let y = 0.5 * (lms[0] + lms[1]);
        let bb = lms[2];
        out.p[0][i] = x * 14.0 + 0.42;
        out.p[1][i] = y + 0.01;
        out.p[2][i] = (bb - y) + 0.55;
    }
    out
}

fn downsample(img: &Planes) -> Planes {
    let (ow, oh) = (img.w.div_ceil(2), img.h.div_ceil(2));
    let mut out = Planes::new(ow, oh);
    for c in 0..3 {
        let src = &img.p[c];
        for oy in 0..oh {
            for ox in 0..ow {
                let mut sum = 0.0;
                for iy in 0..2 {
                    for ix in 0..2 {
                        let x = (ox * 2 + ix).min(img.w - 1);
                        let y = (oy * 2 + iy).min(img.h - 1);
                        sum += src[y * img.w + x];
                    }
                }
                out.p[c][oy * ow + ox] = sum * 0.25;
            }
        }
    }
    out
}

/// Recursive Gaussian (Charalampidis 2016, "Recursive implementation of the
/// Gaussian filter using truncated cosine functions"), as in libjxl.
struct RecursiveGaussian {
    radius: isize,
    n2: [f32; 3],
    d1: [f32; 3],
}

impl RecursiveGaussian {
    fn new(sigma: f64) -> RecursiveGaussian {
        use std::f64::consts::PI;
        let radius = (3.2795 * sigma + 0.2546).round();
        let pi_div_2r = PI / (2.0 * radius);
        let omega = [pi_div_2r, 3.0 * pi_div_2r, 5.0 * pi_div_2r];
        let p1 = 1.0 / (0.5 * omega[0]).tan();
        let p3 = -1.0 / (0.5 * omega[1]).tan();
        let p5 = 1.0 / (0.5 * omega[2]).tan();
        let r1 = p1 * p1 / omega[0].sin();
        let r3 = -p3 * p3 / omega[1].sin();
        let r5 = p5 * p5 / omega[2].sin();
        let neg_half_sigma2 = -0.5 * sigma * sigma;
        let rho: Vec<f64> = omega
            .iter()
            .map(|o| (neg_half_sigma2 * o * o).exp() / radius)
            .collect();
        let d13 = p1 * r3 - r1 * p3;
        let d35 = p3 * r5 - r3 * p5;
        let d51 = p5 * r1 - r5 * p1;
        let zeta15 = d35 / d13;
        let zeta35 = d51 / d13;
        let a = [[p1, p3, p5], [r1, r3, r5], [zeta15, zeta35, 1.0]];
        let inv = invert3(a);
        let gamma = [
            1.0,
            radius * radius - sigma * sigma,
            zeta15 * rho[0] + zeta35 * rho[1] + rho[2],
        ];
        let beta: Vec<f64> = (0..3)
            .map(|i| inv[i][0] * gamma[0] + inv[i][1] * gamma[1] + inv[i][2] * gamma[2])
            .collect();
        let mut n2 = [0.0f32; 3];
        let mut d1 = [0.0f32; 3];
        for i in 0..3 {
            n2[i] = (-beta[i] * (omega[i] * (radius + 1.0)).cos()) as f32;
            d1[i] = (-2.0 * omega[i].cos()) as f32;
        }
        RecursiveGaussian {
            radius: radius as isize,
            n2,
            d1,
        }
    }

    /// Blurs one plane; zero padding outside the image, like the reference.
    fn blur(&self, src: &[f32], w: usize, h: usize, out: &mut [f32], tmp: &mut Vec<f32>) {
        let n_r = self.radius;
        tmp.resize(w * h, 0.0);
        // Horizontal pass.
        for y in 0..h {
            let row = &src[y * w..(y + 1) * w];
            let dst = &mut tmp[y * w..(y + 1) * w];
            let mut prev = [0.0f32; 3];
            let mut prev2 = [0.0f32; 3];
            let mut n = -n_r + 1;
            while n < w as isize {
                let left = n - n_r - 1;
                let right = n + n_r - 1;
                let lv = if left >= 0 { row[left as usize] } else { 0.0 };
                let rv = if right < w as isize {
                    row[right as usize]
                } else {
                    0.0
                };
                let sum = lv + rv;
                let mut total = 0.0;
                for k in 0..3 {
                    let o = self.n2[k] * sum - self.d1[k] * prev[k] - prev2[k];
                    prev2[k] = prev[k];
                    prev[k] = o;
                    total += o;
                }
                if n >= 0 {
                    dst[n as usize] = total;
                }
                n += 1;
            }
        }
        // Vertical pass, all columns at once.
        let mut prev = vec![[0.0f32; 3]; w];
        let mut prev2 = vec![[0.0f32; 3]; w];
        let mut n = -n_r + 1;
        while n < h as isize {
            let top = n - n_r - 1;
            let bottom = n + n_r - 1;
            for x in 0..w {
                let tv = if top >= 0 {
                    tmp[top as usize * w + x]
                } else {
                    0.0
                };
                let bv = if bottom < h as isize {
                    tmp[bottom as usize * w + x]
                } else {
                    0.0
                };
                let sum = tv + bv;
                let mut total = 0.0;
                for k in 0..3 {
                    let o = self.n2[k] * sum - self.d1[k] * prev[x][k] - prev2[x][k];
                    prev2[x][k] = prev[x][k];
                    prev[x][k] = o;
                    total += o;
                }
                if n >= 0 {
                    out[n as usize * w + x] = total;
                }
            }
            n += 1;
        }
    }

    fn blur_planes(&self, img: &Planes) -> Planes {
        let channels = [0usize, 1, 2];
        let planes = par_map(&channels, |&c| {
            let mut out = vec![0.0f32; img.w * img.h];
            let mut tmp = Vec::new();
            self.blur(&img.p[c], img.w, img.h, &mut out, &mut tmp);
            out
        });
        let [a, b, c]: [Vec<f32>; 3] = planes.try_into().unwrap();
        Planes {
            w: img.w,
            h: img.h,
            p: [a, b, c],
        }
    }
}

fn invert3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv_det = 1.0 / det;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ]
}

fn product(a: &Planes, b: &Planes) -> Planes {
    let mut out = Planes::new(a.w, a.h);
    for c in 0..3 {
        for i in 0..a.w * a.h {
            out.p[c][i] = a.p[c][i] * b.p[c][i];
        }
    }
    out
}

#[derive(Default, Clone, Copy)]
struct ScaleScores {
    ssim: [f64; 6],
    edge: [f64; 12],
}

fn quartic(x: f64) -> f64 {
    let x2 = x * x;
    x2 * x2
}

fn ssim_map(
    m1: &Planes,
    m2: &Planes,
    s11: &Planes,
    s22: &Planes,
    s12: &Planes,
    out: &mut [f64; 6],
) {
    let one_per_pixels = 1.0 / (m1.w * m1.h) as f64;
    for c in 0..3 {
        let (mut s1, mut s4) = (0.0f64, 0.0f64);
        for i in 0..m1.w * m1.h {
            let mu1 = m1.p[c][i];
            let mu2 = m2.p[c][i];
            let mu11 = mu1 * mu1;
            let mu22 = mu2 * mu2;
            let mu12 = mu1 * mu2;
            let num_m = 1.0 - (mu1 - mu2) * (mu1 - mu2);
            let num_s = 2.0 * (s12.p[c][i] - mu12) + C2;
            let denom_s = (s11.p[c][i] - mu11) + (s22.p[c][i] - mu22) + C2;
            let d = (1.0 - (num_m * num_s / denom_s) as f64).max(0.0);
            s1 += d;
            s4 += quartic(d);
        }
        out[c * 2] = one_per_pixels * s1;
        out[c * 2 + 1] = (one_per_pixels * s4).sqrt().sqrt();
    }
}

fn edge_diff_map(img1: &Planes, mu1: &Planes, img2: &Planes, mu2: &Planes, out: &mut [f64; 12]) {
    let one_per_pixels = 1.0 / (img1.w * img1.h) as f64;
    for c in 0..3 {
        let mut s = [0.0f64; 4];
        for i in 0..img1.w * img1.h {
            let d1 = (1.0 + (img2.p[c][i] - mu2.p[c][i]).abs() as f64)
                / (1.0 + (img1.p[c][i] - mu1.p[c][i]).abs() as f64)
                - 1.0;
            let artifact = d1.max(0.0);
            s[0] += artifact;
            s[1] += quartic(artifact);
            let detail_lost = (-d1).max(0.0);
            s[2] += detail_lost;
            s[3] += quartic(detail_lost);
        }
        out[c * 4] = one_per_pixels * s[0];
        out[c * 4 + 1] = (one_per_pixels * s[1]).sqrt().sqrt();
        out[c * 4 + 2] = one_per_pixels * s[2];
        out[c * 4 + 3] = (one_per_pixels * s[3]).sqrt().sqrt();
    }
}

fn score_against(orig: &[OrigScale], distorted: &Image, bg: Option<f32>) -> f64 {
    let gauss = RecursiveGaussian::new(1.5);
    let mut lin2 = to_linear(distorted, bg);
    let mut scales = Vec::with_capacity(NUM_SCALES);
    for (scale, o) in orig.iter().enumerate() {
        if scale > 0 {
            lin2 = downsample(&lin2);
        }
        let img2 = to_positive_xyb(&lin2);
        let sigma2_sq = gauss.blur_planes(&product(&img2, &img2));
        let sigma12 = gauss.blur_planes(&product(&o.xyb, &img2));
        let mu2 = gauss.blur_planes(&img2);
        let mut s = ScaleScores::default();
        ssim_map(&o.mu, &mu2, &o.sigma_sq, &sigma2_sq, &sigma12, &mut s.ssim);
        edge_diff_map(&o.xyb, &o.mu, &img2, &mu2, &mut s.edge);
        scales.push(s);
    }
    combine(&scales)
}

#[rustfmt::skip]
const WEIGHT: [f64; 108] = [
    0.0, 0.0007376606707406586, 0.0, 0.0, 0.0007793481682867309, 0.0, 0.0,
    0.0004371155730107379, 0.0, 1.1041726426657346, 0.00066284834129271,
    0.00015231632783718752, 0.0, 0.0016406437456599754, 0.0, 1.8422455520539298,
    11.441172603757666, 0.0, 0.0007989109436015163, 0.000176816438078653, 0.0,
    1.8787594979546387, 10.94906990605142, 0.0, 0.0007289346991508072,
    0.9677937080626833, 0.0, 0.00014003424285435884, 0.9981766977854967,
    0.00031949755934435053, 0.0004550992113792063, 0.0, 0.0, 0.0013648766163243398,
    0.0, 0.0, 0.0, 0.0, 0.0, 7.466890328078848, 0.0, 17.445833984131262,
    0.0006235601634041466, 0.0, 0.0, 6.683678146179332, 0.00037724407979611296,
    1.027889937768264, 225.20515300849274, 0.0, 0.0, 19.213238186143016,
    0.0011401524586618361, 0.001237755635509985, 176.39317598450694, 0.0, 0.0,
    24.43300999870476, 0.28520802612117757, 0.0004485436923833408, 0.0, 0.0, 0.0,
    34.77906344483772, 44.835625328877896, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    0.0008680556573291698, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0005313191874358747, 0.0,
    0.00016533814161379112, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0004179171803251336,
    0.0017290828234722833, 0.0, 0.0020827005846636437, 0.0, 0.0, 8.826982764996862,
    23.19243343998926, 0.0, 95.1080498811086, 0.9863978034400682, 0.9834382792465353,
    0.0012286405048278493, 171.2667255897307, 0.9807858872435379, 0.0, 0.0, 0.0,
    0.0005130064588990679, 0.0, 0.00010854057858411537,
];

fn combine(scales: &[ScaleScores]) -> f64 {
    let mut ssim = 0.0;
    let mut i = 0;
    // Same iteration order as the reference: the weight index advances only
    // over scales that were actually computed.
    for c in 0..3 {
        for s in scales {
            for n in 0..2 {
                ssim += WEIGHT[i] * s.ssim[c * 2 + n].abs();
                ssim += WEIGHT[i + 1] * s.edge[c * 4 + n].abs();
                ssim += WEIGHT[i + 2] * s.edge[c * 4 + n + 2].abs();
                i += 3;
            }
        }
    }
    ssim *= 0.956_238_261_683_484_4;
    ssim = 2.326_765_642_916_932 * ssim - 0.020_884_521_182_843_837 * ssim * ssim
        + 6.248_496_625_763_138e-5 * ssim * ssim * ssim;
    if ssim > 0.0 {
        100.0 - 10.0 * ssim.powf(0.627_633_646_783_138_7)
    } else {
        100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u16; 4]) -> Image {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&f(x, y));
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
    fn identical_is_100() {
        let a = image(40, 30, |x, y| [(x * 6) as u16, (y * 8) as u16, 100, 255]);
        assert!((ssimulacra2(&a, &a).unwrap() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn more_distortion_scores_lower() {
        let a = image(64, 64, |x, y| {
            [
                ((x * 4) % 256) as u16,
                ((y * 4) % 256) as u16,
                ((x + y) % 256) as u16,
                255,
            ]
        });
        let noisy = |amp: i32| {
            image(64, 64, |x, y| {
                let n = (((x * 7919 + y * 104_729) % 17) as i32 - 8) * amp / 8;
                let p = &a.px[((y * 64 + x) * 4) as usize..][..4];
                [
                    (p[0] as i32 + n).clamp(0, 255) as u16,
                    (p[1] as i32 - n).clamp(0, 255) as u16,
                    p[2],
                    255,
                ]
            })
        };
        let s1 = ssimulacra2(&a, &noisy(2)).unwrap();
        let s2 = ssimulacra2(&a, &noisy(8)).unwrap();
        let s3 = ssimulacra2(&a, &noisy(32)).unwrap();
        assert!(100.0 > s1 && s1 > s2 && s2 > s3, "{s1} {s2} {s3}");
    }

    #[test]
    fn gaussian_is_normalized() {
        let g = RecursiveGaussian::new(1.5);
        let (w, h) = (64, 64);
        let src = vec![1.0f32; w * h];
        let mut out = vec![0.0; w * h];
        let mut tmp = Vec::new();
        g.blur(&src, w, h, &mut out, &mut tmp);
        let center = out[32 * w + 32];
        assert!((center - 1.0).abs() < 1e-3, "{center}");
        assert!(out[0] < 0.9, "zero padding darkens corners");
    }
}
