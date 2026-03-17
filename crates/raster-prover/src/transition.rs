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
use raster_core::fingerprint::{BitPacker, Fingerprint};
use raster_core::trace::StepRecord;
use raster_core::transition::{
    InitTransition, Transition, TransitionInput, TransitionJournal, TransitionState,
};

use crate::replay::ReplayResult;
use crate::trace::SerializableFrontier;
use crate::{TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID};

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
    replayed_results: &std::collections::BTreeMap<String, ReplayResult>,
) -> Option<risc0_zkvm::Receipt> {
    let prover = risc0_zkvm::default_prover();
    let cfs = cfs.clone();

    let init_transition = InitTransition {
        init_frontier: initial_frontier.clone(),
        fingerprint,
    };

    let init_state = TransitionState::Init(init_transition);

    let mut transition_receipt: Option<risc0_zkvm::Receipt> = None;
    let mut current_state = init_state;
    let mut current_journal: Option<TransitionJournal> = None;

    let self_image_id: Vec<u8> = TRANSITION_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect();

    for step_record in trace_window {
        match step_record {
            StepRecord::TileExec(record) => {
                let Some(replay_result) = replayed_results.get(&record.fn_call_record.fn_name)
                else {
                    panic!("Replayed IMAGE ID not found");
                };
                // Create the input for this transition with fingerprint data
                let input = TransitionInput {
                    step_record: step_record.clone(),
                    replay_image_id: Some(replay_result.image_id.clone()),
                };

                let replay_receipt_bytes = replay_result.receipt.clone();
                let replay_receipt: risc0_zkvm::Receipt =
                    postcard::from_bytes(&replay_receipt_bytes).unwrap();

                // Build the executor environment
                let env = if let Some(journal) = current_journal {
                    let Some(transition_receipt) = transition_receipt else {
                        panic!("Transition receipt not found");
                    };
                    risc0_zkvm::ExecutorEnv::builder()
                        .add_assumption(replay_receipt)
                        .add_assumption(transition_receipt)
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .write(&journal)
                        .unwrap()
                        .build()
                        .unwrap()
                } else {
                    risc0_zkvm::ExecutorEnv::builder()
                        .add_assumption(replay_receipt)
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .build()
                        .unwrap()
                };

                let prove_info = prover.prove(env, &TRANSITION_GUEST_ELF).unwrap();

                transition_receipt = Some(prove_info.receipt.clone());
                let journal: TransitionJournal = prove_info.receipt.journal.decode().unwrap();

                current_journal = Some(journal.clone());
                current_state = journal.current_state.clone();
            }
            StepRecord::SequenceStart(record) => {
                let input = TransitionInput {
                    step_record: step_record.clone(),
                    replay_image_id: None,
                };

                // Build the executor environment
                let env = if let Some(journal) = current_journal {
                    let Some(transition_receipt) = transition_receipt else {
                        panic!("Transition receipt not found");
                    };
                    risc0_zkvm::ExecutorEnv::builder()
                        .add_assumption(transition_receipt)
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .write(&journal)
                        .unwrap()
                        .build()
                        .unwrap()
                } else {
                    risc0_zkvm::ExecutorEnv::builder()
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .build()
                        .unwrap()
                };

                let prove_info = prover.prove(env, &TRANSITION_GUEST_ELF).unwrap();

                transition_receipt = Some(prove_info.receipt.clone());
                let journal: TransitionJournal = prove_info.receipt.journal.decode().unwrap();

                current_journal = Some(journal.clone());
                current_state = journal.current_state.clone();
            }
            StepRecord::SequenceEnd(item) => {
                let input = TransitionInput {
                    step_record: step_record.clone(),
                    replay_image_id: None,
                };

                // Build the executor environment
                let env = if let Some(journal) = current_journal {
                    let Some(transition_receipt) = transition_receipt else {
                        panic!("Transition receipt not found");
                    };
                    risc0_zkvm::ExecutorEnv::builder()
                        .add_assumption(transition_receipt)
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .write(&journal)
                        .unwrap()
                        .build()
                        .unwrap()
                } else {
                    risc0_zkvm::ExecutorEnv::builder()
                        .write(&cfs)
                        .unwrap()
                        .write(&self_image_id)
                        .unwrap()
                        .write(&input)
                        .unwrap()
                        .write(&current_state)
                        .unwrap()
                        .build()
                        .unwrap()
                };

                let prove_info = prover.prove(env, &TRANSITION_GUEST_ELF).unwrap();

                transition_receipt = Some(prove_info.receipt.clone());
                let journal: TransitionJournal = prove_info.receipt.journal.decode().unwrap();

                current_journal = Some(journal.clone());
                current_state = journal.current_state.clone();
            }
        }
    }

    transition_receipt
}
