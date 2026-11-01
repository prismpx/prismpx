//! Lossless JPEG optimization.
//!
//! The entropy-coded data is decoded to quantized DCT coefficients and
//! re-encoded with Huffman tables built for this image (package-merge,
//! 16-bit length limit), as both a baseline and a progressive file; the
//! smaller wins. Quantization tables and coefficients are untouched, so the
//! decoded pixels are identical. Metadata is filtered by policy; EXIF can be
//! reduced to just its orientation tag.

mod decode;
mod encode;
mod huffman;
mod lossy;
mod metadata;
mod render;

pub(crate) use decode::decode;

/// Decodes a JPEG to 8-bit RGBA pixels (1- and 3-component images).
pub fn render_pixels(data: &[u8]) -> Result<crate::png::Image> {
    render::render(&decode(data)?)
}

use crate::error::{Error, Result};
use crate::metrics::Reference;
use crate::options::{Mode, Options};
use crate::parallel::par_map;

/// One image component as declared in the frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub tq: u8,
}

/// Frame header plus derived block geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub progressive: bool,
    pub width: u16,
    pub height: u16,
    pub comps: Vec<Component>,
}

impl Frame {
    pub fn hmax(&self) -> usize {
        self.comps.iter().map(|c| c.h as usize).max().unwrap_or(1)
    }

    pub fn vmax(&self) -> usize {
        self.comps.iter().map(|c| c.v as usize).max().unwrap_or(1)
    }

    /// MCUs per row / column for interleaved scans.
    pub fn mcus(&self) -> (usize, usize) {
        (
            (self.width as usize).div_ceil(8 * self.hmax()),
            (self.height as usize).div_ceil(8 * self.vmax()),
        )
    }

    /// Blocks that cover the component's own samples (non-interleaved scans).
    pub fn blocks(&self, c: usize) -> (usize, usize) {
        let comp = &self.comps[c];
        let w = (self.width as usize * comp.h as usize).div_ceil(self.hmax());
        let h = (self.height as usize * comp.v as usize).div_ceil(self.vmax());
        (w.div_ceil(8), h.div_ceil(8))
    }

    /// Stored blocks per component, padded to whole MCUs.
    pub fn storage(&self, c: usize) -> (usize, usize) {
        let (mx, my) = self.mcus();
        (mx * self.comps[c].h as usize, my * self.comps[c].v as usize)
    }

    /// Whether all components fit in one interleaved scan (at most 10 blocks per MCU).
    pub fn can_interleave(&self) -> bool {
        self.comps.len() <= 4
            && self
                .comps
                .iter()
                .map(|c| c.h as usize * c.v as usize)
                .sum::<usize>()
                <= 10
    }

    fn describe(&self) -> String {
        let mode = if self.progressive {
            "progressive"
        } else {
            "baseline"
        };
        let sub = match self.comps.len() {
            1 => "gray".to_string(),
            3 => {
                let y = self.comps[0];
                match (y.h, y.v, self.comps[1].h, self.comps[1].v) {
                    (1, 1, 1, 1) => "4:4:4".into(),
                    (2, 1, 1, 1) => "4:2:2".into(),
                    (2, 2, 1, 1) => "4:2:0".into(),
                    (1, 2, 1, 1) => "4:4:0".into(),
                    (4, 1, 1, 1) => "4:1:1".into(),
                    _ => "custom sampling".into(),
                }
            }
            n => format!("{n} components"),
        };
        format!("{mode} {sub}")
    }
}

/// A decoded JPEG: everything needed to write it back out losslessly.
pub(crate) struct Jpeg {
    pub frame: Frame,
    /// APPn and COM segments in file order: (marker, payload).
    pub segments: Vec<(u8, Vec<u8>)>,
    /// DQT segment payloads, in file order.
    pub dqt: Vec<Vec<u8>>,
    /// True if any quantization table uses 16-bit precision.
    pub dqt_16bit: bool,
    /// Quantization tables by id, zigzag order (final definitions).
    pub qtables: [Option<[u16; 64]>; 4],
    /// True if a table id was redefined with different values mid-file.
    pub qtables_redefined: bool,
    /// Quantized coefficients per component, zigzag order, padded storage.
    pub coeffs: Vec<Vec<[i16; 64]>>,
    /// Bytes after EOI (e.g. multi-picture extras).
    pub trailing: usize,
}

impl Jpeg {
    /// True if the coefficients of all blocks inside the image match.
    fn same_image(&self, other: &Jpeg) -> bool {
        if self.frame.width != other.frame.width
            || self.frame.height != other.frame.height
            || self.frame.comps != other.frame.comps
        {
            return false;
        }
        (0..self.frame.comps.len()).all(|c| {
            let (bw, bh) = self.frame.blocks(c);
            let (sw, _) = self.frame.storage(c);
            (0..bh)
                .all(|y| (0..bw).all(|x| self.coeffs[c][y * sw + x] == other.coeffs[c][y * sw + x]))
        })
    }
}

/// Encodes `jpeg` and, if verification is on, checks that the output decodes
/// to the same coefficients and quantization tables.
fn encode_verified(
    jpeg: &Jpeg,
    segments: &[(u8, Vec<u8>)],
    opts: &Options,
) -> Result<(Vec<u8>, &'static str)> {
    let (out, layout) = encode::encode_best(jpeg, segments, opts.effort);
    if opts.verify {
        let back = decode(&out)
            .map_err(|e| Error::VerificationFailed(format!("output does not decode: {e}")))?;
        if !jpeg.same_image(&back) || jpeg.qtables != back.qtables {
            return Err(Error::VerificationFailed(
                "output coefficients differ from input".into(),
            ));
        }
    }
    Ok((out, layout))
}

/// Largest requantization strength tried.
const MAX_STRENGTH: f32 = 4.0;
/// Chroma table scaling relative to luma, for 4:4:4 images.
const CHROMA_444: f32 = 2.5;
/// Bisection steps for the strength search (resolution MAX_STRENGTH / 2^steps).
const SEARCH_STEPS: usize = 6;

pub(crate) fn optimize(input: &[u8], opts: &Options) -> Result<(Vec<u8>, String)> {
    let jpeg = decode(input)?;
    let (segments, meta_note) = metadata::filter(&jpeg.segments, opts.metadata);
    let (out, layout) = encode_verified(&jpeg, &segments, opts)?;

    let mut desc = format!(
        "jpeg: {} -> {layout}, optimized Huffman tables",
        jpeg.frame.describe()
    );
    if !meta_note.is_empty() {
        desc.push_str(&format!(", {meta_note}"));
    }
    if jpeg.trailing > 0 {
        desc.push_str(&format!(", dropped {} bytes after EOI", jpeg.trailing));
    }
    if opts.mode == Mode::Lossless {
        return Ok((out, desc));
    }

    let unsupported = if !matches!(jpeg.frame.comps.len(), 1 | 3) {
        Some("CMYK/YCCK")
    } else if jpeg.qtables_redefined {
        Some("redefined quantization tables")
    } else if jpeg.frame.width < 8 || jpeg.frame.height < 8 {
        Some("image smaller than 8x8")
    } else {
        None
    };
    if let Some(why) = unsupported {
        return Ok((out, format!("{desc}; lossy skipped ({why}), lossless only")));
    }

    let original = render::render(&jpeg)?;
    let reference = Reference::new(&original)?;
    let target = opts.quality as f64;
    let score = |cand: &Jpeg| -> Result<f64> { reference.score(&render::render(cand)?) };
    let full_chroma =
        jpeg.frame.comps.len() == 3 && jpeg.frame.comps.iter().all(|c| c.h == 1 && c.v == 1);
    let chroma = if full_chroma { CHROMA_444 } else { 1.0 };

    // For plain requantization and requantization + rate-weighted zeroing,
    // bisect for the largest strength that still meets the target.
    let search = |&rdo: &bool| -> Result<Option<(Jpeg, f32, f64)>> {
        let params = |strength: f32| lossy::Params {
            strength,
            rdo,
            chroma,
        };
        let (mut lo, mut hi) = (0.0f32, MAX_STRENGTH);
        let mut best: Option<(Jpeg, f32, f64)> = None;
        for _ in 0..SEARCH_STEPS {
            let mid = (lo + hi) / 2.0;
            let cand = lossy::requantize(&jpeg, params(mid));
            let s = score(&cand)?;
            if s >= target {
                best = Some((cand, mid, s));
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(best)
    };
    let mut best: (Vec<u8>, String) = (out, format!("{desc}; lossless was smallest"));
    let modes = [false, true];
    for (found, rdo) in par_map(&modes, search).into_iter().zip(modes) {
        let Some((cand, strength, s)) = found? else {
            continue;
        };
        let (bytes, layout) = encode_verified(&cand, &segments, opts)?;
        if bytes.len() < best.0.len() {
            let rdo = if rdo { " + rate-weighted zeroing" } else { "" };
            best = (
                bytes,
                format!(
                    "jpeg: {} -> {layout}, optimized Huffman tables{}; lossy: perceptual requantization (strength {strength:.2}){rdo}, ssimulacra2={s:.2} (target {target})",
                    jpeg.frame.describe(),
                    if meta_note.is_empty() { String::new() } else { format!(", {meta_note}") },
                ),
            );
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Effort, MetadataPolicy};
    use std::path::PathBuf;

    fn fixtures() -> Vec<(String, Vec<u8>)> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jpeg");
        let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                )
            })
            .collect();
        v.sort();
        assert!(
            v.len() >= 15,
            "JPEG fixtures missing; run scripts/gen_jpeg_fixtures.py"
        );
        v
    }

    fn markers(data: &[u8]) -> Vec<(u8, Vec<u8>)> {
        decode(data).unwrap().segments
    }

    #[test]
    fn coefficients_survive_every_setting() {
        for (name, data) in fixtures() {
            let original = decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
            for effort in [Effort::Fast, Effort::Default, Effort::Max] {
                for metadata in [
                    MetadataPolicy::KeepAll,
                    MetadataPolicy::Safe,
                    MetadataPolicy::ColorOnly,
                ] {
                    let opts = Options::default()
                        .with_effort(effort)
                        .with_metadata(metadata)
                        .with_verify(false);
                    let (out, desc) =
                        optimize(&data, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
                    let back =
                        decode(&out).unwrap_or_else(|e| panic!("{name} output ({desc}): {e}"));
                    assert!(
                        original.same_image(&back),
                        "{name} {effort:?} {metadata:?}: {desc}"
                    );
                }
            }
        }
    }

    #[test]
    fn metadata_policy() {
        let data = fixtures()
            .into_iter()
            .find(|f| f.0 == "metadata_all.jpg")
            .unwrap()
            .1;
        let before = markers(&data);
        let has = |segs: &[(u8, Vec<u8>)], m: u8, prefix: &[u8]| {
            segs.iter().any(|(mm, p)| *mm == m && p.starts_with(prefix))
        };
        assert!(has(&before, 0xE1, b"http://ns.adobe.com/xap/"));
        assert!(has(&before, 0xFE, b"hello"));

        let safe = markers(&optimize(&data, &Options::default()).unwrap().0);
        assert!(has(&safe, 0xE0, b"JFIF"));
        assert!(has(&safe, 0xE2, b"ICC_PROFILE"));
        let exif = safe
            .iter()
            .find(|(m, p)| *m == 0xE1 && p.starts_with(b"Exif"))
            .expect("orientation kept");
        assert_eq!(metadata::exif_orientation(&exif.1), Some(6));
        assert!(exif.1.len() < 40, "EXIF reduced to orientation only");
        assert!(!has(&safe, 0xE1, b"http://ns.adobe.com/xap/"));
        assert!(!has(&safe, 0xFE, b""));

        let all = markers(
            &optimize(
                &data,
                &Options::default().with_metadata(MetadataPolicy::KeepAll),
            )
            .unwrap()
            .0,
        );
        assert_eq!(all, before);
    }

    #[test]
    fn cmyk_keeps_adobe_segment() {
        let data = fixtures()
            .into_iter()
            .find(|f| f.0 == "cmyk_baseline.jpg")
            .unwrap()
            .1;
        let out = optimize(
            &data,
            &Options::default().with_metadata(MetadataPolicy::ColorOnly),
        )
        .unwrap()
        .0;
        assert!(markers(&out)
            .iter()
            .any(|(m, p)| *m == 0xEE && p.starts_with(b"Adobe")));
    }

    #[test]
    fn lossy_meets_target_and_never_grows() {
        use crate::options::Mode;
        for (name, data) in fixtures() {
            let lossless = optimize(&data, &Options::default()).unwrap().0;
            for quality in [90.0f32, 80.0] {
                let opts = Options::default()
                    .with_mode(Mode::VisuallyLossless)
                    .with_quality(quality);
                let (out, desc) = optimize(&data, &opts).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(out.len() <= lossless.len(), "{name}: {desc}");
                let original = decode(&data).unwrap();
                if !matches!(original.frame.comps.len(), 1 | 3)
                    || original.frame.width < 8
                    || original.frame.height < 8
                {
                    continue;
                }
                let s = crate::metrics::ssimulacra2(
                    &render::render(&original).unwrap(),
                    &render::render(&decode(&out).unwrap()).unwrap(),
                )
                .unwrap();
                assert!(s >= quality as f64, "{name} q{quality}: {s} ({desc})");
            }
        }
    }

    #[test]
    fn rejects_corruption_without_panicking() {
        let data = fixtures()
            .into_iter()
            .find(|f| f.0 == "progressive_420_q80.jpg")
            .unwrap()
            .1;
        for i in (2..data.len()).step_by(11) {
            let mut bad = data.clone();
            bad[i] ^= 0x5A;
            let _ = optimize(&bad, &Options::default());
        }
        for cut in (0..data.len()).step_by(17) {
            let _ = optimize(&data[..cut], &Options::default());
        }
    }
}
