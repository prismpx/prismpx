//! Lossless PNG optimization.
//!
//! Pipeline: parse → decode to canonical RGBA → find the smallest exact
//! representation (color type, bit depth, palette order) → choose row filters
//! → DEFLATE → reassemble with the chunks the metadata policy keeps → verify.

mod decode;
mod encode;
mod filter;

pub use decode::decode;

use crate::error::{Error, Result};
use crate::options::Options;

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

    fn to_8bit(&self) -> Image {
        Image {
            width: self.width,
            height: self.height,
            bit16: false,
            px: self.px.iter().map(|&v| v & 0xFF).collect(),
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

pub(crate) fn optimize(input: &[u8], opts: &Options) -> Result<(Vec<u8>, String)> {
    let parsed = decode::parse(input)?;
    let mut img = decode::decode_image(&parsed)?;
    if opts.clean_transparent {
        img.clean_transparent();
    }
    let reference = opts.verify.then(|| img.to_rgba16());
    if img.reducible_to_8bit() {
        img = img.to_8bit();
    }

    let (png, description) = encode::encode_best(&img, &parsed, opts);

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
