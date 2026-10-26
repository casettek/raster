//! RISC0 guest program for the trace of a segment of fraud-window steps
//! (`docs/proposals/transition-split.md`).
//!
//! Binds the window to the disputed `commit.bin`, appends each step's record
//! hash to the trace tree, and applies the fraud verdict at every step: match
//! the committed fingerprint, or, at the window's last index, diverge from it.
//! Commits a `PartSegmentJournal`: the record chain and trace state across the
//! segment, and the window binding as its output.

mod segment;
mod window;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::fraud_window::window_id;
use raster_core::trace_check::TraceSegmentHeader;

fn main() {
    // Host inputs, in write order: the segment header, then one frame per step
    // holding that step's record hash.
    let header: TraceSegmentHeader = frame::read();
    let window = window_id(&header.init);
    let records_before = header.records_before;
    let (binding, entries) = segment::verify_segment(header, frame::read_bytes);
    let journal = segment::segment_journal(window, records_before, binding, &entries);
    env::commit_slice(&journal.encode());
}
