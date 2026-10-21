//! The control-flow check, as the transition guest consumes it: the
//! control-flow guest proves the CFS conformance of a segment of steps and
//! commits a root over one entry per step; this verifies that receipt, checks
//! this step's entry against the root, and holds every field of it the
//! transition guest also holds to its own value. The segment may be
//! this step alone or the whole window; nothing here depends on which. See
//! `docs/proposals/control-flow-prover.md` §Segments.

use risc0_zkvm::guest::env;

use raster_core::cfs::CfsCoordinates;
use raster_core::control_flow::{ControlFlowEntry, ControlFlowEvidence};
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

/// Verify the control-flow receipt covering this step and return the
/// coordinates the next step may take.
///
/// The `env::verify` is unconditional: every step carries evidence, so no step
/// skips the control-flow check by omitting it. Every step verifies the
/// receipt, its entry's path and its entry's fields itself, even when several
/// steps share one segment: each transition step is its own execution, handed
/// its evidence by the host, so a check another step made says nothing about
/// this one.
pub fn verify_control_flow(
    control_flow_image_id: &[u8],
    evidence: &ControlFlowEvidence,
    facts: &ControlFlowFacts<'_>,
) -> Vec<CfsCoordinates> {
    let image_id = risc0_zkvm::sha::Digest::try_from(control_flow_image_id)
        .expect("control-flow image id must be 32 bytes");
    env::verify(image_id, evidence.journal.encode().as_slice())
        .expect("Failed to verify control-flow journal");
    assert_evidence_matches(evidence, facts).expected_after.clone()
}

/// Check that the evidence's entry is the segment's entry for this step, then
/// hold each field to the transition guest's own value. Returns the entry.
///
/// Each equality covers one input both guests hold, so a receipt for some
/// other step, program, trace prefix or argument list is refused. The scope
/// witness and scope parent are absent by design: only the control-flow guest
/// reads them, and it checked them against `trace_root_before`, which is here.
pub fn assert_evidence_matches<'e>(
    evidence: &'e ControlFlowEvidence,
    facts: &ControlFlowFacts<'_>,
) -> &'e ControlFlowEntry {
    assert_eq!(
        evidence.journal.program_commitment, facts.program_commitment,
        "control-flow and transition checks ran on different programs",
    );
    assert!(
        evidence.journal.index_of(facts.trace_position).is_some(),
        "control-flow segment does not cover trace position {}",
        facts.trace_position,
    );
    assert!(
        evidence.entry_is_committed_at(facts.trace_position),
        "control-flow entry for trace position {} is not the one the segment committed",
        facts.trace_position,
    );
    let entry = &evidence.entry;
    assert_eq!(
        entry.step_record_hash,
        hash_trace_item(facts.step_record),
        "control-flow and transition checks ran on different step records",
    );
    assert_eq!(
        entry.input_source_commitment,
        facts.input_source_witness.map(input_source_commitment),
        "control-flow and transition checks ran on different step arguments",
    );
    assert_eq!(
        entry.trace_position, facts.trace_position,
        "control-flow check ran at a different trace position",
    );
    assert_eq!(
        entry.trace_root_before, facts.trace_root_before,
        "control-flow check ran against a different trace prefix",
    );
    assert_eq!(
        entry.expected_before.as_ref(),
        facts.expected_before,
        "control-flow check started from different expected coordinates",
    );
    entry
}
