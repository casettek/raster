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
use raster_core::trace::StepRecord;
use raster_core::transition::{InitTransition, TransitionInput, TransitionJournal, TransitionState};
use std::collections::HashMap;

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
    _witness: &HashMap<StepRecord, Vec<u8>>,
    replayed_results: &std::collections::BTreeMap<String, ReplayResult>,
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
        let (input, replay_receipt_assumption) = match step_record {
            StepRecord::TileExec(record) => {
                let Some(replay_result) = replayed_results.get(&record.fn_call_record.fn_name)
                else {
                    panic!("Replayed IMAGE ID not found");
                };
                let replay_receipt: risc0_zkvm::Receipt =
                    postcard::from_bytes(&replay_result.receipt).unwrap();

                (
                    TransitionInput {
                        step_record: step_record.clone(),
                        replay_image_id: Some(replay_result.image_id.clone()),
                    },
                    Some(replay_receipt),
                )
            }
            StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => (
                TransitionInput {
                    step_record: step_record.clone(),
                    replay_image_id: None,
                },
                None,
            ),
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
