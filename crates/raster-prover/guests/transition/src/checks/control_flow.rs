//! The control-flow check, as the transition guest consumes it: the
//! control-flow guest proves one step's CFS conformance, and this verifies
//! that receipt and holds every journal field the transition guest also holds
//! to its own value. See `docs/proposals/control-flow-prover.md` step 6.

use risc0_zkvm::guest::env;

use raster_core::cfs::CfsCoordinates;
use raster_core::control_flow::ControlFlowJournal;
use raster_core::trace::{FnInput, StepRecord};

use crate::checks::io::input_source_commitment;
use crate::merkle_tree::hash_trace_item;

/// What the transition guest holds about the step the control-flow guest
/// checked: its own copy of every input both guests read.
pub struct ControlFlowFacts<'a> {
    pub program_commitment: &'a [u8],
    pub step_record: &'a StepRecord,
    pub input_source_witness: Option<&'a FnInput>,
    /// The trace frontier's position before this step is appended.
    pub trace_position: u64,
    /// The trace frontier's root before this step is appended.
    pub trace_root_before: &'a [u8],
    pub expected_before: Option<&'a Vec<CfsCoordinates>>,
}

/// Verify the control-flow receipt for this step and return the coordinates
/// the next step may take.
///
/// The `env::verify` is unconditional: every step carries a journal, so no step
/// skips the control-flow check by omitting one.
pub fn verify_control_flow(
    control_flow_image_id: &[u8],
    journal: &ControlFlowJournal,
    facts: &ControlFlowFacts<'_>,
) -> Vec<CfsCoordinates> {
    let image_id = risc0_zkvm::sha::Digest::try_from(control_flow_image_id)
        .expect("control-flow image id must be 32 bytes");
    env::verify(image_id, &risc0_zkvm::serde::to_vec(journal).unwrap())
        .expect("Failed to verify control-flow journal");
    assert_journal_matches(journal, facts);
    journal.expected_after.clone()
}

/// Hold each journal field to the transition guest's own value.
///
/// Each equality covers one input both guests hold, so a receipt for some
/// other step, program, trace prefix or argument list is refused. The scope
/// witness and scope parent are absent by design: only the control-flow guest
/// reads them, and it checked them against `trace_root_before`, which is here.
pub fn assert_journal_matches(journal: &ControlFlowJournal, facts: &ControlFlowFacts<'_>) {
    assert_eq!(
        journal.program_commitment, facts.program_commitment,
        "control-flow and transition checks ran on different programs",
    );
    assert_eq!(
        journal.step_record_hash,
        hash_trace_item(facts.step_record),
        "control-flow and transition checks ran on different step records",
    );
    assert_eq!(
        journal.input_source_commitment,
        facts.input_source_witness.map(input_source_commitment),
        "control-flow and transition checks ran on different step arguments",
    );
    assert_eq!(
        journal.trace_position, facts.trace_position,
        "control-flow check ran at a different trace position",
    );
    assert_eq!(
        journal.trace_root_before, facts.trace_root_before,
        "control-flow check ran against a different trace prefix",
    );
    assert_eq!(
        journal.expected_before.as_ref(),
        facts.expected_before,
        "control-flow check started from different expected coordinates",
    );
}
