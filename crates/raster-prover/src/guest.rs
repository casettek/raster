//! RISC0 guest types and host utilities for iterative trace verification.
//!
//! This module provides:
//! - Shared types for guest input/output (TransitionInput, TransitionOutput)
//! - Host-side utilities for preparing inputs and verifying outputs
//! - The compiled transition guest ELF (when built)
//!
//! The types in this module are designed to be serialization-compatible with
//! the types used in the RISC0 guest program.

use raster_core::trace::TraceItem;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::trace::SerializableFrontier;

// Include the generated methods (ELF) from the build script.
// This may not exist if the build failed or RISC0_SKIP_BUILD was set.
#[cfg(not(feature = "skip-guest-build"))]
include!(concat!(env!("OUT_DIR"), "/methods.rs"));

/// Input to the transition guest program.
///
/// Contains the current frontier state and the trace item to append,
/// along with fingerprint verification data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    /// The current frontier state (before appending)
    pub frontier: SerializableFrontier,
    /// The trace item to hash and append
    pub trace_item: TraceItem,
    /// Expected fingerprint (packed u64s) for verification
    pub fingerprint: Vec<u64>,
    /// Position in fingerprint to verify (index of this trace item in the window)
    pub position: usize,
    /// Bits per fingerprint item
    pub bits_per_item: usize,
}

impl TransitionInput {
    /// Create a new transition input.
    pub fn new(
        frontier: SerializableFrontier,
        trace_item: TraceItem,
        fingerprint: Vec<u64>,
        position: usize,
        bits_per_item: usize,
    ) -> Self {
        Self {
            frontier,
            trace_item,
            fingerprint,
            position,
            bits_per_item,
        }
    }

    /// Serialize the input for passing to the guest.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("Failed to serialize TransitionInput")
    }
}

/// Output from the transition guest program.
///
/// Contains the new frontier state after appending, the hash of the trace item,
/// and fingerprint verification results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionOutput {
    /// The new frontier state (after appending)
    pub new_frontier: SerializableFrontier,
    /// The SHA256 hash of the trace item that was appended
    pub item_hash: Vec<u8>,
    /// The computed tree root after appending
    pub tree_root: Vec<u8>,
    /// Whether fingerprint verification passed
    pub fingerprint_verified: bool,
}

impl TransitionOutput {
    /// Deserialize output from guest journal bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }

    /// Verify that the item_hash matches the expected hash of the trace item.
    ///
    /// This uses postcard serialization (matching the guest) to hash the item.
    pub fn verify_item_hash(&self, item: &TraceItem) -> bool {
        let expected = hash_trace_item_postcard(item);
        self.item_hash == expected
    }
}

/// Hash a TraceItem using postcard serialization (matching guest behavior).
///
/// Note: This differs from the BytesHashable implementation which uses bincode.
/// The guest uses postcard because it's no_std compatible.
pub fn hash_trace_item_postcard(item: &TraceItem) -> Vec<u8> {
    let data = postcard::to_allocvec(item).expect("Failed to serialize TraceItem");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    hasher.finalize().to_vec()
}

/// Prepare a batch of transition inputs from a frontier and trace items.
///
/// This is useful for preparing inputs for multiple sequential transitions.
/// Note: Each input depends on the output of the previous transition, so
/// this function returns inputs that must be executed sequentially.
///
/// # Arguments
/// * `initial_frontier` - The frontier state before the first trace item
/// * `items` - The trace items to process
/// * `fingerprint` - The packed fingerprint u64s for verification
/// * `start_position` - The starting position in the fingerprint for the first item
/// * `bits_per_item` - Bits per fingerprint item
pub fn prepare_batch_inputs(
    initial_frontier: SerializableFrontier,
    items: &[TraceItem],
    fingerprint: Vec<u64>,
    start_position: usize,
    bits_per_item: usize,
) -> Vec<TransitionInput> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            // For the first item, use the initial frontier
            // For subsequent items, the caller must update the frontier
            // from the previous transition's output
            if i == 0 {
                TransitionInput::new(
                    initial_frontier.clone(),
                    item.clone(),
                    fingerprint.clone(),
                    start_position + i,
                    bits_per_item,
                )
            } else {
                // Placeholder - caller must update frontier between transitions
                TransitionInput::new(
                    initial_frontier.clone(),
                    item.clone(),
                    fingerprint.clone(),
                    start_position + i,
                    bits_per_item,
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_trace_item(id: u64) -> TraceItem {
        TraceItem {
            fn_name: format!("test_{}", id),
            desc: None,
            inputs: vec![],
            input_data: format!("{}", id),
            output_type: Some("u64".to_string()),
            output_data: format!("{}", id * 2),
        }
    }

    #[test]
    fn test_transition_input_serialization() {
        // Create a simple frontier
        let leaf = vec![0u8; 32];
        let frontier = SerializableFrontier {
            position: 0,
            leaf,
            ommers: vec![],
        };

        let item = make_test_trace_item(1);
        let fingerprint = vec![0u64; 1];
        let input = TransitionInput::new(frontier, item, fingerprint, 0, 8);

        let bytes = input.to_bytes();
        assert!(!bytes.is_empty());

        // Deserialize and verify
        let recovered: TransitionInput =
            postcard::from_bytes(&bytes).expect("Failed to deserialize");
        assert_eq!(recovered.frontier.position, 0);
        assert_eq!(recovered.trace_item.fn_name, "test_1");
        assert_eq!(recovered.position, 0);
        assert_eq!(recovered.bits_per_item, 8);
    }

    #[test]
    fn test_hash_trace_item_postcard() {
        let item = make_test_trace_item(42);

        let hash1 = hash_trace_item_postcard(&item);
        let hash2 = hash_trace_item_postcard(&item);

        // Same item should produce same hash
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32);

        // Different item should produce different hash
        let other_item = make_test_trace_item(43);
        let other_hash = hash_trace_item_postcard(&other_item);
        assert_ne!(hash1, other_hash);
    }

    #[test]
    fn test_output_verify_item_hash() {
        let item = make_test_trace_item(99);
        let correct_hash = hash_trace_item_postcard(&item);

        let output = TransitionOutput {
            new_frontier: SerializableFrontier {
                position: 1,
                leaf: correct_hash.clone(),
                ommers: vec![],
            },
            item_hash: correct_hash.clone(),
            tree_root: correct_hash,
            fingerprint_verified: true,
        };

        assert!(output.verify_item_hash(&item));

        // Wrong item should fail verification
        let wrong_item = make_test_trace_item(100);
        assert!(!output.verify_item_hash(&wrong_item));
    }
}
