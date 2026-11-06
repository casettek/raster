//! A coordinate index that can step back to an earlier size. Host-side only.
//!
//! Guests use [`IncrementalCoordinateIndex`] directly (the transition guest
//! builds one for the empty root), so its code is part of their image ids.
//! This wrapper adds the journal around it without changing it.

use crate::cfs::CfsCoordinates;
use crate::coordinate_index::{coordinates_key, prefix, IncrementalCoordinateIndex, IndexKey};
use crate::input::Hash32;
use crate::transition::StorageIndexValue;
use std::collections::VecDeque;
use std::vec::Vec;

/// An [`IncrementalCoordinateIndex`] that journals its last inserts, so it can
/// return to an earlier size by restoring the exact nodes each insert wrote
/// ([`Self::rewind_to`]). Reads go to the index itself (`Deref`).
#[derive(Debug, Clone, Default)]
pub struct JournaledCoordinateIndex {
    index: IncrementalCoordinateIndex,
    undo: VecDeque<UndoRecord>,
    capacity: usize,
}

/// What one insert changed, as it was before: enough to undo it exactly.
#[derive(Debug, Clone)]
struct UndoRecord {
    coordinates: CfsCoordinates,
    key: IndexKey,
    /// Each node the insert writes and its previous value (`None`: new).
    nodes: Vec<((u16, IndexKey), Option<Hash32>)>,
    root: Hash32,
}

impl JournaledCoordinateIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Journal the last `inserts` inserts. 0, the default, keeps none.
    pub fn retain_undo(&mut self, inserts: usize) {
        self.capacity = inserts;
        while self.undo.len() > inserts {
            self.undo.pop_front();
        }
    }

    pub fn insert(&mut self, coordinates: CfsCoordinates, value: StorageIndexValue) {
        if self.capacity == 0 {
            return self.index.insert(coordinates, value);
        }
        let key = coordinates_key(&coordinates);
        // The nodes `insert` writes: the key's path from the root down to where
        // it splits from its nearest neighbour. The neighbours, and so the
        // split, are the same before the new leaf is added as after.
        let nodes = match self.index.deepest_shared_prefix(&key) {
            None => Vec::new(),
            Some(split) => (0..=split)
                .map(|depth| {
                    let node = (depth as u16, prefix(&key, depth));
                    (node, self.index.nodes.get(&node).copied())
                })
                .collect(),
        };
        let record = UndoRecord {
            coordinates: coordinates.clone(),
            key,
            nodes,
            root: self.index.root,
        };
        self.index.insert(coordinates, value);
        if self.undo.len() == self.capacity {
            self.undo.pop_front();
        }
        self.undo.push_back(record);
    }

    /// The number of entries, which is also the number of inserts so far.
    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    /// Whether the journal reaches back to an index of `len` entries.
    pub fn can_rewind_to(&self, len: usize) -> bool {
        len <= self.len() && self.len() - len <= self.undo.len()
    }

    /// Undo the latest inserts until `len` entries remain: the nodes, leaves,
    /// entries and root the index had at that size.
    pub fn rewind_to(&mut self, len: usize) {
        assert!(
            self.can_rewind_to(len),
            "coordinate index journal does not reach back to {} entries (holds {}, journals {})",
            len,
            self.len(),
            self.undo.len()
        );
        while self.len() > len {
            let record = self.undo.pop_back().expect("journaled insert");
            for (node, previous) in record.nodes.into_iter().rev() {
                match previous {
                    Some(hash) => self.index.nodes.insert(node, hash),
                    None => self.index.nodes.remove(&node),
                };
            }
            self.index.leaves.remove(&record.key);
            self.index.entries.remove(&record.coordinates);
            self.index.root = record.root;
        }
    }

    /// Every entry in insertion (log) order.
    pub fn entries_by_position(&self) -> Vec<(CfsCoordinates, StorageIndexValue)> {
        let mut entries: Vec<_> = self
            .index
            .entries
            .iter()
            .map(|(coordinates, value)| (coordinates.clone(), value.clone()))
            .collect();
        entries.sort_by_key(|(_, value)| value.log_position);
        entries
    }

    /// The index itself, without the journal.
    pub fn into_index(self) -> IncrementalCoordinateIndex {
        self.index
    }
}

impl core::ops::Deref for JournaledCoordinateIndex {
    type Target = IncrementalCoordinateIndex;
    fn deref(&self) -> &IncrementalCoordinateIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    /// An index rewound to an earlier size is the index built from that
    /// size's entries alone: the same nodes, leaves, entries and root, so the
    /// same proofs. Random inserts and rewinds, including rewinding to zero,
    /// re-inserting after a rewind, and a journal shorter than the history.
    #[test]
    fn rewinding_restores_exactly_the_smaller_index() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let entry = |n: u64| {
            (
                CfsCoordinates(vec![(n % 7) as i32 + 1, (n / 7) as i32 + 1]),
                StorageIndexValue {
                    log_position: 0,
                    object_commitment: n.to_le_bytes().to_vec(),
                },
            )
        };
        let built = |entries: &[(CfsCoordinates, StorageIndexValue)]| {
            let mut index = IncrementalCoordinateIndex::new();
            for (coordinates, value) in entries {
                index.insert(coordinates.clone(), value.clone());
            }
            index
        };

        for capacity in [usize::MAX, 9] {
            let mut index = JournaledCoordinateIndex::new();
            index.retain_undo(capacity);
            let mut inserted: Vec<(CfsCoordinates, StorageIndexValue)> = Vec::new();
            let mut fresh = 0u64;
            for _ in 0..400 {
                if next() % 4 == 0 && !inserted.is_empty() {
                    let back = next() as usize % (inserted.len() + 1);
                    let target = inserted.len() - back;
                    if index.can_rewind_to(target) {
                        index.rewind_to(target);
                        inserted.truncate(target);
                    } else {
                        // Older records were dropped at capacity, or by an earlier rewind.
                        assert!(inserted.len() - target > index.undo.len());
                        assert!(index.undo.len() <= capacity);
                    }
                } else {
                    let (coordinates, mut value) = entry(fresh);
                    fresh += 1;
                    value.log_position = inserted.len() as u64 + 1;
                    index.insert(coordinates.clone(), value.clone());
                    inserted.push((coordinates, value));
                }
                let reference = built(&inserted);
                assert_eq!(index.index.root, reference.root);
                assert_eq!(index.index.entries, reference.entries);
                assert_eq!(index.index.leaves, reference.leaves);
                assert_eq!(index.index.nodes, reference.nodes);
                for (coordinates, _) in &inserted {
                    assert_eq!(
                        index.membership_proof(coordinates),
                        reference.membership_proof(coordinates)
                    );
                }
                let (absent, _) = entry(fresh + 1000);
                assert_eq!(
                    index.non_membership_proof(&absent),
                    reference.non_membership_proof(&absent)
                );
                assert_eq!(index.entries_by_position(), inserted);
            }
        }
    }

    #[test]
    fn without_a_journal_nothing_is_kept_and_nothing_rewinds() {
        let mut index = JournaledCoordinateIndex::new();
        for n in 0..10i32 {
            index.insert(
                CfsCoordinates(vec![n + 1]),
                StorageIndexValue {
                    log_position: n as u64 + 1,
                    object_commitment: vec![n as u8],
                },
            );
        }
        assert!(index.undo.is_empty());
        assert!(index.can_rewind_to(10));
        assert!(!index.can_rewind_to(9));
    }
}
