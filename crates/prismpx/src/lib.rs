//! # prismpx
//!
//! Same-format image optimization. A PNG goes in, a smaller PNG comes out, and
//! in [`Mode::Lossless`] every pixel decodes to exactly the same value.
//!
//! ```no_run
//! let input = std::fs::read("in.png").unwrap();
//! let result = prismpx::optimize(&input, &prismpx::Options::default()).unwrap();
//! println!("{} -> {} bytes ({})", result.original_size, result.size(), result.strategy);
//! std::fs::write("out.png", &result.data).unwrap();
//! ```
//!
//! [`Mode::VisuallyLossless`] trades invisible changes for much smaller files,
//! guided by the SSIMULACRA2 perceptual metric (see [`metrics`]).
//!
//! Status: PNG and JPEG are implemented in both modes.

mod checksum;
mod color;
pub mod deflate;
mod error;
pub mod jpeg;
pub mod metrics;
mod options;
mod parallel;
pub mod png;

pub use error::{Error, Result};
pub use options::{Effort, MetadataPolicy, Mode, Options, DEFAULT_QUALITY};

/// Image container formats prismpx recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
        }
    }
}

/// Detects the format from the file's magic bytes.
pub fn detect_format(data: &[u8]) -> Option<Format> {
    if data.starts_with(&png::SIGNATURE) {
        Some(Format::Png)
    } else if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        Some(Format::Jpeg)
    } else {
        None
    }
}

/// The outcome of [`optimize`].
#[derive(Debug, Clone)]
pub struct Optimized {
    /// The output bytes. When `optimized` is false this is a copy of the input.
    pub data: Vec<u8>,
    pub format: Format,
    pub original_size: usize,
    /// False when no smaller encoding was found and the original was kept.
    pub optimized: bool,
    /// Human-readable description of what was done, for logs and debugging.
    pub strategy: String,
}

impl Optimized {
    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn saved_bytes(&self) -> usize {
        self.original_size.saturating_sub(self.data.len())
    }
}

/// Optimizes an encoded image, keeping its format.
///
/// The result is never larger than the input: if nothing smaller is found the
/// original bytes are returned with `optimized == false`.
pub fn optimize(input: &[u8], opts: &Options) -> Result<Optimized> {
    let format = detect_format(input).ok_or(Error::UnsupportedFormat)?;
    let (data, strategy) = match format {
        Format::Png => png::optimize(input, opts)?,
        Format::Jpeg => jpeg::optimize(input, opts)?,
    };
    if data.len() < input.len() {
        Ok(Optimized {
            data,
            format,
            original_size: input.len(),
            optimized: true,
            strategy,
        })
    } else {
        Ok(Optimized {
            data: input.to_vec(),
            format,
            original_size: input.len(),
            optimized: false,
            strategy: format!("{strategy}; kept original (no gain)"),
        })
    }
}
