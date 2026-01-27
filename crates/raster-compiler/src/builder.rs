//! Build orchestration for Raster projects.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use raster_core::Error;

use crate::{
    backend::BackendImpl,
    tile::TileDiscovery,
    Project,
};
use raster_backend::{
    Backend, CompilationArtifact, ExecutionMode, TileExecutionResult,
};
use raster_core::{
    Result, manifest::Manifest, registry::{TileRegistration, iter_tiles} 
};
use crate::tile::Tile;
/// Orchestrates the build process for a Raster project.
pub struct Builder<'ast, 'b> {
    /// The backend to use for compilation (shared via Arc for TileRunner).
    backend: &'b BackendImpl,
    /// Path to the user's project.
    project: &'ast Project,
}


pub trait TileBuilder<'ast> {
    fn build(&self, tile_id: &str) -> Result<TileArtifact>;
}

impl<'ast, 'b> Builder<'ast, 'b> {
    pub fn new(project: &'ast Project, backend: &'b BackendImpl) -> Self {
        Self {
            backend,
            project,
        }
    }


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

    /// Build all tiles discovered from source files.
    ///
    /// This is the preferred method for CLI usage since it doesn't require
    /// the tiles to be linked into the CLI binary.
    pub fn build_from_source(&self) -> Result<BuildOutput> {
        let tile_discovery = TileDiscovery::new(&self.project);

        let tiles = tile_discovery.tiles;
        if tiles.is_empty() {
            return Ok(BuildOutput {
                tiles_compiled: 0,
                skipped_cached: 0,
                schemas_generated: 0,
                artifacts: HashMap::new(),
                backend: self.backend.name().to_string(),
            });
        }

        let mut artifacts = HashMap::new();
        let mut compiled = 0;

        for tile in tiles {
            let tile_id = tile.id().to_string();
            
            let metadata = tile.to_metadata();
            let content_hash = tile.to_content_hash();

            match self.backend.compile_tile(&metadata, content_hash.as_deref()) {
                Ok(executable) => {
                    let artifact = TileArtifact {
                        tile_id: tile_id.clone(),
                        artifact_dir: executable.path().to_path_buf()
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
            skipped_cached: 0, // Backend handles caching internally now
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
            let tile_id = &tile_meta.id.0;
            // Compile tile - backend handles caching internally
            // TODO: fix caching with content hash
            match self.backend.compile_tile(tile_meta, None) {
                Ok(executable) => {
                    let artifact = TileArtifact {
                        tile_id: tile_id.clone(),
                        artifact_dir: executable
                            .path()
                            .to_path_buf(),
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

    /// Build a tile and return the executable directly.
    ///
    /// The backend handles caching internally - expensive backends like RISC0
    /// will cache their artifacts, while fast backends like native will recompile.
    ///
    /// Returns (executable, artifact_dir, was_cached).
    /// Note: was_cached is always false now since caching is internal to the backend.
    pub fn build_tile(
        &self,
        tile: &Tile<'ast>,
    ) -> Result<Box<dyn CompilationArtifact>> {
        let metadata = tile.to_metadata();
        let content_hash = tile.to_content_hash();
            // Compile tile - backend handles caching internally
        let artifact = self.backend.compile_tile(&metadata, content_hash.as_deref())?;

        return Ok(artifact);
    }

    /// Build a tile and return a TileRunner that can execute it.
    /// This encapsulates the entire build + load process.
    ///
    /// Uses `build_tile_executable` internally to get the executable directly,
    /// avoiding the unnecessary reload pattern.
    pub fn build_tile_runner<'a>(&self, tile: &'a Tile<'a>) -> Result<TileRunner<'a, 'b>> {
        let artifact =
            self.build_tile(tile)?;

        Ok(TileRunner {
            backend: &self.backend,
            executable: artifact,
            tile,
        })
    }

    /// Build a sequence runner from an entrypoint sequence name.
    ///
    /// This creates a SequenceRunner that can execute a full sequence of tiles.
    /// The sequence is expanded (resolving nested sequence calls) and all tiles
    /// are pre-built before returning the runner.
    ///
    /// # Arguments
    /// * `sequence_name` - The name of the sequence to build
    ///
    /// # Returns
    /// A SequenceRunner ready to execute the sequence.
    // pub fn build_sequence_runner<'ast>(
    //     &self,
    //     seq: &'ast Sequence<'ast>,
    //     seq_discovery: &'ast SequenceDiscovery<'ast>,
    // ) -> Result<SequenceRunner<'ast>> {
    //     // Get the project path
    //     // Build all tile runners (filter for tiles only from the flattened steps)
    //     let tile_runners = seq.flatten(seq_discovery)
    //         .filter_map(|step| match step {
    //             FlattenedStep::Tile(tile) => Some(tile),
    //             FlattenedStep::Sequence(_) => None,
    //         })
    //         .map(|tile| self.build_tile_runner(tile))
    //         .collect::<Result<Vec<TileRunner>>>()?;

    //     Ok(SequenceRunner {
    //         backend: self.backend.clone(),
    //         tile_runners,
    //         sequence_name: seq.function.name.clone(),
    //     })
    // }

    /// Get the output directory.
    pub fn output_dir(&self) -> &PathBuf {
        &self.project.output_dir
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
pub struct TileRunner<'ast, 'b> {
    /// The backend to use for execution (shared).
    backend: &'b BackendImpl,
    /// The loaded executable (opaque, backend-specific).
    executable: Box<dyn CompilationArtifact>,

    tile: &'ast Tile<'ast>,
}

impl<'ast, 'b> TileRunner<'ast, 'b> {
    /// Run the tile with the given input and execution mode.
    ///
    /// This abstracts the execution model - callers just "run" the tile
    /// without needing to know about backend-specific details.
    pub fn run(&self, input: &[u8], mode: ExecutionMode) -> Result<TileExecutionResult> {
        self.backend
            .execute_tile(self.executable.as_ref(), input, mode)
    }

    /// Get a reference to the executable.
    pub fn executable(&self) -> &dyn CompilationArtifact {
        self.executable.as_ref()
    }

    /// Get the backend name.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn validate_input(&self, input: Option<&str>) -> Result<()> {
        let tile_function = self.tile.function.clone();
        let input_count = tile_function.inputs.len();

        match (input_count, input) {
            // No inputs expected, none provided - OK
            (0, None) => Ok(()),

            // No inputs expected, but input provided
            (0, Some(_)) => Err(Error::Other(format!(
                "Tile '{}' takes no arguments, but --input was provided.\n\
                 Signature: {}",
                tile_function.name,
                tile_function.signature
            ))),

            // Inputs expected, none provided
            (n, None) => Err(Error::Other(format!(
                "Tile '{}' requires {} argument(s), but no --input provided.\n\
                 Signature: {}\n\
                 Expected types: {}",
                tile_function.name,
                n,
                tile_function.signature,
                tile_function.inputs.join(", ")
            ))),

            // Both present - validate structure
            (n, Some(input_json)) => {
                let value: serde_json::Value =
                    serde_json::from_str(input_json).map_err(|e| Error::Other(format!("Failed to parse input JSON: {}", e)))?;

                if n == 1 {
                    // Single argument - validate it's not an array (unless type is array)
                    Self::validate_single_input(&tile_function.inputs[0], &value)
                } else {
                    // Multiple arguments - expect array with correct length
                    Self::validate_multiple_inputs(&tile_function.inputs, &value)
                }
            }
        }
    }

    fn validate_single_input(expected_type: &str, value: &serde_json::Value) -> Result<()> {
        // Basic type checking based on JSON value type
        let type_mismatch = match (expected_type, value) {
            // String types
            ("String" | "&str" | "& str", serde_json::Value::String(_)) => false,
            ("String" | "&str" | "& str", _) => true,

            // Numeric types
            (
                "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize",
                serde_json::Value::Number(_),
            ) => false,
            (
                "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize",
                _,
            ) => true,

            // Boolean
            ("bool", serde_json::Value::Bool(_)) => false,
            ("bool", _) => true,

            // Arrays/Vecs
            (t, serde_json::Value::Array(_)) if t.starts_with("Vec<") || t.starts_with("Vec <") => {
                false
            }

            // Objects/structs - can't easily validate, allow any object
            (_, serde_json::Value::Object(_)) => false,

            // Unknown types - don't fail, let runtime handle it
            _ => false,
        };

        if type_mismatch {
            Err(Error::Other(format!(
                "Expects input of type '{}', but got {}\n\
                 Hint: Use proper JSON format, e.g., '\"hello\"' for strings, 42 for numbers",
                expected_type,
                Self::json_type_name(value)
            )))
        } else {
            Ok(())
        }
    }

    fn validate_multiple_inputs(
        expected_types: &[String],
        value: &serde_json::Value,
    ) -> Result<()> {
        match value {
            serde_json::Value::Array(arr) => {
                if arr.len() != expected_types.len() {
                    Err(Error::Other(format!(
                        "Expects {} argument(s), but got {} in array.\n\
                         Expected types: ({})\n\
                         Hint: Use JSON array format, e.g., '[\"hello\", 42]'",
                        expected_types.len(),
                        arr.len(),
                        expected_types.join(", ")
                    )))
                } else {
                    // Optionally validate each element
                    for (i, (expected, actual)) in expected_types.iter().zip(arr.iter()).enumerate()
                    {
                        if let Err(e) = Self::validate_single_input(expected, actual) {
                            return Err(Error::Other(format!("Argument {} invalid: {}", i + 1, e)));
                        }
                    }
                    Ok(())
                }
            }
            _ => Err(Error::Other(format!(
                "Expects {} arguments, provide them as a JSON array.\n\
                 Expected types: ({})\n\
                 Example: --input '[\"hello\", 42]'",
                expected_types.len(),
                expected_types.join(", ")
            ))),
        }
    }

    fn json_type_name(value: &serde_json::Value) -> &'static str {
        match value {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }
}


/// A compiled sequence ready to run.
///
/// SequenceRunner encapsulates a sequence of pre-built TileRunners that can
/// be executed in order, with each tile's output becoming the next tile's input.
pub struct SequenceRunner<'ast, 'b> {
    /// The backend used for execution (shared).
    backend: Arc<dyn Backend>,
    /// Pre-built TileRunners in execution order.
    step_runners: Vec<StepRunner<'ast, 'b>>,
    /// The sequence name.
    sequence_name: String,
}

pub enum StepRunner<'ast, 'b> {
    Tile(TileRunner<'ast, 'b>),
    Sequence(SequenceRunner<'ast, 'b>),
}

// impl<'ast> SequenceRunner<'ast> {
//     /// Execute the full sequence, chaining outputs to inputs.
//     ///
//     /// The initial input is passed to the first tile, and each subsequent tile
//     /// receives the output from the previous tile as its input.
//     pub fn run(&self, input: &[u8], mode: ExecutionMode) -> Result<SequenceExecutionResult> {
//         let mut current_input = input.to_vec();
//         let mut step_results = Vec::new();

//         for runner in &self.step_runners {
//             let result = match runner {
//                 StepRunner::Tile(tile_runner) => tile_runner.run(&current_input, mode)?,
//                 StepRunner::Sequence(sequence_runner) => sequence_runner.run(&current_input, mode)?,
//             };

//             step_results.push(StepResult::Tile(TileResult {
//                 output: result.output,
//             }));

//             // Output becomes input for next tile
//             current_input = result.output;
//         }

//         Ok(SequenceExecutionResult {
//             output: current_input,
//             step_results,
//         })
//     }

//     /// Get the sequence name.
//     pub fn sequence_name(&self) -> &str {
//         &self.sequence_name
//     }

//     /// Get the number of tiles in this sequence.
//     pub fn tile_count(&self) -> usize {
//         self.tile_runners.len()
//     }

//     /// Get the tile IDs in execution order.
//     pub fn tile_ids(&self) -> Vec<&str> {
//         self.tile_runners
//             .iter()
//             .map(|r| r.tile.function.name.as_str())
//             .collect()
//     }

//     /// Get the backend name.
//     pub fn backend_name(&self) -> &'static str {
//         self.backend.name()
//     }
// }
