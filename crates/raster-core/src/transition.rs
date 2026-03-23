//! Shared types for trace state transitions (guest and host).
//!
//! These types are used by both the RISC0 transition guest and the prover host.
//! They live in raster-core to avoid circular dependencies (guest cannot depend on prover).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::fingerprint::{Fingerprint, FingerprintAccumulator};
use crate::trace::StepRecord;

/// Serializable representation of a Merkle frontier (position, leaf, ommers).
///
/// Conversion to/from bridgetree's `NonEmptyFrontier` is implemented in the
/// transition guest and in raster-prover's trace module, which have bridgetree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerializableFrontier {
    pub position: u64,
    pub leaf: Vec<u8>,
    pub ommers: Vec<Vec<u8>>,
}

impl SerializableFrontier {
    /// Serialize to bytes using postcard.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    /// Deserialize from bytes using postcard.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecordWitness {
    pub position: u64,
    pub path_elems: Vec<Vec<u8>>,
}

/// Input for a single transition step (passed into the transition guest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    pub step_record: StepRecord,
    pub replay_image_id: Option<Vec<u8>>,
    pub witness: HashMap<StepRecord, Vec<u8>>,
}

/// Result of applying one transition (new frontier and fingerprint state).
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    pub frontier: SerializableFrontier,
    pub actual_fingerprint_acc: FingerprintAccumulator,
    pub next_expected_coordinates: Vec<CfsCoordinates>,
}

/// Initial transition (first step in a window).
#[derive(Clone, Serialize, Deserialize)]
pub struct InitTransition {
    pub init_frontier: SerializableFrontier,
    pub fingerprint: Fingerprint,
}

/// Current state of the transition state machine.
#[derive(Clone, Serialize, Deserialize)]
pub enum TransitionState {
    Init(InitTransition),
    Next(Transition),
    Finished,
}

/// Journal produced by the transition guest (init state + current state + image id).
#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionJournal {
    pub init_state: InitTransition,
    pub current_state: TransitionState,
    pub self_image_id: Vec<u8>,
}
