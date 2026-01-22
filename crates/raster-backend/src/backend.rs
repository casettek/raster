//! Backend trait and execution mode definitions.

use raster_core::{tile::TileMetadata, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Execution mode for tiles.
///
/// Controls whether proofs are generated and verified during execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Execute without proof generation, just return cycle estimates.
    /// This is the default mode for development and testing.
    #[default]
    Estimate,

    /// Execute with proof generation.
    Prove {
        /// Whether to verify the generated proof.
        verify: bool,
    },
}

impl ExecutionMode {
    /// Create a prove mode with verification disabled.
    pub fn prove() -> Self {
        ExecutionMode::Prove { verify: false }
    }

    /// Create a prove mode with verification enabled.
    pub fn prove_and_verify() -> Self {
        ExecutionMode::Prove { verify: true }
    }

    /// Check if this mode generates proofs.
    pub fn generates_proof(&self) -> bool {
        matches!(self, ExecutionMode::Prove { .. })
    }

    /// Check if this mode verifies proofs.
    pub fn verifies_proof(&self) -> bool {
        matches!(self, ExecutionMode::Prove { verify: true })
    }
}

/// Result of compiling a tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilationOutput {
    /// The compiled guest ELF binary.
    pub elf: Vec<u8>,

    /// The method ID (image ID) for the compiled tile.
    /// This is a unique identifier derived from the ELF.
    pub method_id: Vec<u8>,

    /// Path where artifacts were written (if applicable).
    pub artifact_dir: Option<PathBuf>,
}

/// RISC0's minimum segment size for proving (2^16).
pub const MIN_PROOF_SEGMENT_CYCLES: u64 = 65536;

/// Calculate the proof cycle count (padded to next power of 2, min 2^16).
pub fn calculate_proof_cycles(actual_cycles: u64) -> u64 {
    if actual_cycles <= MIN_PROOF_SEGMENT_CYCLES {
        MIN_PROOF_SEGMENT_CYCLES
    } else {
        // Round up to next power of 2
        actual_cycles.next_power_of_two()
    }
}

/// Result of executing a tile.
#[derive(Debug, Clone)]
pub struct TileExecution {
    /// The serialized output of the tile execution.
    pub output: Vec<u8>,

    /// Actual cycle count for the execution (if available).
    /// This is the real number of cycles the program used.
    pub cycles: Option<u64>,

    /// Proof cycle count (padded to power of 2 for STARK proving).
    /// This represents what proving would actually cost.
    /// Only meaningful for zkVM backends.
    pub proof_cycles: Option<u64>,

    /// The serialized receipt (proof) from execution.
    /// Only present when executed in Prove mode.
    pub receipt: Option<Vec<u8>>,

    /// Whether the proof was verified (if proof was generated).
    pub verified: Option<bool>,
}

impl TileExecution {
    /// Create a new execution result with just output (for estimate mode).
    /// Automatically calculates proof_cycles based on actual cycles.
    pub fn estimate(output: Vec<u8>, cycles: u64) -> Self {
        Self {
            output,
            cycles: Some(cycles),
            proof_cycles: Some(calculate_proof_cycles(cycles)),
            receipt: None,
            verified: None,
        }
    }

    /// Create a new execution result with output and receipt (for prove mode).
    pub fn proved(output: Vec<u8>, cycles: Option<u64>, receipt: Vec<u8>, verified: bool) -> Self {
        let proof_cycles = cycles.map(calculate_proof_cycles);
        Self {
            output,
            cycles,
            proof_cycles,
            receipt: Some(receipt),
            verified: Some(verified),
        }
    }
}

/// Resource estimate for a tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEstimate {
    /// Estimated cycle count for execution.
    pub cycles: Option<u64>,

    /// Estimated memory usage in bytes.
    pub memory_bytes: Option<u64>,
}

/// Trait defining the interface for compilation and execution backends.
///
/// Backends are responsible for:
/// - Compiling tiles into executable artifacts (ELF binaries, etc.)
/// - Executing tiles and returning results
/// - Optionally generating and verifying proofs
pub trait Backend: Send + Sync {
    /// Get the name of this backend.
    fn name(&self) -> &'static str;

    /// Compile a tile into a standalone artifact.
    ///
    /// # Arguments
    /// * `metadata` - Metadata about the tile being compiled
    /// * `source_path` - Path to the source file containing the tile
    ///
    /// # Returns
    /// Compilation output including the ELF binary and method ID.
    fn compile_tile(&self, metadata: &TileMetadata, source_path: &str)
        -> Result<CompilationOutput>;

    /// Execute a tile with the given input.
    ///
    /// # Arguments
    /// * `compilation` - The compilation output from `compile_tile`
    /// * `input` - Serialized input data (bincode format)
    /// * `mode` - Execution mode (estimate or prove)
    ///
    /// # Returns
    /// Execution result including output, cycle count, and optional proof.
    fn execute_tile(
        &self,
        compilation: &CompilationOutput,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecution>;

    /// Estimate resource usage for a tile without executing it.
    ///
    /// This provides a quick estimate based on metadata hints.
    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate>;

    /// Verify a proof receipt.
    ///
    /// # Arguments
    /// * `compilation` - The compilation output (for method ID)
    /// * `receipt` - The serialized receipt to verify
    ///
    /// # Returns
    /// True if the proof is valid, false otherwise.
    fn verify_receipt(&self, compilation: &CompilationOutput, receipt: &[u8]) -> Result<bool>;
}
