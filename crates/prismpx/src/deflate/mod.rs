//! Dependency-free DEFLATE (RFC 1951) and zlib (RFC 1950) codec.
//!
//! The encoder is tuned for size rather than speed: hash-chain LZ77 with lazy
//! matching, per-block choice between stored/fixed/dynamic Huffman, and
//! length-limited Huffman codes built with the package-merge algorithm.

mod encode;
pub(crate) mod huffman;
mod inflate;
mod lz77;
mod tables;

pub use encode::{deflate, zlib_compress, Level};
pub use inflate::{inflate, zlib_decompress};
