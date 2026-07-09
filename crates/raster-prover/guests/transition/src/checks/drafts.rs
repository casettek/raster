//! Checks that a tile step's draft transition chains correctly: the replay
//! journal's pre-state is authenticated by the draft witness, ops re-apply to
//! the derived post-root, and roots stay continuous within a tracked chain.

use std::collections::BTreeMap;

use raster_core::draft::{
    apply_draft_ops, schema_hash as compute_schema_hash, verify_witness_root,
    DraftReplayTransition, DraftTransitionWitness, TileReplayJournal, TrackedDraftState,
};
use raster_core::trace::StepRecord;

pub fn verify_draft_transition(
    step_record: &StepRecord,
    replay_journal: Option<&TileReplayJournal>,
    draft_transition_witness: Option<&DraftTransitionWitness>,
    active_drafts: &mut BTreeMap<[u8; 32], TrackedDraftState>,
) {
    if !step_record.requires_replay_proof() {
        assert!(
            replay_journal.is_none(),
            "Only tile steps may carry replay journals",
        );
        assert!(
            draft_transition_witness.is_none(),
            "Only tile steps may carry draft transition witnesses",
        );
        return;
    }

    let Some(replay_journal) = replay_journal else {
        panic!("TileExec replay journal is missing");
    };
    let Some(DraftReplayTransition {
        draft_id,
        schema_hash,
        root_before,
        ops,
    }) = replay_journal.draft_transition.as_ref()
    else {
        assert!(
            draft_transition_witness.is_none(),
            "TileExec without replay-emitted draft transition must not carry draft witnesses",
        );
        return;
    };

    let witness = draft_transition_witness
        .expect("TileExec with replay-emitted draft transition must carry draft witness");
    let witness_schema_hash = compute_schema_hash(&witness.pre_state.schema);
    assert_eq!(
        &witness_schema_hash, schema_hash,
        "Draft witness schema hash does not match replay journal schema hash",
    );
    verify_witness_root(&witness.pre_state, root_before)
        .expect("Draft witness root must match replay journal root_before");

    if let Some(native_transition) = witness.native_transition.as_ref() {
        assert_eq!(
            native_transition.schema_hash, *schema_hash,
            "Native draft capture schema hash does not match replay journal",
        );
        assert_eq!(
            native_transition.root_before, *root_before,
            "Native draft capture root_before does not match replay journal",
        );
    }

    if let Some(tracked_state) = active_drafts.get(draft_id) {
        assert_eq!(
            tracked_state.schema_hash, *schema_hash,
            "Tracked draft schema hash changed within a draft chain",
        );
        assert_eq!(
            tracked_state.root, *root_before,
            "Replay journal root_before does not match tracked draft root",
        );
    }

    let (_, root_after) = apply_draft_ops(&witness.pre_state, ops)
        .expect("Draft replay ops must apply to authenticated pre-state witness");

    active_drafts.insert(
        *draft_id,
        TrackedDraftState {
            schema_hash: *schema_hash,
            root: root_after,
        },
    );
}
