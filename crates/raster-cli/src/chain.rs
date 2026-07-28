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

use serde::Deserialize;
use sha2::{Digest, Sha256};

use raster_compiler::{CfsBuilder, Project};
use raster_core::authorization::ManifestedInputs;
use raster_core::cfs::{CfsCursor, ControlFlowSchema};
use raster_core::chain::{
    ChainCommitment, ChainFraudEvidence, ChainFraudInput, InputBindingSource, StageCheckpoint,
};
use raster_core::input::payload_structural_root;
use raster_core::program::commitment_of_bytes;
use raster_core::trace::Trace;
use raster_core::transition::TransitionJournal;
use raster_core::{Error, Result};
use raster_prover::authorization::{authorization_guest_image_id, authorize_external_inputs};
use raster_prover::chain_fraud::{
    prove_chain_fraud, transition_guest_image_id, verify_chain_fraud_receipt,
};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::Replayer;
use raster_prover::trace::{
    FraudEvidence, FraudProofConfig, TraceCommitment, TraceCommitmentExt, TraceVerifier,
    VerificationResult,
};
use raster_runtime::TraceRecorder;

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
// The checkpoint types (`StageCheckpoint`, `ChainCommitment`,
// `InputBindingSource`) live in `raster_core::chain` so the chain-fraud guest
// can decode the same bytes this module writes.

const CHAIN_COMMITMENT_FILE: &str = "chain-commitment";
const CHAIN_FRAUD_RECEIPT_FILE: &str = "chain-fraud.receipt";

// ---------------------------------------------------------------------------
// `cargo raster chain run <chain.json>`
// ---------------------------------------------------------------------------

/// Run every stage in order, threading each output into the next, and write a
/// `ChainCommitment` over the resulting checkpoints.
pub fn run(chain: Option<&str>, window_size: usize) -> Result<()> {
    let (spec, base_dir) = resolve_chain(chain)?;
    validate_stage_names(&spec)?;

    // Fail fast, before running anything, if a stage has no resolvable program
    // identity (neither a cached `program.bin` nor a `Raster.lock` to regenerate
    // it from). Cheaper to catch here than after a stage has already run.
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
            TraceCommitment::try_build(&trace, &EMPTY_TRIE_NODES[0], fraud_proof_config)
                .map_err(|e| Error::Other(e.to_string()))?;
        let commit_path = stage_dir.join("commit.bin");
        std::fs::write(&commit_path, postcard::to_allocvec(&trace_commitment).unwrap())
            .map_err(|e| Error::Other(format!("failed to write {}: {e}", commit_path.display())))?;
        // The checkpoint carries the commitment's compact identity, not the
        // trace-sized commitment itself; `commit.bin` stays a stage artifact
        // and `chain audit` re-derives the digest from it.
        let trace_commitment_digest = trace_commitment.header().digest();

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
            trace_commitment_digest,
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

/// A recorded chain run, resolved from disk: the authored spec, the decoded
/// commitment (plus its exact bytes, which the digest and the chain-fraud
/// guest bind to), and the run directory the stage artifacts live in.
struct RecordedChain {
    spec: ChainSpec,
    base_dir: PathBuf,
    chain: ChainCommitment,
    chain_bytes: Vec<u8>,
    chain_dir: PathBuf,
}

fn load_recorded_chain(
    manifest: Option<&str>,
    chain_commitment: Option<&str>,
) -> Result<RecordedChain> {
    let (spec, base_dir) = resolve_chain(manifest)?;

    let commitment_path = match chain_commitment {
        Some(p) => PathBuf::from(p),
        None => latest_chain_commitment()?,
    };
    let chain_dir = commitment_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let chain_bytes = std::fs::read(&commitment_path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", commitment_path.display())))?;
    let chain: ChainCommitment = postcard::from_bytes(&chain_bytes)
        .map_err(|e| Error::Other(format!("failed to decode chain-commitment: {e}")))?;

    Ok(RecordedChain {
        spec,
        base_dir,
        chain,
        chain_bytes,
        chain_dir,
    })
}

/// Verify a recorded chain's links and identities — all public, no proving.
/// Reads each stage's `output.bin` and synthesized `input_manifest.json` from
/// the chain run directory (the `chain-commitment`'s parent), and resolves each
/// stage's program identity from its project (cached `program.bin`, else
/// regenerated from source + `Raster.lock`).
///
/// With `execution`, additionally re-runs every stage natively and verifies
/// the honest trace against the stage's committed `commit.bin` — the
/// challenger path that catches intra-stage execution fraud (reported, not
/// proven; `chain fraud-prove` produces the receipt).
pub fn audit(manifest: Option<&str>, chain_commitment: Option<&str>, execution: bool) -> Result<()> {
    let recorded = load_recorded_chain(manifest, chain_commitment)?;
    let RecordedChain {
        spec,
        base_dir,
        chain,
        chain_dir,
        ..
    } = &recorded;

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
        //    identity of the program declared for this stage (light mode: hash
        //    the cached `program.bin`, or regenerate it from source + Raster.lock).
        let project = Project::new(base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let program_commitment = read_program_identity(&project, &stage.name)?;
        if program_commitment != checkpoint.program_commitment {
            return Err(Error::Other(format!(
                "stage '{}': identity fraud — program identity is {} but the checkpoint claims {}",
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

        // 3. Commitment binding — the stage's `commit.bin` must be the exact
        //    artifact the checkpoint's trace_commitment_digest names. With the
        //    checkpoint carrying only the digest, this is what keeps a swapped
        //    or regenerated commitment file from standing in for the recorded
        //    run (and what a stage fraud receipt's refuted_trace_commitment
        //    is attributed against).
        let trace_commitment = read_stage_trace_commitment(&chain_dir.join(&stage.name))?;
        if trace_commitment.header().digest() != checkpoint.trace_commitment_digest {
            return Err(Error::Other(format!(
                "stage '{}': commitment fraud — commit.bin does not match the checkpoint's trace commitment digest",
                stage.name
            )));
        }
        println!(
            "  commit   ✓  {}",
            short_hex(&checkpoint.trace_commitment_digest)
        );

        // 4. Downstream binding — for each `from` parameter, the value this
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

    if execution {
        println!();
        println!("execution audit — re-running every stage against its commit.bin");
        match detect_execution_fraud(&recorded)? {
            None => println!("execution verified ✓  (no stage diverges from its committed trace)"),
            Some(fraud) => {
                println!(
                    "execution fraud ✗  stage {} '{}' diverges from its committed trace",
                    fraud.stage_index, spec.stages[fraud.stage_index].name
                );
                println!("run `cargo raster chain fraud-prove` to produce the chain fraud receipt");
                return Err(Error::Other(format!(
                    "stage '{}': execution fraud detected",
                    spec.stages[fraud.stage_index].name
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fraud: detection (`audit --execution`), proving, verification
// ---------------------------------------------------------------------------

/// Read and structurally validate a stage's `commit.bin`.
fn read_stage_trace_commitment(stage_dir: &Path) -> Result<TraceCommitment> {
    let path = stage_dir.join("commit.bin");
    let bytes = std::fs::read(&path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", path.display())))?;
    let trace_commitment: TraceCommitment = postcard::from_bytes(&bytes)
        .map_err(|e| Error::Other(format!("failed to decode {}: {e}", path.display())))?;
    trace_commitment
        .validate()
        .map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    Ok(trace_commitment)
}

/// Everything the stage-fraud prover needs about one detected execution
/// fraud: the evidence window plus the honest re-execution context it came
/// from.
struct StageExecutionFraud {
    stage_index: usize,
    evidence: FraudEvidence,
    trace: Trace,
    recorder: TraceRecorder,
    cfs: ControlFlowSchema,
    project: Project,
    trace_commitment: TraceCommitment,
    input_manifest_path: PathBuf,
}

/// The challenger path: re-run each stage natively (a deterministic honest
/// replay, into a separate `audit/` dir so the recorded artifacts stay
/// untouched) and verify the honest trace against the stage's committed
/// `commit.bin`. The first diverging stage is returned with its fraud
/// window; stages after it are not checked (one proven fault already
/// condemns the chain, and later stages consumed committed inputs).
fn detect_execution_fraud(recorded: &RecordedChain) -> Result<Option<StageExecutionFraud>> {
    for (stage_index, stage) in recorded.spec.stages.iter().enumerate() {
        let stage_dir = recorded.chain_dir.join(&stage.name);
        let project = Project::new(recorded.base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let cfs = CfsBuilder::new(&project)
            .build()
            .map_err(|e| Error::Other(format!("stage '{}': failed to build CFS: {e}", stage.name)))?;

        let audit_dir = stage_dir.join("audit");
        std::fs::create_dir_all(&audit_dir)
            .map_err(|e| Error::Other(format!("failed to create {}: {e}", audit_dir.display())))?;

        // The committed inputs: the same synthesized files the recorded run
        // was fed (chained inputs resolve to the producers' committed
        // `output.bin` artifacts by absolute path).
        let input_json_path = stage_dir.join("input.json");
        let input_manifest_path = stage_dir.join("input_manifest.json");

        println!("  stage {}/{}  {}  (re-running)", stage_index + 1, recorded.spec.stages.len(), stage.name);
        let (trace, recorder) = build_and_run_stage(
            &project,
            &cfs,
            &input_json_path,
            &input_manifest_path,
            &audit_dir,
        )?;

        let trace_commitment = read_stage_trace_commitment(&stage_dir)?;
        let mut verifier =
            TraceVerifier::new(trace_commitment.clone(), &EMPTY_TRIE_NODES[0], &cfs)
                .map_err(|e| Error::Other(e.to_string()))?;
        match verifier.verify(&trace) {
            VerificationResult::Ok => println!("    execution ✓"),
            VerificationResult::Fraud(evidence) => {
                println!("    execution ✗  committed trace diverges");
                return Ok(Some(StageExecutionFraud {
                    stage_index,
                    evidence,
                    trace,
                    recorder,
                    cfs,
                    project,
                    trace_commitment,
                    input_manifest_path,
                }));
            }
        }
    }
    Ok(None)
}

/// A `Link` fault inside the commitment: a checkpoint whose own committed
/// manifest feeds a chained parameter a value different from the producer's
/// committed output root. Returns the manifest bytes (the checkpoint's
/// preimage) so the fault can be exhibited in-guest via the authorization
/// journal.
fn detect_link_fraud(recorded: &RecordedChain) -> Result<Option<(usize, String, Vec<u8>)>> {
    for (stage_index, checkpoint) in recorded.chain.stages.iter().enumerate() {
        let chained: Vec<(&String, usize)> = checkpoint
            .input_bindings
            .iter()
            .filter_map(|(param, binding)| match binding {
                InputBindingSource::Chained { stage } => Some((param, *stage)),
                InputBindingSource::External => None,
            })
            .collect();
        if chained.is_empty() {
            continue;
        }

        let manifest_path = recorded
            .chain_dir
            .join(&checkpoint.name)
            .join("input_manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path)
            .map_err(|e| Error::Other(format!("failed to read {}: {e}", manifest_path.display())))?;
        if Sha256::digest(&manifest_bytes).to_vec() != checkpoint.input_manifest_commitment {
            // Without the checkpoint's manifest preimage no in-proof Link
            // fault can be exhibited from this artifact; the mismatch itself
            // is caught by `chain audit` as artifact inconsistency.
            continue;
        }

        let manifest = read_input_manifest(&recorded.chain_dir.join(&checkpoint.name))?;
        for (param, producer_index) in chained {
            let producer_output = &recorded.chain.stages[producer_index].output_structural_commitment;
            let actual = manifest.get(param).cloned().unwrap_or_default();
            if actual != hex::encode(producer_output) {
                return Ok(Some((stage_index, param.clone(), manifest_bytes)));
            }
        }
    }
    Ok(None)
}

/// Write the chain fraud receipt, re-verify it as a relying party would, and
/// print the journal.
fn write_chain_fraud_receipt(
    recorded: &RecordedChain,
    receipt: &risc0_zkvm::Receipt,
) -> Result<()> {
    let journal = verify_chain_fraud_receipt(receipt).map_err(|e| Error::Other(e.to_string()))?;

    let receipt_path = recorded.chain_dir.join(CHAIN_FRAUD_RECEIPT_FILE);
    let bytes = postcard::to_allocvec(receipt)
        .map_err(|e| Error::Other(format!("failed to serialize chain fraud receipt: {e}")))?;
    std::fs::write(&receipt_path, bytes)
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", receipt_path.display())))?;

    let stage_name = recorded
        .chain
        .stages
        .get(journal.faulty_stage as usize)
        .map(|s| s.name.as_str())
        .unwrap_or("?");
    println!();
    println!("chain fraud proven ✓ → {}", receipt_path.display());
    println!("  chain digest: {}", hex::encode(journal.chain_commitment_digest));
    println!(
        "  faulty stage: {} '{}'  ({:?})",
        journal.faulty_stage, stage_name, journal.fault
    );
    println!(
        "  program:      {}",
        short_hex(&journal.stage_program_commitment)
    );
    Ok(())
}

/// `cargo raster chain fraud-prove` — detect a chain fault and produce the
/// single succinct chain fraud receipt: link faults first (no re-execution
/// needed), then execution fraud via the single-program pipeline aggregated
/// by the chain-fraud guest.
pub fn fraud_prove(manifest: Option<&str>, chain_commitment: Option<&str>) -> Result<()> {
    let recorded = load_recorded_chain(manifest, chain_commitment)?;

    if let Some((stage_index, parameter, manifest_bytes)) = detect_link_fraud(&recorded)? {
        println!(
            "link fraud: stage {} '{}' parameter '{parameter}' does not commit its producer's output",
            stage_index, recorded.chain.stages[stage_index].name
        );
        let (authorization_receipt, authorization_journal) =
            authorize_external_inputs(&ManifestedInputs { manifest_bytes });
        let input = ChainFraudInput {
            chain_commitment_bytes: recorded.chain_bytes.clone(),
            faulty_stage: stage_index as u32,
            evidence: ChainFraudEvidence::Link {
                parameter,
                authorization_journal,
                authorization_image_id: authorization_guest_image_id(),
            },
        };
        let receipt = prove_chain_fraud(&input, authorization_receipt);
        return write_chain_fraud_receipt(&recorded, &receipt);
    }

    println!("no link fraud — re-running stages against their commitments");
    let Some(fraud) = detect_execution_fraud(&recorded)? else {
        println!("no fraud found — every stage matches its committed trace");
        return Ok(());
    };

    let stage_name = &recorded.spec.stages[fraud.stage_index].name;
    println!("proving stage '{stage_name}' fraud (transition guest)…");
    let backend = raster_backend_risc0::Risc0Backend::new(fraud.project.output_dir.clone())
        .with_user_crate(fraud.project.root_dir.clone());
    let replayer = Replayer::new(&backend, &fraud.project);
    let input_manifest = fraud.input_manifest_path.to_string_lossy().to_string();
    let stage_receipt = crate::commands::run::prove(
        fraud.evidence,
        &fraud.trace,
        &fraud.cfs,
        &fraud.recorder,
        &replayer,
        Some(&input_manifest),
        &fraud.trace_commitment,
    );
    let fraud_journal: TransitionJournal = stage_receipt
        .journal
        .decode()
        .map_err(|e| Error::Other(format!("failed to decode stage fraud journal: {e}")))?;

    println!("aggregating into a chain fraud receipt (chain-fraud guest)…");
    let input = ChainFraudInput {
        chain_commitment_bytes: recorded.chain_bytes.clone(),
        faulty_stage: fraud.stage_index as u32,
        evidence: ChainFraudEvidence::Execution {
            fraud_journal,
            transition_image_id: transition_guest_image_id(),
        },
    };
    let receipt = prove_chain_fraud(&input, stage_receipt);
    write_chain_fraud_receipt(&recorded, &receipt)
}

/// `cargo raster chain fraud-verify` — the relying party's checks: the
/// receipt against the pinned chain-fraud image id, the committed inner
/// image ids against the known-good guests, and the journal's chain digest
/// against the local `chain-commitment`.
pub fn fraud_verify(
    receipt_path: Option<&str>,
    manifest: Option<&str>,
    chain_commitment: Option<&str>,
) -> Result<()> {
    let recorded = load_recorded_chain(manifest, chain_commitment)?;
    let receipt_path = receipt_path
        .map(PathBuf::from)
        .unwrap_or_else(|| recorded.chain_dir.join(CHAIN_FRAUD_RECEIPT_FILE));
    let bytes = std::fs::read(&receipt_path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", receipt_path.display())))?;
    let receipt: risc0_zkvm::Receipt = postcard::from_bytes(&bytes)
        .map_err(|e| Error::Other(format!("failed to decode {}: {e}", receipt_path.display())))?;

    let journal = verify_chain_fraud_receipt(&receipt).map_err(|e| Error::Other(e.to_string()))?;

    let local_digest = recorded.chain.digest();
    if journal.chain_commitment_digest != local_digest {
        return Err(Error::Other(format!(
            "receipt condemns chain {} but the local chain-commitment digest is {}",
            hex::encode(journal.chain_commitment_digest),
            hex::encode(local_digest)
        )));
    }

    let stage_name = recorded
        .chain
        .stages
        .get(journal.faulty_stage as usize)
        .map(|s| s.name.as_str())
        .unwrap_or("?");
    println!("chain fraud receipt verified ✓");
    println!("  chain digest: {}", hex::encode(journal.chain_commitment_digest));
    println!(
        "  faulty stage: {} '{}'  ({:?})",
        journal.faulty_stage, stage_name, journal.fault
    );
    println!(
        "  program:      {}",
        short_hex(&journal.stage_program_commitment)
    );
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

/// The program's identity, light mode: `sha256(domain || program.bin)`.
///
/// `program.bin` is a pure build cache. If a prior `cargo raster build` left it
/// in the project's output dir we hash that (the fast path); otherwise we
/// regenerate the identical frame from checked-in files only — the source CFS,
/// the manifest, and the tile image ids recorded in `Raster.lock` — with no
/// guest recompilation, and hash that. So the identity depends on `Raster.lock`
/// (checked in), never on the gitignored cache file being present. See
/// `docs/proposals/program-identity.md`.
fn read_program_identity(project: &Project, stage_name: &str) -> Result<Vec<u8>> {
    let cached = project.output_dir.join("program.bin");
    if let Ok(bytes) = std::fs::read(&cached) {
        return Ok(commitment_of_bytes(&bytes).to_vec());
    }
    // Cache cold — reassemble from source + Raster.lock (no toolchain).
    let cfs = CfsBuilder::new(project)
        .build()
        .map_err(|e| Error::Other(format!("stage '{stage_name}': failed to build CFS: {e}")))?;
    let program = crate::program::reassemble_from_lock(project, &cfs)
        .map_err(|e| Error::Other(format!("stage '{stage_name}': {e}")))?;
    Ok(program.commitment().to_vec())
}

fn load_chain_spec(chain_file: &Path) -> Result<ChainSpec> {
    let text = std::fs::read_to_string(chain_file)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", chain_file.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", chain_file.display())))
}

// ---------------------------------------------------------------------------
// Manifest resolution — chain.json OR Raster.toml `[chain]`, by path or discovery
// ---------------------------------------------------------------------------

/// A `Raster.toml` that may carry a `[chain]` table. A program manifest leaves
/// `chain` absent; a chain manifest fills it — the same package-vs-virtual-
/// workspace split Cargo draws between `[package]` and `[workspace]`.
#[derive(Debug, Deserialize)]
struct RasterTomlDoc {
    #[serde(default)]
    chain: Option<ChainTable>,
}

/// The `[chain]` table: pipeline metadata plus the ordered `[[chain.stage]]`
/// list. Stages and bindings reuse the same `StageSpec`/`InputBinding` shapes
/// the JSON form uses, so both formats deserialize into one `ChainSpec`.
#[derive(Debug, Deserialize)]
struct ChainTable {
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    version: Option<String>,
    #[serde(default, rename = "stage")]
    stages: Vec<StageSpec>,
}

/// Resolve a chain spec and its (absolute) base directory from an optional path:
/// - `Some(dir)`  — a directory holding `Raster.toml` (`[chain]`) or `chain.json`.
/// - `Some(file)` — a `*.toml` (parsed as `[chain]`) or any other file (`chain.json`).
/// - `None`       — discover, walking up from the current directory.
///
/// The base dir is canonicalized, so every synthesized stage `input.json` gets
/// absolute paths regardless of the caller's working directory (a relative
/// manifest path used to leak relative input paths into the stage run dirs).
fn resolve_chain(path: Option<&str>) -> Result<(ChainSpec, PathBuf)> {
    let manifest = match path {
        Some(p) => {
            let pb = PathBuf::from(p);
            if pb.is_dir() {
                manifest_in_dir(&pb).ok_or_else(|| {
                    Error::Other(format!(
                        "no Raster.toml (with [chain]) or chain.json in {}",
                        pb.display()
                    ))
                })?
            } else {
                pb
            }
        }
        None => discover_manifest()?,
    };

    let base_dir = manifest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let base_dir = std::fs::canonicalize(&base_dir).unwrap_or(base_dir);

    let spec = load_spec(&manifest)?;
    Ok((spec, base_dir))
}

/// Load a `ChainSpec` from either a `Raster.toml` (`[chain]`) or a `chain.json`,
/// chosen by file extension.
fn load_spec(manifest: &Path) -> Result<ChainSpec> {
    if manifest.extension().and_then(|e| e.to_str()) == Some("toml") {
        let text = std::fs::read_to_string(manifest)
            .map_err(|e| Error::Other(format!("failed to read {}: {e}", manifest.display())))?;
        let doc: RasterTomlDoc = toml::from_str(&text)
            .map_err(|e| Error::Other(format!("failed to parse {}: {e}", manifest.display())))?;
        let table = doc.chain.ok_or_else(|| {
            Error::Other(format!(
                "{} has no [chain] table — it is a program manifest, not a chain",
                manifest.display()
            ))
        })?;
        Ok(ChainSpec {
            stages: table.stages,
        })
    } else {
        load_chain_spec(manifest)
    }
}

/// Prefer a chain `Raster.toml` in `dir`, else a `chain.json`.
fn manifest_in_dir(dir: &Path) -> Option<PathBuf> {
    let toml = dir.join("Raster.toml");
    if toml.is_file() && toml_has_chain(&toml) {
        return Some(toml);
    }
    let json = dir.join("chain.json");
    if json.is_file() {
        return Some(json);
    }
    None
}

/// Walk up from the current directory to find the nearest chain manifest.
fn discover_manifest() -> Result<PathBuf> {
    let start = std::env::current_dir()
        .map_err(|e| Error::Other(format!("failed to read current directory: {e}")))?;
    let mut dir = start.as_path();
    loop {
        if let Some(found) = manifest_in_dir(dir) {
            return Ok(found);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    Err(Error::Other(format!(
        "no chain manifest found in {} or any parent (looked for Raster.toml with [chain], then chain.json)",
        start.display()
    )))
}

/// Cheap check: does this `Raster.toml` carry a `[chain]` table?
fn toml_has_chain(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<RasterTomlDoc>(&text).ok())
        .map(|doc| doc.chain.is_some())
        .unwrap_or(false)
}

/// The newest chain run's `chain-commitment` under `target/raster/chains/`.
/// Run ids are timestamp-prefixed, so the lexicographic max is the most recent.
fn latest_chain_commitment() -> Result<PathBuf> {
    let root = chains_root();
    let entries = std::fs::read_dir(&root).map_err(|e| {
        Error::Other(format!(
            "no chain runs under {} ({e}) — run `cargo raster chain run` first",
            root.display()
        ))
    })?;
    let mut newest: Option<PathBuf> = None;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let candidate = entry.path();
            if newest.as_ref().map(|n| candidate > *n).unwrap_or(true) {
                newest = Some(candidate);
            }
        }
    }
    let dir = newest.ok_or_else(|| {
        Error::Other(format!(
            "no chain runs under {} — run `cargo raster chain run` first",
            root.display()
        ))
    })?;
    Ok(dir.join(CHAIN_COMMITMENT_FILE))
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
    fn parses_raster_toml_chain_table() {
        // The `[chain]` form must yield exactly the same ChainSpec as the JSON
        // form: ordered stages, external + `from` bindings, optional index_path.
        let toml = r#"
[chain]
name = "raster-pipeline"
version = "0.1.0"

[[chain.stage]]
name = "normalize"
project = "phase1-normalize"
inputs.readings  = { external = { path = "phase1-normalize/m.rastered", index_path = "phase1-normalize/m.rindex", commitment = "aa" } }
inputs.threshold = { external = { path = "phase1-normalize/t.rastered", commitment = "bb" } }

[[chain.stage]]
name = "aggregate"
project = "phase2-aggregate"
inputs.filtered = { from = "normalize" }
"#;
        let doc: RasterTomlDoc = toml::from_str(toml).unwrap();
        let table = doc.chain.expect("[chain] table present");
        let spec = ChainSpec {
            stages: table.stages,
        };
        assert_eq!(spec.stages.len(), 2);
        assert_eq!(spec.stages[0].name, "normalize");
        match &spec.stages[0].inputs["readings"] {
            InputBinding::External(e) => {
                assert_eq!(e.path, "phase1-normalize/m.rastered");
                assert_eq!(e.index_path.as_deref(), Some("phase1-normalize/m.rindex"));
                assert_eq!(e.commitment, "aa");
            }
            _ => panic!("expected external"),
        }
        // index_path is optional in TOML too.
        match &spec.stages[0].inputs["threshold"] {
            InputBinding::External(e) => assert!(e.index_path.is_none()),
            _ => panic!("expected external"),
        }
        match &spec.stages[1].inputs["filtered"] {
            InputBinding::From(s) => assert_eq!(s, "normalize"),
            _ => panic!("expected from"),
        }
    }

    #[test]
    fn program_raster_toml_has_no_chain_table() {
        // A single-program manifest must NOT be mistaken for a chain.
        let toml = r#"
[program]
name = "normalize"
version = "0.0.0"

[inputs.threshold]
type = "u64"
encoding = "raster"
"#;
        let doc: RasterTomlDoc = toml::from_str(toml).unwrap();
        assert!(doc.chain.is_none());
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
            trace_commitment_digest: vec![7; 32],
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
