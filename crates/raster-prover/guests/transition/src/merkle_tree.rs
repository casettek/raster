//! Trace/bridgetree plumbing: leaf hashing, frontier (de)serialization, roots.

use std::cmp::Ordering;

use bridgetree::{Hashable, Level, NonEmptyFrontier, Position};
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};

use raster_core::trace::StepRecord;
use raster_core::transition::SerializableFrontier;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

pub type TraceBridgeTree = bridgetree::BridgeTree<Bytes, u64, 32>;

/// Shared with the host prover, so both fold to the same depth and use the
/// same empty roots.
pub use raster_core::merkle::{EMPTY_LEAF, EMPTY_ROOTS, TRACE_TREE_DEPTH};
use raster_core::merkle::{to_node_hash, HASH_SIZE};

// ============================================================================
// Bytes + Hashable for bridgetree (matches prover's empty leaf and combine)
// ============================================================================

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
        Bytes(EMPTY_LEAF.to_vec())
    }

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);
        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);
        Bytes(sha256_bytes(&data))
    }

    /// Empty-subtree root, read from [`EMPTY_ROOTS`].
    ///
    /// The trait's default folds from level 0 on every call, and
    /// `NonEmptyFrontier::root` calls it once per level — 0+1+..+31 = 496
    /// hashes of rederived constants per fold, against ~32 of actual spine.
    /// A runtime memo still pays its own 0+1+..+32 = 528 hashes (~0.4M cycles)
    /// on every step, since each step is a fresh guest execution; the table
    /// is compiled in and costs nothing. `empty_roots_match_fold` pins it to
    /// this `combine`.
    fn empty_root(level: Level) -> Self {
        let idx = usize::from(u8::from(level));
        match EMPTY_ROOTS.get(idx) {
            Some(node) => Bytes(node.to_vec()),
            // Above the table: fold, so a deeper tree stays correct.
            None => empty_root_fold(level),
        }
    }
}

/// The `Hashable` default: fold empty leaves up to `level`. O(level) hashes.
fn empty_root_fold(level: Level) -> Bytes {
    Level::from(0)
        .iter_to(level)
        .fold(Bytes::empty_leaf(), |v, lvl| Bytes::combine(lvl, &v, &v))
}

#[cfg(test)]
mod empty_roots_tests {
    use super::*;

    #[test]
    fn empty_roots_match_fold() {
        assert_eq!(EMPTY_ROOTS[0], EMPTY_LEAF);
        for (level, root) in EMPTY_ROOTS.iter().enumerate() {
            assert_eq!(
                empty_root_fold(Level::from(level as u8)).0,
                root.to_vec(),
                "EMPTY_ROOTS[{level}]"
            );
        }
    }
}

// ============================================================================
// SerializableFrontier <-> NonEmptyFrontier<Bytes> (guest-local conversion)
// ============================================================================

pub fn deserialize_frontier(ser: &SerializableFrontier) -> Option<NonEmptyFrontier<Bytes>> {
    NonEmptyFrontier::from_parts(
        Position::from(ser.position),
        Bytes(ser.leaf.clone()),
        ser.ommers.iter().map(|o| Bytes(o.to_vec())).collect(),
    )
    .ok()
}

pub fn serialize_frontier(frontier: &NonEmptyFrontier<Bytes>) -> SerializableFrontier {
    SerializableFrontier {
        position: frontier.position().into(),
        leaf: frontier.leaf().0.clone(),
        ommers: frontier.ommers().iter().map(|o| to_node_hash(&o.0)).collect(),
    }
}

// ============================================================================
// Hashing
// ============================================================================

/// Hash a TileExecRecord using SHA256 of its postcard-serialized form.
pub fn hash_trace_item(item: &StepRecord) -> Vec<u8> {
    let data = postcard::to_allocvec(item).expect("Failed to serialize TileExecRecord");
    sha256_bytes(&data)
}

pub fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Risc0Sha256::hash_bytes(bytes).as_bytes().to_vec()
}

/// Root of a frontier, folded directly against the empty-subtree roots.
///
/// Equivalent to `TraceBridgeTree::from_frontier(1, frontier.clone()).root(0)` —
/// which is what `BridgeTree::root` itself does after rebuilding the bridge —
/// but without the clone and the tree allocation. Mirrors
/// `raster_prover::trace::frontier_root`.
pub fn frontier_root(frontier: &NonEmptyFrontier<Bytes>) -> Vec<u8> {
    frontier.root(Some(Level::from(TRACE_TREE_DEPTH))).0
}

pub fn sha256_hex(bytes: &[u8]) -> Vec<u8> {
    let digest = sha256_bytes(bytes);
    let mut out = Vec::with_capacity(digest.len() * 2);
    for byte in digest {
        let hi = (byte >> 4) & 0x0f;
        let lo = byte & 0x0f;
        out.push(if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) });
        out.push(if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) });
    }
    out
}

pub fn combine_merkle_level(level: usize, left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + left.len() + right.len());
    data.push(level as u8);
    data.extend_from_slice(left);
    data.extend_from_slice(right);
    sha256_bytes(&data)
}
