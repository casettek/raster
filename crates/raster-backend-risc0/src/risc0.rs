//! RISC0 zkVM backend implementation.

use rayon::prelude::*;

use crate::guest_builder::GuestBuilder;
use raster_backend::backend::HexString;
use raster_backend::{
    ArtifactStore, Backend, CompilationArtifact, ExecutionMode, ResourceEstimate,
    TileExecutionResult,
};
use raster_core::postcard;
use risc0_zkvm::{default_executor, ExecutorEnv};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fs;
use std::path::{Path, PathBuf};

use raster_core::{tile::TileMetadata, Error, Result};

/// RISC0 executable - ELF binary and image ID.
#[derive(Debug, Clone)]
pub struct Risc0CompilationArtifact {
    /// The compiled guest ELF binary.
    pub elf: Vec<u8>,
    /// The image ID for the compiled tile.
    pub image_id: HexString,
    /// The tile ID this executable is for.
    pub tile_id: String,
}

impl CompilationArtifact for Risc0CompilationArtifact {
    fn id(&self) -> &str {
        &self.tile_id
    }

    fn artifact_id(&self) -> HexString {
        self.image_id.clone()
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
    image_id: String,
    source_hash: Option<String>,
}

/// RISC0 artifact store - persists ELF and method_id.
pub struct Risc0ArtifactStore;

impl Risc0ArtifactStore {
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
        artifact: &dyn CompilationArtifact,
        output_dir: &Path,
        source_hash: Option<String>,
    ) -> Result<PathBuf> {
        let risc0_artifact = artifact
            .as_any()
            .downcast_ref::<Risc0CompilationArtifact>()
            .ok_or_else(|| Error::Other("Invalid artifact type for Risc0ArtifactStore".into()))?;

        let artifact_dir = output_dir
            .join("tiles")
            .join(&risc0_artifact.tile_id)
            .join("risc0");

        fs::create_dir_all(&artifact_dir).map_err(Error::Io)?;

        let image_id = &risc0_artifact.image_id;

        fs::write(artifact_dir.join(format!("{}.elf", source_hash.as_deref().unwrap_or("none"))), &risc0_artifact.elf)
            .map_err(Error::Io)?;

        fs::write(artifact_dir.join("image_id"), image_id).map_err(Error::Io)?;

        // Write manifest
        let manifest = Risc0Manifest {
            tile_id: risc0_artifact.tile_id.clone(),
            image_id: image_id.clone(),
            source_hash,
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
        source_hash: Option<String>,
    ) -> Option<Box<dyn CompilationArtifact>> {
        let artifact_dir = output_dir.join("tiles").join(tile_id).join("risc0");

        let manifest_content = fs::read_to_string(artifact_dir.join("manifest.json")).ok()?;
        let manifest: Risc0Manifest = serde_json::from_str(&manifest_content).ok()?;

        let elf_path = fs::read_dir(&artifact_dir)
            .ok()?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "elf"))?;

        let elf_name = elf_path.file_name()?.to_str().map(String::from);
        let elf = fs::read(elf_path).ok()?;

        let compiled_source_hash = elf_name;

        // Validate source hash
        // TODO: no additional manifest required for validating source hash just check file name
        if manifest.source_hash != source_hash {
            return None;
        }
        if compiled_source_hash != source_hash {
            return None;
        }

        Some(Box::new(Risc0CompilationArtifact {
            elf,
            image_id: manifest.image_id,
            tile_id: tile_id.to_string(),
        }))
    }
}

/// RISC0 zkVM backend for compiling and executing tiles with optional proving.
pub struct Risc0Backend {
    /// Directory for build artifacts.
    output_dir: PathBuf,
    /// Path to the user's crate (for building guests).
    user_crate_path: Option<PathBuf>,
    /// Artifact store for caching compiled artifacts.
    artifact_store: Risc0ArtifactStore,
}

impl Risc0Backend {
    /// Create a new RISC0 backend with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            user_crate_path: None,
            artifact_store: Risc0ArtifactStore::new(),
        }
    }

    /// Set the path to the user's crate containing tiles.
    pub fn with_user_crate(mut self, path: PathBuf) -> Self {
        self.user_crate_path = Some(path);
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

    fn compile_tile(
        &self,
        metadata: &TileMetadata,
        content_hash: Option<String>,
    ) -> Result<Box<dyn CompilationArtifact>> {
        let tile_id = &metadata.id.0;

        if let Some(cached) =
            self.artifact_store
                .load(tile_id, &self.output_dir, content_hash.clone())
        {
            return Ok(cached);
        }

        // Not cached, need to build
        let builder = self.guest_builder();

        // Create a temporary directory for building
        let temp_dir = tempfile::tempdir()
            .map_err(|e| Error::Other(format!("Failed to create temp directory: {}", e)))?;

        let guest_dir = temp_dir.path().join(format!("guest-{}", tile_id));
        fs::create_dir_all(&guest_dir)
            .map_err(|e| Error::Other(format!("Failed to create guest directory: {}", e)))?;

        // Build the guest
        let elf_path = builder.build_guest(tile_id, &guest_dir).map_err(|e| {
            Error::Other(format!(
                "Failed to build guest for tile '{}': {}",
                tile_id, e
            ))
        })?;

        // Read the ELF
        let elf =
            fs::read(&elf_path).map_err(|e| Error::Other(format!("Failed to read ELF: {}", e)))?;

        // Compute the image ID (method ID)
        let image_id = risc0_zkvm::compute_image_id(&elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        let image_id = hex::encode(image_id);

        let artifact = Risc0CompilationArtifact {
            elf,
            image_id,
            tile_id: tile_id.to_string(),
        };

        self.artifact_store
            .save(&artifact, &self.output_dir, content_hash)?;

        Ok(Box::new(artifact))
    }

    fn execute_tile(
        &self,
        compilation_artifact: &dyn CompilationArtifact,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecutionResult> {
        // Downcast to Risc0Executable
        let risc0 = compilation_artifact
            .as_any()
            .downcast_ref::<Risc0CompilationArtifact>()
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
                let prover = risc0_zkvm::default_prover();
                let prove_info = prover
                    .prove(env, &risc0.elf)
                    .map_err(|e| Error::Other(format!("Proving failed: {}", e)))?;

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
                let receipt_bytes = postcard::to_allocvec(&receipt)
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

    fn verify_receipt(
        &self,
        artifact: &dyn CompilationArtifact,
        receipt_bytes: &[u8],
    ) -> Result<bool> {
        // Downcast to Risc0Executable
        let risc0 = artifact
            .as_any()
            .downcast_ref::<Risc0CompilationArtifact>()
            .ok_or_else(|| Error::Other("Expected Risc0Executable".into()))?;

        let receipt: risc0_zkvm::Receipt = postcard::from_bytes(receipt_bytes)
            .map_err(|e| Error::Other(format!("Failed to deserialize receipt: {}", e)))?;

        let image_id = risc0_zkvm::compute_image_id(&risc0.elf)
            .map_err(|e| Error::Other(format!("Failed to compute image ID: {}", e)))?;

        Ok(receipt.verify(image_id).is_ok())
    }
}
