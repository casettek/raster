//! RISC0 guest types and logic for iterative trace verification.
//!
//! This module provides the types and logic for the three-guest proving pipeline:
//!
//! 1. **Init Guest**: Creates the initial commitment and produces the first chain link
//! 2. **Replay Guest**: Re-executes a tile (tile-specific, uses existing `guest_builder.rs`)
//! 3. **Transition Guest**: Verifies Replay proofs, updates the Merkle tree, and chains
//!
//! # Architecture
//!
//! ```text
//! Init → Transition₁ → Transition₂ → ... → TransitionN
//!            ↑              ↑                   ↑
//!         Replay₁        Replay₂            ReplayN
//! ```
//!
//! Each Transition guest verifies a Replay guest's output against the expected
//! trace item, accumulates the result into a BridgeTree, and validates against
//! the packed fingerprint.
//!
//! # Replay Guests
//!
//! Replay guests are tile-specific and generated dynamically using the existing
//! [`guest_builder`](crate::guest_builder) module in `raster-backend-risc0`.
//! The Transition guest verifies Replay receipts via their image ID, ensuring
//! the correct tile was executed.

pub mod types;

#[cfg(feature = "guest")]
pub mod init;

#[cfg(feature = "guest")]
pub mod transition;

pub mod replay;

// Re-export commonly used types
pub use types::{
    Fingerprint, InitInput, InitOutput, ReplayExpectation, TransitionInput, TransitionOutput,
    TransitionStatus,
};
