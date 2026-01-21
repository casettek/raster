//! Command implementations for the Raster CLI.

use crate::BackendType;
use anyhow::{anyhow, Context, Result};
use raster_backend::{Backend, ExecutionMode, NativeBackend};
use raster_backend_risc0::{is_gpu_available, Risc0Backend};
use raster_compiler::ProjectAst;
use raster_compiler::tile::TileExplorer;
use raster_compiler::{
    extract_project_name, Builder, CfsBuilder, SequenceDiscovery, TileDiscovery,
};
use std::env;
use std::fs;
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
fn create_backend(backend_type: BackendType, use_gpu: bool) -> Result<Box<dyn Backend>> {
    match backend_type {
        BackendType::Native => {
            let backend = NativeBackend::new()
                .with_project_path(project_path());
            Ok(Box::new(backend))
        }
        BackendType::Risc0 => {
            let backend = Risc0Backend::new(output_dir())
                .with_user_crate(project_path())
                .with_gpu(use_gpu);
            Ok(Box::new(backend))
        }
    }
}

/// Build command: compile tiles using the specified backend.
pub fn build(backend_type: BackendType, tile: Option<String>) -> Result<()> {
    println!(
        "Building tiles with {} backend...",
        backend_type_name(backend_type)
    );
    println!();

    // Discover tiles from source files
    let discovery = TileDiscovery::new(project_path());
    let discovered_tiles = discovery
        .discover()
        .map_err(|e| anyhow!("Failed to discover tiles: {}", e))?;

    if discovered_tiles.is_empty() {
        println!("No tiles found. Make sure to use the #[tile] macro.");
        println!("Hint: Run this command from within your Raster project directory.");
        return Ok(());
    }

    println!("Discovered {} tile(s) from source:", discovered_tiles.len());
    for t in &discovered_tiles {
        println!(
            "  - {} ({}:{})",
            t.metadata.name, t.source_file, t.line_number
        );
    }
    println!();

    let backend = create_backend(backend_type, false)?;
    let builder = Builder::new(output_dir())
        .with_backend(backend)
        .with_project_path(project_path());

    if let Some(tile_id) = tile {
        // Build a single tile
        println!("Building tile: {}", tile_id);
        let artifact = builder.build_tile(&tile_id)?;
        println!("  Artifact dir: {}", artifact.artifact_dir.display());
    } else {
        // Build all tiles using source discovery
        let output = builder.build_from_source()?;
        if output.skipped_cached > 0 {
            println!(
                "Compiled {} tile(s), {} cached (unchanged) using {} backend",
                output.tiles_compiled, output.skipped_cached, output.backend
            );
        } else {
            println!(
                "Compiled {} tile(s) using {} backend",
                output.tiles_compiled, output.backend
            );
        }
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
    use_gpu: bool,
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

    // Check GPU availability and warn if requested but not available
    let gpu_status = if use_gpu {
        if is_gpu_available() {
            " (GPU enabled)"
        } else {
            eprintln!("Warning: --gpu requested but GPU acceleration not available.");
            eprintln!("         Rebuild with 'metal' feature on macOS or 'cuda' on Linux/Windows.");
            " (GPU requested but unavailable, using CPU)"
        }
    } else {
        ""
    };

    println!(
        "Running tile '{}' with {} backend in {} mode{}...",
        tile_id,
        backend_type_name(backend_type),
        mode_name,
        gpu_status
    );
    if !no_trace {
        println!("(tracing enabled)");
    }
    println!();

    // Prepare input
    let input_bytes = if let Some(input_json) = input {
        // Parse JSON input and serialize with postcard
        let value: serde_json::Value =
            serde_json::from_str(input_json).context("Failed to parse input JSON")?;
        postcard::to_allocvec(&value).context("Failed to serialize input")?
    } else {
        // Empty input (unit type)
        postcard::to_allocvec(&()).context("Failed to serialize empty input")?
    };

    // Use Builder to create a TileRunner (works for both Native and RISC0)
    let builder = Builder::new(output_dir())
        .with_backend(create_backend(backend_type, use_gpu)?)
        .with_project_path(project_path());

    // Build tile and get a runner (uses cache if source unchanged)
    println!("Building tile '{}'...", tile_id);
    let tile_runner= builder.build_tile_runner(tile_id)?;

    // Execute using TileRunner (abstracts backend-specific details)
    println!("Executing tile with {} backend...", tile_runner.backend_name());
    let result = tile_runner.run(&input_bytes, mode)?;

    println!();
    println!("Execution complete!");
    if let Some(cycles) = result.cycles {
        println!("  Compute Cycles: {}", cycles);
    }
    // Show proof cycles in estimate mode to help users understand proving cost
    if let Some(proof_cycles) = result.proof_cycles {
        if result.receipt.is_none() {
            // Only show this hint in estimate mode
            println!(
                "  Proof cycles: {} (padded for STARK proving)",
                proof_cycles
            );
        }
    }
    if result.receipt.is_some() {
        println!(
            "  Receipt: generated ({} bytes)",
            result.receipt.as_ref().map(|r| r.len()).unwrap_or(0)
        );
    }
    if let Some(verified) = result.verified {
        println!("  Verified: {}", verified);
    }

    Ok(())
}

/// List command: show all registered tiles.
pub fn list_tiles() -> Result<()> {
    let project_ast = ProjectAst::new(&project_path())?;
    let tile_explorer = TileExplorer::new(&project_ast);


    for tile in tile_explorer.tiles {
        println!("  {}", tile.function.signature);
        if let Some(description) = tile.description {
            println!("    Description: {}", description);
        }
        if let Some(cycles) = tile.estimated_cycles {
            println!("    Estimated cycles: {}", cycles);
        }
        if let Some(memory) = tile.max_memory {
            println!("    Max memory: {} bytes", memory);
        }
        println!("    Source: {}", tile.function.path.display());
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
pub fn preview(sequence_id: &str, input: Option<&str>, use_gpu: bool) -> Result<()> {
    println!("Running sequence '{}' in preview mode...", sequence_id);
    println!();

    // Discover sequences from source
    let discovery = SequenceDiscovery::new(project_path());
    let sequences = discovery
        .discover()
        .map_err(|e| anyhow!("Failed to discover sequences: {}", e))?;

    let sequence = sequences
        .iter()
        .find(|s| s.id == sequence_id)
        .ok_or_else(|| {
            let available: Vec<_> = sequences.iter().map(|s| s.id.as_str()).collect();
            if available.is_empty() {
                anyhow!(
                    "Sequence '{}' not found. No sequences defined.\n\
                     Hint: Add #[sequence] attribute to a function.",
                    sequence_id
                )
            } else {
                anyhow!(
                    "Sequence '{}' not found. Available sequences: {}",
                    sequence_id,
                    available.join(", ")
                )
            }
        })?;

    // Extract tile IDs from calls
    let tile_ids: Vec<String> = sequence.calls.iter().map(|c| c.callee.clone()).collect();

    if tile_ids.is_empty() {
        println!("Sequence '{}' has no tiles.", sequence_id);
        return Ok(());
    }

    println!("Sequence: {} ({} tiles)", sequence.id, tile_ids.len());
    println!("Tiles: {}", tile_ids.join(" → "));
    println!();

    // Create builder for compilation (using RISC0 backend for preview)
    let builder = Builder::new(output_dir())
        .with_backend(Box::new(
            Risc0Backend::new(output_dir())
                .with_user_crate(project_path())
                .with_gpu(use_gpu),
        ))
        .with_project_path(project_path());

    // Prepare initial input
    let mut current_input = if let Some(input_json) = input {
        let value: serde_json::Value =
            serde_json::from_str(input_json).context("Failed to parse input JSON")?;
        postcard::to_allocvec(&value).context("Failed to serialize input")?
    } else {
        postcard::to_allocvec(&()).context("Failed to serialize empty input")?
    };

    // Track cycle counts for each tile
    struct TileResult {
        name: String,
        cycles: u64,
        proof_cycles: u64,
    }
    let mut results: Vec<TileResult> = Vec::new();

    // Execute each tile in sequence using TileRunner
    for (idx, tile_id) in tile_ids.iter().enumerate() {
        // Check if compilation is needed before building

        // Build tile and get a runner (uses cache if available)
        let runner = builder
            .build_tile_runner(tile_id)
            .with_context(|| format!("Failed to build tile '{}'", tile_id))?;

        println!(
            "[{}/{}] Executing tile '{}'...",
            idx + 1,
            tile_ids.len(),
            tile_id
        );

        // Execute in estimate mode using TileRunner
        let result = runner
            .run(&current_input, ExecutionMode::Estimate)
            .with_context(|| format!("Failed to execute tile '{}'", tile_id))?;

        let cycles = result.cycles.unwrap_or(0);
        let proof_cycles = result.proof_cycles.unwrap_or(0);

        results.push(TileResult {
            name: tile_id.clone(),
            cycles,
            proof_cycles,
        });

        // Output becomes input for next tile
        current_input = result.output;
    }

    // Print summary table
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                     Cycle Count Summary                      ║");
    println!("╠════════════════════╦══════════════════╦══════════════════════╣");
    println!("║ Tile               ║ Compute Cycles   ║ Proof Cycles         ║");
    println!("╠════════════════════╬══════════════════╬══════════════════════╣");

    let mut total_cycles = 0u64;
    let mut total_proof_cycles = 0u64;

    for result in &results {
        println!(
            "║ {:<18} ║ {:>16} ║ {:>20} ║",
            truncate_str(&result.name, 18),
            format_number(result.cycles),
            format_number(result.proof_cycles)
        );
        total_cycles += result.cycles;
        total_proof_cycles += result.proof_cycles;
    }

    println!("╠════════════════════╬══════════════════╬══════════════════════╣");
    println!(
        "║ {:<18} ║ {:>16} ║ {:>20} ║",
        "TOTAL",
        format_number(total_cycles),
        format_number(total_proof_cycles)
    );
    println!("╚════════════════════╩══════════════════╩══════════════════════╝");

    // Try to deserialize and display final output
    println!();
    if let Ok(output_str) = postcard::from_bytes::<String>(&current_input) {
        println!("Output: \"{}\"", output_str);
    } else if let Ok(output_num) = postcard::from_bytes::<u64>(&current_input) {
        println!("Output: {}", output_num);
    } else {
        println!("Output: {} bytes", current_input.len());
    }

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

    // Extract project name from Cargo.toml
    let project_name = extract_project_name(&root)
        .map_err(|e| anyhow!("Failed to extract project name: {}", e))?;

    // Build the CFS
    let builder = CfsBuilder::new(&project_name);
    let cfs = builder
        .build(&root)
        .map_err(|e| anyhow!("Failed to build CFS: {}", e))?;

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&cfs).context("Failed to serialize CFS to JSON")?;

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
