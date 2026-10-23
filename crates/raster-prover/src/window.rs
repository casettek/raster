//! A fraud window's parts planned and proven as independent jobs
//! (`docs/proposals/transition-split.md`).
//!
//! Every segment of the trace, control-flow and storage parts depends only on
//! the recorded window: none waits for another's receipt. So the host plans
//! them all first and hands one job per segment to `schedule::run_jobs`, next
//! to the tile replays, the authorization and the TRANSITION part's segments
//! (`fraud_window`). Every job yields a [`ProvenPartSegment`]; FRAUD WINDOW
//! takes them in any order, and refuses a window any of them is missing from.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Arguments;

use raster_core::fingerprint::Fingerprint;
use raster_core::fraud_window::PartSegmentJournal;
use raster_core::merkle::NodeHash;
use raster_core::trace::StepRecord;
use raster_core::transition::{
    FingerprintSliceWitness, InitTransition, StepRecordWitness, TraceCommitmentHeader,
};

use crate::control_flow::{plan_control_flow_segments, prove_control_flow_segment};
use crate::schedule::Job;
use crate::segments::{SegmentLengths, ZkvmSegmentPo2};
use crate::storage::{plan_storage_segments, prove_storage_segment, StorageSegment};
use crate::trace::{
    frontier_root, serializable_frontier_into_trace_frontier, SerializableFrontier,
};
use crate::trace_segments::{plan_trace_segments, prove_trace_segment, TraceSegment, TraceWindow};
use crate::transition::StepIo;

/// Everything about a fraud window the parts are planned from.
pub struct WindowSetup<'a> {
    pub initial_frontier: &'a SerializableFrontier,
    pub initial_storage_frontier: &'a SerializableFrontier,
    pub initial_storage_index_root: &'a [u8],
    pub trace_window: &'a [StepRecord],
    pub fingerprint: Fingerprint,
    pub commitment_header: &'a TraceCommitmentHeader,
    pub fingerprint_slice: &'a FingerprintSliceWitness,
    pub revealed_tail_roots: &'a [NodeHash],
    /// The canonical `program.bin` frame.
    pub program_frame: &'a [u8],
    pub sequence_scope_parents: &'a BTreeMap<u64, (StepRecord, StepRecordWitness)>,
    pub recorded_step_io: &'a HashMap<StepRecord, StepIo>,
}

impl WindowSetup<'_> {
    /// The window's initial state: what every part's opening segment starts
    /// from, and what FRAUD WINDOW binds every segment to.
    pub fn init_transition(&self) -> InitTransition {
        let storage_frontier =
            serializable_frontier_into_trace_frontier(self.initial_storage_frontier.clone())
                .expect("storage frontier should deserialize");
        InitTransition {
            init_frontier: self.initial_frontier.clone(),
            init_storage_frontier: self.initial_storage_frontier.clone(),
            init_storage_root: frontier_root(&storage_frontier),
            init_storage_index_root: self.initial_storage_index_root.to_vec(),
            fingerprint: self.fingerprint.clone(),
        }
    }
}

/// One proven segment of any part: its receipt and the journal it commits.
pub struct ProvenPartSegment {
    pub receipt: risc0_zkvm::Receipt,
    pub journal: PartSegmentJournal,
}

/// Prove a part guest's segment and decode the journal it commits. Panics
/// with `refused` and the prover's error if the guest refuses it.
pub(crate) fn prove_part_segment(
    elf: &[u8],
    env: risc0_zkvm::ExecutorEnv<'_>,
    refused: Arguments<'_>,
) -> ProvenPartSegment {
    let receipt = risc0_zkvm::default_prover()
        .prove(env, elf)
        .unwrap_or_else(|e| panic!("{refused}: {e}"))
        .receipt;
    let journal = PartSegmentJournal::decode(&receipt.journal.bytes)
        .expect("a part guest commits a postcard-encoded PartSegmentJournal");
    ProvenPartSegment { receipt, journal }
}

/// Every segment of the trace, control-flow and storage parts, planned and
/// not yet proven.
pub struct PlannedParts {
    pub trace: Vec<TraceSegment>,
    pub control_flow: Vec<raster_core::control_flow::ControlFlowSegment>,
    pub storage: Vec<StorageSegment>,
}

pub fn plan_parts(setup: &WindowSetup<'_>, segments: SegmentLengths) -> PlannedParts {
    let init = setup.init_transition();
    PlannedParts {
        trace: plan_trace_segments(
            &TraceWindow {
                init: &init,
                commitment_header: setup.commitment_header,
                slice_witness: setup.fingerprint_slice,
                revealed_tail_roots: setup.revealed_tail_roots,
            },
            setup.trace_window,
            segments.trace,
        ),
        control_flow: plan_control_flow_segments(
            setup.program_frame,
            &init,
            setup.trace_window,
            setup.recorded_step_io,
            setup.sequence_scope_parents,
            segments.control_flow,
        ),
        storage: plan_storage_segments(
            &init,
            setup.trace_window,
            setup.recorded_step_io,
            segments.storage,
        ),
    }
}

/// One job per planned segment, for `schedule::run_jobs`.
pub fn part_jobs<'a>(
    planned: &'a PlannedParts,
    program_frame: &'a [u8],
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> Vec<Job<'a, ProvenPartSegment>> {
    let mut jobs: Vec<Job<'a, ProvenPartSegment>> = Vec::new();
    for segment in &planned.trace {
        jobs.push(Box::new(move || {
            prove_trace_segment(segment, zkvm_segment_po2)
        }));
    }
    for segment in &planned.control_flow {
        jobs.push(Box::new(move || {
            prove_control_flow_segment(program_frame, segment, zkvm_segment_po2)
        }));
    }
    for segment in &planned.storage {
        jobs.push(Box::new(move || {
            prove_storage_segment(segment, zkvm_segment_po2)
        }));
    }
    jobs
}
