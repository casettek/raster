//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

use risc0_zkvm::guest::env;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use alloc::string::String;
use alloc::vec::Vec;

extern crate alloc;

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
}

/// Output from the transition guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionOutput {
    pub new_frontier: SerializableFrontier,
    pub item_hash: Vec<u8>,
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

    // 5. Convert back to serializable form
    let new_frontier = frontier.to_serializable();

    // 6. Commit output to journal
    let output = TransitionOutput {
        new_frontier,
        item_hash,
    };
    env::commit(&output);
}
