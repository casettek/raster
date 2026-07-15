//! RISC0 guest types and host utilities for iterative trace verification.
//!
//! This module provides:
//! - Shared types for guest input/output (TransitionInput, TransitionOutput)
//! - Host-side utilities for preparing inputs and verifying outputs
//! - The compiled transition guest ELF (when built)
//!
//! The types in this module are designed to be serialization-compatible with
//! the types used in the RISC0 guest program.

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::ControlFlowSchema;
use raster_core::draft::DraftTransitionWitness;
use raster_core::fingerprint::Fingerprint;
use raster_core::input::SelectionWitness;
use raster_core::trace::{FnInput, StepRecord};
use raster_core::transition::{
    InitTransition, StorageReadWitness, StorageWitness, TransitionInput, TransitionJournal,
    TransitionState,
};
use std::collections::{BTreeMap, HashMap};

use crate::authorization::authorization_guest_image_id;
use crate::precomputed::EMPTY_TRIE_NODES;
use crate::replay::ReplayResult;
use crate::trace::{serializable_frontier_into_trace_frontier, SerializableFrontier, TraceTree};
use crate::{TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID};

/// Everything the host recorded about one executed step, to be handed to the
/// guest as that step's witnesses.
#[derive(Debug, Clone, Default)]
pub struct StepIo {
    pub input_witness: Option<Vec<u8>>,
    pub output_witness: Option<Vec<u8>>,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<StorageWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,
}

type RecordedStepIo = HashMap<StepRecord, StepIo>;

fn build_transition_input(
    step_record: &StepRecord,
    input_sources_witnesses: &HashMap<StepRecord, Vec<u8>>,
    recorded_step_io: &RecordedStepIo,
    replayed_results: &HashMap<StepRecord, ReplayResult>,
    authorization_journal: &AuthorizationJournal,
    entrypoint_membership_witness: Option<&StorageReadWitness>,
) -> TransitionInput {
    let StepIo {
        input_witness,
        output_witness,
        input_source_witness,
        sequence_scope_witness,
        storage_selection_witnesses,
        storage_witness,
        draft_transition_witness,
    } = recorded_step_io
        .get(step_record)
        .cloned()
        .unwrap_or_else(|| panic!("Missing recorded I/O for transition step {:?}", step_record));

    let (replay_image_id, replay_journal) = if step_record.requires_replay_proof() {
        let replay_result = replayed_results.get(step_record).unwrap_or_else(|| {
            panic!(
                "Replayed result not found for transition step {:?}",
                step_record
            )
        });
        (
            Some(replay_result.image_id.clone()),
            Some(replay_result.replay_journal.clone()),
        )
    } else {
        (None, None)
    };

    TransitionInput {
        step_record: step_record.clone(),
        authorization_image_id: authorization_guest_image_id(),
        replay_image_id,
        replay_journal,
        input_witness,
        output_witness,
        input_source_witness,
        sequence_scope_witness,
        storage_selection_witnesses,
        storage_witness,
        draft_transition_witness,
        authorization_journal: authorization_journal.clone(),
        input_sources_witnesses: input_sources_witnesses.clone(),
        entrypoint_membership_witness: entrypoint_membership_witness.cloned(),
    }
}

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

fn empty_storage_frontier() -> SerializableFrontier {
    SerializableFrontier {
        position: 0,
        leaf: EMPTY_TRIE_NODES[0].to_vec(),
        ommers: Vec::new(),
    }
}

fn storage_root(frontier: &SerializableFrontier) -> Vec<u8> {
    let frontier = serializable_frontier_into_trace_frontier(frontier.clone())
        .expect("storage frontier should deserialize");
    TraceTree::from_frontier(1, frontier)
        .root(0)
        .expect("storage root should exist")
        .0
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
/// * `entrypoint_membership_witness` - Proof that `main`'s entry-argument
///   binding already exists at coordinate `[0]` of the window's initial
///   storage. Supply it whenever the window opens *after* the binding
///   step; pass `None` when the window contains the binding step itself, in
///   which case that step discharges the authorization instead. See
///   `checks::entrypoint` in the transition guest.
///
/// # Returns
/// A `TransitionReplayResult` with details about success or failure
pub fn step_transitions(
    initial_frontier: &SerializableFrontier,
    initial_storage_frontier: &SerializableFrontier,
    initial_storage_index_root: &[u8],
    trace_window: &[StepRecord],
    fingerprint: Fingerprint,
    cfs: &ControlFlowSchema,
    input_sources_witnesses: &HashMap<StepRecord, Vec<u8>>,
    recorded_step_io: &RecordedStepIo,
    replayed_results: &HashMap<StepRecord, ReplayResult>,
    authorization_journal: &AuthorizationJournal,
    authorization_receipt: &risc0_zkvm::Receipt,
    entrypoint_membership_witness: Option<&StorageReadWitness>,
) -> Option<risc0_zkvm::Receipt> {
    let prover = risc0_zkvm::default_prover();

    let transition_image_id = image_id_bytes(TRANSITION_GUEST_ID);

    let init_transition = InitTransition {
        init_frontier: initial_frontier.clone(),
        init_storage_frontier: initial_storage_frontier.clone(),
        init_storage_root: storage_root(initial_storage_frontier),
        init_storage_index_root: initial_storage_index_root.to_vec(),
        active_drafts: Default::default(),
        fingerprint,
    };

    let mut transition_receipt: Option<risc0_zkvm::Receipt> = None;
    let mut current_journal: Option<TransitionJournal> = None;

    let mut current_state = TransitionState::Init(init_transition);

    for step_record in trace_window {
        let input = build_transition_input(
            step_record,
            input_sources_witnesses,
            recorded_step_io,
            replayed_results,
            authorization_journal,
            // Only the `Init` step proves anything about the window's
            // *initial* store state; later steps inherit the established
            // fact from the previous journal and never read this.
            current_journal
                .is_none()
                .then_some(entrypoint_membership_witness)
                .flatten(),
        );
        let replay_receipt_assumption: Option<risc0_zkvm::Receipt> =
            if step_record.requires_replay_proof() {
                let replay_result = replayed_results.get(step_record).unwrap_or_else(|| {
                    panic!(
                        "Replayed receipt not found for transition step {:?}",
                        step_record
                    )
                });
                let receipt: risc0_zkvm::Receipt =
                    postcard::from_bytes(&replay_result.receipt).unwrap();
                Some(receipt)
            } else {
                None
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
        builder.write(&transition_image_id).unwrap();
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
    use crate::authorization::authorize_external_inputs;
    use crate::precomputed::EMPTY_TRIE_NODES;
    use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};
    use raster_core::cfs::{CfsCoordinates, ControlFlowSchema, SequenceDef};
    use raster_core::coordinate_index::coordinate_index_root;
    use raster_core::draft::TileReplayJournal;
    use raster_core::fingerprint::{BitPacker, Fingerprint};
    use raster_core::trace::{ExecStep, ExecTarget, FnInput, StepKind, StorageRoots};
    use sha2::{Digest, Sha256};

    fn empty_store_roots() -> StorageRoots {
        StorageRoots {
            root_before: EMPTY_TRIE_NODES[0].to_vec(),
            root_after: EMPTY_TRIE_NODES[0].to_vec(),
            index_root_before: Vec::new(),
            index_root_after: Vec::new(),
        }
    }

    /// A step whose only recorded witnesses are its input/output bytes.
    fn io_witnesses(input_witness: Option<Vec<u8>>, output_witness: Option<Vec<u8>>) -> StepIo {
        StepIo {
            input_witness,
            output_witness,
            ..StepIo::default()
        }
    }

    fn make_tile_step(exec_index: u64, coordinates: Vec<u32>) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("shared_tile".to_string()),
                intra_sequence_index: 0,
                input_commitment: vec![exec_index as u8],
                input_source_commitment: Vec::new(),
                output_commitment: vec![exec_index as u8 + 1],
                storage: empty_store_roots(),
            }),
        }
    }

    fn make_manifested_inputs() -> ManifestedInputs {
        ManifestedInputs {
            manifest_bytes: br#"{"personal_data":{"type":"sha256","commitment":"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"}}"#
                .to_vec(),
        }
    }

    fn make_authorization_journal() -> AuthorizationJournal {
        AuthorizationJournal {
            external_inputs_commitments: [(
                "personal_data".to_string(),
                b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_vec(),
            )]
            .into_iter()
            .collect(),
            manifest_commitment: vec![4; 32],
        }
    }

    fn empty_input_source_witness() -> FnInput {
        FnInput {
            data: Vec::new(),
            values: Vec::new(),
            args: Vec::new(),
            storage: Default::default(),
        }
    }

    #[test]
    fn build_transition_input_uses_step_record_key_for_repeated_tiles() {
        let first_step = make_tile_step(1, vec![0]);
        let second_step = make_tile_step(2, vec![1]);

        let recorded_step_io = HashMap::from([
            (
                first_step.clone(),
                io_witnesses(Some(vec![1]), Some(vec![11])),
            ),
            (
                second_step.clone(),
                io_witnesses(Some(vec![2]), Some(vec![22])),
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
                    replay_journal: TileReplayJournal {
                        output_bytes: vec![11],
                        draft_transition: None,
                    },
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
                    replay_journal: TileReplayJournal {
                        output_bytes: vec![22],
                        draft_transition: None,
                    },
                },
            ),
        ]);

        let input = build_transition_input(
            &second_step,
            &HashMap::new(),
            &recorded_step_io,
            &replayed_results,
            &make_authorization_journal(),
            None,
        );

        assert_eq!(input.replay_image_id, Some(vec![7; 32]));
        assert_eq!(input.input_witness, Some(vec![2]));
        assert_eq!(input.output_witness, Some(vec![22]));
        assert_eq!(input.authorization_journal, make_authorization_journal());
    }

    #[test]
    fn build_transition_input_preserves_error_output_bytes_for_tile_steps() {
        let tile_step = make_tile_step(1, vec![0]);
        let error_output =
            raster_core::postcard::to_allocvec(&Err::<u64, String>("denied".to_string())).unwrap();
        let recorded_step_io = HashMap::from([(
            tile_step.clone(),
            io_witnesses(Some(vec![9]), Some(error_output.clone())),
        )]);
        let replayed_results = HashMap::from([(
            tile_step.clone(),
            ReplayResult {
                fn_name: "shared_tile".to_string(),
                receipt: vec![],
                image_id: vec![5; 32],
                input: vec![9],
                output: error_output.clone(),
                replay_journal: TileReplayJournal {
                    output_bytes: error_output.clone(),
                    draft_transition: None,
                },
            },
        )]);

        let input = build_transition_input(
            &tile_step,
            &HashMap::new(),
            &recorded_step_io,
            &replayed_results,
            &make_authorization_journal(),
            None,
        );

        assert_eq!(input.replay_image_id, Some(vec![5; 32]));
        assert_eq!(input.input_witness, Some(vec![9]));
        assert_eq!(input.output_witness, Some(error_output));
    }

    #[test]
    fn build_transition_input_preserves_recorded_io_for_sequence_steps() {
        let sequence_start = StepRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::SequenceStart {
                input_commitment: vec![1; 32],
                input_source_commitment: Vec::new(),
            },
        };
        let sequence_end = StepRecord {
            exec_index: 2,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::SequenceEnd {
                output_commitment: vec![2; 32],
            },
        };
        let recorded_step_io = HashMap::from([
            (sequence_start.clone(), io_witnesses(Some(vec![3, 4]), None)),
            (sequence_end.clone(), io_witnesses(None, Some(vec![5, 6]))),
        ]);

        let start_input = build_transition_input(
            &sequence_start,
            &HashMap::new(),
            &recorded_step_io,
            &HashMap::new(),
            &make_authorization_journal(),
            None,
        );
        let end_input = build_transition_input(
            &sequence_end,
            &HashMap::new(),
            &recorded_step_io,
            &HashMap::new(),
            &make_authorization_journal(),
            None,
        );

        assert_eq!(start_input.replay_image_id, None);
        assert_eq!(start_input.input_witness, Some(vec![3, 4]));
        assert_eq!(start_input.output_witness, None);

        assert_eq!(end_input.replay_image_id, None);
        assert_eq!(end_input.input_witness, None);
        assert_eq!(end_input.output_witness, Some(vec![5, 6]));
    }

    #[test]
    fn authorize_external_inputs_returns_expected_journal() {
        let (_receipt, authorization) = authorize_external_inputs(&make_manifested_inputs());

        assert_eq!(
            authorization
                .external_inputs_commitments
                .get("personal_data"),
            Some(&b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_vec())
        );
        assert_eq!(
            authorization_guest_image_id(),
            image_id_bytes(crate::AUTHORIZATION_GUEST_ID)
        );
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
        StepRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::SequenceStart {
                input_commitment: Sha256::digest(b"sequence-in").to_vec(),
                input_source_commitment: Sha256::digest(
                    empty_input_source_witness().source_witness_bytes(),
                )
                .to_vec(),
            },
        }
    }

    fn prove_single_transition_with_authorization(
        authorization: AuthorizationJournal,
        authorization_receipt: Option<risc0_zkvm::Receipt>,
    ) -> risc0_zkvm::Result<risc0_zkvm::ProveInfo> {
        let prover = risc0_zkvm::default_prover();
        let input = TransitionInput {
            step_record: make_sequence_start_step(),
            authorization_image_id: authorization_guest_image_id(),
            replay_image_id: None,
            replay_journal: None,
            input_witness: Some(b"sequence-in".to_vec()),
            output_witness: None,
            input_source_witness: Some(empty_input_source_witness()),
            sequence_scope_witness: None,
            storage_selection_witnesses: BTreeMap::new(),
            storage_witness: None,
            draft_transition_witness: None,
            authorization_journal: authorization,
            input_sources_witnesses: HashMap::new(),
            entrypoint_membership_witness: None,
        };
        let state = TransitionState::Init(InitTransition {
            init_frontier: make_init_frontier(),
            init_storage_frontier: make_init_frontier(),
            init_storage_root: storage_root(&make_init_frontier()),
            init_storage_index_root: coordinate_index_root(&std::collections::BTreeMap::new()),
            active_drafts: Default::default(),
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
        let (authorization_receipt, authorization) = authorize_external_inputs(&ManifestedInputs {
            manifest_bytes: Vec::new(),
        });

        assert!(prove_single_transition_with_authorization(
            authorization,
            Some(authorization_receipt)
        )
        .is_ok());
    }

    #[test]
    fn transition_guest_rejects_missing_authorization_receipt_assumption() {
        let (_authorization_receipt, authorization) =
            authorize_external_inputs(&ManifestedInputs {
                manifest_bytes: Vec::new(),
            });

        assert!(prove_single_transition_with_authorization(authorization, None).is_err());
    }

    #[test]
    fn transition_guest_rejects_tampered_authorization_journal() {
        let (authorization_receipt, mut authorization) =
            authorize_external_inputs(&ManifestedInputs {
                manifest_bytes: Vec::new(),
            });
        authorization.manifest_commitment = vec![9; 32];

        assert!(prove_single_transition_with_authorization(
            authorization,
            Some(authorization_receipt)
        )
        .is_err());
    }
}
