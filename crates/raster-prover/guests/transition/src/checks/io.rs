//! Checks that recorded step commitments (input, input source, output)
//! match the provided witnesses, and that tile steps carry a verified
//! replay proof whose output matches the recorded output witness.

use risc0_zkvm::guest::env;

use raster_core::authorization::AuthorizationJournal;
use raster_core::trace::{FnInput, StepRecord};

use crate::merkle_tree::sha256_bytes;

pub fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    sha256_bytes(&input.source_witness_bytes())
}

pub fn verify_io_witness(
    step_record: &StepRecord,
    input_witness: Option<&Vec<u8>>,
    output_witness: Option<&Vec<u8>>,
) {
    let commitment_for = |bytes: Option<&Vec<u8>>| -> Vec<u8> {
        bytes.map(|bytes| sha256_bytes(bytes)).unwrap_or_default()
    };

    if let Some(input_commitment) = step_record.input_commitment() {
        assert_eq!(
            input_commitment,
            &commitment_for(input_witness),
            "Step input commitment does not match recorded input bytes",
        );
    }
    if let Some(output_commitment) = step_record.output_commitment() {
        if step_record.is_execution_step() {
            return;
        }
        assert_eq!(
            output_commitment,
            &commitment_for(output_witness),
            "Step output commitment does not match recorded output bytes",
        );
    }
}

pub fn verify_authorization_journal(
    authorization_journal: &AuthorizationJournal,
    authorization_image_id: &[u8],
) -> bool {
    let image_id_digest = risc0_zkvm::sha::Digest::try_from(authorization_image_id)
        .expect("authorization image id must be 32 bytes");

    let journal_bytes = risc0_zkvm::serde::to_vec(authorization_journal)
        .expect("Failed to serialize authorization journal");

    env::verify(image_id_digest, &journal_bytes).is_ok()
}

pub fn verify_step_record(
    step_record: &StepRecord,
    expected_image_id: Option<&[u8; 32]>,
    replay_journal: Option<&raster_core::draft::TileReplayJournal>,

    input_witness_bytes: Option<&Vec<u8>>,
    output_witness_bytes: Option<&Vec<u8>>,
    input_source_witness: Option<&FnInput>,
) {
    verify_io_witness(step_record, input_witness_bytes, output_witness_bytes);
    if let Some(expected_input_source_commitment) = step_record.input_source_commitment() {
        let input_source_witness =
            input_source_witness.expect("Step input source witness is missing");
        assert_eq!(
            expected_input_source_commitment,
            &input_source_commitment(input_source_witness),
            "Step input source witness does not match recorded source commitment",
        );
    } else {
        assert!(
            input_source_witness.is_none(),
            "SequenceEnd must not carry input source witness",
        );
    }

    if step_record.requires_replay_proof() {
        // The expected image id comes from the program's committed tile
        // registry (resolved by the caller from the step's tile id), never
        // from a host-supplied field — this is what binds the replayed binary
        // to the tile the step claims to run. See program-identity.md.
        let expected_image_id =
            expected_image_id.expect("tile step must resolve a registry image id");
        let replay_journal =
            replay_journal.expect("tile execution should provide a replay journal witness");
        let replay_image_id_digest = risc0_zkvm::sha::Digest::try_from(expected_image_id.as_slice())
            .expect("image_id must be 32 bytes");
        let replay_journal_bytes = postcard::to_allocvec(replay_journal)
            .expect("Failed to encode replay journal for receipt verification");
        env::verify(replay_image_id_digest, &replay_journal_bytes)
            .expect("Failed to verify trace replay image id");
        let output_bytes = output_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            replay_journal.output_bytes.as_slice(),
            output_bytes,
            "Replay journal output bytes do not match recorded tile output witness",
        );
        // Bind the replay to the *recorded* input: the tile guest committed
        // `sha256(its input)`, which must equal the hash of the recorded input
        // witness. Without this the proof shows only that `output` is some
        // output of the binary, not that `binary(recorded input) = output`.
        let input_bytes = input_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            replay_journal.input_commitment.as_slice(),
            sha256_bytes(input_bytes).as_slice(),
            "Replay journal input commitment does not match recorded tile input witness",
        );
    }
}
