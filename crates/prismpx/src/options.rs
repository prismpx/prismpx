/// What the optimizer is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every pixel decodes to exactly the same value.
    #[default]
    Lossless,
    /// Pixels may change as long as the difference is not visible.
    /// Not implemented yet.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    pub effort: Effort,
    pub metadata: MetadataPolicy,
    /// Decode the result and compare it with the input before returning it.
    pub verify: bool,
    /// Zero the color of fully transparent pixels. Invisible on screen, but it
    /// changes stored pixel values, so it is off by default.
    pub clean_transparent: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Lossless,
            effort: Effort::Default,
            metadata: MetadataPolicy::Safe,
            verify: true,
            clean_transparent: false,
        }
    }
}

impl Options {
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
