/// What the optimizer is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every pixel decodes to exactly the same value.
    #[default]
    Lossless,
    /// Pixels may change, as long as the SSIMULACRA2 score against the
    /// original stays at or above [`Options::quality`]. PNG only for now.
    VisuallyLossless,
}

/// Speed versus size trade-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    Fast,
    #[default]
    Default,
    Max,
}

/// Which metadata to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataPolicy {
    /// Keep everything that is still valid after re-encoding.
    KeepAll,
    /// Keep data that affects how the image looks (color profile, gamma,
    /// orientation) plus physical size. Drop text, timestamps and unknown chunks.
    #[default]
    Safe,
    /// Keep only color-management data.
    ColorOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub mode: Mode,
    pub effort: Effort,
    pub metadata: MetadataPolicy,
    /// Decode the result and compare it with the input before returning it.
    pub verify: bool,
    /// Zero the color of fully transparent pixels. Invisible on screen, but it
    /// changes stored pixel values, so it is off by default.
    pub clean_transparent: bool,
    /// Minimum SSIMULACRA2 score for [`Mode::VisuallyLossless`].
    /// 90: visually lossless, 85: indistinguishable in a flip test at 1:1,
    /// 80: indistinguishable side by side, 70: high quality.
    pub quality: f32,
}

/// Default SSIMULACRA2 target for [`Mode::VisuallyLossless`].
pub const DEFAULT_QUALITY: f32 = 85.0;

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Lossless,
            effort: Effort::Default,
            metadata: MetadataPolicy::Safe,
            verify: true,
            clean_transparent: false,
            quality: DEFAULT_QUALITY,
        }
    }
}

impl Options {
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the SSIMULACRA2 target for [`Mode::VisuallyLossless`].
    pub fn with_quality(mut self, quality: f32) -> Self {
        self.quality = quality;
        self
    }

    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    pub fn with_metadata(mut self, metadata: MetadataPolicy) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }

    pub fn with_clean_transparent(mut self, clean: bool) -> Self {
        self.clean_transparent = clean;
        self
    }
}
