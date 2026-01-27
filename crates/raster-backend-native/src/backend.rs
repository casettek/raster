//! Native backend for development and testing.
//!
//! The native backend executes tiles as native code without zkVM overhead.
//! It's useful for development, testing, and debugging but does not produce proofs.
//!
//! Native execution works via subprocess: the CLI builds the user's project
//! and runs it with special arguments to execute a specific tile.

use raster_backend::{
    ArtifactStore, Backend, CompilationArtifact, ExecutionMode, ResourceEstimate, TileExecutionResult
};
use raster_core::{Error, Result, tile::TileMetadata};

use std::{any::Any, fs, path::{Path, PathBuf}, process::Command};
use serde::{Deserialize, Serialize};

/// Native executable - just a path to the compiled binary.
#[derive(Debug, Clone)]
pub struct NativeCompilationArtifact {
    /// Path to the compiled binary.
    pub binary_path: PathBuf,
    /// The tile ID this executable is for.
    pub tile_id: String,
}

impl CompilationArtifact for NativeCompilationArtifact {
    fn id(&self) -> &str {
        &self.tile_id
    }

    fn path(&self) -> &Path {
        self.binary_path.parent().unwrap()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Manifest for native artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NativeManifest {
    tile_id: String,
    binary_path: PathBuf,
    source_hash: Option<String>,
}

/// Native artifact store - persists binary path and manifest.
pub struct NativeArtifactStore;

impl ArtifactStore for NativeArtifactStore {
    fn save(
        &self,
        artifact: &dyn CompilationArtifact,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Result<PathBuf> {
        let native_artifact = artifact
            .as_any()
            .downcast_ref::<NativeCompilationArtifact>()
            .ok_or_else(|| Error::Other("Invalid executable type for NativeArtifactStore".into()))?;

        let artifact_dir = output_dir
            .join("tiles")
            .join(&native_artifact.tile_id)
            .join("native");

        fs::create_dir_all(&artifact_dir).map_err(Error::Io)?;

        // Write manifest with binary path reference
        let manifest = NativeManifest {
            tile_id: native_artifact.tile_id.clone(),
            binary_path: native_artifact.binary_path.clone(),
            source_hash: source_hash.map(String::from),
        };

        let manifest_path = artifact_dir.join("manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        fs::write(&manifest_path, manifest_json).map_err(Error::Io)?;

        Ok(artifact_dir)
    }

    fn load(
        &self,
        tile_id: &str,
        output_dir: &Path,
        source_hash: Option<&str>,
    ) -> Option<Box<dyn CompilationArtifact>> {
        let artifact_dir = output_dir.join("tiles").join(tile_id).join("native");

        let manifest_content = fs::read_to_string(artifact_dir.join("manifest.json")).ok()?;
        let manifest: NativeManifest = serde_json::from_str(&manifest_content).ok()?;

        // Validate source hash
        if manifest.source_hash.as_deref() != source_hash {
            return None;
        }

        // Check binary still exists
        if !manifest.binary_path.exists() {
            return None;
        }

        Some(Box::new(NativeCompilationArtifact {
            binary_path: manifest.binary_path,
            tile_id: tile_id.to_string(),
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

/// Native backend that compiles and executes tiles as native code.
///
/// This backend is primarily for development and testing. It does not
/// support proof generation - use the RISC0 backend for that.
///
/// Native execution works via subprocess: the user's project binary is
/// invoked with special arguments to execute a specific tile.
pub struct NativeBackend {
    /// Whether to simulate cycle counting (for testing).
    pub simulate_cycles: bool,
    /// Path to the user's project (for building).
    project_path: Option<PathBuf>,
    /// Artifact store for caching compiled binaries.
    artifact_store: NativeArtifactStore,
}

impl NativeBackend {
    /// Create a new native backend with default settings.
    pub fn new() -> Self {
        Self {
            simulate_cycles: true,
            project_path: None,
            artifact_store: NativeArtifactStore,
        }
    }

    /// Create a native backend with cycle simulation disabled.
    pub fn without_cycle_simulation() -> Self {
        Self {
            simulate_cycles: false,
            project_path: None,
            artifact_store: NativeArtifactStore,
        }
    }

    /// Set the project path for building.
    pub fn with_project_path(mut self, path: PathBuf) -> Self {
        self.project_path = Some(path);
        self
    }

    /// Find the Cargo target directory for a project.
    /// Handles both workspace members and standalone projects.
    fn find_target_path(project_path: &std::path::Path) -> Option<std::path::PathBuf> {
        // Run cargo metadata to get the target directory
        let output = std::process::Command::new("cargo")
            .current_dir(project_path)
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8(output.stdout).ok()?;
        let meta: serde_json::Value = serde_json::from_str(&stdout).ok()?;

        meta.get("target_directory")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
    }

    /// Extract the binary name from a Cargo.toml file.
    fn extract_binary_name(project_path: &std::path::Path) -> Option<String> {
        let cargo_toml = std::fs::read_to_string(project_path.join("Cargo.toml")).ok()?;
        
        // Simple parsing: look for name = "..." in [package] section
        let mut in_package = false;
        for line in cargo_toml.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package = true;
                continue;
            }
            if trimmed.starts_with('[') {
                in_package = false;
                continue;
            }
            if in_package && trimmed.starts_with("name") {
                if let Some(start) = line.find('"') {
                    let rest = &line[start + 1..];
                    if let Some(end) = rest.find('"') {
                        return Some(rest[..end].to_string());
                    }
                }
            }
        }
        None
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

    fn compile_tile(&self, tile_metadata: &TileMetadata, _content_hash: Option<&str>) -> Result<Box<dyn CompilationArtifact>> {
        // Native backend builds the user's project
        let project_path = self.project_path.as_ref().ok_or_else(|| {
            Error::Other("Native backend requires project_path to be set".into())
        })?;

        // Build the project
        let output = Command::new("cargo")
            .current_dir(project_path)
            .args(["build", "--release"])
            .output()
            .map_err(|e| Error::Other(format!("Failed to run cargo build: {}", e)))?;

        println!("cargo build output: {:?}", output);

        if !output.status.success() {
            return Err(Error::Other(format!(
                "cargo build failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Find the binary path
        let binary_name = Self::extract_binary_name(project_path).ok_or_else(|| {
            Error::Other("Could not determine binary name from Cargo.toml".into())
        })?;

        let target_dir =
            Self::find_target_path(project_path).unwrap_or_else(|| project_path.join("target"));
        let binary_path = target_dir.join("release").join(&binary_name);



        if !binary_path.exists() {
            return Err(Error::Other(format!(
                "Binary not found at: {}",
                binary_path.display()
            )));
        }


        Ok(Box::new(NativeCompilationArtifact {
            binary_path,
            tile_id: tile_metadata.id.0.clone(),
        }))
    }

    fn execute_tile(
        &self,
        compilation_artifact: &dyn CompilationArtifact,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecutionResult> {
        // Downcast to NativeExecutable
        let native = compilation_artifact 
            .as_any()
            .downcast_ref::<NativeCompilationArtifact>()
            .ok_or_else(|| Error::Other("Expected NativeExecutable".into()))?;

        // Native backend cannot generate proofs
        if mode.generates_proof() {
            return Err(Error::Other(
                "Native backend does not support proof generation. Use the RISC0 backend.".into(),
            ));
        }

        // Encode input as base64
        let input_b64 = base64_encode(input);

        // Run the binary with --raster-exec arguments
        let output = Command::new(&native.binary_path)
            .args(["--raster-exec", &native.tile_id, "--input", &input_b64])
            .output()
            .map_err(|e| {
                Error::Other(format!("Failed to execute binary: {}", e))
            })?;

        println!("output: {:#?}", output);
        println!("output stdout: {:#?} {:?}", output, output.stdout);
        if !output.status.success() {
            return Err(Error::Other(format!(
                "Tile execution failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // Parse output - look for RASTER_OUTPUT: line
        let stdout = String::from_utf8_lossy(&output.stdout);
        let output_b64 = stdout
            .lines()
            .find(|l| l.starts_with("RASTER_OUTPUT:"))
            .map(|l| l.trim_start_matches("RASTER_OUTPUT:"))
            .ok_or_else(|| {
                Error::Other(format!(
                    "No RASTER_OUTPUT found in tile output. stdout: {}",
                    stdout
                ))
            })?;

        let output_bytes = base64_decode(output_b64)
            .map_err(|e| Error::Other(format!("Failed to decode output: {}", e)))?;

        // Return result with simulated cycles (native doesn't have real cycle counts)
        let simulated_cycles = if self.simulate_cycles { 1000 } else { 0 };
        Ok(TileExecutionResult::estimate(output_bytes, simulated_cycles))
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

    fn verify_receipt(&self, _executable: &dyn CompilationArtifact, _receipt: &[u8]) -> Result<bool> {
        Err(Error::Other(
            "Native backend does not support proof verification. Use the RISC0 backend.".into(),
        ))
    }
}

/// Encode bytes as base64 string.
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Decode base64 string to bytes.
fn base64_decode(data: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data)
}
