//! Backend trait and execution mode definitions.

use raster_core::{tile::TileMetadata, Result};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::{Path, PathBuf};

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
/// 
/// NOTE: This struct is deprecated and will be removed. Use the Executable trait instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileExecDescriptor {
    /// The compiled guest ELF binary.
    pub elf: Vec<u8>,

    /// The method ID (image ID) for the compiled tile.
    /// This is a unique identifier derived from the ELF.
    pub method_id: Vec<u8>,

    /// The tile ID this compilation is for.
    #[serde(default)]
    pub tile_id: String,

    /// Path where artifacts were written (if applicable).
    pub artifact_dir: Option<PathBuf>,
    pub exec_path: Option<PathBuf>,
}

/// Opaque executable descriptor - each backend defines its own concrete type.
/// 
/// This trait provides a common interface for executables across different backends.
/// Backends define their own concrete types (e.g., NativeExecutable, Risc0Executable)
/// that implement this trait.
pub trait CompilationArtifact: Send + Sync + Any {
    /// Get the tile ID this executable is for.
    fn tile_id(&self) -> &str;

    /// Get the artifact directory (if persisted).
    fn artifact_dir(&self) -> Option<&Path>;

    /// Downcast to concrete type (for backend internal use).
    fn as_any(&self) -> &dyn Any;

    /// Downcast to mutable concrete type (for backend internal use).
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Artifact persistence interface - each backend implements its own.
/// 
/// The ArtifactStore trait provides a common interface for persisting and loading
/// backend-specific executable artifacts. Each backend implements this to handle
/// its own artifact format (e.g., native binaries vs zkVM ELF files).
pub trait ArtifactStore: Send + Sync {
    /// Save an executable to disk.
    /// 
    /// # Arguments
    /// * `executable` - The executable to save
    /// * `output_dir` - The base output directory
    /// * `source_hash` - Optional hash of the source file for cache invalidation
    /// 
    /// # Returns
    /// The artifact directory path where artifacts were written.
    fn save(
        &self,
        executable: &dyn CompilationArtifact,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Result<PathBuf>;

    /// Load a cached executable if valid.
    /// 
    /// # Arguments
    /// * `tile_id` - The tile ID to load
    /// * `output_dir` - The base output directory
    /// * `source_hash` - Optional hash to validate against cached artifacts
    /// 
    /// # Returns
    /// The loaded executable, or None if not cached or cache is stale.
    fn load(
        &self,
        tile_id: &str,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Option<Box<dyn CompilationArtifact>>;

    /// Check if recompilation is needed.
    /// 
    /// # Arguments
    /// * `tile_id` - The tile ID to check
    /// * `output_dir` - The base output directory
    /// * `source_hash` - Optional hash to validate against cached artifacts
    /// 
    /// # Returns
    /// True if recompilation is needed, false if cache is valid.
    fn needs_recompilation(
        &self,
        tile_id: &str,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> bool;
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
pub struct TileExecutionResult {
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

impl TileExecutionResult {
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

    /// Compile a tile into an executable descriptor.
    ///
    /// # Arguments
    /// * `metadata` - Metadata about the tile being compiled
    /// * `source_path` - Path to the source file containing the tile
    ///
    /// # Returns
    /// An opaque executable that can be passed to execute_tile.
    fn compile_tile(
        &self,
        metadata: &TileMetadata,
        source_path: &str,
    ) -> Result<Box<dyn CompilationArtifact>>;

    /// Execute a tile with the given input.
    ///
    /// # Arguments
    /// * `executable` - The executable from `prepare_tile_execute`
    /// * `input` - Serialized input data (bincode format)
    /// * `mode` - Execution mode (estimate or prove)
    ///
    /// # Returns
    /// Execution result including output, cycle count, and optional proof.
    fn execute_tile(
        &self,
        compilation_artifact: &dyn CompilationArtifact,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecutionResult>;

    /// Get the artifact store for this backend.
    /// 
    /// The artifact store handles persistence and caching of compiled artifacts.
    fn artifact_store(&self) -> &dyn ArtifactStore;

    /// Estimate resource usage for a tile without executing it.
    ///
    /// This provides a quick estimate based on metadata hints.
    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate>;

    /// Verify a proof receipt.
    ///
    /// # Arguments
    /// * `executable` - The executable (for method ID/verification key)
    /// * `receipt` - The serialized receipt to verify
    ///
    /// # Returns
    /// True if the proof is valid, false otherwise.
    fn verify_receipt(&self, executable: &dyn CompilationArtifact, receipt: &[u8]) -> Result<bool>;
}
