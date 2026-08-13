//! Validation of recur iteration chunk shapes against the CFS-declared
//! chunk size (`RecurTileItem::chunk`).
//!
//! The element count an iteration consumed is read from its replay journal
//! (`RecurPosition::consumed_elements`), which the tile commits directly and
//! the replay receipt covers. This module used to infer the same number from
//! the leading postcard varint of the iteration's ABI bytes — sound, but only
//! because a chunked tile's first argument happened to be `RecurInput<Vec<T>>`
//! with the chunk vector first. That was a layout assumption about user types,
//! and it could not reach the other facts the audit needs. See
//! `docs/proposals/lazy-list-recur.md` §5.

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
pub fn check_previous_chunk_was_full(declared: u64, previous: u64) -> Result<(), ChunkViolation> {
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
