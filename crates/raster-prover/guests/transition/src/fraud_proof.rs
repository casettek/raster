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
use raster_core::trace::{StepKind, StepRecord};
use raster_core::transition::{
    EntrypointAuthorization, InitTransition, OutputAuthorization, Transition, TransitionInput,
    TransitionJournal, TransitionState,
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
    /// - `Init`: start from the genesis state carried in the transition,
    ///   and independently decide what the chain owes for entry-argument
    ///   authorization — the guest never trusts a host-supplied claim about
    ///   the window's initial storage contents.
    /// - `Next`: read the previous journal, recursively verify its receipt
    ///   against our own image id, and require state and manifest
    ///   continuity. Entry-argument authorization is inherited from the
    ///   previous (recursively verified) journal.
    pub fn proceed(
        params: &PublicParams,
        input: &TransitionInput,
        state: TransitionState,
    ) -> (Self, LiveTransition) {
        match state {
            TransitionState::Init(init_transition) => {
                let entrypoint_authorization = checks::entrypoint::verify_genesis_authorization(
                    &params.cfs_cursor,
                    &init_transition.init_storage_root,
                    &init_transition.init_storage_index_root,
                    &input.authorization_journal,
                    input.entrypoint_membership_witness.as_ref(),
                    &input.step_record,
                );
                // The output is bound by the trace's last step, `ProgramEnd`,
                // which no window can open after — so a fresh chain owes it
                // (`Pending`) until that step is verified, with no genesis
                // witness route.
                let output_authorization = if params.cfs_cursor.main_produces_output() {
                    OutputAuthorization::Pending
                } else {
                    OutputAuthorization::NotRequired
                };
                let live = LiveTransition::genesis(
                    &init_transition,
                    entrypoint_authorization,
                    output_authorization,
                );
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

                let live = LiveTransition::resume(
                    &transition,
                    prev_journal.entrypoint_authorization,
                    prev_journal.output_authorization,
                );
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
    storage_frontier: NonEmptyFrontier<Bytes>,
    storage_index_root: Vec<u8>,
    active_drafts: BTreeMap<DraftId, TrackedDraftState>,
    fingerprint_acc: FingerprintAccumulator,
    /// `None` only for the genesis state, where no coordinates are expected yet.
    next_expected_coordinates: Option<Vec<CfsCoordinates>>,
    /// How far this chain has got in tying `main`'s entry arguments to the
    /// authorization journal. Advances at most once, `Pending` ->
    /// `Established`, when an `Entrypoint` step is verified in this window.
    entrypoint_authorization: EntrypointAuthorization,
    /// How far this chain has got in tying the program's output to committed
    /// storage. Advances `Pending` -> `Established` when the `ProgramEnd` step
    /// is verified in this window.
    output_authorization: OutputAuthorization,
}

impl LiveTransition {
    /// Genesis state for the first step of the window.
    fn genesis(
        init_transition: &InitTransition,
        entrypoint_authorization: EntrypointAuthorization,
        output_authorization: OutputAuthorization,
    ) -> Self {
        let frontier = deserialize_frontier(&init_transition.init_frontier)
            .expect("Invalid frontier in input");
        let storage_frontier =
            deserialize_frontier(&init_transition.init_storage_frontier)
                .expect("Invalid storage frontier in input");
        assert_eq!(
            frontier_root(&storage_frontier),
            init_transition.init_storage_root,
            "Initial storage root does not match initial storage frontier",
        );

        Self {
            frontier,
            storage_frontier,
            storage_index_root: init_transition.init_storage_index_root.clone(),
            active_drafts: init_transition.active_drafts.clone(),
            fingerprint_acc: FingerprintAccumulator::new(init_transition.fingerprint.bits_packer),
            next_expected_coordinates: None,
            entrypoint_authorization,
            output_authorization,
        }
    }

    /// Resume from the state carried over from the previous (verified) step.
    fn resume(
        transition: &Transition,
        entrypoint_authorization: EntrypointAuthorization,
        output_authorization: OutputAuthorization,
    ) -> Self {
        let frontier =
            deserialize_frontier(&transition.frontier).expect("Invalid frontier in input");
        let storage_frontier = deserialize_frontier(&transition.storage_frontier)
            .expect("Invalid storage frontier in input");
        assert_eq!(
            frontier_root(&storage_frontier),
            transition.storage_root,
            "Transition storage root does not match transition storage frontier",
        );

        Self {
            frontier,
            storage_frontier,
            storage_index_root: transition.storage_index_root.clone(),
            active_drafts: transition.active_drafts.clone(),
            fingerprint_acc: transition.actual_fingerprint_acc.clone(),
            next_expected_coordinates: Some(transition.next_expected_coordinates.clone()),
            entrypoint_authorization,
            output_authorization,
        }
    }

    /// What this chain has established so far — read by `main` to commit the
    /// journal the next step inherits.
    pub fn entrypoint_authorization(&self) -> EntrypointAuthorization {
        self.entrypoint_authorization
    }

    pub fn output_authorization(&self) -> OutputAuthorization {
        self.output_authorization
    }

    /// Verify every recorded aspect of one step and advance the state:
    ///
    /// - the step's inputs obey the CFS bindings at its coordinates,
    /// - a `ProgramStart` step binds exactly the CFS-declared entry arguments
    ///   to their authorized commitments,
    /// - recorded IO commitments match the witnesses, and tile steps carry
    ///   a verified replay proof,
    /// - the storage transition is consistent with the recorded roots,
    /// - the step's coordinates are among the expected next coordinates,
    /// - the draft chain stays continuous,
    /// - the step record is appended to the trace frontier and fingerprint.
    pub fn apply_verified_step(mut self, cfs_cursor: &CfsCursor, input: &TransitionInput) -> Self {
        checks::cfs::verify_step_record_inputs(
            cfs_cursor,
            &input.step_record,
            input.input_source_witness.as_ref(),
            input.sequence_scope_witness.as_ref(),
        );
        if let StepKind::ProgramStart(program_start) = &input.step_record.kind {
            self.entrypoint_authorization = checks::entrypoint::verify_step(
                cfs_cursor,
                &input.step_record,
                program_start,
                &input.authorization_journal,
            );
        }
        if let StepKind::ProgramEnd(program_end) = &input.step_record.kind {
            // The output object lives in the current storage state (the
            // frontier reflects every prior step's writes; `ProgramEnd` adds
            // none), so verify the read against the current roots.
            let current_storage_root = frontier_root(&self.storage_frontier);
            self.output_authorization = checks::entrypoint::verify_program_end(
                cfs_cursor,
                &input.step_record,
                program_end,
                &current_storage_root,
                &self.storage_index_root,
                input.program_output_read_witness.as_ref(),
                input.program_output_selection_witness.as_ref(),
            );
        }
        checks::io::verify_step_record(
            &input.step_record,
            input.replay_image_id.as_ref(),
            input.replay_journal.as_ref(),
            input.input_witness.as_ref(),
            input.output_witness.as_ref(),
            input.input_source_witness.as_ref(),
        );
        let (_, _, next_index_root) = checks::store::verify_storage_transition(
            &input.step_record,
            input.input_source_witness.as_ref(),
            &input.storage_selection_witnesses,
            input.output_witness.as_ref(),
            input.storage_witness.as_ref(),
            &mut self.storage_frontier,
            &self.storage_index_root,
        );
        self.storage_index_root = next_index_root;
        // `get_next_expected_coordinates` both checks this step was expected
        // and computes the successors. `ProgramEnd` is the unique terminal
        // step: it must be expected, but nothing may follow it.
        let next_coordinates = checks::cfs::get_next_expected_coordinates(
            cfs_cursor,
            &input.step_record,
            self.next_expected_coordinates.as_ref(),
        );
        self.next_expected_coordinates = Some(if matches!(input.step_record.kind, StepKind::ProgramEnd(_)) {
            Vec::new()
        } else {
            next_coordinates
        });
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
    ///
    /// Entry-argument authorization needs no deadline here: it is
    /// `Established` (or `NotRequired`) from the moment the window opens, so
    /// there is never an unauthorized chain able to reach `Finished`.
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
            storage_frontier: serialize_frontier(&self.storage_frontier),
            storage_root: frontier_root(&self.storage_frontier),
            storage_index_root: self.storage_index_root,
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
    entrypoint_authorization: EntrypointAuthorization,
    output_authorization: OutputAuthorization,
) {
    let journal = TransitionJournal {
        init_state,
        current_state,
        transition_image_id,
        authorization_image_id: input.authorization_image_id.clone(),
        manifest_commitment: input.authorization_journal.manifest_commitment.clone(),
        entrypoint_authorization,
        output_authorization,
    };

    env::commit(&journal);
}
