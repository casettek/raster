//! CLI tool for the Raster toolchain.
//!
//! Provides commands for building, running, and analyzing Raster tiles.

mod commands;
mod program;
mod utils;

use clap::{Parser, ValueEnum};
use raster_core::Result;
use raster_prover::trace::FraudProofConfig;

#[derive(Parser)]
#[command(name = "cargo-raster")]
#[command(bin_name = "cargo raster")]
#[command(about = "Raster toolchain CLI", long_about = None)]
#[command(version)]
enum Cli {
    #[command(subcommand)]
    Raster(Commands),
}

#[derive(Parser)]
enum Commands {
    /// Build tiles and generate schemas
    Build {
        /// Backend to use for compilation
        #[arg(long, short, value_enum, default_value = "native")]
        backend: BackendType,

        /// Specific tile to build (builds all if not specified)
        #[arg(long)]
        tile: Option<String>,
    },

    /// Execute a tile
    RunTile {
        /// Backend to use for execution
        #[arg(long, short, value_enum, default_value = "native")]
        backend: BackendType,

        /// Tile ID to execute
        #[arg(long)]
        tile: String,

        /// Input data as JSON string
        #[arg(long)]
        input: Option<String>,

        /// Generate a proof (RISC0 backend only)
        #[arg(long)]
        prove: bool,

        /// Verify the generated proof (implies --prove)
        #[arg(long)]
        verify: bool,
    },

    /// List all project tiles
    List,

    /// Analyze execution traces
    Analyze {
        /// Path to a run-scoped profile file emitted by `cargo raster run`
        profile_path: Option<String>,

        /// Follow a live run-scoped NDJSON profile stream emitted by `cargo raster run`
        #[arg(long)]
        follow: Option<String>,

        /// Refresh interval for follow mode, in milliseconds
        #[arg(long, default_value_t = 500)]
        refresh_ms: u64,

        /// Report output format
        #[arg(long, value_enum, default_value = "text")]
        format: AnalyzeFormat,
    },

    /// Initialize a new Raster project
    Init {
        /// Project name
        name: String,
    },

    /// Preview a sequence with cycle count breakdown
    // Preview {
    //     /// Sequence name to execute (default: "main")
    //     #[arg(long, default_value = "main")]
    //     sequence: String,

    //     /// Input data as JSON string
    //     #[arg(long)]
    //     input: Option<String>,

    //     /// Use GPU acceleration for execution (Metal on macOS, CUDA on Linux/Windows)
    //     #[arg(long)]
    //     gpu: bool,
    // },

    /// Execute a sequence
    RunSequence {
        /// Backend to use for execution
        #[arg(long, short, value_enum, default_value = "native")]
        backend: BackendType,

        /// Sequence name to execute
        #[arg(long)]
        sequence: String,

        /// Input data as JSON string
        #[arg(long)]
        input: Option<String>,

        /// Generate a proof (RISC0 backend only)
        #[arg(long)]
        prove: bool,

        /// Verify the generated proof (implies --prove)
        #[arg(long)]
        verify: bool,
    },

    /// Generate control flow schema (CFS)
    Cfs {
        /// Output file path (default: target/raster/cfs.json)
        #[arg(long, short)]
        output: Option<String>,
    },

    /// Show the program's identity (commitment, interface, tile registry)
    Program {
        /// Recompute from source and check against Raster.lock
        #[arg(long)]
        verify: bool,
    },

    /// Run the user program
    Run {
        /// Backend to use for execution
        #[arg(long, short, value_enum, default_value = "native")]
        backend: BackendType,

        /// Input as path to a JSON file
        #[arg(long)]
        input: Option<String>,

        /// Public manifest as path to a JSON file
        #[arg(long = "input-manifest")]
        input_manifest: Option<String>,

        /// Write trace to file (mutually exclusive with --audit)
        #[arg(long, conflicts_with = "audit", requires = "fraud_proof_config")]
        commit: Option<String>,

        /// Number of trace items covered by a fraud-proof window; must be a
        /// power of two between 2 and 1024. Fingerprint bits revealed per item
        /// are derived from it to reach 128-bit fraud detection (window 128 ->
        /// 1 bit/item, 32 -> 4 bits/item). Required with --commit; audits
        /// derive it from the commitment file.
        #[arg(
            long = "fraud-proof-window-size",
            value_parser = parse_fraud_proof_config,
            requires = "commit"
        )]
        fraud_proof_config: Option<FraudProofConfig>,

        /// Read and verify trace from file (mutually exclusive with --commit)
        #[arg(long, conflicts_with = "commit")]
        audit: Option<String>,

        /// Read and verify trace from file (mutually exclusive with --commit)
        #[arg(long)]
        verbose: bool,

        /// Trace transport format used between the user process and Raster CLI
        #[arg(long = "trace-format", value_enum, default_value = "binary")]
        trace_format: TraceFormat,

        /// Space- or comma-separated Cargo features for building the target project
        #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append)]
        features: Vec<String>,

        /// Enable all Cargo features when building the target project
        #[arg(long)]
        all_features: bool,

        /// Disable default Cargo features when building the target project
        #[arg(long)]
        no_default_features: bool,
    },
}

/// Parse and validate the --fraud-proof-window-size argument into the
/// fraud-proof window parameters used for building trace commitments.
fn parse_fraud_proof_config(value: &str) -> std::result::Result<FraudProofConfig, String> {
    let window_size: usize = value
        .parse()
        .map_err(|_| format!("'{value}' is not a valid window size"))?;
    FraudProofConfig::from_window_size(window_size).map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AnalyzeFormat {
    Text,
    Json,
}

/// Available backends for compilation and execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackendType {
    /// Native execution without zkVM (default)
    Native,
    /// RISC0 zkVM backend with optional proving
    Risc0,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TraceFormat {
    /// Length-prefixed postcard-encoded TraceEvent frames
    Binary,
    /// Newline-delimited JSON TraceEvent records
    Json,
}

impl TraceFormat {
    pub fn as_runtime_str(self) -> &'static str {
        match self {
            Self::Binary => raster_runtime::TraceFormat::Binary.as_str(),
            Self::Json => raster_runtime::TraceFormat::Json.as_str(),
        }
    }

    pub fn trace_file_name(self) -> &'static str {
        match self {
            Self::Binary => "trace.bin",
            Self::Json => "trace.ndjson",
        }
    }
}

fn main() {
    if let Err(err) = try_main() {
        eprintln!("Runtime error: {}", err);
        std::process::exit(1);
    }
}

fn try_main() -> Result<()> {
    let Cli::Raster(cmd) = Cli::parse();

    match cmd {
        Commands::Build { backend, tile } => commands::build(backend, tile),
        Commands::RunTile {
            backend,
            tile,
            input,
            prove,
            verify,
        } => commands::tile::run_tile::run_tile(backend, &tile, input.as_deref(), prove, verify),
        Commands::List => commands::tile::list_tile::list_tiles(),
        Commands::Analyze {
            profile_path,
            follow,
            refresh_ms,
            format,
        } => commands::analyze(profile_path, follow, refresh_ms, format),
        Commands::Init { name } => commands::init(name),
        // Commands::Preview { sequence, input, gpu } => {
        //     commands::preview(&sequence, input.as_deref(), gpu)
        // }
        Commands::RunSequence {
            backend,
            sequence,
            input,
            prove,
            verify,
        } => commands::run_sequence(backend, &sequence, input.as_deref(), prove, verify),
        Commands::Cfs { output } => commands::cfs(output),
        Commands::Program { verify } => commands::program(verify),
        Commands::Run {
            backend,
            input,
            input_manifest,
            commit,
            fraud_proof_config,
            audit,
            verbose,
            trace_format,
            features,
            all_features,
            no_default_features,
        } => commands::run::run(
            backend,
            input.as_deref(),
            input_manifest.as_deref(),
            commit.as_deref(),
            fraud_proof_config,
            audit.as_deref(),
            verbose,
            trace_format,
            &features,
            all_features,
            no_default_features,
        ),
    }
}
