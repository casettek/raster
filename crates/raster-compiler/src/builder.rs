//! Build orchestration for Raster projects.

use crate::{ast::ProjectAst, discovery::{DiscoveredTile, TileDiscovery}, sequence::SequenceExplorer, tile::TileExplorer};
use raster_backend::{Backend, CompilationArtifact, ExecutionMode, NativeBackend, TileExecutionResult};
use raster_core::{
    manifest::Manifest,
    registry::{iter_tiles, TileRegistration},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

/// Compute a simple hash of a file's contents for cache invalidation.
fn compute_source_hash(path: &str) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).ok()?;

    // Simple hash: use the first 16 bytes of a basic checksum
    // This is fast and good enough for cache invalidation
    let mut hash: u64 = 0;
    for (i, byte) in contents.iter().enumerate() {
        hash = hash.wrapping_add((*byte as u64).wrapping_mul((i as u64).wrapping_add(1)));
        hash = hash.rotate_left(7);
    }
    let len_hash = contents.len() as u64;
    Some(format!("{:016x}{:016x}", hash, len_hash))
}

/// Orchestrates the build process for a Raster project.
pub struct Builder {
    /// The backend to use for compilation (shared via Arc for TileRunner).
    backend: Arc<dyn Backend>,
    /// Output directory for artifacts.
    output_dir: PathBuf,
    /// Path to the user's project.
    project_path: Option<PathBuf>,
}

impl Builder {
    /// Create a new builder with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            backend: Arc::new(NativeBackend::new()),
            output_dir,
            project_path: None,
        }
    }

    /// Set a custom backend for compilation.
    /// Accepts Box<dyn Backend> for convenience, converts to Arc internally.
    pub fn with_backend(mut self, backend: Box<dyn Backend>) -> Self {
        self.backend = Arc::from(backend);
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

        let project_ast = ProjectAst::new(&project_path).unwrap();
        let tile_explorer = TileExplorer::new(&project_ast);
        let sequences = SequenceExplorer::new(&project_ast, &tile_explorer);

        println!("\nproject_ast: {:?}", project_ast);
        println!("\nsequences: {:#?}", sequences);

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
                skipped_cached: 0,
                schemas_generated: 0,
                artifacts: HashMap::new(),
                backend: self.backend.name().to_string(),
            });
        }

        let store = self.backend.artifact_store();
        let mut artifacts = HashMap::new();
        let mut compiled = 0;
        let mut skipped = 0;

        for tile in tiles {
            let tile_id = &tile.metadata.id.0;
            let source_hash = compute_source_hash(&tile.source_file);

            // Try loading from cache (backend handles its own format)
            if let Some(cached) = store.load(tile_id, &self.output_dir, source_hash.as_deref()) {
                let artifact = TileArtifact {
                    tile_id: tile_id.clone(),
                    artifact_dir: cached
                        .artifact_dir()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| self.default_artifact_dir(tile_id)),
                };
                artifacts.insert(tile_id.clone(), artifact);
                skipped += 1;
                continue;
            }

            // Compile tile (backend returns its own Executable type)
            match self.backend.compile_tile(&tile.metadata, &tile.source_file) {
                Ok(executable) => {
                    // Persist artifacts (backend handles its own format)
                    let artifact_dir = store.save(
                        executable.as_ref(),
                        &self.output_dir,
                        source_hash.as_deref(),
                    )?;

                    let artifact = TileArtifact {
                        tile_id: tile_id.clone(),
                        artifact_dir,
                    };
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
            skipped_cached: skipped,
            schemas_generated: 0,
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }

    /// Get the default artifact directory for a tile.
    fn default_artifact_dir(&self, tile_id: &str) -> PathBuf {
        self.output_dir
            .join("tiles")
            .join(tile_id)
            .join(self.backend.name())
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
                skipped_cached: 0,
                schemas_generated: 0,
                artifacts: HashMap::new(),
                backend: self.backend.name().to_string(),
            });
        }

        let store = self.backend.artifact_store();
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

            let source_hash = compute_source_hash(&source_path);

            match self.backend.compile_tile(&metadata, &source_path) {
                Ok(executable) => {
                    // Persist artifacts
                    let artifact_dir = store.save(
                        executable.as_ref(),
                        &self.output_dir,
                        source_hash.as_deref(),
                    )?;

                    let artifact = TileArtifact {
                        tile_id: tile_id.to_string(),
                        artifact_dir,
                    };
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
            skipped_cached: 0,
            schemas_generated: 0,
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }

    /// Build all tiles and schemas from a manifest.
    pub fn build(&self, manifest: &Manifest) -> Result<BuildOutput> {
        let store = self.backend.artifact_store();
        let mut artifacts = HashMap::new();
        let mut compiled = 0;

        for tile_meta in &manifest.tiles {
            let tile_id = &tile_meta.id.0;

            // Determine source path (placeholder for now)
            let source_path = self
                .project_path
                .as_ref()
                .map(|p| p.join("src/lib.rs").to_string_lossy().to_string())
                .unwrap_or_else(|| "src/lib.rs".to_string());

            let source_hash = compute_source_hash(&source_path);

            match self.backend.compile_tile(tile_meta, &source_path) {
                Ok(executable) => {
                    // Persist artifacts
                    let artifact_dir = store.save(
                        executable.as_ref(),
                        &self.output_dir,
                        source_hash.as_deref(),
                    )?;

                    let artifact = TileArtifact {
                        tile_id: tile_id.clone(),
                        artifact_dir,
                    };
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
            skipped_cached: 0,
            schemas_generated: manifest.sequences.len(),
            artifacts,
            backend: self.backend.name().to_string(),
        })
    }


    /// Build a single tile by ID using source-based discovery.
    /// Uses cached artifacts if source hasn't changed.
    pub fn build_tile(&self, tile_id: &str) -> Result<TileArtifact> {
        let (artifact, _) = self.build_tile_with_cache_info(tile_id)?;
        Ok(artifact)
    }

    /// Build a single tile by ID, returning whether cache was used.
    pub fn build_tile_with_cache_info(&self, tile_id: &str) -> Result<(TileArtifact, bool)> {
        let (executable, artifact_dir, was_cached) = self.build_tile_executable(tile_id)?;
        // Discard the executable, caller only needs the artifact
        drop(executable);
        Ok((
            TileArtifact {
                tile_id: tile_id.to_string(),
                artifact_dir,
            },
            was_cached,
        ))
    }

    /// Build a tile and return the executable directly.
    ///
    /// This avoids the unnecessary reload pattern where we compile, save to disk,
    /// discard the executable, then reload it. Instead, we keep the executable
    /// in memory and return it directly.
    ///
    /// Returns (executable, artifact_dir, was_cached).
    pub fn build_tile_executable(
        &self,
        tile_id: &str,
    ) -> Result<(Box<dyn CompilationArtifact>, PathBuf, bool)> {
        let store = self.backend.artifact_store();

        // First try source-based discovery
        let tiles = self.discover_tiles_from_source()?;
        println!("build tiles: {:?}", tiles);

        if let Some(tile) = tiles.iter().find(|t| t.metadata.id.0 == tile_id) {
            let source_hash = compute_source_hash(&tile.source_file);

            // Check cache first
            if let Some(cached) = store.load(tile_id, &self.output_dir, source_hash.as_deref()) {
                println!("cached executable loaded");
                let artifact_dir = cached
                    .artifact_dir()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.default_artifact_dir(tile_id));
                return Ok((cached, artifact_dir, true));
            }

            // Compile the tile
            let executable = self
                .backend
                .compile_tile(&tile.metadata, &tile.source_file)?;
            println!("executable compiled");

            // Persist artifacts
            let artifact_dir = store.save(
                executable.as_ref(),
                &self.output_dir,
                source_hash.as_deref(),
            )?;

            return Ok((executable, artifact_dir, false));
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

        let source_hash = compute_source_hash(&source_path);

        let executable = self.backend.compile_tile(&metadata, &source_path)?;
        let artifact_dir = store.save(
            executable.as_ref(),
            &self.output_dir,
            source_hash.as_deref(),
        )?;

        Ok((executable, artifact_dir, false))
    }

    /// Build a tile and return a TileRunner that can execute it.
    /// This encapsulates the entire build + load process.
    ///
    /// Uses `build_tile_executable` internally to get the executable directly,
    /// avoiding the unnecessary reload pattern.
    pub fn build_tile_runner(&self, tile_id: &str) -> Result<TileRunner> {
        let (executable, _artifact_dir, _was_cached) = self.build_tile_executable(tile_id)?;

        Ok(TileRunner {
            backend: self.backend.clone(),
            executable,
            tile_id: tile_id.to_string(),
        })
    }

    /// Get the project path.
    pub fn project_path(&self) -> Option<&PathBuf> {
        self.project_path.as_ref()
    }

    /// Get the output directory.
    pub fn output_dir(&self) -> &PathBuf {
        &self.output_dir
    }
}

/// Output from a build operation.
#[derive(Debug)]
pub struct BuildOutput {
    /// Number of tiles successfully compiled.
    pub tiles_compiled: usize,
    /// Number of tiles skipped due to cache.
    pub skipped_cached: usize,
    /// Number of schemas generated.
    pub schemas_generated: usize,
    /// Artifacts produced per tile.
    pub artifacts: HashMap<String, TileArtifact>,
    /// Backend used for compilation.
    pub backend: String,
}

/// Artifact produced for a single tile (backend-agnostic).
/// 
/// The specific artifact format (ELF files, binaries, etc.) is handled
/// by each backend's ArtifactStore implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileArtifact {
    /// The tile ID.
    pub tile_id: String,
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
    /// Hash of source file for cache invalidation.
    #[serde(default)]
    pub source_hash: Option<String>,
}

/// A compiled tile that is ready to run.
///
/// TileRunner encapsulates the backend and loaded executable,
/// providing a simple `run()` method that abstracts away backend-specific
/// execution details (ELF loading, zkVM execution, subprocess calls, etc.).
pub struct TileRunner {
    /// The backend to use for execution (shared).
    backend: Arc<dyn Backend>,
    /// The loaded executable (opaque, backend-specific).
    executable: Box<dyn CompilationArtifact>,
    /// The tile ID.
    tile_id: String,
}

impl TileRunner {
    /// Run the tile with the given input and execution mode.
    ///
    /// This abstracts the execution model - callers just "run" the tile
    /// without needing to know about backend-specific details.
    pub fn run(&self, input: &[u8], mode: ExecutionMode) -> Result<TileExecutionResult> {
        self.backend.execute_tile(self.executable.as_ref(), input, mode)
    }

    /// Get the tile ID.
    pub fn tile_id(&self) -> &str {
        &self.tile_id
    }

    /// Get a reference to the executable.
    pub fn executable(&self) -> &dyn CompilationArtifact {
        self.executable.as_ref()
    }

    /// Get the backend name.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }
}
