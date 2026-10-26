//! Shared types for a fraud window's guests and host: the window's initial
//! state, the storage witnesses, the transition part's inputs, and the two
//! authorization stages the window's journal reports.
//!
//! They live in raster-core to avoid circular dependencies (a guest cannot
//! depend on the prover).

use serde::{Deserialize, Serialize};
use crate::hash::{Digest, Sha256};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

use crate::authorization::AuthorizationJournal;
use crate::cfs::CfsCoordinates;
use crate::draft::{DraftTransitionWitness, TileReplayJournal};
use crate::recur_progress::RecurProgressStack;
use crate::storage_check::StorageState;
use crate::fingerprint::Fingerprint;
use crate::input::SelectionWitness;
use crate::merkle::NodeHash;
use crate::trace::{FnInput, StepRecord};

/// Serializable representation of a Merkle frontier (position, leaf, ommers).
///
/// Conversion to/from bridgetree's `NonEmptyFrontier` is implemented in the
/// transition guest and in raster-prover's trace module, which have bridgetree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerializableFrontier {
    pub position: u64,
    #[serde(with = "serde_bytes")]
    pub leaf: Vec<u8>,
    pub ommers: Vec<NodeHash>,
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
    pub path_elems: Vec<NodeHash>,
}

/// Domain prefix for [`TraceCommitmentHeader::digest`].
/// Bumped to v2 when the header gained `window_size` and
/// `revealed_tail_roots_commitment`: the digest is the commitment's identity,
/// so a v1 `commit.bin` and any receipt carrying its
/// `refuted_trace_commitment` no longer match.
pub const TRACE_COMMITMENT_DOMAIN: &[u8] = b"raster/trace-commitment/v2";

/// The compact, guest-friendly identity of a `TraceCommitment`.
///
/// A full `commit.bin` scales with the trace (the packed fingerprint spans
/// every step; the revealed window is a window of full step records), so the
/// trace guest never ingests it. Instead this constant-size header
/// stands for it: the fingerprint is represented by a Merkle root over its
/// packed `u64` blocks (leaf `i` = `sha256(bits[i].to_le_bytes())`, combined
/// exactly like the trace tree), and the revealed items by their hash. The
/// guest hashes the header into `refuted_trace_commitment` and checks its
/// window is a slice of the fingerprint via a [`FingerprintSliceWitness`] —
/// O(window) data instead of O(trace). Host-side construction lives in
/// `raster-prover::trace` (`TraceCommitmentExt`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceCommitmentHeader {
    pub bits_packer: crate::fingerprint::BitPacker,
    /// Trace items the fingerprint covers (`fingerprint.len`).
    pub fingerprint_len: u64,
    /// Merkle root over the fingerprint's packed `u64` blocks.
    #[serde(with = "serde_bytes")]
    pub fingerprint_root: Vec<u8>,
    /// `sha256(postcard(revealed_items))`.
    #[serde(with = "serde_bytes")]
    pub revealed_items_commitment: Vec<u8>,
    /// The fraud-proof window size this commitment was built with —
    /// `revealed_items.len()` host-side.
    ///
    /// Here because the guest could not otherwise learn it, and without it
    /// `window_len` and `window_start` are challenger-supplied with only
    /// `window_start + window_len <= fingerprint_len` constraining them: an
    /// upper bound, not a shape. A two-item window anywhere in the trace has no
    /// margin at all, since `finalize` never compares the first item and
    /// requires the last to diverge.
    pub window_size: u64,
    /// `sha256(postcard(revealed_tail_roots))`.
    ///
    /// The roots themselves are O(window), so they travel as a witness when a
    /// tail divergence is proven; the header stays constant-size, which is its
    /// whole purpose.
    #[serde(with = "serde_bytes")]
    pub revealed_tail_roots_commitment: Vec<u8>,
}

impl TraceCommitmentHeader {
    /// The commitment identity: `sha256(domain || postcard(self))`. This is
    /// what a journal's `refuted_trace_commitment` carries. A chain checkpoint
    /// no longer names one — see `docs/proposals/chain-io-commitment.md`.
    pub fn digest(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(TRACE_COMMITMENT_DOMAIN);
        hasher.update(postcard::to_allocvec(self).expect("TraceCommitmentHeader is serializable"));
        hasher.finalize().to_vec()
    }
}

/// Inclusion proof for one packed fingerprint block against
/// [`TraceCommitmentHeader::fingerprint_root`]. Same path shape as every
/// other Merkle witness the guests fold (`position`-bit ordering,
/// level-prefixed combine).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintBlockWitness {
    pub block: u64,
    /// The block's index in the commitment's packed `bits` array.
    pub position: u64,
    pub path_elems: Vec<NodeHash>,
}

/// The contiguous run of proven fingerprint blocks covering a fraud window's
/// item range `[window_start, window_start + window_len)`. The guest derives
/// the required block range itself (from the window's frontier position and
/// length) and rejects a witness whose positions differ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintSliceWitness {
    pub blocks: Vec<FingerprintBlockWitness>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageEntry {
    pub coordinates: CfsCoordinates,
    #[serde(with = "serde_bytes")]
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
    #[serde(with = "serde_bytes")]
    pub object_commitment: Vec<u8>,
}

/// `(coordinates, value)` is in the coordinate index: the leaf for
/// `key(coordinates)` sits at depth `siblings.len()`, and folds up through
/// `siblings` to the root. See `crate::coordinate_index`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexMembershipProof {
    pub coordinates: CfsCoordinates,
    pub value: StorageIndexValue,
    /// From the leaf's level up: `siblings[0]` is the leaf's sibling,
    /// `siblings.last()` a child of the root.
    pub siblings: Vec<NodeHash>,
}

/// `coordinates` is not in the coordinate index: `terminal` is what sits at
/// depth `siblings.len()` on `key(coordinates)`'s path, and it holds no other
/// key there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinateIndexNonMembershipProof {
    pub coordinates: CfsCoordinates,
    pub terminal: IndexTerminal,
    /// From the terminal's level up, as in a membership proof.
    pub siblings: Vec<NodeHash>,
}

/// The subtree a non-membership proof stops at.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IndexTerminal {
    /// No key has this prefix.
    Empty,
    /// Exactly one key has this prefix, and it is not the one being proven
    /// absent.
    Leaf {
        key: [u8; 32],
        value: StorageIndexValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageLogWitness {
    pub position: u64,
    pub path_elems: Vec<NodeHash>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageReadWitness {
    pub entry: StorageEntry,
    pub log_witness: StorageLogWitness,
    pub index_witness: CoordinateIndexMembershipProof,
}

/// A write's proof: the entry, and that its coordinates were absent from the
/// index before. The index root after the write is not proven by a second
/// proof: the guest computes it from this one (`verify_coordinate_index_insert`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageWriteWitness {
    pub entry: StorageEntry,
    pub index_non_membership_witness: CoordinateIndexNonMembershipProof,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageWitness {
    pub reads: Vec<StorageReadWitness>,
    pub write: Option<StorageWriteWitness>,
}

/// The first frame the transition part's guest reads after the program frame
/// (`transition-split`); the segment's steps follow, one
/// [`TransitionStep`] frame each. A segment reads no receipt: FRAUD WINDOW
/// stitches its chains and state to its neighbours', and verifies the replay
/// claims and the authorization journal it read.
#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionPartHeader {
    /// The window. The segment that opens it (at `init_frontier.position`)
    /// derives its starting state from it; every segment commits its id.
    pub init: InitTransition,
    pub first_position: u64,
    pub step_count: u64,
    /// The state before the segment's first step. `None` exactly when the
    /// segment opens the window, which derives it instead.
    pub start: Option<crate::fraud_window::TransitionPartState>,
    /// The record chain, the two consumed column chains (in
    /// [`crate::fraud_window::Column::ALL`] order) and the replay-claims chain
    /// before the segment's first step.
    pub records_before: crate::input::Hash32,
    pub columns_before: [crate::input::Hash32; 2],
    pub replay_claims_before: crate::input::Hash32,
    /// The journal the entrypoint checks read; FRAUD WINDOW verifies it.
    pub authorization_journal: AuthorizationJournal,
    /// The recur-progress stack the window opens with, when it opens inside a
    /// live recur site. Read only by the segment opening the window, and
    /// absent elsewhere. Never believed: the first step's own recorded
    /// `recur_progress_commitment` must be reproduced by advancing it.
    pub window_start_recur_progress: Option<RecurProgressStack>,
    /// For a window opening after `ProgramStart`: proof that `main`'s
    /// entry-argument binding is at `[]` in the window's initial store. Read
    /// only by the segment opening the window, and absent elsewhere.
    pub entrypoint_membership_witness: Option<StorageReadWitness>,
}

/// One step of a transition part segment: what the semantic checks read, and
/// the `StoreBefore` column's value — the store before the step, as the
/// storage part produced it.
#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionStep {
    pub step_record: StepRecord,
    pub replay_journal: Option<TileReplayJournal>,
    #[serde(with = "serde_bytes")]
    pub input_witness: Option<Vec<u8>>,
    #[serde(with = "serde_bytes")]
    pub output_witness: Option<Vec<u8>>,
    pub input_source_witness: Option<FnInput>,
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    pub draft_transition_witness: Option<DraftTransitionWitness>,
    pub store_before: StorageState,
    pub program_output_read_witness: Option<StorageReadWitness>,
    pub program_output_selection_witness: Option<SelectionWitness>,
    pub returned_state_read_witness: Option<StorageReadWitness>,
}

/// A fraud window's initial state: the trace frontier and store it opens on,
/// and the slice of the committed fingerprint it covers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InitTransition {
    pub init_frontier: SerializableFrontier,
    pub init_storage_frontier: SerializableFrontier,
    #[serde(with = "serde_bytes")]
    pub init_storage_root: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub init_storage_index_root: Vec<u8>,
    pub fingerprint: Fingerprint,
}

/// Whether `main`'s entry-argument binding has been tied to the authorization
/// journal within a fraud window.
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
/// there is no deferred debt to discharge at the window's end. The transition
/// part carries it in its state, so every later segment inherits the
/// established fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntrypointAuthorization {
    /// The CFS declares no `main` entry arguments — there is nothing to bind.
    NotRequired,
    /// The binding has been tied to the authorization journal in this window.
    Established,
}

/// Whether the program's authorized output has been tied to committed storage
/// within a fraud window.
///
/// The output is bound by the trace's *last* step, `ProgramEnd`, which proves
/// the returned value is a selection out of a committed storage object (see
/// the transition guest's `checks::entrypoint::verify_program_end`). Because it
/// is the last step, no window can open *after* it, so — unlike
/// [`EntrypointAuthorization`] — there is no membership-witness route and no
/// genesis case: a window is `Pending` until it verifies the `ProgramEnd` step,
/// then `Established`.
///
/// This is not discharged at the window's end: a fraud window legitimately concludes
/// at a mid-trace divergence, before any output exists, and must stay
/// `Pending`. The invariant is enforced elsewhere — `ProgramEnd` is the unique
/// terminal step bound into the fingerprint, host-side full-trace verification
/// requires it, and any consumer accepting a completed-program journal requires
/// `Established`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputAuthorization {
    /// The CFS declares `main` returns no value — there is no output to bind.
    NotRequired,
    /// The window has not reached (and verified) the program's end.
    Pending,
    /// A `ProgramEnd` step has been verified in this window: the committed
    /// output provably lives in committed storage — and `output_commitment`
    /// is the value it committed (`ProgramEnd.output_commitment`, checked
    /// in-guest against the selection witness by
    /// `checks::entrypoint::verify_program_end`).
    ///
    /// Carrying the value rather than only the fact is what makes the journal
    /// symmetric: [`crate::fraud_window::FraudWindowJournal::input_manifest_commitment`] names the
    /// authorized *input* the same window is bound to, and this names the
    /// output it produced. See `docs/proposals/chain-io-commitment.md`.
    Established {
        #[serde(with = "serde_bytes")]
        output_commitment: Vec<u8>,
    },
}

