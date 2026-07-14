//! Validation of recur iteration chunk shapes against the CFS-declared
//! chunk size (`RecurTileItem::chunk`).
//!
//! For a chunked recur tile the first tile argument is `RecurInput<Vec<T>>`,
//! and `RecurInput`'s first field is the chunk vector, so the iteration's ABI
//! input bytes begin with the postcard varint element count of the chunk.
//! Both the native recorder and the transition guest validate against those
//! canonical bytes (the same bytes the replay proof executes on, and that
//! `input_commitment` pins), so a lying length prefix cannot pass replay.

use core::fmt;

/// A violation of the declared chunking discipline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkViolation {
    /// The iteration input bytes are too short to carry a chunk length.
    Undecodable,
    /// An iteration consumed an empty chunk.
    Empty,
    /// An iteration consumed more elements than the declared chunk size.
    Oversized { declared: u64, actual: u64 },
    /// A short (non-full) chunk was followed by another iteration; only the
    /// final chunk may be shorter than the declared size.
    ShortNonFinal { declared: u64, actual: u64 },
}

impl fmt::Display for ChunkViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Undecodable => {
                write!(f, "recur iteration input does not carry a chunk length")
            }
            Self::Empty => write!(f, "recur iteration consumed an empty chunk"),
            Self::Oversized { declared, actual } => write!(
                f,
                "recur iteration chunk of {} elements exceeds declared chunk size {}",
                actual, declared
            ),
            Self::ShortNonFinal { declared, actual } => write!(
                f,
                "non-final recur iteration chunk of {} elements is smaller than declared chunk size {}",
                actual, declared
            ),
        }
    }
}

/// Decode the leading postcard varint from a byte slice.
///
/// Postcard encodes `u64` (and collection lengths) as LEB128 varints:
/// little-endian 7-bit groups with the high bit as a continuation flag.
pub fn leading_varint(bytes: &[u8]) -> Option<u64> {
    let mut value: u64 = 0;
    for (index, byte) in bytes.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Element count of the chunk consumed by a recur iteration, decoded from the
/// iteration's canonical ABI input bytes.
pub fn iteration_chunk_len(input_data: &[u8]) -> Option<u64> {
    leading_varint(input_data)
}

/// Stateless per-iteration rule: a chunk must hold `1..=declared` elements.
pub fn check_iteration_chunk_len(declared: u64, actual: u64) -> Result<(), ChunkViolation> {
    if actual == 0 {
        return Err(ChunkViolation::Empty);
    }
    if actual > declared {
        return Err(ChunkViolation::Oversized { declared, actual });
    }
    Ok(())
}

/// Ordering rule across iterations: every chunk except the final one must be
/// exactly `declared` elements. Call with the length of the iteration that
/// preceded the current one.
pub fn check_previous_chunk_was_full(
    declared: u64,
    previous: u64,
) -> Result<(), ChunkViolation> {
    if previous != declared {
        return Err(ChunkViolation::ShortNonFinal {
            declared,
            actual: previous,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn leading_varint_matches_postcard_collection_lengths() {
        for len in [0usize, 1, 2, 5, 127, 128, 300, 20_000] {
            let value: Vec<u8> = vec![7u8; len];
            let bytes = postcard::to_allocvec(&value).unwrap();
            assert_eq!(leading_varint(&bytes), Some(len as u64), "len {}", len);
        }
    }

    #[test]
    fn iteration_chunk_len_reads_recur_input_layout() {
        // Mirrors `RecurInput<Vec<String>> { value, index, len }`: postcard
        // encodes struct fields in order with no framing, so a tuple with the
        // same field order produces identical leading bytes.
        let recur_input = (
            vec![String::from("a"), String::from("bc")],
            3u64, // index
            5u64, // len
        );
        let bytes = postcard::to_allocvec(&recur_input).unwrap();
        assert_eq!(iteration_chunk_len(&bytes), Some(2));

        // Multi-argument ABI: the tuple still leads with the RecurInput.
        let with_extra_args = (recur_input, String::from("title"));
        let bytes = postcard::to_allocvec(&with_extra_args).unwrap();
        assert_eq!(iteration_chunk_len(&bytes), Some(2));
    }

    #[test]
    fn per_iteration_rule_accepts_full_and_partial_chunks() {
        assert_eq!(check_iteration_chunk_len(4, 4), Ok(()));
        assert_eq!(check_iteration_chunk_len(4, 1), Ok(()));
    }

    #[test]
    fn per_iteration_rule_rejects_empty_and_oversized_chunks() {
        assert_eq!(check_iteration_chunk_len(4, 0), Err(ChunkViolation::Empty));
        assert_eq!(
            check_iteration_chunk_len(4, 5),
            Err(ChunkViolation::Oversized {
                declared: 4,
                actual: 5
            })
        );
    }

    #[test]
    fn ordering_rule_rejects_short_non_final_chunks() {
        assert_eq!(check_previous_chunk_was_full(4, 4), Ok(()));
        assert_eq!(
            check_previous_chunk_was_full(4, 2),
            Err(ChunkViolation::ShortNonFinal {
                declared: 4,
                actual: 2
            })
        );
    }
}
