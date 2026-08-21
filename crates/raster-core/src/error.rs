//! Internal runtime/protocol errors for the Raster toolchain.
//!
//! This module provides `no_std` compatible error types.

use alloc::string::String;
use core::fmt;

/// Internal error type for Raster runtime/protocol operations.
#[derive(Debug)]
pub enum Error {
    /// Invalid tile ID.
    InvalidTileId(String),

    /// Invalid sequence.
    InvalidSequence(String),

    /// Serialization/deserialization error.
    Serialization(String),

    /// IO error (only available with std).
    #[cfg(feature = "std")]
    Io(std::io::Error),

    /// Generic error with a message.
    Other(String),

    /// `Bytes::paged` was called with a zero page size.
    PageSizeZero,

    /// An artifact's committed `page_size` does not match the type's `Bytes<N>`.
    PageSizeMismatch { declared: u64, artifact: u64 },

    /// A page's `offset`/`len` do not form a partition of the region.
    PageShape { index: u64, offset: u64, len: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidTileId(id) => write!(f, "Invalid tile ID: {}", id),
            Error::InvalidSequence(msg) => write!(f, "Invalid sequence: {}", msg),
            Error::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            #[cfg(feature = "std")]
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Other(msg) => write!(f, "{}", msg),
            Error::PageSizeZero => write!(f, "Bytes page size must be greater than zero"),
            Error::PageSizeMismatch { declared, artifact } => write!(
                f,
                "artifact page size {artifact} does not match declared Bytes<{declared}>"
            ),
            Error::PageShape { index, offset, len } => write!(
                f,
                "bytes page {index} has offset {offset} and len {len}, which is not a valid partition"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(feature = "std")]
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serialization(alloc::format!("{}", e))
    }
}

/// Result type for Raster runtime/protocol operations.
pub type Result<T> = core::result::Result<T, Error>;
