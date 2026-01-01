//! Build orchestration for Raster projects.

use crate::discovery::{DiscoveredTile, TileDiscovery};
use raster_backend::{Backend, CompilationOutput, NativeBackend};
use raster_core::{
    manifest::Manifest,
    registry::{iter_tiles, TileRegistration},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Orchestrates the build process for a Raster project.
pub struct Builder {
    /// The backend to use for compilation.
    backend: Box<dyn Backend>,
    /// Output directory for artifacts.
    output_dir: PathBuf,
    /// Path to the user's project.
    project_path: Option<PathBuf>,
}

impl Builder {
    /// Create a new builder with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            backend: Box::new(NativeBackend::new()),
            output_dir,
            project_path: None,
        }
    }

    /// Set a custom backend for compilation.
    pub fn with_backend(mut self, backend: Box<dyn Backend>) -> Self {
        self.backend = backend;
        self
    }

    /// Set the project path for the user's crate.
    pub fn with_project_path(mut self, path: PathBuf) -> Self {
        self.project_path = Some(path);
        self
    }

    /// Get the backend name.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Discover all registered tiles from the in-process registry.
    ///
    /// This uses the tile registry populated by the `#[tile]` macro.
    /// Note: This only works when the tiles are linked into the same binary.
    pub fn discover_tiles(&self) -> Vec<&'static TileRegistration> {
        iter_tiles().collect()
    }

    /// Discover tiles by scanning source files.
    ///
    /// This parses the project's source files to find `#[tile]` annotations.
    /// This works even when the CLI is a separate binary from the user's project.
    pub fn discover_tiles_from_source(&self) -> Result<Vec<DiscoveredTile>> {
        let project_path = self
            .project_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let discovery = TileDiscovery::new(&project_path);
        discovery.discover()
    }

    /// Build all tiles discovered from source files.
    ///
    /// This is the preferred method for CLI usage since it doesn't require
    /// the tiles to be linked into the CLI binary.
    pub fn build_from_source(&self) -> Result<BuildOutput> {
        let tiles = self.discover_tiles_from_source()?;

        if tiles.is_empty() {
            return Ok(BuildOutput {
                tiles_compiled: 0,
                schemas_generated: 0,
                artifacts: HashMap::new(),
                backend: self.backend.name().to_string(),
            });
        }

        let mut artifacts = HashMap::new();
        let mut compiled = 0;

        for tile in tiles {
            let tile_id = &tile.metadata.id.0;

            match self.backend.compile_tile(&tile.metadata, &tile.source_file) {
                Ok(output) => {
                    // Write artifacts
                    let artifact = self.write_tile_artifacts(tile_id, &output)?;
                    artifacts.insert(tile_id.clone(), artifact);
                    compiled += 1;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to compile tile '{}': {}", tile_id, e);
                }
            }
        }

        Ok(BuildOutput {
            tiles_compiled: compiled,
            schemas_generated: 0,
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }

    /// Build all tiles from the in-process registry.
    ///
    /// Note: This only works when tiles are linked into the same binary.
    /// For CLI usage, prefer `build_from_source()`.
    pub fn build_from_registry(&self) -> Result<BuildOutput> {
        let tiles: Vec<_> = self.discover_tiles();

        if tiles.is_empty() {
            return Ok(BuildOutput {
                tiles_compiled: 0,
                schemas_generated: 0,
                artifacts: HashMap::new(),
                backend: self.backend.name().to_string(),
            });
        }

        let mut artifacts = HashMap::new();
        let mut compiled = 0;

        for tile in tiles {
            let metadata = tile.metadata_owned();
            let tile_id = tile.id_str();

            // Determine source path (placeholder for now)
            let source_path = self
                .project_path
                .as_ref()
                .map(|p| p.join("src/lib.rs").to_string_lossy().to_string())
                .unwrap_or_else(|| "src/lib.rs".to_string());

            match self.backend.compile_tile(&metadata, &source_path) {
                Ok(output) => {
                    // Write artifacts
                    let artifact = self.write_tile_artifacts(tile_id, &output)?;
                    artifacts.insert(tile_id.to_string(), artifact);
                    compiled += 1;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to compile tile '{}': {}", tile_id, e);
                }
            }
        }

        Ok(BuildOutput {
            tiles_compiled: compiled,
            schemas_generated: 0,
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }

    /// Build all tiles and schemas from a manifest.
    pub fn build(&self, manifest: &Manifest) -> Result<BuildOutput> {
        let mut artifacts = HashMap::new();
        let mut compiled = 0;

        for tile_meta in &manifest.tiles {
            // Determine source path (placeholder for now)
            let source_path = self
                .project_path
                .as_ref()
                .map(|p| p.join("src/lib.rs").to_string_lossy().to_string())
                .unwrap_or_else(|| "src/lib.rs".to_string());

            match self.backend.compile_tile(tile_meta, &source_path) {
                Ok(output) => {
                    let artifact = self.write_tile_artifacts(&tile_meta.id.0, &output)?;
                    artifacts.insert(tile_meta.id.0.clone(), artifact);
                    compiled += 1;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to compile tile '{}': {}", tile_meta.id.0, e);
                }
            }
        }

        Ok(BuildOutput {
            tiles_compiled: compiled,
            schemas_generated: manifest.sequences.len(),
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }

    /// Build a single tile by ID using source-based discovery.
    pub fn build_tile(&self, tile_id: &str) -> Result<TileArtifact> {
        // First try source-based discovery
        let tiles = self.discover_tiles_from_source()?;
        if let Some(tile) = tiles.iter().find(|t| t.metadata.id.0 == tile_id) {
            let output = self.backend.compile_tile(&tile.metadata, &tile.source_file)?;
            return self.write_tile_artifacts(tile_id, &output);
        }

        // Fall back to registry (for in-process usage)
        let tile = iter_tiles()
            .find(|t| t.id_str() == tile_id)
            .ok_or_else(|| Error::InvalidTileId(tile_id.to_string()))?;

        let metadata = tile.metadata_owned();
        let source_path = self
            .project_path
            .as_ref()
            .map(|p| p.join("src/lib.rs").to_string_lossy().to_string())
            .unwrap_or_else(|| "src/lib.rs".to_string());

        let output = self.backend.compile_tile(&metadata, &source_path)?;
        self.write_tile_artifacts(tile_id, &output)
    }

    /// Write tile artifacts to the output directory.
    fn write_tile_artifacts(
        &self,
        tile_id: &str,
        output: &CompilationOutput,
    ) -> Result<TileArtifact> {
        let artifact_dir = self.output_dir.join("tiles").join(tile_id);
        let backend_dir = artifact_dir.join(self.backend.name());

        fs::create_dir_all(&backend_dir).map_err(|e| {
            Error::Io(e)
        })?;

        // Write ELF if non-empty
        let elf_path = if !output.elf.is_empty() {
            let path = backend_dir.join("guest.elf");
            fs::write(&path, &output.elf).map_err(Error::Io)?;
            Some(path)
        } else {
            None
        };

        // Write method ID
        let method_id_path = backend_dir.join("method_id");
        let method_id_hex = hex::encode(&output.method_id);
        fs::write(&method_id_path, &method_id_hex).map_err(Error::Io)?;

        // Write manifest
        let manifest = TileManifest {
            tile_id: tile_id.to_string(),
            backend: self.backend.name().to_string(),
            method_id: method_id_hex.clone(),
            elf_size: output.elf.len(),
        };
        let manifest_path = backend_dir.join("manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| Error::Serialization(e))?;
        fs::write(&manifest_path, manifest_json).map_err(Error::Io)?;

        Ok(TileArtifact {
            tile_id: tile_id.to_string(),
            elf_path,
            method_id: method_id_hex,
            artifact_dir: backend_dir,
        })
    }
}

/// Output from a build operation.
#[derive(Debug)]
pub struct BuildOutput {
    /// Number of tiles successfully compiled.
    pub tiles_compiled: usize,
    /// Number of schemas generated.
    pub schemas_generated: usize,
    /// Artifacts produced per tile.
    pub artifacts: HashMap<String, TileArtifact>,
    /// Backend used for compilation.
    pub backend: String,
}

/// Artifact produced for a single tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileArtifact {
    /// The tile ID.
    pub tile_id: String,
    /// Path to the ELF binary (if produced).
    pub elf_path: Option<PathBuf>,
    /// Method ID (hex-encoded).
    pub method_id: String,
    /// Directory containing all artifacts.
    pub artifact_dir: PathBuf,
}

/// Manifest for a tile's artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileManifest {
    /// The tile ID.
    pub tile_id: String,
    /// Backend used for compilation.
    pub backend: String,
    /// Method ID (hex-encoded).
    pub method_id: String,
    /// Size of the ELF in bytes.
    pub elf_size: usize,
}
