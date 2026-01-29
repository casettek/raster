//! Trace commitment utilities.
//!
//! This module provides types and functions for creating cryptographic
//! commitments to execution traces using incremental Merkle trees.

use bridgetree::{Hashable, Level, NonEmptyFrontier};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fmt::Debug;

use crate::error::{BitPackerError, Result};
use crate::precomputed::{EMPTY_TRIE_NODES, HASH_SIZE};

/// A trace item containing input and output states.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct RawTraceItem<T> {
    /// Input state for this step.
    pub input: T,
    /// Output state after execution.
    pub output: T,
}

impl<T> RawTraceItem<T> {
    /// Create a new trace item with the given input and output.
    pub fn new(input: T, output: T) -> Self {
        Self { input, output }
    }
}

/// Trait for types that can be hashed to bytes.
pub trait BytesHashable {
    /// Compute the SHA256 hash of this item.
    fn hash(&self) -> Vec<u8>;

    /// Try to compute the hash, returning an error on failure.
    fn try_hash(&self) -> Result<Vec<u8>> {
        Ok(self.hash())
    }
}

impl<T> BytesHashable for RawTraceItem<T>
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de>,
{
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
#[derive(Debug, Clone)]
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
    pub fn from<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Clone + Debug>(
        items: &[RawTraceItem<T>],
        seed: &[u8],
    ) -> ExecutionCommitment {
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
    pub fn try_from<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Clone + Debug>(
        items: &[RawTraceItem<T>],
        seed: &[u8],
    ) -> Result<ExecutionCommitment> {
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
    pub fn frontier<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Clone + Debug>(
        items: &[RawTraceItem<T>],
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
    pub fn try_frontier<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Clone + Debug>(
        items: &[RawTraceItem<T>],
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

/// Builder for incrementally constructing an ExecutionCommitment.
///
/// This allows appending trace items one at a time rather than
/// providing all items upfront, which is useful for streaming scenarios.
///
/// # Example
///
/// ```ignore
/// let mut builder = ExecutionCommitmentBuilder::new(seed);
/// builder.append(&item1);
/// builder.append(&item2);
/// // Can check intermediate state
/// let current = builder.current_root();
/// // Finalize
/// let commitment = builder.build();
/// ```
pub struct ExecutionCommitmentBuilder {
    trace_tree: TraceBridgeTree,
    roots: Vec<Vec<u8>>,
}

impl ExecutionCommitmentBuilder {
    /// Initialize a new builder with the given seed.
    ///
    /// The seed is used as the initial leaf in the Merkle tree.
    pub fn new(seed: &[u8]) -> Self {
        let mut trace_tree = TraceBridgeTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));
        Self {
            trace_tree,
            roots: Vec::new(),
        }
    }

    /// Append a single trace item to the commitment.
    ///
    /// Computes the hash of the item, adds it to the tree, and stores the new root.
    pub fn append<T: serde::Serialize + for<'de> serde::Deserialize<'de>>(
        &mut self,
        item: &RawTraceItem<T>,
    ) {
        let item_hash = item.hash();
        self.trace_tree.append(Bytes(item_hash));
        if let Some(root) = self.trace_tree.root(0) {
            self.roots.push(root.0);
        }
    }

    /// Try to append a single trace item, returning an error on failure.
    ///
    /// This is the fallible version of [`append`](Self::append) that propagates
    /// serialization errors instead of panicking.
    pub fn try_append<T: serde::Serialize + for<'de> serde::Deserialize<'de>>(
        &mut self,
        item: &RawTraceItem<T>,
    ) -> Result<()> {
        let item_hash = item.try_hash()?;
        self.trace_tree.append(Bytes(item_hash));
        if let Some(root) = self.trace_tree.root(0) {
            self.roots.push(root.0);
        }
        Ok(())
    }

    /// Get the current Merkle root.
    ///
    /// Returns `None` if no items have been appended yet.
    /// This is useful for checking intermediate state during streaming.
    pub fn current_root(&self) -> Option<&[u8]> {
        self.roots.last().map(|v| v.as_slice())
    }

    /// Get the number of items that have been appended.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Check if no items have been appended yet.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Consume the builder and return the ExecutionCommitment.
    pub fn build(self) -> ExecutionCommitment {
        ExecutionCommitment(self.roots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precomputed;

    #[test]
    fn trace_should_be_not_equal() {
        let items: Vec<RawTraceItem<u64>> = vec![
            RawTraceItem::new(0, 0),
            RawTraceItem::new(1, 1),
            RawTraceItem::new(2, 2),
            RawTraceItem::new(3, 3),
            RawTraceItem::new(4, 4),
        ];

        let ref_items: Vec<RawTraceItem<u64>> = vec![
            RawTraceItem::new(0, 0),
            RawTraceItem::new(1, 1),
            RawTraceItem::new(5, 5), // Different
            RawTraceItem::new(3, 3),
            RawTraceItem::new(4, 4),
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
            if item != ref_item {
                assert_ne!(binded_trace_item, ref_binded_trace_item);
            }
        }
    }

    #[test]
    fn test_trace_item_hash() {
        let item: RawTraceItem<u64> = RawTraceItem::new(1, 2);
        let hash = item.hash();
        assert_eq!(hash.len(), 32); // SHA256 produces 32 bytes
    }

    #[test]
    fn test_try_from_empty_trace() {
        let items: Vec<RawTraceItem<u64>> = vec![];
        let result = ExecutionCommitment::try_from(&items, &precomputed::EMPTY_TRIE_NODES[0]);
        assert!(matches!(result, Err(BitPackerError::EmptyTrace)));
    }

    #[test]
    fn test_builder_matches_batch_from() {
        let items: Vec<RawTraceItem<u64>> = vec![
            RawTraceItem::new(0, 0),
            RawTraceItem::new(1, 1),
            RawTraceItem::new(2, 2),
            RawTraceItem::new(3, 3),
            RawTraceItem::new(4, 4),
        ];

        let seed = &precomputed::EMPTY_TRIE_NODES[0];

        // Build using batch method
        let batch_commitment = ExecutionCommitment::from(&items, seed);

        // Build using incremental builder
        let mut builder = ExecutionCommitmentBuilder::new(seed);
        for item in &items {
            builder.append(item);
        }
        let builder_commitment = builder.build();

        // They should produce identical results
        assert_eq!(batch_commitment.0, builder_commitment.0);
    }

    #[test]
    fn test_builder_current_root() {
        let seed = &precomputed::EMPTY_TRIE_NODES[0];
        let mut builder = ExecutionCommitmentBuilder::new(seed);

        // Initially no root
        assert!(builder.current_root().is_none());
        assert_eq!(builder.len(), 0);
        assert!(builder.is_empty());

        // After appending one item
        builder.append(&RawTraceItem::new(1u64, 2u64));
        assert!(builder.current_root().is_some());
        assert_eq!(builder.len(), 1);
        assert!(!builder.is_empty());

        // After appending another item
        builder.append(&RawTraceItem::new(3u64, 4u64));
        assert_eq!(builder.len(), 2);
    }

    #[test]
    fn test_builder_try_append() {
        let seed = &precomputed::EMPTY_TRIE_NODES[0];
        let mut builder = ExecutionCommitmentBuilder::new(seed);

        let item = RawTraceItem::new(1u64, 2u64);
        let result = builder.try_append(&item);
        assert!(result.is_ok());
        assert_eq!(builder.len(), 1);
    }
}
