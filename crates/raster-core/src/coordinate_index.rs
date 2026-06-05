use alloc::vec;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::transition::{
    CoordinateIndexMembershipProof, CoordinateIndexNonMembershipProof, InternalStoreIndexValue,
};

const INDEX_BITS: usize = 256;
const EMPTY_LEAF_DOMAIN: &[u8] = b"raster-internal-index-empty";
const LEAF_DOMAIN: &[u8] = b"raster-internal-index-leaf";
const NODE_DOMAIN: &[u8] = b"raster-internal-index-node";

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
    sha256(&[
        NODE_DOMAIN,
        &(depth as u16).to_le_bytes(),
        left,
        right,
    ])
}

fn coordinates_key(coordinates: &CfsCoordinates) -> Vec<u8> {
    let bytes = postcard::to_allocvec(coordinates).unwrap_or_default();
    sha256(&[b"raster-internal-index-key", &bytes])
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

type IndexedEntries<'a> = Vec<(&'a CfsCoordinates, Vec<u8>, &'a InternalStoreIndexValue)>;

fn indexed_entries<'a>(
    entries: &'a BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
) -> IndexedEntries<'a> {
    entries
        .iter()
        .map(|(coordinates, value)| (coordinates, coordinates_key(coordinates), value))
        .collect()
}

fn subtree_hash(entries: &[(&CfsCoordinates, Vec<u8>, &InternalStoreIndexValue)], depth: usize, empty: &[Vec<u8>]) -> Vec<u8> {
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
            right.push((entry.0, entry.1.clone(), entry.2));
        } else {
            left.push((entry.0, entry.1.clone(), entry.2));
        }
    }

    let left_hash = subtree_hash(&left, depth + 1, empty);
    let right_hash = subtree_hash(&right, depth + 1, empty);
    combine_node_hash(depth, &left_hash, &right_hash)
}

fn collect_siblings(
    entries: &[(&CfsCoordinates, Vec<u8>, &InternalStoreIndexValue)],
    key: &[u8],
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
            right.push((entry.0, entry.1.clone(), entry.2));
        } else {
            left.push((entry.0, entry.1.clone(), entry.2));
        }
    }

    if bit_at(key, depth) {
        collect_siblings(&right, key, depth + 1, empty, siblings);
        siblings.push(subtree_hash(&left, depth + 1, empty));
    } else {
        collect_siblings(&left, key, depth + 1, empty, siblings);
        siblings.push(subtree_hash(&right, depth + 1, empty));
    }
}

pub fn coordinate_index_root(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
) -> Vec<u8> {
    let empty = empty_hashes();
    subtree_hash(&indexed_entries(entries), 0, &empty)
}

pub fn coordinate_index_membership_proof(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    coordinates: &CfsCoordinates,
) -> Option<CoordinateIndexMembershipProof> {
    let value = entries.get(coordinates)?.clone();
    let empty = empty_hashes();
    let key = coordinates_key(coordinates);
    let mut siblings = Vec::with_capacity(INDEX_BITS);
    collect_siblings(&indexed_entries(entries), &key, 0, &empty, &mut siblings);
    Some(CoordinateIndexMembershipProof {
        coordinates: coordinates.clone(),
        value,
        siblings,
    })
}

pub fn coordinate_index_non_membership_proof(
    entries: &BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
    coordinates: &CfsCoordinates,
) -> CoordinateIndexNonMembershipProof {
    assert!(
        !entries.contains_key(coordinates),
        "Coordinate index non-membership proof requested for existing coordinates {:?}",
        coordinates
    );
    let empty = empty_hashes();
    let key = coordinates_key(coordinates);
    let mut siblings = Vec::with_capacity(INDEX_BITS);
    collect_siblings(&indexed_entries(entries), &key, 0, &empty, &mut siblings);
    CoordinateIndexNonMembershipProof {
        coordinates: coordinates.clone(),
        siblings,
    }
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

    #[test]
    fn membership_and_non_membership_proofs_round_trip() {
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

        let root = coordinate_index_root(&entries);
        let membership = coordinate_index_membership_proof(&entries, &CfsCoordinates(vec![1]))
            .expect("membership proof");
        let non_membership =
            coordinate_index_non_membership_proof(&entries, &CfsCoordinates(vec![9, 9]));

        assert!(verify_coordinate_index_membership(&root, &membership));
        assert!(verify_coordinate_index_non_membership(&root, &non_membership));
    }
}
