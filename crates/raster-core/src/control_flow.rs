//! The control-flow guest's interface, shared by the host and both guests.
//!
//! The control-flow guest proves the CFS conformance of a **segment** of
//! consecutive transition steps: does each record sit where the program's
//! control flow schema says it can, and do its arguments come from where the
//! schema says they do? It computes one [`ControlFlowEntry`] per step and
//! commits a Merkle root over them ([`ControlFlowJournal`]). Each transition
//! step verifies the segment's receipt, is handed its own entry with a path to
//! that root ([`ControlFlowEvidence`]), and checks the entry field by field
//! against its own state. A step's work is logarithmic in the segment length.
//!
//! A segment of one step is the per-step mode; a segment covering the window is
//! the whole-window mode. Guests, journal and checks are the same in every mode
//! — the segment length is the host's choice. See
//! `docs/proposals/control-flow-prover.md` §Segments.

use serde::{Deserialize, Serialize};
use std::vec::Vec;

use crate::cfs::{CfsCoordinates, CfsCursor};
use crate::hash::{Digest, Sha256};
use crate::input::Hash32;
use crate::trace::{FnInput, StepKind, StepRecord};
use crate::transition::{SerializableFrontier, StepRecordWitness};

/// One step of a segment, as the host supplies it: the record and the witnesses
/// the check anchors. The trace facts are not here — the guest derives them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowStep {
    pub step_record: StepRecord,
    pub input_source_witness: Option<FnInput>,
    pub sequence_scope_witness: Option<FnInput>,
    pub sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>,
}

/// What the control-flow guest reads: consecutive steps, and the trace state
/// before the first of them.
///
/// The guest derives every later step's trace position, trace root and
/// expected coordinates itself, by appending each record's hash and carrying
/// each step's successors forward. Only the start is host-supplied, and every
/// entry it leads to is equated by the transition guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowSegment {
    /// The trace frontier before the segment's first step.
    pub start_frontier: SerializableFrontier,
    /// The coordinates the step before the segment allows. `None` when the
    /// segment opens the window.
    pub expected_before: Option<Vec<CfsCoordinates>>,
    pub steps: Vec<ControlFlowStep>,
}

/// Everything the control-flow check reads for one step. The guest assembles one
/// per step of a [`ControlFlowSegment`]: the record and witnesses from the
/// [`ControlFlowStep`], the trace facts from its own walk.
///
/// Every field is either state the transition guest also holds — the record,
/// the trace position and root before the step is appended, the coordinates
/// the previous step allowed — or a witness the check anchors to one of those.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowInput {
    pub step_record: StepRecord,
    /// The step's trace index: the trace frontier's position before the step
    /// is appended.
    pub trace_position: u64,
    /// The root of the trace prefix the step is appended to.
    #[serde(with = "serde_bytes")]
    pub trace_root_before: Vec<u8>,
    /// The coordinates the previous step allows. `None` on a window's first
    /// step, whose predecessor is outside the window.
    pub expected_before: Option<Vec<CfsCoordinates>>,
    /// Anchored to `step_record.input_source_commitment`.
    pub input_source_witness: Option<FnInput>,
    /// The caller's arguments, for a step that binds a sequence-scope input.
    /// Anchored to the scope parent's recorded `input_source_commitment`.
    pub sequence_scope_witness: Option<FnInput>,
    /// The `SequenceStart` that opened the step's frame, for a step that binds
    /// a sequence-scope input. Anchored to `trace_root_before` by its
    /// inclusion witness.
    pub sequence_scope_parent: Option<(StepRecord, StepRecordWitness)>,
}

/// What the control-flow guest commits for a segment: the program, the trace
/// positions it covers, and a Merkle root over its entries.
///
/// The entries themselves are not committed. A transition step needs only its
/// own, which it is handed with a path to `entries_root` — so verifying the
/// receipt costs every step the same few bytes however long the segment is.
/// Committed as raw postcard bytes ([`Self::encode`], `env::commit_slice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFlowJournal {
    /// `sha256(domain || program.bin)` of the frame this guest verified against.
    #[serde(with = "serde_bytes")]
    pub program_commitment: Vec<u8>,
    /// The trace position of the segment's first step. Entry `k` is the step
    /// at `first_position + k`.
    pub first_position: u64,
    pub entry_count: u64,
    /// [`entries_root`] over [`ControlFlowEntry::leaf_hash`] of each entry, in
    /// order.
    pub entries_root: Hash32,
}

impl ControlFlowJournal {
    /// The journal bytes the control-flow guest commits and the transition
    /// guest verifies the receipt against. One function, so the two cannot
    /// disagree on the encoding.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a control-flow journal always encodes")
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

/// What a transition step is handed about its control flow: the segment's
/// journal (verified against the receipt), its own entry, and the path that
/// ties the entry to the journal's root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFlowEvidence {
    pub journal: ControlFlowJournal,
    pub entry: ControlFlowEntry,
    pub path: Vec<Hash32>,
}

impl ControlFlowEvidence {
    /// Whether `entry` is the journal's entry for the step at `trace_position`:
    /// the position falls in the segment, and the entry's leaf folds along
    /// `path` at that index to `entries_root`.
    pub fn entry_is_committed_at(&self, trace_position: u64) -> bool {
        let Some(index) = self.journal.index_of(trace_position) else {
            return false;
        };
        root_from_path(
            self.entry.leaf_hash(),
            index,
            self.journal.entry_count,
            &self.path,
        ) == Some(self.journal.entries_root)
    }
}

/// What the control-flow guest commits for one step.
///
/// Every input the transition guest also holds is here, directly or as a hash,
/// so the transition step can equate each with its own value — on its own,
/// without trusting a check some other transition step made on the segment.
/// The scope witness and its parent are not: only this guest reads them, and
/// it checked them against `trace_root_before`, which is here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFlowEntry {
    /// `hash_trace_item(step_record)`: the same hash the trace leaf uses.
    #[serde(with = "serde_bytes")]
    pub step_record_hash: Vec<u8>,
    /// `input_source_commitment(w)` of the input source witness the checks ran
    /// on, computed in this guest. `None` exactly when the step carries none.
    ///
    /// Implied by the record, since both guests anchor the witness to
    /// `step_record.input_source_commitment`. It is committed so that "both
    /// guests used the same arguments" is one equality in the transition guest
    /// rather than a conclusion drawn from two checks.
    #[serde(with = "serde_bytes")]
    pub input_source_commitment: Option<Vec<u8>>,
    pub trace_position: u64,
    #[serde(with = "serde_bytes")]
    pub trace_root_before: Vec<u8>,
    pub expected_before: Option<Vec<CfsCoordinates>>,
    /// The coordinates the next step may take. Empty after `ProgramEnd`.
    pub expected_after: Vec<CfsCoordinates>,
}

impl ControlFlowEntry {
    /// The entry's leaf in the segment's tree: a domain-separated hash of its
    /// postcard encoding, so no leaf can be read as an inner node.
    pub fn leaf_hash(&self) -> Hash32 {
        let bytes = postcard::to_allocvec(self).expect("a control-flow entry always encodes");
        let mut hasher = Sha256::new();
        hasher.update(ENTRY_LEAF_DOMAIN);
        hasher.update(&bytes);
        hasher.finalize().into()
    }
}

const ENTRY_LEAF_DOMAIN: &[u8] = b"raster/control-flow/entry/v1";
const ENTRY_NODE_DOMAIN: &[u8] = b"raster/control-flow/node/v1";

fn node_hash(left: &Hash32, right: &Hash32) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(ENTRY_NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// The next level up: pairs hashed, an odd last node carried up unchanged.
///
/// Carrying rather than duplicating the odd node means no two different leaf
/// lists share a root — the duplication ambiguity of Bitcoin's tree — and the
/// committed `entry_count` fixes the shape on both sides.
fn next_level(level: &[Hash32]) -> Vec<Hash32> {
    level
        .chunks(2)
        .map(|pair| match pair {
            [left, right] => node_hash(left, right),
            [single] => *single,
            _ => unreachable!("chunks(2) yields one or two"),
        })
        .collect()
}

/// The Merkle root over a segment's leaves. Panics on an empty list: a segment
/// covers at least one step.
pub fn entries_root(leaves: &[Hash32]) -> Hash32 {
    assert!(!leaves.is_empty(), "a control-flow segment has at least one entry");
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = next_level(&level);
    }
    level[0]
}

/// The sibling hashes from leaf `index` up to the root, skipping levels where
/// the node is carried up without a sibling.
pub fn entry_path(leaves: &[Hash32], index: usize) -> Vec<Hash32> {
    assert!(index < leaves.len(), "entry index out of range");
    let mut path = Vec::new();
    let mut level = leaves.to_vec();
    let mut index = index;
    while level.len() > 1 {
        if let Some(sibling) = level.get(index ^ 1) {
            path.push(*sibling);
        }
        level = next_level(&level);
        index /= 2;
    }
    path
}

/// Fold `leaf` at `index` of a tree over `count` leaves along `path`. `None`
/// when the path is the wrong length for that position.
pub fn root_from_path(leaf: Hash32, index: u64, count: u64, path: &[Hash32]) -> Option<Hash32> {
    if index >= count {
        return None;
    }
    let mut node = leaf;
    let mut index = index;
    let mut len = count;
    let mut siblings = path.iter();
    while len > 1 {
        let has_sibling = (index ^ 1) < len;
        if has_sibling {
            let sibling = siblings.next()?;
            node = if index % 2 == 0 {
                node_hash(&node, sibling)
            } else {
                node_hash(sibling, &node)
            };
        }
        index /= 2;
        len = len.div_ceil(2);
    }
    siblings.next().is_none().then_some(node)
}

/// The successor set the control-flow check commits for an honest step:
/// nothing after `ProgramEnd`, otherwise what the CFS declares.
///
/// For the host, which has to supply each segment's `expected_before` before
/// any step is proven. It does not check anything: the guest computes the same
/// set itself, and a host that disagrees is refused at that step's equality.
pub fn expected_successors(cfs: &CfsCursor, record: &StepRecord) -> Option<Vec<CfsCoordinates>> {
    if matches!(record.kind, StepKind::ProgramEnd(_)) {
        return Some(Vec::new());
    }
    cfs.try_get_next_coordinates(record.coordinates())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: u8) -> Vec<Hash32> {
        (0..n).map(|i| [i; 32]).collect()
    }

    /// Every leaf of every tree shape up to 17 leaves folds back to the root,
    /// odd levels included.
    #[test]
    fn every_entry_path_folds_to_the_root() {
        for n in 1..=17u8 {
            let leaves = leaves(n);
            let root = entries_root(&leaves);
            for (index, leaf) in leaves.iter().enumerate() {
                let path = entry_path(&leaves, index);
                assert_eq!(
                    root_from_path(*leaf, index as u64, n as u64, &path),
                    Some(root),
                    "n={n} index={index}",
                );
            }
        }
    }

    #[test]
    fn a_path_at_another_index_or_with_another_leaf_does_not_fold() {
        let leaves = leaves(8);
        let root = entries_root(&leaves);
        let path = entry_path(&leaves, 3);
        assert_ne!(root_from_path(leaves[3], 4, 8, &path), Some(root));
        assert_ne!(root_from_path([9; 32], 3, 8, &path), Some(root));
        assert_eq!(root_from_path(leaves[3], 8, 8, &path), None, "index past the count");
    }

    #[test]
    fn a_path_of_the_wrong_length_does_not_fold() {
        let leaves = leaves(5);
        let mut path = entry_path(&leaves, 0);
        path.push([0; 32]);
        assert_eq!(root_from_path(leaves[0], 0, 5, &path), None);
        path.truncate(1);
        assert_eq!(root_from_path(leaves[0], 0, 5, &path), None);
    }

    /// Carrying the odd node up, not duplicating it, keeps `[a, b, c]` and
    /// `[a, b, c, c]` apart.
    #[test]
    fn a_duplicated_last_leaf_changes_the_root() {
        let three = leaves(3);
        let mut four = three.clone();
        four.push(three[2]);
        assert_ne!(entries_root(&three), entries_root(&four));
    }
}
