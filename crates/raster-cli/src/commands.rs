//! Command implementations for the Raster CLI.
mod utils;
use utils::encode::{decode_output, encode_input};

use crate::BackendType;
use raster_core::{Error, Result};
use raster_backend::{Backend, ExecutionMode};
use raster_backend_native::NativeBackend;
use raster_backend_risc0::Risc0Backend;
use raster_compiler::sequence::{FlattenedStep, SequenceDiscovery};
use raster_compiler::tile::TileDiscovery;

use raster_compiler::Project;
use raster_compiler::{Builder, CfsBuilder};

use std::env;
use std::fs;
use std::path::{PathBuf};
use raster_compiler::backend::BackendImpl;

pub mod tile;

/// Get the output directory for artifacts.
fn output_dir() -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target")
        .join("raster")
}

/// Get the project path (current directory).
fn project_path() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Create a backend instance.
fn create_backend(backend_type: BackendType) -> Result<BackendImpl> {
    match backend_type {
        BackendType::Native => {
            Ok(BackendImpl::Native(NativeBackend::new().with_project_path(project_path())))
        }
        BackendType::Risc0 => {
            Ok(BackendImpl::Risc0(Risc0Backend::new(output_dir()).with_user_crate(project_path())))
        }
        _ => return Err(Error::Other("Invalid backend type".into())),
    }
}

/// Build command: compile tiles using the specified backend.
pub fn build(backend_type: BackendType, tile: Option<String>) -> Result<()> {
    println!(
        "Building tiles with {} backend...",
        backend_type_name(backend_type)
    );
    println!();

    let project = Project::new(project_path())?;

    // Discover tiles from source files
    let discovery = TileDiscovery::new(&project);

    let discovered_tiles = discovery.tiles;

    if discovered_tiles.is_empty() {
        println!("No tiles found. Make sure to use the #[tile] macro.");
        println!("Hint: Run this command from within your Raster project directory.");
        return Ok(());
    }

    println!("Discovered {} tile(s) from source:", discovered_tiles.len());
    for t in &discovered_tiles {
        let metadata = t.to_metadata();
        println!(
            "  - {} ({})",
            metadata.name, t.source_file().display()
        );
    }
    println!();

    let backend = create_backend(backend_type)?;
    let builder = Builder::new(&project, &backend);

    // if let Some(tile) = discovery.get(tile_id) {
    //     // Build a single tile
    //     println!("Building tile: {}", tile_id);
    //     let artifact = builder.build_tile(&tile)?;
    //     println!("  Artifact dir: {}", artifact.path().display());
    // } else {
    //     // Build all tiles using source discovery
    //     let output = builder.build_from_source()?;
    //     if output.skipped_cached > 0 {
    //         println!(
    //             "Compiled {} tile(s), {} cached (unchanged) using {} backend",
    //             output.tiles_compiled, output.skipped_cached, output.backend
    //         );
    //     } else {
    //         println!(
    //             "Compiled {} tile(s) using {} backend",
    //             output.tiles_compiled, output.backend
    //         );
    //     }
    //     for (tile_id, artifact) in &output.artifacts {
    //         println!("  {} -> {}", tile_id, artifact.artifact_dir.display());
    //     }
    // }

    println!();
    println!("Build complete!");
    Ok(())
}


/// Analyze command: analyze execution traces.
pub fn analyze(trace_path: Option<String>) -> Result<()> {
    match trace_path {
        Some(path) => println!("Analyzing trace: {}", path),
        None => println!("Analyzing most recent trace..."),
    }
    println!();
    println!("Trace analysis not yet implemented.");
    // TODO: Implement trace analysis
    Ok(())
}

/// Init command: initialize a new Raster project.
pub fn init(name: String) -> Result<()> {
    println!("Initializing project: {}", name);
    println!();

    // Create project structure
    let project_dir = PathBuf::from(&name);
    let src_dir = project_dir.join("src");

    std::fs::create_dir_all(&src_dir)?;

    // Write Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
raster = {{ path = "../path/to/raster/crates/raster" }}
serde = {{ version = "1.0", features = ["derive"] }}
"#
    );
    std::fs::write(project_dir.join("Cargo.toml"), cargo_toml)?;

    // Write main.rs with example tile
    let main_rs = r#"use raster::prelude::*;

#[tile(description = "Example tile that doubles a number")]
fn double(x: u64) -> u64 {
    x * 2
}

fn main() {
    println!("Raster Project");
    println!();

    // Show registered tiles
    println!("Registered tiles: {}", tile_count());
    for tile in iter_tiles() {
        println!("  - {}", tile.id_str());
    }

    // Execute directly
    println!();
    println!("double(21) = {}", double(21));

    // Execute via registry
    if let Some(tile) = find_tile_by_str("double") {
        let input = raster::core::postcard::to_allocvec(&42u64).unwrap();
        let output = tile.execute(&input).unwrap();
        let result: u64 = raster::core::postcard::from_bytes(&output).unwrap();
        println!("double(42) via registry = {}", result);
    }
}
"#;
    std::fs::write(src_dir.join("main.rs"), main_rs)?;

    println!("Created project '{}' with example tile.", name);
    println!();
    println!("Next steps:");
    println!("  cd {}", name);
    println!("  cargo build");
    println!("  cargo run");
    println!();
    println!("To build with RISC0 backend:");
    println!("  cargo raster build --backend risc0");

    Ok(())
}

/// Get a human-readable name for a backend type.
fn backend_type_name(backend_type: BackendType) -> &'static str {
    match backend_type {
        BackendType::Native => "native",
        BackendType::Risc0 => "risc0",
    }
}

/// Preview command: execute a sequence with cycle count breakdown.
pub fn run_sequence(
    backend_type: BackendType,
    tile_id: &str,
    input: Option<&str>,
    prove: bool,
    verify: bool
) -> Result<()> {
    let mode = match (prove, verify) {
        (_, true) => ExecutionMode::prove_and_verify(),
        (true, false) => ExecutionMode::prove(),
        (false, false) => ExecutionMode::Estimate,
    };


    let project = Project::new(project_path())?;

    let tile_discovery = TileDiscovery::new(&project);
    let sequence_discovery = SequenceDiscovery::new(&project, &tile_discovery);

    let sequence = sequence_discovery.get(tile_id).unwrap();
    
    println!("Running sequence '{}' in preview mode...", sequence.function.name);
    println!();

    let backend = create_backend(backend_type)?;

    let builder = Builder::new(&project, &backend);

    for step in sequence.flatten(&sequence_discovery) {
        match step {
            FlattenedStep::Tile(tile) => {
                let tile_runner = builder.build_tile_runner(&tile)?;
                tile_runner.validate_input(input)?;
                let input_bytes = encode_input(input)?;
                let result = tile_runner.run(&input_bytes, mode)?;
                let output_display = decode_output(
                    tile.function.output.as_deref().unwrap_or("()"),
                    &result.output,
                );
                println!("  Output: {}", output_display);
            }
            FlattenedStep::Sequence(seq) => {
                println!("  Entering sequence '{}'...", seq.function.name);
            }
        }
    }

    // let mut results: Vec<TileResult> = Vec::new();

    // // Execute each tile in sequence using TileRunner
    // for (idx, tile_id) in tile_ids.iter().enumerate() {
    //     // Check if compilation is needed before building

    //     // Build tile and get a runner (uses cache if available)
    //     let runner = builder
    //         .build_tile_runner(tile_id)
    //         .with_context(|| format!("Failed to build tile '{}'", tile_id))?;

    //     println!(
    //         "[{}/{}] Executing tile '{}'...",
    //         idx + 1,
    //         tile_ids.len(),
    //         tile_id
    //     );

    //     // Execute in estimate mode using TileRunner
    //     let result = runner
    //         .run(&current_input, ExecutionMode::Estimate)
    //         .with_context(|| format!("Failed to execute tile '{}'", tile_id))?;

    //     let cycles = result.cycles.unwrap_or(0);
    //     let proof_cycles = result.proof_cycles.unwrap_or(0);

    //     results.push(TileResult {
    //         name: tile_id.clone(),
    //         cycles,
    //         proof_cycles,
    //     });

    //     // Output becomes input for next tile
    //     current_input = result.output;
    // }

    // // Print summary table
    // println!();
    // println!("╔══════════════════════════════════════════════════════════════╗");
    // println!("║                     Cycle Count Summary                      ║");
    // println!("╠════════════════════╦══════════════════╦══════════════════════╣");
    // println!("║ Tile               ║ Compute Cycles   ║ Proof Cycles         ║");
    // println!("╠════════════════════╬══════════════════╬══════════════════════╣");

    // let mut total_cycles = 0u64;
    // let mut total_proof_cycles = 0u64;

    // for result in &results {
    //     println!(
    //         "║ {:<18} ║ {:>16} ║ {:>20} ║",
    //         truncate_str(&result.name, 18),
    //         format_number(result.cycles),
    //         format_number(result.proof_cycles)
    //     );
    //     total_cycles += result.cycles;
    //     total_proof_cycles += result.proof_cycles;
    // }

    // println!("╠════════════════════╬══════════════════╬══════════════════════╣");
    // println!(
    //     "║ {:<18} ║ {:>16} ║ {:>20} ║",
    //     "TOTAL",
    //     format_number(total_cycles),
    //     format_number(total_proof_cycles)
    // );
    // println!("╚════════════════════╩══════════════════╩══════════════════════╝");

    // // Try to deserialize and display final output
    // println!();
    // if let Ok(output_str) = postcard::from_bytes::<String>(&current_input) {
    //     println!("Output: \"{}\"", output_str);
    // } else if let Ok(output_num) = postcard::from_bytes::<u64>(&current_input) {
    //     println!("Output: {}", output_num);
    // } else {
    //     println!("Output: {} bytes", current_input.len());
    // }

    Ok(())
}

/// Truncate a string to a maximum length.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Format a number with thousand separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*c);
    }

    result
}


/// CFS command: generate control flow schema.
pub fn cfs(output: Option<String>) -> Result<()> {
    println!("Generating control flow schema...");
    println!();

    let root = project_path();

    let project = Project::new(root)?;
        // Build the CFS
    let cfs_builder = CfsBuilder::new(&project);
    let cfs = cfs_builder
        .build()
        .map_err(|e| Error::Other(format!("Failed to build CFS: {}", e)))?;

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&cfs).map_err(|e| Error::Other(format!("Failed to serialize CFS to JSON: {}", e)))?;

    // Determine output path
    let output_path = match output {
        Some(path) => PathBuf::from(path),
        None => output_dir().join("cfs.json"),
    };

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write the file
    fs::write(&output_path, &json)?;

    println!(
        "Generated CFS with {} tiles and {} sequences",
        cfs.tiles.len(),
        cfs.sequences.len()
    );
    println!();
    println!("Tiles:");
    for tile in &cfs.tiles {
        println!(
            "  - {} (inputs: {}, outputs: {})",
            tile.id, tile.inputs, tile.outputs
        );
    }
    println!();
    println!("Sequences:");
    for seq in &cfs.sequences {
        println!("  - {} ({} items)", seq.id, seq.items.len());
        for (idx, item) in seq.items.iter().enumerate() {
            println!("      [{}] {} '{}'", idx, item.item_type, item.item_id);
        }
    }
    println!();
    println!("CFS written to: {}", output_path.display());

    Ok(())
}
