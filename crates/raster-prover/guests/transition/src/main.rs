//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

#![no_main]
#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use risc0_zkvm::guest::env;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

risc0_zkvm::guest::entry!(main);

// ============================================================================
// Types (duplicated for no_std compatibility)
// ============================================================================

/// Input parameter metadata for a trace item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceInputParam {
    pub name: String,
    pub ty: String,
}

/// A structured trace item emitted during tile execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceItem {
    pub fn_name: String,
    pub desc: Option<String>,
    pub inputs: Vec<TraceInputParam>,
    pub input_data: String,
    pub output_type: Option<String>,
    pub output_data: String,
}

/// A serializable representation of a frontier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializableFrontier {
    pub position: u64,
    pub leaf: Vec<u8>,
    pub ommers: Vec<Vec<u8>>,
}

/// Input to the transition guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    pub frontier: SerializableFrontier,
    pub trace_item: TraceItem,
    /// Expected fingerprint (packed u64s) for verification
    pub fingerprint: Vec<u64>,
    /// Position in fingerprint to verify (index of this trace item)
    pub position: usize,
    /// Bits per fingerprint item
    pub bits_per_item: usize,
}

/// Output from the transition guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionOutput {
    pub new_frontier: SerializableFrontier,
    pub item_hash: Vec<u8>,
    /// The computed tree root after appending
    pub tree_root: Vec<u8>,
    /// Whether fingerprint verification passed
    pub fingerprint_verified: bool,
}

// ============================================================================
// Precomputed empty node (level 0)
// ============================================================================

/// Empty leaf hash (precomputed SHA256 of "empty")
const EMPTY_LEAF: [u8; 32] =
    hex_literal::hex!("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a");

const HASH_SIZE: usize = 32;

// ============================================================================
// Hashable operations (matching bridgetree's Hashable trait)
// ============================================================================

/// Combine two hashes at a given level to produce parent hash.
/// This matches bridgetree's Hashable::combine implementation.
fn combine(level: u8, left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);
    data.push(level);
    data.extend_from_slice(left);
    data.extend_from_slice(right);

    let mut hasher = Sha256::new();
    hasher.update(&data);
    hasher.finalize().to_vec()
}

/// Get the empty node hash at a given level.
/// Level 0 is the empty leaf, higher levels are computed recursively.
fn empty_at_level(level: u8) -> Vec<u8> {
    if level == 0 {
        return EMPTY_LEAF.to_vec();
    }
    let child = empty_at_level(level - 1);
    combine(level - 1, &child, &child)
}

// ============================================================================
// Minimal Frontier Implementation
// ============================================================================

/// A minimal no_std-compatible frontier for incremental Merkle tree building.
///
/// This implements the same semantics as bridgetree::NonEmptyFrontier but
/// without the std dependencies.
struct Frontier {
    position: u64,
    leaf: Vec<u8>,
    ommers: Vec<Vec<u8>>,
}

impl Frontier {
    /// Create a frontier from serializable form.
    fn from_serializable(s: &SerializableFrontier) -> Self {
        Self {
            position: s.position,
            leaf: s.leaf.clone(),
            ommers: s.ommers.clone(),
        }
    }

    /// Convert to serializable form.
    fn to_serializable(&self) -> SerializableFrontier {
        SerializableFrontier {
            position: self.position,
            leaf: self.leaf.clone(),
            ommers: self.ommers.clone(),
        }
    }

    /// Append a new leaf to the frontier.
    ///
    /// This implements the same algorithm as bridgetree's NonEmptyFrontier::append.
    fn append(&mut self, leaf: Vec<u8>) {
        // The new position after appending
        let new_position = self.position + 1;

        // Determine how many complete subtrees we need to hash together.
        // This is based on the trailing zeros in the new position.
        let complete_levels = new_position.trailing_zeros() as usize;

        // Start with the current leaf as the value to combine upward
        let mut carry = self.leaf.clone();

        // Combine with ommers at each level where we complete a subtree
        for level in 0..complete_levels {
            if level < self.ommers.len() {
                // Combine: omers[level] is the left sibling, carry is the right
                carry = combine(level as u8, &self.ommers[level], &carry);
            } else {
                // No ommer at this level, combine with empty
                let empty = empty_at_level(level as u8);
                carry = combine(level as u8, &empty, &carry);
            }
        }

        // Remove the ommers that were consumed
        if complete_levels > 0 && complete_levels <= self.ommers.len() {
            self.ommers.drain(0..complete_levels);
        } else if complete_levels > self.ommers.len() {
            self.ommers.clear();
        }

        // The carry becomes the new ommer at the complete_levels position
        // Insert it at the front (level 0 position)
        if complete_levels > 0 {
            self.ommers.insert(0, carry);
        }

        // Update state
        self.leaf = leaf;
        self.position = new_position;
    }
}

// ============================================================================
// Hashing
// ============================================================================

/// Hash a TraceItem using SHA256 of its postcard-serialized form.
fn hash_trace_item(item: &TraceItem) -> Vec<u8> {
    // Use postcard for serialization (matching the guest's dependencies)
    let data = postcard::to_allocvec(item).expect("Failed to serialize TraceItem");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    hasher.finalize().to_vec()
}

// ============================================================================
// Root Computation and Fingerprint Verification
// ============================================================================

/// Tree depth for the merkle tree (2^32 leaves max).
const TREE_DEPTH: u8 = 32;

/// Compute the merkle root from the frontier at the given tree depth.
///
/// This walks up from the current leaf position, combining with ommers
/// (stored siblings) or empty nodes at each level.
fn compute_root(frontier: &Frontier) -> Vec<u8> {
    let mut cur = frontier.leaf.clone();
    let mut ommer_idx = 0;

    for level in 0..TREE_DEPTH {
        let bit = (frontier.position >> level) & 1;
        if bit == 0 {
            // Current is left child, sibling is empty
            cur = combine(level, &cur, &empty_at_level(level));
        } else {
            // Current is right child, sibling is ommer
            let left = if ommer_idx < frontier.ommers.len() {
                frontier.ommers[ommer_idx].clone()
            } else {
                empty_at_level(level)
            };
            cur = combine(level, &left, &cur);
            ommer_idx += 1;
        }
    }
    cur
}

/// Crop a hash to the specified number of bits, returning as u64.
///
/// Takes the first `bits` bits from the hash (little-endian interpretation).
fn crop_to_bits(hash: &[u8], bits: usize) -> u64 {
    if bits == 0 || hash.is_empty() {
        return 0;
    }

    // Number of full bytes needed
    let bytes_needed = (bits + 7) / 8;
    let bytes_to_use = bytes_needed.min(hash.len()).min(8);

    // Build u64 from bytes (little-endian)
    let mut value: u64 = 0;
    for (i, &byte) in hash.iter().take(bytes_to_use).enumerate() {
        value |= (byte as u64) << (i * 8);
    }

    // Mask to keep only the requested bits
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };

    value & mask
}

/// Extract a fingerprint value at the given position from packed u64s.
///
/// Each fingerprint item is stored at `position * bits_per_item` bits offset.
fn get_fingerprint_at(packed: &[u64], position: usize, bits: usize) -> u64 {
    if bits == 0 || packed.is_empty() {
        return 0;
    }

    let bit_offset = position * bits;
    let block_idx = bit_offset / 64;
    let block_offset = bit_offset % 64;

    if block_idx >= packed.len() {
        return 0;
    }

    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };

    let value = (packed[block_idx] >> block_offset) & mask;

    // Handle overflow to next block if the value spans two u64s
    let overflow = (block_offset + bits).saturating_sub(64);
    if overflow > 0 && block_idx + 1 < packed.len() {
        let overflow_mask = (1u64 << overflow) - 1;
        let next_bits = packed[block_idx + 1] & overflow_mask;
        value | (next_bits << (bits - overflow))
    } else {
        value
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    // 1. Read input from host
    let input: TransitionInput = env::read();

    // 2. Reconstruct frontier from serializable form
    let mut frontier = Frontier::from_serializable(&input.frontier);

    // 3. Hash the trace item
    let item_hash = hash_trace_item(&input.trace_item);

    // 4. Append the hash to the frontier
    frontier.append(item_hash.clone());

    // 5. Compute the merkle tree root
    let tree_root = compute_root(&frontier);

    // 6. Verify fingerprint
    let fingerprint_verified = if !input.fingerprint.is_empty() && input.bits_per_item > 0 {
        // Crop the tree root to fingerprint bits
        let computed_fingerprint = crop_to_bits(&tree_root, input.bits_per_item);

        // Extract expected fingerprint at this position
        let expected_fingerprint =
            get_fingerprint_at(&input.fingerprint, input.position, input.bits_per_item);

        // Compare
        computed_fingerprint == expected_fingerprint
    } else {
        // No fingerprint verification requested
        true
    };

    // 7. Convert back to serializable form
    let new_frontier = frontier.to_serializable();

    // 8. Commit output to journal
    let output = TransitionOutput {
        new_frontier,
        item_hash,
        tree_root,
        fingerprint_verified,
    };
    env::commit(&output);
}
