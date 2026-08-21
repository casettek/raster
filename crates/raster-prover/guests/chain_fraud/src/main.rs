//! RISC0 guest turning a single-stage fault into a whole-chain fraud proof.
//!
//! One execution verifies one piece of stage-level evidence and binds it to
//! one checkpoint of a `ChainCommitment`, committing a `ChainFraudJournal`:
//! *"chain D is fraudulent; stage i (program P_i) is to blame."* The guest
//! never trusts a host claim — the chain digest is recomputed from the exact
//! bytes the checkpoints are read from, the evidence receipts are verified
//! with `env::verify`, and attribution is by equality against the named
//! checkpoint. See `docs/proposals/chain-fraud-proof.md`.
//!
//! Trust in the inner guests is threaded, never assumed: the image ids used
//! for `env::verify` are committed to the journal, and the relying party
//! checks them (plus this guest's own image id, pinned out-of-band) against
//! known-good values.

use risc0_zkvm::guest::env;
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};

use raster_core::authorization::AuthorizationJournal;
use raster_core::chain::{
    ChainCommitment, ChainFaultKind, ChainFraudEvidence, ChainFraudInput, ChainFraudJournal,
    InputBindingSource, StageCheckpoint,
};
use raster_core::transition::{TransitionJournal, TransitionState};

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Risc0Sha256::hash_bytes(bytes).as_bytes().try_into().unwrap()
}

/// Lowercase hex, as ASCII bytes — the normalization the authorization guest
/// applies to manifest commitments, so the two sides compare byte-for-byte.
fn hex_lower(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let hi = (byte >> 4) & 0x0f;
        let lo = byte & 0x0f;
        out.push(if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) });
        out.push(if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) });
    }
    out
}

/// `env::verify` a journal committed by a guest with the given image id.
fn verify_inner_receipt<J: serde::Serialize>(image_id: &[u8], journal: &J) {
    let digest =
        risc0_zkvm::sha::Digest::try_from(image_id).expect("image id must be 32 bytes");
    env::verify(digest, &risc0_zkvm::serde::to_vec(journal).unwrap())
        .expect("Failed to verify inner receipt");
}

/// An `Execution` fault: a verified, `Finished` stage fraud receipt whose
/// attribution triple matches the checkpoint — it refutes exactly this
/// stage's program, authorized inputs, and committed trace.
fn verify_execution_fault(
    stage: &StageCheckpoint,
    fraud_journal: &TransitionJournal,
    transition_image_id: &[u8],
) {
    verify_inner_receipt(transition_image_id, fraud_journal);
    // The transition guest holds its image id constant across the receipt's
    // whole recursion, so this equality extends the verification to every
    // inner step, and the id we commit is the one that recursion ran under.
    assert!(
        fraud_journal.transition_image_id == transition_image_id,
        "Fraud journal names a different transition guest than it was verified with"
    );
    assert!(
        matches!(fraud_journal.current_state, TransitionState::Finished),
        "Stage receipt is an honest window, not a fraud proof"
    );
    assert!(
        fraud_journal.program_commitment == stage.program_commitment,
        "Fraud receipt refutes a different program than the checkpoint's"
    );
    assert!(
        fraud_journal.input_manifest_commitment == stage.input_manifest_commitment,
        "Fraud receipt was authorized against a different input manifest"
    );
    assert!(
        fraud_journal.refuted_trace_commitment == stage.trace_commitment_digest,
        "Fraud receipt refutes a different trace commitment than the checkpoint's"
    );
}

/// A `Link` fault: the checkpoint's own committed manifest feeds a `from`
/// parameter a value different from the producing stage's committed output
/// root — an inconsistency inside the `ChainCommitment`, exhibited without
/// parsing JSON in-guest by verifying the authorization guest's journal for
/// the same manifest commitment.
fn verify_link_fault(
    chain: &ChainCommitment,
    faulty_stage: usize,
    parameter: &str,
    authorization_journal: &AuthorizationJournal,
    authorization_image_id: &[u8],
) {
    let stage = &chain.stages[faulty_stage];
    let producer_index = match stage.input_bindings.get(parameter) {
        Some(InputBindingSource::Chained { stage: producer }) => *producer,
        _ => panic!("Parameter '{parameter}' is not a chained input of the faulty stage"),
    };
    assert!(
        producer_index < faulty_stage,
        "Chained producer does not run earlier than the consumer"
    );

    verify_inner_receipt(authorization_image_id, authorization_journal);
    assert!(
        authorization_journal.input_manifest_commitment == stage.input_manifest_commitment,
        "Authorization journal is for a different manifest than the checkpoint committed"
    );

    let committed_input = authorization_journal
        .external_inputs_commitments
        .get(parameter)
        .expect("Parameter is absent from the authorized manifest");

    let producer_output = &chain.stages[producer_index].output_structural_commitment;
    assert!(
        !producer_output.is_empty(),
        "Chained producer committed no output"
    );
    assert!(
        *committed_input != hex_lower(producer_output),
        "Link is consistent — the committed input equals the producer's output root"
    );
}

fn main() {
    let input: ChainFraudInput = env::read();

    // The digest names the chain; hashing the exact bytes the checkpoints
    // are decoded from binds attribution to that same chain.
    let chain_commitment_digest = sha256_bytes(&input.chain_commitment_bytes);
    let chain: ChainCommitment = postcard::from_bytes(&input.chain_commitment_bytes)
        .expect("host must supply a valid ChainCommitment");

    let faulty_stage = usize::try_from(input.faulty_stage).expect("faulty_stage overflows usize");
    let stage = chain
        .stages
        .get(faulty_stage)
        .expect("faulty_stage is out of range for this chain");

    let (fault, transition_image_id, authorization_image_id) = match &input.evidence {
        ChainFraudEvidence::Execution {
            fraud_journal,
            transition_image_id,
        } => {
            verify_execution_fault(stage, fraud_journal, transition_image_id);
            (ChainFaultKind::Execution, transition_image_id.clone(), Vec::new())
        }
        ChainFraudEvidence::Link {
            parameter,
            authorization_journal,
            authorization_image_id,
        } => {
            verify_link_fault(
                &chain,
                faulty_stage,
                parameter,
                authorization_journal,
                authorization_image_id,
            );
            (ChainFaultKind::Link, Vec::new(), authorization_image_id.clone())
        }
    };

    env::commit(&ChainFraudJournal {
        chain_commitment_digest,
        faulty_stage: input.faulty_stage,
        stage_program_commitment: stage.program_commitment.clone(),
        fault,
        transition_image_id,
        authorization_image_id,
    });
}
