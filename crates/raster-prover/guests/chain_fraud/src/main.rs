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
use raster_core::input::scalar_leaf_root;
use raster_core::chain::{
    ChainCommitment, ChainFaultKind, ChainFraudEvidence, ChainFraudInput, ChainFraudJournal,
    InputBindingSource, StageCheckpoint,
};

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


/// A `Shape` fault: the chain records a trip count that the stage supposed to
/// have produced it did not commit — so the chain expanded to the wrong number
/// of stages.
///
/// Every value comes out of the `ChainCommitment` the guest already hashed;
/// the host supplies only `repeat_index`. Nothing is verified with `env::verify`
/// and no artifact is read, because a stage-produced count is that stage's
/// *whole* output: the check is to re-encode the count the chain claims and
/// compare one hash against what the stage committed. Re-encoding rather than
/// decoding is deliberate — adversarial payload bytes have no path in here, and
/// there is no decoder to diverge from the runtime's.
fn verify_shape_fault(chain: &ChainCommitment, faulty_stage: usize, repeat_index: u32) {
    let repeat = chain
        .shape
        .repeats
        .get(usize::try_from(repeat_index).expect("repeat_index overflows usize"))
        .expect("repeat_index is out of range for this chain");

    let source_stage = repeat
        .source_stage
        .expect("a literal or external count has nothing in the commitment to contradict");
    let source_stage = usize::try_from(source_stage).expect("source_stage overflows usize");

    // Blame lands on the stage that produced the count. Without this the host
    // could exhibit a real disagreement and attribute it to any stage it liked.
    assert!(
        source_stage == faulty_stage,
        "A shape fault blames the stage that produced the count, not another"
    );

    let producer = chain
        .stages
        .get(source_stage)
        .expect("the count's producing stage is out of range for this chain");
    let claimed = scalar_leaf_root(repeat.width, u64::from(repeat.resolved_count))
        .expect("the recorded count must fit its recorded width");

    // The inequality *is* the fault: honest execution would have made these
    // equal, so a chain exhibiting both a count and a producer that disagree
    // about it is fraudulent on its face.
    assert!(
        claimed.as_slice() != producer.output_structural_commitment.as_slice(),
        "Recorded count matches what the producing stage committed — no shape fault here"
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
        ChainFraudEvidence::Shape { repeat_index } => {
            verify_shape_fault(&chain, faulty_stage, *repeat_index);
            // Neither id is filled, and that is checked on the way out: a
            // `Shape` receipt verified no inner receipt, so an image id here
            // would be a claim about a recursion that never happened.
            (ChainFaultKind::Shape, Vec::new(), Vec::new())
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
