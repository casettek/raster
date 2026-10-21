//! The Merkle tree a segment guest commits over its per-step entries.
//!
//! A segment guest (control flow, storage) proves a run of consecutive
//! transition steps, computes one entry per step, and commits only a root over
//! the entries. Each transition step is handed its own entry with a path to
//! that root, so its work is logarithmic in the segment's length. Each guest
//! hashes under its own [`TreeDomain`], so no entry of one can be read as an
//! entry of the other, and no leaf as an inner node.

use std::vec::Vec;

use crate::hash::{Digest, Sha256};
use crate::input::Hash32;

/// The domain-separation prefixes of one guest's tree.
#[derive(Debug, Clone, Copy)]
pub struct TreeDomain {
    pub leaf: &'static [u8],
    pub node: &'static [u8],
}

impl TreeDomain {
    /// An entry's leaf: a hash of its encoding under the leaf domain.
    pub fn leaf_hash(&self, entry_bytes: &[u8]) -> Hash32 {
        let mut hasher = Sha256::new();
        hasher.update(self.leaf);
        hasher.update(entry_bytes);
        hasher.finalize().into()
    }

    fn node_hash(&self, left: &Hash32, right: &Hash32) -> Hash32 {
        let mut hasher = Sha256::new();
        hasher.update(self.node);
        hasher.update(left);
        hasher.update(right);
        hasher.finalize().into()
    }

    /// The next level up: pairs hashed, an odd last node carried up unchanged.
    ///
    /// Carrying rather than duplicating the odd node means no two different
    /// leaf lists share a root — the duplication ambiguity of Bitcoin's tree —
    /// and the committed entry count fixes the shape on both sides.
    fn next_level(&self, level: &[Hash32]) -> Vec<Hash32> {
        level
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => self.node_hash(left, right),
                [single] => *single,
                _ => unreachable!("chunks(2) yields one or two"),
            })
            .collect()
    }

    /// The Merkle root over a segment's leaves. Panics on an empty list: a
    /// segment covers at least one step.
    pub fn root(&self, leaves: &[Hash32]) -> Hash32 {
        assert!(!leaves.is_empty(), "a segment has at least one entry");
        let mut level = leaves.to_vec();
        while level.len() > 1 {
            level = self.next_level(&level);
        }
        level[0]
    }

    /// The sibling hashes from leaf `index` up to the root, skipping levels
    /// where the node is carried up without a sibling.
    pub fn path(&self, leaves: &[Hash32], index: usize) -> Vec<Hash32> {
        assert!(index < leaves.len(), "entry index out of range");
        let mut path = Vec::new();
        let mut level = leaves.to_vec();
        let mut index = index;
        while level.len() > 1 {
            if let Some(sibling) = level.get(index ^ 1) {
                path.push(*sibling);
            }
            level = self.next_level(&level);
            index /= 2;
        }
        path
    }

    /// Fold `leaf` at `index` of a tree over `count` leaves along `path`.
    /// `None` when the path is the wrong length for that position.
    pub fn root_from_path(
        &self,
        leaf: Hash32,
        index: u64,
        count: u64,
        path: &[Hash32],
    ) -> Option<Hash32> {
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
                    self.node_hash(&node, sibling)
                } else {
                    self.node_hash(sibling, &node)
                };
            }
            index /= 2;
            len = len.div_ceil(2);
        }
        siblings.next().is_none().then_some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: TreeDomain = TreeDomain {
        leaf: b"raster/test/entry/v1",
        node: b"raster/test/node/v1",
    };

    fn leaves(n: u8) -> Vec<Hash32> {
        (0..n).map(|i| [i; 32]).collect()
    }

    /// Every leaf of every tree shape up to 17 leaves folds back to the root,
    /// odd levels included.
    #[test]
    fn every_entry_path_folds_to_the_root() {
        for n in 1..=17u8 {
            let leaves = leaves(n);
            let root = TEST.root(&leaves);
            for (index, leaf) in leaves.iter().enumerate() {
                let path = TEST.path(&leaves, index);
                assert_eq!(
                    TEST.root_from_path(*leaf, index as u64, n as u64, &path),
                    Some(root),
                    "n={n} index={index}",
                );
            }
        }
    }

    #[test]
    fn a_path_at_another_index_or_with_another_leaf_does_not_fold() {
        let leaves = leaves(8);
        let root = TEST.root(&leaves);
        let path = TEST.path(&leaves, 3);
        assert_ne!(TEST.root_from_path(leaves[3], 4, 8, &path), Some(root));
        assert_ne!(TEST.root_from_path([9; 32], 3, 8, &path), Some(root));
        assert_eq!(TEST.root_from_path(leaves[3], 8, 8, &path), None, "index past the count");
    }

    #[test]
    fn a_path_of_the_wrong_length_does_not_fold() {
        let leaves = leaves(5);
        let mut path = TEST.path(&leaves, 0);
        path.push([0; 32]);
        assert_eq!(TEST.root_from_path(leaves[0], 0, 5, &path), None);
        path.truncate(1);
        assert_eq!(TEST.root_from_path(leaves[0], 0, 5, &path), None);
    }

    #[test]
    fn an_odd_node_is_carried_not_duplicated() {
        let three = leaves(3);
        let mut four = three.clone();
        four.push(three[2]);
        assert_ne!(TEST.root(&three), TEST.root(&four));
    }

    #[test]
    fn domains_separate_trees() {
        const OTHER: TreeDomain = TreeDomain {
            leaf: b"raster/other/entry/v1",
            node: b"raster/other/node/v1",
        };
        let leaves = leaves(4);
        assert_ne!(TEST.root(&leaves), OTHER.root(&leaves));
        assert_ne!(TEST.leaf_hash(b"entry"), OTHER.leaf_hash(b"entry"));
    }
}
