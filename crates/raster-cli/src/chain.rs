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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::ser::{SerializeStructVariant, Serializer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use raster_compiler::{CfsBuilder, Project};
use raster_core::authorization::ManifestedInputs;
use raster_core::cfs::{CfsCursor, ControlFlowSchema};
use raster_core::chain::{
    ChainCommitment, ChainFraudEvidence, ChainFraudInput, ChainShape, InputBindingSource,
    RepeatResolution, StageCheckpoint,
};
use raster_core::input::{payload_structural_root, scalar_leaf_root, IndexWidth};
use raster_core::program::commitment_of_bytes;
use raster_core::trace::Trace;
use raster_core::transition::TransitionJournal;
use raster_core::{Error, Result};
use raster_prover::authorization::{authorization_guest_image_id, authorize_external_inputs};
use raster_prover::chain_fraud::{prove_chain_fraud, verify_chain_fraud_receipt};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::Replayer;
use raster_prover::trace::{
    FraudProofConfig, TraceCommitment, TraceCommitmentExt, TraceVerifier,
};
use raster_runtime::TraceRecorder;

use crate::commands::run::load_trace_from_file;
use crate::runtime_env::RuntimeEnv;
use crate::TraceFormat;

mod expand;
use expand::{
    expand, resolve, resolve_partial, spec_digest, verify_shape, PendingCount, RecordedCount,
    VerifiedCounts,
};

// ---------------------------------------------------------------------------
// chain.json — the pipeline definition (authored)
// ---------------------------------------------------------------------------

/// A chain manifest as authored: named externals plus an ordered list of items,
/// each either one stage or one repeat block. **Unexpanded** — a repeat block is
/// still a template here, and the list's length is not yet the chain's length.
///
/// `expand` turns this into a `ChainSpec`, and only a `ChainSpec` reaches the
/// run loop, the audit, or a checkpoint. The two types are kept apart on
/// purpose: a dozen sites index `ChainSpec::stages` positionally against
/// `ChainCommitment::stages`, and a single "possibly unexpanded" type would
/// make every one of them correct only if someone remembered to expand first,
/// enforced by nothing. See `docs/proposals/chain-repeat.md` §4.
#[derive(Debug, Serialize)]
struct ChainManifest {
    inputs: BTreeMap<String, ExternalRef>,
    items: Vec<ChainItem>,
}

/// One authored entry of a chain: a stage, or a block that expands to several.
///
/// They interleave, and the order between them is consensus-critical —
/// `InputBindingSource::Chained { stage }` records a producer by index into the
/// *expanded* list, so two manifests that differ only in the relative position
/// of a stage and a repeat are different chains. TOML does not preserve that
/// order across two arrays-of-tables on its own; `merge_chain_items` recovers
/// it from source spans.
///
/// Externally tagged in JSON — `{"stage": {..}}` / `{"repeat": {..}}` — matching
/// how `InputBinding` already spells its variants.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ChainItem {
    Stage(StageSpec),
    Repeat(RepeatSpec),
}

/// A chain pipeline: an ordered list of stages, plus the externals its stages
/// bind by name. The **expanded** form — what every consumer works in.
///
/// Only `expand` produces one. Nothing deserializes directly into it: a file
/// yields a `ChainManifest`, which may still be a template.
#[derive(Debug)]
struct ChainSpec {
    stages: Vec<StageSpec>,
    /// Top-level externals a stage may bind as `{ input = "<name>" }`. Empty
    /// unless the manifest declares `[chain.input]`; the indexed form is
    /// flattened into this map at load, so nothing downstream distinguishes
    /// the two spellings. See `docs/proposals/chain-repeat.md` §1.
    inputs: BTreeMap<String, ExternalRef>,
}

/// One stage: a raster project plus the binding for each of its `main`
/// parameters.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct StageSpec {
    name: String,
    /// Project directory, relative to the `chain.json` file.
    project: String,
    #[serde(default)]
    inputs: BTreeMap<String, InputBinding>,
}

/// How a stage parameter is fed: an external top-level input, or the single
/// output of an earlier stage.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum InputBinding {
    /// A top-level input, wired exactly like `run`'s `--input`/`--input-manifest`.
    External(ExternalRef),
    /// This parameter is fed from `<stage>`'s single output (v1: one output per
    /// stage). Only the commitment value carries over — under this parameter's
    /// name, which need not match the producing stage's output name.
    From(String),
    /// A top-level external declared once under `[chain.input.<name>]` and
    /// referred to here. Resolves to exactly the same `ExternalRef` an inline
    /// `external = { .. }` would, so the two are indistinguishable downstream
    /// — including in the checkpoint, where both record
    /// `InputBindingSource::External`.
    ///
    /// It exists so a commitment written once can be referenced from several
    /// places, which is what a repeat block needs: 35 layer files declared in
    /// one indexed block and bound as `{ input = "layer_{l}" }`.
    Input(String),
}

/// A `[[chain.repeat]]` block: a stage template repeated `count` times.
///
/// Parsed here, expanded in `chain::expand`. See `docs/proposals/chain-repeat.md` §2.
#[derive(Debug, Deserialize, Serialize)]
struct RepeatSpec {
    /// Names the block, for `{ from = "<name>.<export>" }` and for the
    /// `RepeatResolution` recorded in the chain commitment.
    name: String,
    /// The index bound inside the block, substituted as `{<index>}`.
    index: String,
    /// First value of the index. 0-based, matching the `l0`-based naming every
    /// layer stage already uses; `start` exists because a segment sometimes
    /// begins partway (`start = 15, count = 20` is layers 15..34).
    #[serde(default)]
    start: u32,
    count: CountSource,
    /// The templated stages, in declaration order.
    #[serde(default, rename = "stage", alias = "stages")]
    stages: Vec<RepeatStageSpec>,
    /// What outside stages may bind to. Names, not positions.
    #[serde(default)]
    exports: BTreeMap<String, ExportDecl>,
}

/// Serialize the untagged parse enums with an explicit discriminant.
///
/// `CountSource` and `RepeatBinding` are `#[serde(untagged)]` because that is
/// what reads well in TOML — `count = 35` beside `count = { from = .. }`, with
/// no discriminant key. Untagged **serialization**, though, writes the variant's
/// fields and nothing else, so two different variants can encode to the same
/// bytes. That is harmless for a config file and unacceptable for
/// `ChainShape::spec_digest`, whose entire job is to distinguish one manifest
/// from another.
///
/// So these two are written by hand, externally tagged, as the derive would do
/// without `untagged`. Nothing deserializes the result — it exists only to be
/// hashed.
macro_rules! serialize_struct_variant {
    ($serializer:expr, $name:literal, $index:expr, $variant:literal, $($field:literal => $value:expr),+ $(,)?) => {{
        let mut sv = $serializer.serialize_struct_variant(
            $name,
            $index,
            $variant,
            [$($field),+].len(),
        )?;
        $(sv.serialize_field($field, $value)?;)+
        sv.end()
    }};
}

impl Serialize for CountSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        match self {
            CountSource::Literal(n) => {
                serializer.serialize_newtype_variant("CountSource", 0, "literal", n)
            }
            CountSource::Input { input, select, max } => serialize_struct_variant!(
                serializer, "CountSource", 1, "input",
                "input" => input, "select" => select, "max" => max,
            ),
            CountSource::Stage { from, max } => serialize_struct_variant!(
                serializer, "CountSource", 2, "stage",
                "from" => from, "max" => max,
            ),
        }
    }
}

impl Serialize for RepeatBinding {
    fn serialize<S: Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        match self {
            RepeatBinding::From { from, first } => serialize_struct_variant!(
                serializer, "RepeatBinding", 0, "from",
                "from" => from, "first" => first,
            ),
            RepeatBinding::Input { input } => serialize_struct_variant!(
                serializer, "RepeatBinding", 1, "input",
                "input" => input,
            ),
            RepeatBinding::External { external } => serialize_struct_variant!(
                serializer, "RepeatBinding", 2, "external",
                "external" => external,
            ),
        }
    }
}

/// Where a repeat block's trip count comes from.
///
/// `select` is available only on the `{ input = .. }` form: a stage-produced
/// count is the producing stage's *whole* output, so that a shape fault stays
/// one hash in-guest rather than a payload decoder (§3, §6.1).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CountSource {
    /// `count = 35`
    Literal(u32),
    /// `count = { input = "sampling", select = "max_new_tokens", max = 128 }`
    Input {
        input: String,
        #[serde(default)]
        select: String,
        max: u32,
    },
    /// `count = { from = "plan_generation", max = 128 }`
    Stage { from: String, max: u32 },
}

/// One templated stage inside a repeat block. May carry its own inner index,
/// whose count is always a literal — an inner fan is a property of the code.
#[derive(Debug, Deserialize, Serialize)]
struct RepeatStageSpec {
    name: String,
    project: String,
    #[serde(default)]
    index: Option<String>,
    #[serde(default)]
    start: u32,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    inputs: BTreeMap<String, RepeatBinding>,
}

/// A binding inside a repeat block: an `InputBinding` plus the `first` fallback
/// a `{ident-1}` template needs at the block's entry edge.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepeatBinding {
    From {
        from: String,
        /// What `{ident-1}` resolves to at `ident == start`. Mandatory whenever
        /// the template contains `{ident-1}` — that is the underflow case, and
        /// exactly the edge that mis-wires when hand-written.
        #[serde(default)]
        first: Option<String>,
    },
    Input {
        input: String,
    },
    External {
        external: ExternalRef,
    },
}

/// `[chain.repeat.exports.<name>]` — the stage an outside binding reaches, and
/// the one it falls back to when the block runs zero times.
#[derive(Debug, Deserialize, Serialize)]
struct ExportDecl {
    stage: String,
    /// Used when `count == 0`; per export, because a block emits several stages
    /// per iteration and each export may fall back somewhere different.
    entry: String,
}

/// An external input reference: where its bytes live and what it commits to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
///
/// With `no_auth`, every stage runs unauthenticated
/// (`docs/proposals/unauthenticated-execution.md`): no trace, therefore no
/// per-stage trace commitment and no chain-commitment. The linking is
/// unchanged — a stage still produces a real `output.bin` (§6.1) whose
/// structural root is hashed here, host-side, and fed to the next stage — so
/// the chain computes the same values, it just attests nothing about how.
/// This is the dev-loop half of §10; the other half, per-stage commitment on
/// demand for a contested stage, is still chain-level policy and unaddressed:
/// this flag is all-or-nothing.
pub fn run(
    chain: Option<&str>,
    window_size: usize,
    no_auth: bool,
    stage_selection: Option<&str>,
    run_dir: Option<&str>,
    trace_format: TraceFormat,
) -> Result<()> {
    let (manifest, base_dir) = resolve_chain(chain)?;

    let fraud_proof_config =
        FraudProofConfig::from_window_size(window_size).map_err(|e| Error::Other(e.to_string()))?;

    let root = if no_auth {
        no_auth_chains_root()
    } else {
        chains_root()
    };
    let minting = run_dir.is_none() && stage_selection.is_none();
    let chain_dir = resolve_run_dir(&root, run_dir, stage_selection.is_some())?;
    std::fs::create_dir_all(&chain_dir)
        .map_err(|e| Error::Other(format!("failed to create {}: {e}", chain_dir.display())))?;
    // Only a fresh whole-chain run moves `latest`. A `--stage` run works inside
    // an existing directory; repointing from there would turn `latest` into
    // "most recently touched" and let a run against an older directory drag the
    // pointer backwards.
    //
    // Written in the authenticated root too, now that `--stage` is legal there:
    // that is how a single-stage re-run finds the directory to work in. It
    // cannot confuse `latest_chain_commitment`'s newest-dir discovery, which
    // counts only entries whose own file type is a directory — a symlink (or
    // the text-file fallback) is neither.
    if minting {
        write_latest(&root, &chain_dir);
    }

    match stage_selection {
        Some(name) => println!(
            "chain run  {}  (stage '{name}')",
            chain_run_id_label(&chain_dir),
        ),
        None => println!("chain run  {}", chain_run_id_label(&chain_dir)),
    }
    println!("  dir: {}", chain_dir.display());
    // Printed either way: a reader must never have to infer which mode produced
    // the output in front of them. Mirrors `commands::run`.
    if no_auth {
        println!("  mode: unauthenticated (--no-auth) — results are not authoritative");
    } else {
        println!("  mode: authenticated");
    }
    println!();

    // Counts already resolved by an earlier invocation, if any. **Only a
    // `--stage` re-run inherits them**: it works inside an existing run
    // directory, executes one stage, and cannot re-derive a stage-produced count
    // without re-running the stage that produced it.
    //
    // A whole-chain run never does, not even one pointed at an existing
    // directory with `--run`: it starts at the first stage, so every
    // stage-produced count is re-derived by executing its producer. Seeding it
    // from the sidecar there would let a count recorded by a previous run
    // outrank the one this run just produced — and since a whole-chain run
    // *writes a commitment*, that is how a chain-commitment gets minted whose
    // `resolved_count` contradicts the producing stage's own recorded output.
    let recorded = if stage_selection.is_some() {
        read_chain_shape(&chain_dir, &manifest)
    } else {
        RecordedShape::default()
    };
    let mut known = recorded.counts;
    let mut partial = resolve_partial(&manifest, &known)?;

    if stage_selection.is_some() && partial.pending.is_some() {
        let pending = partial.pending.expect("checked above");
        // Said explicitly, because otherwise this reads as "that stage never
        // ran" when what happened is that it ran against a manifest which has
        // since been edited.
        let discarded = if recorded.stale {
            "\n  (that directory holds a recorded shape resolved from a different manifest, \
             so its counts describe another chain and were discarded)"
        } else {
            ""
        };
        return Err(Error::Other(format!(
            "this chain's shape is not fully known: repeat block '{}' takes its count from \
             stage '{}', which has not run in {}.{discarded}\n  run the whole chain once \
             first: cargo raster chain run{}",
            pending.block,
            pending.from,
            chain_dir.display(),
            if no_auth { " --no-auth" } else { "" },
        )));
    }

    let mut checkpoints: Vec<StageCheckpoint> = Vec::new();
    // Every stage's output structural commitment, by stage index — what a
    // downstream `from` binding resolves to. `None` means "no artifact", which
    // covers both "has not run" and "ran but returns unit". Kept beside
    // `checkpoints` rather than read out of them, because an unauthenticated
    // stage produces this and no checkpoint.
    let mut outputs: Vec<Option<Vec<u8>>> = Vec::new();
    // Wall clock of each stage binary. Reported only — never part of a
    // `StageCheckpoint`, which `chain audit` and the chain-fraud guest digest
    // and which must stay identical across runs.
    let mut execution_times: Vec<(String, Duration)> = Vec::new();

    // How far execution has got. Everything below this index has run, in this
    // invocation or a previous one.
    let mut cursor = 0usize;
    let mut commit_chain = true;
    let mut repeats: Vec<RepeatResolution> = Vec::new();

    // One round per stage-produced count, plus one. A chain whose shape is
    // fully known from its manifest — every chain before this feature — runs
    // exactly one round and behaves precisely as it did.
    loop {
        let spec = &partial.spec;
        validate_spec(spec)?;
        repeats = partial.repeats.clone();
        outputs.resize(spec.stages.len(), None);

        let stage_index: BTreeMap<String, usize> = spec
            .stages
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();

        let selected: Vec<usize> = match stage_selection {
            Some(name) => vec![*stage_index.get(name).ok_or_else(|| {
                Error::Other(format!(
                    "no stage named '{name}' in this chain — stages are: {}",
                    summarize_names(
                        &spec.stages.iter().map(|s| s.name.as_str()).collect::<Vec<_>>()
                    )
                ))
            })?],
            None => (cursor..spec.stages.len()).collect(),
        };

        if !preflight_identity(spec, &selected, &base_dir, no_auth)? {
            commit_chain = false;
        }

        if let Some(&only) = selected.first().filter(|_| stage_selection.is_some()) {
            // The producers this stage is fed from already ran; recover what
            // they committed straight from their artifacts. Everything after it
            // is now stale and goes.
            rehydrate_producers(spec, &selected, &stage_index, &chain_dir, &mut outputs)?;
            invalidate_downstream(&chain_dir, spec, only)?;
        }

        let stage_count = spec.stages.len();
        for &idx in &selected {
        let stage = &spec.stages[idx];
        // Only meaningful once the shape is settled: mid-expansion the last
        // stage of the partial list is not the chain's last, and treating it as
        // terminal would let a unit-output stage through with nothing to feed
        // the block that follows it.
        let is_terminal = partial.pending.is_none() && idx + 1 == stage_count;
        println!(
            "▸ stage {}/{}  {}   ({}){}",
            idx + 1,
            stage_count,
            stage.name,
            stage.project,
            if stage_selection.is_some() {
                "   (re-run in place)"
            } else {
                ""
            }
        );

        let project = Project::new(base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let cfs = CfsBuilder::new(&project).build().map_err(|e| {
            Error::Other(format!("stage '{}': failed to build CFS: {e}", stage.name))
        })?;

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
        let synth = synthesize_inputs(
            stage,
            &stage_dir,
            &base_dir,
            &chain_dir,
            &outputs,
            &stage_index,
            &spec.inputs,
        )?;
        println!("    build & run …");

        let (replay, execution_time) = build_and_run_stage(
            &project,
            &cfs,
            &synth.input_json_path,
            &synth.input_manifest_path,
            &stage_dir,
            no_auth,
            trace_format,
        )?;
        execution_times.push((stage.name.clone(), execution_time));
        println!("    exec {}", format_duration(execution_time));

        // A recorded trace still yields a `commit.bin`, but nothing in the
        // checkpoint names it any more: it is this stage's *dispute* artifact,
        // the thing a divergence is proven against, and it is needed only for a
        // stage somebody contests. See `docs/proposals/chain-io-commitment.md`.
        if let Some((trace, _recorder)) = replay {
            let trace_commitment =
                TraceCommitment::try_build(&trace, &EMPTY_TRIE_NODES[0], fraud_proof_config)
                    .map_err(|e| Error::Other(e.to_string()))?;
            let commit_path = stage_dir.join("commit.bin");
            std::fs::write(
                &commit_path,
                postcard::to_allocvec(&trace_commitment).unwrap(),
            )
            .map_err(|e| {
                Error::Other(format!("failed to write {}: {e}", commit_path.display()))
            })?;
        }

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

        outputs[idx] = if produces_output {
            Some(output_structural_commitment.clone())
        } else {
            None
        };

        // Every field here is a pure function of public artifacts, so this is
        // the same checkpoint in either posture — the property `chain audit`
        // and the equivalence test both rest on.
        if commit_chain {
            let program_commitment = program_identity_with_cfs(&project, &cfs, &stage.name)?;
            checkpoints.push(StageCheckpoint {
                name: stage.name.clone(),
                program_commitment,
                input_manifest_commitment: synth.input_manifest_commitment,
                input_bindings: synth.bindings,
                output_payload_commitment,
                output_structural_commitment,
            });
            println!("    commit ✓");
        }
        println!();
        }

        cursor = spec.stages.len();
        let Some(pending) = partial.pending.take() else {
            break;
        };
        if stage_selection.is_some() {
            break;
        }

        // The producing stage has just run. Its whole output *is* the count.
        let (width, count) = read_trip_count(&chain_dir, &pending)?;
        println!(
            "  repeat '{}': {count} iteration(s), from stage '{}'",
            pending.block, pending.from
        );
        println!();
        known.insert(
            pending.block.clone(),
            RecordedCount {
                width,
                count,
                source_stage: pending.source_stage,
            },
        );

        // Re-expand from the manifest rather than splicing, then check that the
        // part already executed did not move. Expansion does no I/O and the
        // cursor only advances, so nothing can be re-run; what this catches is
        // an expansion that is not a function of its inputs, which would
        // silently invalidate the positional producer indices in every
        // checkpoint written so far.
        let next = resolve_partial(&manifest, &known)?;
        if next.spec.stages[..cursor.min(next.spec.stages.len())] != spec.stages[..cursor] {
            return Err(Error::Other(format!(
                "expansion is not stable: resolving repeat block '{}' changed stages that had \
                 already run. This is a bug in chain expansion, not in the manifest",
                pending.block
            )));
        }
        partial = next;
    }

    // Written before any of the early returns below: the shape is what a later
    // `--stage` re-run needs to reconstruct this directory's stage list, and
    // that is true whether or not a chain-commitment was produced.
    write_chain_shape(&chain_dir, &manifest, &repeats)?;

    // The interlock that used to live here — no trace, therefore no
    // checkpoint — is gone: a checkpoint names only inputs and outputs, which
    // both postures produce identically, so an unauthenticated run now writes
    // a real chain-commitment. What it does *not* write is any `commit.bin`,
    // so no stage of it can be disputed until that stage is re-run with
    // `--stage`. The only thing that still suppresses the commitment is an
    // unresolvable program identity (see `commit_chain` above).
    if !commit_chain {
        print_execution_times(&execution_times);
        return Ok(());
    }

    // A chain-commitment is a statement about a *whole* chain: the digest
    // folds every stage. A single-stage run has one checkpoint, so writing one
    // here would replace the recorded commitment with an impostor that audits
    // against a chain nobody ran. The stage's `commit.bin` is already on disk —
    // which is the entire point of an authenticated `--stage` run, and what a
    // dispute over that stage needs. See `docs/proposals/chain-io-commitment.md`.
    if stage_selection.is_some() {
        println!("stage re-run complete — commit.bin written, chain-commitment left untouched");
        print_execution_times(&execution_times);
        return Ok(());
    }

    let chain = ChainCommitment {
        stages: checkpoints,
        shape: ChainShape {
            spec_digest: spec_digest(&manifest),
            repeats,
        },
    };
    let chain_commitment_path = chain_dir.join(CHAIN_COMMITMENT_FILE);
    std::fs::write(
        &chain_commitment_path,
        postcard::to_allocvec(&chain).unwrap(),
    )
    .map_err(|e| {
        Error::Other(format!(
            "failed to write {}: {e}",
            chain_commitment_path.display()
        ))
    })?;

    println!("chain-commitment → {}", chain_commitment_path.display());
    println!("chain digest: {}", hex::encode(chain.digest()));
    print_execution_times(&execution_times);
    Ok(())
}

/// Where a run records the counts it resolved.
///
/// Written unconditionally, beside the chain-commitment but not part of it. A
/// `--stage` re-run has to reconstruct the same stage list the directory was
/// built with, and a stage-produced count cannot be re-derived without
/// re-running its producer — so the shape has to be on disk even when the
/// commitment is not. It is a convenience for this run directory, never
/// evidence: everything a *verifier* needs is in `ChainCommitment::shape`.
///
/// It holds a `ChainShape` — the same `spec_digest` + resolutions the commitment
/// carries, and for the same reason. A count is only meaningful for the manifest
/// it was resolved from, and the resolutions are keyed by block *name*, which
/// survives an edit that changes everything else about the block.
const CHAIN_SHAPE_FILE: &str = "chain-shape";

/// What a run directory's recorded shape says about the manifest in hand.
#[derive(Debug, Default)]
struct RecordedShape {
    /// Stage-produced counts by block name. Empty unless the sidecar was
    /// resolved from this very manifest.
    counts: BTreeMap<String, RecordedCount>,
    /// A sidecar was there and decoded, but records a different manifest — the
    /// difference between "not resolved yet" and "resolved for another chain",
    /// which the caller reports rather than acts on.
    stale: bool,
}

/// Read back the counts an earlier invocation resolved in this run directory.
///
/// A missing or unreadable sidecar is not an error — it means "nothing resolved
/// yet", which is the normal state of a fresh run. Neither is one belonging to
/// another manifest: it is discarded, and the caller's refusal explains why.
/// Both land in the same place, an unresolved shape, which every consumer of
/// this already handles — including a sidecar written before it carried a
/// digest, which lands in one or the other and costs that directory one
/// whole-chain run before `--stage` works in it again.
fn read_chain_shape(chain_dir: &Path, manifest: &ChainManifest) -> RecordedShape {
    let Ok(bytes) = std::fs::read(chain_dir.join(CHAIN_SHAPE_FILE)) else {
        return RecordedShape::default();
    };
    let Ok(recorded) = postcard::from_bytes::<ChainShape>(&bytes) else {
        return RecordedShape::default();
    };
    if recorded.spec_digest != spec_digest(manifest) {
        return RecordedShape {
            counts: BTreeMap::new(),
            stale: true,
        };
    }

    RecordedShape {
        counts: recorded
            .repeats
            .into_iter()
            .filter_map(|resolution| {
                // A literal count is manifest-static and re-derived by
                // expansion; only a stage-produced one is unrecoverable without
                // re-running its producer, and `source_stage` is what tells the
                // two apart.
                let source_stage = resolution.source_stage?;
                Some((
                    resolution.name,
                    RecordedCount {
                        width: resolution.width,
                        count: resolution.resolved_count,
                        source_stage,
                    },
                ))
            })
            .collect(),
        stale: false,
    }
}

fn write_chain_shape(
    chain_dir: &Path,
    manifest: &ChainManifest,
    repeats: &[RepeatResolution],
) -> Result<()> {
    let path = chain_dir.join(CHAIN_SHAPE_FILE);
    let shape = ChainShape {
        spec_digest: spec_digest(manifest),
        repeats: repeats.to_vec(),
    };
    std::fs::write(&path, postcard::to_allocvec(&shape).unwrap())
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", path.display())))
}

/// Read a repeat block's trip count out of the stage that produced it.
///
/// The stage's whole output is the count (`docs/proposals/chain-repeat.md` §3),
/// so this is a decode of one fixed-width unsigned leaf and nothing else —
/// which is what keeps the *verifier's* side of this to a single hash, with no
/// payload parsing in a guest.
fn read_trip_count(chain_dir: &Path, pending: &PendingCount) -> Result<(IndexWidth, u32)> {
    let path = chain_dir.join(&pending.from).join("output.bin");
    let bytes = std::fs::read(&path).map_err(|e| {
        Error::Other(format!(
            "repeat block '{}': cannot read the count from stage '{}' ({}): {e}",
            pending.block,
            pending.from,
            path.display(),
        ))
    })?;

    let (width, value) = raster_core::input::parse_scalar_leaf(&bytes).ok_or_else(|| {
        Error::Other(format!(
            "repeat block '{}': stage '{}' does not return a plain unsigned integer, so it \
             cannot supply a trip count. A count-producing stage's `main` must return `uN` and \
             nothing else",
            pending.block, pending.from,
        ))
    })?;

    // Bounded from the manifest, before any expansion happens, so a hostile or
    // corrupt artifact cannot ask for 10^9 stages. Refused rather than clamped:
    // clamping would make two different honest counts produce the same chain.
    if value > u64::from(pending.max) {
        return Err(Error::Other(format!(
            "repeat block '{}': stage '{}' asks for {value} iterations, over the manifest's \
             max of {}",
            pending.block, pending.from, pending.max,
        )));
    }

    Ok((width, value as u32))
}

/// Fail fast, before running anything, if a stage has no resolvable program
/// identity (neither a cached `program.bin` nor a `Raster.lock` to regenerate
/// it from). Cheaper to catch here than after a stage has already run.
///
/// A checkpoint now costs no trace, so **both** postures produce one and both
/// need identity. But the unauthenticated mode exists precisely for the case
/// where identity cannot be resolved — a source change whose `Raster.lock` has
/// not been rebuilt — so there it degrades instead of refusing: the run proceeds
/// and writes no chain-commitment. Authenticated still errors, because a run
/// that cannot name its programs cannot produce the object it was asked for.
///
/// Returns whether a chain-commitment can still be written.
fn preflight_identity(
    spec: &ChainSpec,
    selected: &[usize],
    base_dir: &Path,
    no_auth: bool,
) -> Result<bool> {
    for &idx in selected {
        let stage = &spec.stages[idx];
        let stage_root = base_dir.join(&stage.project);
        if !program_identity_resolvable(&stage_root) {
            if !no_auth {
                return Err(Error::Other(format!(
                    "stage '{}': no program identity — neither a cached `program.bin` nor a \
                     `Raster.lock` to rebuild one from. Run `cargo raster build` in {}",
                    stage.name,
                    stage_root.display(),
                )));
            }
            println!(
                "  no chain-commitment: stage '{}' has no resolvable program identity",
                stage.name
            );
            return Ok(false);
        }
    }
    Ok(true)
}

/// Per-stage execution time — the stage binary's own wall clock, not the build
/// that precedes it or the commitment work that follows. The total is the sum
/// of the stages, so it is deliberately less than the command's runtime.
fn print_execution_times(execution_times: &[(String, Duration)]) {
    if execution_times.is_empty() {
        return;
    }

    let name_width = execution_times
        .iter()
        .map(|(name, _)| name.len())
        .chain(std::iter::once("stage".len()))
        .max()
        .unwrap_or_default();

    println!();
    println!("{:<name_width$}  {:>10}", "stage", "exec");
    for (name, duration) in execution_times {
        println!("{:<name_width$}  {:>10}", name, format_duration(*duration));
    }
    let total: Duration = execution_times.iter().map(|(_, d)| *d).sum();
    println!("{:<name_width$}  {:>10}", "total", format_duration(total));
}

/// Human-readable duration: sub-second in milliseconds, above that in seconds.
fn format_duration(duration: Duration) -> String {
    if duration.as_secs() == 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.2}s", duration.as_secs_f64())
    }
}

// ---------------------------------------------------------------------------
// `cargo raster chain audit <chain.json> <chain-commitment>`
// ---------------------------------------------------------------------------

/// A recorded chain run, resolved from disk: the authored spec, the decoded
/// commitment (plus its exact bytes, which the digest and the chain-fraud
/// guest bind to), and the run directory the stage artifacts live in.
struct RecordedChain {
    /// Expanded — paired positionally with `chain.stages`.
    spec: ChainSpec,
    base_dir: PathBuf,
    chain: ChainCommitment,
    chain_bytes: Vec<u8>,
    chain_dir: PathBuf,
}

/// Whether loading a recorded chain should insist its shape is honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapePolicy {
    /// Re-derive every count and refuse a chain whose shape disagrees. What a
    /// verifier wants: a bad shape is a verdict, reached with no prover.
    Verify,
    /// Expand using the counts the commitment records, whatever they say. What
    /// a fraud *prover* needs — it must reconstruct the chain the claimer
    /// asserts in order to exhibit what is wrong with it, and refusing to load
    /// it would make the fault unprovable.
    AsClaimed,
}

fn load_recorded_chain(
    manifest: Option<&str>,
    chain_commitment: Option<&str>,
    shape_policy: ShapePolicy,
) -> Result<RecordedChain> {
    let (manifest, base_dir) = resolve_chain(manifest)?;

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

    // The stage list every consumer below indexes is *derived*, not read out of
    // the commitment: the counts are checked against the manifest and against
    // the producing stages' own commitments first, and only then is the
    // manifest re-expanded. Nothing here executes a stage.
    let counts = match shape_policy {
        ShapePolicy::Verify => verify_shape(&manifest, &chain)?,
        ShapePolicy::AsClaimed => VerifiedCounts::as_claimed(&chain),
    };
    let spec = expand(&manifest, &counts)?;

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
pub fn audit(
    manifest: Option<&str>,
    chain_commitment: Option<&str>,
    execution: bool,
) -> Result<()> {
    let recorded = load_recorded_chain(manifest, chain_commitment, ShapePolicy::Verify)?;
    let RecordedChain {
        spec,
        base_dir,
        chain,
        chain_dir,
        ..
    } = &recorded;

    // `spec.stages` is now *derived* — `load_recorded_chain` verified the
    // recorded counts against the manifest and against the producing stages'
    // own commitments, then re-expanded. So this compares the length the chain
    // must have against the length it claims, rather than one declaration
    // against another.
    if chain.stages.len() != spec.stages.len() {
        return Err(Error::Other(format!(
            "shape fraud — the manifest expands to {} stages but the commitment records {}",
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
            println!(
                "  output   ✓  payload={}",
                short_hex(&out.payload_commitment)
            );
        }

        // (The commitment-binding check that used to sit here is gone with the
        //  checkpoint field it compared: a checkpoint no longer names a
        //  `commit.bin`, so there is no binding to verify. Execution is checked
        //  by re-running the stage and comparing its output — `--execution`
        //  below — not by matching a recorded trace.)

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
                let expected =
                    hex::encode(&chain.stages[producer_idx].output_structural_commitment);
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
        println!("execution audit — re-running every stage against its committed output");
        match detect_output_fraud(&recorded)? {
            None => {
                println!("execution verified ✓  (every stage reproduces its committed output)")
            }
            Some(fraud) => {
                println!(
                    "execution fraud ✗  stage {} '{}' does not reproduce its committed output",
                    fraud.stage_index, spec.stages[fraud.stage_index].name
                );
                println!("    committed  {}", short_hex(&fraud.committed));
                println!("    recomputed {}", short_hex(&fraud.recomputed));
                println!("run `cargo raster chain fraud-prove` to produce the evidence receipt");
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

/// Fraud-proof window the auditor builds its own commitment with.
///
/// The auditor picks this, not the chain: a checkpoint no longer records a
/// window size because it no longer records a trace commitment at all. It only
/// has to be a legal window (`FraudProofConfig::from_window_size`) shorter than
/// the stage's trace — the security margin is fixed at
/// `FRAUD_DETECTION_SECURITY_BITS` per window however the size is split.
const EVIDENCE_WINDOW_SIZE: usize = 2;

/// A stage whose recomputed output disagrees with what the chain committed.
struct StageOutputFraud {
    stage_index: usize,
    committed: Vec<u8>,
    recomputed: Vec<u8>,
}

/// The challenger's scan: re-run every stage **unauthenticated** and compare
/// the output it actually produces against the output the checkpoint claims.
///
/// Cheap by construction, and that is the point. Execution is a pure function
/// of the committed program and the committed inputs, so a stage whose honest
/// re-run yields a different `output.bin` is a stage whose checkpoint is
/// false — no trace, no commitment and no proving needed to *find* it. Only
/// arguing about it costs anything, and only for the one stage
/// (`prove_terminal_window`).
///
/// The first disagreeing stage is returned; later stages are not checked, since
/// one false checkpoint already condemns the chain and the stages after it
/// consumed committed inputs.
fn detect_output_fraud(recorded: &RecordedChain) -> Result<Option<StageOutputFraud>> {
    // `audit` checks this before calling; `fraud_prove` does not, and indexing
    // the checkpoints by a spec position would panic rather than report. The
    // mismatch is itself a finding, so it gets a message rather than an
    // assertion.
    if recorded.chain.stages.len() != recorded.spec.stages.len() {
        return Err(Error::Other(format!(
            "the chain manifest declares {} stages but the commitment records {} — \
             they describe different chains",
            recorded.spec.stages.len(),
            recorded.chain.stages.len(),
        )));
    }

    for (stage_index, stage) in recorded.spec.stages.iter().enumerate() {
        let checkpoint = &recorded.chain.stages[stage_index];
        // A unit-output terminal stage commits no output, so there is nothing
        // to disagree with. It also feeds nothing downstream, so it cannot
        // change the chain's result.
        if checkpoint.output_payload_commitment.is_empty() {
            println!(
                "  stage {}/{}  {}  (unit output — nothing committed to check)",
                stage_index + 1,
                recorded.spec.stages.len(),
                stage.name
            );
            continue;
        }

        let stage_dir = recorded.chain_dir.join(&stage.name);
        let project = Project::new(recorded.base_dir.join(&stage.project))
            .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
        let cfs = CfsBuilder::new(&project).build().map_err(|e| {
            Error::Other(format!("stage '{}': failed to build CFS: {e}", stage.name))
        })?;

        let audit_dir = stage_dir.join("audit");
        std::fs::create_dir_all(&audit_dir)
            .map_err(|e| Error::Other(format!("failed to create {}: {e}", audit_dir.display())))?;

        println!(
            "  stage {}/{}  {}  (re-running)",
            stage_index + 1,
            recorded.spec.stages.len(),
            stage.name
        );
        // Unauthenticated: `output.bin` is byte-identical either way
        // (`unauthenticated-execution.md` §6.1), and the trace this would
        // otherwise record is not what the comparison reads.
        let (_replay, _execution_time) = build_and_run_stage(
            &project,
            &cfs,
            &stage_dir.join("input.json"),
            &stage_dir.join("input_manifest.json"),
            &audit_dir,
            true,
            TraceFormat::Binary,
        )?;

        let recomputed = collect_output(&audit_dir)?;
        if recomputed.payload_commitment != checkpoint.output_payload_commitment {
            println!("    output ✗  recomputed {}", short_hex(&recomputed.payload_commitment));
            return Ok(Some(StageOutputFraud {
                stage_index,
                committed: checkpoint.output_payload_commitment.clone(),
                recomputed: recomputed.payload_commitment,
            }));
        }
        println!("    output ✓");
    }
    Ok(None)
}

/// Re-run one stage **authenticated** and prove its terminal window.
///
/// The receipt says: *honest execution of this checkpoint's committed program,
/// on its committed inputs, terminates in `output_commitment`.* Paired with the
/// checkpoint's disagreeing `output_payload_commitment`, that is the evidence a
/// stage's execution is fraudulent.
///
/// It is **evidence, not a self-contained fraud proof**: nothing in it shows the
/// trace it was proven over is the honest one, which is what the
/// challenge/response timeout supplies (`chain-io-commitment.md` §3). For a
/// local auditor — the same party proving and checking — that gap costs
/// nothing. The artifact is unchanged when the protocol lands: it *is* the
/// challenge receipt.
fn prove_terminal_window(
    recorded: &RecordedChain,
    stage_index: usize,
    fraud_proof_config: FraudProofConfig,
) -> Result<risc0_zkvm::Receipt> {
    let stage = &recorded.spec.stages[stage_index];
    let stage_dir = recorded.chain_dir.join(&stage.name);
    let project = Project::new(recorded.base_dir.join(&stage.project))
        .map_err(|e| Error::Other(format!("stage '{}': {e}", stage.name)))?;
    let cfs = CfsBuilder::new(&project)
        .build()
        .map_err(|e| Error::Other(format!("stage '{}': failed to build CFS: {e}", stage.name)))?;

    let evidence_dir = stage_dir.join("evidence");
    std::fs::create_dir_all(&evidence_dir)
        .map_err(|e| Error::Other(format!("failed to create {}: {e}", evidence_dir.display())))?;

    let input_manifest_path = stage_dir.join("input_manifest.json");
    let (replay, _execution_time) = build_and_run_stage(
        &project,
        &cfs,
        &stage_dir.join("input.json"),
        &input_manifest_path,
        &evidence_dir,
        false,
        TraceFormat::Binary,
    )?;
    let (trace, recorder) = replay.expect("an authenticated stage run produces a trace");

    let trace_commitment =
        TraceCommitment::try_build(&trace, &EMPTY_TRIE_NODES[0], fraud_proof_config)
            .map_err(|e| Error::Other(e.to_string()))?;
    let mut verifier = TraceVerifier::new(trace_commitment.clone(), &EMPTY_TRIE_NODES[0], &cfs)
        .map_err(|e| Error::Other(e.to_string()))?;
    let window = verifier
        .terminal_window(&trace)
        .map_err(|e| Error::Other(e.to_string()))?;

    let backend = raster_backend_risc0::Risc0Backend::new(project.output_dir.clone())
        .with_user_crate(project.root_dir.clone());
    let replayer = Replayer::new(&backend, &project);
    Ok(crate::commands::run::prove(
        window,
        &trace,
        &cfs,
        &recorder,
        &replayer,
        Some(&input_manifest_path.to_string_lossy().to_string()),
        &trace_commitment,
    ))
}


/// One exhibitable `Shape` fault: which repeat block, and the stage blame lands
/// on.
#[derive(Debug)]
struct ShapeFault {
    repeat_index: usize,
    /// In range for `chain.stages` — `detect_shape_fraud` will not return a
    /// fault naming a stage that is not there, so callers may index with it.
    source_stage: u32,
}

/// A repeat block whose recorded count is not the one its producing stage
/// committed.
///
/// Reads only the `ChainCommitment` — like `detect_link_fraud`, and unlike
/// `detect_output_fraud`, which has to re-run stages. Stage-sourced counts
/// only: a literal or external count is pinned by `spec_digest` and checked
/// against the manifest in `verify_shape`, where it needs no receipt at all.
///
/// In practice `load_recorded_chain` has already refused such a chain — this
/// exists so `fraud-prove` can produce the *receipt* for a fault a relying
/// party can then check without the manifest.
///
/// `Err` is a **malformed** record, not a fault: a `source_stage` past the end
/// of the chain, or a count its own recorded width cannot represent. Both make
/// the chain invalid on its face, and neither is exhibitable — a `Shape` fault
/// is an inequality between the recorded count and what the producing stage
/// committed, and in these two cases one side of it does not exist. Saying so
/// is the whole point of separating them: this path loads the chain *as
/// claimed*, so unlike `audit` it has no `verify_shape` upstream to have
/// refused them, and treating them as faults would index past `chain.stages`
/// here and hit the guest's own `expect`s after paying for a proof.
fn detect_shape_fraud(recorded: &RecordedChain) -> Result<Option<ShapeFault>> {
    for (repeat_index, repeat) in recorded.chain.shape.repeats.iter().enumerate() {
        let Some(source_stage) = repeat.source_stage else {
            continue;
        };
        let malformed = |detail: String| {
            Error::Other(format!(
                "malformed chain commitment — repeat block '{}': {detail}. The chain is invalid, \
                 but this is not a fault a receipt can exhibit",
                repeat.name
            ))
        };

        let producer = recorded
            .chain
            .stages
            .get(source_stage as usize)
            .ok_or_else(|| {
                malformed(format!(
                    "it names producing stage {source_stage}, past the end of a {}-stage chain",
                    recorded.chain.stages.len()
                ))
            })?;
        let claimed = scalar_leaf_root(repeat.width, u64::from(repeat.resolved_count))
            .ok_or_else(|| {
                malformed(format!(
                    "it records {} iteration(s), which its recorded width {:?} cannot represent",
                    repeat.resolved_count, repeat.width
                ))
            })?;

        if claimed.as_slice() != producer.output_structural_commitment.as_slice() {
            return Ok(Some(ShapeFault {
                repeat_index,
                source_stage,
            }));
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
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|e| {
            Error::Other(format!("failed to read {}: {e}", manifest_path.display()))
        })?;
        if Sha256::digest(&manifest_bytes).to_vec() != checkpoint.input_manifest_commitment {
            // Without the checkpoint's manifest preimage no in-proof Link
            // fault can be exhibited from this artifact; the mismatch itself
            // is caught by `chain audit` as artifact inconsistency.
            continue;
        }

        let manifest = read_input_manifest(&recorded.chain_dir.join(&checkpoint.name))?;
        for (param, producer_index) in chained {
            let producer_output =
                &recorded.chain.stages[producer_index].output_structural_commitment;
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
    println!(
        "  chain digest: {}",
        hex::encode(journal.chain_commitment_digest)
    );
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
    // Loaded as the claimer asserts it, not as a verifier would accept it: the
    // first thing checked below is whether that assertion is even self-consistent.
    let recorded = load_recorded_chain(manifest, chain_commitment, ShapePolicy::AsClaimed)?;

    // Shape first. It is the cheapest of the three — pure re-derivation from
    // the commitment, no re-execution and no receipt to verify — and it is the
    // only one that can be answered *before* the stage list is agreed: an
    // output comparison is meaningless against a chain with the wrong number of
    // stages. See `docs/proposals/chain-repeat.md` §6.1.
    if let Some(ShapeFault {
        repeat_index,
        source_stage,
    }) = detect_shape_fraud(&recorded)?
    {
        let repeat = &recorded.chain.shape.repeats[repeat_index];
        println!(
            "shape fraud: repeat block '{}' records {} iteration(s), which stage {source_stage} \
             ('{}') did not commit",
            repeat.name,
            repeat.resolved_count,
            recorded.chain.stages[source_stage as usize].name,
        );
        let input = ChainFraudInput {
            chain_commitment_bytes: recorded.chain_bytes.clone(),
            faulty_stage: source_stage,
            evidence: ChainFraudEvidence::Shape {
                repeat_index: repeat_index as u32,
            },
        };
        let receipt = prove_chain_fraud(&input, None);
        return write_chain_fraud_receipt(&recorded, &receipt);
    }

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
        let receipt = prove_chain_fraud(&input, Some(authorization_receipt));
        return write_chain_fraud_receipt(&recorded, &receipt);
    }

    println!("no link fraud — re-running stages against their committed outputs");
    let Some(fraud) = detect_output_fraud(&recorded)? else {
        println!("no fraud found — every stage reproduces its committed output");
        return Ok(());
    };

    let stage_name = &recorded.spec.stages[fraud.stage_index].name;
    println!(
        "output fraud: stage {} '{stage_name}' committed {} but honest execution yields {}",
        fraud.stage_index,
        short_hex(&fraud.committed),
        short_hex(&fraud.recomputed),
    );

    // No chain-fraud aggregation for this fault. The chain-fraud guest can only
    // attribute what the `ChainCommitment` names, and it no longer names a
    // trace commitment — so execution fraud is established by the
    // challenge/response protocol, not by a single self-contained receipt
    // (`docs/proposals/chain-io-commitment.md` §3, §7). What is produced here
    // is the object that protocol opens a challenge with.
    println!("proving stage '{stage_name}' terminal window (transition guest)…");
    let fraud_proof_config = FraudProofConfig::from_window_size(EVIDENCE_WINDOW_SIZE)
        .map_err(|e| Error::Other(e.to_string()))?;
    let receipt = prove_terminal_window(&recorded, fraud.stage_index, fraud_proof_config)?;
    let journal: TransitionJournal = receipt
        .journal
        .decode()
        .map_err(|e| Error::Other(format!("failed to decode terminal-window journal: {e}")))?;

    let evidence_path = recorded.chain_dir.join("execution-fraud.receipt");
    std::fs::write(&evidence_path, postcard::to_allocvec(&receipt).unwrap())
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", evidence_path.display())))?;

    println!();
    println!("execution-fraud evidence → {}", evidence_path.display());
    println!("  stage            {} '{stage_name}'", fraud.stage_index);
    println!("  program          {}", short_hex(&journal.program_commitment));
    println!("  input manifest   {}", short_hex(&journal.input_manifest_commitment));
    println!("  honest output    {}", short_hex(&fraud.recomputed));
    println!("  committed output {}", short_hex(&fraud.committed));
    println!("  window terminal  {}", journal.window_is_terminal);
    println!();
    println!(
        "Evidence, not a self-contained fraud proof: it shows honest execution of this \n\
         checkpoint's program and inputs ends in a different output, but not that the trace \n\
         it was proven over is the honest one. A settlement layer supplies that by timeout."
    );
    Ok(())
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
    // The receipt on the table condemns this chain, so refusing to load a chain
    // whose shape is wrong would make exactly the receipts worth checking
    // uncheckable. What is verified here is the receipt, against the recorded
    // digest — not the chain's honesty, which the receipt is the answer to.
    let recorded = load_recorded_chain(manifest, chain_commitment, ShapePolicy::AsClaimed)?;
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
    println!(
        "  chain digest: {}",
        hex::encode(journal.chain_commitment_digest)
    );
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
/// `stage_dir/` under `trace_format`'s file name and the output artifact to
/// `stage_dir` (via `RASTER_OUTPUT_DIR`). Returns the loaded trace and how long
/// the stage binary itself ran — wall clock of the child process, excluding the
/// build that precedes it and the trace load that follows.
///
/// `trace_format` is transport only: this function writes the trace and reads
/// it back within the one call, and nothing downstream reads the file again —
/// `chain audit` and `chain fraud-prove` work from the stage's `commit.bin`.
/// So the choice is per-invocation and changes no commitment.
///
/// With `no_auth` the stage runs with authenticated storage off and the trace
/// is `None`: the runtime installs no publisher in that mode, so there would be
/// no file to load. The output artifact is written either way.
fn build_and_run_stage(
    project: &Project,
    cfs: &ControlFlowSchema,
    input_json_path: &Path,
    input_manifest_path: &Path,
    stage_dir: &Path,
    no_auth: bool,
    trace_format: TraceFormat,
) -> Result<(Option<(Trace, TraceRecorder)>, Duration)> {
    // The stage build is plumbing, not chain output: its cargo progress,
    // dependency warnings, and protocol-guest build chatter would bury the
    // per-stage lines this command exists to print. Capture it and surface it
    // only when the build fails (or when RASTER_VERBOSE asks for everything).
    let (build_stdio, capture_build) = if crate::utils::verbose_output() {
        (Stdio::inherit as fn() -> Stdio, false)
    } else {
        (Stdio::piped as fn() -> Stdio, true)
    };
    let mut build_command = Command::new("cargo");
    build_command
        .current_dir(&project.root_dir)
        .args(["build", "--release"])
        .stdout(build_stdio())
        .stderr(build_stdio());
    crate::utils::quiet_guest_build(&mut build_command);
    let build = build_command
        .output()
        .map_err(|e| Error::Other(format!("failed to run cargo build: {e}")))?;
    if !build.status.success() {
        if capture_build {
            // The failure is the one time this output is what the user needs.
            std::io::stderr().write_all(&build.stdout).ok();
            std::io::stderr().write_all(&build.stderr).ok();
        }
        return Err(Error::Other(format!(
            "stage build failed for {} — {}",
            project.name,
            crate::utils::VERBOSE_HINT
        )));
    }

    let binary_path = project.target_dir.join("release").join(&project.name);
    if !binary_path.exists() {
        return Err(Error::Other(format!(
            "binary not found at {}",
            binary_path.display()
        )));
    }

    let trace_path = stage_dir.join(trace_format.trace_file_name());
    let input_json = input_json_path.to_string_lossy().to_string();
    let input_manifest = input_manifest_path.to_string_lossy().to_string();

    let mut command = Command::new(&binary_path);
    command
        .current_dir(&project.root_dir)
        .args(["--input", &input_json])
        .args(["--input-manifest", &input_manifest])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The mode belongs to the run, and this is where the run is launched.
    // Which shape gets built states it: a stage that records a trace is an
    // authenticated stage, and there is no way to spell one without the other.
    // No `.profiling()` — chain stages are timed by the runner, not profiled.
    // See `crate::runtime_env`.
    let runtime_env = RuntimeEnv::new(stage_dir);
    if no_auth {
        runtime_env.apply(&mut command);
    } else {
        runtime_env
            .authenticated(&trace_path, trace_format)
            .apply(&mut command);
    }

    let started = Instant::now();
    let status = command
        .status()
        .map_err(|e| Error::Other(format!("failed to run stage binary: {e}")))?;
    let execution_time = started.elapsed();
    if !status.success() {
        // A stage that errors/panics publishes no ProgramEnd and no artifact —
        // the chain halts here; nothing downstream can be attested.
        return Err(Error::Other(format!(
            "stage '{}' exited unsuccessfully ({status}) — no authorized output; chain halts",
            project.name
        )));
    }

    if no_auth {
        return Ok((None, execution_time));
    }

    let (trace, recorder) = load_trace_from_file(
        &trace_path,
        trace_format,
        cfs,
        Some(&input_json),
        Some(&input_manifest),
    )?;
    Ok((Some((trace, recorder)), execution_time))
}

// ---------------------------------------------------------------------------
// Per-stage execution
// ---------------------------------------------------------------------------

/// Recover, from the run directory, the output commitments the selected stages
/// are fed from.
///
/// This is the whole mechanism a mid-chain run needs: a `from` binding resolves
/// to the producer's structural root, and that root is a pure function of the
/// producer's `output.bin`, which is already on disk. `collect_output` recomputes
/// it and cross-checks it against the `output_manifest.json` written beside it,
/// so a producer whose artifacts disagree is rejected here rather than silently
/// feeding a stage.
///
/// A producer with no artifact is left `None` on purpose: `synthesize_inputs`
/// owns that error, and states which stage to run.
fn rehydrate_producers(
    spec: &ChainSpec,
    selected: &[usize],
    stage_index: &BTreeMap<String, usize>,
    chain_dir: &Path,
    outputs: &mut [Option<Vec<u8>>],
) -> Result<()> {
    for &idx in selected {
        for binding in spec.stages[idx].inputs.values() {
            let InputBinding::From(producer) = binding else {
                continue;
            };
            let Some(&producer_idx) = stage_index.get(producer) else {
                continue; // `validate_spec` already rejected this
            };
            if outputs[producer_idx].is_some() {
                continue;
            }
            let producer_dir = chain_dir.join(producer);
            if producer_dir.join("output.bin").is_file() {
                outputs[producer_idx] = Some(collect_output(&producer_dir)?.structural_commitment);
            }
        }
    }
    Ok(())
}

/// Delete the stage directories after `from_idx`, which the stage about to
/// re-run has just invalidated.
///
/// "After" is **spec order**, not the dependency closure. In a linear v1 chain
/// the two coincide; where they diverge (a downstream stage bound only to
/// externals) spec order over-deletes rather than under-deletes. Over-deleting
/// costs recompute; under-deleting leaves a stale artifact that looks fresh and
/// silently feeds the next comparison.
///
/// The whole directory goes, not just the outputs: a downstream
/// `input_manifest.json` was synthesized against the *old* upstream commitment
/// and is stale in exactly the same way.
fn invalidate_downstream(chain_dir: &Path, spec: &ChainSpec, from_idx: usize) -> Result<()> {
    let victims: Vec<&str> = spec.stages[from_idx + 1..]
        .iter()
        .map(|s| s.name.as_str())
        .filter(|name| chain_dir.join(name).is_dir())
        .collect();
    if victims.is_empty() {
        return Ok(());
    }

    // Announced, because it can throw away a lot of work.
    println!(
        "  invalidating {} downstream stage{}: {}",
        victims.len(),
        if victims.len() == 1 { "" } else { "s" },
        summarize_names(&victims)
    );
    for name in &victims {
        let dir = chain_dir.join(name);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| Error::Other(format!("failed to remove {}: {e}", dir.display())))?;
    }
    Ok(())
}

/// The error a binding gets when it names a chain input nothing declares.
///
/// Shared by `validate_spec` (which fires first, before any stage runs) and
/// `synthesize_inputs` (which is reachable directly from a `--stage` run), so
/// the two cannot drift apart.
fn unknown_chain_input(
    stage: &str,
    param: &str,
    name: &str,
    declared: &BTreeMap<String, ExternalRef>,
) -> Error {
    let names: Vec<&str> = declared.keys().map(String::as_str).collect();
    let known = if names.is_empty() {
        "the manifest declares no [chain.input] table".to_string()
    } else {
        format!("declared inputs are: {}", summarize_names(&names))
    };
    Error::Other(format!(
        "stage '{stage}': parameter '{param}' names chain input '{name}', which is not declared \
         — {known}"
    ))
}

/// `a, b, c` in full when short, `a, b … y, z` when long — a list meant to be
/// recognized, not read.
fn summarize_names(names: &[&str]) -> String {
    if names.len() <= 6 {
        return names.join(", ");
    }
    format!(
        "{} … {}",
        names[..2].join(", "),
        names[names.len() - 2..].join(", ")
    )
}

// ---------------------------------------------------------------------------
// Run directory resolution — `--run`, and `latest`
// ---------------------------------------------------------------------------

/// The pointer to the newest run under a chains root.
const LATEST_LINK: &str = "latest";

/// Where this invocation's stage directories live.
///
/// - `--run <path>` names one explicitly (it must already exist);
/// - a per-stage run without `--run` follows `latest`;
/// - anything else mints a fresh timestamped directory.
fn resolve_run_dir(root: &Path, explicit: Option<&str>, per_stage: bool) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let dir = PathBuf::from(path);
        if !dir.is_dir() {
            return Err(Error::Other(format!(
                "no such chain run directory: {}",
                dir.display()
            )));
        }
        return Ok(dir);
    }
    if per_stage {
        // A single stage runs *into* an existing run; there is nothing to
        // rehydrate from in a directory this invocation just created.
        return read_latest(root).filter(|dir| dir.is_dir()).ok_or_else(|| {
            Error::Other(format!(
                "no previous chain run under {} — run the whole chain once before running a single stage",
                root.display()
            ))
        });
    }
    Ok(root.join(chain_run_id()))
}

/// Point `latest` at `target`. Best-effort: a missing pointer costs a `--run`
/// argument, so a failure here is not worth aborting a run over.
fn write_latest(root: &Path, target: &Path) {
    let link = root.join(LATEST_LINK);
    let Some(name) = target.file_name() else {
        return;
    };
    // Whether it is a symlink or the text fallback, it has to go before it can
    // be replaced.
    let _ = std::fs::remove_file(&link);
    if symlink_dir(Path::new(name), &link).is_ok() {
        return;
    }
    // Symlinks need a privilege we may not have (Windows without developer
    // mode). A file holding the directory name reads the same way.
    let _ = std::fs::write(&link, name.to_string_lossy().as_bytes());
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn symlink_dir(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks unsupported on this platform",
    ))
}

/// Resolve `latest`, accepting either form `write_latest` may have produced.
fn read_latest(root: &Path) -> Option<PathBuf> {
    let link = root.join(LATEST_LINK);
    let metadata = std::fs::symlink_metadata(&link).ok()?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(&link).ok()?;
        return Some(if target.is_absolute() {
            target
        } else {
            root.join(target)
        });
    }
    if metadata.is_file() {
        let name = std::fs::read_to_string(&link).ok()?;
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        return Some(root.join(name));
    }
    None
}

// ---------------------------------------------------------------------------
// Input synthesis
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SynthesizedInputs {
    input_json_path: PathBuf,
    input_manifest_path: PathBuf,
    /// `sha256` over the exact `input_manifest.json` bytes written.
    input_manifest_commitment: Vec<u8>,
    bindings: BTreeMap<String, InputBindingSource>,
}

/// Resolve one external reference to `(path, index_path, commitment, source)`.
///
/// Shared by the inline `external = { .. }` binding and the named
/// `input = "<name>"` one, so the two produce byte-identical manifest entries.
fn resolve_external(
    base_dir: &Path,
    ext: &ExternalRef,
) -> (PathBuf, PathBuf, String, InputBindingSource) {
    let path = absolute(base_dir, &ext.path);
    let index_path = match &ext.index_path {
        Some(p) => absolute(base_dir, p),
        None => path.with_extension("rindex"),
    };
    (
        path,
        index_path,
        ext.commitment.clone(),
        InputBindingSource::External,
    )
}

/// Write a stage's `input.json` (private file paths) and `input_manifest.json`
/// (commitments) into `stage_dir` from its bindings, returning their paths, the
/// manifest digest, and the per-parameter provenance.
///
/// `outputs` holds each stage's output structural commitment by index, which is
/// what a `from` binding resolves to; `None` means that stage has produced no
/// artifact — either it has not run, or its `main` returns unit. It is
/// recomputed host-side from `output.bin` in either authentication mode, so the
/// links a stage is fed are identical whether or not the producing stage was
/// traced, and identical whether the producer ran in this invocation or an
/// earlier one.
fn synthesize_inputs(
    stage: &StageSpec,
    stage_dir: &Path,
    base_dir: &Path,
    chain_dir: &Path,
    outputs: &[Option<Vec<u8>>],
    stage_index: &BTreeMap<String, usize>,
    chain_inputs: &BTreeMap<String, ExternalRef>,
) -> Result<SynthesizedInputs> {
    let mut input_entries: Vec<(String, serde_json::Value)> = Vec::new();
    let mut manifest_entries: Vec<(String, serde_json::Value)> = Vec::new();
    let mut bindings: BTreeMap<String, InputBindingSource> = BTreeMap::new();

    for (param, binding) in &stage.inputs {
        let (path, index_path, commitment, source) = match binding {
            InputBinding::External(ext) => resolve_external(base_dir, ext),
            // Deliberately the *same* resolution as an inline external, not a
            // parallel one: a named input is a spelling, so the two must be
            // indistinguishable in `input.json`, in `input_manifest.json`, and
            // therefore in `input_manifest_commitment`.
            InputBinding::Input(name) => {
                let ext = chain_inputs
                    .get(name)
                    .ok_or_else(|| unknown_chain_input(&stage.name, param, name, chain_inputs))?;
                resolve_external(base_dir, ext)
            }
            InputBinding::From(producer) => {
                let producer_idx = *stage_index.get(producer).ok_or_else(|| {
                    Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from unknown stage '{producer}'",
                        stage.name
                    ))
                })?;
                let producer_dir = chain_dir.join(producer);
                // Absent either because the producer has not run in this run
                // directory, or because its `main` returns unit. The first is
                // the case a per-stage run exists to hit, so it names the fix.
                let structural = outputs[producer_idx].as_ref().ok_or_else(|| {
                    Error::Other(format!(
                        "stage '{}': parameter '{param}' is fed from '{producer}', which has no output.bin\n  \
                         expected: {}\n  \
                         run: cargo raster chain run --no-auth --run {} --stage {producer}",
                        stage.name,
                        producer_dir.join("output.bin").display(),
                        chain_dir.display(),
                    ))
                })?;
                let commitment = hex::encode(structural);
                (
                    producer_dir.join("output.bin"),
                    producer_dir.join("output.rindex"),
                    commitment,
                    InputBindingSource::Chained {
                        stage: producer_idx,
                    },
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

    std::fs::write(
        &input_json_path,
        serde_json::to_vec_pretty(&input_json).unwrap(),
    )
    .map_err(|e| {
        Error::Other(format!(
            "failed to write {}: {e}",
            input_json_path.display()
        ))
    })?;
    std::fs::write(&input_manifest_path, &input_manifest_bytes).map_err(|e| {
        Error::Other(format!(
            "failed to write {}: {e}",
            input_manifest_path.display()
        ))
    })?;

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
        Error::Other(format!(
            "{} is not a well-formed raster payload",
            output_bin.display()
        ))
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
    // Cache cold — build the CFS, then reassemble from source + Raster.lock.
    let cfs = CfsBuilder::new(project)
        .build()
        .map_err(|e| Error::Other(format!("stage '{stage_name}': failed to build CFS: {e}")))?;
    program_identity_with_cfs(project, &cfs, stage_name)
}

/// Program identity for a caller that already holds the stage's CFS.
///
/// The chain runner builds one per stage anyway, and a cold `program.bin`
/// otherwise makes [`read_program_identity`] build a *second* one — a full syn
/// AST discovery over the crate (`CfsBuilder::build`), per stage. On a
/// 74-stage chain that is the difference between one traversal per stage and
/// several. Pass the CFS you have.
fn program_identity_with_cfs(
    project: &Project,
    cfs: &ControlFlowSchema,
    stage_name: &str,
) -> Result<Vec<u8>> {
    let cached = project.output_dir.join("program.bin");
    if let Ok(bytes) = std::fs::read(&cached) {
        return Ok(commitment_of_bytes(&bytes).to_vec());
    }
    let program = crate::program::reassemble_from_lock(project, cfs)
        .map_err(|e| Error::Other(format!("stage '{stage_name}': {e}")))?;
    Ok(program.commitment().to_vec())
}

/// Can this stage's identity be resolved at all, without paying to resolve it?
///
/// **Takes a path, not a `Project`, and that is the whole point.**
/// `Project::new` spawns `cargo metadata` and parses the project's entire AST
/// (`raster-compiler/src/project.rs:21-36`). The run loop already builds one
/// per stage; constructing a second one here just to look at two filenames
/// doubled that — 74 extra subprocess spawns and AST parses on a chain like
/// `raster-chain-inference`, which has 74 stages over 6 distinct projects.
///
/// `output_dir` is `root/target/raster` unconditionally (`project.rs:29`), so
/// both paths are derivable from the stage root. Two `stat`s, no process, no
/// parse.
///
/// The pre-run check exists to catch "stage 40 of 74 has no identity" before
/// stage 1 runs, and that failure is always a missing artifact — neither a
/// cached frame nor a lock to rebuild one from.
fn program_identity_resolvable(stage_root: &Path) -> bool {
    stage_root.join("target").join("raster").join("program.bin").is_file()
        || stage_root.join("Raster.lock").is_file()
}

fn load_chain_manifest(chain_file: &Path) -> Result<ChainManifest> {
    let text = std::fs::read_to_string(chain_file)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", chain_file.display())))?;
    let doc: ChainJsonDoc = serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", chain_file.display())))?;
    doc.into_manifest(chain_file)
}

/// The JSON form. Ordering is free here — one array, already in order — so
/// `items` is the general spelling and `stages` stays as the shorthand for a
/// chain with no repeat blocks, which is every `chain.json` written to date.
#[derive(Debug, Deserialize)]
struct ChainJsonDoc {
    #[serde(default)]
    inputs: BTreeMap<String, ExternalRef>,
    #[serde(default)]
    stages: Vec<StageSpec>,
    #[serde(default)]
    items: Vec<ChainItem>,
}

impl ChainJsonDoc {
    fn into_manifest(self, chain_file: &Path) -> Result<ChainManifest> {
        if !self.items.is_empty() && !self.stages.is_empty() {
            return Err(Error::Other(format!(
                "{}: declares both 'stages' and 'items' — 'items' is the general form and \
                 'stages' the shorthand for a chain with no repeat blocks; use one",
                chain_file.display()
            )));
        }
        let items = if self.items.is_empty() {
            self.stages.into_iter().map(ChainItem::Stage).collect()
        } else {
            self.items
        };
        Ok(ChainManifest {
            inputs: self.inputs,
            items,
        })
    }
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

/// The `[chain]` table: pipeline metadata, the named-external table, and the
/// ordered `[[chain.stage]]` list. Stages and bindings reuse the same
/// `StageSpec`/`InputBinding` shapes the JSON form uses, so both formats
/// deserialize into one `ChainSpec`.
#[derive(Debug, Deserialize)]
struct ChainTable {
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    version: Option<String>,
    #[serde(default, rename = "input")]
    inputs: BTreeMap<String, InputDecl>,
    /// Spanned so the relative order of `[[chain.stage]]` and
    /// `[[chain.repeat]]` survives. TOML gives serde two independent arrays
    /// with no ordering between them, and that ordering is what
    /// `InputBindingSource::Chained { stage }` indexes into.
    #[serde(default, rename = "stage")]
    stages: Vec<toml::Spanned<StageSpec>>,
    #[serde(default, rename = "repeat")]
    repeats: Vec<toml::Spanned<RepeatSpec>>,
}

/// Recover the authored order of a `[chain]` table's stages and repeat blocks
/// from their source spans.
///
/// The spans come from the parser, not from the document — two array-of-tables
/// entries cannot share a start offset, so the sort is total and the result is
/// the order a reader of the file sees.
fn merge_chain_items(
    stages: Vec<toml::Spanned<StageSpec>>,
    repeats: Vec<toml::Spanned<RepeatSpec>>,
) -> Vec<ChainItem> {
    let mut spanned: Vec<(usize, ChainItem)> = stages
        .into_iter()
        .map(|s| (s.span().start, ChainItem::Stage(s.into_inner())))
        .chain(
            repeats
                .into_iter()
                .map(|r| (r.span().start, ChainItem::Repeat(r.into_inner()))),
        )
        .collect();
    spanned.sort_by_key(|(offset, _)| *offset);
    spanned.into_iter().map(|(_, item)| item).collect()
}

/// One `[chain.input.<name>]` entry: a single external, or an indexed family of
/// them.
///
/// The indexed form is **sugar**. It flattens at load into one plain entry per
/// index (`<name>_0` … `<name>_N`), so a binding refers to a member by ordinary
/// textual substitution — `{ input = "layer_{l}" }` — and nothing past
/// `load_spec` knows the family existed. What it compresses is the path and the
/// surrounding block, never the commitments: those are distinct hashes, one per
/// index, and they stay written out positionally because a per-index commitment
/// is exactly the thing a verifier has to read.
///
/// Untagged because the two forms are distinguished by `commitment` vs.
/// `commitments`, which reads better in TOML than an explicit discriminant
/// would. See `docs/proposals/chain-repeat.md` §1.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InputDecl {
    Indexed(IndexedInputDecl),
    Single(ExternalRef),
}

/// `[chain.input.<name>]` with `commitments = [..]` — one external per index.
#[derive(Debug, Deserialize)]
struct IndexedInputDecl {
    /// The index name substituted into `path` / `index_path`. Declared rather
    /// than inferred so the templates read the same as a repeat block's.
    index: String,
    path: String,
    #[serde(default)]
    index_path: Option<String>,
    /// One commitment per index, in order. Its length *is* the family's count.
    commitments: Vec<String>,
}

impl IndexedInputDecl {
    /// Expand into `(name_i, ExternalRef)` pairs, substituting `{index}` in the
    /// paths. Indexes are 0-based, matching the repeat block's convention and
    /// the `layer0`-based naming every manifest already uses.
    fn flatten(self, family: &str) -> Result<Vec<(String, ExternalRef)>> {
        let placeholder = format!("{{{}}}", self.index);
        if !self.path.contains(&placeholder) {
            return Err(Error::Other(format!(
                "[chain.input.{family}]: path '{}' does not mention '{placeholder}', so every \
                 index would resolve to the same file",
                self.path
            )));
        }

        Ok(self
            .commitments
            .into_iter()
            .enumerate()
            .map(|(i, commitment)| {
                let at = |s: &str| s.replace(&placeholder, &i.to_string());
                (
                    format!("{family}_{i}"),
                    ExternalRef {
                        path: at(&self.path),
                        index_path: self.index_path.as_deref().map(at),
                        commitment,
                    },
                )
            })
            .collect())
    }
}

/// Flatten a `[chain.input]` table into the plain `name -> ExternalRef` map the
/// rest of the CLI works in.
///
/// A name produced twice is refused rather than resolved. The two spellings
/// land in one namespace — `[chain.input.layer_0]` and member 0 of the family
/// `layer` are the same name — and which one won would otherwise depend on
/// `BTreeMap` ordering, i.e. on the spelling of the *other* key. Refusing costs
/// nothing and the alternative is a commitment silently replaced.
fn flatten_input_decls(decls: BTreeMap<String, InputDecl>) -> Result<BTreeMap<String, ExternalRef>> {
    let mut inputs: BTreeMap<String, ExternalRef> = BTreeMap::new();
    // Which declaration produced each name, for the error message.
    let mut origin: BTreeMap<String, String> = BTreeMap::new();

    for (name, decl) in decls {
        let flattened = match decl {
            InputDecl::Single(external) => vec![(name.clone(), external)],
            InputDecl::Indexed(indexed) => indexed.flatten(&name)?,
        };
        for (member, external) in flattened {
            if let Some(previous) = origin.get(&member) {
                return Err(Error::Other(format!(
                    "chain input '{member}' is declared twice — by '{previous}' and by '{name}'"
                )));
            }
            origin.insert(member.clone(), name.clone());
            inputs.insert(member, external);
        }
    }
    Ok(inputs)
}

/// Resolve a chain spec and its (absolute) base directory from an optional path:
/// - `Some(dir)`  — a directory holding `Raster.toml` (`[chain]`) or `chain.json`.
/// - `Some(file)` — a `*.toml` (parsed as `[chain]`) or any other file (`chain.json`).
/// - `None`       — discover, walking up from the current directory.
///
/// The base dir is canonicalized, so every synthesized stage `input.json` gets
/// absolute paths regardless of the caller's working directory (a relative
/// manifest path used to leak relative input paths into the stage run dirs).
fn resolve_chain(path: Option<&str>) -> Result<(ChainManifest, PathBuf)> {
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

    Ok((load_spec(&manifest)?, base_dir))
}

/// Load an **unexpanded** `ChainManifest` from either a `Raster.toml`
/// (`[chain]`) or a `chain.json`, chosen by file extension.
fn load_spec(manifest: &Path) -> Result<ChainManifest> {
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
        Ok(ChainManifest {
            inputs: flatten_input_decls(table.inputs)?,
            items: merge_chain_items(table.stages, table.repeats),
        })
    } else {
        load_chain_manifest(manifest)
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
            return Err(Error::Other(format!(
                "duplicate stage name '{}'",
                stage.name
            )));
        }
    }
    Ok(())
}

/// Whether the manifest is well-formed — names unique, and every `from` binding
/// naming a stage that actually precedes it.
///
/// The ordering rule is a property of `chain.json`, not of any particular run,
/// so it is checked before anything executes. A sequential run could never
/// reach a violation (a later producer simply has not run yet, and fails on its
/// missing artifact instead), but `--stage` can name one directly, and would
/// otherwise get the confusing "no output.bin" error for a binding that is
/// unsatisfiable by construction.
///
/// `audit` keeps its own copy of this check: it verifies a commitment that may
/// have been produced elsewhere, and must not assume this ran.
fn validate_spec(spec: &ChainSpec) -> Result<()> {
    validate_stage_names(spec)?;

    let stage_index: BTreeMap<&str, usize> = spec
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    for (idx, stage) in spec.stages.iter().enumerate() {
        for (param, binding) in &stage.inputs {
            // An undeclared chain input is a manifest typo, and catching it
            // here rather than in `synthesize_inputs` means it is reported
            // before any stage runs — the same reason the `from` checks below
            // live here.
            if let InputBinding::Input(name) = binding {
                if !spec.inputs.contains_key(name) {
                    return Err(unknown_chain_input(
                        &stage.name,
                        param,
                        name,
                        &spec.inputs,
                    ));
                }
            }
            let InputBinding::From(producer) = binding else {
                continue;
            };
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
    raster_target_dir().join("chains")
}

/// Where `--no-auth` runs go. A separate root, not a marker inside the usual
/// one, so `latest_chain_commitment`'s "newest directory wins" can never land
/// on a run that has no chain-commitment in it — an unauthenticated run is
/// invisible to every command that reads a commitment.
fn no_auth_chains_root() -> PathBuf {
    raster_target_dir().join("chains-no-auth")
}

fn raster_target_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target")
        .join("raster")
}

fn chain_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
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

    /// A `RecordedChain` carrying nothing but the commitment — enough for the
    /// detectors that read only that, and for nothing else.
    fn recorded_from(chain: ChainCommitment) -> RecordedChain {
        RecordedChain {
            spec: ChainSpec {
                stages: Vec::new(),
                inputs: BTreeMap::new(),
            },
            base_dir: PathBuf::from("."),
            chain,
            chain_bytes: Vec::new(),
            chain_dir: PathBuf::from("."),
        }
    }

    /// A one-stage chain whose repeat block claims `count`, sourced from
    /// `source_stage` at `width`, against a planner that committed `committed`.
    fn shape_chain(
        source_stage: u32,
        width: IndexWidth,
        count: u32,
        committed: u32,
    ) -> ChainCommitment {
        ChainCommitment {
            stages: vec![StageCheckpoint {
                name: "planner".into(),
                program_commitment: vec![1],
                input_manifest_commitment: vec![2],
                input_bindings: BTreeMap::new(),
                output_payload_commitment: vec![3],
                output_structural_commitment: scalar_leaf_root(
                    IndexWidth::U32,
                    u64::from(committed),
                )
                .unwrap()
                .to_vec(),
            }],
            shape: ChainShape {
                spec_digest: Vec::new(),
                repeats: vec![RepeatResolution {
                    name: "decode".into(),
                    source_stage: Some(source_stage),
                    source_commitment: Vec::new(),
                    selector: String::new(),
                    width,
                    max: 128,
                    resolved_count: count,
                }],
            },
        }
    }

    #[test]
    fn detect_shape_fraud_finds_the_inequality_it_can_exhibit() {
        let recorded = recorded_from(shape_chain(0, IndexWidth::U32, 5, 3));
        let fault = detect_shape_fraud(&recorded).unwrap().unwrap();
        assert_eq!(fault.repeat_index, 0);
        assert_eq!(fault.source_stage, 0);

        let honest = recorded_from(shape_chain(0, IndexWidth::U32, 3, 3));
        assert!(detect_shape_fraud(&honest).unwrap().is_none());
    }

    #[test]
    fn a_source_stage_past_the_end_is_malformed_not_a_fault() {
        // Reported, not proved, and above all not indexed: treating this as a
        // fault would have `fraud_prove` index `chain.stages[7]` to name the
        // stage to blame.
        let recorded = recorded_from(shape_chain(7, IndexWidth::U32, 3, 3));
        let err = detect_shape_fraud(&recorded).unwrap_err().to_string();
        assert!(err.contains("malformed chain commitment"), "{err}");
        assert!(err.contains("past the end of a 1-stage chain"), "{err}");
    }

    #[test]
    fn a_count_its_width_cannot_represent_is_malformed_not_a_fault() {
        // `scalar_leaf_root` returns `None`, so there is no value to compare
        // against what the producer committed. The guest asserts the same thing
        // with an `expect`, which is a panic mid-proof rather than a verdict.
        let recorded = recorded_from(shape_chain(0, IndexWidth::U8, 300, 3));
        let err = detect_shape_fraud(&recorded).unwrap_err().to_string();
        assert!(err.contains("malformed chain commitment"), "{err}");
        assert!(err.contains("cannot represent"), "{err}");
    }

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
        let spec = spec_from_json(json).unwrap();
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

    fn manifest_from_toml(text: &str) -> Result<ChainManifest> {
        let doc: RasterTomlDoc = toml::from_str(text).unwrap();
        let table = doc.chain.expect("[chain] table present");
        Ok(ChainManifest {
            inputs: flatten_input_decls(table.inputs)?,
            items: merge_chain_items(table.stages, table.repeats),
        })
    }

    fn spec_from_toml(text: &str) -> Result<ChainSpec> {
        Ok(resolve(&manifest_from_toml(text)?)?.spec)
    }

    fn spec_from_json(text: &str) -> Result<ChainSpec> {
        let doc: ChainJsonDoc = serde_json::from_str(text).unwrap();
        Ok(resolve(&doc.into_manifest(Path::new("chain.json"))?)?.spec)
    }

    /// A repeat block with a literal count, spelled minimally.
    fn repeat_toml(name: &str) -> String {
        format!(
            r#"
[[chain.repeat]]
name  = "{name}"
index = "l"
count = 2

  [[chain.repeat.stage]]
  name    = "{name}_l{{l}}"
  project = "p"
"#
        )
    }

    #[test]
    fn stages_and_repeat_blocks_keep_their_authored_order() {
        // TOML hands serde two independent arrays-of-tables with no ordering
        // between them. That ordering is not cosmetic: a checkpoint records its
        // producer as `Chained { stage: <index into the expanded list> }`, so
        // two manifests differing only in where a repeat block sits are
        // different chains. The spans are what recover it.
        let text = format!(
            r#"
[chain]
[[chain.stage]]
name = "first"
project = "p"
{}
[[chain.stage]]
name = "middle"
project = "p"
{}
[[chain.stage]]
name = "last"
project = "p"
"#,
            repeat_toml("early"),
            repeat_toml("late"),
        );

        let manifest = manifest_from_toml(&text).unwrap();
        let order: Vec<&str> = manifest
            .items
            .iter()
            .map(|item| match item {
                ChainItem::Stage(s) => s.name.as_str(),
                ChainItem::Repeat(r) => r.name.as_str(),
            })
            .collect();
        assert_eq!(
            order,
            ["first", "early", "middle", "late", "last"],
            "items must appear in the order a reader of the file sees them"
        );
    }

    #[test]
    fn the_json_form_takes_items_or_stages_but_not_both() {
        let both = r#"{
            "stages": [{ "name": "a", "project": "p" }],
            "items":  [{ "stage": { "name": "b", "project": "p" } }]
        }"#;
        let doc: ChainJsonDoc = serde_json::from_str(both).unwrap();
        let err = doc
            .into_manifest(Path::new("chain.json"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("declares both 'stages' and 'items'"), "{err}");

        // `items` is the general spelling; `stages` remains the shorthand every
        // chain.json written to date uses, and both reach the same manifest.
        let spec = spec_from_json(
            r#"{ "items": [{ "stage": { "name": "a", "project": "p" } }] }"#,
        )
        .unwrap();
        assert_eq!(spec.stages.len(), 1);
        assert_eq!(spec.stages[0].name, "a");
    }

    #[test]
    fn a_repeat_block_survives_the_load_and_expands() {
        // The load path and the expansion are separate concerns and this test
        // covers the seam: the block arrives as one item, and becomes several
        // stages. Substitution itself is covered in `chain::expand`.
        let manifest = manifest_from_toml(&format!("[chain]\n{}", repeat_toml("decode"))).unwrap();
        assert_eq!(manifest.items.len(), 1, "one authored item");

        let spec = resolve(&manifest).unwrap().spec;
        let names: Vec<&str> = spec.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["decode_l0", "decode_l1"]);
    }

    #[test]
    fn a_named_chain_input_resolves_exactly_like_an_inline_external() {
        // The point of the feature: `{ input = "m" }` and the inline form must
        // be indistinguishable downstream, or the two spellings would produce
        // different `input_manifest.json` bytes and therefore different
        // `input_manifest_commitment`s for the same chain.
        let named = spec_from_toml(
            r#"
[chain]
[chain.input.m]
path       = "data/m.rastered"
index_path = "data/m.rindex"
commitment = "aa"

[[chain.stage]]
name = "s"
project = "p"
inputs.readings = { input = "m" }
"#,
        )
        .unwrap();

        let inline = spec_from_toml(
            r#"
[chain]
[[chain.stage]]
name = "s"
project = "p"
inputs.readings = { external = { path = "data/m.rastered", index_path = "data/m.rindex", commitment = "aa" } }
"#,
        )
        .unwrap();

        let base = Path::new("/base");
        let InputBinding::Input(name) = &named.stages[0].inputs["readings"] else {
            panic!("expected a named binding");
        };
        let InputBinding::External(ext) = &inline.stages[0].inputs["readings"] else {
            panic!("expected an inline binding");
        };
        assert_eq!(
            resolve_external(base, &named.inputs[name]),
            resolve_external(base, ext),
        );
    }

    #[test]
    fn an_indexed_input_flattens_to_one_entry_per_commitment() {
        let spec = spec_from_toml(
            r#"
[chain]
[chain.input.layer]
index       = "l"
path        = "prefill/layer{l}.rastered"
index_path  = "prefill/layer{l}.rindex"
commitments = ["c0", "c1", "c2"]

[[chain.stage]]
name = "s"
project = "p"
inputs.layer = { input = "layer_1" }
"#,
        )
        .unwrap();

        // 0-based, one per commitment, and the family name itself is not a key —
        // only its members are, which is what makes `{ input = "layer_{l}" }`
        // ordinary textual substitution against an ordinary name.
        assert_eq!(
            spec.inputs.keys().map(String::as_str).collect::<Vec<_>>(),
            ["layer_0", "layer_1", "layer_2"],
        );
        assert_eq!(spec.inputs["layer_2"].path, "prefill/layer2.rastered");
        assert_eq!(
            spec.inputs["layer_2"].index_path.as_deref(),
            Some("prefill/layer2.rindex"),
        );
        assert_eq!(spec.inputs["layer_0"].commitment, "c0");
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn an_indexed_input_whose_path_ignores_the_index_is_rejected() {
        // Otherwise all N members silently resolve to the same file while
        // carrying N different commitments — N-1 of which can never match.
        let err = spec_from_toml(
            r#"
[chain]
[chain.input.layer]
index       = "l"
path        = "prefill/layer.rastered"
commitments = ["c0", "c1"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("does not mention '{l}'"), "{err}");
    }

    #[test]
    fn an_indexed_member_colliding_with_a_plain_declaration_is_rejected() {
        let err = spec_from_toml(
            r#"
[chain]
[chain.input.layer_0]
path       = "elsewhere.rastered"
commitment = "zz"

[chain.input.layer]
index       = "l"
path        = "prefill/layer{l}.rastered"
commitments = ["c0", "c1"]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("declared twice"), "{err}");
    }

    #[test]
    fn validate_spec_rejects_an_undeclared_chain_input() {
        let spec = spec_from_toml(
            r#"
[chain]
[chain.input.m]
path       = "data/m.rastered"
commitment = "aa"

[[chain.stage]]
name = "s"
project = "p"
inputs.readings = { input = "typo" }
"#,
        )
        .unwrap();
        let err = validate_spec(&spec).unwrap_err().to_string();
        assert!(err.contains("names chain input 'typo'"), "{err}");
        assert!(err.contains("declared inputs are: m"), "{err}");
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
        let spec = spec_from_toml(toml).unwrap();
        assert!(spec.inputs.is_empty(), "no [chain.input] table declared");
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
            inputs: BTreeMap::new(),
            stages: vec![
                StageSpec {
                    name: "s".into(),
                    project: "a".into(),
                    inputs: BTreeMap::new(),
                },
                StageSpec {
                    name: "s".into(),
                    project: "b".into(),
                    inputs: BTreeMap::new(),
                },
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
        };
        let commitment = |payload: u8, spec_digest: Vec<u8>| ChainCommitment {
            stages: vec![checkpoint("s", payload)],
            shape: ChainShape {
                spec_digest,
                repeats: Vec::new(),
            },
        };
        let a = commitment(9, vec![0xaa]);
        let b = commitment(9, vec![0xaa]);
        let c = commitment(10, vec![0xaa]);
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), c.digest(), "a stage's output must move the digest");

        // The shape is inside the digest too, so a chain expanded from a
        // different manifest is a different chain even when the stages it
        // happens to record are identical.
        assert_ne!(
            a.digest(),
            commitment(9, vec![0xbb]).digest(),
            "the manifest the chain was expanded from must move the digest"
        );
    }

    #[test]
    fn absolute_joins_relative_but_keeps_absolute() {
        let base = Path::new("/base/dir");
        assert_eq!(absolute(base, "x.bin"), PathBuf::from("/base/dir/x.bin"));
        assert_eq!(absolute(base, "/abs/x.bin"), PathBuf::from("/abs/x.bin"));
    }

    // -----------------------------------------------------------------------
    // Per-stage execution
    // -----------------------------------------------------------------------

    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "raster-chain-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stage_spec(name: &str, inputs: &[(&str, InputBinding)]) -> StageSpec {
        StageSpec {
            name: name.into(),
            project: name.into(),
            inputs: inputs
                .iter()
                .map(|(p, b)| ((*p).to_string(), b.clone()))
                .collect(),
        }
    }

    #[test]
    fn validate_spec_rejects_a_forward_from_reference() {
        // 'a' is fed from 'b', which runs after it — unsatisfiable by
        // construction, and reachable only via `--stage`.
        let spec = ChainSpec {
            inputs: BTreeMap::new(),
            stages: vec![
                stage_spec("a", &[("x", InputBinding::From("b".into()))]),
                stage_spec("b", &[]),
            ],
        };
        let err = validate_spec(&spec).unwrap_err().to_string();
        assert!(err.contains("does not run earlier"), "{err}");
    }

    #[test]
    fn validate_spec_rejects_an_unknown_producer() {
        let spec = ChainSpec {
            inputs: BTreeMap::new(),
            stages: vec![stage_spec(
                "a",
                &[("x", InputBinding::From("ghost".into()))],
            )],
        };
        let err = validate_spec(&spec).unwrap_err().to_string();
        assert!(err.contains("unknown stage 'ghost'"), "{err}");
    }

    #[test]
    fn validate_spec_accepts_a_backward_reference() {
        let spec = ChainSpec {
            inputs: BTreeMap::new(),
            stages: vec![
                stage_spec("a", &[]),
                stage_spec("b", &[("x", InputBinding::From("a".into()))]),
            ],
        };
        assert!(validate_spec(&spec).is_ok());
    }

    #[test]
    fn synthesize_inputs_names_the_stage_to_run_when_a_producer_has_no_artifact() {
        let dir = scratch("synth-missing");
        let stage = stage_spec("b", &[("kv", InputBinding::From("a".into()))]);
        let stage_index: BTreeMap<String, usize> = [("a".to_string(), 0), ("b".to_string(), 1)]
            .into_iter()
            .collect();
        // 'a' is a real stage of the chain, it just has not produced anything
        // in this run directory.
        let outputs = vec![None, None];

        let err = synthesize_inputs(&stage, &dir, &dir, &dir, &outputs, &stage_index, &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no output.bin"), "{err}");
        assert!(err.contains("--stage a"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn synthesize_inputs_feeds_a_rehydrated_producer_commitment() {
        // The point of the feature: a commitment recovered from a producer's
        // artifact reaches the consumer's manifest unchanged, exactly as if the
        // producer had run in this same invocation.
        let dir = scratch("synth-rehydrated");
        let stage = stage_spec("b", &[("kv", InputBinding::From("a".into()))]);
        let stage_index: BTreeMap<String, usize> = [("a".to_string(), 0), ("b".to_string(), 1)]
            .into_iter()
            .collect();
        let outputs = vec![Some(vec![0xab, 0xcd]), None];

        let synth = synthesize_inputs(&stage, &dir, &dir, &dir, &outputs, &stage_index, &BTreeMap::new()).unwrap();
        assert!(matches!(
            synth.bindings["kv"],
            InputBindingSource::Chained { stage: 0 }
        ));
        let manifest = read_input_manifest(&dir).unwrap();
        assert_eq!(manifest["kv"], "abcd");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A dynamic chain, and the same chain with one thing changed that leaves
    /// every stage *name* alone — so a shape recorded for one of them looks
    /// applicable to the other by every key the sidecar has.
    const SHAPE_MANIFEST: &str = r#"
[chain]
[[chain.stage]]
name = "planner"
project = "planner"

[[chain.repeat]]
name  = "steps"
index = "t"
count = { from = "planner", max = 8 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "step"
"#;

    const SHAPE_MANIFEST_EDITED: &str = r#"
[chain]
[[chain.stage]]
name = "planner"
project = "planner"

[[chain.repeat]]
name  = "steps"
index = "t"
count = { from = "planner", max = 3 }
  [[chain.repeat.stage]]
  name    = "step{t}"
  project = "step"
"#;

    fn stage_sourced_shape(count: u32) -> Vec<RepeatResolution> {
        vec![RepeatResolution {
            name: "steps".into(),
            source_stage: Some(0),
            source_commitment: Vec::new(),
            selector: String::new(),
            width: IndexWidth::U64,
            max: 8,
            resolved_count: count,
        }]
    }

    #[test]
    fn a_recorded_shape_is_read_back_only_for_the_manifest_that_wrote_it() {
        // The sidecar is keyed by block *name*, which survives every edit that
        // does not rename the block — so without the digest an edited manifest
        // inherits counts belonging to a graph it is not building. Both
        // manifests here declare a block called `steps`, and only `max` differs.
        let dir = scratch("shape-binding");
        let manifest = manifest_from_toml(SHAPE_MANIFEST).unwrap();
        write_chain_shape(&dir, &manifest, &stage_sourced_shape(5)).unwrap();

        let read = read_chain_shape(&dir, &manifest);
        assert!(!read.stale);
        assert_eq!(read.counts["steps"].count, 5);
        assert_eq!(read.counts["steps"].source_stage, 0);

        let edited = manifest_from_toml(SHAPE_MANIFEST_EDITED).unwrap();
        let read = read_chain_shape(&dir, &edited);
        assert!(
            read.counts.is_empty(),
            "a count resolved from another manifest must not be inherited"
        );
        assert!(
            read.stale,
            "and the caller has to be able to say so — 'that stage never ran' would be a lie"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_literal_count_is_never_read_back_from_the_recorded_shape() {
        // `source_stage: None` is a manifest-static count. Expansion re-derives
        // it, and inheriting it would let a recorded number outrank the
        // manifest's own.
        let dir = scratch("shape-literal");
        let manifest = manifest_from_toml(SHAPE_MANIFEST).unwrap();
        let mut repeats = stage_sourced_shape(5);
        repeats[0].source_stage = None;
        write_chain_shape(&dir, &manifest, &repeats).unwrap();

        assert!(read_chain_shape(&dir, &manifest).counts.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_recorded_shape_is_not_a_stale_one() {
        let dir = scratch("shape-absent");
        let manifest = manifest_from_toml(SHAPE_MANIFEST).unwrap();
        let read = read_chain_shape(&dir, &manifest);
        assert!(read.counts.is_empty());
        assert!(!read.stale, "nothing recorded is not the same as recorded for another chain");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalidate_downstream_removes_later_stages_only() {
        let dir = scratch("invalidate");
        let spec = ChainSpec {
            inputs: BTreeMap::new(),
            stages: vec![
                stage_spec("a", &[]),
                stage_spec("b", &[]),
                stage_spec("c", &[]),
            ],
        };
        for name in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("output.bin"), b"x").unwrap();
        }

        invalidate_downstream(&dir, &spec, 0).unwrap();
        assert!(dir.join("a").is_dir(), "the re-run stage survives");
        assert!(!dir.join("b").exists());
        assert!(!dir.join("c").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalidate_downstream_on_the_last_stage_removes_nothing() {
        let dir = scratch("invalidate-last");
        let spec = ChainSpec {
            inputs: BTreeMap::new(),
            stages: vec![stage_spec("a", &[]), stage_spec("b", &[])],
        };
        for name in ["a", "b"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }

        invalidate_downstream(&dir, &spec, 1).unwrap();
        assert!(dir.join("a").is_dir());
        assert!(dir.join("b").is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn latest_round_trips() {
        let root = scratch("latest");
        let target = root.join("00018-pid5566");
        std::fs::create_dir_all(&target).unwrap();

        write_latest(&root, &target);
        assert_eq!(
            read_latest(&root).map(|p| std::fs::canonicalize(p).unwrap()),
            Some(std::fs::canonicalize(&target).unwrap())
        );

        // Repointing replaces rather than failing on an existing link.
        let second = root.join("00019-pid5567");
        std::fs::create_dir_all(&second).unwrap();
        write_latest(&root, &second);
        assert_eq!(
            read_latest(&root).map(|p| std::fs::canonicalize(p).unwrap()),
            Some(std::fs::canonicalize(&second).unwrap())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn latest_reads_the_text_pointer_fallback() {
        // What a platform without symlink privileges leaves behind.
        let root = scratch("latest-text");
        let target = root.join("00020-pid1");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(root.join(LATEST_LINK), "00020-pid1\n").unwrap();

        assert_eq!(read_latest(&root), Some(target));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_run_dir_requires_an_existing_explicit_directory() {
        let root = scratch("resolve-explicit");
        let missing = root.join("nope");
        let err = resolve_run_dir(&root, Some(missing.to_str().unwrap()), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no such chain run directory"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_run_dir_per_stage_needs_a_previous_run() {
        let root = scratch("resolve-no-latest");
        let err = resolve_run_dir(&root, None, true).unwrap_err().to_string();
        assert!(err.contains("run the whole chain once"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resolve_run_dir_mints_only_for_a_whole_chain_run() {
        let root = scratch("resolve-mint");
        let minted = resolve_run_dir(&root, None, false).unwrap();
        assert_eq!(minted.parent(), Some(root.as_path()));
        assert!(
            !minted.exists(),
            "minting names a directory, it does not create one"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn summarize_names_elides_only_long_lists() {
        assert_eq!(summarize_names(&["a", "b", "c"]), "a, b, c");
        assert_eq!(
            summarize_names(&["a", "b", "c", "d", "e", "f", "g"]),
            "a, b … f, g"
        );
    }
}
