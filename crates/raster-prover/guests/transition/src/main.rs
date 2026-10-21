//! RISC0 guest program for trace state transitions.
//!
//! Each execution proves a segment of consecutive steps of a fraud-proof
//! window — one step, the whole window, or anything between, chosen by the
//! host (`docs/proposals/transition-segments.md`):
//!
//! 1. Attach to the chain: genesis state (`Init`) or a recursively verified
//!    previous transition journal (`Next`).
//! 2. For each step: verify it against every recorded commitment (the
//!    control-flow guest's receipt for the CFS bindings, the storage guest's
//!    receipt for the storage transition, IO/replay, draft chain), append it to
//!    the trace frontier + fingerprint, and compare the accumulated fingerprint
//!    with the committed one. The state stays in memory from step to step.
//! 3. Commit the resulting journal (`Next` for the next execution to resume
//!    from, or `Finished` on the proven divergence).

mod checks;
mod fraud_proof;

// Shared with the control-flow guest; imported at the crate root so the
// `crate::frame` and `crate::merkle_tree` paths read as they always have.
use guest_common::{frame, merkle_tree};

#[cfg(test)]
mod tests;

use raster_core::transition::{
    FingerprintSliceWitness, TraceCommitmentHeader, TransitionInput, TransitionSegmentHeader,
    TransitionState,
};

use crate::checks::io::verify_authorization_journal;
use crate::checks::receipts::VerifiedJournals;
use crate::fraud_proof::{
    assert_init_only_inputs_absent, assert_same_authorization, commit_journal,
    FraudProofWindowContext, PublicParams,
};

fn main() {
    // Host inputs, in write order: the public parameters, the segment header,
    // the first step's input, the state, then — at `Init` — the commitment
    // binding or — at `Next` — the previous journal (read inside
    // `FraudProofWindowContext::proceed`), then every later step's input.
    let params = PublicParams::read();
    let header: TransitionSegmentHeader = frame::read();
    assert!(
        header.step_count > 0,
        "a transition segment must cover at least one step"
    );
    let first: TransitionInput = frame::read();
    let state: TransitionState = frame::read();
    let opens_window = matches!(state, TransitionState::Init(_));
    // Only the window-opening segment binds the committed fingerprint to a
    // named `commit.bin`: it reads the commitment's compact header and the
    // slice witness covering this window. `Next` segments inherit the (already
    // verified) binding from the previous journal instead.
    let commitment_binding: Option<(TraceCommitmentHeader, FingerprintSliceWitness)> =
        opens_window.then(|| (frame::read(), frame::read()));

    // Precondition: external inputs were authorized against a manifest. The
    // journal is the same for the whole window, so it is verified once here
    // and every later step's copy is held equal to it.
    assert!(verify_authorization_journal(
        &first.authorization_journal,
        &first.authorization_image_id
    ));
    if !opens_window {
        assert_init_only_inputs_absent(&first);
    }

    // Attach this segment to the fraud-proof chain.
    let (window_context, mut live) =
        FraudProofWindowContext::proceed(&params, &first, state, commitment_binding);

    let mut journals = VerifiedJournals::default();
    let mut reached_window_end = false;
    let mut first = Some(first);
    let mut last: Option<TransitionInput> = None;
    for _ in 0..header.step_count {
        assert!(
            !reached_window_end,
            "a transition segment may not continue past the window's last step"
        );
        let input = match first.take() {
            Some(first) => first,
            None => {
                let input: TransitionInput = frame::read();
                // Read only at a window's `Init`, from its first step; anywhere
                // else nothing reads them, so they must be absent, not ignored.
                assert_init_only_inputs_absent(&input);
                assert_same_authorization(last.as_ref().expect("a previous step"), &input);
                input
            }
        };

        // Verify every recorded aspect of the step and advance the state.
        live = live.apply_verified_step(&params, &input, &mut journals);
        // Every item before the window's last must match the commitment; the
        // last must diverge.
        reached_window_end = live.reaches_window_end(
            &window_context.init_state.fingerprint,
            window_context.final_committed_root.as_ref(),
        );
        // The previous step is dropped here, so the guest holds one step's
        // witnesses at a time.
        last = Some(input);
    }
    let last = last.expect("a segment covers at least one step");

    let entrypoint_authorization = live.entrypoint_authorization();
    let output_authorization = live.output_authorization();
    // Continue the window, or finish on the proven fingerprint divergence.
    let current_state = live.into_state(reached_window_end);

    commit_journal(
        window_context.init_state,
        current_state,
        params.transition_image_id,
        params.control_flow_image_id,
        params.storage_image_id,
        params.program_commitment,
        window_context.refuted_trace_commitment,
        window_context.window_is_terminal,
        window_context.final_committed_root,
        &last,
        entrypoint_authorization,
        output_authorization,
    );
}
