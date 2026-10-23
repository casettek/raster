//! The control-flow guest's interface, shared by the host and the guest.
//!
//! The control-flow guest proves the CFS conformance of a **segment** of
//! consecutive window steps: does each record sit where the program's control
//! flow schema says it can, and do its arguments come from where the schema
//! says they do? It derives one [`ControlFlowEntry`] per step and commits the
//! segment's record chain and state before and after
//! ([`crate::fraud_window::PartSegmentJournal`]), which FRAUD WINDOW stitches
//! to its neighbours'.
//!
//! A segment of one step is the per-step mode; a segment covering the window is
//! the whole-window mode. Guest and journal are the same in every mode — the
//! segment length is the host's choice. See
//! `docs/proposals/control-flow-prover.md` §Segments.

use serde::{Deserialize, Serialize};
use std::vec::Vec;

use crate::cfs::{CfsCoordinates, CfsCursor};
use crate::input::Hash32;
use crate::trace::{FnInput, StepKind, StepRecord};
use crate::transition::{InitTransition, SerializableFrontier, StepRecordWitness};

/// One step of a segment, as the host supplies it: the record and the witnesses
/// the check anchors. The trace facts are not here — the guest derives them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowStep {
    pub step_record: StepRecord,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>,
}

/// What the control-flow guest reads: consecutive steps, and the trace state
/// before the first of them.
///
/// The guest derives every later step's trace position, trace root and
/// expected coordinates itself, by appending each record's hash and carrying
/// each step's successors forward. Only the start is host-supplied: the opening
/// segment checks it against the window, and every other segment's start must
/// stitch to the state the segment before it committed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowSegment {
    /// The window. The segment that opens it must start from its initial
    /// frontier with no expected coordinates; every segment commits its id.
    pub init: InitTransition,
    /// The record chain before the segment's first step.
    pub records_before: Hash32,
    /// The trace frontier before the segment's first step.
    pub start_frontier: SerializableFrontier,
    /// The coordinates the step before the segment allows. `None` when the
    /// segment opens the window.
    pub expected_before: Option<Vec<CfsCoordinates>>,
    pub steps: Vec<ControlFlowStep>,
}

/// Everything the control-flow check reads for one step. The guest assembles one
/// per step of a [`ControlFlowSegment`]: the record and witnesses from the
/// [`ControlFlowStep`], the trace facts from its own walk.
///
/// Every field is either part of the segment's state — the record, the trace
/// position and root before the step is appended, the coordinates the previous
/// step allowed — or a witness the check anchors to one of those.
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

/// What the control-flow check derives for one step: the facts the segment's
/// journal is folded from — its record chain, and its state before and after
/// (trace position and root, expected coordinates).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFlowEntry {
    /// `hash_trace_item(step_record)`: the same hash the trace leaf uses.
    #[serde(with = "serde_bytes")]
    pub step_record_hash: Vec<u8>,
    /// `input_source_commitment(w)` of the input source witness the checks ran
    /// on. `None` exactly when the step carries none.
    #[serde(with = "serde_bytes")]
    pub input_source_commitment: Option<Vec<u8>>,
    pub trace_position: u64,
    #[serde(with = "serde_bytes")]
    pub trace_root_before: Vec<u8>,
    pub expected_before: Option<Vec<CfsCoordinates>>,
    /// The coordinates the next step may take. Empty after `ProgramEnd`.
    pub expected_after: Vec<CfsCoordinates>,
}

/// The successor set the control-flow check commits for an honest step:
/// nothing after `ProgramEnd`, otherwise what the CFS declares.
///
/// For the host, which has to supply each segment's `expected_before` before
/// any step is proven. It does not check anything: the guest computes the same
/// set itself, and a segment started from another set doesn't stitch to the
/// one before it.
pub fn expected_successors(cfs: &CfsCursor, record: &StepRecord) -> Option<Vec<CfsCoordinates>> {
    if matches!(record.kind, StepKind::ProgramEnd(_)) {
        return Some(Vec::new());
    }
    cfs.try_get_next_coordinates(record.coordinates())
}
