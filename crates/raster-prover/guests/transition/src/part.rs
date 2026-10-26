//! The transition part (`docs/proposals/transition-split.md`): the semantic
//! checks — io, recur, drafts, entrypoint — over a segment of steps, with no
//! receipt verified and no other part's state.
//!
//! What it needs from other guests arrives as values, and leaves as chains
//! FRAUD WINDOW compares with the producers':
//! - the store before each step and its selection witnesses, through
//!   [`checks::columns`], folded into the two column chains the storage part
//!   also folds;
//! - each tile step's replay journal, checked against the record and its
//!   witnesses, and folded as a [`ReplayClaim`] FRAUD WINDOW verifies;
//! - the authorization journal, read by the entrypoint checks and committed as
//!   a digest FRAUD WINDOW verifies.
//!
//! Every step also folds its record into the record chain, so the parts can be
//! shown to have run on the same records. The part's own state — recur
//! progress and the two authorization stages — is committed as a digest before
//! and after the segment and stitched to its neighbours'. The segment opening
//! the window derives its start from `InitTransition`.

use raster_core::cfs::{CfsCoordinates, CfsCursor, SequenceChildItem};
use raster_core::fraud_window::{
    authorization_digest, window_id, Chain, ChainKind, Column, PartKind, PartOutput,
    PartSegmentJournal, ReplayClaim, Span, TransitionOutput, TransitionPartState, WindowChains,
};
use raster_core::program::{ImageId, ProgramDefinition};
use raster_core::recur_progress::RecurProgressStack;
use raster_core::storage_check::selection_witnesses_commitment;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord};
use raster_core::transition::{OutputAuthorization, TransitionPartHeader, TransitionStep};

use crate::checks;
use crate::merkle_tree::hash_trace_item;

/// The program a segment runs under: decoded from its `program.bin` frame,
/// whose commitment the segment commits.
pub struct PartProgram {
    pub program: ProgramDefinition,
    pub program_commitment: Vec<u8>,
    pub cfs_cursor: CfsCursor,
}

impl PartProgram {
    /// The frame is hashed before decoding, so the committed commitment is
    /// bound to the exact bytes checked against.
    pub fn from_frame(program_bytes: &[u8]) -> Self {
        let program_commitment = raster_core::program::commitment_of_bytes(program_bytes).to_vec();
        let program = ProgramDefinition::decode(program_bytes)
            .expect("host must supply a valid ProgramDefinition frame");
        let cfs_cursor = CfsCursor::new(program.cfs.clone());
        Self {
            program,
            program_commitment,
            cfs_cursor,
        }
    }
}

/// Run the semantic checks over every step of a segment and return its
/// journal, and the state after it — which the journal commits only as a
/// digest, and the host hands the next segment as its start. Steps are pulled
/// from `next_step` one at a time, so each is dropped before the next is read.
pub fn verify_segment(
    program: &PartProgram,
    header: TransitionPartHeader,
    mut next_step: impl FnMut() -> TransitionStep,
) -> (PartSegmentJournal, TransitionPartState) {
    assert!(
        header.step_count > 0,
        "a transition segment must cover at least one step"
    );
    let window = window_id(&header.init);
    let opens_window = header.first_position == header.init.init_frontier.position;
    let cfs = &program.cfs_cursor;

    let mut chains = WindowChains {
        records: Chain::resume(ChainKind::Records, header.records_before),
        store_before: Chain::resume(
            ChainKind::Column(Column::StoreBefore),
            header.columns_before[0],
        ),
        selections: Chain::resume(
            ChainKind::Column(Column::Selections),
            header.columns_before[1],
        ),
    };
    let mut replay_claims = Chain::resume(ChainKind::ReplayClaims, header.replay_claims_before);

    let first = next_step();
    let mut state = if opens_window {
        assert!(
            header.start.is_none(),
            "the segment opening the window derives its state; the host may not supply one",
        );
        // Decide what the window owes for entry-argument
        // authorization from its initial store, never from a host claim; owe
        // the output until `ProgramEnd`; and take the recur seed, which the
        // first step's own commitment then has to reproduce.
        let entrypoint_authorization = checks::entrypoint::verify_genesis_authorization(
            cfs,
            &header.init.init_storage_root,
            &header.init.init_storage_index_root,
            &header.authorization_journal,
            header.entrypoint_membership_witness.as_ref(),
            &first.step_record,
        );
        let output_authorization = if cfs.main_produces_output() {
            OutputAuthorization::Pending
        } else {
            OutputAuthorization::NotRequired
        };
        assert_seed_present_for_mid_loop_open(
            cfs,
            &first.step_record,
            header.window_start_recur_progress.as_ref(),
        );
        TransitionPartState {
            recur_progress: header
                .window_start_recur_progress
                .clone()
                .unwrap_or_default(),
            entrypoint_authorization,
            output_authorization,
        }
    } else {
        // Read only where the window opens: anywhere else nothing reads them,
        // so they must be absent, not ignored.
        assert!(
            header.window_start_recur_progress.is_none(),
            "Only a window's opening step may carry a recur-progress seed",
        );
        assert!(
            header.entrypoint_membership_witness.is_none(),
            "Only a window's opening step may carry an entrypoint membership witness",
        );
        header
            .start
            .clone()
            .expect("a segment inside the window starts from the state the one before it ended on")
    };
    let state_before = state.digest();

    let mut step = Some(first);
    for _ in 0..header.step_count {
        let input = step.take().unwrap_or_else(&mut next_step);
        if let Some(claim) = apply_step(program, &header, &input, &mut state, &mut chains) {
            claim.fold_into(&mut replay_claims);
        }
    }

    let journal = PartSegmentJournal {
        part: PartKind::Transition,
        window_id: window,
        program_commitment: Some(program.program_commitment.clone()),
        first_position: header.first_position,
        step_count: header.step_count,
        records: Span {
            before: header.records_before,
            after: chains.records.value(),
        },
        state: Span {
            before: state_before,
            after: state.digest(),
        },
        columns: vec![
            (
                Column::StoreBefore,
                Span {
                    before: header.columns_before[0],
                    after: chains.store_before.value(),
                },
            ),
            (
                Column::Selections,
                Span {
                    before: header.columns_before[1],
                    after: chains.selections.value(),
                },
            ),
        ],
        out: Some(PartOutput::Transition(TransitionOutput {
            entrypoint_authorization: state.entrypoint_authorization,
            output_authorization: state.output_authorization.clone(),
            replay_claims: Span {
                before: header.replay_claims_before,
                after: replay_claims.value(),
            },
            authorization: authorization_digest(&header.authorization_journal),
        })),
    };
    (journal, state)
}

/// The semantic checks of one step. Returns the step's replay claim.
fn apply_step(
    program: &PartProgram,
    header: &TransitionPartHeader,
    input: &TransitionStep,
    state: &mut TransitionPartState,
    chains: &mut WindowChains,
) -> Option<ReplayClaim> {
    let cfs = &program.cfs_cursor;
    let step_record_hash = hash_trace_item(&input.step_record);
    let input_source_commitment = input
        .input_source_witness
        .as_ref()
        .map(checks::io::input_source_commitment);
    let selections_commitment = selection_witnesses_commitment(&input.storage_selection_witnesses);
    // The two values the checks below read from the storage part, folded into
    // their column chains as they cross.
    let store_before = checks::columns::consume_store_before(chains, &input.store_before);
    let selections = checks::columns::consume_selections(
        chains,
        &input.storage_selection_witnesses,
        &selections_commitment,
    );
    checks::recur::verify_recur_iteration_chunking(
        cfs,
        &input.step_record,
        input.replay_journal.as_ref(),
    );
    let draft_step = checks::drafts::verify_draft_transition(
        &input.step_record,
        input.replay_journal.as_ref(),
        input.draft_transition_witness.as_ref(),
    );
    checks::recur::advance_recur_progress(
        cfs,
        &mut state.recur_progress,
        &input.step_record,
        input.replay_journal.as_ref(),
        draft_step.as_ref(),
        input.input_source_witness.as_ref(),
        input.output_witness.as_ref(),
        &selections,
        input.returned_state_read_witness.as_ref(),
        &store_before,
    );
    if let StepKind::ProgramStart(program_start) = &input.step_record.kind {
        state.entrypoint_authorization = checks::entrypoint::verify_step(
            cfs,
            &input.step_record,
            program_start,
            &header.authorization_journal,
        );
    }
    if let StepKind::ProgramEnd(program_end) = &input.step_record.kind {
        state.output_authorization = checks::entrypoint::verify_program_end(
            cfs,
            &input.step_record,
            program_end,
            &store_before,
            input.program_output_read_witness.as_ref(),
            input.program_output_selection_witness.as_ref(),
        );
    }
    let claim = checks::io::verify_step_record(
        &input.step_record,
        expected_tile_image_id(&program.program, &input.step_record),
        input.replay_journal.as_ref(),
        input.input_witness.as_ref(),
        input.output_witness.as_ref(),
        input.input_source_witness.as_ref(),
        input_source_commitment.as_deref(),
        &selections,
    );
    chains.records.fold(&step_record_hash);
    claim
}

/// The registry image id for a tile step, or `None` for a step that carries no
/// replay proof. Panics if a tile step's id is absent from the program
/// registry — an unregistered tile cannot be proven.
fn expected_tile_image_id<'a>(
    program: &'a ProgramDefinition,
    step_record: &StepRecord,
) -> Option<&'a ImageId> {
    match &step_record.kind {
        StepKind::Exec(ExecStep {
            target: ExecTarget::Tile(name),
            ..
        }) => Some(
            program
                .tile_image_id(name)
                .unwrap_or_else(|| panic!("tile '{name}' has no image id in the program registry")),
        ),
        _ => None,
    }
}

/// Refuse a window that opens inside a live recur site with no seed, naming
/// the cause.
///
/// **Diagnostics only, no soundness weight.** The advance-and-compare in
/// [`checks::recur::advance_recur_progress`] remains the sole authority on
/// whether a seed is correct — a wrong seed advances to a different stack and
/// fails there, seeded or not.
///
/// What this buys is the error message. Without it, a missing seed surfaces as
/// a recur-progress commitment mismatch, which reads like a soundness
/// violation in the trace and points a reader at the guest rather than at the
/// host that failed to reconstruct the seed. That misreading is exactly what
/// let an unfilled parameter ship for weeks as a de-facto "refuse to open
/// mid-loop" rule — the design `recur-progress-commitment.md` §Problem had
/// explicitly rejected. See `window-seed-reconstruction.md` §Uncertainty 3.
///
/// Best-effort by construction: it fires on a step whose coordinates sit under
/// a recur site, and stays silent where the CFS cannot resolve them.
fn assert_seed_present_for_mid_loop_open(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    seed: Option<&RecurProgressStack>,
) {
    if let Some(seed) = seed {
        if !seed.is_empty() {
            return;
        }
    }

    let coordinates = step_record.coordinates();
    // A *strict* prefix naming a recur site means this step executes inside
    // one. The full coordinate is excluded on purpose: a window opening on the
    // site's own `Start` opens before the frame is pushed, so the empty stack
    // is the true state there.
    let opens_inside_recur_site = (1..coordinates.len()).any(|depth| {
        matches!(
            cfs_cursor.try_get_item(&CfsCoordinates(coordinates[..depth].to_vec())),
            Some(SequenceChildItem::RecurTile(_)) | Some(SequenceChildItem::RecurSequence(_))
        )
    });

    assert!(
        !opens_inside_recur_site,
        "Window opens at {:?}, which executes inside a live recur site, but no \
         recur-progress seed was supplied. The host must reconstruct it from the \
         trace prefix (`TraceRecorder::recur_progress_after`); an empty stack here \
         is the positive claim \"no loop in flight\", which these coordinates \
         contradict.",
        coordinates
    );
}
