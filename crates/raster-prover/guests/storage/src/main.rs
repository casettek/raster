//! RISC0 guest program for the storage transitions of a segment of
//! fraud-window steps.
//!
//! Proves that each step's storage reads resolve in the store as it stood
//! before the step, that its selection witnesses fold to the objects they
//! read, and that its write — or its absence — moves the append log and the
//! coordinate index to the roots the record claims. A segment of one step is
//! the per-step mode; a segment covering the window is the whole-window mode.
//! The journal (`PartSegmentJournal`) commits the record chain, the store
//! before and after, and the two columns the storage part produces, which FRAUD
//! WINDOW stitches and compares with the transition part's. See
//! `docs/proposals/storage-prover.md` and `transition-split.md`.

mod segment;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::fraud_window::window_id;
use raster_core::storage_check::StorageSegmentHeader;

fn main() {
    // Host inputs, in write order: the segment header, then one frame per
    // step. Steps are read one at a time and dropped once checked, so the
    // guest never holds more than one step's selection payloads and index
    // proofs — a window's worth in one heap is how a 106 MB `List<MergeBucket>`
    // once exhausted a guest.
    let header: StorageSegmentHeader = frame::read();
    let window = window_id(&header.init);
    let (records_before, columns_before) = (header.records_before, header.columns_before);
    let entries = segment::verify_segment(header, frame::read);
    let journal = segment::segment_journal(window, records_before, columns_before, &entries);
    env::commit_slice(&journal.encode());
}
