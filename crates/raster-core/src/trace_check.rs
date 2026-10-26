//! The trace guest's interface, shared by the host and the guest
//! (`transition-split`).
//!
//! The trace guest is the part of a fraud window's proof that reads
//! `commit.bin`: it binds the window to the disputed commitment (its
//! fingerprint slice, the genesis rule for a window at trace index 0, the
//! revealed tail roots), appends each step's record hash to the trace tree,
//! and applies the verdict at every step: the step's root matches the
//! committed fingerprint item, or, at the window's last index, diverges from
//! it. It proves a **segment** of steps per execution, derives one
//! [`TraceStepEntry`] per step, and commits the segment's record chain and
//! trace state before and after, with the window binding as its output
//! ([`crate::fraud_window::PartSegmentJournal`]).

use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::input::Hash32;
use crate::merkle::NodeHash;
use crate::transition::{
    FingerprintSliceWitness, InitTransition, SerializableFrontier, TraceCommitmentHeader,
};

/// The first frame the trace guest reads. Every segment re-binds the window to
/// the commitment, so any segment's journal names the same refuted commitment
/// and needs nothing from another segment. The step frames follow: one
/// `hash_trace_item(record)` each.
#[derive(Clone, Serialize, Deserialize)]
pub struct TraceSegmentHeader {
    pub init: InitTransition,
    pub commitment_header: TraceCommitmentHeader,
    pub slice_witness: FingerprintSliceWitness,
    /// The commitment's revealed tail roots, if the host has them. Absence
    /// costs only the root-divergence route at the window's last index.
    pub revealed_tail_roots: Option<Vec<NodeHash>>,
    /// The record chain before the segment's first step.
    pub records_before: Hash32,
    /// The trace frontier before the segment's first step. It must be
    /// `init.init_frontier` when the segment opens the window; otherwise it
    /// must stitch to the state the segment before it committed.
    pub start_frontier: SerializableFrontier,
    pub step_count: u64,
}

/// What the trace check derives for one step: the facts the segment's journal
/// is folded from. The trace guest refuses a segment where the verdict fails at
/// any step, so a segment's receipt says it held at every one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceStepEntry {
    /// The hash appended: `hash_trace_item(step_record)`.
    #[serde(with = "serde_bytes")]
    pub step_record_hash: Vec<u8>,
    pub trace_position: u64,
    #[serde(with = "serde_bytes")]
    pub trace_root_before: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub trace_root_after: Vec<u8>,
}
