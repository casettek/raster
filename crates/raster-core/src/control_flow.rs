//! The control-flow guest's interface, shared by the host and both guests.
//!
//! The control-flow guest proves one transition step's CFS conformance: does
//! the record sit where the program's control flow schema says it can, and do
//! its arguments come from where the schema says they do? The transition guest
//! verifies its receipt and checks the journal field by field against its own
//! state. See `docs/proposals/control-flow-prover.md` step 6.

use serde::{Deserialize, Serialize};
use std::vec::Vec;

use crate::cfs::{CfsCoordinates, CfsCursor};
use crate::trace::{FnInput, StepKind, StepRecord};
use crate::transition::StepRecordWitness;

/// Everything the control-flow check reads for one step.
///
/// Every field is either state the transition guest also holds — the record,
/// the trace position and root before the step is appended, the coordinates
/// the previous step allowed — or a witness the check anchors to one of those.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowInput {
    pub step_record: StepRecord,
    /// The step's trace index: the trace frontier's position before the step
    /// is appended.
    pub trace_position: u64,
    /// The root of the trace prefix the step is appended to.
    #[serde(with = "serde_bytes")]
    pub trace_root_before: Vec<u8>,
    /// The coordinates the previous step allows. `None` on a window's first
    /// step, whose predecessor is outside the window.
    pub expected_before: Option<Vec<CfsCoordinates>>,
    /// Anchored to `step_record.input_source_commitment`.
    pub input_source_witness: Option<FnInput>,
    /// The caller's arguments, for a step that binds a sequence-scope input.
    /// Anchored to the scope parent's recorded `input_source_commitment`.
    pub sequence_scope_witness: Option<FnInput>,
    /// The `SequenceStart` that opened the step's frame, for a step that binds
    /// a sequence-scope input. Anchored to `trace_root_before` by its
    /// inclusion witness.
    pub sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>,
}

/// What the control-flow guest commits.
///
/// Every input the transition guest also holds is here, directly or as a hash,
/// so the transition guest can equate each with its own value. The scope
/// witness and its parent are not: only this guest reads them, and it checked
/// them against `trace_root_before`, which is here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFlowJournal {
    /// `sha256(domain || program.bin)` of the frame this guest verified against.
    pub program_commitment: Vec<u8>,
    /// `hash_trace_item(step_record)`: the same hash the trace leaf uses.
    pub step_record_hash: Vec<u8>,
    /// `input_source_commitment(w)` of the input source witness the checks ran
    /// on, computed in this guest. `None` exactly when the step carries none.
    ///
    /// Implied by the record, since both guests anchor the witness to
    /// `step_record.input_source_commitment`. It is committed so that "both
    /// guests used the same arguments" is one equality in the transition guest
    /// rather than a conclusion drawn from two checks.
    pub input_source_commitment: Option<Vec<u8>>,
    pub trace_position: u64,
    pub trace_root_before: Vec<u8>,
    pub expected_before: Option<Vec<CfsCoordinates>>,
    /// The coordinates the next step may take. Empty after `ProgramEnd`.
    pub expected_after: Vec<CfsCoordinates>,
}

/// The successor set the control-flow check commits for an honest step:
/// nothing after `ProgramEnd`, otherwise what the CFS declares.
///
/// For the host, which has to supply each step's `expected_before` before any
/// step is proven. It does not check anything: the guest computes the same set
/// itself, and a host that disagrees is refused at the next step's equality.
pub fn expected_successors(cfs: &CfsCursor, record: &StepRecord) -> Option<Vec<CfsCoordinates>> {
    if matches!(record.kind, StepKind::ProgramEnd(_)) {
        return Some(Vec::new());
    }
    cfs.try_get_next_coordinates(record.coordinates())
}
