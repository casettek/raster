use alloc::vec;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::transition::{
    CoordinateIndexMembershipProof, CoordinateIndexNonMembershipProof, InternalStoreIndexValue,
};

const INDEX_BITS: usize = 256;
const EMPTY_LEAF_DOMAIN: &[u8] = b"raster-internal-index-empty";
const LEAF_DOMAIN: &[u8] = b"raster-internal-index-leaf";
const NODE_DOMAIN: &[u8] = b"raster-internal-index-node";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NodeKey {
    depth: usize,
    prefix: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct IncrementalCoordinateIndex {
    entries: BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    occupied_keys: HashMap<[u8; 32], CfsCoordinates>,
    node_hashes: HashMap<NodeKey, Vec<u8>>,
    empty_hashes: Vec<Vec<u8>>,
    root: Vec<u8>,
}

impl IncrementalCoordinateIndex {
    pub fn new() -> Self {
        let empty_hashes = empty_hashes();
        Self {
            entries: BTreeMap::new(),
            occupied_keys: HashMap::new(),
            node_hashes: HashMap::new(),
            root: empty_hashes[0].clone(),
            empty_hashes,
        }
    }

    pub fn from_entries(entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>) -> Self {
        let mut index = Self::new();
        for (coordinates, value) in entries {
            index.insert(coordinates.clone(), value.clone());
        }
        index
    }

    pub fn root(&self) -> Vec<u8> {
        self.root.clone()
    }

    pub fn contains_key(&self, coordinates: &CfsCoordinates) -> bool {
        self.entries.contains_key(coordinates)
    }

    pub fn get(&self, coordinates: &CfsCoordinates) -> Option<&InternalStoreIndexValue> {
        self.entries.get(coordinates)
    }

    pub fn insert(&mut self, coordinates: CfsCoordinates, value: InternalStoreIndexValue) {
        assert!(
            !self.entries.contains_key(&coordinates),
            "Duplicate internal store write at coordinates {:?}",
            coordinates
        );

        let key = coordinates_key(&coordinates);
        if let Some(existing_coordinates) = self.occupied_keys.get(&key) {
            assert_eq!(
                existing_coordinates, &coordinates,
                "Coordinate index hash collision for internal store entries",
            );
        }

        let leaf = leaf_hash(&key, &value);
        self.node_hashes
            .insert(node_key(&key, INDEX_BITS), leaf.clone());
        let previous = self.occupied_keys.insert(key, coordinates.clone());
        assert!(
            previous.is_none(),
            "Coordinate index hash collision for internal store entries",
        );

        let mut current = leaf;
        for depth in (0..INDEX_BITS).rev() {
            let bit = bit_at(&key, depth);
            let sibling = self.child_hash(&key, depth, !bit);
            current = if bit {
                combine_node_hash(depth, &sibling, &current)
            } else {
                combine_node_hash(depth, &current, &sibling)
            };
            self.node_hashes
                .insert(node_key(&key, depth), current.clone());
        }

        self.root = current;
        self.entries.insert(coordinates, value);
    }

    pub fn membership_proof(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<CoordinateIndexMembershipProof> {
        let value = self.entries.get(coordinates)?.clone();
        Some(CoordinateIndexMembershipProof {
            coordinates: coordinates.clone(),
            value,
            siblings: self.sibling_path(&coordinates_key(coordinates)),
        })
    }

    pub fn non_membership_proof(
        &self,
        coordinates: &CfsCoordinates,
    ) -> CoordinateIndexNonMembershipProof {
        assert!(
            !self.entries.contains_key(coordinates),
            "Coordinate index non-membership proof requested for existing coordinates {:?}",
            coordinates
        );

        CoordinateIndexNonMembershipProof {
            coordinates: coordinates.clone(),
            siblings: self.sibling_path(&coordinates_key(coordinates)),
        }
    }

    fn child_hash(&self, key: &[u8; 32], depth: usize, bit: bool) -> Vec<u8> {
        self.node_hashes
            .get(&child_node_key(key, depth, bit))
            .cloned()
            .unwrap_or_else(|| self.empty_hashes[depth + 1].clone())
    }

    fn sibling_path(&self, key: &[u8; 32]) -> Vec<Vec<u8>> {
        let mut siblings = Vec::with_capacity(INDEX_BITS);
        for depth in 0..INDEX_BITS {
            siblings.push(self.child_hash(key, depth, !bit_at(key, depth)));
        }
        siblings.reverse();
        siblings
    }
}

impl Default for IncrementalCoordinateIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn sha256(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

fn empty_hashes() -> Vec<Vec<u8>> {
    let mut hashes = vec![Vec::new(); INDEX_BITS + 1];
    hashes[INDEX_BITS] = sha256(&[EMPTY_LEAF_DOMAIN]);
    for depth in (0..INDEX_BITS).rev() {
        hashes[depth] = combine_node_hash(depth, &hashes[depth + 1], &hashes[depth + 1]);
    }
    hashes
}

fn combine_node_hash(depth: usize, left: &[u8], right: &[u8]) -> Vec<u8> {
    sha256(&[NODE_DOMAIN, &(depth as u16).to_le_bytes(), left, right])
}

fn coordinates_key(coordinates: &CfsCoordinates) -> [u8; 32] {
    let bytes = postcard::to_allocvec(coordinates).unwrap_or_default();
    let key = sha256(&[b"raster-internal-index-key", &bytes]);
    let mut array = [0u8; 32];
    array.copy_from_slice(&key);
    array
}

fn leaf_hash(key: &[u8], value: &InternalStoreIndexValue) -> Vec<u8> {
    let value_bytes = postcard::to_allocvec(value).unwrap_or_default();
    sha256(&[LEAF_DOMAIN, key, &value_bytes])
}

fn bit_at(key: &[u8], depth: usize) -> bool {
    let byte = key[depth / 8];
    let bit = 7 - (depth % 8);
    ((byte >> bit) & 1) == 1
}

fn prefix_for_depth(key: &[u8; 32], depth: usize) -> [u8; 32] {
    let mut prefix = [0u8; 32];
    let full_bytes = depth / 8;
    prefix[..full_bytes].copy_from_slice(&key[..full_bytes]);
    let partial_bits = depth % 8;
    if partial_bits != 0 {
        let mask = u8::MAX << (8 - partial_bits);
        prefix[full_bytes] = key[full_bytes] & mask;
    }
    prefix
}

fn node_key(key: &[u8; 32], depth: usize) -> NodeKey {
    NodeKey {
        depth,
        prefix: prefix_for_depth(key, depth),
    }
}

fn child_node_key(key: &[u8; 32], depth: usize, bit: bool) -> NodeKey {
    let mut prefix = prefix_for_depth(key, depth);
    let byte = depth / 8;
    let bit_index = 7 - (depth % 8);
    if bit {
        prefix[byte] |= 1 << bit_index;
    }
    NodeKey {
        depth: depth + 1,
        prefix,
    }
}

pub fn coordinate_index_root(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
) -> Vec<u8> {
    IncrementalCoordinateIndex::from_entries(entries).root()
}

pub fn coordinate_index_membership_proof(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    coordinates: &CfsCoordinates,
) -> Option<CoordinateIndexMembershipProof> {
    IncrementalCoordinateIndex::from_entries(entries).membership_proof(coordinates)
}

pub fn coordinate_index_non_membership_proof(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    coordinates: &CfsCoordinates,
) -> CoordinateIndexNonMembershipProof {
    IncrementalCoordinateIndex::from_entries(entries).non_membership_proof(coordinates)
}

fn root_from_proof(
    coordinates: &CfsCoordinates,
    siblings: &[Vec<u8>],
    leaf: Vec<u8>,
) -> Option<Vec<u8>> {
    if siblings.len() != INDEX_BITS {
        return None;
    }

    let key = coordinates_key(coordinates);
    let mut current = leaf;
    for (offset, sibling) in siblings.iter().enumerate() {
        let depth = INDEX_BITS - 1 - offset;
        current = if bit_at(&key, depth) {
            combine_node_hash(depth, sibling, &current)
        } else {
            combine_node_hash(depth, &current, sibling)
        };
    }
    Some(current)
}

pub fn verify_coordinate_index_membership(
    root: &[u8],
    proof: &CoordinateIndexMembershipProof,
) -> bool {
    let key = coordinates_key(&proof.coordinates);
    let leaf = leaf_hash(&key, &proof.value);
    root_from_proof(&proof.coordinates, &proof.siblings, leaf)
        .is_some_and(|computed| computed == root)
}

pub fn verify_coordinate_index_non_membership(
    root: &[u8],
    proof: &CoordinateIndexNonMembershipProof,
) -> bool {
    let empty = empty_hashes();
    root_from_proof(
        &proof.coordinates,
        &proof.siblings,
        empty[INDEX_BITS].clone(),
    )
    .is_some_and(|computed| computed == root)
}

#[cfg(test)]
mod tests {
    use super::*;

    type IndexedEntries<'a> = Vec<(&'a CfsCoordinates, [u8; 32], &'a InternalStoreIndexValue)>;

    fn legacy_indexed_entries<'a>(
        entries: &'a BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    ) -> IndexedEntries<'a> {
        entries
            .iter()
            .map(|(coordinates, value)| (coordinates, coordinates_key(coordinates), value))
            .collect()
    }

    fn legacy_subtree_hash(
        entries: &[(&CfsCoordinates, [u8; 32], &InternalStoreIndexValue)],
        depth: usize,
        empty: &[Vec<u8>],
    ) -> Vec<u8> {
        if entries.is_empty() {
            return empty[depth].clone();
        }
        if depth == INDEX_BITS {
            assert_eq!(
                entries.len(),
                1,
                "Coordinate index hash collision for internal store entries",
            );
            let (_, key, value) = &entries[0];
            return leaf_hash(key, value);
        }

        let mut left = Vec::new();
        let mut right = Vec::new();
        for entry in entries {
            if bit_at(&entry.1, depth) {
                right.push((entry.0, entry.1, entry.2));
            } else {
                left.push((entry.0, entry.1, entry.2));
            }
        }

        let left_hash = legacy_subtree_hash(&left, depth + 1, empty);
        let right_hash = legacy_subtree_hash(&right, depth + 1, empty);
        combine_node_hash(depth, &left_hash, &right_hash)
    }

    fn legacy_collect_siblings(
        entries: &[(&CfsCoordinates, [u8; 32], &InternalStoreIndexValue)],
        key: &[u8; 32],
        depth: usize,
        empty: &[Vec<u8>],
        siblings: &mut Vec<Vec<u8>>,
    ) {
        if depth == INDEX_BITS {
            return;
        }

        let mut left = Vec::new();
        let mut right = Vec::new();
        for entry in entries {
            if bit_at(&entry.1, depth) {
                right.push((entry.0, entry.1, entry.2));
            } else {
                left.push((entry.0, entry.1, entry.2));
            }
        }

        if bit_at(key, depth) {
            legacy_collect_siblings(&right, key, depth + 1, empty, siblings);
            siblings.push(legacy_subtree_hash(&left, depth + 1, empty));
        } else {
            legacy_collect_siblings(&left, key, depth + 1, empty, siblings);
            siblings.push(legacy_subtree_hash(&right, depth + 1, empty));
        }
    }

    fn legacy_root(entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>) -> Vec<u8> {
        let empty = empty_hashes();
        legacy_subtree_hash(&legacy_indexed_entries(entries), 0, &empty)
    }

    fn legacy_membership(
        entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
        coordinates: &CfsCoordinates,
    ) -> Option<CoordinateIndexMembershipProof> {
        let value = entries.get(coordinates)?.clone();
        let empty = empty_hashes();
        let key = coordinates_key(coordinates);
        let mut siblings = Vec::with_capacity(INDEX_BITS);
        legacy_collect_siblings(
            &legacy_indexed_entries(entries),
            &key,
            0,
            &empty,
            &mut siblings,
        );
        Some(CoordinateIndexMembershipProof {
            coordinates: coordinates.clone(),
            value,
            siblings,
        })
    }

    fn legacy_non_membership(
        entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
        coordinates: &CfsCoordinates,
    ) -> CoordinateIndexNonMembershipProof {
        let empty = empty_hashes();
        let key = coordinates_key(coordinates);
        let mut siblings = Vec::with_capacity(INDEX_BITS);
        legacy_collect_siblings(
            &legacy_indexed_entries(entries),
            &key,
            0,
            &empty,
            &mut siblings,
        );
        CoordinateIndexNonMembershipProof {
            coordinates: coordinates.clone(),
            siblings,
        }
    }

    fn sample_entries() -> BTreeMap<CfsCoordinates, InternalStoreIndexValue> {
        let mut entries = BTreeMap::new();
        entries.insert(
            CfsCoordinates(vec![1]),
            InternalStoreIndexValue {
                log_position: 7,
                object_commitment: vec![1; 32],
            },
        );
        entries.insert(
            CfsCoordinates(vec![2, 3]),
            InternalStoreIndexValue {
                log_position: 8,
                object_commitment: vec![2; 32],
            },
        );
        entries.insert(
            CfsCoordinates(vec![4, 5, 6]),
            InternalStoreIndexValue {
                log_position: 9,
                object_commitment: vec![3; 32],
            },
        );
        entries
    }

    #[test]
    fn membership_and_non_membership_proofs_round_trip() {
        let entries = sample_entries();
        let root = coordinate_index_root(&entries);
        let membership = coordinate_index_membership_proof(&entries, &CfsCoordinates(vec![1]))
            .expect("membership proof");
        let non_membership =
            coordinate_index_non_membership_proof(&entries, &CfsCoordinates(vec![9, 9]));

        assert!(verify_coordinate_index_membership(&root, &membership));
        assert!(verify_coordinate_index_non_membership(
            &root,
            &non_membership
        ));
    }

    #[test]
    fn incremental_index_matches_legacy_root_and_proofs() {
        let entries = sample_entries();
        let index = IncrementalCoordinateIndex::from_entries(&entries);
        let missing = CfsCoordinates(vec![42, 24]);

        assert_eq!(index.root(), legacy_root(&entries));
        assert_eq!(
            index
                .membership_proof(&CfsCoordinates(vec![2, 3]))
                .expect("membership proof"),
            legacy_membership(&entries, &CfsCoordinates(vec![2, 3])).expect("legacy proof")
        );
        assert_eq!(
            index.non_membership_proof(&missing),
            legacy_non_membership(&entries, &missing)
        );
    }

    #[test]
    fn write_proofs_keep_same_siblings_across_fresh_insert() {
        let mut index = IncrementalCoordinateIndex::new();
        index.insert(
            CfsCoordinates(vec![1]),
            InternalStoreIndexValue {
                log_position: 1,
                object_commitment: vec![7; 32],
            },
        );
        let coordinates = CfsCoordinates(vec![9, 9]);
        let before = index.non_membership_proof(&coordinates);
        index.insert(
            coordinates.clone(),
            InternalStoreIndexValue {
                log_position: 2,
                object_commitment: vec![8; 32],
            },
        );
        let after = index
            .membership_proof(&coordinates)
            .expect("membership proof after insert");

        assert_eq!(before.siblings, after.siblings);
    }
}
