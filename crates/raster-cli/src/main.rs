//! CLI tool for the Raster toolchain.
//!
//! Provides commands for building, running, and analyzing Raster tiles.

mod commands;

use clap::{Parser, ValueEnum};
use raster_core::Result;

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

    /// Run the user program
    Run {
        /// Backend to use for execution
        #[arg(long, short, value_enum, default_value = "native")]
        backend: BackendType,

        /// Input data as JSON string
        #[arg(long)]
        input: Option<String>,

        /// Write trace to file (mutually exclusive with --audit)
        #[arg(long, conflicts_with = "audit")]
        commit: Option<String>,

        /// Read and verify trace from file (mutually exclusive with --commit)
        #[arg(long, conflicts_with = "commit")]
        audit: Option<String>,
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
        Commands::RunTile {
            backend,
            tile,
            input,
            prove,
            verify,
        } => commands::tile::run_tile::run_tile(backend, &tile, input.as_deref(), prove, verify),
        Commands::List => commands::tile::list_tile::list_tiles(),
        Commands::Analyze { trace_path } => commands::analyze(trace_path),
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
        Commands::Run { backend, input, commit, audit } => {
            commands::run::run(backend, input.as_deref(), commit.as_deref(), audit.as_deref())
        }
    }
}
