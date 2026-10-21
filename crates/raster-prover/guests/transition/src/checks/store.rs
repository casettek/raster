//! The storage check, as the transition guest consumes it: the storage guest
//! proves the storage transitions of a segment of steps and commits a root over
//! one entry per step; this verifies that receipt, checks this step's entry
//! against the root, holds every field of it the transition guest also holds to
//! its own value, and returns the store after the step for the caller to adopt.
//! The segment may be this step alone or the whole window; nothing here depends
//! on which. See `docs/proposals/storage-prover.md`.
//!
//! The check itself — reads, the write, the witness shape — is
//! `guest_common::storage`. Its read half stays callable here
//! (`verify_storage_read_witness`) for the reads recur and entrypoint make
//! against the roots this check vouches for.

use std::collections::BTreeMap;

use risc0_zkvm::guest::env;

use raster_core::input::SelectionWitness;
use raster_core::storage_check::{
    selection_witnesses_commitment, StorageEvidence, StorageState, StorageStepEntry,
};
use raster_core::trace::{FnInput, StepRecord};

use crate::checks::io::input_source_commitment;
use crate::merkle_tree::hash_trace_item;

/// What the transition guest holds about the step the storage guest checked:
/// its own copy of every input both guests read, and its own store before the
/// step.
pub struct StorageFacts<'a> {
    pub step_record: &'a StepRecord,
    pub input_source_witness: Option<&'a FnInput>,
    /// Read again here by recur and io, which rely on the storage check having
    /// folded them.
    pub storage_selection_witnesses: &'a BTreeMap<String, SelectionWitness>,
    /// The trace frontier's position before this step is appended.
    pub trace_position: u64,
    /// The store before this step.
    pub before: &'a StorageState,
}

/// Verify the storage receipt covering this step and return the store after
/// it.
///
/// The `env::verify` is unconditional: every step carries evidence, so no step
/// skips the storage check by omitting it. Every step verifies the receipt, its
/// entry's path and its entry's fields itself, even when several steps share
/// one segment: each transition step is its own execution, handed its evidence
/// by the host, so a check another step made says nothing about this one.
pub fn verify_storage(
    storage_image_id: &[u8],
    evidence: &StorageEvidence,
    facts: &StorageFacts<'_>,
) -> StorageState {
    let image_id = risc0_zkvm::sha::Digest::try_from(storage_image_id)
        .expect("storage image id must be 32 bytes");
    env::verify(image_id, evidence.journal.encode().as_slice())
        .expect("Failed to verify storage journal");
    assert_evidence_matches(evidence, facts).after.clone()
}

/// Check that the evidence's entry is the segment's entry for this step, then
/// hold each field to the transition guest's own value. Returns the entry.
///
/// Each equality covers one input both guests hold, so a receipt for some
/// other step, store or witness set is refused. The storage witness is absent
/// by design: only the storage guest reads it, and it checked it against
/// `before` and the record's roots, which are here.
pub fn assert_evidence_matches<'e>(
    evidence: &'e StorageEvidence,
    facts: &StorageFacts<'_>,
) -> &'e StorageStepEntry {
    assert!(
        evidence.journal.index_of(facts.trace_position).is_some(),
        "storage segment does not cover trace position {}",
        facts.trace_position,
    );
    assert!(
        evidence.entry_is_committed_at(facts.trace_position),
        "storage entry for trace position {} is not the one the segment committed",
        facts.trace_position,
    );
    let entry = &evidence.entry;
    assert_eq!(
        entry.step_record_hash,
        hash_trace_item(facts.step_record),
        "storage and transition checks ran on different step records",
    );
    assert_eq!(
        entry.input_source_commitment,
        facts.input_source_witness.map(input_source_commitment),
        "storage and transition checks ran on different step arguments",
    );
    // Load-bearing: recur and io read these witnesses in this guest assuming
    // the storage check folded them, and the record anchors them only through
    // those folds.
    assert_eq!(
        entry.selection_witnesses_commitment,
        selection_witnesses_commitment(facts.storage_selection_witnesses),
        "storage and transition checks ran on different selection witnesses",
    );
    assert_eq!(
        entry.trace_position, facts.trace_position,
        "storage check ran at a different trace position",
    );
    assert_eq!(
        &entry.before, facts.before,
        "storage check started from a different store",
    );
    entry
}
