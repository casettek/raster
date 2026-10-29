//! RISC0 guest answering a disputed stage challenge
//! (`docs/proposals/stage-challenge.md` §Phase 3).
//!
//! One execution verifies one transition receipt — a window proven against the
//! challenger's trace commitment `H_C` — and commits a `DefenceJournal`:
//! *"this challenge is false"*, either because the window refutes `H_C`
//! (`Finished`) or because honest execution of `H_C` ends in an output other
//! than the one claimed (`TerminalMatch`). The decision is
//! `raster_core::chain::defend`, the same function the host runs before
//! proving.
//!
//! A guest of its own, not a mode of the chain-fraud guest: one image id
//! commits one journal type, so a defence can never be decoded as a chain
//! fraud proof or the reverse. The transition image id is committed for the
//! relying party to pin, as the chain-fraud guest does.

use risc0_zkvm::guest::env;

use raster_core::chain::{defend, DefenceInput};

fn main() {
    let input: DefenceInput = env::read();

    let image_id = risc0_zkvm::sha::Digest::try_from(input.transition_image_id.as_slice())
        .expect("transition image id must be 32 bytes");
    env::verify(
        image_id,
        &risc0_zkvm::serde::to_vec(&input.transition_journal).unwrap(),
    )
    .expect("Failed to verify the transition receipt");

    let journal = defend(&input).unwrap_or_else(|refusal| panic!("No defence: {refusal}"));
    env::commit(&journal);
}
