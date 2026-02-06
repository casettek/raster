//! Transition guest logic for verifying Replay proofs and chaining.
//!
//! The Transition guest verifies a Replay proof matches expected expectations,
//! compares the output, accumulates into the BridgeTree, validates against
//! the fingerprint, and produces the next replay expectation.
//!
//! This module provides the core logic that can be used both in std and no_std
//! environments (the guest crates handle the alloc setup).

use bridgetree::{BridgeTree, NonEmptyFrontier};
use sha2::{Digest, Sha256};

use super::types::{
    ReplayExpectation, TransitionInput, TransitionOutput, TransitionStatus,
};
use crate::bit_packer::BitPacker;
use crate::trace::{Bytes, SerializableFrontier};

/// Error type for Transition guest execution.
#[derive(Debug, Clone)]
pub enum TransitionError {
    /// The Replay receipt image ID doesn't match expected.
    ImageIdMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    /// Failed to deserialize the Replay receipt.
    ReceiptDeserializationFailed,
    /// Failed to verify the Replay proof.
    ProofVerificationFailed,
    /// Failed to decode the expected output.
    OutputDecodeFailed,
    /// The output from Replay doesn't match expected.
    OutputMismatch,
    /// Failed to reconstruct the frontier.
    FrontierReconstructionFailed,
    /// Failed to compute tree root.
    TreeRootFailed,
    /// Fingerprint validation failed.
    FingerprintValidationFailed,
}

/// Result of Transition execution.
pub type TransitionResult = core::result::Result<TransitionOutput, TransitionError>;

/// Transition guest execution context.
///
/// This struct holds the input and provides methods to perform each step
/// of the Transition verification process.
pub struct TransitionContext {
    input: TransitionInput,
}

impl TransitionContext {
    /// Create a new Transition context.
    pub fn new(input: TransitionInput) -> Self {
        Self { input }
    }

    /// Execute the full Transition logic.
    ///
    /// This performs all verification steps and produces the output.
    /// In the actual guest, RISC0's `env::verify()` is called for proof verification.
    ///
    /// # Arguments
    ///
    /// * `verify_fn` - A function to verify the Replay receipt. In the guest,
    ///   this should call `risc0_zkvm::guest::env::verify()`.
    ///   Returns `(image_id, journal_bytes)` on success.
    pub fn execute<F>(self, verify_fn: F) -> TransitionResult
    where
        F: FnOnce(&[u8]) -> Result<([u8; 32], Vec<u8>), ()>,
    {
        let expectation = &self.input.current_replay_expectation;

        // Step 1: Verify the Replay receipt and extract image_id + journal
        let (replay_image_id, replay_output) = verify_fn(&self.input.replay_receipt_bytes)
            .map_err(|_| TransitionError::ProofVerificationFailed)?;

        // Step 2: Check that image_id matches expected
        if replay_image_id != expectation.image_id {
            return Err(TransitionError::ImageIdMismatch {
                expected: expectation.image_id,
                actual: replay_image_id,
            });
        }

        // Step 3: Compare Replay output with expected output
        // The expected output is base64-encoded in the trace item
        let expected_output = decode_base64(&expectation.trace_item.output_data)
            .ok_or(TransitionError::OutputDecodeFailed)?;
        let output_matched = replay_output == expected_output;

        // Step 4: Compute trace item hash
        let trace_item_hash = compute_trace_item_hash(&expectation.trace_item);

        // Step 5: Reconstruct frontier and add item to tree
        // Position is tracked in the frontier
        let position = expectation.frontier.position;
        let frontier = expectation
            .frontier
            .to_frontier()
            .ok_or(TransitionError::FrontierReconstructionFailed)?;

        let (new_frontier, root_hash) = add_to_tree(frontier, &trace_item_hash)?;

        // Step 6: Validate against fingerprint
        let bp = BitPacker::new(self.input.fingerprint.bits_per_item);
        let fingerprint_valid = validate_fingerprint(
            &bp,
            position as usize,
            &self.input.fingerprint.bytes,
            &root_hash,
        );

        // Step 7: Build next replay expectation (if there's a next item)
        let (next_replay_expectation, status) = match (
            &self.input.next_trace_item,
            &self.input.next_replay_image_id,
        ) {
            (Some(next_item), Some(next_image_id)) => {
                let next_expectation = ReplayExpectation {
                    image_id: *next_image_id,
                    trace_item: next_item.clone(),
                    frontier: new_frontier.clone(),
                };
                (Some(next_expectation), TransitionStatus::Continue)
            }
            _ => {
                // This is the last item
                let all_valid = output_matched && fingerprint_valid;
                (None, TransitionStatus::Complete { all_valid })
            }
        };

        Ok(TransitionOutput {
            verified_position: position,
            trace_item_hash,
            new_frontier,
            next_replay_expectation,
            status,
        })
    }
}

/// Compute the hash of a TraceItem.
fn compute_trace_item_hash(item: &raster_core::trace::TraceItem) -> [u8; 32] {
    // Serialize using bincode and hash with SHA256
    let data = bincode::serialize(item).expect("Failed to serialize trace item");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Add an item hash to the BridgeTree and return the new frontier and root.
fn add_to_tree(
    frontier: NonEmptyFrontier<Bytes>,
    item_hash: &[u8; 32],
) -> Result<(SerializableFrontier, [u8; 32]), TransitionError> {
    // Create a tree from the frontier
    let mut tree: BridgeTree<Bytes, u64, 32> = BridgeTree::from_frontier(1, frontier);

    // Append the item hash
    tree.append(Bytes(item_hash.to_vec()));

    // Get the new frontier
    let new_frontier = tree
        .frontier()
        .cloned()
        .ok_or(TransitionError::TreeRootFailed)?;

    // Get the root
    let root = tree.root(0).ok_or(TransitionError::TreeRootFailed)?;

    let mut root_hash = [0u8; 32];
    root_hash.copy_from_slice(&root.0);

    Ok((SerializableFrontier::from_frontier(&new_frontier), root_hash))
}

/// Validate that the root matches the fingerprint at the given position.
fn validate_fingerprint(
    bp: &BitPacker,
    position: usize,
    fingerprint: &[u64],
    root_hash: &[u8; 32],
) -> bool {
    // Extract expected bits from fingerprint
    let expected_bits = match bp.get(position, fingerprint) {
        Some(bits) => bits,
        None => return false,
    };

    // Extract the same number of bits from the root hash
    let bits_per_item = bp.bits_per_item();

    // Convert root hash bytes to a comparable value
    // We take the first bits_per_item bits from the root hash
    let mut root_bytes = [0u8; 8];
    let bytes_needed = (bits_per_item + 7) / 8;
    root_bytes[..bytes_needed.min(8)].copy_from_slice(&root_hash[..bytes_needed.min(8)]);
    let root_value = u64::from_le_bytes(root_bytes);

    // Mask to get only the bits we care about
    let mask = (1u64 << bits_per_item) - 1;
    let actual_bits = root_value & mask;

    expected_bits == actual_bits
}

/// Decode a base64 string to bytes.
///
/// Simple base64 decoder for use in no_std environment.
fn decode_base64(encoded: &str) -> Option<Vec<u8>> {
    fn char_to_value(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => None, // Padding
            _ => None,
        }
    }

    let bytes = encoded.as_bytes();
    let mut result = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for &b in bytes {
        if b == b'=' {
            break;
        }
        if let Some(value) = char_to_value(b) {
            buffer = (buffer << 6) | (value as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                result.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        } else if b != b'\n' && b != b'\r' && b != b' ' {
            return None; // Invalid character
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::Fingerprint;
    use crate::trace::SerializableFrontier;
    use raster_core::trace::TraceItem;

    fn make_test_trace_item() -> TraceItem {
        TraceItem {
            fn_name: "test_tile".into(),
            desc: None,
            inputs: vec![],
            input_data: "dGVzdA==".into(), // base64 "test"
            output_type: Some("u64".into()),
            output_data: "dGVzdA==".into(), // base64 "test"
        }
    }

    fn make_test_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: vec![0u8; 32],
            ommers: vec![],
        }
    }

    fn make_test_fingerprint() -> Fingerprint {
        Fingerprint {
            bytes: vec![0u64; 1],
            bits_per_item: 8,
            inclusion_proof: [0u8; 32],
        }
    }

    #[test]
    fn test_decode_base64() {
        assert_eq!(decode_base64("dGVzdA=="), Some(b"test".to_vec()));
        assert_eq!(decode_base64("SGVsbG8="), Some(b"Hello".to_vec()));
        assert_eq!(decode_base64(""), Some(vec![]));
    }

    #[test]
    fn test_compute_trace_item_hash_deterministic() {
        let item = make_test_trace_item();
        let hash1 = compute_trace_item_hash(&item);
        let hash2 = compute_trace_item_hash(&item);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_trace_item_hash_differs() {
        let item1 = make_test_trace_item();
        let mut item2 = make_test_trace_item();
        item2.fn_name = "different_tile".into();

        let hash1 = compute_trace_item_hash(&item1);
        let hash2 = compute_trace_item_hash(&item2);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_transition_context_with_matching_output() {
        let trace_item = make_test_trace_item();
        let frontier = make_test_frontier();

        let expectation = ReplayExpectation {
            image_id: [1u8; 32],
            trace_item: trace_item.clone(),
            frontier,
        };

        let input = TransitionInput {
            current_replay_expectation: expectation,
            replay_receipt_bytes: vec![],
            fingerprint: make_test_fingerprint(),
            next_trace_item: None,
            next_replay_image_id: None,
        };

        let context = TransitionContext::new(input);

        // Mock verify function that returns matching output
        let result = context.execute(|_receipt| {
            Ok(([1u8; 32], b"test".to_vec()))
        });

        assert!(result.is_ok());
        let output = result.unwrap();
        // Status should be Complete since there's no next item
        assert!(matches!(output.status, TransitionStatus::Complete { .. }));
    }

    #[test]
    fn test_transition_context_with_mismatched_image_id() {
        let trace_item = make_test_trace_item();
        let frontier = make_test_frontier();

        let expectation = ReplayExpectation {
            image_id: [1u8; 32],
            trace_item,
            frontier,
        };

        let input = TransitionInput {
            current_replay_expectation: expectation,
            replay_receipt_bytes: vec![],
            fingerprint: make_test_fingerprint(),
            next_trace_item: None,
            next_replay_image_id: None,
        };

        let context = TransitionContext::new(input);

        // Mock verify function that returns wrong image ID
        let result = context.execute(|_receipt| {
            Ok(([2u8; 32], b"test".to_vec())) // Wrong image ID
        });

        assert!(matches!(result, Err(TransitionError::ImageIdMismatch { .. })));
    }

    #[test]
    fn test_transition_context_with_next_item() {
        let trace_item = make_test_trace_item();
        let frontier = make_test_frontier();

        let expectation = ReplayExpectation {
            image_id: [1u8; 32],
            trace_item: trace_item.clone(),
            frontier,
        };

        let mut next_trace_item = make_test_trace_item();
        next_trace_item.fn_name = "next_tile".into();
        let next_image_id = [2u8; 32];

        let input = TransitionInput {
            current_replay_expectation: expectation,
            replay_receipt_bytes: vec![],
            fingerprint: make_test_fingerprint(),
            next_trace_item: Some(next_trace_item.clone()),
            next_replay_image_id: Some(next_image_id),
        };

        let context = TransitionContext::new(input);

        let result = context.execute(|_receipt| {
            Ok(([1u8; 32], b"test".to_vec()))
        });

        assert!(result.is_ok());
        let output = result.unwrap();

        // Should have Continue status with next expectation
        assert_eq!(output.status, TransitionStatus::Continue);
        assert!(output.next_replay_expectation.is_some());

        let next_exp = output.next_replay_expectation.unwrap();
        assert_eq!(next_exp.image_id, next_image_id);
        assert_eq!(next_exp.trace_item.fn_name, "next_tile");
    }
}
