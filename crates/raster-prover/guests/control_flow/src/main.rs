//! RISC0 guest program for one transition step's control flow.
//!
//! Proves that the step record sits where the program's control flow schema
//! says it can, and that its arguments come from where the schema says they
//! do. The transition guest verifies this receipt and equates every journal
//! field it also holds with its own value. See
//! `docs/proposals/control-flow-prover.md` step 6.

mod checks;

#[cfg(test)]
mod tests;

use risc0_zkvm::guest::env;

use guest_common::frame;
use guest_common::input_source::input_source_commitment;
use guest_common::merkle_tree::hash_trace_item;
use raster_core::cfs::CfsCursor;
use raster_core::control_flow::{ControlFlowInput, ControlFlowJournal};
use raster_core::program::{commitment_of_bytes, ProgramDefinition};

fn main() {
    // Host inputs, in write order: the program definition frame, then the
    // step's control-flow input. The frame is hashed before decoding, so the
    // committed `program_commitment` is bound to the exact bytes checked
    // against.
    let program_bytes = frame::read_bytes();
    let program_commitment = commitment_of_bytes(&program_bytes).to_vec();
    let program = ProgramDefinition::decode(&program_bytes)
        .expect("host must supply a valid ProgramDefinition frame");
    let cfs = CfsCursor::new(program.cfs);
    let input: ControlFlowInput = frame::read();

    let expected_after = checks::verify_step(&cfs, &input);

    env::commit(&ControlFlowJournal {
        program_commitment,
        step_record_hash: hash_trace_item(&input.step_record),
        input_source_commitment: input.input_source_witness.as_ref().map(input_source_commitment),
        trace_position: input.trace_position,
        trace_root_before: input.trace_root_before,
        expected_before: input.expected_before,
        expected_after,
    });
}
