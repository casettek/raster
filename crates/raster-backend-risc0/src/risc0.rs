//! RISC0 zkVM backend implementation.

use crate::guest_builder::GuestBuilder;
use raster_backend::{
    ArtifactStore, Backend, Executable, ExecutionMode, ResourceEstimate, TileExecutionResult,
};
use raster_core::{tile::TileMetadata, Error, Result};
use risc0_zkvm::{default_executor, ExecutorEnv, ProverOpts};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fs;
use std::path::{Path, PathBuf};

/// Check if Metal GPU acceleration is available (compile-time + runtime).
pub fn is_metal_available() -> bool {
    #[cfg(all(feature = "metal", target_os = "macos"))]
    {
        // Metal feature is enabled and we're on macOS
        true
    }
    #[cfg(not(all(feature = "metal", target_os = "macos")))]
    {
        false
    }
}

/// Check if CUDA GPU acceleration is available (compile-time).
pub fn is_cuda_available() -> bool {
    #[cfg(feature = "cuda")]
    {
        true
    }
    #[cfg(not(feature = "cuda"))]
    {
        false
    }
}

/// Check if any GPU acceleration is available.
pub fn is_gpu_available() -> bool {
    is_metal_available() || is_cuda_available()
}

/// RISC0 executable - ELF binary and method ID.
#[derive(Debug, Clone)]
pub struct Risc0Executable {
    /// The compiled guest ELF binary.
    pub elf: Vec<u8>,
    /// The method ID (image ID) for the compiled tile.
    pub method_id: Vec<u8>,
    /// The tile ID this executable is for.
    pub tile_id: String,
    /// Path where artifacts were written (if applicable).
    pub artifact_dir: Option<PathBuf>,
}

impl Executable for Risc0Executable {
    fn tile_id(&self) -> &str {
        &self.tile_id
    }

    fn artifact_dir(&self) -> Option<&Path> {
        self.artifact_dir.as_deref()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Manifest for RISC0 artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Risc0Manifest {
    tile_id: String,
    method_id: String,
    elf_size: usize,
    source_hash: Option<String>,
}

/// RISC0 artifact store - persists ELF and method_id.
pub struct Risc0ArtifactStore;

impl Risc0ArtifactStore {
    /// Create a new RISC0 artifact store.
    pub fn new() -> Self {
        Self
    }
}

impl Default for Risc0ArtifactStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactStore for Risc0ArtifactStore {
    fn save(
        &self,
        executable: &dyn Executable,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Result<PathBuf> {
        let risc0 = executable
            .as_any()
            .downcast_ref::<Risc0Executable>()
            .ok_or_else(|| Error::Other("Invalid executable type for Risc0ArtifactStore".into()))?;

        let artifact_dir = output_dir
            .join("tiles")
            .join(&risc0.tile_id)
            .join("risc0");

        fs::create_dir_all(&artifact_dir).map_err(Error::Io)?;

        // Write ELF
        fs::write(artifact_dir.join("guest.elf"), &risc0.elf).map_err(Error::Io)?;

        // Write method ID (hex encoded)
        fs::write(artifact_dir.join("method_id"), hex::encode(&risc0.method_id)).map_err(Error::Io)?;

        // Write manifest
        let manifest = Risc0Manifest {
            tile_id: risc0.tile_id.clone(),
            method_id: hex::encode(&risc0.method_id),
            elf_size: risc0.elf.len(),
            source_hash: source_hash.map(String::from),
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        fs::write(artifact_dir.join("manifest.json"), manifest_json).map_err(Error::Io)?;

        Ok(artifact_dir)
    }

    fn load(
        &self,
        tile_id: &str,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Option<Box<dyn Executable>> {
        let artifact_dir = output_dir.join("tiles").join(tile_id).join("risc0");

        let manifest_content = fs::read_to_string(artifact_dir.join("manifest.json")).ok()?;
        let manifest: Risc0Manifest = serde_json::from_str(&manifest_content).ok()?;

        // Validate source hash
        if manifest.source_hash.as_deref() != source_hash {
            return None;
        }

        let elf = fs::read(artifact_dir.join("guest.elf")).ok()?;
        let method_id = hex::decode(&manifest.method_id).ok()?;

        Some(Box::new(Risc0Executable {
            elf,
            method_id,
            tile_id: tile_id.to_string(),
            artifact_dir: Some(artifact_dir),
        }))
    }

    fn needs_recompilation(
        &self,
        tile_id: &str,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> bool {
        self.load(tile_id, output_dir, source_hash).is_none()
    }
}

/// RISC0 zkVM backend for compiling and executing tiles with optional proving.
pub struct Risc0Backend {
    /// Directory for build artifacts.
    output_dir: PathBuf,
    /// Path to the user's crate (for building guests).
    user_crate_path: Option<PathBuf>,
    /// Whether to use GPU acceleration for proving.
    use_gpu: bool,
    /// Artifact store for caching compiled artifacts.
    artifact_store: Risc0ArtifactStore,
}

impl Risc0Backend {
    /// Create a new RISC0 backend with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            user_crate_path: None,
            use_gpu: false,
            artifact_store: Risc0ArtifactStore::new(),
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
    #[allow(dead_code)]
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

    fn prepare_tile_executable(
        &self,
        metadata: &TileMetadata,
        _source_path: &str,
    ) -> Result<Box<dyn Executable>> {
        let tile_id = &metadata.id.0;
        let builder = self.guest_builder();

        // Create a temporary directory for building
        let temp_dir = tempfile::tempdir()
            .map_err(|e| Error::Other(format!("Failed to create temp directory: {}", e)))?;

        let guest_dir = temp_dir.path().join(format!("guest-{}", tile_id));
        fs::create_dir_all(&guest_dir)
            .map_err(|e| Error::Other(format!("Failed to create guest directory: {}", e)))?;

        // Build the guest
        let elf_path = builder
            .build_guest(tile_id, &guest_dir)
            .map_err(|e| Error::Other(format!("Failed to build guest for tile '{}': {}", tile_id, e)))?;

        // Read the ELF
        let elf =
            fs::read(&elf_path).map_err(|e| Error::Other(format!("Failed to read ELF: {}", e)))?;

        // Compute the image ID (method ID)
        let method_id = risc0_zkvm::compute_image_id(&elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        // Write artifacts to output directory
        let artifact_dir = builder
            .write_artifacts(tile_id, &elf, method_id.as_bytes())
            .map_err(|e| Error::Other(format!("Failed to write artifacts: {}", e)))?;

        Ok(Box::new(Risc0Executable {
            elf,
            method_id: method_id.as_bytes().to_vec(),
            tile_id: tile_id.to_string(),
            artifact_dir: Some(artifact_dir),
        }))
    }

    fn execute_tile(
        &self,
        executable: &dyn Executable,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecutionResult> {
        // Downcast to Risc0Executable
        let risc0 = executable
            .as_any()
            .downcast_ref::<Risc0Executable>()
            .ok_or_else(|| Error::Other("Expected Risc0Executable".into()))?;

        // Build the executor environment with the input
        // Write length first, then raw bytes (guest expects this format)
        let input_len = input.len() as u32;
        let env = ExecutorEnv::builder()
            .write(&input_len)
            .map_err(|e| Error::Other(format!("Failed to write input length: {}", e)))?
            .write_slice(input)
            .build()
            .map_err(|e| Error::Other(format!("Failed to build executor env: {}", e)))?;

        match mode {
            ExecutionMode::Estimate => {
                // Execute without proving - just get cycle count
                let executor = default_executor();
                let session = executor
                    .execute(env, &risc0.elf)
                    .map_err(|e| Error::Other(format!("Execution failed: {}", e)))?;

                // Get the journal (output)
                let output = session.journal.bytes.clone();
                let cycles = session.cycles();

                Ok(TileExecutionResult::estimate(output, cycles))
            }
            ExecutionMode::Prove { verify } => {
                // Execute with proving, optionally using GPU acceleration
                let prove_info = if self.use_gpu && is_gpu_available() {
                    // Use GPU-accelerated prover
                    let opts = ProverOpts::default();
                    let prover = risc0_zkvm::default_prover();
                    prover
                        .prove_with_opts(env, &risc0.elf, &opts)
                        .map_err(|e| Error::Other(format!("GPU proving failed: {}", e)))?
                } else {
                    // Use default CPU prover
                    let prover = risc0_zkvm::default_prover();
                    prover
                        .prove(env, &risc0.elf)
                        .map_err(|e| Error::Other(format!("Proving failed: {}", e)))?
                };

                let receipt = prove_info.receipt;
                let output = receipt.journal.bytes.clone();

                // Get cycle count from the receipt if available
                let cycles = prove_info.stats.total_cycles;

                // Optionally verify
                let verified = if verify {
                    let image_id = risc0_zkvm::compute_image_id(&risc0.elf)
                        .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

                    receipt.verify(image_id).map(|_| true).unwrap_or(false)
                } else {
                    false
                };

                // Serialize the receipt
                let receipt_bytes = bincode::serialize(&receipt)
                    .map_err(|e| Error::Other(format!("Failed to serialize receipt: {}", e)))?;

                Ok(TileExecutionResult::proved(
                    output,
                    Some(cycles),
                    receipt_bytes,
                    verified,
                ))
            }
        }
    }

    fn artifact_store(&self) -> &dyn ArtifactStore {
        &self.artifact_store
    }

    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate> {
        Ok(ResourceEstimate {
            cycles: metadata.estimated_cycles,
            memory_bytes: metadata.max_memory,
        })
    }

    fn verify_receipt(&self, executable: &dyn Executable, receipt_bytes: &[u8]) -> Result<bool> {
        // Downcast to Risc0Executable
        let risc0 = executable
            .as_any()
            .downcast_ref::<Risc0Executable>()
            .ok_or_else(|| Error::Other("Expected Risc0Executable".into()))?;

        let receipt: risc0_zkvm::Receipt = bincode::deserialize(receipt_bytes)
            .map_err(|e| Error::Other(format!("Failed to deserialize receipt: {}", e)))?;

        let image_id = risc0_zkvm::compute_image_id(&risc0.elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        Ok(receipt.verify(image_id).is_ok())
    }
}
