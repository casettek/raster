//! Replay Guest Documentation
//!
//! # Overview
//!
//! Replay guests are tile-specific RISC0 guests that re-execute individual tile
//! functions in the zkVM. Unlike the Init and Transition guests which are generic,
//! Replay guests are generated dynamically for each tile using the existing
//! [`guest_builder`] module in `raster-backend-risc0`.
//!
//! # Architecture
//!
//! The Replay guest is the "worker" in the three-guest proving pipeline:
//!
//! ```text
//! Init → Transition₁ → Transition₂ → ... → TransitionN
//!            ↑              ↑                   ↑
//!         Replay₁        Replay₂            ReplayN
//! ```
//!
//! Each Replay guest:
//! 1. Receives serialized tile input from the host
//! 2. Executes the tile's ABI wrapper function
//! 3. Commits the serialized output to the journal
//!
//! # Verification by Transition
//!
//! The Transition guest verifies Replay receipts in two ways:
//!
//! 1. **Image ID Verification**: Each tile produces a unique Replay guest with
//!    its own image ID. The Transition guest checks that the Replay receipt's
//!    image ID matches the expected image ID from the chain link. This ensures
//!    the correct tile was executed.
//!
//! 2. **Output Verification**: The Transition guest compares the Replay journal
//!    output with the expected output from the trace item. This ensures the
//!    tile produced the expected result.
//!
//! # No Changes Required
//!
//! The existing Replay guest implementation in `raster-backend-risc0::guest_builder`
//! is used unchanged. The only requirement is that each tile's Replay guest:
//!
//! - Has a deterministic image ID (based on the tile's code)
//! - Commits its output as raw bytes to the journal
//! - Uses the standard input format (length-prefixed bytes)
//!
//! # Image ID Mapping
//!
//! To use this system, you need a mapping from tile IDs to their Replay guest
//! image IDs. This is typically computed during the build process when each
//! tile's Replay guest is compiled.
//!
//! ```rust,ignore
//! use std::collections::HashMap;
//!
//! // Map from tile_id to its Replay guest image ID
//! let image_ids: HashMap<String, [u8; 32]> = HashMap::from([
//!     ("my_tile".to_string(), [/* computed from build */]),
//!     ("another_tile".to_string(), [/* ... */]),
//! ]);
//! ```
//!
//! # Example Integration
//!
//! When creating a Transition input, you provide the expected Replay image ID:
//!
//! ```rust,ignore
//! use raster_prover::guest::types::{ReplayExpectation, TransitionInput, Fingerprint};
//!
//! let replay_expectation = ReplayExpectation {
//!     image_id: image_ids["my_tile"],
//!     trace_item: trace_item,
//!     frontier: frontier,
//! };
//!
//! let fingerprint = Fingerprint {
//!     bytes: packed_fingerprint_bytes,
//!     bits_per_item: 8,
//!     inclusion_proof: [0u8; 32],
//! };
//!
//! let input = TransitionInput {
//!     current_replay_expectation: replay_expectation,
//!     replay_receipt_bytes: replay_receipt.to_bytes(),
//!     fingerprint: fingerprint,
//!     next_trace_item: Some(next_item),
//!     next_replay_image_id: Some(next_image_id),
//! };
//! ```

// This module is documentation-only; no code is needed here.
// The actual Replay guest implementation is in:
// crates/raster-backend-risc0/src/guest_builder.rs
