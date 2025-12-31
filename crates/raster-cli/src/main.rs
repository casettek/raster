//! CLI tool for the Raster toolchain.

mod commands;

use clap::Parser;
use anyhow::Result;

#[derive(Parser)]
#[command(name = "cargo-raster")]
#[command(bin_name = "cargo raster")]
#[command(about = "Raster toolchain CLI", long_about = None)]
enum Cli {
    #[command(subcommand)]
    Raster(Commands),
}

#[derive(Parser)]
enum Commands {
    /// Build tiles and generate schemas
    Build,

    /// Execute a sequence natively
    Run {
        /// Disable tracing
        #[arg(long)]
        no_trace: bool,
    },

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
}

fn main() -> Result<()> {
    let Cli::Raster(cmd) = Cli::parse();

    match cmd {
        Commands::Build => commands::build(),
        Commands::Run { no_trace } => commands::run(no_trace),
        Commands::Analyze { trace_path } => commands::analyze(trace_path),
        Commands::Init { name } => commands::init(name),
    }
}
