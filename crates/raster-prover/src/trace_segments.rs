//! Host driver for the trace guest (`transition-split`).
//!
//! The trace guest binds a fraud window to the disputed commitment, appends
//! each step's record hash to the trace tree, and applies the fraud verdict at
//! every step. It proves a segment of steps per execution and commits a
//! `PartSegmentJournal` whose output is the window binding. Segments depend on
//! no other receipt: each starts from the trace frontier the record hashes
//! before it lead to, which the host computes, so every segment can be proven
//! at once.

use raster_core::trace::StepRecord;
use raster_core::fraud_window::{window_id, Chain};
use raster_core::trace_check::TraceSegmentHeader;
use raster_core::merkle::NodeHash;
use raster_core::transition::{FingerprintSliceWitness, InitTransition, TraceCommitmentHeader};

use crate::segments::{SegmentLength, ZkvmSegmentPo2};
use crate::trace::{
    serializable_frontier_from_trace_frontier, serializable_frontier_into_trace_frontier, Bytes,
    BytesHashable,
};
use crate::transition::{write_frame, write_postcard};
use crate::window::{prove_part_segment, ProvenPartSegment};
use crate::{TRACE_GUEST_ELF, TRACE_GUEST_ID};

/// The trace guest's image id, as the byte encoding journals carry.
pub fn trace_guest_image_id() -> Vec<u8> {
    TRACE_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// One segment as the host writes it: the header frame, then one frame per
/// step holding its record hash.
#[derive(Clone)]
pub struct TraceSegment {
    pub header: TraceSegmentHeader,
    pub record_hashes: Vec<Vec<u8>>,
}

/// Prove the trace check for one segment: the header frame, then one frame
/// per step holding its record hash.
pub fn prove_trace_segment(
    segment: &TraceSegment,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> ProvenPartSegment {
    let mut builder = zkvm_segment_po2.env_builder();
    write_postcard(&mut builder, &segment.header);
    for hash in &segment.record_hashes {
        write_frame(&mut builder, hash);
    }
    let first = segment.header.start_frontier.position;
    prove_part_segment(
        TRACE_GUEST_ELF,
        builder.build().unwrap(),
        format_args!("trace guest refused the segment starting at trace position {first}"),
    )
}

/// What every segment of a window shares: the window and its binding to the
/// commitment. Every segment re-binds it, so each journal stands alone.
pub struct TraceWindow<'a> {
    pub init: &'a InitTransition,
    pub commitment_header: &'a TraceCommitmentHeader,
    pub slice_witness: &'a FingerprintSliceWitness,
    pub revealed_tail_roots: &'a [NodeHash],
}

/// Split a window into trace segments, each starting from the frontier the
/// record hashes before it lead to.
pub fn plan_trace_segments(
    window: &TraceWindow<'_>,
    records: &[StepRecord],
    segments: SegmentLength,
) -> Vec<TraceSegment> {
    let mut frontier = serializable_frontier_into_trace_frontier(window.init.init_frontier.clone())
        .expect("the window's initial frontier must deserialize");
    let mut record_chain = Chain::records(&window_id(window.init));
    records
        .chunks(segments.len_for(records.len()))
        .map(|chunk| {
            let start_frontier = serializable_frontier_from_trace_frontier(frontier.clone());
            let records_before = record_chain.value();
            let record_hashes: Vec<Vec<u8>> = chunk.iter().map(StepRecord::hash).collect();
            for hash in &record_hashes {
                frontier.append(Bytes(hash.clone()));
                record_chain.fold(hash);
            }
            TraceSegment {
                header: TraceSegmentHeader {
                    init: window.init.clone(),
                    commitment_header: window.commitment_header.clone(),
                    slice_witness: window.slice_witness.clone(),
                    revealed_tail_roots: Some(window.revealed_tail_roots.to_vec()),
                    records_before,
                    start_frontier,
                    step_count: chunk.len() as u64,
                },
                record_hashes,
            }
        })
        .collect()
}
