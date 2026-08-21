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
use crate::transition::TransitionJournal;

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
    /// `TraceCommitmentHeader::digest()` of the stage's `commit.bin` — ties
    /// this checkpoint to a specific (optimistically auditable) run without
    /// embedding the trace-sized commitment itself. `chain audit` recomputes
    /// it from the stage artifact; a stage fraud receipt's
    /// `refuted_trace_commitment` is attributed by equality with it.
    pub trace_commitment_digest: Vec<u8>,
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
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        digest
    }
}

/// Which committed chain fault a chain fraud proof exhibits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainFaultKind {
    /// The stage's committed trace diverges from honest execution — proven by
    /// a transition fraud receipt bound to the stage's `commit.bin`.
    Execution,
    /// A `from` parameter's committed input value differs from the producing
    /// stage's committed output structural root — an inconsistency inside the
    /// `ChainCommitment` itself, proven from the manifest the checkpoint
    /// committed.
    Link,
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
    /// The transition-guest image id the stage fraud receipt was verified
    /// against (`Execution` faults; empty for `Link`).
    pub transition_image_id: Vec<u8>,
    /// The authorization-guest image id the manifest journal was verified
    /// against (`Link` faults; empty for `Execution`).
    pub authorization_image_id: Vec<u8>,
}

/// Per-fault evidence handed to the chain-fraud guest. Every receipt named
/// here must also be supplied to the prover as an assumption.
#[derive(Clone, Serialize, Deserialize)]
pub enum ChainFraudEvidence {
    /// A stage fraud receipt's (reduced, `Finished`) journal, plus the image
    /// id to verify it against. The guest asserts the journal's own
    /// `transition_image_id` matches, so the id it commits is the one the
    /// receipt's whole recursion was checked with.
    Execution {
        fraud_journal: TransitionJournal,
        transition_image_id: Vec<u8>,
    },
    /// The consumer stage's authorized-manifest journal (which carries the
    /// parsed `param -> commitment` map), plus the image id to verify it
    /// against, and the offending `from` parameter.
    Link {
        parameter: String,
        authorization_journal: AuthorizationJournal,
        authorization_image_id: Vec<u8>,
    },
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
