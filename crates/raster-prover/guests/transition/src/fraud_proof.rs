//! The transition state machine.
//!
//! A fraud-proof window is proven one step per guest execution:
//!
//! 1. [`FraudProofWindowContext::establish`] attaches the step to the chain — either a
//!    genesis state (`Init`) or a recursively verified previous journal
//!    (`Next`) — and yields the [`LiveTransition`] to advance.
//! 2. [`LiveTransition::apply_verified_step`] verifies every recorded aspect
//!    of the step and folds it into the live state.
//! 3. [`LiveTransition::finalize`] compares the accumulated fingerprint with
//!    the committed window fingerprint and decides `Next` vs `Finished`.
//! 4. [`commit_journal`] commits the resulting [`TransitionJournal`].

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;
use risc0_zkvm::guest::env;

use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema};
use raster_core::draft::{DraftId, TrackedDraftState};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use raster_core::trace::StepRecord;
use raster_core::transition::{
    InitTransition, Transition, TransitionInput, TransitionJournal, TransitionState,
};

use crate::checks;
use crate::merkle_tree::{
    deserialize_frontier, frontier_root, hash_trace_item, serialize_frontier, Bytes,
};

/// Public parameters every step of the fraud proof runs under.
pub struct PublicParams {
    pub cfs_cursor: CfsCursor,
    pub transition_image_id: Vec<u8>,
}

impl PublicParams {
    /// Reads the leading host inputs. The host write order is: cfs,
    /// transition image id, transition input, transition state, and — only
    /// for `Next` steps — the previous journal (read in [`FraudProofWindowContext`]).
    pub fn read() -> Self {
        let cfs: ControlFlowSchema = env::read();
        let transition_image_id: Vec<u8> = env::read();
        Self {
            cfs_cursor: CfsCursor::new(cfs),
            transition_image_id,
        }
    }
}

/// Whether this step opens the fraud-proof window or continues it.
pub enum StepPosition {
    /// First step of the window: committed without a fingerprint comparison.
    First,
    /// Subsequent step: must match the committed fingerprint, except the
    /// final item which must diverge (the fraud being proven).
    Subsequent,
}

/// Where this step attaches to the fraud-proof chain.
pub struct FraudProofWindowContext {
    pub init_state: InitTransition,
    pub position: StepPosition,
}

impl FraudProofWindowContext {
    /// Attach the step to the fraud proof window context.
    ///
    /// - `Init`: start from the genesis state carried in the transition.
    /// - `Next`: read the previous journal, recursively verify its receipt
    ///   against our own image id, and require state and manifest continuity.
    pub fn proceed(
        params: &PublicParams,
        input: &TransitionInput,
        state: TransitionState,
    ) -> (Self, LiveTransition) {
        match state {
            TransitionState::Init(init_transition) => {
                let live = LiveTransition::genesis(&init_transition);
                (
                    Self {
                        init_state: init_transition,
                        position: StepPosition::First,
                    },
                    live,
                )
            }
            TransitionState::Next(transition) => {
                let prev_journal: TransitionJournal = env::read();
                verify_previous_journal(&prev_journal, &params.transition_image_id);
                assert_state_continuity(&prev_journal, &transition);
                assert_manifest_continuity(&prev_journal, input);

                let live = LiveTransition::resume(&transition);
                (
                    Self {
                        init_state: prev_journal.init_state,
                        position: StepPosition::Subsequent,
                    },
                    live,
                )
            }
            TransitionState::Finished => {
                panic!("Finished Transition");
            }
        }
    }
}

/// Recursively verify the previous transition receipt for this same guest.
fn verify_previous_journal(prev_journal: &TransitionJournal, transition_image_id: &[u8]) {
    let transition_image_id_digest =
        risc0_zkvm::sha::Digest::try_from(transition_image_id).expect("image_id must be 32 bytes");
    env::verify(
        transition_image_id_digest,
        &risc0_zkvm::serde::to_vec(prev_journal).unwrap(),
    )
    .expect("Failed to verify previous transition journal");
    assert!(
        transition_image_id == prev_journal.transition_image_id,
        "The transition image ID is not the same within the fraud proof"
    );
}

/// The state we resume from must be exactly the previous journal's output.
fn assert_state_continuity(prev_journal: &TransitionJournal, transition: &Transition) {
    let TransitionState::Next(prev_transition) = &prev_journal.current_state else {
        panic!("Provided Transition state does not align to fraud proof state");
    };
    assert!(
        prev_transition == transition,
        "Transition mismatch: the provided transition does not align with the fraud proof"
    );
}

/// Every step of the fraud proof must be authorized against the same manifest.
fn assert_manifest_continuity(prev_journal: &TransitionJournal, input: &TransitionInput) {
    assert!(
        input.authorization_journal.manifest_commitment == prev_journal.manifest_commitment,
        "Manifest commitment does not match"
    );
}

/// The deserialized, in-progress twin of [`Transition`]: the state advanced
/// by applying one verified step.
pub struct LiveTransition {
    frontier: NonEmptyFrontier<Bytes>,
    internal_store_frontier: NonEmptyFrontier<Bytes>,
    internal_store_index_root: Vec<u8>,
    active_drafts: BTreeMap<DraftId, TrackedDraftState>,
    fingerprint_acc: FingerprintAccumulator,
    /// `None` only for the genesis state, where no coordinates are expected yet.
    next_expected_coordinates: Option<Vec<CfsCoordinates>>,
}

impl LiveTransition {
    /// Genesis state for the first step of the window.
    fn genesis(init_transition: &InitTransition) -> Self {
        let frontier = deserialize_frontier(&init_transition.init_frontier)
            .expect("Invalid frontier in input");
        let internal_store_frontier =
            deserialize_frontier(&init_transition.init_internal_store_frontier)
                .expect("Invalid internal store frontier in input");
        assert_eq!(
            frontier_root(&internal_store_frontier),
            init_transition.init_internal_store_root,
            "Initial internal store root does not match initial internal store frontier",
        );

        Self {
            frontier,
            internal_store_frontier,
            internal_store_index_root: init_transition.init_internal_store_index_root.clone(),
            active_drafts: init_transition.active_drafts.clone(),
            fingerprint_acc: FingerprintAccumulator::new(init_transition.fingerprint.bits_packer),
            next_expected_coordinates: None,
        }
    }

    /// Resume from the state carried over from the previous (verified) step.
    fn resume(transition: &Transition) -> Self {
        let frontier =
            deserialize_frontier(&transition.frontier).expect("Invalid frontier in input");
        let internal_store_frontier = deserialize_frontier(&transition.internal_store_frontier)
            .expect("Invalid internal store frontier in input");
        assert_eq!(
            frontier_root(&internal_store_frontier),
            transition.internal_store_root,
            "Transition internal store root does not match transition internal store frontier",
        );

        Self {
            frontier,
            internal_store_frontier,
            internal_store_index_root: transition.internal_store_index_root.clone(),
            active_drafts: transition.active_drafts.clone(),
            fingerprint_acc: transition.actual_fingerprint_acc.clone(),
            next_expected_coordinates: Some(transition.next_expected_coordinates.clone()),
        }
    }

    /// Verify every recorded aspect of one step and advance the state:
    ///
    /// - the step's inputs obey the CFS bindings at its coordinates,
    /// - recorded IO/external commitments match the witnesses, and tile
    ///   steps carry a verified replay proof,
    /// - the internal store transition is consistent with the recorded roots,
    /// - the step's coordinates are among the expected next coordinates,
    /// - the draft chain stays continuous,
    /// - the step record is appended to the trace frontier and fingerprint.
    pub fn apply_verified_step(mut self, cfs_cursor: &CfsCursor, input: &TransitionInput) -> Self {
        checks::cfs::verify_step_record_inputs(
            cfs_cursor,
            &input.step_record,
            input.input_source_witness.as_ref(),
            input.sequence_scope_witness.as_ref(),
            input.input_witness.as_ref(),
        );
        checks::io::verify_step_record(
            &input.step_record,
            input.replay_image_id.as_ref(),
            input.replay_journal.as_ref(),
            input.input_witness.as_ref(),
            input.output_witness.as_ref(),
            input.input_source_witness.as_ref(),
            &input.external_input,
            &input.external_selection_witnesses,
            &input.authorization_journal.external_inputs_commitments,
        );
        let (_, _, next_index_root) = checks::store::verify_internal_store_transition(
            &input.step_record,
            input.input_source_witness.as_ref(),
            &input.internal_selection_witnesses,
            input.output_witness.as_ref(),
            input.internal_store_witness.as_ref(),
            &mut self.internal_store_frontier,
            &self.internal_store_index_root,
        );
        self.internal_store_index_root = next_index_root;
        self.next_expected_coordinates = Some(checks::cfs::get_next_expected_coordinates(
            cfs_cursor,
            &input.step_record,
            self.next_expected_coordinates.as_ref(),
        ));
        checks::drafts::verify_draft_transition(
            &input.step_record,
            input.replay_journal.as_ref(),
            input.draft_transition_witness.as_ref(),
            &mut self.active_drafts,
        );
        self.append_to_trace(&input.step_record);
        self
    }

    /// Append the step record hash to the trace frontier and accumulate the
    /// resulting root into the actual fingerprint.
    fn append_to_trace(&mut self, step_record: &StepRecord) {
        let item_hash = hash_trace_item(step_record);
        self.frontier.append(Bytes(item_hash));
        let tree_root = frontier_root(&self.frontier);
        self.fingerprint_acc.append(&tree_root);
    }

    /// Decide the next machine state against the committed window fingerprint.
    ///
    /// The first window step is committed without comparison. Every later
    /// step must match the committed fingerprint at its index — except the
    /// final window item, which must diverge: that divergence is the fraud
    /// being proven, and it transitions the machine to `Finished`.
    pub fn finalize(
        self,
        committed_fingerprint: &Fingerprint,
        position: &StepPosition,
    ) -> TransitionState {
        match position {
            StepPosition::First => TransitionState::Next(self.into_transition()),
            StepPosition::Subsequent => {
                let actual_fingerprint: Fingerprint =
                    self.fingerprint_acc.clone().into_fingerprint();
                let last_index = actual_fingerprint.len() - 1;
                let diverges = actual_fingerprint.diff_at_index(last_index, committed_fingerprint);

                if actual_fingerprint.len() == committed_fingerprint.len() {
                    assert!(diverges);
                    TransitionState::Finished
                } else {
                    assert!(!diverges);
                    let mut transition = self.into_transition();
                    transition.actual_fingerprint_acc =
                        FingerprintAccumulator::from(actual_fingerprint);
                    TransitionState::Next(transition)
                }
            }
        }
    }

    /// Fold the live state back into the serializable [`Transition`].
    fn into_transition(self) -> Transition {
        Transition {
            frontier: serialize_frontier(&self.frontier),
            internal_store_frontier: serialize_frontier(&self.internal_store_frontier),
            internal_store_root: frontier_root(&self.internal_store_frontier),
            internal_store_index_root: self.internal_store_index_root,
            active_drafts: self.active_drafts,
            actual_fingerprint_acc: self.fingerprint_acc,
            next_expected_coordinates: self
                .next_expected_coordinates
                .expect("Step must produce next expected coordinates"),
        }
    }
}

/// Commit the step's journal: the window's init state, the advanced state,
/// and the image ids / manifest commitment the chain is verified against.
pub fn commit_journal(
    init_state: InitTransition,
    current_state: TransitionState,
    transition_image_id: Vec<u8>,
    input: &TransitionInput,
) {
    let journal = TransitionJournal {
        init_state,
        current_state,
        transition_image_id,
        authorization_image_id: input.authorization_image_id.clone(),
        manifest_commitment: input.authorization_journal.manifest_commitment.clone(),
    };

    env::commit(&journal);
}
