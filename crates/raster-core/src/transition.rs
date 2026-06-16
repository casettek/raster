//! Shared types for trace state transitions (guest and host).
//!
//! These types are used by both the RISC0 transition guest and the prover host.
//! They live in raster-core to avoid circular dependencies (guest cannot depend on prover).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::vec::Vec;

use crate::authorization::AuthorizationJournal;
use crate::cfs::CfsCoordinates;
use crate::draft::{DraftId, DraftTransitionWitness, TileReplayJournal, TrackedDraftState};
use crate::fingerprint::{Fingerprint, FingerprintAccumulator};
use crate::trace::{ExternalInput, FnInput, StepRecord};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreEntry {
    pub coordinates: CfsCoordinates,
    pub object_commitment: Vec<u8>,
}

impl InternalStoreEntry {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreIndexValue {
    pub log_position: u64,
    pub object_commitment: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexMembershipProof {
    pub coordinates: CfsCoordinates,
    pub value: InternalStoreIndexValue,
    pub siblings: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexNonMembershipProof {
    pub coordinates: CfsCoordinates,
    pub siblings: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreLogWitness {
    pub position: u64,
    pub path_elems: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreReadWitness {
    pub entry: InternalStoreEntry,
    pub log_witness: InternalStoreLogWitness,
    pub index_witness: CoordinateIndexMembershipProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreWriteWitness {
    pub entry: InternalStoreEntry,
    pub index_non_membership_witness: CoordinateIndexNonMembershipProof,
    pub index_membership_witness: CoordinateIndexMembershipProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalStoreWitness {
    pub reads: Vec<InternalStoreReadWitness>,
    pub write: Option<InternalStoreWriteWitness>,
}

/// Input for a single transition step (passed into the transition guest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    pub step_record: StepRecord,
    pub replay_image_id: Option<Vec<u8>>,
    pub replay_journal: Option<TileReplayJournal>,

    pub input_witness: Option<Vec<u8>>,
    pub output_witness: Option<Vec<u8>>,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub external_input: ExternalInput,
    pub internal_store_witness: Option<InternalStoreWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,

    pub input_sources_witnesses: HashMap<StepRecord, Vec<u8>>,

    pub authorization_image_id: Vec<u8>,
    pub authorization_journal: AuthorizationJournal,
}

/// Result of applying one transition (new frontier and fingerprint state).
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    pub frontier: SerializableFrontier,
    pub internal_store_frontier: SerializableFrontier,
    pub internal_store_root: Vec<u8>,
    pub internal_store_index_root: Vec<u8>,
    pub active_drafts: BTreeMap<DraftId, TrackedDraftState>,
    pub actual_fingerprint_acc: FingerprintAccumulator,
    pub next_expected_coordinates: Vec<CfsCoordinates>,
}

/// Initial transition (first step in a window).
#[derive(Clone, Serialize, Deserialize)]
pub struct InitTransition {
    pub init_frontier: SerializableFrontier,
    pub init_internal_store_frontier: SerializableFrontier,
    pub init_internal_store_root: Vec<u8>,
    pub init_internal_store_index_root: Vec<u8>,
    pub active_drafts: BTreeMap<DraftId, TrackedDraftState>,
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
    pub transition_image_id: Vec<u8>,
    pub authorization_image_id: Vec<u8>,
    pub manifest_commitment: Vec<u8>,
}
