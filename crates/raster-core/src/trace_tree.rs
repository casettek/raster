//! Trace tree types and hashing for bridgetree.
//!
//! This module provides a single source of truth for the trace commitment tree:
//! the `Bytes` type, the empty leaf constant, and the `Hashable` implementation
//! used by both the prover and the RISC0 guest.

use alloc::vec::Vec;
use bridgetree::{Hashable, Level};
use core::cmp::Ordering;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Size of a SHA256 hash in bytes.
const HASH_SIZE: usize = 32;

/// Empty leaf hash (precomputed SHA256 of "empty").
///
/// This is the canonical empty leaf for the trace tree. Prover and guest both
/// use this constant so verification stays aligned.
pub const EMPTY_TRACE_LEAF: [u8; 32] = [
    0x6d, 0x97, 0xa6, 0xc0, 0x26, 0x76, 0xa4, 0x1a, 0x96, 0x36, 0xc6, 0xcd, 0x4e, 0x5d, 0x2d, 0x47,
    0xd1, 0x4d, 0x27, 0xa3, 0x5d, 0x18, 0xe6, 0x08, 0x11, 0x5f, 0xd9, 0x3c, 0xd4, 0x2e, 0x6b, 0x3a,
];

/// Wrapper for byte vectors that implements [`Hashable`] for bridgetree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Bytes {}

impl PartialOrd for Bytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hashable for Bytes {
    fn empty_leaf() -> Self {
        Bytes(EMPTY_TRACE_LEAF.to_vec())
    }

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);
        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);

        let mut hasher = Sha256::new();
        hasher.update(&data);

        Bytes(hasher.finalize().to_vec())
    }
}
