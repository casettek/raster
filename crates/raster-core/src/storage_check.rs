//! The storage guest's interface, shared by the host and both guests.
//!
//! The storage guest proves the storage transition of a **segment** of
//! consecutive transition steps: every storage read resolves in the store as it
//! stood before the step, every selection witness folds to the object it reads,
//! and the step's write (or its absence) moves the append log and the
//! coordinate index to the roots the record claims. It computes one
//! [`StorageStepEntry`] per step and commits a Merkle root over them
//! ([`StorageJournal`]). Each transition step verifies the segment's receipt, is
//! handed its own entry with a path to that root ([`StorageEvidence`]), checks
//! the entry against its own state, and adopts the entry's state after.
//!
//! A segment of one step is the per-step mode; a segment covering the window is
//! the whole-window mode. Guests, journal and checks are the same in every mode
//! — the segment length is the host's choice. See
//! `docs/proposals/storage-prover.md`.

use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::hash::{Digest, Sha256};
use crate::input::{Hash32, SelectionWitness};
use crate::segment_tree::TreeDomain;
use crate::trace::{FnInput, StepRecord};
use crate::transition::{SerializableFrontier, StorageWitness};

/// The store's state between two steps: the append log's position and root,
/// and the coordinate index's root.
///
/// `(position, root)` binds the append-log frontier. A frontier's shape —
/// which levels hold an ommer — is fixed by its position, so two frontiers at
/// one position that fold to one root are a hash collision. That is what lets
/// the transition guest hold this instead of the frontier, and why `position`
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
    /// The append-log frontier before the segment's first step.
    pub start_frontier: SerializableFrontier,
    /// The coordinate-index root before the segment's first step.
    #[serde(with = "serde_bytes")]
    pub start_index_root: Vec<u8>,
    /// The trace position of the segment's first step. Entry `k` is the step at
    /// `first_position + k`.
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
    /// the transition guest, hence [`StorageStepEntry::selection_witnesses_commitment`].
    pub storage_selection_witnesses: BTreeMap<String, SelectionWitness>,
    /// Append-log and coordinate-index proofs. Only this guest reads them.
    pub storage_witness: Option<StorageWitness>,
}

/// What the storage guest commits for one step.
///
/// Every input the transition guest also holds is here, directly or as a hash,
/// so the transition step can equate each with its own value — on its own,
/// without trusting a check some other transition step made on the segment.
/// `storage_witness` is not: only this guest reads it, and it checked it
/// against `before` and the record's roots, which are here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStepEntry {
    /// `hash_trace_item(step_record)`: the same hash the trace leaf uses.
    #[serde(with = "serde_bytes")]
    pub step_record_hash: Vec<u8>,
    /// `input_source_commitment(w)` of the input source witness the checks ran
    /// on. Implied by the record, since both guests anchor the witness to it;
    /// committed so that "both guests used the same arguments" is one equality.
    #[serde(with = "serde_bytes")]
    pub input_source_commitment: Option<Vec<u8>>,
    /// [`selection_witnesses_commitment`] of the selection witnesses the checks
    /// ran on.
    ///
    /// **Load-bearing**, unlike `input_source_commitment`. Recur and io read
    /// these witnesses in the transition guest assuming the storage check
    /// folded them, and the record anchors them only through those folds. This
    /// equality is what makes the transition guest's copy the folded one.
    pub selection_witnesses_commitment: Hash32,
    pub trace_position: u64,
    pub before: StorageState,
    pub after: StorageState,
}

impl StorageStepEntry {
    /// The entry's leaf in the segment's tree.
    pub fn leaf_hash(&self) -> Hash32 {
        let bytes = postcard::to_allocvec(self).expect("a storage entry always encodes");
        STORAGE_TREE.leaf_hash(&bytes)
    }
}

/// What the storage guest commits for a segment: the trace positions it covers
/// and a Merkle root over its entries. No program: the storage check reads
/// none, and every entry is tied to the window by its record hash and position.
/// Committed as raw postcard bytes ([`Self::encode`], `env::commit_slice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageJournal {
    pub first_position: u64,
    pub entry_count: u64,
    /// [`entries_root`] over [`StorageStepEntry::leaf_hash`] of each entry.
    pub entries_root: Hash32,
}

impl StorageJournal {
    /// The journal bytes the storage guest commits and the transition guest
    /// verifies the receipt against.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a storage journal always encodes")
    }

    /// Read a receipt's journal bytes back.
    pub fn decode(bytes: &[u8]) -> postcard::Result<Self> {
        postcard::from_bytes(bytes)
    }

    /// The index of the entry for the step at `trace_position`, if the segment
    /// covers it.
    pub fn index_of(&self, trace_position: u64) -> Option<u64> {
        let index = trace_position.checked_sub(self.first_position)?;
        (index < self.entry_count).then_some(index)
    }
}

/// What a transition step is handed about its storage transition: the
/// segment's journal (verified against the receipt), its own entry, and the
/// path that ties the entry to the journal's root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageEvidence {
    pub journal: StorageJournal,
    pub entry: StorageStepEntry,
    pub path: Vec<Hash32>,
}

impl StorageEvidence {
    /// Whether `entry` is the journal's entry for the step at `trace_position`.
    pub fn entry_is_committed_at(&self, trace_position: u64) -> bool {
        let Some(index) = self.journal.index_of(trace_position) else {
            return false;
        };
        STORAGE_TREE.root_from_path(
            self.entry.leaf_hash(),
            index,
            self.journal.entry_count,
            &self.path,
        ) == Some(self.journal.entries_root)
    }
}

/// The storage guest's tree over its entries.
pub const STORAGE_TREE: TreeDomain = TreeDomain {
    leaf: b"raster/storage/entry/v1",
    node: b"raster/storage/node/v1",
};

/// The Merkle root over a storage segment's leaves.
pub fn entries_root(leaves: &[Hash32]) -> Hash32 {
    STORAGE_TREE.root(leaves)
}

/// The sibling hashes from leaf `index` up to the root.
pub fn entry_path(leaves: &[Hash32], index: usize) -> Vec<Hash32> {
    STORAGE_TREE.path(leaves, index)
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

    fn state(position: u64) -> StorageState {
        StorageState {
            position,
            root: vec![position as u8; 32],
            index_root: vec![0xA0 | position as u8; 32],
        }
    }

    fn entry(position: u64) -> StorageStepEntry {
        StorageStepEntry {
            step_record_hash: vec![position as u8; 32],
            input_source_commitment: None,
            selection_witnesses_commitment: [0; 32],
            trace_position: position,
            before: state(position),
            after: state(position + 1),
        }
    }

    fn evidence(entries: &[StorageStepEntry], first: u64, own: usize) -> StorageEvidence {
        let leaves: Vec<_> = entries.iter().map(StorageStepEntry::leaf_hash).collect();
        StorageEvidence {
            journal: StorageJournal {
                first_position: first,
                entry_count: entries.len() as u64,
                entries_root: entries_root(&leaves),
            },
            entry: entries[own].clone(),
            path: entry_path(&leaves, own),
        }
    }

    #[test]
    fn an_entry_is_committed_only_at_its_own_position() {
        let entries: Vec<_> = (10..15).map(entry).collect();
        let evidence = evidence(&entries, 10, 2);
        assert!(evidence.entry_is_committed_at(12));
        assert!(!evidence.entry_is_committed_at(11));
        assert!(!evidence.entry_is_committed_at(15), "past the segment");
        assert!(!evidence.entry_is_committed_at(9), "before the segment");
    }

    #[test]
    fn a_changed_entry_is_not_committed() {
        let entries: Vec<_> = (0..4).map(entry).collect();
        let mut evidence = evidence(&entries, 0, 1);
        evidence.entry.after.index_root[0] ^= 1;
        assert!(!evidence.entry_is_committed_at(1));
    }

    /// A control-flow root over the same leaves is a different root: neither
    /// guest's entries can stand in for the other's.
    #[test]
    fn the_storage_tree_is_not_the_control_flow_tree() {
        let leaves: Vec<_> = (0..4).map(entry).map(|e| e.leaf_hash()).collect();
        assert_ne!(entries_root(&leaves), crate::control_flow::entries_root(&leaves));
    }

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
