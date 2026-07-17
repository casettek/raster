//! RISC0 guest program for trace state transitions.
//!
//! Each execution proves one step of a fraud-proof window:
//!
//! 1. Attach to the chain: genesis state (`Init`) or a recursively verified
//!    previous transition journal (`Next`).
//! 2. Verify the step against every recorded commitment (CFS bindings,
//!    IO/replay, storage, draft chain) and append it to the trace
//!    frontier + fingerprint.
//! 3. Compare the accumulated fingerprint with the committed one and commit
//!    the resulting journal (`Next`, or `Finished` on the proven divergence).

mod checks;
mod fraud_proof;
mod merkle_tree;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use raster_core::transition::{TransitionInput, TransitionState};

use crate::checks::io::verify_authorization_journal;
use crate::fraud_proof::{commit_journal, FraudProofWindowContext, PublicParams};

fn main() {
    // Host inputs, in write order. A `Next` step's previous journal is read
    // inside `ChainLink::establish`.
    let params = PublicParams::read();
    let input: TransitionInput = env::read();
    let state: TransitionState = env::read();

    // Precondition: external inputs were authorized against a manifest.
    assert!(verify_authorization_journal(
        &input.authorization_journal,
        &input.authorization_image_id
    ));

    // Attach this step to the fraud-proof chain.
    let (window_context, current) = FraudProofWindowContext::proceed(&params, &input, state);

    // Verify every recorded aspect of the step and advance the state.
    let next = current.apply_verified_step(&params.cfs_cursor, &input);
    let entrypoint_authorization = next.entrypoint_authorization();
    let output_authorization = next.output_authorization();

    // Continue the window, or finish on the proven fingerprint divergence.
    let current_state = next.finalize(
        &window_context.init_state.fingerprint,
        &window_context.position,
    );

    commit_journal(
        window_context.init_state,
        current_state,
        params.transition_image_id,
        &input,
        entrypoint_authorization,
        output_authorization,
    );
}
