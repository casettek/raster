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
pub struct StorageEntry {
    pub coordinates: CfsCoordinates,
    pub object_commitment: Vec<u8>,
}

impl StorageEntry {
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageIndexValue {
    pub log_position: u64,
    pub object_commitment: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexMembershipProof {
    pub coordinates: CfsCoordinates,
    pub value: StorageIndexValue,
    pub siblings: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexNonMembershipProof {
    pub coordinates: CfsCoordinates,
    pub siblings: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageLogWitness {
    pub position: u64,
    pub path_elems: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageReadWitness {
    pub entry: StorageEntry,
    pub log_witness: StorageLogWitness,
    pub index_witness: CoordinateIndexMembershipProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageWriteWitness {
    pub entry: StorageEntry,
    pub index_non_membership_witness: CoordinateIndexNonMembershipProof,
    pub index_membership_witness: CoordinateIndexMembershipProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageWitness {
    pub reads: Vec<StorageReadWitness>,
    pub write: Option<StorageWriteWitness>,
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
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<StorageWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,

    pub input_sources_witnesses: HashMap<StepRecord, Vec<u8>>,

    pub authorization_image_id: Vec<u8>,
    pub authorization_journal: AuthorizationJournal,

    /// Membership witness for `main`'s entry-argument coordinate (`[]`)
    /// against the window's *initial* storage state, proving the binding
    /// already existed when this window opened.
    ///
    /// Only ever read on the step that establishes a fresh
    /// `TransitionState::Init`; `Next` steps inherit the fact from the
    /// previous journal instead. `None` at `Init` means the window opens at
    /// genesis, so its first step is the `ProgramStart` that binds and
    /// authorizes the entry arguments itself.
    pub entrypoint_membership_witness: Option<StorageReadWitness>,

    /// For a `ProgramEnd` step: the trace-inclusion proof that the program
    /// output object (`ProgramEndStep::output`) lives in the current storage
    /// state, and the selection proof narrowing it to the returned value.
    /// Both `None` for every other step and for a unit program's end.
    pub program_output_read_witness: Option<StorageReadWitness>,
    pub program_output_selection_witness: Option<SelectionWitness>,
}

/// Result of applying one transition (new frontier and fingerprint state).
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    pub frontier: SerializableFrontier,
    pub storage_frontier: SerializableFrontier,
    pub storage_root: Vec<u8>,
    pub storage_index_root: Vec<u8>,
    pub active_drafts: BTreeMap<DraftId, TrackedDraftState>,
    pub actual_fingerprint_acc: FingerprintAccumulator,
    pub next_expected_coordinates: Vec<CfsCoordinates>,
}

/// Initial transition (first step in a window).
#[derive(Clone, Serialize, Deserialize)]
pub struct InitTransition {
    pub init_frontier: SerializableFrontier,
    pub init_storage_frontier: SerializableFrontier,
    pub init_storage_root: Vec<u8>,
    pub init_storage_index_root: Vec<u8>,
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
/// Because the binding is now a single `ProgramStart` step at coordinates
/// `[]` — always the trace's first step — a window can establish the fact one
/// of two ways, both decided when the window opens (see the transition
/// guest's `checks::entrypoint::verify_genesis_authorization`):
///
/// - the window opens at genesis, so its first step *is* `ProgramStart`,
///   verified against the journal in the same guest run; or
/// - the window opens later, so a trace-inclusion witness proves the binding
///   is already at `[]` in the window's initial storage state.
///
/// Either way authorization is `Established` before any later step runs, so
/// there is no deferred debt and no `Finished`-time discharge to enforce.
/// The state is carried on the journal so every `Next` step inherits the
/// established fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntrypointAuthorization {
    /// The CFS declares no `main` entry arguments — there is nothing to bind.
    NotRequired,
    /// The binding has been tied to the authorization journal in this chain.
    Established,
}

/// Whether the program's authorized output has been tied to committed storage
/// within a proof chain.
///
/// The output is bound by the trace's *last* step, `ProgramEnd`, which proves
/// the returned value is a selection out of a committed storage object (see
/// the transition guest's `checks::program::verify_program_end`). Because it
/// is the last step, no window can open *after* it, so — unlike
/// [`EntrypointAuthorization`] — there is no membership-witness route and no
/// genesis case: a chain is `Pending` until it verifies the `ProgramEnd` step,
/// then `Established`.
///
/// This is not discharged at `Finished`: a fraud chain legitimately concludes
/// at a mid-trace divergence, before any output exists, and must stay
/// `Pending`. The invariant is enforced elsewhere — `ProgramEnd` is the unique
/// terminal step bound into the fingerprint, host-side full-trace verification
/// requires it, and any consumer accepting a completed-program journal requires
/// `Established`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputAuthorization {
    /// The CFS declares `main` returns no value — there is no output to bind.
    NotRequired,
    /// The chain has not yet reached (and verified) the program's end.
    Pending,
    /// A `ProgramEnd` step has been verified in this chain: the committed
    /// output provably lives in committed storage.
    Established,
}

/// Journal produced by the transition guest (init state + current state + image id).
#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionJournal {
    pub init_state: InitTransition,
    pub current_state: TransitionState,
    pub transition_image_id: Vec<u8>,
    pub authorization_image_id: Vec<u8>,
    pub manifest_commitment: Vec<u8>,

    /// Whether this chain has tied `main`'s entry-argument binding to the
    /// authorization journal — established when the window opens (from a
    /// trace-inclusion witness, or by the `ProgramStart` step at genesis),
    /// and inherited by every `Next` step from the previous (recursively
    /// verified) journal. See [`EntrypointAuthorization`].
    pub entrypoint_authorization: EntrypointAuthorization,

    /// Whether this chain has tied the program's output to committed storage —
    /// `NotRequired` when `main` returns unit, `Pending` until a `ProgramEnd`
    /// step is verified, `Established` after. Inherited across `Next` steps.
    /// See [`OutputAuthorization`].
    pub output_authorization: OutputAuthorization,
}
