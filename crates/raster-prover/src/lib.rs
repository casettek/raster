//! BPHC - Bit-Packed Hash Commitment library.
//!
//! This library provides tools for creating compact fingerprints from execution traces
//! and detecting fraud in trace commitments.
//!
//! # Main Components
//!
//! - [`trace::TraceCommitment`] - Create incremental Merkle commitments
//! - [`fraud_window`] - Prove a fraud window: every part's segments, then the
//!   FRAUD WINDOW aggregator whose receipt is the fraud proof
//! - [`error`] - Error types for the library

// The FRAUD WINDOW guest, built here in the second pass of the guest build.
include!(concat!(env!("OUT_DIR"), "/methods.rs"));
// Every other guest, built in the first pass.
pub use raster_prover_guests::*;

pub mod authorization;
pub mod availability;
pub mod chain_fraud;
pub mod control_flow;
pub mod error;
pub mod fraud_window;
pub mod replay;
pub mod schedule;
pub mod segments;
pub mod storage;
pub mod trace;
pub mod trace_segments;
pub mod transition;
pub mod utils;
pub mod window;

pub use error::{BitPackerError, Result};
