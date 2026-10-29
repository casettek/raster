//! Host driver for the defence guest (`docs/proposals/stage-challenge.md`
//! §Phase 3).
//!
//! `prove_defence` runs the guest over a `DefenceInput`, with the transition
//! receipt whose journal it carries as an assumption; `verify_defence` performs
//! the relying party's checks — the receipt against the pinned defence image
//! id, then the inner image ids its journal commits against the known-good
//! transition and authorization guests.

use raster_core::chain::{defend, DefenceInput, DefenceJournal};

use crate::authorization::authorization_guest_image_id;
use crate::chain_fraud::transition_guest_image_id;
use crate::error::{BitPackerError, Result};
use crate::{DEFENCE_GUEST_ELF, DEFENCE_GUEST_ID};

/// Prove a defence: verify the transition window in-guest and check it
/// against the challenge.
///
/// The guest's decision is checked here first, with the same function, so a
/// window that defends nothing is refused with its reason rather than as a
/// guest panic after a proof's worth of work.
pub fn prove_defence(
    input: &DefenceInput,
    transition_receipt: risc0_zkvm::Receipt,
) -> Result<(risc0_zkvm::Receipt, DefenceJournal)> {
    let expected = defend(input).map_err(|refusal| {
        BitPackerError::InvalidCommitment(format!("no defence: {refusal}"))
    })?;
    let env = risc0_zkvm::ExecutorEnv::builder()
        .add_assumption(transition_receipt)
        .write(input)
        .map_err(|e| BitPackerError::SerializationError(format!("defence input: {e}")))?
        .build()
        .map_err(|e| BitPackerError::InvalidCommitment(format!("defence executor env: {e}")))?;
    let receipt = risc0_zkvm::default_prover()
        .prove(env, DEFENCE_GUEST_ELF)
        .map_err(|e| BitPackerError::InvalidCommitment(format!("defence proving failed: {e}")))?
        .receipt;
    let journal: DefenceJournal = receipt
        .journal
        .decode()
        .map_err(|e| BitPackerError::SerializationError(format!("defence journal: {e}")))?;
    if journal != expected {
        return Err(BitPackerError::InvalidCommitment(
            "the defence guest committed another journal than the host computed".to_string(),
        ));
    }
    Ok((receipt, journal))
}

/// The relying party's verification: the receipt is a defence receipt (pinned
/// image id), and the inner guests its journal names are the known-good ones —
/// without this a receipt could have verified its window against an
/// attacker-compiled transition guest committing any journal it liked.
pub fn verify_defence(receipt: &risc0_zkvm::Receipt) -> Result<DefenceJournal> {
    receipt
        .verify(DEFENCE_GUEST_ID)
        .map_err(|e| BitPackerError::InvalidCommitment(format!("defence receipt: {e}")))?;
    let journal: DefenceJournal = receipt
        .journal
        .decode()
        .map_err(|e| BitPackerError::SerializationError(format!("defence journal: {e}")))?;
    if journal.transition_image_id != transition_guest_image_id() {
        return Err(BitPackerError::InvalidCommitment(
            "defence receipt verified an unknown transition guest".to_string(),
        ));
    }
    if journal.authorization_image_id != authorization_guest_image_id() {
        return Err(BitPackerError::InvalidCommitment(
            "defence receipt's window was authorized by an unknown authorization guest"
                .to_string(),
        ));
    }
    Ok(journal)
}
