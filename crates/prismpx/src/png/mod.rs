//! PNG optimization.
//!
//! Lossless pipeline: parse → decode to canonical RGBA → find the smallest
//! exact representation (color type, bit depth, palette order) → choose row
//! filters → DEFLATE → reassemble with the chunks the metadata policy keeps →
//! verify.
//!
//! Visually-lossless pipeline: generate lossy candidates (16→8 bit, palette
//! quantization with and without dithering at varying palette sizes), keep
//! those whose SSIMULACRA2 score against the original reaches the target, run
//! each through the lossless pipeline and return the smallest file.

mod decode;
mod encode;
mod filter;
mod quantize;

pub use decode::decode;

use crate::error::{Error, Result};
use crate::metrics::Reference;
use crate::options::{Mode, Options};

pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// A decoded image in canonical form: RGBA samples, 8-bit (0..=255) unless
/// `bit16`, in which case samples are 0..=65535. Low-bit-depth grayscale is
/// scaled up exactly to 8 bits (e.g. 1-bit white becomes 255).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bit16: bool,
    pub px: Vec<u16>,
}

impl Image {
    pub fn max_value(&self) -> u16 {
        if self.bit16 {
            65535
        } else {
            255
        }
    }

    /// All samples on the 16-bit scale, for exact comparison across depths.
    pub fn to_rgba16(&self) -> Vec<u16> {
        if self.bit16 {
            self.px.clone()
        } else {
            self.px.iter().map(|&v| v * 257).collect()
        }
    }

    /// True if every 16-bit sample is an exact 8-bit value scaled by 257.
    fn reducible_to_8bit(&self) -> bool {
        self.bit16 && self.px.iter().all(|&v| v >> 8 == v & 0xFF)
    }

    /// Converts to 8 bits per sample, rounding to nearest.
    fn to_8bit(&self) -> Image {
        if !self.bit16 {
            return self.clone();
        }
        Image {
            width: self.width,
            height: self.height,
            bit16: false,
            px: self
                .px
                .iter()
                .map(|&v| ((v as u32 * 255 + 32767) / 65535) as u16)
                .collect(),
        }
    }

    fn clean_transparent(&mut self) {
        for p in self.px.chunks_exact_mut(4) {
            if p[3] == 0 {
                p[0] = 0;
                p[1] = 0;
                p[2] = 0;
            }
        }
    }
}

/// Encodes `img` with the lossless pipeline and checks that the output
/// decodes back to exactly `img` (if verification is on).
fn encode_exact(img: &Image, parsed: &decode::Parsed, opts: &Options) -> Result<(Vec<u8>, String)> {
    let reference = opts.verify.then(|| img.to_rgba16());
    let work = if img.reducible_to_8bit() {
        img.to_8bit()
    } else {
        img.clone()
    };
    let (png, description) = encode::encode_best(&work, parsed, opts);
    if let Some(reference) = reference {
        let back = decode(&png)
            .map_err(|e| Error::VerificationFailed(format!("output does not decode: {e}")))?;
        if back.width != img.width || back.height != img.height || back.to_rgba16() != reference {
            return Err(Error::VerificationFailed(
                "output pixels differ from input".into(),
            ));
        }
    }
    Ok((png, description))
}

pub(crate) fn optimize(input: &[u8], opts: &Options) -> Result<(Vec<u8>, String)> {
    let parsed = decode::parse(input)?;
    let mut img = decode::decode_image(&parsed)?;
    if opts.clean_transparent {
        img.clean_transparent();
    }
    let lossless = encode_exact(&img, &parsed, opts)?;
    match opts.mode {
        Mode::Lossless => Ok(lossless),
        Mode::VisuallyLossless => optimize_lossy(&img, &parsed, opts, lossless),
    }
}

/// Error bounds (in 8-bit sample units) tried by the near-lossless search.
const NEAR_LOSSLESS_DELTAS: [u8; 12] = [1, 2, 3, 4, 5, 6, 8, 10, 12, 14, 16, 20];

/// Palette sizes tried, smallest first.
const PALETTE_SIZES: [usize; 13] = [2, 4, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256];

struct Candidate {
    png: Vec<u8>,
    description: String,
}

fn optimize_lossy(
    original: &Image,
    parsed: &decode::Parsed,
    opts: &Options,
    lossless: (Vec<u8>, String),
) -> Result<(Vec<u8>, String)> {
    if original.width < 8 || original.height < 8 {
        let (png, d) = lossless;
        return Ok((
            png,
            format!("{d}; too small for perceptual search, kept lossless"),
        ));
    }
    let target = opts.quality as f64;
    let mut best = Candidate {
        png: lossless.0,
        description: format!("{}; lossless was smallest", lossless.1),
    };
    let mut consider = |img: &Image, label: String, score: f64| -> Result<()> {
        let (png, d) = encode_exact(img, parsed, opts)?;
        if png.len() < best.png.len() {
            best = Candidate {
                png,
                description: format!(
                    "{d}; lossy: {label}, ssimulacra2={score:.2} (target {target})"
                ),
            };
        }
        Ok(())
    };

    let reference = Reference::new(original)?;
    let base = original.to_8bit();
    if original.bit16 {
        let s = reference.score(&base)?;
        if s >= target {
            consider(&base, "16-bit to 8-bit".into(), s)?;
        }
    }

    // Binary search for the smallest palette size at which a dithered or an
    // undithered remap meets the target. Both share one palette per size and
    // are scored concurrently.
    let prep = quantize::Prepared::new(&base);
    type Variant = (f32, Image, f64);
    let eval = |i: usize| -> Result<Vec<Variant>> {
        let pal = prep.palette(PALETTE_SIZES[i]);
        let dithers = [1.0f32, 0.0];
        let scored = par_map(&dithers, |&d| -> Result<Variant> {
            let img = prep.remap(&pal, d);
            let s = reference.score(&img)?;
            Ok((d, img, s))
        });
        let mut passing = Vec::new();
        for v in scored {
            let v = v?;
            if v.2 >= target {
                passing.push(v);
            }
        }
        Ok(passing)
    };
    let (mut lo, mut hi) = (0usize, PALETTE_SIZES.len() - 1);
    let mut found = eval(hi)?;
    if !found.is_empty() {
        let mut found_at = hi;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let passing = eval(mid)?;
            if passing.is_empty() {
                lo = mid + 1;
            } else {
                found = passing;
                found_at = mid;
                hi = mid;
            }
        }
        for (dither, img, s) in found {
            let label = format!(
                "{}-color palette{}",
                PALETTE_SIZES[found_at],
                if dither > 0.0 { ", dithered" } else { "" }
            );
            consider(&img, label, s)?;
        }
    }

    // Near-lossless: the largest uniform error bound that meets the target.
    // (A texture-adaptive bound was tried and lost: flat-but-noisy photo
    // regions are where the bits are, and mixed step sizes hurt DEFLATE.)
    let search = || -> Result<Option<(encode::NearLossless, f64)>> {
        let (mut lo, mut hi) = (0usize, NEAR_LOSSLESS_DELTAS.len());
        let mut found = None;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let d = NEAR_LOSSLESS_DELTAS[mid];
            let nl = encode::near_lossless(&base, d);
            let s = reference.score(&nl.recon)?;
            if s >= target {
                found = Some((nl, s));
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(found)
    };
    if let Some((nl, s)) = search()? {
        let (png, d) = encode::finish_near_lossless(&nl, parsed, opts);
        if opts.verify {
            let back = decode(&png)
                .map_err(|e| Error::VerificationFailed(format!("output does not decode: {e}")))?;
            if back.to_rgba16() != nl.recon.to_rgba16() {
                return Err(Error::VerificationFailed(
                    "near-lossless output differs from its reconstruction".into(),
                ));
            }
        }
        if png.len() < best.png.len() {
            best = Candidate {
                png,
                description: format!(
                    "{d}; lossy: {}, ssimulacra2={s:.2} (target {target})",
                    nl.label
                ),
            };
        }
    }
    Ok((best.png, best.description))
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
