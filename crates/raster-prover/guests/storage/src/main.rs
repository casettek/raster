//! RISC0 guest program for the storage transitions of a segment of transition
//! steps.
//!
//! Proves that each step's storage reads resolve in the store as it stood
//! before the step, that its selection witnesses fold to the objects they
//! read, and that its write — or its absence — moves the append log and the
//! coordinate index to the roots the record claims. A segment of one step is
//! the per-step mode; a segment covering the window is the whole-window mode.
//! The journal commits a Merkle root over one entry per step; each transition
//! step verifies this receipt, checks its own entry against the root, equates
//! the entry with its own state, and adopts the entry's state after. See
//! `docs/proposals/storage-prover.md`.

mod segment;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::storage_check::{entries_root, StorageJournal, StorageSegmentHeader};

fn main() {
    // Host inputs, in write order: the segment header, then one frame per
    // step. Steps are read one at a time and dropped once checked, so the
    // guest never holds more than one step's selection payloads and index
    // proofs — a window's worth in one heap is how a 106 MB `List<MergeBucket>`
    // once exhausted the transition guest.
    let header: StorageSegmentHeader = frame::read();
    let first_position = header.first_position;
    let entries = segment::verify_segment(header, frame::read);

    // Commit a root over the entries, not the entries: each transition step is
    // handed its own entry with a path, so its work doesn't grow with the
    // segment. The entries go to the host on stdout, untrusted — the host can
    // only use them if they fold to the committed root.
    let leaves: Vec<_> = entries.iter().map(|entry| entry.leaf_hash()).collect();
    let journal = StorageJournal {
        first_position,
        entry_count: entries.len() as u64,
        entries_root: entries_root(&leaves),
    };
    env::commit_slice(&journal.encode());
    env::write_slice(&postcard::to_allocvec(&entries).expect("entries always encode"));
}
