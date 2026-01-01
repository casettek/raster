//! Command implementations for the Raster CLI.

use crate::BackendType;
use anyhow::{Context, Result, anyhow};
use raster_backend::{Backend, ExecutionMode, NativeBackend};
use raster_compiler::Builder;
use raster_core::registry::{find_tile_by_str, iter_tiles, tile_count};
use std::env;
use std::path::PathBuf;

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
fn create_backend(backend_type: BackendType) -> Result<Box<dyn Backend>> {
    match backend_type {
        BackendType::Native => Ok(Box::new(NativeBackend::new())),
        BackendType::Risc0 => {
            // RISC0 backend would be created here
            // For now, return an error since it requires additional setup
            Err(anyhow!(
                "RISC0 backend is not available in this build. \n\
                 To use RISC0, ensure risc0-zkvm is installed and the \n\
                 raster-backend-risc0 crate is properly configured."
            ))
        }
    }
}

/// Build command: compile tiles using the specified backend.
pub fn build(backend_type: BackendType, tile: Option<String>) -> Result<()> {
    println!("Building tiles with {} backend...", backend_type_name(backend_type));
    println!();

    // Check if we have any tiles registered
    let count = tile_count();
    if count == 0 {
        println!("No tiles registered. Make sure to use the #[tile] macro.");
        println!("Hint: Run this command from within your Raster project directory.");
        return Ok(());
    }

    let backend = create_backend(backend_type)?;
    let builder = Builder::new(output_dir())
        .with_backend(backend)
        .with_project_path(project_path());

    if let Some(tile_id) = tile {
        // Build a single tile
        println!("Building tile: {}", tile_id);
        let artifact = builder.build_tile(&tile_id)?;
        println!("  Artifact dir: {}", artifact.artifact_dir.display());
        println!("  Method ID: {}", artifact.method_id);
        if let Some(elf_path) = artifact.elf_path {
            println!("  ELF: {}", elf_path.display());
        }
    } else {
        // Build all tiles
        let output = builder.build_from_registry()?;
        println!("Compiled {} tile(s) using {} backend", output.tiles_compiled, output.backend);
        for (tile_id, artifact) in &output.artifacts {
            println!("  {} -> {}", tile_id, artifact.artifact_dir.display());
        }
    }

    println!();
    println!("Build complete!");
    Ok(())
}

/// Run command: execute a tile with the specified backend.
pub fn run(
    backend_type: BackendType,
    tile_id: &str,
    input: Option<&str>,
    prove: bool,
    verify: bool,
    no_trace: bool,
) -> Result<()> {
    // Determine execution mode
    let mode = if verify {
        ExecutionMode::prove_and_verify()
    } else if prove {
        ExecutionMode::prove()
    } else {
        ExecutionMode::Estimate
    };

    let mode_name = match mode {
        ExecutionMode::Estimate => "estimate",
        ExecutionMode::Prove { verify: true } => "prove+verify",
        ExecutionMode::Prove { verify: false } => "prove",
    };

    println!("Running tile '{}' with {} backend in {} mode...",
             tile_id, backend_type_name(backend_type), mode_name);
    if !no_trace {
        println!("(tracing enabled)");
    }
    println!();

    // Find the tile in the registry
    let tile = find_tile_by_str(tile_id)
        .ok_or_else(|| anyhow!("Tile '{}' not found in registry", tile_id))?;

    // Prepare input
    let input_bytes = if let Some(input_json) = input {
        // Parse JSON input and serialize with bincode
        let value: serde_json::Value = serde_json::from_str(input_json)
            .context("Failed to parse input JSON")?;
        raster_core::bincode::serialize(&value)
            .context("Failed to serialize input")?
    } else {
        // Empty input (unit type)
        raster_core::bincode::serialize(&())
            .context("Failed to serialize empty input")?
    };

    // For native backend, we can execute directly via the registry
    if backend_type == BackendType::Native {
        println!("Executing tile directly via registry...");
        let output_bytes = tile.execute(&input_bytes)?;

        println!();
        println!("Execution complete!");
        println!("  Output bytes: {} bytes", output_bytes.len());

        // Try to decode as JSON for display
        if !output_bytes.is_empty() {
            if let Ok(output_str) = raster_core::bincode::deserialize::<String>(&output_bytes) {
                println!("  Output (string): {}", output_str);
            } else {
                println!("  Output (hex): {}", hex::encode(&output_bytes));
            }
        }

        // Simulate cycle count for native
        println!("  Cycles: ~1000 (simulated)");

        return Ok(());
    }

    // For RISC0 backend, we need to use the full compilation and execution pipeline
    let backend = create_backend(backend_type)?;

    // First compile the tile
    println!("Compiling tile...");
    let metadata = tile.metadata_owned();
    let compilation = backend.compile_tile(&metadata, "src/lib.rs")?;

    // Execute with the specified mode
    println!("Executing in zkVM...");
    let result = backend.execute_tile(&compilation, &input_bytes, mode)?;

    println!();
    println!("Execution complete!");
    println!("  Output bytes: {} bytes", result.output.len());
    if let Some(cycles) = result.cycles {
        println!("  Cycles: {}", cycles);
    }
    if result.receipt.is_some() {
        println!("  Receipt: generated ({} bytes)", result.receipt.as_ref().map(|r| r.len()).unwrap_or(0));
    }
    if let Some(verified) = result.verified {
        println!("  Verified: {}", verified);
    }

    Ok(())
}

/// List command: show all registered tiles.
pub fn list_tiles() -> Result<()> {
    let count = tile_count();
    println!("Registered tiles: {}", count);
    println!();

    if count == 0 {
        println!("No tiles registered. Make sure to use the #[tile] macro.");
        return Ok(());
    }

    for tile in iter_tiles() {
        println!("  {} (id: {})", tile.metadata.name, tile.id_str());
        if let Some(desc) = tile.metadata.description {
            println!("    Description: {}", desc);
        }
        if let Some(cycles) = tile.metadata.estimated_cycles {
            println!("    Estimated cycles: {}", cycles);
        }
        if let Some(memory) = tile.metadata.max_memory {
            println!("    Max memory: {} bytes", memory);
        }
        println!();
    }

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
    let cargo_toml = format!(r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
raster = {{ path = "../path/to/raster/crates/raster" }}
serde = {{ version = "1.0", features = ["derive"] }}
"#);
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
        let input = raster::core::bincode::serialize(&42u64).unwrap();
        let output = tile.execute(&input).unwrap();
        let result: u64 = raster::core::bincode::deserialize(&output).unwrap();
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
