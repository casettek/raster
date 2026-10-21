//! Inner receipts a transition segment has already verified.
//!
//! Every step verifies the control-flow and storage receipts covering it, and
//! consecutive steps in one execution usually share them. Verifying the same
//! claim twice in one execution adds nothing — the first call already made it
//! an assumption of this receipt — so repeats are skipped. Each step still
//! checks its own entry, path and fields against the journal; only the
//! `env::verify` is shared. See `docs/proposals/transition-segments.md`
//! §Inner receipts.

use std::collections::BTreeSet;

use risc0_zkvm::guest::env;

#[derive(Default)]
pub struct VerifiedJournals {
    verified: BTreeSet<(Vec<u8>, Vec<u8>)>,
}

impl VerifiedJournals {
    /// `env::verify(image_id, journal)`, unless this execution already did.
    /// Panics with `what` in the message if the receipt doesn't verify.
    pub fn verify(&mut self, image_id: &[u8], journal: Vec<u8>, what: &str) {
        let key = (image_id.to_vec(), journal);
        if self.verified.contains(&key) {
            return;
        }
        let digest = risc0_zkvm::sha::Digest::try_from(image_id)
            .unwrap_or_else(|_| panic!("{what} image id must be 32 bytes"));
        env::verify(digest, &key.1).unwrap_or_else(|_| panic!("Failed to verify {what} journal"));
        self.verified.insert(key);
    }
}
