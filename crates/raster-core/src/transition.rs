//! Shared types for trace state transitions (guest and host).
//!
//! These types are used by both the RISC0 transition guest and the prover host.
//! They live in raster-core to avoid circular dependencies (guest cannot depend on prover).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::string::String;
use std::vec::Vec;

use crate::authorization::AuthorizationJournal;
use crate::cfs::CfsCoordinates;
use crate::draft::{DraftId, DraftTransitionWitness, TileReplayJournal, TrackedDraftState};
use crate::fingerprint::{Fingerprint, FingerprintAccumulator};
use crate::input::SelectionWitness;
use crate::trace::{FnInput, StepRecord};

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
    pub internal_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub internal_store_witness: Option<InternalStoreWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,

    pub input_sources_witnesses: HashMap<StepRecord, Vec<u8>>,

    pub authorization_image_id: Vec<u8>,
    pub authorization_journal: AuthorizationJournal,

    /// Membership witness for `main`'s entry-argument coordinate (`[0]`)
    /// against the window's *initial* internal-store state, proving the
    /// binding already existed when this window opened.
    ///
    /// Only ever read on the step that establishes a fresh
    /// `TransitionState::Init`; `Next` steps inherit the fact from the
    /// previous journal instead. `None` at `Init` is not a failure: it means
    /// the window opens at or before the binding itself, leaving the chain
    /// [`EntrypointAuthorization::Pending`] until an `Entrypoint` step
    /// inside the window discharges it.
    pub entrypoint_membership_witness: Option<InternalStoreReadWitness>,
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

/// Whether `main`'s entry-argument binding has been tied to the authorization
/// journal within a proof chain.
///
/// A window may legitimately open *before* the binding exists (its trace
/// starts at genesis, with an empty internal store, and replays the
/// `Entrypoint` step itself) or *after* it (the binding is already in the
/// window's initial internal-store state). Those two cases establish the
/// same fact by different means, so the chain carries this as a state rather
/// than a bool: `Pending` is a debt that a later step in the same chain must
/// discharge, and [`EntrypointAuthorization::assert_discharged`] is what
/// makes it enforceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntrypointAuthorization {
    /// The CFS declares no `main` entry arguments — there is nothing to
    /// bind, and no `Entrypoint` step may appear at all.
    NotRequired,
    /// The window opened before the binding existed: no trace-inclusion
    /// witness could exist at `Init`, so an `Entrypoint` step inside this
    /// chain must still establish the binding before the chain finishes.
    Pending,
    /// The binding has been tied to the authorization journal in this chain
    /// — either by a trace-inclusion witness at `Init`, or by an
    /// `Entrypoint` step verified within the chain.
    Established,
}

impl EntrypointAuthorization {
    /// A chain must not finish while it still owes an entry-argument
    /// binding: without this, `Pending` would be indistinguishable from
    /// `Established` to any consumer of the final journal, and a trace that
    /// never binds its entry arguments at all would verify.
    pub fn assert_discharged(&self) {
        assert!(
            *self != EntrypointAuthorization::Pending,
            "Fraud proof chain finished while main's entry-argument binding was never authorized",
        );
    }
}

/// Journal produced by the transition guest (init state + current state + image id).
#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionJournal {
    pub init_state: InitTransition,
    pub current_state: TransitionState,
    pub transition_image_id: Vec<u8>,
    pub authorization_image_id: Vec<u8>,
    pub manifest_commitment: Vec<u8>,

    /// How far this chain has got in tying `main`'s entry-argument binding
    /// to the authorization journal — established at `Init` from a
    /// trace-inclusion witness, or by an `Entrypoint` step replayed inside
    /// the chain, and inherited by every `Next` step from the previous
    /// (recursively verified) journal. See [`EntrypointAuthorization`].
    pub entrypoint_authorization: EntrypointAuthorization,
}
