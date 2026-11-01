//! Visually-lossless JPEG recompression in the DCT domain.
//!
//! The file is never decoded to pixels and re-encoded (which would stack a
//! second round of blocking and ringing on the first). Instead:
//!
//! 1. Requantization: each quantization step is scaled up with a perceptual
//!    frequency weighting, larger for high frequencies where the eye is less
//!    sensitive (the principle behind Watson's DCTune, "DCT quantization
//!    matrices visually optimized for individual images", SPIE 1993). Each
//!    coefficient is mapped to the new step by rounding its exact dequantized
//!    value, so no information beyond the original quantization is invented.
//! 2. Rate-weighted thresholding: a ±1 coefficient after a long zero run costs
//!    many bits and contributes little; it is zeroed when the distortion it
//!    removes is small relative to an estimate of its bit cost (a simplified
//!    rate-distortion decision in the spirit of trellis quantization,
//!    Ramchandran & Vetterli, IEEE TIP 1994).
//!
//! One strength parameter drives both; the caller searches it against
//! SSIMULACRA 2. Chroma-only tables are scaled harder for 4:4:4 images (the
//! eye resolves color detail poorly); for subsampled images this was measured
//! to lose, as was raising the zeroing threshold in textured blocks.

use super::render::ZIGZAG;
use super::Jpeg;

/// Frequency weight in 0..=1 for zigzag position `k` (0 = DC, 1 = highest).
fn frequency(k: usize) -> f32 {
    let n = ZIGZAG[k];
    ((n % 8) + (n / 8)) as f32 / 14.0
}

/// Scaled quantization table. Steps never decrease and stay 8-bit where they were.
fn scale_table(q: &[u16; 64], strength: f32) -> [u16; 64] {
    let mut out = [0u16; 64];
    for k in 0..64 {
        let factor = 1.0 + strength * (0.25 + 1.75 * frequency(k));
        let limit = if q[k] <= 255 { 255 } else { q[k] };
        out[k] = ((q[k] as f32 * factor).round() as u16).clamp(q[k], limit.max(q[k]));
    }
    out
}

/// Tuning knobs for [`requantize`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Params {
    /// Overall strength; 0 leaves the image unchanged.
    pub strength: f32,
    /// Rate-weighted zeroing of isolated ±1 coefficients.
    pub rdo: bool,
    /// Extra table scaling for chroma-only tables (1 = same as luma).
    pub chroma: f32,
}

/// Builds a new JPEG with requantized coefficients.
pub(super) fn requantize(jpeg: &Jpeg, p: Params) -> Jpeg {
    let strength = p.strength;
    let rdo = p.rdo;
    // Tables used only by chroma components get the chroma multiplier.
    let luma_table = jpeg.frame.comps[0].tq as usize;
    let mut tables: [Option<[u16; 64]>; 4] = [None; 4];
    for (i, t) in jpeg.qtables.iter().enumerate() {
        let chroma_only = jpeg.frame.comps.len() == 3 && i != luma_table;
        let s = if chroma_only {
            strength * p.chroma
        } else {
            strength
        };
        tables[i] = t.map(|q| scale_table(&q, s));
    }

    let mut coeffs = jpeg.coeffs.clone();
    for (c, comp) in jpeg.frame.comps.iter().enumerate() {
        let (Some(old), Some(new)) = (jpeg.qtables[comp.tq as usize], tables[comp.tq as usize])
        else {
            continue;
        };
        let ratio: Vec<f32> = (0..64).map(|k| old[k] as f32 / new[k] as f32).collect();
        for block in coeffs[c].iter_mut() {
            let mut exact = [0.0f32; 64];
            for k in 0..64 {
                exact[k] = block[k] as f32 * ratio[k];
                let r = exact[k].abs().round().copysign(exact[k]);
                block[k] = r as i16;
            }
            if rdo && strength > 0.0 {
                // Distortion of zeroing ±1 is (2|v|-1) squared steps; its rate is
                // roughly the Huffman code for (run, size 1) plus one bit, which
                // grows with the zero run in front of it.
                let mut run = 0usize;
                for k in 1..64 {
                    if block[k] == 0 {
                        run += 1;
                        continue;
                    }
                    if block[k].abs() == 1 {
                        let delta_d = 2.0 * exact[k].abs() - 1.0;
                        let bits = 3.0 + run as f32 * 0.75;
                        if delta_d < strength * 0.12 * bits {
                            block[k] = 0;
                            run += 1;
                            continue;
                        }
                    }
                    run = 0;
                }
            }
        }
    }

    // One DQT segment with every defined table.
    let mut dqt = Vec::new();
    let mut wide_any = false;
    for (id, t) in tables.iter().enumerate() {
        if let Some(t) = t {
            let wide = t.iter().any(|&v| v > 255);
            wide_any |= wide;
            dqt.push((wide as u8) << 4 | id as u8);
            for &v in t {
                if wide {
                    dqt.extend_from_slice(&v.to_be_bytes());
                } else {
                    dqt.push(v as u8);
                }
            }
        }
    }
    Jpeg {
        frame: jpeg.frame.clone(),
        segments: jpeg.segments.clone(),
        dqt: vec![dqt],
        dqt_16bit: wide_any,
        qtables: tables,
        qtables_redefined: false,
        coeffs,
        trailing: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_scale_more_at_high_frequency() {
        let q = [10u16; 64];
        let s = scale_table(&q, 1.0);
        assert!(s[0] < s[63]);
        assert!(s.iter().zip(q.iter()).all(|(a, b)| a >= b));
        assert_eq!(scale_table(&q, 0.0), q);
        assert!(scale_table(&[200; 64], 5.0).iter().all(|&v| v == 255));
    }
}
