//! RISC0 zkVM backend implementation.

use crate::guest_builder::GuestBuilder;
use raster_backend::{
    Backend, CompilationOutput, ExecutionMode, ResourceEstimate, TileExecution,
};
use raster_core::{tile::TileMetadata, Error, Result};
use risc0_zkvm::{
    default_executor, default_prover, ExecutorEnv, ProverOpts, VerifierContext,
};
use std::fs;
use std::path::PathBuf;

/// RISC0 zkVM backend for compiling and executing tiles with optional proving.
pub struct Risc0Backend {
    /// Directory for build artifacts.
    output_dir: PathBuf,
    /// Path to the user's crate (for building guests).
    user_crate_path: Option<PathBuf>,
    /// Whether to use GPU acceleration for proving.
    use_gpu: bool,
}

impl Risc0Backend {
    /// Create a new RISC0 backend with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            user_crate_path: None,
            use_gpu: false,
        }
    }

    /// Set the path to the user's crate containing tiles.
    pub fn with_user_crate(mut self, path: PathBuf) -> Self {
        self.user_crate_path = Some(path);
        self
    }

    /// Enable GPU acceleration for proving.
    pub fn with_gpu(mut self, enabled: bool) -> Self {
        self.use_gpu = enabled;
        self
    }

    /// Get the guest builder.
    fn guest_builder(&self) -> GuestBuilder {
        let mut builder = GuestBuilder::new(self.output_dir.clone());
        if let Some(ref path) = self.user_crate_path {
            builder = builder.with_user_crate(path.clone());
        }
        builder
    }

    /// Load a compiled ELF from the artifact directory.
    fn load_elf(&self, tile_id: &str) -> Result<Vec<u8>> {
        let builder = self.guest_builder();
        let elf_path = builder.artifact_dir(tile_id).join("guest.elf");
        fs::read(&elf_path).map_err(|e| {
            Error::Other(format!(
                "Failed to load ELF for tile '{}' from {}: {}",
                tile_id,
                elf_path.display(),
                e
            ))
        })
    }
}

impl Backend for Risc0Backend {
    fn name(&self) -> &'static str {
        "risc0"
    }

    fn compile_tile(&self, metadata: &TileMetadata, _source_path: &str) -> Result<CompilationOutput> {
        let tile_id = &metadata.id.0;
        let builder = self.guest_builder();

        // Create a temporary directory for building
        let temp_dir = tempfile::tempdir().map_err(|e| {
            Error::Other(format!("Failed to create temp directory: {}", e))
        })?;

        let guest_dir = temp_dir.path().join(format!("guest-{}", tile_id));
        fs::create_dir_all(&guest_dir).map_err(|e| {
            Error::Other(format!("Failed to create guest directory: {}", e))
        })?;

        // Build the guest
        let elf_path = builder.build_guest(tile_id, &guest_dir).map_err(|e| {
            Error::Other(format!("Failed to build guest for tile '{}': {}", tile_id, e))
        })?;

        // Read the ELF
        let elf = fs::read(&elf_path).map_err(|e| {
            Error::Other(format!("Failed to read ELF: {}", e))
        })?;

        // Compute the image ID (method ID)
        let method_id = risc0_zkvm::compute_image_id(&elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        // Write artifacts to output directory
        let artifact_dir = builder.write_artifacts(tile_id, &elf, method_id.as_bytes())
            .map_err(|e| Error::Other(format!("Failed to write artifacts: {}", e)))?;

        Ok(CompilationOutput {
            elf,
            method_id: method_id.as_bytes().to_vec(),
            artifact_dir: Some(artifact_dir),
        })
    }

    fn execute_tile(
        &self,
        compilation: &CompilationOutput,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecution> {
        // Build the executor environment with the input
        let env = ExecutorEnv::builder()
            .write_slice(input)
            .build()
            .map_err(|e| Error::Other(format!("Failed to build executor env: {}", e)))?;

        match mode {
            ExecutionMode::Estimate => {
                // Execute without proving - just get cycle count
                let executor = default_executor();
                let session = executor
                    .execute(env, &compilation.elf)
                    .map_err(|e| Error::Other(format!("Execution failed: {}", e)))?;

                // Get the journal (output)
                let output = session.journal.bytes.clone();
                let cycles = session.cycles();

                Ok(TileExecution::estimate(output, cycles))
            }
            ExecutionMode::Prove { verify } => {
                // Execute with proving
                let prover = default_prover();
                let prove_info = prover
                    .prove(env, &compilation.elf)
                    .map_err(|e| Error::Other(format!("Proving failed: {}", e)))?;

                let receipt = prove_info.receipt;
                let output = receipt.journal.bytes.clone();

                // Get cycle count from the receipt if available
                let cycles = prove_info.stats.total_cycles;

                // Optionally verify
                let verified = if verify {
                    let image_id = risc0_zkvm::compute_image_id(&compilation.elf)
                        .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

                    receipt
                        .verify(image_id)
                        .map(|_| true)
                        .unwrap_or(false)
                } else {
                    false
                };

                // Serialize the receipt
                let receipt_bytes = bincode::serialize(&receipt)
                    .map_err(|e| Error::Other(format!("Failed to serialize receipt: {}", e)))?;

                Ok(TileExecution::proved(
                    output,
                    Some(cycles),
                    receipt_bytes,
                    verified,
                ))
            }
        }
    }

    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate> {
        Ok(ResourceEstimate {
            cycles: metadata.estimated_cycles,
            memory_bytes: metadata.max_memory,
        })
    }

    fn verify_receipt(&self, compilation: &CompilationOutput, receipt_bytes: &[u8]) -> Result<bool> {
        let receipt: risc0_zkvm::Receipt = bincode::deserialize(receipt_bytes)
            .map_err(|e| Error::Other(format!("Failed to deserialize receipt: {}", e)))?;

        let image_id = risc0_zkvm::compute_image_id(&compilation.elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        Ok(receipt.verify(image_id).is_ok())
    }
}
