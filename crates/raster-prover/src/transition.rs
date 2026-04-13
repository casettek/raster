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
    AuthorizationAssumption, AuthorizationInput, AuthorizationJournal, InitTransition,
    TransitionInput, TransitionJournal, TransitionState,
};
use std::collections::HashMap;

use crate::replay::ReplayResult;
use crate::trace::SerializableFrontier;
use crate::{
    AUTHORIZATION_GUEST_ELF, AUTHORIZATION_GUEST_ID, TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID,
};

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
    authorization: &AuthorizationAssumption,
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
                authorization: authorization.clone(),
                witness: witness.clone(),
            }
        }
        StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => TransitionInput {
            step_record: step_record.clone(),
            replay_image_id: None,
            recorded_input,
            recorded_output,
            external_input,
            authorization: authorization.clone(),
            witness: witness.clone(),
        },
    }
}

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

fn prove_authorization(
    authorization_input: &AuthorizationInput,
) -> (risc0_zkvm::Receipt, AuthorizationAssumption) {
    let prover = risc0_zkvm::default_prover();
    let mut builder = risc0_zkvm::ExecutorEnv::builder();
    builder.write(authorization_input).unwrap();
    let env = builder.build().unwrap();
    let receipt = prover.prove(env, &AUTHORIZATION_GUEST_ELF).unwrap().receipt;
    let journal: AuthorizationJournal = receipt.journal.decode().unwrap();

    (
        receipt,
        AuthorizationAssumption {
            image_id: image_id_bytes(AUTHORIZATION_GUEST_ID),
            journal,
        },
    )
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
    authorization_input: &AuthorizationInput,
) -> Option<risc0_zkvm::Receipt> {
    let prover = risc0_zkvm::default_prover();
    let cfs = cfs.clone();
    let (authorization_receipt, authorization) = prove_authorization(authorization_input);

    let self_image_id = image_id_bytes(TRANSITION_GUEST_ID);

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
            &authorization,
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
        builder.add_assumption(authorization_receipt.clone());
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
    use raster_core::cfs::{CfsCoordinates, ControlFlowSchema, SequenceDef};
    use raster_core::fingerprint::{BitPacker, Fingerprint};
    use raster_core::trace::{
        ExternalBindingMeta, SequenceEndRecord, SequenceStartRecord, TileExecRecord,
    };
    use raster_core::transition::AuthorizationJournal;
    use sha2::{Digest, Sha256};

    fn external_input_commitment(external_input: &ExternalInput) -> Vec<u8> {
        let bytes = raster_core::postcard::to_allocvec(external_input).unwrap_or_default();
        Sha256::digest(bytes).to_vec()
    }

    fn make_external_input(binding_name: &str, commitment: &[u8], payload_bytes: &[u8]) -> ExternalInput {
        HashMap::from([(
            "arg".to_string(),
            ExternalBindingMeta {
                name: binding_name.to_string(),
                data_commitment: commitment.to_vec(),
                payload_bytes: payload_bytes.to_vec(),
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

    fn make_authorization_input() -> AuthorizationInput {
        AuthorizationInput {
            manifest_bytes: br#"{"personal_data":"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"}"#
                .to_vec(),
            payload_witnesses: [("personal_data".to_string(), b"payload".to_vec())]
                .into_iter()
                .collect(),
        }
    }

    fn make_authorization_assumption() -> AuthorizationAssumption {
        AuthorizationAssumption {
            image_id: vec![3; 32],
            journal: AuthorizationJournal {
                authorized_external_inputs: raster_core::transition::AuthorizedExternalInputs {
                    commitments: [(
                        "personal_data".to_string(),
                        b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_vec()
                    )]
                        .into_iter()
                        .collect(),
                },
                authorized_payloads: [("personal_data".to_string(), b"payload".to_vec())]
                    .into_iter()
                    .collect(),
                manifest_commitment: vec![4; 32],
            },
        }
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
                    make_external_input(
                        "personal_data",
                        b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5",
                        b"payload",
                    ),
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
            &make_authorization_assumption(),
        );

        assert_eq!(input.replay_image_id, Some(vec![7; 32]));
        assert_eq!(input.recorded_input, Some(vec![2]));
        assert_eq!(input.recorded_output, Some(vec![22]));
        assert_eq!(
            input.external_input,
            make_external_input(
                "personal_data",
                b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5",
                b"payload",
            )
        );
        assert_eq!(input.authorization, make_authorization_assumption());
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
            &make_authorization_assumption(),
        );
        let end_input = build_transition_input(
            &sequence_end,
            &HashMap::new(),
            &recorded_step_io,
            &HashMap::new(),
            &make_authorization_assumption(),
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

    #[test]
    fn prove_authorization_returns_expected_journal() {
        let (_receipt, authorization) = prove_authorization(&make_authorization_input());

        assert_eq!(
            authorization
                .journal
                .authorized_external_inputs
                .commitments
                .get("personal_data"),
            Some(
                &b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_vec()
            )
        );
        assert_eq!(
            authorization.journal.authorized_payloads.get("personal_data"),
            Some(&b"payload".to_vec())
        );
        assert_eq!(authorization.image_id, image_id_bytes(AUTHORIZATION_GUEST_ID));
    }

    fn make_init_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: crate::precomputed::EMPTY_TRIE_NODES[0].to_vec(),
            ommers: Vec::new(),
        }
    }

    fn make_minimal_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.sequences.push(SequenceDef::new("main"));
        cfs
    }

    fn make_sequence_start_step() -> StepRecord {
        let external_input = ExternalInput::new();
        StepRecord::SequenceStart(SequenceStartRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            input_commitment: Sha256::digest(b"sequence-in").to_vec(),
            external_input_commitment: external_input_commitment(&external_input),
        })
    }

    fn prove_single_transition_with_authorization(
        authorization: AuthorizationAssumption,
        authorization_receipt: Option<risc0_zkvm::Receipt>,
    ) -> risc0_zkvm::Result<risc0_zkvm::ProveInfo> {
        let prover = risc0_zkvm::default_prover();
        let input = TransitionInput {
            step_record: make_sequence_start_step(),
            replay_image_id: None,
            recorded_input: Some(b"sequence-in".to_vec()),
            recorded_output: None,
            external_input: ExternalInput::new(),
            authorization,
            witness: HashMap::new(),
        };
        let state = TransitionState::Init(InitTransition {
            init_frontier: make_init_frontier(),
            fingerprint: Fingerprint::from(vec![0], BitPacker::new(64), 1),
        });

        let mut builder = risc0_zkvm::ExecutorEnv::builder();
        if let Some(receipt) = authorization_receipt {
            builder.add_assumption(receipt);
        }
        builder.write(&make_minimal_cfs()).unwrap();
        builder.write(&image_id_bytes(TRANSITION_GUEST_ID)).unwrap();
        builder.write(&input).unwrap();
        builder.write(&state).unwrap();
        let env = builder.build().unwrap();

        prover.prove(env, &TRANSITION_GUEST_ELF)
    }

    #[test]
    fn transition_guest_accepts_valid_authorization_receipt_assumption() {
        let (authorization_receipt, authorization) = prove_authorization(&AuthorizationInput {
            manifest_bytes: Vec::new(),
            payload_witnesses: std::collections::BTreeMap::new(),
        });

        assert!(
            prove_single_transition_with_authorization(authorization, Some(authorization_receipt))
                .is_ok()
        );
    }

    #[test]
    fn transition_guest_rejects_missing_authorization_receipt_assumption() {
        let (_authorization_receipt, authorization) = prove_authorization(&AuthorizationInput {
            manifest_bytes: Vec::new(),
            payload_witnesses: std::collections::BTreeMap::new(),
        });

        assert!(prove_single_transition_with_authorization(authorization, None).is_err());
    }

    #[test]
    fn transition_guest_rejects_tampered_authorization_journal() {
        let (authorization_receipt, mut authorization) = prove_authorization(&AuthorizationInput {
            manifest_bytes: Vec::new(),
            payload_witnesses: std::collections::BTreeMap::new(),
        });
        authorization.journal.manifest_commitment = vec![9; 32];

        assert!(
            prove_single_transition_with_authorization(authorization, Some(authorization_receipt))
                .is_err()
        );
    }
}
