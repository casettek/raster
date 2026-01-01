//! Error types for the Raster toolchain.
//!
//! This module provides `no_std` compatible error types.

use alloc::string::String;
use core::fmt;

/// Error type for Raster operations.
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

/// Result type for Raster operations.
pub type Result<T> = core::result::Result<T, Error>;
