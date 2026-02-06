//! Trace replay and verification for the Raster toolchain.
//!
//! This crate provides:
//!
//! - `TraceReplayer`: Replays individual trace items on a backend with proof generation
//! - `SequenceProver`: Recursive multi-guest proving with fingerprint accumulation
//!
//! # Sequence Proving
//!
//! The `SequenceProver` provides recursive ZK proofs for trace sequences:
//!
//! ```ignore
//! use raster_tracing::{SequenceProver, SequenceProverConfig};
//!
//! let prover = SequenceProver::new(SequenceProverConfig::default());
//! let inputs = prover.prepare_from_audit(&audit_result)?;
//!
//! // Use with Risc0Backend::prove_sequence()
//! let result = backend.prove_sequence(
//!     &inputs.trace_items,
//!     inputs.input_frontier,
//!     inputs.reference_fingerprint,
//!     inputs.bits_per_item,
//! )?;
//! ```

mod replayer;

pub use replayer::{ReplayResult, TraceReplayer};
