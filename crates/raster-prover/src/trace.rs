//! Trace commitment utilities.
//!
//! This module provides types and functions for creating cryptographic
//! commitments to execution traces using incremental Merkle trees.

use bridgetree::{Hashable, Level, NonEmptyFrontier};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fmt::Debug;

use crate::error::{BitPackerError, Result};
use crate::precomputed::{EMPTY_TRIE_NODES, HASH_SIZE};

use raster_core::fingerprint::BitPacker;
use raster_core::trace::{TraceItem, TraceWindow};

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
        let data = postcard::to_allocvec(self).expect("Failed to serialize for hashing");
        let mut hasher = Sha256::new();
        hasher.update(&data);
        hasher.finalize().to_vec()
    }

    fn try_hash(&self) -> Result<Vec<u8>> {
        let data = postcard::to_allocvec(self)
            .map_err(|e| BitPackerError::SerializationError(e.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        Ok(hasher.finalize().to_vec())
    }
}

/// Wrapper for byte vectors that implements Hashable for bridgetree.
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

/// A serializable representation of a NonEmptyFrontier<Bytes>.
///
/// This can be used to persist and restore frontier state for replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerializableFrontier {
    /// The position in the tree (as u64)
    pub position: u64,
    /// The current leaf value
    pub leaf: Vec<u8>,
    /// The ommer hashes (path to root)
    pub ommers: Vec<Vec<u8>>,
}

impl SerializableFrontier {
    /// Consume a NonEmptyFrontier<Bytes> into a serializable form.
    pub fn from_frontier(frontier: TraceTreeFrontier) -> Self {
        Self {
            position: frontier.position().into(),
            leaf: frontier.leaf().clone().0,
            ommers: frontier
                .ommers()
                .clone()
                .iter()
                .map(|o| o.clone().0)
                .collect(),
        }
    }

    /// Reconstruct a NonEmptyFrontier<Bytes> from the serialized form.
    ///
    /// Returns None if the frontier cannot be reconstructed (e.g., invalid position).
    pub fn into_frontier(self) -> Option<TraceTreeFrontier> {
        use bridgetree::Position;
        TraceTreeFrontier::from_parts(
            Position::from(self.position),
            Bytes(self.leaf.clone()),
            self.ommers.iter().map(|o| Bytes(o.clone())).collect(),
        )
        .ok()
    }

    /// Serialize to bytes using bincode.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    /// Deserialize from bytes using bincode.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// Bridge tree for trace commitments with 32 levels.
pub type TraceTree = bridgetree::BridgeTree<Bytes, u64, 32>;
pub type TraceTreeFrontier = NonEmptyFrontier<Bytes>;

pub const WINDOW_SIZE: u8 = 2;
pub const BITS_PER_ITEM: usize = 16;

/// Commitment to an execution trace using incremental Merkle roots.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceCommitment {
    pub fingerprint: Fingerprint,
    pub revealed_items: Vec<TraceItem>,
}

impl TraceCommitment {
    pub fn from(items: &[TraceItem], seed: &[u8]) -> TraceCommitment {
        let revealed_items = items[..(WINDOW_SIZE as usize)].to_vec();

        let items_hashes: Vec<Vec<u8>> = items.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(BITS_PER_ITEM);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut fingerprint_acc = FingerprintAccumulator::new(BitPacker(33));

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
            if let Some(root) = trace_tree.root(0) {
                fingerprint_acc.append(&root.0);
            }
        }

        let fingerprint = fingerprint_acc.into_fingerprint();

        TraceCommitment {
            fingerprint,
            revealed_items,
        }
    }

    /// Get the frontier (partial Merkle path) at position n.
    ///
    /// This can be used to continue building the tree from position n.
    pub fn frontier(items: &[TraceItem], n: usize, seed: &[u8]) -> Option<TraceTreeFrontier> {
        let items_hashes: Vec<Vec<u8>> = items.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in items_hashes.iter().take(n) {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree.frontier().cloned()
    }

    /// Try to get the frontier, returning an error on failure.
    pub fn try_frontier(items: &[TraceItem], n: usize, seed: &[u8]) -> Result<TraceTreeFrontier> {
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

        let mut trace_tree = TraceTree::new(1);
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
        self.fingerprint.len()
    }

    /// Check if the commitment is empty.
    pub fn is_empty(&self) -> bool {
        self.fingerprint.is_empty()
    }

    pub fn diff(&self, other: &TraceCommitment) -> Option<usize> {
        assert!(
            self.fingerprint.len() == other.fingerprint.len(),
            "Trace commitetment length mismatch"
        );
        assert!(
            self.fingerprint.bits_per_item() == other.fingerprint.bits_per_item(),
            "Trace commitetment bit packing mismatch"
        );

        // TODO: did we actually need those diff parts in BitPacker?
        let Some((index, _, _)) = self
            .fingerprint
            .bits_packer
            .diff(&self.fingerprint.bits, &other.fingerprint.bits)
        else {
            return None;
        };

        Some(index)
    }
}

struct Window<T: Clone> {
    queue: VecDeque<Option<T>>,
    size: usize,
}

impl<T: Clone> Window<T> {
    fn new(size: usize) -> Self {
        Self {
            queue: VecDeque::from(vec![None; size]),
            size,
        }
    }

    fn push(&mut self, item: T) {
        self.queue.pop_front();
        self.queue.push_back(Some(item));
    }

    fn first(&self) -> Option<&T> {
        self.queue.iter().flatten().next()
    }

    fn to_vec(&self) -> Vec<T> {
        self.queue.clone().into_iter().flatten().collect()
    }
}

pub struct TraceVerifier {
    pub trace_commitment: TraceCommitment,

    pub fingerprint_acc: FingerprintAccumulator,
    pub latest_frontier: TraceTreeFrontier,

    pub window_frontiers: Window<TraceTreeFrontier>,
    pub window_items: Window<TraceItem>,
}

pub enum VerificationResult {
    Ok,
    Fraud(TraceWindow),
}

impl TraceVerifier {
    pub fn new(trace_commitment: TraceCommitment, seed: &[u8]) -> Self {
        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let init_frontier = trace_tree.frontier().cloned().unwrap();

        let bit_packer = trace_commitment.fingerprint.bits_packer.clone();
        let mut fingerprint_acc = FingerprintAccumulator::new(bit_packer);

        let mut window_frontiers: Window<TraceTreeFrontier> = Window::new(WINDOW_SIZE.into());
        let window_items: Window<TraceItem> = Window::new(WINDOW_SIZE.into());

        window_frontiers.push(init_frontier.clone());

        Self {
            trace_commitment,

            fingerprint_acc,
            latest_frontier: init_frontier,

            window_frontiers,
            window_items,
        }
    }

    pub fn verify(&mut self, item: &TraceItem) -> VerificationResult {
        let item_frontier = self.latest_frontier.clone();

        self.window_frontiers.push(item_frontier);
        self.window_items.push(item.clone());

        self.latest_frontier.append(Bytes(item.hash()));

        let trace_tree = TraceTree::from_frontier(1, self.latest_frontier.clone());

        let root = trace_tree
            .root(0)
            .expect("Failed to get Trace Merkle Tree root");

        self.latest_frontier = trace_tree.frontier().unwrap().clone();
        self.fingerprint_acc.append(&root.0);

        let latest_fingerprint = self.fingerprint_acc.clone().into_fingerprint();

        let index = latest_fingerprint.len() - 1;

        if latest_fingerprint.bits_packer.diff_at_index(
            index,
            &latest_fingerprint.bits,
            &self.trace_commitment.fingerprint.bits,
        ) {
            let diff_bits = self
                .trace_commitment
                .fingerprint
                .bits_packer
                .get_range(
                    index.saturating_sub(WINDOW_SIZE.into()) + 1,
                    index + 1,
                    &self.trace_commitment.fingerprint.bits,
                )
                .unwrap();

            let window_fingerprint = Fingerprint::from(
                diff_bits,
                self.trace_commitment.fingerprint.bits_packer,
                WINDOW_SIZE.into(),
            );

            let window_frontier = self.window_frontiers.first().unwrap().clone();
            let ser_window_frontier =
                SerializableFrontier::from_frontier(window_frontier).to_bytes();

            // TODO: consider renaming Window struct and TraceWindow have different behavior but
            // similiar naming
            let fraud_window = TraceWindow {
                frontier: ser_window_frontier,
                items: self.window_items.to_vec(),
                fingerprint: window_fingerprint,
            };

            return VerificationResult::Fraud(fraud_window);
        }

        VerificationResult::Ok
    }
}

#[cfg(test)]
mod tests {
    use raster_core::trace::TraceInputParam;

    use super::*;
    use crate::precomputed;

    /// Helper function to create a TileTraceItem for testing.
    fn make_tile_trace_item(input: u64, output: u64) -> TraceItem {
        TraceItem {
            fn_name: format!("test_tile_{}", input),
            desc: None,
            inputs: vec![TraceInputParam {
                name: "input".to_string(),
                ty: "u64".to_string(),
            }],
            input_data: Vec::new(),
            output_type: Some("u64".to_string()),
            output_data: output.to_le_bytes().to_vec(),
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

    /// Test that guest-style compute_root matches bridgetree's root for various frontiers.
    #[test]
    fn test_compute_root_matches_bridgetree() {
        fn empty_at_level(level: u8) -> Vec<u8> {
            if level == 0 {
                return precomputed::EMPTY_TRIE_NODES[0].to_vec();
            }
            let child = empty_at_level(level - 1);
            combine_level(level - 1, &child, &child)
        }

        fn combine_level(level: u8, left: &[u8], right: &[u8]) -> Vec<u8> {
            let mut data = Vec::with_capacity(1 + 32 + 32);
            data.push(level);
            data.extend_from_slice(left);
            data.extend_from_slice(right);
            let mut hasher = sha2::Sha256::new();
            hasher.update(&data);
            hasher.finalize().to_vec()
        }

        fn compute_root_guest(position: u64, leaf: &[u8], ommers: &[Vec<u8>]) -> Vec<u8> {
            let mut cur = leaf.to_vec();
            let mut ommer_idx = 0;
            for level in 0u8..32 {
                let bit = (position >> level) & 1;
                if bit == 0 {
                    cur = combine_level(level, &cur, &empty_at_level(level));
                } else {
                    let left = if ommer_idx < ommers.len() {
                        ommers[ommer_idx].clone()
                    } else {
                        empty_at_level(level)
                    };
                    cur = combine_level(level, &left, &cur);
                    ommer_idx += 1;
                }
            }
            cur
        }

        let seed = precomputed::EMPTY_TRIE_NODES[0];
        let items: Vec<TraceItem> = (0..10).map(|i| make_tile_trace_item(i, i)).collect();

        let mut tree = TraceTree::new(1);
        tree.append(Bytes(seed.to_vec()));

        for (i, item) in items.iter().enumerate() {
            tree.append(Bytes(item.hash()));
            let bridgetree_root = tree.root(0).expect("root").0.clone();

            let frontier = tree.frontier().expect("frontier").clone();
            let ser_frontier = SerializableFrontier::from_frontier(&frontier);
            let deser_frontier = ser_frontier
                .into_frontier()
                .expect("Can't deserialize frontier");

            let pos = u64::from(deser_frontier.position());
            let leaf = deser_frontier.leaf().0.clone();
            let ommers: Vec<Vec<u8>> = deser_frontier
                .ommers()
                .iter()
                .map(|o| o.0.clone())
                .collect();

            let guest_root = compute_root_guest(pos, &leaf, &ommers);

            assert_eq!(
                bridgetree_root,
                guest_root,
                "Root mismatch at position {} (after {} items)",
                pos,
                i + 1
            );
        }
    }
}
