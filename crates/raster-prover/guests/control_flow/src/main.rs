//! RISC0 guest program for the control flow of a segment of fraud-window steps.
//!
//! Proves that each step record sits where the program's control flow schema
//! says it can, and that its arguments come from where the schema says they
//! do. A segment of one step is the per-step mode; a segment covering the
//! window is the whole-window mode. The journal (`PartSegmentJournal`) commits
//! the record chain and the part's state before and after, which FRAUD WINDOW
//! stitches to its neighbours'. See
//! `docs/proposals/control-flow-prover.md` §Segments and `transition-split.md`.

mod checks;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::cfs::CfsCursor;
use raster_core::control_flow::ControlFlowSegment;
use raster_core::fraud_window::window_id;
use raster_core::program::{commitment_of_bytes, ProgramDefinition};

fn main() {
    // Host inputs, in write order: the program definition frame, then the
    // segment. The frame is hashed before decoding, so the committed
    // `program_commitment` is bound to the exact bytes checked against.
    let program_bytes = frame::read_bytes();
    let program_commitment = commitment_of_bytes(&program_bytes).to_vec();
    let program = ProgramDefinition::decode(&program_bytes)
        .expect("host must supply a valid ProgramDefinition frame");
    let cfs = CfsCursor::new(program.cfs);
    let segment: ControlFlowSegment = frame::read();
    let window = window_id(&segment.init);
    let records_before = segment.records_before;

    let (entries, end_root) = checks::verify_segment_to_end(&cfs, segment);
    let journal =
        checks::segment_journal(window, program_commitment, records_before, &entries, &end_root);
    env::commit_slice(&journal.encode());
}
