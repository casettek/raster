//! BPHC - Bit-Packed Hash Commitment library.
//!
//! This library provides tools for creating compact fingerprints from execution traces
//! and detecting fraud in trace commitments.
//!
//! # Main Components
//!
//! - [`bit_packer::BitPacker`] - Pack hash bits into compact fingerprints
//! - [`trace::ExecutionCommitment`] - Create incremental Merkle commitments
//! - [`guest`] - RISC0 guest types for iterative trace verification
//! - [`error`] - Error types for the library

pub mod bit_packer;
pub mod error;
pub mod guest;
pub mod precomputed;
pub mod trace;
pub mod utils;

pub use error::{BitPackerError, Result};
pub use guest::{
    Fingerprint, InitInput, InitOutput, ReplayExpectation, TransitionInput, TransitionOutput,
    TransitionStatus,
};
