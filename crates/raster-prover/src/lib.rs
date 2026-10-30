//! BPHC - Bit-Packed Hash Commitment library.
//!
//! This library provides tools for creating compact fingerprints from execution traces
//! and detecting fraud in trace commitments.
//!
//! # Main Components
//!
//! - [`bit_packer::BitPacker`] - Pack hash bits into compact fingerprints
//! - [`trace::TraceCommitment`] - Create incremental Merkle commitments
//! - [`guest`] - RISC0 guest types for iterative trace verification
//! - [`error`] - Error types for the library

include!(concat!(env!("OUT_DIR"), "/methods.rs"));

pub mod archive;
pub mod authorization;
pub mod availability;
pub mod chain_fraud;
pub mod error;
pub mod replay;
pub mod trace;
pub mod transition;
pub mod utils;

pub use error::{BitPackerError, Result};

pub mod commitment_file;

pub mod compression;
