//! Checks that recorded step commitments (input, input source, external
//! inputs, output) match the provided witnesses, that external inputs are
//! covered by the verified authorization journal, and that tile steps carry a
//! verified replay proof whose output matches the recorded output witness.

use std::collections::BTreeMap;

use risc0_zkvm::guest::env;

use raster_core::authorization::AuthorizationJournal;
use raster_core::input::verify_selection_witness;
use raster_core::trace::{ExternalInput, FnInput, StepRecord};

use crate::merkle_tree::sha256_bytes;

pub fn external_input_commitment(external_input: &ExternalInput) -> Vec<u8> {
    let bytes = postcard::to_allocvec(external_input).unwrap_or_default();
    sha256_bytes(&bytes)
}

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

pub fn verify_external_inputs(
    step: &StepRecord,
    external_input: &ExternalInput,
    external_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
) {
    let computed_commitment = external_input_commitment(external_input);

    if let Some(external_commitment) = step.external_input_commitment() {
        assert_eq!(
            external_commitment, &computed_commitment,
            "Step external input commitment does not match authorized inputs",
        );
    } else {
        assert!(
            external_input.is_empty(),
            "SequenceEnd must not carry external input metadata",
        );
    }

    for (binding_name, meta) in external_input {
        let authorized_commitment =
            external_inputs_commitments
                .get(&meta.name)
                .unwrap_or_else(|| {
                    panic!(
                        "Missing authorized commitment for external input '{}'",
                        meta.name
                    )
                });
        assert_eq!(
            authorized_commitment, &meta.commitment,
            "External input '{}' commitment does not match authorized source",
            meta.name,
        );
        assert_eq!(
            meta.tree_root, meta.selection.source_root_hash,
            "External input '{}' tree root does not match selection commitment root",
            meta.name,
        );
        if !meta.selector.is_empty() || meta.selection.selected_len > 0 {
            let witness = external_selection_witnesses
                .get(binding_name.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "Missing external selection witness for binding '{}'",
                        binding_name
                    )
                });
            assert!(
                verify_selection_witness(&meta.selection, witness),
                "External input '{}' selection witness is invalid",
                meta.name,
            );
        }
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
    replay_image_id: Option<&Vec<u8>>,
    replay_journal: Option<&raster_core::draft::TileReplayJournal>,

    input_witness_bytes: Option<&Vec<u8>>,
    output_witness_bytes: Option<&Vec<u8>>,
    input_source_witness: Option<&FnInput>,

    external_inputs: &ExternalInput,
    external_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
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
    verify_external_inputs(
        step_record,
        external_inputs,
        external_selection_witnesses,
        external_inputs_commitments,
    );

    if step_record.requires_replay_proof() {
        let replay_image_id =
            replay_image_id.expect("replay image id should be provided for tile execution should ");
        let replay_journal =
            replay_journal.expect("tile execution should provide a replay journal witness");
        let replay_image_id_digest = risc0_zkvm::sha::Digest::try_from(replay_image_id.as_slice())
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
    }
}
