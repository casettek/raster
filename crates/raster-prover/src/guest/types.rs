//! Shared types for RISC0 proof guests.
//!
//! This module provides the types used for communication between the Init,
//! Replay, and Transition guests in the proving pipeline.

use serde::{Deserialize, Serialize};

use crate::trace::SerializableFrontier;
use raster_core::trace::TraceItem;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayExpectation {
    /// The expected image ID of the Replay guest.
    /// The Replay receipt must have this image ID for verification to pass.
    pub image_id: [u8; 32],

    /// The trace item that should have been executed by the Replay guest.
    pub trace_item: TraceItem,

    /// Merkle tree frontier state before this item.
    pub frontier: SerializableFrontier,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fingerprint {
    pub bytes: Vec<u64>,
    pub bits_per_item: usize,
    // Proof that partial fingerprint is part of full fingerprint
    pub inclusion_proof: [u8; 32],
}

// === Init Guest Types ===

/// Input to the Init guest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InitInput {
    /// BitPacker-packed fingerprint bits.
    pub fingerprint: Fingerprint,

    /// The trace to be verified.
    pub trace: Vec<TraceItem>,

    /// Merkle tree frontier before the first item.
    pub frontier: SerializableFrontier,

    /// The expected image ID for the first Replay guest.
    pub first_replay_image_id: [u8; 32],
}

/// Output from the Init guest.
///
/// Committed to the journal to establish the initial proven commitment
/// and provide the first chain link for Transition₁.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InitOutput {
    pub initial_replay_expectation: ReplayExpectation,
}

// === Transition Guest Types ===

/// Status of a Transition guest execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransitionStatus {
    /// More items remain to be verified.
    Continue,
    /// Final item has been verified.
    Complete {
        /// Whether all items verified successfully.
        all_valid: bool,
    },
}

/// Input to the Transition guest.
///
/// Contains the chain link from the previous step, the Replay receipt to verify,
/// and information about the next item in the chain (if any).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitionInput {
    /// What THIS Transition must verify (from Init or previous Transition).
    pub current_replay_expectation: ReplayExpectation,

    /// Serialized Replay receipt to verify.
    pub replay_receipt_bytes: Vec<u8>,

    /// Full packed fingerprint.
    pub fingerprint: Fingerprint,

    /// Next trace item (None if this is the last item).
    pub next_trace_item: Option<TraceItem>,

    /// Next Replay image ID (None if this is the last item).
    pub next_replay_image_id: Option<[u8; 32]>,
}

/// Output from the Transition guest.
///
/// Committed to the journal to record verification results and
/// provide the next chain link (if any).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitionOutput {
    /// The position that was verified.
    pub verified_position: u64,

    /// Hash of the verified trace item.
    pub trace_item_hash: [u8; 32],

    /// Updated tree frontier after adding the item.
    pub new_frontier: SerializableFrontier,

    /// What the next Transition should verify (None if complete).
    pub next_replay_expectation: Option<ReplayExpectation>,

    /// Status: Continue or Complete.
    pub status: TransitionStatus,
}
