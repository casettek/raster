//! The coordinate index: every stored object's coordinates, mapped to its
//! append-log position and commitment, under one root.
//!
//! A **compact sparse Merkle tree** over 256-bit keys, as in Haider, "Compact
//! Sparse Merkle Trees" (2018). A key's leaf sits at the shallowest depth where
//! it is alone in its subtree, rather than at depth 256, so a proof is about
//! log₂(objects) levels deep. The shape is a function of the key set only:
//!
//! ```text
//! node(d, p) = EMPTY                          if no key has prefix p
//!            = leaf(k, v)                     if exactly one key k has prefix p
//!            = internal(node(d+1, p‖0), node(d+1, p‖1))   otherwise
//! ```
//!
//! `EMPTY` is a constant that is not a hash output, and leaves and internal
//! nodes hash under different domains, so no node kind can pass for another.
//! A node's position is fixed by the path the verifier walks with the key it
//! holds, so depth is not hashed in.
//!
//! The verifiers ([`verify_coordinate_index_membership`],
//! [`verify_coordinate_index_non_membership`],
//! [`verify_coordinate_index_insert`]) run in the guests; the builder
//! ([`IncrementalCoordinateIndex`]) runs in the recorder and the prover. Both
//! use the same node functions, so they cannot define the tree differently.

use crate::hash::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::input::Hash32;
use crate::merkle::NodeHash;
use crate::transition::{
    CoordinateIndexMembershipProof, CoordinateIndexNonMembershipProof, IndexTerminal,
    StorageIndexValue,
};

/// A coordinate's position in the index.
pub type IndexKey = [u8; 32];

const KEY_BITS: usize = 256;
const KEY_DOMAIN: &[u8] = b"raster-storage-index-key";
const LEAF_DOMAIN: &[u8] = b"raster-index/v2/leaf";
const NODE_DOMAIN: &[u8] = b"raster-index/v2/node";

/// An empty subtree. Not a hash output, so it can't be produced by a leaf or
/// an internal node.
pub const EMPTY: Hash32 = [0; 32];

/// `sha256(domain ‖ postcard(coordinates))`: uniform over 256 bits.
pub fn coordinates_key(coordinates: &CfsCoordinates) -> IndexKey {
    let bytes = postcard::to_allocvec(coordinates).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(KEY_DOMAIN);
    hasher.update(&bytes);
    hasher.finalize().into()
}

/// A leaf commits to its full key, so a leaf at depth `d` says "the only key
/// under this prefix is `key`".
pub fn leaf_hash(key: &IndexKey, value: &StorageIndexValue) -> Hash32 {
    let value_bytes = postcard::to_allocvec(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(LEAF_DOMAIN);
    hasher.update(key);
    hasher.update(&value_bytes);
    hasher.finalize().into()
}

pub fn internal_hash(left: &Hash32, right: &Hash32) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Bit `depth` of `key`, most significant first: which child of the node at
/// `depth` the key descends into.
fn bit(key: &IndexKey, depth: usize) -> bool {
    (key[depth / 8] >> (7 - depth % 8)) & 1 == 1
}

/// How many leading bits `a` and `b` share. 256 when equal.
fn shared_prefix_len(a: &IndexKey, b: &IndexKey) -> usize {
    for (index, (x, y)) in a.iter().zip(b).enumerate() {
        let differing = x ^ y;
        if differing != 0 {
            return index * 8 + differing.leading_zeros() as usize;
        }
    }
    KEY_BITS
}

/// The parent of `node` (at `depth`) and `sibling`, on `key`'s side.
fn parent(key: &IndexKey, depth: usize, node: &Hash32, sibling: &Hash32) -> Hash32 {
    if bit(key, depth - 1) {
        internal_hash(sibling, node)
    } else {
        internal_hash(node, sibling)
    }
}

/// Fold `node`, which sits at depth `siblings.len()` on `key`'s path, up to
/// the root. `siblings[0]` is `node`'s sibling. `None` for a path deeper than
/// a key.
pub fn fold_up(node: Hash32, key: &IndexKey, siblings: &[NodeHash]) -> Option<Hash32> {
    if siblings.len() > KEY_BITS {
        return None;
    }
    let mut depth = siblings.len();
    let mut current = node;
    for sibling in siblings {
        current = parent(key, depth, &current, &**sibling);
        depth -= 1;
    }
    Some(current)
}

/// `(proof.coordinates, proof.value)` is in the index under `root`.
///
/// A path placed too deep folds `internal(leaf, EMPTY)`, and one too shallow
/// folds a leaf where an internal node is; the canonical tree holds neither,
/// so both miss the root. No separate shape check is needed.
pub fn verify_coordinate_index_membership(
    root: &[u8],
    proof: &CoordinateIndexMembershipProof,
) -> bool {
    let key = coordinates_key(&proof.coordinates);
    fold_up(leaf_hash(&key, &proof.value), &key, &proof.siblings)
        .is_some_and(|computed| computed.as_slice() == root)
}

/// The hash of a non-membership proof's terminal, if the terminal is a
/// well-formed witness that `key` is absent at depth `depth`: empty, or the
/// one other key sharing `key`'s first `depth` bits.
fn terminal_hash(key: &IndexKey, depth: usize, terminal: &IndexTerminal) -> Option<Hash32> {
    match terminal {
        IndexTerminal::Empty => Some(EMPTY),
        IndexTerminal::Leaf {
            key: other,
            value,
        } => (other != key && shared_prefix_len(key, other) >= depth)
            .then(|| leaf_hash(other, value)),
    }
}

/// `proof.coordinates` is absent from the index under `root`.
pub fn verify_coordinate_index_non_membership(
    root: &[u8],
    proof: &CoordinateIndexNonMembershipProof,
) -> bool {
    key_is_absent(root, &coordinates_key(&proof.coordinates), &proof.terminal, &proof.siblings)
}

/// The index root after inserting `(proof.coordinates, value)`, if `proof`
/// shows the coordinates absent under `root`. `None` otherwise.
pub fn verify_coordinate_index_insert(
    root: &[u8],
    proof: &CoordinateIndexNonMembershipProof,
    value: &StorageIndexValue,
) -> Option<Hash32> {
    let key = coordinates_key(&proof.coordinates);
    key_is_absent(root, &key, &proof.terminal, &proof.siblings)
        .then(|| root_after_insert(&key, &proof.terminal, &proof.siblings, value))
}

fn key_is_absent(root: &[u8], key: &IndexKey, terminal: &IndexTerminal, siblings: &[NodeHash]) -> bool {
    terminal_hash(key, siblings.len(), terminal)
        .and_then(|terminal| fold_up(terminal, key, siblings))
        .is_some_and(|computed| computed.as_slice() == root)
}

/// The root after inserting `(key, value)` at a terminal already shown to be
/// on `key`'s path and not holding `key`.
///
/// Only the terminal's subtree changes, and its new form follows from what it
/// held: a lone new leaf in an empty slot, or the new leaf and the old one
/// split at the first bit where their keys differ, with one-child levels down
/// to there. The siblings above the terminal are the same nodes before and
/// after, so folding the new subtree with them gives the new root.
fn root_after_insert(
    key: &IndexKey,
    terminal: &IndexTerminal,
    siblings: &[NodeHash],
    value: &StorageIndexValue,
) -> Hash32 {
    let depth = siblings.len();
    let ours = leaf_hash(key, value);
    let subtree = match terminal {
        IndexTerminal::Empty => ours,
        IndexTerminal::Leaf {
            key: other,
            value: other_value,
        } => {
            // Checked by `terminal_hash`: the keys differ and agree on
            // [0, depth).
            let split = shared_prefix_len(key, other);
            let theirs = leaf_hash(other, other_value);
            // The node at `split`, whose children are the two leaves.
            let mut node = if bit(key, split) {
                internal_hash(&theirs, &ours)
            } else {
                internal_hash(&ours, &theirs)
            };
            // One-child levels from `split` up to the terminal's depth.
            for level in (depth..split).rev() {
                node = parent(key, level + 1, &node, &EMPTY);
            }
            node
        }
    };
    fold_up(subtree, key, siblings).expect("a verified path is no deeper than a key")
}

/// Bits `[0, depth)` of `key`, the rest zero: a subtree's prefix.
fn prefix(key: &IndexKey, depth: usize) -> IndexKey {
    let mut out = [0u8; 32];
    let full = depth / 8;
    out[..full].copy_from_slice(&key[..full]);
    if depth % 8 != 0 {
        out[full] = key[full] & (u8::MAX << (8 - depth % 8));
    }
    out
}

/// `prefix(key, depth)` with every bit from `depth` on set: the subtree's
/// last key.
fn prefix_end(key: &IndexKey, depth: usize) -> IndexKey {
    let mut out = prefix(key, depth);
    let full = depth / 8;
    if depth % 8 != 0 {
        out[full] |= u8::MAX >> (depth % 8);
        out[full + 1..].fill(u8::MAX);
    } else {
        out[full..].fill(u8::MAX);
    }
    out
}

/// The prefix of the child of `(depth, prefix)` on side `right`: bit `depth`
/// set or clear. `prefix` has every bit from `depth` on clear.
fn child_prefix(prefix: &IndexKey, depth: usize, right: bool) -> IndexKey {
    let mut out = *prefix;
    if right {
        out[depth / 8] |= 1 << (7 - depth % 8);
    }
    out
}

/// The host's index: built incrementally by the recorder and by the prover's
/// replay of the store, and producing the proofs the guests verify.
///
/// `leaves` holds every key, ordered, so the keys under a prefix are a range.
/// `nodes` holds every internal node by `(depth, prefix)`. A subtree with no
/// entry there has at most one key, found by a range lookup. An insert
/// rehashes only the new leaf's path: about log₂(objects) nodes.
#[derive(Debug, Clone)]
pub struct IncrementalCoordinateIndex {
    entries: BTreeMap<CfsCoordinates, StorageIndexValue>,
    leaves: BTreeMap<IndexKey, (Hash32, StorageIndexValue)>,
    nodes: HashMap<(u16, IndexKey), Hash32>,
    root: Hash32,
}

impl Default for IncrementalCoordinateIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalCoordinateIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            leaves: BTreeMap::new(),
            nodes: HashMap::new(),
            root: EMPTY,
        }
    }

    pub fn from_entries(entries: &BTreeMap<CfsCoordinates, StorageIndexValue>) -> Self {
        let mut index = Self::new();
        for (coordinates, value) in entries {
            index.insert(coordinates.clone(), value.clone());
        }
        index
    }

    pub fn root(&self) -> Vec<u8> {
        self.root.to_vec()
    }

    pub fn contains_key(&self, coordinates: &CfsCoordinates) -> bool {
        self.entries.contains_key(coordinates)
    }

    pub fn get(&self, coordinates: &CfsCoordinates) -> Option<&StorageIndexValue> {
        self.entries.get(coordinates)
    }

    pub fn insert(&mut self, coordinates: CfsCoordinates, value: StorageIndexValue) {
        assert!(
            !self.entries.contains_key(&coordinates),
            "Duplicate storage write at coordinates {:?}",
            coordinates
        );
        let key = coordinates_key(&coordinates);
        let previous = self
            .leaves
            .insert(key, (leaf_hash(&key, &value), value.clone()));
        assert!(
            previous.is_none(),
            "Coordinate index hash collision for storage entries",
        );
        self.entries.insert(coordinates, value);

        // Every node on the new key's path down to where it splits from its
        // nearest neighbour now holds at least two keys: an internal node,
        // rehashed bottom-up. Below the split the new leaf is alone.
        match self.deepest_shared_prefix(&key) {
            None => self.root = self.leaves[&key].0,
            Some(split) => {
                for depth in (0..=split).rev() {
                    let node_prefix = prefix(&key, depth);
                    let hash = internal_hash(
                        &self.subtree_hash(depth + 1, &child_prefix(&node_prefix, depth, false)),
                        &self.subtree_hash(depth + 1, &child_prefix(&node_prefix, depth, true)),
                    );
                    self.nodes.insert((depth as u16, node_prefix), hash);
                    if depth == 0 {
                        self.root = hash;
                    }
                }
            }
        }
    }

    pub fn membership_proof(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<CoordinateIndexMembershipProof> {
        let value = self.entries.get(coordinates)?.clone();
        let key = coordinates_key(coordinates);
        let depth = self.deepest_shared_prefix(&key).map_or(0, |split| split + 1);
        Some(CoordinateIndexMembershipProof {
            coordinates: coordinates.clone(),
            value,
            siblings: self.siblings(&key, depth),
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
        let key = coordinates_key(coordinates);
        // Walk down while the key's path runs through internal nodes; the
        // first subtree that isn't one holds at most one key, not ours.
        let mut depth = 0;
        while self.nodes.contains_key(&(depth as u16, prefix(&key, depth))) {
            depth += 1;
        }
        let terminal = match self.single_key_under(&key, depth) {
            None => IndexTerminal::Empty,
            Some(other) => IndexTerminal::Leaf {
                key: other,
                value: self.leaves[&other].1.clone(),
            },
        };
        CoordinateIndexNonMembershipProof {
            coordinates: coordinates.clone(),
            terminal,
            siblings: self.siblings(&key, depth),
        }
    }

    /// The longest prefix `key` shares with any other key in the index, or
    /// `None` when it is the only one. Only the ordered neighbours can share
    /// the longest.
    fn deepest_shared_prefix(&self, key: &IndexKey) -> Option<usize> {
        let before = self.leaves.range(..*key).next_back().map(|(other, _)| other);
        let after = self
            .leaves
            .range((core::ops::Bound::Excluded(*key), core::ops::Bound::Unbounded))
            .next()
            .map(|(other, _)| other);
        before
            .into_iter()
            .chain(after)
            .map(|other| shared_prefix_len(key, other))
            .max()
    }

    /// The subtree at `depth` with `node_prefix`: an internal node if one is
    /// recorded, otherwise the lone leaf under it, otherwise `EMPTY`.
    fn subtree_hash(&self, depth: usize, node_prefix: &IndexKey) -> Hash32 {
        if let Some(hash) = self.nodes.get(&(depth as u16, *node_prefix)) {
            return *hash;
        }
        match self.single_key_under(node_prefix, depth) {
            Some(other) => self.leaves[&other].0,
            None => EMPTY,
        }
    }

    /// The one key under `key`'s first `depth` bits, when that subtree holds no
    /// internal node. Panics if it holds several: the index would be missing
    /// a node.
    fn single_key_under(&self, key: &IndexKey, depth: usize) -> Option<IndexKey> {
        let mut keys = self
            .leaves
            .range(prefix(key, depth)..=prefix_end(key, depth))
            .map(|(other, _)| *other);
        let first = keys.next();
        assert!(
            keys.next().is_none(),
            "coordinate index is missing an internal node at depth {depth}",
        );
        first
    }

    /// The siblings of the nodes on `key`'s path from `depth` up to the root's
    /// children, `depth`'s first.
    fn siblings(&self, key: &IndexKey, depth: usize) -> Vec<NodeHash> {
        (1..=depth)
            .rev()
            .map(|level| {
                let parent_prefix = prefix(key, level - 1);
                let sibling = child_prefix(&parent_prefix, level - 1, !bit(key, level - 1));
                self.subtree_hash(level, &sibling).into()
            })
            .collect()
    }
}

/// The root of an index holding exactly `entries`.
pub fn coordinate_index_root(entries: &BTreeMap<CfsCoordinates, StorageIndexValue>) -> Vec<u8> {
    IncrementalCoordinateIndex::from_entries(entries).root()
}

pub fn coordinate_index_membership_proof(
    entries: &BTreeMap<CfsCoordinates, StorageIndexValue>,
    coordinates: &CfsCoordinates,
) -> Option<CoordinateIndexMembershipProof> {
    IncrementalCoordinateIndex::from_entries(entries).membership_proof(coordinates)
}

pub fn coordinate_index_non_membership_proof(
    entries: &BTreeMap<CfsCoordinates, StorageIndexValue>,
    coordinates: &CfsCoordinates,
) -> CoordinateIndexNonMembershipProof {
    IncrementalCoordinateIndex::from_entries(entries).non_membership_proof(coordinates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    // ------------------------------------------------------------------
    // A direct recursive definition of the tree over raw keys: the reference
    // the incremental index and the verifiers are held to.
    // ------------------------------------------------------------------

    type Leaves = Vec<(IndexKey, Hash32)>;

    fn under(leaves: &[(IndexKey, Hash32)], key: &IndexKey, depth: usize) -> Leaves {
        leaves
            .iter()
            .filter(|(other, _)| shared_prefix_len(key, other) >= depth)
            .cloned()
            .collect()
    }

    fn reference_node(leaves: &[(IndexKey, Hash32)], depth: usize) -> Hash32 {
        match leaves {
            [] => EMPTY,
            [(_, leaf)] => *leaf,
            _ => {
                let (right, left): (Leaves, Leaves) =
                    leaves.iter().cloned().partition(|(key, _)| bit(key, depth));
                internal_hash(
                    &reference_node(&left, depth + 1),
                    &reference_node(&right, depth + 1),
                )
            }
        }
    }

    /// The first depth on `key`'s path whose subtree holds at most one key
    /// (`key`'s own leaf, for a member), and the siblings from there up.
    fn reference_path(leaves: &[(IndexKey, Hash32)], key: &IndexKey) -> (usize, Vec<NodeHash>) {
        let mut depth = 0;
        while under(leaves, key, depth).len() >= 2 {
            depth += 1;
        }
        let siblings = (1..=depth)
            .rev()
            .map(|level| {
                let side: Leaves = under(leaves, key, level - 1)
                    .into_iter()
                    .filter(|(other, _)| bit(other, level - 1) != bit(key, level - 1))
                    .collect();
                reference_node(&side, level).into()
            })
            .collect();
        (depth, siblings)
    }

    fn value(seed: u64) -> StorageIndexValue {
        StorageIndexValue {
            log_position: seed,
            object_commitment: vec![(seed % 251) as u8; 32],
        }
    }

    /// Deterministic pseudo-random coordinates: `n` distinct ones.
    fn coordinates(n: usize, seed: u64) -> Vec<CfsCoordinates> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out: Vec<CfsCoordinates> = Vec::new();
        while out.len() < n {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let len = 1 + (state >> 60) as usize % 4;
            let coords: Vec<i32> = (0..len)
                .map(|i| ((state >> (8 * i)) & 0x3f) as i32 - 8)
                .collect();
            let candidate = CfsCoordinates(coords);
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
        out
    }

    fn keyed(entries: &[(CfsCoordinates, StorageIndexValue)]) -> Leaves {
        entries
            .iter()
            .map(|(coordinates, value)| {
                let key = coordinates_key(coordinates);
                (key, leaf_hash(&key, value))
            })
            .collect()
    }

    fn entries(n: usize, seed: u64) -> Vec<(CfsCoordinates, StorageIndexValue)> {
        coordinates(n, seed)
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c, value(i as u64 + 1)))
            .collect()
    }

    // ------------------------------------------------------------------
    // The incremental index against the reference
    // ------------------------------------------------------------------

    #[test]
    fn the_empty_index_has_the_empty_root() {
        assert_eq!(IncrementalCoordinateIndex::new().root(), EMPTY.to_vec());
        assert_eq!(coordinate_index_root(&BTreeMap::new()), EMPTY.to_vec());
    }

    #[test]
    fn a_lone_key_is_the_root() {
        let entries = entries(1, 7);
        let index = IncrementalCoordinateIndex::from_entries(&entries.iter().cloned().collect());
        assert_eq!(index.root(), keyed(&entries)[0].1.to_vec());
        let proof = index.membership_proof(&entries[0].0).unwrap();
        assert!(proof.siblings.is_empty());
        assert!(verify_coordinate_index_membership(&index.root(), &proof));
    }

    /// After every insert, in every order tried, the root and every proof are
    /// the reference's, and every proof verifies.
    #[test]
    fn the_incremental_index_matches_the_reference_after_every_insert() {
        for (n, seed) in [(2, 1), (3, 2), (5, 3), (17, 4), (64, 5), (300, 6)] {
            let entries = entries(n, seed);
            let absent = coordinates(n + 8, seed + 100)
                .into_iter()
                .filter(|c| !entries.iter().any(|(e, _)| e == c))
                .take(8)
                .collect::<Vec<_>>();
            let mut index = IncrementalCoordinateIndex::new();
            for (inserted, (coordinates, value)) in entries.iter().enumerate() {
                index.insert(coordinates.clone(), value.clone());
                let present = &entries[..=inserted];
                let leaves = keyed(present);
                let root = index.root();
                assert_eq!(root, reference_node(&leaves, 0).to_vec(), "n={n} after {inserted}");
                if inserted + 1 < n && inserted % 7 != 0 {
                    continue; // checking every proof after every insert is quadratic
                }
                for (coordinates, _) in present {
                    let proof = index.membership_proof(coordinates).unwrap();
                    let (depth, siblings) = reference_path(&leaves, &coordinates_key(coordinates));
                    assert_eq!(proof.siblings.len(), depth);
                    assert_eq!(proof.siblings, siblings);
                    assert!(verify_coordinate_index_membership(&root, &proof));
                }
                for coordinates in &absent {
                    let proof = index.non_membership_proof(coordinates);
                    assert!(verify_coordinate_index_non_membership(&root, &proof));
                }
            }
        }
    }

    #[test]
    fn insertion_order_does_not_change_the_root() {
        let entries = entries(40, 9);
        let forward = IncrementalCoordinateIndex::from_entries(&entries.iter().cloned().collect());
        let mut backward = IncrementalCoordinateIndex::new();
        for (coordinates, value) in entries.iter().rev() {
            backward.insert(coordinates.clone(), value.clone());
        }
        assert_eq!(forward.root(), backward.root());
    }

    /// The write rule: the root the guest computes from the non-membership
    /// proof is the root the recorder reaches by inserting.
    #[test]
    fn the_root_after_an_insert_follows_from_its_non_membership_proof() {
        for (n, seed) in [(1, 11), (2, 12), (9, 13), (120, 14)] {
            let entries = entries(n, seed);
            let mut index = IncrementalCoordinateIndex::new();
            for (coordinates, value) in &entries {
                let before = index.root();
                let proof = index.non_membership_proof(coordinates);
                let computed = verify_coordinate_index_insert(&before, &proof, value)
                    .expect("an honest proof verifies");
                index.insert(coordinates.clone(), value.clone());
                assert_eq!(computed.to_vec(), index.root());
            }
        }
    }

    #[test]
    fn proofs_are_shallow() {
        let entries = entries(300, 21);
        let index = IncrementalCoordinateIndex::from_entries(&entries.iter().cloned().collect());
        let deepest = entries
            .iter()
            .map(|(c, _)| index.membership_proof(c).unwrap().siblings.len())
            .max()
            .unwrap();
        assert!(deepest < 32, "300 uniform keys, deepest leaf at {deepest}");
    }

    // ------------------------------------------------------------------
    // Collisions at every depth, with constructed keys
    // ------------------------------------------------------------------

    fn with_bit(mut key: IndexKey, depth: usize) -> IndexKey {
        key[depth / 8] ^= 1 << (7 - depth % 8);
        key
    }

    /// For each depth `j`, a key that splits from an existing one exactly at
    /// `j`: the root `root_after_insert` computes is the reference root of
    /// the larger set, with the terminal at depth 0 (a lone leaf) and, for
    /// `j > 0`, at depth 1 (under an internal root).
    #[test]
    fn an_insert_splitting_at_every_depth_reaches_the_reference_root() {
        let a: IndexKey = [0x5a; 32];
        for split in 0..KEY_BITS {
            let b = with_bit(a, split);
            assert_eq!(shared_prefix_len(&a, &b), split);
            let mut sets: Vec<Vec<IndexKey>> = vec![vec![a]];
            if split > 0 {
                sets.push(vec![a, with_bit(a, 0)]);
            }
            for set in sets {
                let leaves: Leaves = set
                    .iter()
                    .enumerate()
                    .map(|(i, key)| (*key, leaf_hash(key, &value(i as u64))))
                    .collect();
                let root = reference_node(&leaves, 0);
                let (depth, siblings) = reference_path(&leaves, &b);
                let terminal = match under(&leaves, &b, depth).as_slice() {
                    [] => IndexTerminal::Empty,
                    [(other, _)] => IndexTerminal::Leaf {
                        key: *other,
                        value: value(set.iter().position(|k| k == other).unwrap() as u64),
                    },
                    _ => unreachable!(),
                };
                assert!(key_is_absent(&root, &b, &terminal, &siblings), "split={split}");
                let new_value = value(99);
                let mut grown = leaves.clone();
                grown.push((b, leaf_hash(&b, &new_value)));
                assert_eq!(
                    root_after_insert(&b, &terminal, &siblings, &new_value),
                    reference_node(&grown, 0),
                    "split={split} set={}",
                    set.len(),
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Tampering
    // ------------------------------------------------------------------

    fn sample() -> (IncrementalCoordinateIndex, Vec<(CfsCoordinates, StorageIndexValue)>) {
        let entries = entries(50, 31);
        (IncrementalCoordinateIndex::from_entries(&entries.iter().cloned().collect()), entries)
    }

    /// A member whose proof has at least one sibling.
    fn deep_member(
        index: &IncrementalCoordinateIndex,
        entries: &[(CfsCoordinates, StorageIndexValue)],
    ) -> CoordinateIndexMembershipProof {
        entries
            .iter()
            .map(|(c, _)| index.membership_proof(c).unwrap())
            .find(|proof| proof.siblings.len() >= 2)
            .unwrap()
    }

    #[test]
    fn a_membership_proof_with_a_changed_sibling_or_value_is_refused() {
        let (index, entries) = sample();
        let root = index.root();
        let honest = deep_member(&index, &entries);
        assert!(verify_coordinate_index_membership(&root, &honest));

        let mut sibling = honest.clone();
        let mut first = *sibling.siblings[0];
        first[0] ^= 1;
        sibling.siblings[0] = first.into();
        assert!(!verify_coordinate_index_membership(&root, &sibling));

        let mut changed = honest.clone();
        changed.value.log_position += 1;
        assert!(!verify_coordinate_index_membership(&root, &changed));

        let mut other_coordinates = honest;
        other_coordinates.coordinates = CfsCoordinates(vec![12345]);
        assert!(!verify_coordinate_index_membership(&root, &other_coordinates));
    }

    /// A leaf placed one level too deep (an extra `EMPTY` sibling) or too
    /// shallow (a sibling dropped) folds to a tree the index never holds.
    #[test]
    fn a_membership_proof_at_the_wrong_depth_is_refused() {
        let (index, entries) = sample();
        let root = index.root();
        let honest = deep_member(&index, &entries);

        let mut deeper = honest.clone();
        deeper.siblings.insert(0, EMPTY.into());
        assert!(!verify_coordinate_index_membership(&root, &deeper));

        let mut shallower = honest;
        shallower.siblings.remove(0);
        assert!(!verify_coordinate_index_membership(&root, &shallower));
    }

    #[test]
    fn a_member_cannot_be_shown_absent() {
        let (index, entries) = sample();
        let root = index.root();
        let member = deep_member(&index, &entries);
        let key = coordinates_key(&member.coordinates);
        // Its own leaf as the terminal: refused for naming the key itself.
        let own = CoordinateIndexNonMembershipProof {
            coordinates: member.coordinates.clone(),
            terminal: IndexTerminal::Leaf {
                key,
                value: member.value.clone(),
            },
            siblings: member.siblings.clone(),
        };
        assert!(!verify_coordinate_index_non_membership(&root, &own));
        // An empty slot where its leaf is: refused by the root.
        let empty = CoordinateIndexNonMembershipProof {
            terminal: IndexTerminal::Empty,
            ..own
        };
        assert!(!verify_coordinate_index_non_membership(&root, &empty));
    }

    /// A real leaf from elsewhere in the tree, presented as the terminal on
    /// an absent key's path: its key doesn't share the path's prefix.
    #[test]
    fn a_terminal_off_the_keys_path_is_refused() {
        let (index, entries) = sample();
        let root = index.root();
        let absent = CfsCoordinates(vec![777, 1]);
        let honest = index.non_membership_proof(&absent);
        assert!(verify_coordinate_index_non_membership(&root, &honest));
        let depth = honest.siblings.len();
        let key = coordinates_key(&absent);
        let (elsewhere, elsewhere_value) = entries
            .iter()
            .map(|(c, v)| (coordinates_key(c), v.clone()))
            .find(|(other, _)| shared_prefix_len(&key, other) < depth)
            .expect("some key diverges above the terminal");
        let forged = CoordinateIndexNonMembershipProof {
            terminal: IndexTerminal::Leaf {
                key: elsewhere,
                value: elsewhere_value,
            },
            ..honest
        };
        assert!(!verify_coordinate_index_non_membership(&root, &forged));
    }

    #[test]
    fn an_insert_from_a_proof_that_does_not_verify_has_no_root() {
        let (index, _) = sample();
        let root = index.root();
        let absent = CfsCoordinates(vec![777, 2]);
        let mut proof = index.non_membership_proof(&absent);
        assert!(verify_coordinate_index_insert(&root, &proof, &value(1)).is_some());
        let mut first = *proof.siblings[0];
        first[31] ^= 1;
        proof.siblings[0] = first.into();
        assert!(verify_coordinate_index_insert(&root, &proof, &value(1)).is_none());
    }

    #[test]
    fn a_path_deeper_than_a_key_is_refused() {
        let key = [0u8; 32];
        assert!(fold_up(EMPTY, &key, &vec![EMPTY.into(); KEY_BITS + 1]).is_none());
    }
}
