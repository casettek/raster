//! Shared chain types (guest and host).
//!
//! The checkpoint types were born in `raster-cli`'s `chain` module; they live
//! here because the chain-fraud guest must decode the exact `ChainCommitment`
//! bytes it attributes a fault to (the same reason `TransitionJournal` lives
//! here). The CLI re-imports them. See `docs/proposals/program-chain.md` and
//! `docs/proposals/chain-fraud-proof.md`.

use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::vec::Vec;

use serde::{Deserialize, Serialize};
use crate::hash::{Digest, Sha256};

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

/// A cheap stage challenge (`docs/proposals/stage-challenge.md` §Phase 2):
/// *stage `challenged_stage` of this chain does not output what the chain
/// claims — here is a trace commitment `H_C` of the stage, and the output it
/// ends in.*
///
/// It carries **no proof**. Whether `H_C` is an honest execution, and whether
/// it ends in `claimed_output`, is exactly what an answer disproves: a
/// refutation (`H_C` diverges) or a mislabel proof (`H_C` is honest but ends
/// elsewhere). Admission checks only what public data can settle
/// (`raster_prover::challenge::verify_challenge`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageChallenge {
    /// The chain commitment under challenge, whole, so admission and the
    /// answer's aggregation recompute its digest rather than trust one.
    #[serde(with = "serde_bytes")]
    pub chain_commitment_bytes: Vec<u8>,
    pub challenged_stage: u32,
    /// `H_C`'s header; its body is published alongside (locally, for now:
    /// `chain challenge` writes it beside the challenge).
    pub trace_commitment_header: crate::transition::TraceCommitmentHeader,
    /// The output the challenger says `H_C` ends in — an assertion, not a
    /// proof. `sha256(output.bin)`, the form of `output_payload_commitment`.
    #[serde(with = "serde_bytes")]
    pub claimed_output: Vec<u8>,
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
    /// instead — see `docs/proposals/stage-challenge.md`: a `Link` or `Shape`
    /// receipt is a *proven* challenge, a wrong stage output a *disputed* one.
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

// ---------------------------------------------------------------------------
// The defence (`docs/proposals/stage-challenge.md` §Phase 3)
// ---------------------------------------------------------------------------

/// Which defence dismissed a disputed challenge. Read from the transition
/// journal's final state, never offered by the defender: `Finished` is a
/// refutation, `TerminalMatch` a mislabel proof, and no other state is either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefenceReason {
    /// `H_C` is not an honest, complete execution of the stage: a window of
    /// it diverges from honest execution, or it misstates the trace's length.
    Refuted,
    /// `H_C` is honest, but ends in an output other than `claimed_output`.
    Mislabelled,
}

/// Everything the defence guest reads from the host. The transition receipt
/// whose journal this carries must be supplied to the prover as an assumption.
#[derive(Clone, Serialize, Deserialize)]
pub struct DefenceInput {
    /// The challenge exactly as posted (`postcard(StageChallenge)`); hashed
    /// into `challenge_digest` before it is decoded, so the journal names
    /// the bytes it checked.
    #[serde(with = "serde_bytes")]
    pub challenge_bytes: Vec<u8>,
    pub transition_journal: crate::transition::TransitionJournal,
    /// The image id the transition journal is verified against.
    #[serde(with = "serde_bytes")]
    pub transition_image_id: Vec<u8>,
}

/// The defence guest's journal: *"challenge `challenge_digest`, against stage
/// `challenged_stage` of chain `chain_commitment_digest`, is false."*
///
/// Every field is computed or checked in the guest ([`defend`]). The inner
/// image ids are committed for the relying party to pin against known-good
/// values, as `ChainFraudJournal` does: the guest guarantees they are what it
/// verified against, not that they are honest guests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefenceJournal {
    /// `sha256(postcard(StageChallenge))` — which challenge this answers.
    pub challenge_digest: [u8; 32],
    /// `sha256(challenge.chain_commitment_bytes)` — the chain it defends.
    pub chain_commitment_digest: [u8; 32],
    pub challenged_stage: u32,
    /// `H_C`'s digest, the commitment the window was proven against.
    #[serde(with = "serde_bytes")]
    pub trace_commitment: Vec<u8>,
    pub reason: DefenceReason,
    /// `X`, the output honest execution of `H_C` ends in: `Some` exactly for
    /// [`DefenceReason::Mislabelled`], and never the challenge's
    /// `claimed_output`.
    pub attested_output: Option<Vec<u8>>,
    #[serde(with = "serde_bytes")]
    pub transition_image_id: Vec<u8>,
    /// The authorization guest the window's input manifest was verified
    /// against. Pinned like the transition guest's: the manifest equality
    /// below is only as good as the guest that produced the manifest journal.
    #[serde(with = "serde_bytes")]
    pub authorization_image_id: Vec<u8>,
}

/// Why a transition journal does not defend against a challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefenceRefusal {
    UndecodableChallenge(String),
    UndecodableChainCommitment(String),
    StageOutOfRange { stage: u32, stages: usize },
    /// The journal names a transition image id other than the one it was
    /// verified against: its earlier steps were checked by another guest.
    TransitionImageIdMismatch,
    /// The window was proven against a commitment other than `H_C`.
    AnotherTraceCommitment,
    /// The window executes a program other than the challenged stage's.
    AnotherProgram,
    /// The window executes on inputs other than the challenged stage's.
    AnotherInputManifest,
    /// The window has not ended (`Init` or `Next`).
    WindowNotEnded,
    /// A `TerminalMatch` window that is not terminal — unreachable from an
    /// honest transition guest, checked so the reason never rests on it.
    NotTerminal,
    /// A mislabel proof whose output is not `Established`.
    OutputNotEstablished,
    /// A mislabel proof ending in the claimed output: `H_C` is honest and the
    /// challenger labelled it correctly, so this window confirms the challenge
    /// rather than dismissing it.
    OutputIsTheClaimed,
}

impl core::fmt::Display for DefenceRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UndecodableChallenge(e) => write!(f, "the challenge does not decode: {e}"),
            Self::UndecodableChainCommitment(e) => {
                write!(f, "the challenge's chain commitment does not decode: {e}")
            }
            Self::StageOutOfRange { stage, stages } => {
                write!(f, "the challenge names stage {stage} of a {stages}-stage chain")
            }
            Self::TransitionImageIdMismatch => write!(
                f,
                "the transition journal names a different transition image id than it was \
                 verified against"
            ),
            Self::AnotherTraceCommitment => {
                write!(f, "the window was proven against another trace commitment, not H_C")
            }
            Self::AnotherProgram => {
                write!(f, "the window executes another program than the challenged stage's")
            }
            Self::AnotherInputManifest => {
                write!(f, "the window executes on other inputs than the challenged stage's")
            }
            Self::WindowNotEnded => write!(f, "the window has not ended"),
            Self::NotTerminal => write!(f, "a TerminalMatch window that is not terminal"),
            Self::OutputNotEstablished => {
                write!(f, "the mislabel proof establishes no output")
            }
            Self::OutputIsTheClaimed => write!(
                f,
                "H_C ends in the claimed output: the challenge is true, and no defence exists"
            ),
        }
    }
}

/// Check a proven transition window against a posted challenge, and build the
/// journal that dismisses it. The defence guest's whole decision — run by the
/// guest after verifying the window's receipt, and by the host before proving,
/// so the two cannot disagree.
///
/// Common to both defences: the window was proven against `H_C`, on the
/// challenged stage's program and committed inputs. Those equalities are what
/// make the window *about this challenge*: a refutation of another commitment
/// says nothing about `H_C`, and an honest output of another program or input
/// says nothing about this stage. Then by the window's final state:
/// - `Finished` — a refutation: `H_C` is not an honest, complete execution.
/// - `TerminalMatch` — a mislabel proof: honest execution of `H_C` ends in an
///   established output `X`, and `X` is not `claimed_output`.
pub fn defend(input: &DefenceInput) -> Result<DefenceJournal, DefenceRefusal> {
    use crate::transition::{OutputAuthorization, TransitionState};

    let challenge_digest: [u8; 32] = Sha256::digest(&input.challenge_bytes).into();
    let challenge: StageChallenge = postcard::from_bytes(&input.challenge_bytes)
        .map_err(|e| DefenceRefusal::UndecodableChallenge(e.to_string()))?;
    let chain_commitment_digest: [u8; 32] =
        Sha256::digest(&challenge.chain_commitment_bytes).into();
    let chain: ChainCommitment = postcard::from_bytes(&challenge.chain_commitment_bytes)
        .map_err(|e| DefenceRefusal::UndecodableChainCommitment(e.to_string()))?;
    let stage = usize::try_from(challenge.challenged_stage)
        .ok()
        .and_then(|index| chain.stages.get(index))
        .ok_or(DefenceRefusal::StageOutOfRange {
            stage: challenge.challenged_stage,
            stages: chain.stages.len(),
        })?;

    let journal = &input.transition_journal;
    if journal.transition_image_id != input.transition_image_id {
        return Err(DefenceRefusal::TransitionImageIdMismatch);
    }
    let trace_commitment = challenge.trace_commitment_header.digest();
    if journal.refuted_trace_commitment != trace_commitment {
        return Err(DefenceRefusal::AnotherTraceCommitment);
    }
    if journal.program_commitment != stage.program_commitment {
        return Err(DefenceRefusal::AnotherProgram);
    }
    if journal.input_manifest_commitment != stage.input_manifest_commitment {
        return Err(DefenceRefusal::AnotherInputManifest);
    }
    // `entrypoint_authorization` needs no check: both of its values mean the
    // entry arguments are bound (`Established`) or there are none
    // (`NotRequired`), and the window has it from the moment it opens.

    let (reason, attested_output) = match &journal.current_state {
        TransitionState::Finished => (DefenceReason::Refuted, None),
        TransitionState::TerminalMatch => {
            if !journal.window_is_terminal {
                return Err(DefenceRefusal::NotTerminal);
            }
            let OutputAuthorization::Established { output_commitment } =
                &journal.output_authorization
            else {
                return Err(DefenceRefusal::OutputNotEstablished);
            };
            if *output_commitment == challenge.claimed_output {
                return Err(DefenceRefusal::OutputIsTheClaimed);
            }
            (DefenceReason::Mislabelled, Some(output_commitment.clone()))
        }
        TransitionState::Init(_) | TransitionState::Next(_) => {
            return Err(DefenceRefusal::WindowNotEnded)
        }
    };

    Ok(DefenceJournal {
        challenge_digest,
        chain_commitment_digest,
        challenged_stage: challenge.challenged_stage,
        trace_commitment,
        reason,
        attested_output,
        transition_image_id: input.transition_image_id.clone(),
        authorization_image_id: journal.authorization_image_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use crate::fingerprint::{BitPacker, Fingerprint};
    use crate::transition::{
        EntrypointAuthorization, InitTransition, OutputAuthorization, SerializableFrontier,
        TraceCommitmentHeader, TransitionJournal, TransitionState,
    };

    const TRANSITION_ID: [u8; 32] = [0x7a; 32];
    const PROGRAM: [u8; 32] = [0x10; 32];
    const MANIFEST: [u8; 32] = [0x11; 32];
    const COMMITTED_OUTPUT: [u8; 32] = [0x12; 32];
    const CLAIMED_OUTPUT: [u8; 32] = [0x13; 32];

    fn header() -> TraceCommitmentHeader {
        TraceCommitmentHeader {
            bits_packer: BitPacker::new(1),
            fingerprint_len: 200,
            fingerprint_root: vec![1; 32],
            revealed_items_commitment: vec![2; 32],
            window_size: 128,
            revealed_tail_roots_commitment: vec![3; 32],
        }
    }

    fn challenge() -> StageChallenge {
        let chain = ChainCommitment {
            stages: vec![StageCheckpoint {
                name: "stage".into(),
                program_commitment: PROGRAM.to_vec(),
                input_manifest_commitment: MANIFEST.to_vec(),
                input_bindings: BTreeMap::new(),
                output_payload_commitment: COMMITTED_OUTPUT.to_vec(),
                output_structural_commitment: vec![0x14; 32],
            }],
            shape: ChainShape {
                spec_digest: vec![0x15; 32],
                repeats: Vec::new(),
            },
        };
        StageChallenge {
            chain_commitment_bytes: postcard::to_allocvec(&chain).unwrap(),
            challenged_stage: 0,
            trace_commitment_header: header(),
            claimed_output: CLAIMED_OUTPUT.to_vec(),
        }
    }

    fn frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: vec![0; 32],
            ommers: Vec::new(),
        }
    }

    /// The window a defence over `challenge()`'s `H_C` ends in, with
    /// `output` attested — before any field is spoiled.
    fn journal(current_state: TransitionState, output: &[u8]) -> TransitionJournal {
        TransitionJournal {
            init_state: InitTransition {
                init_frontier: frontier(),
                init_storage_frontier: frontier(),
                init_storage_root: vec![0; 32],
                init_storage_index_root: vec![0; 32],
                fingerprint: Fingerprint {
                    bits_packer: BitPacker::new(1),
                    bits: Vec::new(),
                    len: 0,
                },
            },
            current_state,
            transition_image_id: TRANSITION_ID.to_vec(),
            authorization_image_id: vec![0xa0; 32],
            input_manifest_commitment: MANIFEST.to_vec(),
            program_commitment: PROGRAM.to_vec(),
            refuted_trace_commitment: header().digest(),
            entrypoint_authorization: EntrypointAuthorization::Established,
            output_authorization: OutputAuthorization::Established {
                output_commitment: output.to_vec(),
            },
            window_is_terminal: true,
            final_committed_root: None,
        }
    }

    fn input(challenge: &StageChallenge, journal: TransitionJournal) -> DefenceInput {
        DefenceInput {
            challenge_bytes: postcard::to_allocvec(challenge).unwrap(),
            transition_journal: journal,
            transition_image_id: TRANSITION_ID.to_vec(),
        }
    }

    fn mislabel() -> TransitionJournal {
        journal(TransitionState::TerminalMatch, &COMMITTED_OUTPUT)
    }

    #[test]
    fn a_refutation_of_h_c_dismisses_the_challenge() {
        let challenge = challenge();
        let mut window = journal(TransitionState::Finished, &[]);
        // A mid-trace refutation has no output yet, and is not terminal.
        window.output_authorization = OutputAuthorization::Pending;
        window.window_is_terminal = false;
        let defence = defend(&input(&challenge, window)).expect("a refutation");
        assert_eq!(defence.reason, DefenceReason::Refuted);
        assert_eq!(defence.attested_output, None);
        assert_eq!(defence.challenged_stage, 0);
        assert_eq!(defence.trace_commitment, header().digest());
        let challenge_bytes = postcard::to_allocvec(&challenge).unwrap();
        assert_eq!(defence.challenge_digest, <[u8; 32]>::from(Sha256::digest(&challenge_bytes)));
        assert_eq!(
            defence.chain_commitment_digest,
            <[u8; 32]>::from(Sha256::digest(&challenge.chain_commitment_bytes))
        );
        assert_eq!(defence.transition_image_id, TRANSITION_ID.to_vec());
    }

    #[test]
    fn an_honest_end_other_than_the_claimed_output_is_a_mislabel_proof() {
        let defence = defend(&input(&challenge(), mislabel())).expect("a mislabel proof");
        assert_eq!(defence.reason, DefenceReason::Mislabelled);
        assert_eq!(defence.attested_output, Some(COMMITTED_OUTPUT.to_vec()));
    }

    /// The true challenge: `H_C` is honest and ends where the challenger said.
    #[test]
    fn an_honest_end_in_the_claimed_output_defends_nothing() {
        let window = journal(TransitionState::TerminalMatch, &CLAIMED_OUTPUT);
        assert_eq!(
            defend(&input(&challenge(), window)),
            Err(DefenceRefusal::OutputIsTheClaimed)
        );
    }

    #[test]
    fn a_mislabel_proof_must_establish_an_output() {
        for output in [OutputAuthorization::Pending, OutputAuthorization::NotRequired] {
            let mut window = mislabel();
            window.output_authorization = output;
            assert_eq!(
                defend(&input(&challenge(), window)),
                Err(DefenceRefusal::OutputNotEstablished)
            );
        }
    }

    #[test]
    fn a_mislabel_proof_must_be_terminal() {
        let mut window = mislabel();
        window.window_is_terminal = false;
        assert_eq!(defend(&input(&challenge(), window)), Err(DefenceRefusal::NotTerminal));
    }

    /// The reason is read from the state, so a window that has not ended is
    /// neither defence — there is no way to offer it as one.
    #[test]
    fn a_window_that_has_not_ended_is_no_defence() {
        let window = mislabel();
        let state = TransitionState::Init(window.init_state.clone());
        let window = TransitionJournal {
            current_state: state,
            ..window
        };
        assert_eq!(defend(&input(&challenge(), window)), Err(DefenceRefusal::WindowNotEnded));
    }

    #[test]
    fn a_window_over_another_commitment_program_or_manifest_is_no_defence() {
        for state in [TransitionState::Finished, TransitionState::TerminalMatch] {
            let base = || journal(state.clone(), &COMMITTED_OUTPUT);

            let mut window = base();
            window.refuted_trace_commitment = vec![0xee; 32];
            assert_eq!(
                defend(&input(&challenge(), window)),
                Err(DefenceRefusal::AnotherTraceCommitment)
            );

            let mut window = base();
            window.program_commitment = vec![0xee; 32];
            assert_eq!(defend(&input(&challenge(), window)), Err(DefenceRefusal::AnotherProgram));

            let mut window = base();
            window.input_manifest_commitment = vec![0xee; 32];
            assert_eq!(
                defend(&input(&challenge(), window)),
                Err(DefenceRefusal::AnotherInputManifest)
            );
        }
    }

    /// A journal whose earlier steps were verified against another guest.
    #[test]
    fn a_window_naming_another_transition_guest_is_no_defence() {
        let mut window = mislabel();
        window.transition_image_id = vec![0xee; 32];
        assert_eq!(
            defend(&input(&challenge(), window)),
            Err(DefenceRefusal::TransitionImageIdMismatch)
        );
    }

    #[test]
    fn a_challenge_naming_a_missing_stage_is_refused() {
        let mut challenge = challenge();
        challenge.challenged_stage = 1;
        assert_eq!(
            defend(&input(&challenge, mislabel())),
            Err(DefenceRefusal::StageOutOfRange {
                stage: 1,
                stages: 1
            })
        );
    }
}
