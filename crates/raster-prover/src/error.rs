//! Error types for the raster-prover crate.

use std::fmt;

#[derive(Debug, Clone)]
pub enum BitPackerError {
    /// BitPacker index is out of bounds.
    IndexOutOfBounds { index: usize, max: usize },
    /// Invalid range for BitPacker operations.
    InvalidRange { start: usize, end: usize, max: usize },
    /// Arrays have different lengths in comparison.
    LengthMismatch { expected: usize, actual: usize },
    /// Failed to serialize data.
    SerializationError(String),
    /// Failed to compute hash.
    HashError(String),
    /// Invalid window parameters.
    InvalidWindow(String),
    /// Trace is empty.
    EmptyTrace,
    /// IO error.
    IoError(String),
    /// Failed to get root.
    TreeRootError(String),
}

impl fmt::Display for BitPackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitPackerError::IndexOutOfBounds { index, max } => {
                write!(f, "Index {} out of bounds (max: {})", index, max)
            }
            BitPackerError::InvalidRange { start, end, max } => {
                write!(f, "Invalid range [{}, {}) for max {}", start, end, max)
            }
            BitPackerError::LengthMismatch { expected, actual } => {
                write!(f, "Length mismatch: expected {}, got {}", expected, actual)
            }
            BitPackerError::SerializationError(msg) => {
                write!(f, "Serialization error: {}", msg)
            }
            BitPackerError::HashError(msg) => {
                write!(f, "Hash error: {}", msg)
            }
            BitPackerError::InvalidWindow(msg) => {
                write!(f, "Invalid window: {}", msg)
            }
            BitPackerError::EmptyTrace => {
                write!(f, "Trace is empty")
            }
            BitPackerError::IoError(msg) => {
                write!(f, "IO error: {}", msg)
            }
            BitPackerError::TreeRootError(msg) => {
                write!(f, "Failed to get root {}", msg)
            }
        }
    }
}

impl std::error::Error for BitPackerError {}

impl From<std::io::Error> for BitPackerError {
    fn from(err: std::io::Error) -> Self {
        BitPackerError::IoError(err.to_string())
    }
}

/// Result type for bitpacker operations.
pub type Result<T> = std::result::Result<T, BitPackerError>;
