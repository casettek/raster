//! CLI tool for the Raster toolchain.
//!
//! Provides commands for building, running, and analyzing Raster tiles.

mod chain;
mod commands;
mod program;
mod runtime_env;
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

    /// Print the value inside a raster artifact — a program's `output.bin`, a
    /// chain stage's artifact, or an external `*.rastered` input. They are one
    /// format, so this is one command (see docs/proposals/artifact-inspection.md)
    Show {
        /// Path to the raster payload
        artifact: String,

        /// Path to the `.rindex` (default: the artifact path with a `.rindex`
        /// extension, matching how the chain runner resolves it)
        #[arg(long)]
        index: Option<String>,

        /// Output format. Text truncates for a terminal; JSON does not, so it
        /// composes with `jq`
        #[arg(long, value_enum, default_value = "text")]
        format: ShowFormat,

        /// Bytes kept from a single string or bytes-page leaf
        #[arg(long = "max-bytes")]
        max_bytes: Option<usize>,

        /// Elements kept from a single list
        #[arg(long = "max-list")]
        max_list: Option<usize>,

        /// Fields kept from a single struct
        #[arg(long = "max-fields")]
        max_fields: Option<usize>,

        /// Nesting depth before a subtree is elided
        #[arg(long)]
        depth: Option<usize>,
    },

    /// Run or audit a multi-program chain, defined by a `Raster.toml` `[chain]`
    /// table or a `chain.json` (see docs/proposals/program-chain.md)
    Chain {
        #[command(subcommand)]
        command: ChainCommand,
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
        /// are derived from it so that one window reveals 128 bits (window 128
        /// -> 1 bit/item, 32 -> 4 bits/item). The trace's final window is
        /// covered separately: its trace roots are committed in full, so
        /// detection there does not depend on bits per item. Sizing this is
        /// therefore a proof-size choice, not a detection-strength one.
        /// Required with --commit; audits derive it from the commitment file.
        #[arg(
            long = "fraud-proof-window-size",
            value_parser = parse_fraud_proof_config,
            requires = "commit"
        )]
        fraud_proof_config: Option<FraudProofConfig>,

        /// Which executed step the fraud injector should corrupt, instead of
        /// picking one at random. `<n>` is a position in the eligible list,
        /// `exec:<n>` an `exec_index`, `tile:<name>` the first step running
        /// that tile or recur sequence, `seed:<n>` a reproducible random
        /// choice, and `list` prints the eligible steps and writes nothing.
        /// Passing this enables the injector on its own; without it the
        /// injector is enabled by a `fraud_` filename prefix and its random
        /// choice is reported as a `seed:` you can replay.
        #[arg(long = "fraud-step", requires = "commit")]
        fraud_step: Option<commands::fraud::FraudTarget>,

        /// Read and verify trace from file (mutually exclusive with --commit)
        #[arg(long, conflicts_with = "commit")]
        audit: Option<String>,

        /// Run without authenticated storage: tile outputs are passed as plain
        /// Rust values, nothing is hashed or stored between tiles, and no trace
        /// is written. Faster, and not authoritative — results cannot be
        /// committed or audited. Drafts and recur loops are not supported yet.
        #[arg(long = "no-auth", conflicts_with_all = ["commit", "audit"])]
        no_auth: bool,

        /// Read and verify trace from file (mutually exclusive with --commit)
        #[arg(long)]
        verbose: bool,

        /// After the run, print the value in the `output.bin` it produced —
        /// the same reader as `cargo raster show`, on the path this command
        /// already knows
        #[arg(long = "show-output")]
        show_output: bool,

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

#[derive(Parser)]
enum ChainCommand {
    /// Run every stage in order, threading each output into the next, and write
    /// a chain-commitment over the resulting checkpoints.
    Run {
        /// Chain manifest: a `Raster.toml` with a `[chain]` table, a directory
        /// containing one (or a `chain.json`), or an explicit `chain.json`.
        /// Omit to discover a chain manifest from the current directory upward.
        chain: Option<String>,

        /// Trace items covered by each stage's fraud-proof window (power of two,
        /// 2..=1024); each stage is committed with this window.
        #[arg(long = "fraud-proof-window-size", default_value_t = 2)]
        fraud_proof_window_size: usize,

        /// Run every stage without authenticated storage: no trace, no
        /// per-stage `commit.bin`, and no chain-commitment. Stages still link
        /// through their real `output.bin`, so the chain computes the same
        /// values — it just cannot be audited. For iterating on stage logic.
        #[arg(long = "no-auth", conflicts_with = "fraud_proof_window_size")]
        no_auth: bool,

        /// Run only this stage, in place, against an existing run directory.
        /// Its `from` producers must already have an `output.bin` there; every
        /// stage after it is deleted, since it is now stale.
        ///
        /// Authenticated single-stage runs write the stage's `commit.bin` but
        /// **no** chain-commitment: one is a statement about a whole chain, so
        /// there is no per-stage form of it, and the existing one is left
        /// untouched rather than overwritten with a one-stage impostor. That
        /// is what a dispute needs — the contested stage's trace, on demand.
        /// See `docs/proposals/chain-io-commitment.md`.
        #[arg(long)]
        stage: Option<String>,

        /// The chain run directory to work in, e.g.
        /// `target/raster/chains/00017…-pid4021` (or `chains-no-auth/…` under
        /// `--no-auth`). Omit to use the most recent one (`latest`).
        #[arg(long = "run")]
        run_dir: Option<String>,

        /// After the run, print the value in the final stage's `output.bin` —
        /// the same reader as `cargo raster show`. Final stage only, so a long
        /// chain does not print one value per stage; combine with `--stage` to
        /// inspect a middle stage, since the stage you re-ran is the last one
        /// that ran.
        #[arg(long = "show-output")]
        show_output: bool,

        /// Trace transport format used between each stage process and the
        /// Raster CLI. Transport only: every stage's commitment is built from
        /// the decoded trace, so this changes no commitment and no audit.
        /// Ignored under `--no-auth`, which records no trace at all.
        #[arg(long = "trace-format", value_enum, default_value = "binary")]
        trace_format: TraceFormat,
    },

    /// Verify a recorded chain's links and identities — public, no proving.
    Audit {
        /// Chain manifest (see `chain run`). Omit to discover from the current
        /// directory upward.
        chain: Option<String>,

        /// Path to the chain-commitment written by `chain run`. Omit to use the
        /// most recent run under `target/raster/chains/`.
        chain_commitment: Option<String>,

        /// Additionally re-run every stage natively and compare its output
        /// against the committed one (finds a stage to dispute; use
        /// `chain challenge` to post the challenge).
        #[arg(long)]
        execution: bool,
    },

    /// Challenger: challenge a chain commitment. A fault the commitment
    /// exhibits by itself (shape, then link) is proven on the spot with the
    /// chain-fraud guest — a proven challenge, which condemns the chain. Failing
    /// that, a stage whose re-run does not reproduce its committed output gets
    /// a disputed challenge: the stage, a trace commitment `H_C` of the re-run
    /// and the output it ends in, with no proof — the zkVM runs only in a
    /// defence. Writes `<run>/challenge/challenge.bin`. See
    /// `docs/proposals/stage-challenge.md`.
    Challenge {
        /// Chain manifest (see `chain run`).
        chain: Option<String>,

        /// Path to the chain-commitment under challenge. Omit to use the most
        /// recent run under `target/raster/chains/`.
        chain_commitment: Option<String>,

        /// Dispute this stage's output, skipping the search for a fault the
        /// commitment exhibits by itself. `H_C` is committed at the
        /// protocol's window size (128, capped below the trace length).
        #[arg(long)]
        stage: Option<String>,

        /// Test only: post a *false* challenge, to exercise the defences.
        /// Disputes the honest output with a flipped one, over `H_C` built
        /// from the honest trace (`claimed-output`), with one item replaced
        /// (`item`), one item appended (`pad`) or the last one dropped
        /// (`truncate`). Requires `--stage`.
        #[arg(long, value_enum, hide = true, requires = "stage")]
        forge: Option<chain::Forge>,
    },

    /// Verify a challenge of either form: a proven one's receipt against the
    /// pinned image ids (the chain is condemned), or a disputed one's
    /// admission and that `H_C`'s body is available beside it (it awaits a
    /// defence).
    ChallengeVerify {
        /// Path to the `challenge.bin` written by `chain challenge`.
        challenge: String,

        /// Also check that the challenge is against this chain-commitment.
        #[arg(long)]
        chain_commitment: Option<String>,
    },

    /// Defender (the claimer, or anyone): answer a disputed challenge. Re-runs
    /// the challenged stage and walks it against the challenger's `H_C`: a
    /// divergence is proven as a refutation; an honest `H_C` ending in another
    /// output than the claimed one, as a mislabel proof (its terminal window).
    /// The window is aggregated by the defence guest and written to
    /// `defence.bin` beside the challenge. When `H_C` is honest and ends in the
    /// claimed output the challenge is true: no defence exists, and nothing is
    /// proven. See `docs/proposals/stage-challenge.md` §Phase 3.
    Defend {
        /// Chain manifest (see `chain run`).
        chain: Option<String>,

        /// Path to the chain-commitment being defended. Omit to use the most
        /// recent run under `target/raster/chains/`.
        chain_commitment: Option<String>,

        /// Path to the challenge. Defaults to the run's
        /// `challenge/challenge.bin`, with `H_C`'s `commit.bin` beside it.
        #[arg(long)]
        challenge: Option<String>,
    },

    /// Verify a defence: the receipt against the pinned defence image id and
    /// the known-good inner guests, and that it answers the challenge.
    DefendVerify {
        /// Path to the `defence.bin` written by `chain defend`.
        defence: String,

        /// The challenge it must answer. Defaults to the `challenge.bin`
        /// beside the defence.
        #[arg(long)]
        challenge: Option<String>,
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

/// Rendering format for `show` / `--show-output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ShowFormat {
    /// Indented tree, truncated for a terminal
    Text,
    /// JSON, untruncated by default so it composes with `jq`
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
        Commands::Chain { command } => match command {
            ChainCommand::Run {
                chain,
                fraud_proof_window_size,
                no_auth,
                stage,
                run_dir,
                show_output,
                trace_format,
            } => chain::run(
                chain.as_deref(),
                fraud_proof_window_size,
                no_auth,
                stage.as_deref(),
                run_dir.as_deref(),
                show_output,
                trace_format,
            ),
            ChainCommand::Audit {
                chain,
                chain_commitment,
                execution,
            } => chain::audit(chain.as_deref(), chain_commitment.as_deref(), execution),
            ChainCommand::Challenge {
                chain,
                chain_commitment,
                stage,
                forge,
            } => chain::challenge(
                chain.as_deref(),
                chain_commitment.as_deref(),
                stage.as_deref(),
                forge,
            ),
            ChainCommand::ChallengeVerify {
                challenge,
                chain_commitment,
            } => chain::challenge_verify(&challenge, chain_commitment.as_deref()),
            ChainCommand::Defend {
                chain,
                chain_commitment,
                challenge,
            } => chain::defend(chain.as_deref(), chain_commitment.as_deref(), challenge.as_deref()),
            ChainCommand::DefendVerify { defence, challenge } => {
                chain::defend_verify(&defence, challenge.as_deref())
            }
        },
        Commands::Run {
            backend,
            input,
            input_manifest,
            commit,
            fraud_proof_config,
            fraud_step,
            audit,
            no_auth,
            verbose,
            show_output,
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
            fraud_step.as_ref(),
            audit.as_deref(),
            no_auth,
            verbose,
            show_output,
            trace_format,
            &features,
            all_features,
            no_default_features,
        ),
        Commands::Show {
            artifact,
            index,
            format,
            max_bytes,
            max_list,
            max_fields,
            depth,
        } => commands::show::show(
            &artifact,
            index.as_deref(),
            format,
            max_bytes,
            max_list,
            max_fields,
            depth,
        ),
    }
}
