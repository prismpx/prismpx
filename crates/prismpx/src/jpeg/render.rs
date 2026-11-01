//! Decoding quantized coefficients to RGB pixels, for perceptual scoring.
//!
//! Float IDCT, "fancy" (triangle-filter) chroma upsampling like libjpeg's
//! default, and JFIF YCbCr→RGB conversion. Results are within a level or two
//! of libjpeg, which is plenty for comparing two versions of one image.

// Index loops over parallel planes and fixed 8x8 arrays read more clearly here.
#![allow(clippy::needless_range_loop)]

use super::Jpeg;
use crate::error::{Error, Result};
use crate::png::Image;

/// Natural (row-major) index of each zigzag position.
pub(crate) const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn idct_table() -> [[f32; 8]; 8] {
    // t[x][u] = C(u)/2 * cos((2x+1)uπ/16)
    let mut t = [[0.0f32; 8]; 8];
    for (x, row) in t.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            let c = if u == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            *v = 0.5 * c * (((2 * x + 1) * u) as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }
    t
}

/// Inverse DCT of one block of dequantized coefficients (natural order).
fn idct(coef: &[f32; 64], t: &[[f32; 8]; 8], out: &mut [f32; 64]) {
    let mut tmp = [0.0f32; 64];
    // Columns: tmp[y][u] = sum_v t[y][v] * coef[v][u]
    for u in 0..8 {
        for y in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                s += t[y][v] * coef[v * 8 + u];
            }
            tmp[y * 8 + u] = s;
        }
    }
    // Rows: out[y][x] = sum_u t[x][u] * tmp[y][u]
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for u in 0..8 {
                s += t[x][u] * tmp[y * 8 + u];
            }
            out[y * 8 + x] = s;
        }
    }
}

/// How the components map to color.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorModel {
    Gray,
    YCbCr,
    Rgb,
}

fn color_model(jpeg: &Jpeg) -> Result<ColorModel> {
    let adobe = jpeg
        .segments
        .iter()
        .find(|(m, p)| *m == 0xEE && p.starts_with(b"Adobe") && p.len() >= 12)
        .map(|(_, p)| p[11]);
    match jpeg.frame.comps.len() {
        1 => Ok(ColorModel::Gray),
        3 => {
            let ids: Vec<u8> = jpeg.frame.comps.iter().map(|c| c.id).collect();
            if adobe == Some(0) || (adobe.is_none() && ids == b"RGB") {
                Ok(ColorModel::Rgb)
            } else {
                Ok(ColorModel::YCbCr)
            }
        }
        n => Err(Error::Unsupported(format!("rendering {n}-component JPEG"))),
    }
}

/// Decodes the image to 8-bit RGBA pixels.
pub(crate) fn render(jpeg: &Jpeg) -> Result<Image> {
    let model = color_model(jpeg)?;
    let f = &jpeg.frame;
    let (w, h) = (f.width as usize, f.height as usize);
    let (hmax, vmax) = (f.hmax(), f.vmax());
    let t = idct_table();

    // Full-resolution planes, one per component.
    let mut planes: Vec<Vec<f32>> = Vec::with_capacity(f.comps.len());
    for (c, comp) in f.comps.iter().enumerate() {
        let q = jpeg.qtables[comp.tq as usize]
            .ok_or(Error::Malformed("missing quantization table".into()))?;
        let (sw, sh) = f.storage(c);
        let (pw, ph) = (sw * 8, sh * 8);
        let mut plane = vec![0.0f32; pw * ph];
        let mut deq = [0.0f32; 64];
        let mut px = [0.0f32; 64];
        for by in 0..sh {
            for bx in 0..sw {
                let block = &jpeg.coeffs[c][by * sw + bx];
                for k in 0..64 {
                    deq[ZIGZAG[k]] = block[k] as f32 * q[k] as f32;
                }
                idct(&deq, &t, &mut px);
                for y in 0..8 {
                    let row = (by * 8 + y) * pw + bx * 8;
                    for x in 0..8 {
                        plane[row + x] = (px[y * 8 + x] + 128.0).clamp(0.0, 255.0);
                    }
                }
            }
        }
        // Upsample to full resolution with a triangle filter (sample centers aligned).
        let (fx, fy) = (hmax / comp.h as usize, vmax / comp.v as usize);
        if fx == 1 && fy == 1 {
            let mut full = vec![0.0f32; w * h];
            for y in 0..h {
                full[y * w..(y + 1) * w].copy_from_slice(&plane[y * pw..y * pw + w]);
            }
            planes.push(full);
            continue;
        }
        let (cw, ch) = (
            (w * comp.h as usize).div_ceil(hmax),
            (h * comp.v as usize).div_ceil(vmax),
        );
        let sample = |x: usize, y: usize| plane[y.min(ch - 1) * pw + x.min(cw - 1)];
        let mut full = vec![0.0f32; w * h];
        for y in 0..h {
            let sy = ((y as f32 + 0.5) / fy as f32 - 0.5).max(0.0);
            let (y0, wy) = (sy.floor() as usize, sy.fract());
            for x in 0..w {
                let sx = ((x as f32 + 0.5) / fx as f32 - 0.5).max(0.0);
                let (x0, wx) = (sx.floor() as usize, sx.fract());
                let top = sample(x0, y0) * (1.0 - wx) + sample(x0 + 1, y0) * wx;
                let bot = sample(x0, y0 + 1) * (1.0 - wx) + sample(x0 + 1, y0 + 1) * wx;
                full[y * w + x] = top * (1.0 - wy) + bot * wy;
            }
        }
        planes.push(full);
    }

    let to8 = |v: f32| v.round().clamp(0.0, 255.0) as u16;
    let mut px = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let rgb = match model {
            ColorModel::Gray => {
                let g = to8(planes[0][i]);
                [g, g, g]
            }
            ColorModel::Rgb => [to8(planes[0][i]), to8(planes[1][i]), to8(planes[2][i])],
            ColorModel::YCbCr => {
                let (y, cb, cr) = (planes[0][i], planes[1][i] - 128.0, planes[2][i] - 128.0);
                [
                    to8(y + 1.402 * cr),
                    to8(y - 0.344_136 * cb - 0.714_136 * cr),
                    to8(y + 1.772 * cb),
                ]
            }
        };
        px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    Ok(Image {
        width: w as u32,
        height: h as u32,
        bit16: false,
        px,
    })
}
