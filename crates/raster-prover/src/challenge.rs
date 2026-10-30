//! Challenges to a chain commitment, and their admission
//! (`docs/proposals/stage-challenge.md` §Phase 2).
//!
//! A challenge takes one of two forms, fixed by the fault it alleges:
//!
//! - **Proven** — a fault the chain commitment exhibits by itself (`Link`,
//!   `Shape`). The challenger proves it on the spot with the chain-fraud guest;
//!   there is nothing to defend, so verifying it condemns the chain.
//! - **Disputed** — a stage whose committed output the challenger disputes.
//!   It carries no proof, so admission checks only what public data can settle:
//!   that it names a real stage with a committed output, that it disputes that
//!   output, and that its trace commitment is well-formed. Whether the trace is
//!   honest, and whether it ends in the claimed output, is left to the defence —
//!   a refutation or a mislabel proof — which is the only thing that runs the
//!   zkVM.

use std::fmt;

use raster_core::chain::{ChainCommitment, ChainFraudJournal, StageChallenge, StageCheckpoint};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chain_fraud::verify_chain_fraud_receipt;
use crate::trace::{FraudProofConfig, TraceCommitment, TraceCommitmentExt};

/// The protocol's fraud-proof window size: every disputed challenge's `H_C`
/// is committed at it, so its defence costs one window proof of a known size.
/// Not the challenger's choice — the defence's cost grows with the window and
/// the challenger's does not, so a chosen window would be a griefing knob.
pub const PROTOCOL_WINDOW_SIZE: usize = 128;

/// The window size `H_C` must be committed at for a trace of `trace_len`
/// items: [`PROTOCOL_WINDOW_SIZE`], or for a trace too short for it, the
/// largest power of two below its length — a commitment needs more items
/// than its window. `None` for a trace too short for any window (2 items or
/// fewer: a window of 1 would need more fingerprint bits than an item has).
///
/// A function of `H_C`'s own `fingerprint_len`, so admission checks it from
/// the header alone. A challenger that inflates `fingerprint_len` gains at
/// most the cap, and its `H_C` is then not the honest trace.
pub fn protocol_window_size(trace_len: usize) -> Option<usize> {
    if trace_len <= 2 {
        return None;
    }
    let below = 1usize << (usize::BITS - 1 - (trace_len - 1).leading_zeros());
    Some(below.min(PROTOCOL_WINDOW_SIZE))
}

/// A challenge to a chain commitment, in the one form its fault allows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Challenge {
    /// A `Link` or `Shape` fault, proven: verifying it condemns the chain.
    Proven(ProvenChallenge),
    /// A stage's output, disputed without proof: defended, or upheld at the
    /// timeout.
    Disputed(StageChallenge),
}

/// A fault the chain commitment exhibits by itself, with the chain-fraud
/// receipt that exhibits it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenChallenge {
    /// The chain commitment under challenge, whole, so the receipt's chain
    /// digest is checked against bytes rather than trusted.
    pub chain_commitment_bytes: Vec<u8>,
    pub receipt: risc0_zkvm::Receipt,
}

/// What verifying a challenge establishes.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// A proven challenge: the chain is fraudulent, and `journal` names the
    /// stage to blame.
    Condemned {
        chain: ChainCommitment,
        chain_digest: [u8; 32],
        journal: ChainFraudJournal,
    },
    /// An admitted disputed challenge, awaiting a defence.
    AwaitingDefence(AdmittedChallenge),
}

impl Verdict {
    pub fn chain_digest(&self) -> [u8; 32] {
        match self {
            Self::Condemned { chain_digest, .. } => *chain_digest,
            Self::AwaitingDefence(admitted) => admitted.chain_digest,
        }
    }
}

/// Verify a challenge of either form — a pure function of the challenge alone.
pub fn verify(challenge: &Challenge) -> Result<Verdict, AdmissionRefusal> {
    match challenge {
        Challenge::Proven(proven) => verify_proven(proven),
        Challenge::Disputed(disputed) => verify_challenge(disputed).map(Verdict::AwaitingDefence),
    }
}

fn verify_proven(challenge: &ProvenChallenge) -> Result<Verdict, AdmissionRefusal> {
    let chain = decode_chain(&challenge.chain_commitment_bytes)?;
    let chain_digest = chain_digest(&challenge.chain_commitment_bytes);
    let journal = verify_chain_fraud_receipt(&challenge.receipt)
        .map_err(|e| AdmissionRefusal::InvalidFraudReceipt(e.to_string()))?;
    // The guest hashed the bytes it attributed the fault in; they must be these.
    if journal.chain_commitment_digest != chain_digest {
        return Err(AdmissionRefusal::ReceiptNamesAnotherChain {
            receipt: journal.chain_commitment_digest,
            challenge: chain_digest,
        });
    }
    Ok(Verdict::Condemned {
        chain,
        chain_digest,
        journal,
    })
}

fn decode_chain(bytes: &[u8]) -> Result<ChainCommitment, AdmissionRefusal> {
    postcard::from_bytes(bytes).map_err(|e| AdmissionRefusal::UndecodableChainCommitment(e.to_string()))
}

/// The digest a challenge names: of the bytes it carries, as the chain-fraud
/// guest computes it. Equal to `ChainCommitment::digest` for the canonical
/// encoding `chain run` writes.
fn chain_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Why a challenge is refused before anyone answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionRefusal {
    /// `chain_commitment_bytes` does not decode as a `ChainCommitment`.
    UndecodableChainCommitment(String),
    /// A proven challenge's receipt is not a valid chain-fraud receipt: wrong
    /// image id, or an inner guest that is not the known-good one.
    InvalidFraudReceipt(String),
    /// A proven challenge's receipt condemns a different chain than the one
    /// the challenge carries.
    ReceiptNamesAnotherChain { receipt: [u8; 32], challenge: [u8; 32] },
    /// `challenged_stage` names no stage of the chain.
    UnknownStage { challenged: u32, stages: usize },
    /// The stage commits no output (its `main` returns `()`): nothing to
    /// dispute.
    UnitOutputStage,
    /// `claimed_output` is the output the chain already commits: the
    /// challenge disputes nothing.
    OutputNotDisputed,
    /// `H_C` commits a trace too short for any fraud-proof window.
    TraceTooShort { fingerprint_len: u64 },
    /// `H_C` is not committed at the protocol's window size for its length
    /// (`protocol_window_size`).
    WindowSizeNotProtocol { fingerprint_len: u64, required: usize, found: u64 },
    /// `H_C`'s bits per item is not the canonical value for its window size
    /// (decision 1: exact match, not a floor), so its fingerprint would be
    /// weaker or stronger than every honest commitment of that size.
    NonCanonicalBitsPerItem { window_size: u64, expected: usize, found: usize },
}

impl fmt::Display for AdmissionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndecodableChainCommitment(error) => {
                write!(f, "the chain commitment does not decode: {error}")
            }
            Self::InvalidFraudReceipt(error) => write!(f, "invalid chain-fraud receipt: {error}"),
            Self::ReceiptNamesAnotherChain { receipt, challenge } => write!(
                f,
                "the receipt condemns chain {} but the challenge carries chain {}",
                hex::encode(receipt),
                hex::encode(challenge)
            ),
            Self::UnknownStage { challenged, stages } => {
                write!(f, "stage {challenged} does not exist; the chain has {stages} stage(s)")
            }
            Self::UnitOutputStage => write!(f, "the stage commits no output to dispute"),
            Self::OutputNotDisputed => {
                write!(f, "the claimed output is the one the chain already commits")
            }
            Self::TraceTooShort { fingerprint_len } => write!(
                f,
                "the trace commitment covers {fingerprint_len} item(s), too few for any window"
            ),
            Self::WindowSizeNotProtocol {
                fingerprint_len,
                required,
                found,
            } => write!(
                f,
                "a trace of {fingerprint_len} items must be committed at window {required} \
                 (protocol window {PROTOCOL_WINDOW_SIZE}), the commitment uses {found}"
            ),
            Self::NonCanonicalBitsPerItem {
                window_size,
                expected,
                found,
            } => write!(
                f,
                "a window of {window_size} items needs exactly {expected} bit(s) per item, \
                 the commitment uses {found}"
            ),
        }
    }
}

/// An admitted disputed challenge, with what its defence will be checked
/// against.
#[derive(Debug, Clone)]
pub struct AdmittedChallenge {
    pub chain: ChainCommitment,
    pub chain_digest: [u8; 32],
    pub stage_index: usize,
    pub stage: StageCheckpoint,
    /// `H_C`'s digest: what an answer's `refuted_trace_commitment` must be.
    pub trace_commitment: Vec<u8>,
}

/// Admit or refuse a disputed challenge — a pure function of the challenge
/// alone.
pub fn verify_challenge(challenge: &StageChallenge) -> Result<AdmittedChallenge, AdmissionRefusal> {
    let chain = decode_chain(&challenge.chain_commitment_bytes)?;

    let stage_index = challenge.challenged_stage as usize;
    let stage = chain
        .stages
        .get(stage_index)
        .cloned()
        .ok_or(AdmissionRefusal::UnknownStage {
            challenged: challenge.challenged_stage,
            stages: chain.stages.len(),
        })?;
    if stage.output_payload_commitment.is_empty() {
        return Err(AdmissionRefusal::UnitOutputStage);
    }
    if challenge.claimed_output == stage.output_payload_commitment {
        return Err(AdmissionRefusal::OutputNotDisputed);
    }

    let header = &challenge.trace_commitment_header;
    let required = usize::try_from(header.fingerprint_len)
        .ok()
        .and_then(protocol_window_size)
        .ok_or(AdmissionRefusal::TraceTooShort {
            fingerprint_len: header.fingerprint_len,
        })?;
    if header.window_size != required as u64 {
        return Err(AdmissionRefusal::WindowSizeNotProtocol {
            fingerprint_len: header.fingerprint_len,
            required,
            found: header.window_size,
        });
    }
    let canonical = FraudProofConfig::from_window_size(required)
        .expect("every protocol window size is a legal window size");
    if header.bits_packer.0 != canonical.bits_per_item {
        return Err(AdmissionRefusal::NonCanonicalBitsPerItem {
            window_size: header.window_size,
            expected: canonical.bits_per_item,
            found: header.bits_packer.0,
        });
    }

    Ok(AdmittedChallenge {
        chain_digest: chain_digest(&challenge.chain_commitment_bytes),
        chain,
        stage_index,
        stage,
        trace_commitment: header.digest(),
    })
}

/// Why `H_C`'s body does not count as available.
///
/// Availability is what starts a disputed challenge's clock
/// (`docs/proposals/settlement-layer.md`), and a defence can only be built
/// from a body the defender can walk. A body that is present but malformed is
/// no more available than a missing one. Admission cannot rule it out: the
/// header it checks hashes the body's parts without describing them, so a body
/// short one tail root is admitted under the header recomputed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyRefusal {
    /// The bytes do not decode as a `TraceCommitment`.
    Undecodable(String),
    /// The body's header digest is not the `H_C` the challenge names.
    NotTheNamedCommitment { body: Vec<u8>, named: Vec<u8> },
    /// The body is `H_C`, but fails `TraceCommitmentExt::validate`, so no
    /// defender can walk it.
    Malformed(String),
}

impl fmt::Display for BodyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Undecodable(error) => {
                write!(f, "the body does not decode as a trace commitment: {error}")
            }
            Self::NotTheNamedCommitment { body, named } => write!(
                f,
                "the body is trace commitment {}, but the challenge names {}",
                hex::encode(body),
                hex::encode(named)
            ),
            Self::Malformed(error) => {
                write!(f, "the body is the named trace commitment, but malformed: {error}")
            }
        }
    }
}

/// Accept `H_C`'s body as available for an admitted challenge: it decodes, it
/// is the commitment the challenge names, and it is well-formed — the body
/// `TraceVerifier::new` accepts, so a defence can be built from it.
///
/// The one definition of availability. Whatever decides that a body is
/// available — the CLI today, a settlement layer or a check of a
/// data-availability posting later — must apply this, or an admitted challenge
/// can name a body no one can defend against.
pub fn admit_body(
    bytes: &[u8],
    admitted: &AdmittedChallenge,
) -> Result<TraceCommitment, BodyRefusal> {
    let body: TraceCommitment =
        postcard::from_bytes(bytes).map_err(|e| BodyRefusal::Undecodable(e.to_string()))?;
    let digest = body.header().digest();
    if digest != admitted.trace_commitment {
        return Err(BodyRefusal::NotTheNamedCommitment {
            body: digest,
            named: admitted.trace_commitment.clone(),
        });
    }
    body.validate()
        .map_err(|e| BodyRefusal::Malformed(e.to_string()))?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceVerifier;
    use raster_core::cfs::{CfsCoordinates, ControlFlowSchema};
    use raster_core::chain::ChainShape;
    use raster_core::fingerprint::BitPacker;
    use raster_core::merkle::EMPTY_LEAF;
    use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord, StorageRoots, Trace};
    use raster_core::transition::TraceCommitmentHeader;
    use std::collections::BTreeMap;

    const COMMITTED: [u8; 32] = [7; 32];

    fn stage(output: Vec<u8>) -> StageCheckpoint {
        StageCheckpoint {
            name: "greet".into(),
            program_commitment: vec![1; 32],
            input_manifest_commitment: vec![2; 32],
            input_bindings: BTreeMap::new(),
            output_structural_commitment: output.clone(),
            output_payload_commitment: output,
        }
    }

    /// A trace of 40 items: protocol window 32.
    const ITEMS: u64 = 40;
    const WINDOW: u64 = 32;

    fn header(window_size: u64, bits_per_item: usize) -> TraceCommitmentHeader {
        sized_header(ITEMS, window_size, bits_per_item)
    }

    fn sized_header(
        fingerprint_len: u64,
        window_size: u64,
        bits_per_item: usize,
    ) -> TraceCommitmentHeader {
        TraceCommitmentHeader {
            bits_packer: BitPacker::new(bits_per_item),
            fingerprint_len,
            fingerprint_root: vec![3; 32],
            revealed_items_commitment: vec![4; 32],
            window_size,
            revealed_tail_roots_commitment: vec![5; 32],
        }
    }

    fn canonical_bits(window_size: usize) -> usize {
        FraudProofConfig::from_window_size(window_size).unwrap().bits_per_item
    }

    fn challenge(stages: Vec<StageCheckpoint>, claimed: Vec<u8>, h: TraceCommitmentHeader) -> StageChallenge {
        let chain = ChainCommitment {
            stages,
            shape: ChainShape {
                spec_digest: vec![9; 32],
                repeats: Vec::new(),
            },
        };
        StageChallenge {
            chain_commitment_bytes: postcard::to_allocvec(&chain).unwrap(),
            challenged_stage: 0,
            trace_commitment_header: h,
            claimed_output: claimed,
        }
    }

    #[test]
    fn a_challenge_disputing_a_committed_output_is_admitted() {
        let h = header(WINDOW, canonical_bits(32));
        let admitted =
            verify_challenge(&challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], h.clone()))
                .expect("admitted");
        assert_eq!(admitted.stage_index, 0);
        assert_eq!(admitted.trace_commitment, h.digest());
    }

    #[test]
    fn a_challenge_claiming_the_committed_output_is_refused() {
        let c = challenge(
            vec![stage(COMMITTED.to_vec())],
            COMMITTED.to_vec(),
            header(WINDOW, canonical_bits(32)),
        );
        assert_eq!(verify_challenge(&c).unwrap_err(), AdmissionRefusal::OutputNotDisputed);
    }

    #[test]
    fn a_unit_output_stage_cannot_be_challenged() {
        let c = challenge(vec![stage(Vec::new())], vec![8; 32], header(WINDOW, canonical_bits(32)));
        assert_eq!(verify_challenge(&c).unwrap_err(), AdmissionRefusal::UnitOutputStage);
    }

    #[test]
    fn a_challenge_naming_a_missing_stage_is_refused() {
        let mut c =
            challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], header(WINDOW, canonical_bits(32)));
        c.challenged_stage = 3;
        assert_eq!(
            verify_challenge(&c).unwrap_err(),
            AdmissionRefusal::UnknownStage { challenged: 3, stages: 1 }
        );
    }

    #[test]
    fn the_protocol_window_is_128_capped_below_the_trace_length() {
        for (trace_len, window) in [
            (0, None),
            (2, None),
            (3, Some(2)),
            (7, Some(4)),
            (8, Some(4)),
            (9, Some(8)),
            (128, Some(64)),
            (129, Some(128)),
            (100_000, Some(128)),
        ] {
            assert_eq!(protocol_window_size(trace_len), window, "trace of {trace_len}");
        }
    }

    /// Not the challenger's choice: any other window is refused, smaller or
    /// larger, and so is the full 128 for a trace too short for it.
    #[test]
    fn a_window_other_than_the_protocols_is_refused() {
        for found in [2u64, 16, 64] {
            let bits = canonical_bits(found as usize);
            let c = challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], header(found, bits));
            assert_eq!(
                verify_challenge(&c).unwrap_err(),
                AdmissionRefusal::WindowSizeNotProtocol { fingerprint_len: ITEMS, required: 32, found }
            );
        }
        let long = sized_header(1_000, 64, canonical_bits(64));
        let c = challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], long);
        assert_eq!(
            verify_challenge(&c).unwrap_err(),
            AdmissionRefusal::WindowSizeNotProtocol { fingerprint_len: 1_000, required: 128, found: 64 }
        );
    }

    #[test]
    fn a_long_trace_is_admitted_at_window_128() {
        let h = sized_header(1_000, 128, canonical_bits(128));
        assert!(verify_challenge(&challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], h)).is_ok());
    }

    #[test]
    fn a_trace_too_short_for_any_window_is_refused() {
        let c = challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], sized_header(2, 2, 64));
        assert_eq!(
            verify_challenge(&c).unwrap_err(),
            AdmissionRefusal::TraceTooShort { fingerprint_len: 2 }
        );
    }

    /// Decision 1: exact match. Fewer bits weaken the fingerprint; more bits
    /// are not a commitment any honest run of that window size produces.
    #[test]
    fn bits_per_item_must_be_exactly_canonical() {
        let expected = canonical_bits(32);
        for found in [expected - 1, expected + 1] {
            let c = challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], header(WINDOW, found));
            assert_eq!(
                verify_challenge(&c).unwrap_err(),
                AdmissionRefusal::NonCanonicalBitsPerItem { window_size: WINDOW, expected, found }
            );
        }
    }

    #[test]
    fn a_chain_commitment_that_does_not_decode_is_refused() {
        let mut c =
            challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], header(WINDOW, canonical_bits(32)));
        c.chain_commitment_bytes = vec![0xff; 3];
        assert!(matches!(
            verify_challenge(&c).unwrap_err(),
            AdmissionRefusal::UndecodableChainCommitment(_)
        ));
    }

    fn tile_step(index: u64) -> StepRecord {
        StepRecord {
            exec_index: index,
            sequence_id: "main".into(),
            coordinates: CfsCoordinates(vec![1]),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(format!("tile_{index}")),
                intra_sequence_index: 1,
                input_commitment: Vec::new(),
                input_source_commitment: Vec::new(),
                output_commitment: index.to_le_bytes().to_vec(),
                storage: StorageRoots {
                    root_before: Vec::new(),
                    root_after: Vec::new(),
                    index_root_before: Vec::new(),
                    index_root_after: Vec::new(),
                },
            }),
            recur_progress_commitment: [0; 32],
            recur_state: None,
        }
    }

    /// An honest `H_C` for a 10-item trace: protocol window 8.
    fn honest_body() -> TraceCommitment {
        let trace = Trace((0..10).map(tile_step).collect());
        let window = protocol_window_size(trace.len()).unwrap();
        let config = FraudProofConfig::from_window_size(window).unwrap();
        TraceCommitment::try_build(&trace, &EMPTY_LEAF, config).unwrap()
    }

    /// Admit a challenge naming `body`, as the challenger who built it posts
    /// it: under the header recomputed from the body.
    fn admit(body: &TraceCommitment) -> AdmittedChallenge {
        verify_challenge(&challenge(vec![stage(COMMITTED.to_vec())], vec![8; 32], body.header()))
            .expect("admission sees only the header")
    }

    fn body_bytes(body: &TraceCommitment) -> Vec<u8> {
        postcard::to_allocvec(body).unwrap()
    }

    /// What availability promises: the defender can walk the body.
    #[test]
    fn an_honest_body_is_available_and_can_be_walked() {
        let body = honest_body();
        let admitted = admit(&body);
        let available = admit_body(&body_bytes(&body), &admitted).expect("available");
        let cfs = ControlFlowSchema::new("test");
        assert!(TraceVerifier::new(available, &EMPTY_LEAF, &cfs).is_ok());
    }

    /// The header hashes the tail roots without counting them, so a body short
    /// one root is admitted under its recomputed header and is the commitment
    /// the challenge names, yet no defender can walk it.
    #[test]
    fn a_body_short_a_tail_root_is_not_available() {
        let mut body = honest_body();
        body.revealed_tail_roots.pop();
        let admitted = admit(&body);
        assert_eq!(body.header().digest(), admitted.trace_commitment);

        assert!(matches!(
            admit_body(&body_bytes(&body), &admitted),
            Err(BodyRefusal::Malformed(_))
        ));
        let cfs = ControlFlowSchema::new("test");
        assert!(TraceVerifier::new(body, &EMPTY_LEAF, &cfs).is_err());
    }

    #[test]
    fn a_body_with_a_wrong_block_count_is_not_available() {
        let mut body = honest_body();
        body.fingerprint.bits.push(0);
        let admitted = admit(&body);
        assert!(matches!(
            admit_body(&body_bytes(&body), &admitted),
            Err(BodyRefusal::Malformed(_))
        ));
    }

    #[test]
    fn a_body_whose_tail_contradicts_its_fingerprint_is_not_available() {
        let mut body = honest_body();
        body.revealed_tail_roots[0] = [0xff; 32].into();
        let admitted = admit(&body);
        assert!(matches!(
            admit_body(&body_bytes(&body), &admitted),
            Err(BodyRefusal::Malformed(_))
        ));
    }

    #[test]
    fn a_body_other_than_the_named_commitment_is_not_available() {
        let admitted = admit(&honest_body());
        let mut other = honest_body();
        other.revealed_items.pop();
        assert!(matches!(
            admit_body(&body_bytes(&other), &admitted),
            Err(BodyRefusal::NotTheNamedCommitment { .. })
        ));
    }

    #[test]
    fn bytes_that_are_not_a_trace_commitment_are_not_available() {
        let admitted = admit(&honest_body());
        assert!(matches!(
            admit_body(&[0xff; 3], &admitted),
            Err(BodyRefusal::Undecodable(_))
        ));
    }
}
