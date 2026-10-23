//! RISC0 guest program for the TRANSITION part of a fraud window
//! (`docs/proposals/transition-split.md`): io, recur, drafts and entrypoint
//! over a segment of steps, verifying no receipt. Commits a
//! `PartSegmentJournal` that FRAUD WINDOW stitches and compares.

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::transition::TransitionPartHeader;

use transition_guest::part::{verify_segment, PartProgram};

fn main() {
    // Host inputs, in write order: the program frame, the segment header,
    // then one `TransitionStep` frame per step.
    let program = PartProgram::from_frame(&frame::read_bytes());
    let header: TransitionPartHeader = frame::read();
    let (journal, state_after) = verify_segment(&program, header, frame::read);
    env::commit_slice(&journal.encode());
    // The state after, for the host to start the next segment from: untrusted
    // there, since that segment's state before must stitch to this digest.
    env::write_slice(&postcard::to_allocvec(&state_after).expect("a state always encodes"));
}
