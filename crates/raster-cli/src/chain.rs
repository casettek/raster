//! `cargo raster chain` — provable multi-program execution.
//!
//! A chain runs several raster programs in order, where each program's
//! authorized output (`ProgramEnd` → `output.bin`) becomes the next program's
//! authorized input (`ProgramStart` ← `--input`/`--input-manifest`). The chain
//! is provable at two levels (see `docs/proposals/program-chain.md`):
//!
//! - **Checkpoint level** — the links `(program identity, input commitment,
//!   output commitment)` per stage verify end-to-end by public, cheap hash
//!   checks over the artifact bytes; no proving is required to check a link.
//! - **Intra level** — any single stage is optimistically audited with the
//!   unchanged fraud-proof machinery, and its receipt is stage-attributable.
//!
//! v1 is **linear** (one output → one named input per link). The chain runner
//! reuses the two boundaries `ProgramStart`/`ProgramEnd` already established, so
//! `raster-runtime`/the guests need no chain-specific change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use raster_compiler::{CfsBuilder, Project};
use raster_core::cfs::{ControlFlowSchema, CfsCursor};
use raster_core::input::payload_structural_root;
use raster_core::program::commitment_of_bytes;
use raster_core::{Error, Result};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{FraudProofConfig, TraceCommitment};

use crate::commands::run::load_trace_from_file;
use crate::TraceFormat;

// ---------------------------------------------------------------------------
// chain.json — the pipeline definition (authored)
// ---------------------------------------------------------------------------

/// A chain pipeline: an ordered list of stages. Deserialized from `chain.json`.
#[derive(Debug, Deserialize)]
struct ChainSpec {
    stages: Vec<StageSpec>,
}

/// One stage: a raster project plus the binding for each of its `main`
/// parameters.
#[derive(Debug, Deserialize)]
struct StageSpec {
    name: String,
    /// Project directory, relative to the `chain.json` file.
    project: String,
    #[serde(default)]
    inputs: BTreeMap<String, InputBinding>,
}

/// How a stage parameter is fed: an external top-level input, or the single
/// output of an earlier stage.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputBinding {
    /// A top-level input, wired exactly like `run`'s `--input`/`--input-manifest`.
    External(ExternalRef),
    /// This parameter is fed from `<stage>`'s single output (v1: one output per
    /// stage). Only the commitment value carries over — under this parameter's
    /// name, which need not match the producing stage's output name.
    From(String),
}

/// An external input reference: where its bytes live and what it commits to.
#[derive(Debug, Deserialize)]
struct ExternalRef {
    /// Path to the raster payload (`*.rastered`/`output.bin`), relative to
    /// `chain.json`.
    path: String,
    /// Path to the raster index. Defaults to `path` with a `.rindex` extension.
    #[serde(default)]
    index_path: Option<String>,
    /// The value's structural commitment (hex), as it appears in an input
    /// manifest.
    commitment: String,
}

// ---------------------------------------------------------------------------
// chain-commitment — the recorded checkpoints (postcard)
// ---------------------------------------------------------------------------

/// Provenance of one stage parameter, recorded in the checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputBindingSource {
    /// Fed from a top-level external input.
    External,
    /// Fed from stage `stage`'s output (index into `ChainCommitment::stages`).
    Chained { stage: usize },
}

/// One link of the chain: which program ran, on which authorized inputs, to
/// which authorized output — the same tuple `program-identity.md` names, with
/// the output side expanded into the two commitments a link needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageCheckpoint {
    pub name: String,
    /// `sha256(domain || program.bin)` — the program's identity.
    pub program_commitment: Vec<u8>,
    /// `sha256(input_manifest bytes)` — the authorized inputs' document digest.
    pub input_manifest_commitment: Vec<u8>,
    /// Per-parameter provenance (external vs. chained).
    pub input_bindings: BTreeMap<String, InputBindingSource>,
    /// `sha256(output.bin)` == `ProgramEnd.output_commitment`. Empty for a
    /// unit-output terminal stage.
    pub output_payload_commitment: Vec<u8>,
    /// `payload_structural_root(output.bin)` == the output manifest's per-value
    /// commitment. Empty for a unit-output terminal stage.
    pub output_structural_commitment: Vec<u8>,
    /// The stage's trace commitment, tying this checkpoint to a specific
    /// (optimistically auditable) run.
    pub trace_commitment: TraceCommitment,
}

/// The chain-level object: an ordered list of stage checkpoints. A verifier
/// holding this plus each stage's program source and `output.bin` can check the
/// whole chain's links and identities with no prover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainCommitment {
    pub stages: Vec<StageCheckpoint>,
}

impl ChainCommitment {
    /// The chain digest: `sha256(postcard(self))`.
    pub fn digest(&self) -> [u8; 32] {
        let bytes = postcard::to_allocvec(self).expect("ChainCommitment is serializable");
        Sha256::digest(bytes).into()
    }
}

const CHAIN_COMMITMENT_FILE: &str = "chain-commitment";

// ---------------------------------------------------------------------------
// `cargo raster chain run <chain.json>`
// ---------------------------------------------------------------------------

/// Run every stage in order, threading each output into the next, and write a
/// `ChainCommitment` over the resulting checkpoints.
pub fn run(chain_path: &str, window_size: usize) -> Result<()> {
    let chain_file = PathBuf::from(chain_path);
    let base_dir = chain_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let spec = load_chain_spec(&chain_file)?;
    validate_stage_names(&spec)?;

    // Fail fast, before running anything, if a stage has not been built with the
    // risc0 backend (no `program.bin` to name its identity). Cheaper to catch
    // here than after a stage has already run.
    for stage in &spec.stages {
        let project = Project::new(base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        read_program_identity(&project, &stage.name)?;
    }

    let fraud_proof_config = FraudProofConfig::from_window_size(window_size)
        .map_err(|e| Error::Other(e.to_string()))?;

    let chain_dir = chains_root().join(chain_run_id());
    std::fs::create_dir_all(&chain_dir)
        .map_err(|e| Error::Other(format!("failed to create {}: {e}", chain_dir.display())))?;

    println!("chain run  {}  ({} stages)", chain_run_id_label(&chain_dir), spec.stages.len());
    println!("  dir: {}", chain_dir.display());
    println!();

    let mut checkpoints: Vec<StageCheckpoint> = Vec::new();
    let mut stage_index: BTreeMap<String, usize> = BTreeMap::new();

    let stage_count = spec.stages.len();
    for (idx, stage) in spec.stages.iter().enumerate() {
        let is_terminal = idx + 1 == stage_count;
        println!("▸ stage {}/{}  {}   ({})", idx + 1, stage_count, stage.name, stage.project);

        let project = Project::new(base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let cfs = CfsBuilder::new(&project)
            .build()
            .map_err(|e| Error::Other(format!("stage '{}': failed to build CFS: {e}", stage.name)))?;

        // Fail fast, before running anything, if a non-terminal stage produces
        // no output to feed downstream (rather than discovering it mid-chain).
        let produces_output = CfsCursor::new(cfs.clone()).main_produces_output();
        if !is_terminal && !produces_output {
            return Err(Error::Other(format!(
                "stage '{}' is non-terminal but its main returns unit — nothing to feed the next stage",
                stage.name
            )));
        }

        let stage_dir = chain_dir.join(&stage.name);
        std::fs::create_dir_all(&stage_dir)
            .map_err(|e| Error::Other(format!("failed to create {}: {e}", stage_dir.display())))?;

        // Synthesize this stage's `input.json` + `input_manifest.json` from its
        // bindings: external inputs copy through; `from` inputs resolve to the
        // producing stage's `output.bin`/`output.rindex` and its structural
        // commitment, under this parameter's name.
        let synth = synthesize_inputs(stage, &stage_dir, &base_dir, &chain_dir, &checkpoints, &stage_index)?;
        println!("    build & run …");

        let (trace, _recorder) = build_and_run_stage(
            &project,
            &cfs,
            &synth.input_json_path,
            &synth.input_manifest_path,
            &stage_dir,
        )?;

        let trace_commitment =
            TraceCommitment::try_from(&trace, &EMPTY_TRIE_NODES[0], fraud_proof_config)
                .map_err(|e| Error::Other(e.to_string()))?;
        let commit_path = stage_dir.join("commit.bin");
        std::fs::write(&commit_path, postcard::to_allocvec(&trace_commitment).unwrap())
            .map_err(|e| Error::Other(format!("failed to write {}: {e}", commit_path.display())))?;

        let (output_payload_commitment, output_structural_commitment) = if produces_output {
            let out = collect_output(&stage_dir)?;
            println!(
                "    output.bin  payload={}  structural={}",
                short_hex(&out.payload_commitment),
                short_hex(&out.structural_commitment)
            );
            (out.payload_commitment, out.structural_commitment)
        } else {
            println!("    (unit output — nothing to link downstream)");
            (Vec::new(), Vec::new())
        };

        let program_commitment = read_program_identity(&project, &stage.name)?;

        checkpoints.push(StageCheckpoint {
            name: stage.name.clone(),
            program_commitment,
            input_manifest_commitment: synth.input_manifest_commitment,
            input_bindings: synth.bindings,
            output_payload_commitment,
            output_structural_commitment,
            trace_commitment,
        });
        stage_index.insert(stage.name.clone(), idx);
        println!("    commit ✓");
        println!();
    }

    let chain = ChainCommitment { stages: checkpoints };
    let chain_commitment_path = chain_dir.join(CHAIN_COMMITMENT_FILE);
    std::fs::write(&chain_commitment_path, postcard::to_allocvec(&chain).unwrap())
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", chain_commitment_path.display())))?;

    println!("chain-commitment → {}", chain_commitment_path.display());
    println!("chain digest: {}", hex::encode(chain.digest()));
    Ok(())
}

// ---------------------------------------------------------------------------
// `cargo raster chain audit <chain.json> <chain-commitment>`
// ---------------------------------------------------------------------------

/// Verify a recorded chain's links and identities — all public, no proving.
/// Reads each stage's `output.bin` and synthesized `input_manifest.json` from
/// the chain run directory (the `chain-commitment`'s parent), and each stage's
/// `program.bin` from its project.
pub fn audit(chain_path: &str, chain_commitment_path: &str) -> Result<()> {
    let chain_file = PathBuf::from(chain_path);
    let base_dir = chain_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let spec = load_chain_spec(&chain_file)?;

    let commitment_path = PathBuf::from(chain_commitment_path);
    let chain_dir = commitment_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let chain: ChainCommitment = postcard::from_bytes(
        &std::fs::read(&commitment_path)
            .map_err(|e| Error::Other(format!("failed to read {}: {e}", commitment_path.display())))?,
    )
    .map_err(|e| Error::Other(format!("failed to decode chain-commitment: {e}")))?;

    if chain.stages.len() != spec.stages.len() {
        return Err(Error::Other(format!(
            "chain.json declares {} stages but the commitment records {}",
            spec.stages.len(),
            chain.stages.len()
        )));
    }

    let stage_index: BTreeMap<&str, usize> = spec
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    for (idx, (stage, checkpoint)) in spec.stages.iter().zip(&chain.stages).enumerate() {
        if stage.name != checkpoint.name {
            return Err(Error::Other(format!(
                "stage {idx}: chain.json names '{}' but the commitment names '{}'",
                stage.name, checkpoint.name
            )));
        }
        println!("stage {}/{}  {}", idx + 1, chain.stages.len(), stage.name);

        // 1. Identity — the checkpoint's program_commitment must equal the
        //    identity of the program declared for this stage (light mode: one
        //    sha256 over the stage's `program.bin`).
        let project = Project::new(base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let program_commitment = read_program_identity(&project, &stage.name)?;
        if program_commitment != checkpoint.program_commitment {
            return Err(Error::Other(format!(
                "stage '{}': identity fraud — program.bin hashes to {} but the checkpoint claims {}",
                stage.name,
                short_hex(&program_commitment),
                short_hex(&checkpoint.program_commitment),
            )));
        }
        println!("  identity ✓  {}", short_hex(&program_commitment));

        // 2. Output link — recompute both output hashes from the actual
        //    `output.bin` bytes and match the checkpoint (catches an artifact
        //    swapped or corrupted after the stage ran).
        if !checkpoint.output_payload_commitment.is_empty() {
            let out = collect_output(&chain_dir.join(&stage.name))?;
            if out.payload_commitment != checkpoint.output_payload_commitment {
                return Err(Error::Other(format!(
                    "stage '{}': link fraud — sha256(output.bin) does not match the checkpoint",
                    stage.name
                )));
            }
            if out.structural_commitment != checkpoint.output_structural_commitment {
                return Err(Error::Other(format!(
                    "stage '{}': link fraud — output.bin structural root does not match the checkpoint",
                    stage.name
                )));
            }
            println!("  output   ✓  payload={}", short_hex(&out.payload_commitment));
        }

        // 3. Downstream binding — for each `from` parameter, the value this
        //    stage was fed must equal the producing stage's output structural
        //    commitment. Read straight from this stage's synthesized manifest.
        let manifest = read_input_manifest(&chain_dir.join(&stage.name))?;
        for (param, binding) in &stage.inputs {
            if let InputBinding::From(producer) = binding {
                let producer_idx = *stage_index.get(producer.as_str()).ok_or_else(|| {
                    Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from unknown stage '{producer}'",
                        stage.name
                    ))
                })?;
                if producer_idx >= idx {
                    return Err(Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from '{producer}', which does not run earlier",
                        stage.name
                    )));
                }
                let expected = hex::encode(&chain.stages[producer_idx].output_structural_commitment);
                let actual = manifest.get(param).cloned().unwrap_or_default();
                if actual != expected {
                    return Err(Error::Other(format!(
                        "stage '{}': link fraud — parameter '{param}' commits {actual} but '{producer}' output is {expected}",
                        stage.name
                    )));
                }
                println!("  link     ✓  {param} ⇐ {producer}");
            }
        }
        println!();
    }

    println!(
        "chain verified ✓  ({} stages, links + identity — no proving)",
        chain.stages.len()
    );
    println!("chain digest: {}", hex::encode(chain.digest()));
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage execution
// ---------------------------------------------------------------------------

/// Build the stage project and run its binary, writing the trace to
/// `stage_dir/trace.bin` and the output artifact to `stage_dir` (via
/// `RASTER_OUTPUT_DIR`). Returns the loaded trace.
fn build_and_run_stage(
    project: &Project,
    cfs: &ControlFlowSchema,
    input_json_path: &Path,
    input_manifest_path: &Path,
    stage_dir: &Path,
) -> Result<(raster_core::trace::Trace, raster_runtime::TraceRecorder)> {
    let build_status = Command::new("cargo")
        .current_dir(&project.root_dir)
        .args(["build", "--release"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::Other(format!("failed to run cargo build: {e}")))?;
    if !build_status.success() {
        return Err(Error::Other(format!("stage build failed for {}", project.name)));
    }

    let binary_path = project.target_dir.join("release").join(&project.name);
    if !binary_path.exists() {
        return Err(Error::Other(format!(
            "binary not found at {}",
            binary_path.display()
        )));
    }

    let trace_path = stage_dir.join(TraceFormat::Binary.trace_file_name());
    let input_json = input_json_path.to_string_lossy().to_string();
    let input_manifest = input_manifest_path.to_string_lossy().to_string();

    let status = Command::new(&binary_path)
        .current_dir(&project.root_dir)
        .env(raster_runtime::TRACE_PATH_ENV, &trace_path)
        .env(
            raster_runtime::TRACE_FORMAT_ENV,
            TraceFormat::Binary.as_runtime_str(),
        )
        .env(raster_runtime::OUTPUT_DIR_ENV, stage_dir)
        .args(["--input", &input_json])
        .args(["--input-manifest", &input_manifest])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::Other(format!("failed to run stage binary: {e}")))?;
    if !status.success() {
        // A stage that errors/panics publishes no ProgramEnd and no artifact —
        // the chain halts here; nothing downstream can be attested.
        return Err(Error::Other(format!(
            "stage '{}' exited unsuccessfully ({status}) — no authorized output; chain halts",
            project.name
        )));
    }

    load_trace_from_file(
        &trace_path,
        TraceFormat::Binary,
        cfs,
        Some(&input_json),
        Some(&input_manifest),
    )
}

// ---------------------------------------------------------------------------
// Input synthesis
// ---------------------------------------------------------------------------

struct SynthesizedInputs {
    input_json_path: PathBuf,
    input_manifest_path: PathBuf,
    /// `sha256` over the exact `input_manifest.json` bytes written.
    input_manifest_commitment: Vec<u8>,
    bindings: BTreeMap<String, InputBindingSource>,
}

/// Write a stage's `input.json` (private file paths) and `input_manifest.json`
/// (commitments) into `stage_dir` from its bindings, returning their paths, the
/// manifest digest, and the per-parameter provenance.
fn synthesize_inputs(
    stage: &StageSpec,
    stage_dir: &Path,
    base_dir: &Path,
    chain_dir: &Path,
    checkpoints: &[StageCheckpoint],
    stage_index: &BTreeMap<String, usize>,
) -> Result<SynthesizedInputs> {
    let mut input_entries: Vec<(String, serde_json::Value)> = Vec::new();
    let mut manifest_entries: Vec<(String, serde_json::Value)> = Vec::new();
    let mut bindings: BTreeMap<String, InputBindingSource> = BTreeMap::new();

    for (param, binding) in &stage.inputs {
        let (path, index_path, commitment, source) = match binding {
            InputBinding::External(ext) => {
                let path = absolute(base_dir, &ext.path);
                let index_path = match &ext.index_path {
                    Some(p) => absolute(base_dir, p),
                    None => path.with_extension("rindex"),
                };
                (path, index_path, ext.commitment.clone(), InputBindingSource::External)
            }
            InputBinding::From(producer) => {
                let producer_idx = *stage_index.get(producer).ok_or_else(|| {
                    Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from stage '{producer}', which has not run",
                        stage.name
                    ))
                })?;
                let producer_dir = chain_dir.join(producer);
                let commitment = hex::encode(&checkpoints[producer_idx].output_structural_commitment);
                if checkpoints[producer_idx].output_structural_commitment.is_empty() {
                    return Err(Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from '{producer}', which produced no output",
                        stage.name
                    )));
                }
                (
                    producer_dir.join("output.bin"),
                    producer_dir.join("output.rindex"),
                    commitment,
                    InputBindingSource::Chained { stage: producer_idx },
                )
            }
        };

        input_entries.push((
            param.clone(),
            serde_json::json!({
                "path": path.to_string_lossy(),
                "index_path": index_path.to_string_lossy(),
                "load_preference": "read",
            }),
        ));
        manifest_entries.push((
            param.clone(),
            serde_json::json!({ "type": "sha256", "encoding": "raster", "commitment": commitment }),
        ));
        bindings.insert(param.clone(), source);
    }

    let input_json = serde_json::Value::Object(input_entries.into_iter().collect());
    let input_manifest = serde_json::Value::Object(manifest_entries.into_iter().collect());

    let input_json_path = stage_dir.join("input.json");
    let input_manifest_path = stage_dir.join("input_manifest.json");
    let input_manifest_bytes = serde_json::to_vec_pretty(&input_manifest)
        .map_err(|e| Error::Other(format!("failed to serialize input manifest: {e}")))?;

    std::fs::write(&input_json_path, serde_json::to_vec_pretty(&input_json).unwrap())
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", input_json_path.display())))?;
    std::fs::write(&input_manifest_path, &input_manifest_bytes)
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", input_manifest_path.display())))?;

    Ok(SynthesizedInputs {
        input_json_path,
        input_manifest_path,
        input_manifest_commitment: Sha256::digest(&input_manifest_bytes).to_vec(),
        bindings,
    })
}

// ---------------------------------------------------------------------------
// Output collection & helpers
// ---------------------------------------------------------------------------

struct StageOutput {
    payload_commitment: Vec<u8>,
    structural_commitment: Vec<u8>,
}

/// Recompute a produced stage's two link hashes from its `output.bin`, and
/// cross-check the structural root against the value the `output_manifest.json`
/// committed (so the untrusted user process could not have written a manifest
/// that disagrees with the payload).
fn collect_output(stage_dir: &Path) -> Result<StageOutput> {
    let output_bin = stage_dir.join("output.bin");
    let bytes = std::fs::read(&output_bin)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", output_bin.display())))?;

    let payload_commitment = Sha256::digest(&bytes).to_vec();
    let structural = payload_structural_root(&bytes).ok_or_else(|| {
        Error::Other(format!("{} is not a well-formed raster payload", output_bin.display()))
    })?;
    let structural_commitment = structural.to_vec();

    // Cross-check against the output manifest the runtime wrote.
    let manifest = read_output_manifest_commitment(stage_dir)?;
    if manifest != hex::encode(&structural_commitment) {
        return Err(Error::Other(format!(
            "{}: output_manifest commitment {manifest} disagrees with the recomputed structural root {}",
            stage_dir.display(),
            hex::encode(&structural_commitment)
        )));
    }

    Ok(StageOutput {
        payload_commitment,
        structural_commitment,
    })
}

/// Read the single `output` commitment (hex) from a stage's `output_manifest.json`.
fn read_output_manifest_commitment(stage_dir: &Path) -> Result<String> {
    let path = stage_dir.join("output_manifest.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", path.display())))?;
    let doc: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", path.display())))?;
    doc.get("output")
        .and_then(|v| v.get("commitment"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Other(format!("{} has no output.commitment", path.display())))
}

/// Read a stage's synthesized `input_manifest.json` as `param -> commitment hex`.
fn read_input_manifest(stage_dir: &Path) -> Result<BTreeMap<String, String>> {
    let path = stage_dir.join("input_manifest.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", path.display())))?;
    let doc: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", path.display())))?;
    let mut out = BTreeMap::new();
    if let Some(map) = doc.as_object() {
        for (name, entry) in map {
            if let Some(c) = entry.get("commitment").and_then(|v| v.as_str()) {
                out.insert(name.clone(), c.to_string());
            }
        }
    }
    Ok(out)
}

/// The program's identity, light mode: `sha256(domain || program.bin)` over the
/// `program.bin` a prior `cargo raster build --backend risc0` produced.
fn read_program_identity(project: &Project, stage_name: &str) -> Result<Vec<u8>> {
    let bin = project.output_dir.join("program.bin");
    let bytes = std::fs::read(&bin).map_err(|_| {
        Error::Other(format!(
            "stage '{stage_name}': {} not found — run `cargo raster build --backend risc0` in {} first",
            bin.display(),
            project.root_dir.display()
        ))
    })?;
    Ok(commitment_of_bytes(&bytes).to_vec())
}

fn load_chain_spec(chain_file: &Path) -> Result<ChainSpec> {
    let text = std::fs::read_to_string(chain_file)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", chain_file.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", chain_file.display())))
}

fn validate_stage_names(spec: &ChainSpec) -> Result<()> {
    if spec.stages.is_empty() {
        return Err(Error::Other("chain has no stages".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for stage in &spec.stages {
        if !seen.insert(stage.name.as_str()) {
            return Err(Error::Other(format!("duplicate stage name '{}'", stage.name)));
        }
    }
    Ok(())
}

fn absolute(base_dir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

fn chains_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target")
        .join("raster")
        .join("chains")
}

fn chain_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:020}-pid{}", now.as_nanos(), std::process::id())
}

fn chain_run_id_label(chain_dir: &Path) -> String {
    chain_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn short_hex(bytes: &[u8]) -> String {
    let s = hex::encode(bytes);
    if s.len() > 12 {
        format!("{}…", &s[..12])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chain_json_external_and_from_bindings() {
        let json = r#"{
          "stages": [
            { "name": "summarize", "project": "a",
              "inputs": {
                "personal_data": { "external": { "path": "pd.rastered", "index_path": "pd.rindex", "commitment": "aa" } },
                "seed": { "external": { "path": "seed.rastered", "commitment": "bb" } }
              } },
            { "name": "expand", "project": "b",
              "inputs": { "summary": { "from": "summarize" } } }
          ]
        }"#;
        let spec: ChainSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.stages.len(), 2);
        match &spec.stages[0].inputs["personal_data"] {
            InputBinding::External(e) => {
                assert_eq!(e.path, "pd.rastered");
                assert_eq!(e.index_path.as_deref(), Some("pd.rindex"));
                assert_eq!(e.commitment, "aa");
            }
            _ => panic!("expected external"),
        }
        // index_path is optional (defaults to `.rindex`).
        match &spec.stages[0].inputs["seed"] {
            InputBinding::External(e) => assert!(e.index_path.is_none()),
            _ => panic!("expected external"),
        }
        match &spec.stages[1].inputs["summary"] {
            InputBinding::From(s) => assert_eq!(s, "summarize"),
            _ => panic!("expected from"),
        }
    }

    #[test]
    fn duplicate_stage_names_rejected() {
        let spec = ChainSpec {
            stages: vec![
                StageSpec { name: "s".into(), project: "a".into(), inputs: BTreeMap::new() },
                StageSpec { name: "s".into(), project: "b".into(), inputs: BTreeMap::new() },
            ],
        };
        assert!(validate_stage_names(&spec).is_err());
    }

    #[test]
    fn chain_commitment_digest_is_deterministic_and_binds_stages() {
        let checkpoint = |name: &str, payload: u8| StageCheckpoint {
            name: name.into(),
            program_commitment: vec![1, 2, 3],
            input_manifest_commitment: vec![4, 5],
            input_bindings: BTreeMap::new(),
            output_payload_commitment: vec![payload],
            output_structural_commitment: vec![payload, payload],
            trace_commitment: TraceCommitment {
                fingerprint: raster_core::fingerprint::Fingerprint {
                    bits_packer: raster_core::fingerprint::BitPacker(1),
                    bits: Vec::new(),
                    len: 0,
                },
                revealed_items: Vec::new(),
            },
        };
        let a = ChainCommitment { stages: vec![checkpoint("s", 9)] };
        let b = ChainCommitment { stages: vec![checkpoint("s", 9)] };
        let c = ChainCommitment { stages: vec![checkpoint("s", 10)] };
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn absolute_joins_relative_but_keeps_absolute() {
        let base = Path::new("/base/dir");
        assert_eq!(absolute(base, "x.bin"), PathBuf::from("/base/dir/x.bin"));
        assert_eq!(absolute(base, "/abs/x.bin"), PathBuf::from("/abs/x.bin"));
    }
}
