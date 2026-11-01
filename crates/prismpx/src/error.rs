use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The input is not a format prismpx recognizes.
    UnsupportedFormat,
    /// The format is recognized but this input or option is not supported yet.
    Unsupported(String),
    /// The input is corrupt or violates the format specification.
    Malformed(String),
    /// The optimized output did not decode to the same pixels. This is a bug;
    /// the original should be kept.
    VerificationFailed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnsupportedFormat => write!(f, "unsupported or unrecognized image format"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Malformed(m) => write!(f, "malformed input: {m}"),
            Error::VerificationFailed(m) => write!(f, "verification failed: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) fn malformed(msg: impl Into<String>) -> Error {
    Error::Malformed(msg.into())
}
