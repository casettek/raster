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
}

impl fmt::Display for BitPackerError  {
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
        }
    }
}

impl std::error::Error for BitPackerError {}

/// Result type for bphc operations.
pub type Result<T> = std::result::Result<T, BitPackerError>;
