//! RISC0 guest program for the control flow of a segment of transition steps.
//!
//! Proves that each step record sits where the program's control flow schema
//! says it can, and that its arguments come from where the schema says they
//! do. A segment of one step is the per-step mode; a segment covering the
//! window is the whole-window mode. The journal commits a Merkle root over one
//! entry per step; each transition step verifies this receipt, checks its own
//! entry against the root, and equates the entry with its own state. See
//! `docs/proposals/control-flow-prover.md` §Segments.

mod checks;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use raster_core::cfs::CfsCursor;
use raster_core::control_flow::{entries_root, ControlFlowJournal, ControlFlowSegment};
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

    let entries = checks::verify_segment(&cfs, segment);

    // Commit a root over the entries, not the entries: each transition step is
    // handed its own entry with a path, so its work doesn't grow with the
    // segment. The entries go to the host on stdout, untrusted — the host can
    // only use them if they fold to the committed root.
    let leaves: Vec<_> = entries.iter().map(|entry| entry.leaf_hash()).collect();
    let journal = ControlFlowJournal {
        program_commitment,
        first_position: entries[0].trace_position,
        entry_count: entries.len() as u64,
        entries_root: entries_root(&leaves),
    };
    env::commit_slice(&journal.encode());
    env::write_slice(&postcard::to_allocvec(&entries).expect("entries always encode"));
}
