//! Trace commitment utilities.
//!
//! This module provides types and functions for creating cryptographic
//! commitments to execution traces using incremental Merkle trees.

use bridgetree::{Hashable, Level, NonEmptyFrontier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fmt::Debug;

use crate::error::{BitPackerError, Result};
use crate::precomputed::{EMPTY_TRIE_NODES, HASH_SIZE};
use raster_core::trace::TraceItem;

/// Trait for types that can be hashed to bytes.
pub trait BytesHashable {
    /// Compute the SHA256 hash of this item.
    fn hash(&self) -> Vec<u8>;

    /// Try to compute the hash, returning an error on failure.
    fn try_hash(&self) -> Result<Vec<u8>> {
        Ok(self.hash())
    }
}

impl BytesHashable for TraceItem {
    fn hash(&self) -> Vec<u8> {
        let data = bincode::serialize(self).expect("Failed to serialize for hashing");
        let mut hasher = Sha256::new();
        hasher.update(&data);
        hasher.finalize().to_vec()
    }

    fn try_hash(&self) -> Result<Vec<u8>> {
        let data = bincode::serialize(self)
            .map_err(|e| BitPackerError::SerializationError(e.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        Ok(hasher.finalize().to_vec())
    }
}

/// Wrapper for byte vectors that implements Hashable for bridgetree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

/// A serializable representation of a NonEmptyFrontier<Bytes>.
///
/// This can be used to persist and restore frontier state for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableFrontier {
    /// The position in the tree (as u64)
    pub position: u64,
    /// The current leaf value
    pub leaf: Vec<u8>,
    /// The ommer hashes (path to root)
    pub ommers: Vec<Vec<u8>>,
}

impl SerializableFrontier {
    /// Convert a NonEmptyFrontier<Bytes> into a serializable form.
    pub fn from_frontier(frontier: &NonEmptyFrontier<Bytes>) -> Self {
        Self {
            position: frontier.position().into(),
            leaf: frontier.leaf().0.clone(),
            ommers: frontier.ommers().iter().map(|o| o.0.clone()).collect(),
        }
    }

    /// Reconstruct a NonEmptyFrontier<Bytes> from the serialized form.
    ///
    /// Returns None if the frontier cannot be reconstructed (e.g., invalid position).
    pub fn to_frontier(&self) -> Option<NonEmptyFrontier<Bytes>> {
        use bridgetree::Position;
        NonEmptyFrontier::from_parts(
            Position::from(self.position),
            Bytes(self.leaf.clone()),
            self.ommers.iter().map(|o| Bytes(o.clone())).collect(),
        )
        .ok()
    }

    /// Serialize to bytes using bincode.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Deserialize from bytes using bincode.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bincode::deserialize(bytes).ok()
    }
}

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
        Bytes(EMPTY_TRIE_NODES[0].to_vec())
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

/// Bridge tree for trace commitments with 32 levels.
pub type TraceBridgeTree = bridgetree::BridgeTree<Bytes, u64, 32>;

/// Commitment to an execution trace using incremental Merkle roots.
pub struct ExecutionCommitment(pub Vec<Vec<u8>>);

impl ExecutionCommitment {
    /// Create a commitment from a trace.
    ///
    /// Returns a vector of Merkle roots, one for each item in the trace.
    /// Each root represents the commitment up to and including that item.
    pub fn from(items: &[TraceItem], seed: &[u8]) -> ExecutionCommitment {
        let items_hashes: Vec<Vec<u8>> = items.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut roots: Vec<Vec<u8>> = Vec::with_capacity(items.len());

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
            if let Some(root) = trace_tree.root(0) {
                roots.push(root.0);
            }
        }

        ExecutionCommitment(roots)
    }

    /// Try to create a commitment, returning an error on failure.
    pub fn try_from(items: &[TraceItem], seed: &[u8]) -> Result<ExecutionCommitment> {
        if items.is_empty() {
            return Err(BitPackerError::EmptyTrace);
        }

        let mut items_hashes: Vec<Vec<u8>> = Vec::with_capacity(items.len());
        for item in items {
            items_hashes.push(item.try_hash()?);
        }

        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut roots: Vec<Vec<u8>> = Vec::with_capacity(items.len());

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
            if let Some(root) = trace_tree.root(0) {
                roots.push(root.0);
            }
        }

        Ok(ExecutionCommitment(roots))
    }

    /// Get the frontier (partial Merkle path) at position n.
    ///
    /// This can be used to continue building the tree from position n.
    pub fn frontier(
        items: &[TraceItem],
        n: usize,
        seed: &[u8],
    ) -> Option<NonEmptyFrontier<Bytes>> {
        let items_hashes: Vec<Vec<u8>> = items.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in items_hashes.iter().take(n) {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree.frontier().cloned()
    }

    /// Try to get the frontier, returning an error on failure.
    pub fn try_frontier(
        items: &[TraceItem],
        n: usize,
        seed: &[u8],
    ) -> Result<NonEmptyFrontier<Bytes>> {
        if n > items.len() {
            return Err(BitPackerError::InvalidRange {
                start: 0,
                end: n,
                max: items.len(),
            });
        }

        let mut items_hashes: Vec<Vec<u8>> = Vec::with_capacity(n);
        for item in items.iter().take(n) {
            items_hashes.push(item.try_hash()?);
        }

        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree
            .frontier()
            .cloned()
            .ok_or_else(|| BitPackerError::InvalidWindow("Failed to get frontier".to_string()))
    }

    /// Get the number of commitments.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if the commitment is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Builder for incrementally constructing an ExecutionCommitment with streaming output.
///
/// Similar to `TraceCommitmentSink`, but writes each new root to the
/// provided writer as raw bytes (32 bytes per root) immediately after appending.
///
/// # Example
///
/// ```ignore
/// let file = File::create("roots.bin")?;
/// let mut builder = TraceCommitmentSink::new(seed, file);
/// builder.append(&item1)?; // writes root to file
/// builder.append(&item2)?; // writes root to file
/// ```
pub struct TraceCommitmentProducer {
    frontier: NonEmptyFrontier<Bytes>,
    trace_items_commitments: Vec<Vec<u8>>,
}

impl TraceCommitmentProducer {
    pub fn new(seed: &[u8]) -> Self {
        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));
        let frontier = trace_tree.frontier().cloned().unwrap();
        Self { frontier, trace_items_commitments: Vec::new() }
    }

    pub fn append(&mut self, item: &TraceItem) -> Result<()> {
        self.frontier.append(Bytes(item.hash()));
        let mut trace_tree = TraceBridgeTree::from_frontier(1, self.frontier.clone());
        let item_hash = item.hash();
        trace_tree.append(Bytes(item_hash));

        let Some(root) = trace_tree.root(0) else {
            return Err(BitPackerError::TreeRootError("Failed to get root".to_string()));
        };

        self.frontier = trace_tree.frontier().cloned().unwrap();
        self.trace_items_commitments.push(root.0);

        Ok(())
    }

    pub fn try_append(&mut self, item: &TraceItem) -> Result<()> {
        let item_hash = item.try_hash()?;
        self.frontier.append(Bytes(item_hash));
        let mut trace_tree = TraceBridgeTree::from_frontier(1, self.frontier.clone());
        let item_hash = item.hash();
        trace_tree.append(Bytes(item_hash));

        let Some(root) = trace_tree.root(0) else {
            return Err(BitPackerError::TreeRootError("Failed to get root".to_string()));
        };

        self.frontier = trace_tree.frontier().cloned().unwrap();
        self.trace_items_commitments.push(root.0);

        Ok(())
    }

    /// Get a clone of the current frontier state.
    ///
    /// This can be used to snapshot the frontier before appending items,
    /// allowing replay from this point.
    pub fn frontier(&self) -> NonEmptyFrontier<Bytes> {
        self.frontier.clone()
    }

    pub fn finish(self) -> Vec<Vec<u8>> {
        self.trace_items_commitments
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precomputed;

    /// Helper function to create a TileTraceItem for testing.
    fn make_tile_trace_item(input: u64, output: u64) -> TraceItem {
        TraceItem {
            fn_name: format!("test_tile_{}", input),
            desc: None,
            inputs: vec![],
            input_data: format!("{}", input),
            output_type: Some("u64".to_string()),
            output_data: format!("{}", output),
        }
    }

    #[test]
    fn trace_should_be_not_equal() {
        let items: Vec<TraceItem> = vec![
            make_tile_trace_item(0, 0),
            make_tile_trace_item(1, 1),
            make_tile_trace_item(2, 2),
            make_tile_trace_item(3, 3),
            make_tile_trace_item(4, 4),
        ];

        let ref_items: Vec<TraceItem> = vec![
            make_tile_trace_item(0, 0),
            make_tile_trace_item(1, 1),
            make_tile_trace_item(5, 5), // Different
            make_tile_trace_item(3, 3),
            make_tile_trace_item(4, 4),
        ];

        let binded_trace = ExecutionCommitment::from(&items, &precomputed::EMPTY_TRIE_NODES[0]);
        let ref_binded_trace =
            ExecutionCommitment::from(&ref_items, &precomputed::EMPTY_TRIE_NODES[0]);

        // Check that different items produce different commitments
        let trace_comp_iter = items.iter().zip(ref_items.iter());
        let binded_trace_comp_iter = binded_trace.0.iter().zip(ref_binded_trace.0.iter());

        for ((item, ref_item), (binded_trace_item, ref_binded_trace_item)) in
            trace_comp_iter.zip(binded_trace_comp_iter)
        {
            if item.input_data != ref_item.input_data || item.output_data != ref_item.output_data {
                assert_ne!(binded_trace_item, ref_binded_trace_item);
            }
        }
    }

    #[test]
    fn test_trace_item_hash() {
        let item = make_tile_trace_item(1, 2);
        let hash = item.hash();
        assert_eq!(hash.len(), 32); // SHA256 produces 32 bytes
    }

    #[test]
    fn test_try_from_empty_trace() {
        let items: Vec<TraceItem> = vec![];
        let result = ExecutionCommitment::try_from(&items, &precomputed::EMPTY_TRIE_NODES[0]);
        assert!(matches!(result, Err(BitPackerError::EmptyTrace)));
    }
}
