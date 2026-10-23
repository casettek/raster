//! The storage guest's interface, shared by the host and the guests.
//!
//! The storage guest proves the storage transition of a **segment** of
//! consecutive window steps: every storage read resolves in the store as it
//! stood before the step, every selection witness folds to the object it reads,
//! and the step's write (or its absence) moves the append log and the
//! coordinate index to the roots the record claims. It derives one
//! [`StorageStepEntry`] per step and commits the segment's record chain, its
//! store before and after, and the two columns it produces for the TRANSITION
//! part ([`crate::fraud_window::PartSegmentJournal`]).
//!
//! A segment of one step is the per-step mode; a segment covering the window is
//! the whole-window mode. Guest and journal are the same in every mode — the
//! segment length is the host's choice. See
//! `docs/proposals/storage-prover.md`.

use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::hash::{Digest, Sha256};
use crate::input::{Hash32, SelectionWitness};
use crate::trace::{FnInput, StepRecord};
use crate::transition::{InitTransition, SerializableFrontier, StorageWitness};

/// The store's state between two steps: the append log's position and root,
/// and the coordinate index's root.
///
/// `(position, root)` binds the append-log frontier. A frontier's shape —
/// which levels hold an ommer — is fixed by its position, so two frontiers at
/// one position that fold to one root are a hash collision. That is what lets
/// a segment's state and the `StoreBefore` column hold this instead of the
/// frontier, and why `position`
/// is here: a write's index entry names its log position, so a frontier at the
/// wrong position would let a write claim the wrong slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageState {
    pub position: u64,
    #[serde(with = "serde_bytes")]
    pub root: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub index_root: Vec<u8>,
}

/// The first frame the storage guest reads: the store before the segment's
/// first step, and the segment's extent. The steps follow, one frame each
/// ([`StorageStep`]), so the guest never holds more than one step's witnesses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSegmentHeader {
    /// The window. The segment that opens it must start from its initial
    /// store; every segment commits its id.
    pub init: InitTransition,
    /// The record chain and the two column chains before the segment's first
    /// step, in [`crate::fraud_window::Column::ALL`] order.
    pub records_before: Hash32,
    pub columns_before: [Hash32; 2],
    /// The append-log frontier before the segment's first step.
    pub start_frontier: SerializableFrontier,
    /// The coordinate-index root before the segment's first step.
    #[serde(with = "serde_bytes")]
    pub start_index_root: Vec<u8>,
    /// The trace position of the segment's first step.
    pub first_position: u64,
    pub step_count: u64,
}

/// One step of a segment, as the host supplies it: the record and every
/// witness the storage check reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStep {
    pub step_record: StepRecord,
    /// Anchored to `step_record.input_source_commitment`.
    pub input_source_witness: Option<FnInput>,
    /// Folded to the commitments `input_source_witness` binds. Also read by
    /// the TRANSITION part, through the `Selections` column.
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    /// Append-log and coordinate-index proofs. Only this guest reads them.
    pub storage_witness: Option<StorageWitness>,
}

/// What the storage check derives for one step: the facts the segment's
/// journal is folded from — its record chain, its state before and after, and
/// the two columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStepEntry {
    /// `hash_trace_item(step_record)`: the same hash the trace leaf uses.
    #[serde(with = "serde_bytes")]
    pub step_record_hash: Vec<u8>,
    /// `input_source_commitment(w)` of the input source witness the checks ran
    /// on.
    #[serde(with = "serde_bytes")]
    pub input_source_commitment: Option<Vec<u8>>,
    /// [`selection_witnesses_commitment`] of the selection witnesses the checks
    /// ran on: the `Selections` column's value.
    ///
    /// **Load-bearing.** Recur and io read these witnesses in the TRANSITION
    /// part assuming the storage check folded them, and the record anchors
    /// them only through those folds. The column chain is what makes the
    /// TRANSITION part's copy the folded one.
    pub selection_witnesses_commitment: Hash32,
    pub trace_position: u64,
    pub before: StorageState,
    pub after: StorageState,
}

const SELECTION_WITNESSES_DOMAIN: &[u8] = b"raster/storage/selection-witnesses/v1";

/// `sha256(domain ‖ postcard(witnesses))`: one hash over every selection
/// witness of a step, keyed by binding name. The map is ordered, so the
/// encoding is canonical. Computed with risc0's accelerated SHA inside a guest.
pub fn selection_witnesses_commitment(witnesses: &BTreeMap<String, SelectionWitness>) -> Hash32 {
    let bytes = postcard::to_allocvec(witnesses).expect("selection witnesses always encode");
    let mut hasher = Sha256::new();
    hasher.update(SELECTION_WITNESSES_DOMAIN);
    hasher.update(&bytes);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::SelectionProof;
    use std::string::ToString;
    use std::vec;

    #[test]
    fn the_selection_commitment_covers_every_witness() {
        let witness = |byte: u8| SelectionWitness {
            bytes: vec![byte; 4],
            proof: SelectionProof::default(),
            selected_root: None,
        };
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), witness(1));
        let one = selection_witnesses_commitment(&map);
        map.insert("b".to_string(), witness(2));
        let two = selection_witnesses_commitment(&map);
        assert_ne!(one, two);
        map.get_mut("b").unwrap().bytes[3] = 9;
        assert_ne!(two, selection_witnesses_commitment(&map));
        assert_ne!(
            selection_witnesses_commitment(&BTreeMap::new()),
            one,
            "the empty map has its own commitment",
        );
    }
}
