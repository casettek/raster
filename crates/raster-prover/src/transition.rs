//! RISC0 guest types and host utilities for iterative trace verification.
//!
//! This module provides:
//! - Shared types for guest input/output (TransitionInput, TransitionOutput)
//! - Host-side utilities for preparing inputs and verifying outputs
//! - The compiled transition guest ELF (when built)
//!
//! The types in this module are designed to be serialization-compatible with
//! the types used in the RISC0 guest program.

use raster_core::cfs::ControlFlowSchema;
use raster_core::fingerprint::Fingerprint;
use raster_core::trace::{ExternalInput, StepRecord};
use raster_core::transition::{
    AuthorizedExternalInputs, InitTransition, TransitionInput, TransitionJournal, TransitionState,
};
use std::collections::HashMap;

use crate::replay::ReplayResult;
use crate::trace::SerializableFrontier;
use crate::{TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID};

type RecordedStepIo = HashMap<StepRecord, (Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput)>;

fn lookup_recorded_io(
    step_record: &StepRecord,
    recorded_step_io: &RecordedStepIo,
) -> (Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput) {
    recorded_step_io
        .get(step_record)
        .cloned()
        .unwrap_or_else(|| panic!("Missing recorded I/O for transition step {:?}", step_record))
}

fn build_transition_input(
    step_record: &StepRecord,
    witness: &HashMap<StepRecord, Vec<u8>>,
    recorded_step_io: &RecordedStepIo,
    replayed_results: &HashMap<StepRecord, ReplayResult>,
    authorized_external_inputs: &AuthorizedExternalInputs,
) -> TransitionInput {
    let (recorded_input, recorded_output, external_input) =
        lookup_recorded_io(step_record, recorded_step_io);

    match step_record {
        StepRecord::TileExec(_) => {
            let Some(replay_result) = replayed_results.get(step_record) else {
                panic!(
                    "Replayed result not found for transition step {:?}",
                    step_record
                );
            };

            TransitionInput {
                step_record: step_record.clone(),
                replay_image_id: Some(replay_result.image_id.clone()),
                recorded_input,
                recorded_output,
                external_input,
                authorized_external_inputs: authorized_external_inputs.clone(),
                witness: witness.clone(),
            }
        }
        StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => TransitionInput {
            step_record: step_record.clone(),
            replay_image_id: None,
            recorded_input,
            recorded_output,
            external_input,
            authorized_external_inputs: authorized_external_inputs.clone(),
            witness: witness.clone(),
        },
    }
}

/// Replay trace transitions using the transition guest to prove merkle tree state transitions.
///
/// For each trace item in the window:
/// 1. Create a TransitionInput with the current frontier, trace item, and fingerprint data
/// 2. Execute the transition guest in the RISC0 zkVM
/// 3. Verify the output (including fingerprint) and update the frontier for the next iteration
///
/// # Arguments
/// * `initial_frontier` - The frontier state before the first trace item
/// * `trace_window` - The trace items to replay
/// * `fingerprint` - The packed fingerprint u64s for verification
/// * `window_start_position` - The starting position in the fingerprint for the first item
/// * `bits_per_item` - Bits per fingerprint item
///
/// # Returns
/// A `TransitionReplayResult` with details about success or failure
pub fn step_transitions(
    initial_frontier: &SerializableFrontier,
    trace_window: &[StepRecord],
    fingerprint: Fingerprint,
    cfs: &ControlFlowSchema,
    witness: &HashMap<StepRecord, Vec<u8>>,
    recorded_step_io: &RecordedStepIo,
    replayed_results: &HashMap<StepRecord, ReplayResult>,
    authorized_external_inputs: &AuthorizedExternalInputs,
) -> Option<risc0_zkvm::Receipt> {
    let prover = risc0_zkvm::default_prover();
    let cfs = cfs.clone();

    let self_image_id: Vec<u8> = TRANSITION_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect();

    let init_transition = InitTransition {
        init_frontier: initial_frontier.clone(),
        fingerprint,
    };

    let mut transition_receipt: Option<risc0_zkvm::Receipt> = None;
    let mut current_journal: Option<TransitionJournal> = None;

    let mut current_state = TransitionState::Init(init_transition);

    for step_record in trace_window {
        let input = build_transition_input(
            step_record,
            witness,
            recorded_step_io,
            replayed_results,
            authorized_external_inputs,
        );
        let replay_receipt_assumption: Option<risc0_zkvm::Receipt> = match step_record {
            StepRecord::TileExec(_) => {
                let replay_result = replayed_results.get(step_record).unwrap_or_else(|| {
                    panic!(
                        "Replayed receipt not found for transition step {:?}",
                        step_record
                    )
                });
                let receipt: risc0_zkvm::Receipt =
                    postcard::from_bytes(&replay_result.receipt).unwrap();
                Some(receipt)
            }
            StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => None,
        };

        let mut builder = risc0_zkvm::ExecutorEnv::builder();
        if let Some(replay_receipt) = replay_receipt_assumption {
            builder.add_assumption(replay_receipt);
        }
        if current_journal.is_some() {
            let transition_receipt = transition_receipt.unwrap_or_else(|| {
                panic!("Previous transition receipt is required when previous journal is present")
            });
            builder.add_assumption(transition_receipt);
        }
        builder.write(&cfs).unwrap();
        builder.write(&self_image_id).unwrap();
        builder.write(&input).unwrap();
        builder.write(&current_state).unwrap();
        if let Some(previous_journal) = current_journal {
            builder.write(&previous_journal).unwrap();
        }
        let env = builder.build().unwrap();
        let receipt = prover.prove(env, &TRANSITION_GUEST_ELF).unwrap().receipt;
        let journal: TransitionJournal = receipt.journal.decode().unwrap();

        current_state = journal.current_state.clone();
        current_journal = Some(journal);

        transition_receipt = Some(receipt);
    }

    transition_receipt
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::CfsCoordinates;
    use raster_core::trace::{
        ExternalBindingMeta, SequenceEndRecord, SequenceStartRecord, TileExecRecord,
    };
    use sha2::{Digest, Sha256};

    fn external_input_commitment(external_input: &ExternalInput) -> Vec<u8> {
        let bytes = raster_core::postcard::to_allocvec(external_input).unwrap_or_default();
        Sha256::digest(bytes).to_vec()
    }

    fn make_external_input(binding_name: &str, commitment: &[u8]) -> ExternalInput {
        HashMap::from([(
            "arg".to_string(),
            ExternalBindingMeta {
                name: binding_name.to_string(),
                data_commitment: commitment.to_vec(),
            },
        )])
        .into_iter()
        .collect()
    }

    fn make_tile_step(exec_index: u64, coordinates: Vec<u32>) -> StepRecord {
        StepRecord::TileExec(TileExecRecord {
            exec_index,
            tile_id: "shared_tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(coordinates),
            intra_sequence_index: 0,
            input_commitment: vec![exec_index as u8],
            external_input_commitment: Vec::new(),
            output_commitment: vec![exec_index as u8 + 1],
        })
    }

    #[test]
    fn build_transition_input_uses_step_record_key_for_repeated_tiles() {
        let first_step = make_tile_step(1, vec![0]);
        let second_step = make_tile_step(2, vec![1]);

        let recorded_step_io = HashMap::from([
            (
                first_step.clone(),
                (Some(vec![1]), Some(vec![11]), ExternalInput::new()),
            ),
            (
                second_step.clone(),
                (
                    Some(vec![2]),
                    Some(vec![22]),
                    make_external_input("personal_data", b"hash-2"),
                ),
            ),
        ]);
        let replayed_results = HashMap::from([
            (
                first_step.clone(),
                ReplayResult {
                    fn_name: "shared_tile".to_string(),
                    receipt: vec![],
                    image_id: vec![9; 32],
                    input: vec![1],
                    output: vec![11],
                },
            ),
            (
                second_step.clone(),
                ReplayResult {
                    fn_name: "shared_tile".to_string(),
                    receipt: vec![],
                    image_id: vec![7; 32],
                    input: vec![2],
                    output: vec![22],
                },
            ),
        ]);

        let input = build_transition_input(
            &second_step,
            &HashMap::new(),
            &recorded_step_io,
            &replayed_results,
            &AuthorizedExternalInputs {
                commitments: [("personal_data".to_string(), b"hash-2".to_vec())]
                    .into_iter()
                    .collect(),
            },
        );

        assert_eq!(input.replay_image_id, Some(vec![7; 32]));
        assert_eq!(input.recorded_input, Some(vec![2]));
        assert_eq!(input.recorded_output, Some(vec![22]));
        assert_eq!(
            input.external_input,
            make_external_input("personal_data", b"hash-2")
        );
    }

    #[test]
    fn build_transition_input_preserves_recorded_io_for_sequence_steps() {
        let sequence_start = StepRecord::SequenceStart(SequenceStartRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            input_commitment: vec![1; 32],
            external_input_commitment: external_input_commitment(&ExternalInput::new()),
        });
        let sequence_end = StepRecord::SequenceEnd(SequenceEndRecord {
            exec_index: 2,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            output_commitment: vec![2; 32],
        });
        let recorded_step_io = HashMap::from([
            (
                sequence_start.clone(),
                (Some(vec![3, 4]), None, ExternalInput::new()),
            ),
            (
                sequence_end.clone(),
                (None, Some(vec![5, 6]), ExternalInput::new()),
            ),
        ]);

        let start_input = build_transition_input(
            &sequence_start,
            &HashMap::new(),
            &recorded_step_io,
            &HashMap::new(),
            &AuthorizedExternalInputs::default(),
        );
        let end_input = build_transition_input(
            &sequence_end,
            &HashMap::new(),
            &recorded_step_io,
            &HashMap::new(),
            &AuthorizedExternalInputs::default(),
        );

        assert_eq!(start_input.replay_image_id, None);
        assert_eq!(start_input.recorded_input, Some(vec![3, 4]));
        assert_eq!(start_input.recorded_output, None);
        assert!(start_input.external_input.is_empty());

        assert_eq!(end_input.replay_image_id, None);
        assert_eq!(end_input.recorded_input, None);
        assert_eq!(end_input.recorded_output, Some(vec![5, 6]));
        assert!(end_input.external_input.is_empty());
    }
}
