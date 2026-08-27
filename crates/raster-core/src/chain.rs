//! Shared chain types (guest and host).
//!
//! The checkpoint types were born in `raster-cli`'s `chain` module; they live
//! here because the chain-fraud guest must decode the exact `ChainCommitment`
//! bytes it attributes a fault to (the same reason `TransitionJournal` lives
//! here). The CLI re-imports them. See `docs/proposals/program-chain.md` and
//! `docs/proposals/chain-fraud-proof.md`.

use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authorization::AuthorizationJournal;
use crate::input::IndexWidth;

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
///
/// **Input and output only.** Every field is a pure function of public
/// artifacts, so a checkpoint costs no trace and no authenticated storage —
/// a cheap run produces byte-identical bytes to an authenticated one. The
/// trace commitment a stage's execution is disputed against is not named
/// here: it is a dispute-time artifact, produced on demand by re-running the
/// contested stage (`cargo raster chain run --stage <name>`), because
/// execution is a pure function of the committed program and inputs. See
/// `docs/proposals/chain-io-commitment.md`.
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
}

/// The chain-level object: an ordered list of stage checkpoints, plus the shape
/// they were derived from. A verifier holding this plus each stage's program
/// source and `output.bin` can check the whole chain's links, identities, and
/// **graph** with no prover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainCommitment {
    pub stages: Vec<StageCheckpoint>,
    pub shape: ChainShape,
}

/// How the recorded stage list was arrived at.
///
/// Without this a chain's length is whatever the claimer wrote down. With it, a
/// verifier re-derives the length from the manifest and the counts, and the
/// existing "declared stages vs. recorded stages" check runs against a *derived*
/// number rather than a declared one. See `docs/proposals/chain-repeat.md` §5.
///
/// Recorded unconditionally, including for a chain with no repeat blocks. Making
/// it optional would buy nothing — postcard is not self-describing, so `None`
/// still encodes a byte and every recorded digest moves either way — while
/// costing a "no shape recorded" posture a verifier would have to decide how to
/// treat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainShape {
    /// `sha256` over the canonical encoding of the **unexpanded** manifest: the
    /// template, not the result, and the decoded manifest, not the file.
    ///
    /// Digesting the parse rather than the bytes is what keeps this
    /// format-agnostic — the same chain authored as `Raster.toml` or as
    /// `chain.json` must produce the same digest — and it pins the thing
    /// expansion is actually a function of. Hashing file bytes would pin an
    /// encoding instead, and never establish that the verifier's decode is the
    /// one the claimer expanded from.
    ///
    /// It also closes `chain-fraud-proof.md`'s S1: the commitment now names the
    /// chain spec, so a spec-chained parameter recorded as `External` is
    /// detectable rather than merely suspicious.
    pub spec_digest: Vec<u8>,
    /// One entry per repeat block, in expansion order.
    pub repeats: Vec<RepeatResolution>,
}

/// The trip count one repeat block resolved to, and where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepeatResolution {
    /// The block's `name`, as authored.
    pub name: String,
    /// Which stage produced the count (index into `ChainCommitment::stages`), or
    /// `None` for a literal or external source — those are manifest-static and
    /// re-derived without touching a stage artifact.
    pub source_stage: Option<u32>,
    /// The external's declared commitment, for a `{ input = .. }` source.
    pub source_commitment: Vec<u8>,
    /// The `select!` path into that external. Empty for the other two sources:
    /// a stage-produced count is the producing stage's *whole* output, which is
    /// what keeps a shape fault to one hash in-guest rather than a payload
    /// decoder.
    pub selector: String,
    /// The unsigned width the count-producing stage returns.
    ///
    /// Not derivable from the count and therefore recorded: a leaf's bytes are
    /// fixed-width little-endian and never widened, so `7u32` and `7u64` are
    /// different payloads with different roots (`crate::input::IndexWidth`).
    /// Reading it from anywhere but the commitment would be a forgery vector —
    /// a `Shape` fault asserts an *inequality*, so an accuser who supplied the
    /// width could pick one that fails against an honest chain.
    pub width: IndexWidth,
    /// The manifest's bound on this count, checked before expansion.
    pub max: u32,
    pub resolved_count: u32,
}

impl ChainCommitment {
    /// The chain digest: `sha256(postcard(self))`.
    pub fn digest(&self) -> [u8; 32] {
        let bytes = postcard::to_allocvec(self).expect("ChainCommitment is serializable");
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        digest
    }
}

/// Which committed chain fault a chain fraud proof exhibits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainFaultKind {
    /// A `from` parameter's committed input value differs from the producing
    /// stage's committed output structural root — an inconsistency inside the
    /// `ChainCommitment` itself, proven from the manifest the checkpoint
    /// committed.
    ///
    /// One of two faults a `ChainCommitment` condemns *on its own*. Execution
    /// fraud is not among them: with no trace commitment in the checkpoint
    /// there is nothing for a stage fraud receipt to be attributed against,
    /// so a divergent stage is established by the challenge/response protocol
    /// instead — see `docs/proposals/chain-io-commitment.md` §3. Until that
    /// lands, `chain audit --execution` detects it and `chain fraud-prove`
    /// emits a terminal-window receipt as evidence.
    Link,
    /// A repeat block's recorded trip count is not the one the stage that
    /// produced it committed — so the chain has the wrong *number of stages*,
    /// and every positional producer index after the block is a claim about a
    /// graph nobody ran.
    ///
    /// Admissible beside `Link` for the same reason `Link` is: it is
    /// re-derived entirely from the `ChainCommitment`, with no trace and no
    /// artifact. Given `chain-repeat.md` §3's rule that a stage-produced count
    /// is the producing stage's *whole* output, the check is one hash — the
    /// guest re-encodes the recorded count and compares against what that
    /// stage committed, which is strictly less machinery than `Link`'s
    /// authorization receipt.
    ///
    /// Covers stage-sourced counts only. A literal or external count is
    /// manifest-static, so a verifier re-derives it host-side for free and
    /// there is nothing in the commitment to compare against.
    Shape,
}

/// The chain-fraud guest's journal: *"chain `chain_commitment_digest` is
/// fraudulent; stage `faulty_stage` (program `stage_program_commitment`) is to
/// blame."*
///
/// The inner image ids are committed so the relying party can pin the whole
/// trust chain: the guest guarantees they are the ids it actually ran
/// `env::verify` against, and the verifier checks them (plus this receipt's
/// own chain-fraud image id, pinned out-of-band) against known-good values.
/// There is deliberately no self-referential chain-fraud image id — a receipt
/// cannot vouch for its own verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainFraudJournal {
    /// Which chain — `== ChainCommitment::digest()`, recomputed in-guest.
    pub chain_commitment_digest: [u8; 32],
    pub faulty_stage: u32,
    /// `== ChainCommitment.stages[faulty_stage].program_commitment`.
    pub stage_program_commitment: Vec<u8>,
    pub fault: ChainFaultKind,
    /// Reserved for a fault kind that verifies a transition receipt. Always
    /// empty today: `Link` is the only kind, and it verifies an authorization
    /// journal. Kept so the journal shape survives the dispute protocol
    /// landing (`docs/proposals/chain-io-commitment.md` §5).
    pub transition_image_id: Vec<u8>,
    /// The authorization-guest image id the manifest journal was verified
    /// against (`Link` faults; empty for `Execution`).
    pub authorization_image_id: Vec<u8>,
}

/// Per-fault evidence handed to the chain-fraud guest. Every receipt named
/// here must also be supplied to the prover as an assumption.
#[derive(Clone, Serialize, Deserialize)]
pub enum ChainFraudEvidence {
    /// The consumer stage's authorized-manifest journal (which carries the
    /// parsed `param -> commitment` map), plus the image id to verify it
    /// against, and the offending `from` parameter.
    Link {
        parameter: String,
        authorization_journal: AuthorizationJournal,
        authorization_image_id: Vec<u8>,
    },
    /// Which repeat block disagrees — and **nothing else**.
    ///
    /// Every value the check needs (the width, the resolved count, the
    /// producing stage, and what that stage committed) is read out of
    /// `chain_commitment_bytes`, which the guest hashes before decoding. That
    /// is a soundness requirement rather than economy: like `Link`, this fault
    /// asserts an *inequality*, so an accuser permitted to supply any of those
    /// values could pick a width that fails against an honest chain and
    /// condemn it. The accuser gets to point, not to describe.
    Shape { repeat_index: u32 },
}

/// Everything the chain-fraud guest reads from the host.
#[derive(Clone, Serialize, Deserialize)]
pub struct ChainFraudInput {
    /// The exact `chain-commitment` bytes (`postcard(ChainCommitment)`); the
    /// guest hashes these into `chain_commitment_digest` before decoding, so
    /// attribution is bound to the same bytes the digest names.
    pub chain_commitment_bytes: Vec<u8>,
    pub faulty_stage: u32,
    pub evidence: ChainFraudEvidence,
}
