//! Where an input manifest's authority comes from — named, and not yet
//! implemented.
//!
//! # The assumption this module exists to make visible
//!
//! The authorization guest is, in full:
//!
//! ```text
//! let input: ManifestedInputs = env::read();   // { manifest_bytes } — the whole struct
//! let journal = build_authorization_journal(&input);
//! env::commit(&journal);
//! ```
//!
//! It parses the manifest JSON, republishes the commitments the document
//! declares, and hashes the document. It never checks that any input's bytes
//! hash to its declared commitment — it is not given the inputs, and could not.
//!
//! So the entire trust anchor for a program's external inputs is one value:
//! `input_manifest_commitment = sha256(manifest_bytes)`. It is carried on every
//! [`TransitionJournal`](raster_core::transition::TransitionJournal) and held
//! continuous across a window by `assert_manifest_continuity` in the transition
//! guest. A verifier is expected to compare it against a manifest obtained
//! **independently of the prover**.
//!
//! That comparison is the data-availability layer. Nothing in this workspace
//! performs it, and until this module nothing named it either — which is the
//! problem: an unnamed assumption reads like an absent one.
//!
//! # Why this matters more than it looks
//!
//! The fraud-proof machinery leans on it at exactly one point, and it is the
//! sharpest one. A window opening at trace index 0 carries no pre-divergence
//! margin — there are no steps before it — so the transition guest asserts the
//! genesis opening state instead (`assert_opens_at_genesis`). But the trace
//! seed is the same public constant for every program, so genesis fixes the
//! *machine* state and says nothing about **which program ran with which
//! inputs**.
//!
//! That comes from the window's first step being `ProgramStart`, whose
//! `output_commitment` must equal the combined root over this journal's
//! entry-argument commitments (`checks::entrypoint::verify_step`). Follow that
//! chain back and it terminates here: the commitments are whatever the manifest
//! declared. A one-item head window is therefore exactly as trustworthy as the
//! manifest behind it, and no more.
//!
//! # What a real implementation owes
//!
//! 1. the manifest document is retrievable by its digest;
//! 2. for each named input, the bytes behind its commitment are published and
//!    fetchable by any verifier;
//! 3. both stay retrievable for the length of the dispute window — a fraud
//!    proof that cannot be *built* because its inputs vanished settles as
//!    though no fraud occurred.

use sha2::{Digest, Sha256};

/// Whether the bytes behind a named input can be fetched by a verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The layer was not consulted. An admission, not a claim — and the only
    /// answer [`UncheckedAvailability`] gives.
    Unchecked,
    /// Published and fetchable.
    Available,
    /// Committed to, but not retrievable. A prover that cannot produce its own
    /// inputs cannot be audited, so this is a dispute in its own right rather
    /// than a missing optimisation.
    Missing,
}

/// The source of a manifest's authority.
///
/// Implementors answer for a *published* manifest: one a verifier can obtain
/// without asking the prover. See the module docs for what that entails.
pub trait InputAvailability {
    /// `sha256` of the manifest document these answers are about.
    ///
    /// A verifier compares this against the `input_manifest_commitment` on a
    /// receipt's journal. Equality is what ties a proof to a manifest the
    /// verifier already trusts; without it the journal names a document nobody
    /// else has seen.
    fn manifest_commitment(&self) -> &[u8];

    /// Whether `name`'s bytes are retrievable at `commitment`.
    fn availability(&self, name: &str, commitment: &[u8]) -> Availability;
}

/// The stub: it answers [`Availability::Unchecked`] for everything.
///
/// Deliberately not wired into any proving or verification path. Its purpose is
/// to give the assumption one place to live and one place to be replaced, so
/// that adding a real layer is a matter of implementing this trait rather than
/// discovering where the reasoning was supposed to bottom out.
pub struct UncheckedAvailability {
    manifest_commitment: Vec<u8>,
}

impl UncheckedAvailability {
    /// Take the digest of the manifest bytes the authorization guest was given,
    /// computed exactly as the guest computes it.
    pub fn for_manifest(manifest_bytes: &[u8]) -> Self {
        Self {
            manifest_commitment: Sha256::digest(manifest_bytes).to_vec(),
        }
    }
}

impl InputAvailability for UncheckedAvailability {
    fn manifest_commitment(&self) -> &[u8] {
        &self.manifest_commitment
    }

    fn availability(&self, _name: &str, _commitment: &[u8]) -> Availability {
        Availability::Unchecked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::authorization::ManifestedInputs;

    /// The stub's digest must be the one the journal carries, or a verifier
    /// comparing the two would reject honest proofs — and the single anchor the
    /// whole scheme rests on would be anchored to nothing.
    ///
    /// Recomputed here the way `build_authorization_journal` does it, so the
    /// two cannot drift apart silently.
    #[test]
    fn the_stub_reports_the_manifests_own_digest() {
        let manifest = br#"{"arg":{"type":"sha256","commitment":"ab"}}"#;
        let stub = UncheckedAvailability::for_manifest(manifest);

        assert_eq!(stub.manifest_commitment(), Sha256::digest(manifest).as_slice());

        // And the shape the guest is handed is the same bytes, nothing more —
        // which is the whole reason it cannot check anything.
        let inputs = ManifestedInputs {
            manifest_bytes: manifest.to_vec(),
        };
        assert_eq!(inputs.manifest_bytes, manifest);
    }

    #[test]
    fn the_stub_checks_nothing() {
        let stub = UncheckedAvailability::for_manifest(b"{}");
        assert_eq!(
            stub.availability("anything", &[0u8; 32]),
            Availability::Unchecked,
        );
    }
}
