//! Command implementations for the Raster CLI.
pub mod run;
pub mod tile;

use crate::utils::encode::{decode_execution_output, encode_input};

use crate::{AnalyzeFormat, BackendType, TraceFormat};
use raster_analysis::{Analyzer, Report};
use raster_backend::{Backend, ExecutionFailure, ExecutionMode};
use raster_backend_native::NativeBackend;
use raster_backend_risc0::Risc0Backend;
use raster_compiler::sequence::{FlattenedStep, SequenceDiscovery};
use raster_compiler::tile::TileDiscovery;
use raster_core::{Error, Result};

use raster_compiler::Project;
use raster_compiler::{Builder, CfsBuilder};
use raster_runtime::{ExecutionProfile, ProfileRecord, ProfileStreamEvent};

use raster_compiler::backend::BackendImpl;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

/// Get the output directory for artifacts.
fn output_dir() -> PathBuf {
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target")
        .join("raster")
}

#[derive(Debug, Clone)]
pub(crate) struct RunArtifacts {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub trace_path: PathBuf,
    pub profile_path: PathBuf,
    pub profile_stream_path: PathBuf,
}

impl RunArtifacts {
    fn new(run_id: String, trace_format: TraceFormat) -> Self {
        let run_dir = output_dir().join("runs").join(&run_id);
        Self {
            trace_path: run_dir.join(trace_format.trace_file_name()),
            profile_path: run_dir.join("profile.json"),
            profile_stream_path: run_dir.join("profile.ndjson"),
            run_dir,
            run_id,
        }
    }
}

pub(crate) fn create_run_artifacts(trace_format: TraceFormat) -> Result<RunArtifacts> {
    let run_id = generate_run_id();
    let artifacts = RunArtifacts::new(run_id, trace_format);
    fs::create_dir_all(&artifacts.run_dir)?;
    Ok(artifacts)
}

fn generate_run_id() -> String {
    static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let sequence = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:020}-{:09}-pid{}-{:06}",
        timestamp.as_secs(),
        timestamp.subsec_nanos(),
        std::process::id(),
        sequence
    )
}

/// Get the project path (current directory).
fn project_path() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Create a backend instance.
fn create_backend(backend_type: BackendType) -> Result<BackendImpl> {
    match backend_type {
        BackendType::Native => Ok(BackendImpl::Native(
            NativeBackend::new().with_project_path(project_path()),
        )),
        BackendType::Risc0 => Ok(BackendImpl::Risc0(
            Risc0Backend::new(output_dir()).with_user_crate(project_path()),
        )),
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
        println!("  - {} ({})", metadata.name, t.source_file().display());
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

/// Analyze command: analyze execution profiles.
pub fn analyze(
    profile_path: Option<String>,
    follow: Option<String>,
    refresh_ms: u64,
    format: AnalyzeFormat,
) -> Result<()> {
    if let Some(stream_path) = follow {
        return follow_profile_stream(PathBuf::from(stream_path), refresh_ms, format);
    }

    let Some(profile_path) = profile_path.map(PathBuf::from) else {
        return Err(Error::Other(
            "Provide a profile path, or use `cargo raster analyze --follow <path>`.".into(),
        ));
    };
    println!("Analyzing profile: {}", profile_path.display());
    println!();

    let analyzer = Analyzer::from_path(&profile_path)?;
    let metrics = analyzer.analyze()?;
    let report = Report::new(metrics);
    println!("{}", render_report(&report, format)?);

    Ok(())
}

fn render_report(report: &Report, format: AnalyzeFormat) -> Result<String> {
    match format {
        AnalyzeFormat::Text => Ok(report.to_text()),
        AnalyzeFormat::Json => report.to_json(),
    }
}

fn follow_profile_stream(path: PathBuf, refresh_ms: u64, format: AnalyzeFormat) -> Result<()> {
    println!("Following profile stream: {}", path.display());
    println!();

    let mut reader = loop {
        match fs::File::open(&path) {
            Ok(file) => break BufReader::new(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                thread::sleep(Duration::from_millis(refresh_ms.max(50)));
            }
            Err(error) => return Err(raster_core::Error::Io(error)),
        }
    };

    let mut profile = ExecutionProfile::new(Vec::new(), None, None);
    let mut saw_finish = false;
    let mut dirty = true;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                if dirty {
                    redraw_live_report(&profile, format)?;
                    dirty = false;
                }
                if saw_finish {
                    break;
                }
                thread::sleep(Duration::from_millis(refresh_ms.max(50)));
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let event: ProfileStreamEvent = serde_json::from_str(trimmed).map_err(|error| {
                    raster_core::Error::Serialization(format!(
                        "Failed to decode profile stream event '{}': {}",
                        trimmed, error
                    ))
                })?;
                saw_finish |= apply_stream_event(&mut profile, event);
                dirty = true;
            }
            Err(error) => return Err(raster_core::Error::Io(error)),
        }
    }

    Ok(())
}

fn redraw_live_report(profile: &ExecutionProfile, format: AnalyzeFormat) -> Result<()> {
    let analyzer = Analyzer::new(profile.clone());
    let metrics = analyzer.analyze()?;
    let report = Report::new(metrics);
    print!("\x1B[2J\x1B[H");
    println!("{}", render_report(&report, format)?);
    std::io::stdout().flush().map_err(raster_core::Error::Io)?;
    Ok(())
}

fn apply_stream_event(profile: &mut ExecutionProfile, event: ProfileStreamEvent) -> bool {
    match event {
        ProfileStreamEvent::RunStarted { .. } => false,
        ProfileStreamEvent::Record(record) => {
            profile.records.push(record);
            false
        }
        ProfileStreamEvent::TileOutputStore {
            invocation_index,
            output_store_ns,
        } => {
            if let Some(ProfileRecord::Tile(record)) = profile
                .records
                .iter_mut()
                .rev()
                .find(|record| matches!(record, ProfileRecord::Tile(tile) if tile.invocation_index == invocation_index))
            {
                record.total_duration_ns = record.total_duration_ns.saturating_add(output_store_ns);
                record.raster_overhead_ns = record.raster_overhead_ns.saturating_add(output_store_ns);
                record.output_store_ns = record.output_store_ns.saturating_add(output_store_ns);
            }
            false
        }
        ProfileStreamEvent::RunFinished {
            program_total_duration_ns,
            ..
        } => {
            profile.program_total_duration_ns = program_total_duration_ns;
            true
        }
    }
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

[features]
default = ["std"]
std = ["raster/std"]
profiling = ["raster/profiling"]

[dependencies]
raster = {{ path = "../path/to/raster/crates/raster", default-features = false }}
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
    println!("double(21) = {}", double(21));
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
    verify: bool,
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

    println!(
        "Running sequence '{}' in preview mode...",
        sequence.function.name
    );
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
                match decode_execution_output(
                    tile.function.output.as_deref().unwrap_or("()"),
                    &result.output,
                ) {
                    Ok(output_display) => println!("  Output: {}", output_display),
                    Err(ExecutionFailure::User(user_error)) => {
                        println!("  User error: {}", user_error)
                    }
                    Err(ExecutionFailure::Runtime(err)) => return Err(err),
                }
            }
            FlattenedStep::Recur(tile) => {
                println!(
                    "  Recur tile '{}' is not supported in preview mode",
                    tile.function.name
                );
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
    let json = serde_json::to_string_pretty(&cfs)
        .map_err(|e| Error::Other(format!("Failed to serialize CFS to JSON: {}", e)))?;

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
            match item {
                raster_core::cfs::SequenceChildItem::Tile(item) => {
                    println!("      [{}] {} '{}'", idx, "tile", item.id)
                }
                raster_core::cfs::SequenceChildItem::Recur(item) => {
                    println!("      [{}] {} '{}'", idx, "recur", item.id)
                }
                raster_core::cfs::SequenceChildItem::Sequence(item) => {
                    println!("      [{}] {} '{}'", idx, "sequence", item.id)
                }
            }
        }
    }
    println!();
    println!("CFS written to: {}", output_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn create_run_artifacts_returns_unique_run_scoped_paths() {
        let first = create_run_artifacts(TraceFormat::Binary)
            .expect("first artifact allocation should succeed");
        let second = create_run_artifacts(TraceFormat::Json)
            .expect("second artifact allocation should succeed");

        assert_ne!(first.run_id, second.run_id);
        assert_ne!(first.run_dir, second.run_dir);
        assert_eq!(first.trace_path, first.run_dir.join("trace.bin"));
        assert_eq!(second.trace_path, second.run_dir.join("trace.ndjson"));
        assert_eq!(first.profile_path, first.run_dir.join("profile.json"));
        assert_eq!(
            first.profile_stream_path,
            first.run_dir.join("profile.ndjson")
        );
        assert!(first.run_dir.exists());
        assert!(second.run_dir.exists());
    }

    #[test]
    fn create_run_artifacts_stays_unique_across_concurrent_allocations() {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    create_run_artifacts(TraceFormat::Binary)
                        .expect("artifact allocation should succeed")
                })
            })
            .collect();

        let artifacts: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread should complete"))
            .collect();

        let unique_run_dirs: HashSet<_> =
            artifacts.iter().map(|item| item.run_dir.clone()).collect();
        let unique_run_ids: HashSet<_> = artifacts.iter().map(|item| item.run_id.clone()).collect();

        assert_eq!(unique_run_dirs.len(), artifacts.len());
        assert_eq!(unique_run_ids.len(), artifacts.len());
    }
}
