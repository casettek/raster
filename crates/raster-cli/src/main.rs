//! CLI tool for the Raster toolchain.
//!
//! Provides commands for building, running, and analyzing Raster tiles.

mod commands;

use anyhow::Result;
use clap::{Parser, ValueEnum};

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
    Run {
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

        /// Use GPU acceleration for proving (Metal on macOS, CUDA on Linux/Windows)
        #[arg(long)]
        gpu: bool,

        /// Disable tracing
        #[arg(long)]
        no_trace: bool,
    },

    /// List all registered tiles
    List,

    /// Analyze execution traces
    Analyze {
        /// Path to trace file (optional, uses most recent if not specified)
        trace_path: Option<String>,
    },

    /// Initialize a new Raster project
    Init {
        /// Project name
        name: String,
    },

    /// Preview a sequence with cycle count breakdown
    Preview {
        /// Sequence name to execute (default: "main")
        #[arg(long, default_value = "main")]
        sequence: String,

        /// Input data as JSON string
        #[arg(long)]
        input: Option<String>,

        /// Use GPU acceleration for execution (Metal on macOS, CUDA on Linux/Windows)
        #[arg(long)]
        gpu: bool,
    },

    /// Generate control flow schema (CFS)
    Cfs {
        /// Output file path (default: target/raster/cfs.json)
        #[arg(long, short)]
        output: Option<String>,
    },
}

/// Available backends for compilation and execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum BackendType {
    /// Native execution without zkVM (default)
    Native,
    /// RISC0 zkVM backend with optional proving
    Risc0,
}

fn main() -> Result<()> {
    let Cli::Raster(cmd) = Cli::parse();

    match cmd {
        Commands::Build { backend, tile } => commands::build(backend, tile),
        Commands::Run {
            backend,
            tile,
            input,
            prove,
            verify,
            gpu,
            no_trace,
        } => commands::run(
            backend,
            &tile,
            input.as_deref(),
            prove,
            verify,
            gpu,
            no_trace,
        ),
        Commands::List => commands::list_tiles(),
        Commands::Analyze { trace_path } => commands::analyze(trace_path),
        Commands::Init { name } => commands::init(name),
        Commands::Preview {
            sequence,
            input,
            gpu,
        } => commands::preview(&sequence, input.as_deref(), gpu),
        Commands::Cfs { output } => commands::cfs(output),
    }
}
