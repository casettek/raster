//! Native backend for development and testing.
//!
//! The native backend executes tiles as native code without zkVM overhead.
//! It's useful for development, testing, and debugging but does not produce proofs.

use crate::backend::{Backend, CompilationOutput, ExecutionMode, ResourceEstimate, TileExecution};
use raster_core::{tile::TileMetadata, Error, Result};

/// Native backend that compiles and executes tiles as native code.
///
/// This backend is primarily for development and testing. It does not
/// support proof generation - use the RISC0 backend for that.
pub struct NativeBackend {
    /// Whether to simulate cycle counting (for testing).
    pub simulate_cycles: bool,
}

impl NativeBackend {
    /// Create a new native backend with default settings.
    pub fn new() -> Self {
        Self {
            simulate_cycles: true,
        }
    }

    /// Create a native backend with cycle simulation disabled.
    pub fn without_cycle_simulation() -> Self {
        Self {
            simulate_cycles: false,
        }
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for NativeBackend {
    fn name(&self) -> &'static str {
        "native"
    }

    fn compile_tile(&self, metadata: &TileMetadata, _source_path: &str) -> Result<CompilationOutput> {
        // Native backend doesn't actually compile to a separate binary.
        // The tile is already compiled into the host binary.
        // We return empty artifacts as a placeholder.
        Ok(CompilationOutput {
            elf: Vec::new(),
            method_id: metadata.id.0.as_bytes().to_vec(),
            artifact_dir: None,
        })
    }

    fn execute_tile(
        &self,
        _compilation: &CompilationOutput,
        _input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecution> {
        // Native backend cannot generate proofs
        if mode.generates_proof() {
            return Err(Error::Other(
                "Native backend does not support proof generation. Use the RISC0 backend.".into(),
            ));
        }

        // TODO: Actually execute the tile via the registry
        // For now, return a placeholder result
        let simulated_cycles = if self.simulate_cycles { 1000 } else { 0 };

        Ok(TileExecution::estimate(Vec::new(), simulated_cycles))
    }

    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate> {
        Ok(ResourceEstimate {
            cycles: metadata.estimated_cycles,
            memory_bytes: metadata.max_memory,
        })
    }

    fn verify_receipt(&self, _compilation: &CompilationOutput, _receipt: &[u8]) -> Result<bool> {
        Err(Error::Other(
            "Native backend does not support proof verification. Use the RISC0 backend.".into(),
        ))
    }
}
