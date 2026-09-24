//! Trace commitment utilities.
//!
//! This module provides types and functions for creating cryptographic
//! commitments to execution traces using incremental Merkle trees.

use bridgetree::{Hashable, Level, NonEmptyFrontier};
use incrementalmerkletree::{MerklePath, Position};
use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource,
    SequenceChildItem, FIRST_COORDINATE,
};
use raster_core::fingerprint::{fingerprint_value, Fingerprint, FingerprintAccumulator};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;

use crate::error::{BitPackerError, Result};
use crate::precomputed::{EMPTY_TRIE_NODES, HASH_SIZE};

use raster_core::fingerprint::BitPacker;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord, Trace, TraceWindow};
use raster_core::transition::{
    FingerprintBlockWitness, FingerprintSliceWitness, StepRecordWitness, TraceCommitmentHeader,
};

/// Trait for types that can be hashed to bytes.
pub trait BytesHashable {
    /// Compute the SHA256 hash of this item.
    fn hash(&self) -> Vec<u8>;

    /// Try to compute the hash, returning an error on failure.
    fn try_hash(&self) -> Result<Vec<u8>> {
        Ok(self.hash())
    }
}

impl BytesHashable for StepRecord {
    fn hash(&self) -> Vec<u8> {
        let data = postcard::to_allocvec(self).expect("Failed to serialize for hashing");
        sha256_bytes(&data)
    }

    fn try_hash(&self) -> Result<Vec<u8>> {
        let data = postcard::to_allocvec(self)
            .map_err(|e| BitPackerError::SerializationError(e.to_string()))?;
        Ok(sha256_bytes(&data))
    }
}

/// Wrapper for byte vectors that implements Hashable for bridgetree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Bytes {}

impl PartialOrd for Bytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hashable for Bytes {
    fn empty_leaf() -> Self {
        Bytes(EMPTY_TRIE_NODES[0].to_vec())
    }

    /// Memoized empty-subtree root.
    ///
    /// The trait's default is an unmemoized fold from level 0, and
    /// `NonEmptyFrontier::root` calls it once per level — so folding a
    /// depth-32 frontier spent 0+1+..+31 = 496 hashes rederiving the same
    /// constants, against ~32 hashes of actual spine. The memo is built by
    /// that same fold, so the values are identical by construction.
    ///
    /// Note this is *not* `EMPTY_TRIE_NODES`: despite that table's doc
    /// comment, its entries above level 0 are not this `combine`'s empty
    /// roots (`empty_root_memo_matches_fold` is the guard that would catch a
    /// swap).
    fn empty_root(level: Level) -> Self {
        let idx = usize::from(u8::from(level));
        match empty_root_memo().get(idx) {
            Some(node) => Bytes(node.to_vec()),
            // Above the memo: fold, so a deeper tree stays correct.
            None => empty_root_fold(level),
        }
    }

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);

        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);

        Bytes(sha256_bytes(&data))
    }
}

/// The `Hashable` default: fold empty leaves up to `level`. O(level) hashes.
fn empty_root_fold(level: Level) -> Bytes {
    Level::from(0)
        .iter_to(level)
        .fold(Bytes::empty_leaf(), |v, lvl| Bytes::combine(lvl, &v, &v))
}

/// Empty-subtree roots for levels `0..=TRACE_TREE_DEPTH`, folded once.
fn empty_root_memo() -> &'static [[u8; HASH_SIZE]] {
    static MEMO: std::sync::OnceLock<Vec<[u8; HASH_SIZE]>> = std::sync::OnceLock::new();
    MEMO.get_or_init(|| {
        (0..=TRACE_TREE_DEPTH)
            .map(|level| {
                let root = empty_root_fold(Level::from(level));
                let mut out = [0u8; HASH_SIZE];
                out.copy_from_slice(&root.0);
                out
            })
            .collect()
    })
}

fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Re-export from raster-core; conversion to/from TraceTreeFrontier via functions below.
pub use raster_core::transition::SerializableFrontier;

/// Convert a trace tree frontier into a serializable form (for persistence/replay).
pub fn serializable_frontier_from_trace_frontier(
    frontier: TraceTreeFrontier,
) -> SerializableFrontier {
    SerializableFrontier {
        position: frontier.position().into(),
        leaf: frontier.leaf().clone().0,
        ommers: frontier.ommers().iter().map(|o| o.clone().0).collect(),
    }
}

/// Reconstruct a TraceTreeFrontier from a serializable frontier.
pub fn serializable_frontier_into_trace_frontier(
    s: SerializableFrontier,
) -> Option<TraceTreeFrontier> {
    use bridgetree::Position;
    TraceTreeFrontier::from_parts(
        Position::from(s.position),
        Bytes(s.leaf.clone()),
        s.ommers.iter().map(|o| Bytes(o.clone())).collect(),
    )
    .ok()
}

/// Bridge tree for trace commitments with 32 levels.
pub type TraceTree = bridgetree::BridgeTree<Bytes, u64, 32>;
pub type TraceTreeFrontier = NonEmptyFrontier<Bytes>;

/// Depth of [`TraceTree`]; the root level a frontier must be folded to.
pub const TRACE_TREE_DEPTH: u8 = 32;

/// Root of a frontier, folded directly against the empty-subtree roots.
///
/// Equivalent to `TraceTree::from_frontier(1, frontier.clone()).root(0)` — which
/// is what `BridgeTree::root` itself does after rebuilding the bridge — but
/// without the clone and the tree allocation. The rebuild dominated every
/// storage append (~53 us of a ~56 us root recompute), so the fold is called
/// directly.
pub fn frontier_root(frontier: &TraceTreeFrontier) -> Vec<u8> {
    frontier.root(Some(Level::from(TRACE_TREE_DEPTH))).0
}

/// Soundness target (in bits) for fraud detection: a fraud-proof window
/// reveals `window_size * bits_per_item >= FRAUD_DETECTION_SECURITY_BITS`
/// fingerprint bits.
pub const FRAUD_DETECTION_SECURITY_BITS: usize = 128;

/// Lower limit for fingerprint bits revealed per trace item; window sizes
/// beyond [`FRAUD_DETECTION_SECURITY_BITS`] still reveal one bit per item.
pub const MIN_BITS_PER_ITEM: usize = 1;

/// Upper limit for fingerprint bits revealed per trace item: the bit packer
/// packs each item into u64 blocks.
pub const MAX_BITS_PER_ITEM: usize = 64;

/// Upper limit for the fraud-proof window size.
pub const MAX_FRAUD_PROOF_WINDOW_SIZE: usize = 1024;

/// Parameters of the fraud-proof window a trace commitment is built with.
#[derive(Debug, Clone, Copy)]
pub struct FraudProofConfig {
    pub window_size: usize,
    pub bits_per_item: usize,
}

impl FraudProofConfig {
    /// Derive the config from the CLI-provided fraud-proof window size.
    ///
    /// The window size must be a power of two no greater than
    /// [`MAX_FRAUD_PROOF_WINDOW_SIZE`]; `bits_per_item` is derived so the
    /// window reveals [`FRAUD_DETECTION_SECURITY_BITS`] fingerprint bits
    /// (e.g. window size 128 reveals 1 bit per item, 32 reveals 4), never
    /// dropping below [`MIN_BITS_PER_ITEM`].
    pub fn from_window_size(window_size: usize) -> Result<Self> {
        if !window_size.is_power_of_two() || window_size > MAX_FRAUD_PROOF_WINDOW_SIZE {
            return Err(BitPackerError::InvalidWindow(format!(
                "Fraud proof window size must be a power of two no greater than {}, got {}",
                MAX_FRAUD_PROOF_WINDOW_SIZE, window_size
            )));
        }

        let bits_per_item = FRAUD_DETECTION_SECURITY_BITS
            .div_ceil(window_size)
            .max(MIN_BITS_PER_ITEM);

        if bits_per_item > MAX_BITS_PER_ITEM {
            return Err(BitPackerError::InvalidWindow(format!(
                "Fraud proof window size {} requires {} fingerprint bits per item, \
                 but the bit packer supports at most {}; use a window size of at least {}",
                window_size,
                bits_per_item,
                MAX_BITS_PER_ITEM,
                FRAUD_DETECTION_SECURITY_BITS / MAX_BITS_PER_ITEM
            )));
        }

        Ok(Self {
            window_size,
            bits_per_item,
        })
    }
}

/// Re-export from raster-core: the struct is shared with the transition guest,
/// which decodes and hashes the exact `commit.bin` bytes it refutes (see
/// `docs/proposals/chain-fraud-proof.md`). Everything needing the Merkle tree
/// stays here, behind [`TraceCommitmentExt`].
pub use raster_core::trace::TraceCommitment;

/// Host-side construction and verification of a [`TraceCommitment`] — the
/// tree-dependent half of the type, which cannot live in `raster-core`.
///
/// `build`/`try_build` were `TraceCommitment::from`/`try_from` when the
/// struct was local; the rename avoids ambiguity with the prelude's
/// `From`/`TryFrom` now that these are trait methods.
pub trait TraceCommitmentExt: Sized {
    fn build(trace: &Trace, seed: &[u8], fraud_proof_config: FraudProofConfig) -> Self;
    fn try_build(trace: &Trace, seed: &[u8], fraud_proof_config: FraudProofConfig) -> Result<Self>;
    fn validate(&self) -> Result<()>;
    fn frontier(trace: &Trace, n: usize, seed: &[u8]) -> Option<TraceTreeFrontier>;
    fn witness(trace: &Trace, n: usize, seed: &[u8]) -> Option<MerklePath<Bytes, 32>>;
    fn try_frontier(trace: &Trace, n: usize, seed: &[u8]) -> Result<TraceTreeFrontier>;
    fn diff(&self, other: &TraceCommitment) -> Option<usize>;
    /// The commitment's compact identity (see [`TraceCommitmentHeader`]):
    /// what journals and chain checkpoints hash instead of the whole file.
    fn header(&self) -> TraceCommitmentHeader;
    /// Inclusion proofs for the packed fingerprint blocks covering window
    /// items `[window_start, window_start + window_len)` — the transition
    /// guest's Init-time evidence that its window fingerprint occurs in this
    /// commitment at exactly that offset.
    fn fingerprint_slice_witness(
        &self,
        window_start: usize,
        window_len: usize,
    ) -> FingerprintSliceWitness;
}

/// Leaf `i` of the fingerprint-block tree: `sha256(bits[i].to_le_bytes())`.
pub fn fingerprint_block_leaf(block: u64) -> Bytes {
    Bytes(sha256_bytes(&block.to_le_bytes()))
}

/// The packed-block index range `[first, last]` covering fingerprint items
/// `[window_start, window_start + window_len)` at `bits_per_item`.
pub fn fingerprint_block_range(
    bits_per_item: usize,
    window_start: usize,
    window_len: usize,
) -> (usize, usize) {
    debug_assert!(window_len > 0);
    let first = (window_start * bits_per_item) / 64;
    let last = ((window_start + window_len) * bits_per_item - 1) / 64;
    (first, last)
}

/// Merkle root over the fingerprint's packed `u64` blocks.
pub fn fingerprint_blocks_root(bits: &[u64]) -> Vec<u8> {
    assert!(
        !bits.is_empty(),
        "a trace commitment fingerprint is never empty"
    );
    let mut tree = TraceTree::new(1);
    for block in bits {
        tree.append(fingerprint_block_leaf(*block));
    }
    tree.root(0).expect("fingerprint blocks root").0
}

impl TraceCommitmentExt for TraceCommitment {
    fn build(trace: &Trace, seed: &[u8], fraud_proof_config: FraudProofConfig) -> TraceCommitment {
        assert!(
            trace.len() > fraud_proof_config.window_size,
            "Trace length can't be less than verification window"
        );

        let revealed_items = trace[..fraud_proof_config.window_size].to_vec();

        let items_hashes: Vec<Vec<u8>> = trace.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut fingerprint_acc =
            FingerprintAccumulator::new(BitPacker(fraud_proof_config.bits_per_item));

        // The tail's roots are revealed in full. Captured on the walk that
        // already computes them, so there is no second pass over the trace.
        let tail_start = items_hashes.len() - fraud_proof_config.window_size;
        let mut revealed_tail_roots = Vec::with_capacity(fraud_proof_config.window_size);

        for (index, item_hash) in items_hashes.iter().enumerate() {
            trace_tree.append(Bytes(item_hash.clone()));
            if let Some(root) = trace_tree.root(0) {
                if index >= tail_start {
                    revealed_tail_roots.push(root.0.clone());
                }
                fingerprint_acc.append(&root.0);
            }
        }

        let fingerprint = fingerprint_acc.into_fingerprint();

        TraceCommitment {
            fingerprint,
            revealed_items,
            revealed_tail_roots,
        }
    }

    /// Try to create a commitment from items, returning an error if the trace
    /// is empty or too short for the fraud-proof window.
    fn try_build(
        trace: &Trace,
        seed: &[u8],
        fraud_proof_config: FraudProofConfig,
    ) -> Result<TraceCommitment> {
        if trace.is_empty() {
            return Err(BitPackerError::EmptyTrace);
        }
        if trace.len() <= fraud_proof_config.window_size {
            return Err(BitPackerError::InvalidWindow(format!(
                "Trace has {} steps but fraud proof window size {} requires at least {}; \
                 use a smaller window size",
                trace.len(),
                fraud_proof_config.window_size,
                fraud_proof_config.window_size + 1
            )));
        }
        Ok(Self::build(trace, seed, fraud_proof_config))
    }

    /// Check structural consistency of a (possibly untrusted) deserialized
    /// commitment, so the verifier can reject a malformed file instead of
    /// panicking on it.
    ///
    /// This does not prove the commitment is honest — catching wrong hashes
    /// or fingerprints is verification's job — only that its fields are
    /// consistent with each other.
    fn validate(&self) -> Result<()> {
        let bits_per_item = self.fingerprint.bits_per_item();
        if !(MIN_BITS_PER_ITEM..=MAX_BITS_PER_ITEM).contains(&bits_per_item) {
            return Err(BitPackerError::InvalidCommitment(format!(
                "Fingerprint claims {} bits per item, expected between {} and {}",
                bits_per_item, MIN_BITS_PER_ITEM, MAX_BITS_PER_ITEM
            )));
        }

        let window_size = self.window_size();
        if window_size == 0 || window_size > MAX_FRAUD_PROOF_WINDOW_SIZE {
            return Err(BitPackerError::InvalidCommitment(format!(
                "Commitment reveals {} items, expected between 1 and {}",
                window_size, MAX_FRAUD_PROOF_WINDOW_SIZE
            )));
        }

        // The fingerprint holds one entry per trace step and a valid
        // commitment requires the trace to be strictly longer than the
        // window, so the window can never outgrow the fingerprint.
        if self.fingerprint.len() <= window_size {
            return Err(BitPackerError::InvalidCommitment(format!(
                "Fingerprint covers {} trace steps but the fraud-proof window \
                 reveals {} items and requires at least {}",
                self.fingerprint.len(),
                window_size,
                window_size + 1
            )));
        }

        let expected_blocks = (self.fingerprint.len() * bits_per_item).div_ceil(64);
        if self.fingerprint.bits.len() != expected_blocks {
            return Err(BitPackerError::InvalidCommitment(format!(
                "Fingerprint claims {} items of {} bits ({} packed blocks) but holds {} blocks",
                self.fingerprint.len(),
                bits_per_item,
                expected_blocks,
                self.fingerprint.bits.len()
            )));
        }

        // The revealed tail covers exactly the final window.
        if self.revealed_tail_roots.len() != window_size {
            return Err(BitPackerError::InvalidCommitment(format!(
                "Commitment reveals {} tail roots but the fraud-proof window covers {}",
                self.revealed_tail_roots.len(),
                window_size
            )));
        }

        // Each revealed root must squeeze to the fingerprint entry already
        // committed at its index. The roots are strictly more information than
        // the entries — the entries are derived from them — so this is what
        // stops a commitment carrying a tail that contradicts its own
        // fingerprint. `fingerprint_value` is the same function the accumulator
        // used to produce those entries.
        let tail_start = self.fingerprint.len() - window_size;
        for (offset, root) in self.revealed_tail_roots.iter().enumerate() {
            let index = tail_start + offset;
            let committed = self
                .fingerprint
                .bits_packer
                .try_get(index, &self.fingerprint.bits)
                .map_err(|error| {
                    BitPackerError::InvalidCommitment(format!(
                        "Fingerprint has no entry at tail index {}: {}",
                        index, error
                    ))
                })?;
            let derived = fingerprint_value(root, bits_per_item);
            if derived != committed {
                return Err(BitPackerError::InvalidCommitment(format!(
                    "Revealed tail root at index {} squeezes to {} but the fingerprint \
                     commits {}",
                    index, derived, committed
                )));
            }
        }

        Ok(())
    }

    /// Get the frontier (partial Merkle path) at position n.
    ///
    /// This can be used to continue building the tree from position n.
    fn frontier(trace: &Trace, n: usize, seed: &[u8]) -> Option<TraceTreeFrontier> {
        let items_hashes: Vec<Vec<u8>> = trace.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in items_hashes.iter().take(n) {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree.frontier().cloned()
    }

    fn witness(trace: &Trace, n: usize, seed: &[u8]) -> Option<MerklePath<Bytes, 32>> {
        if n >= trace.len() {
            return None;
        }

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut marked_position = None;

        for (idx, item) in trace.iter().enumerate() {
            trace_tree.append(Bytes(item.hash()));

            if idx == n {
                // Trace item `n` is stored at Merkle position `n + 1` because
                // position `0` is reserved for the seed leaf.
                marked_position = trace_tree.mark();
            }
        }

        let marked_position = marked_position?;
        debug_assert_eq!(marked_position, Position::from(u64::try_from(n).ok()? + 1));

        let auth_path = trace_tree.witness(marked_position, 0).ok()?;
        MerklePath::from_parts(auth_path, marked_position).ok()
    }

    /// Try to get the frontier, returning an error on failure.
    fn try_frontier(trace: &Trace, n: usize, seed: &[u8]) -> Result<TraceTreeFrontier> {
        if n > trace.len() {
            return Err(BitPackerError::InvalidRange {
                start: 0,
                end: n,
                max: trace.len(),
            });
        }

        let mut items_hashes: Vec<Vec<u8>> = Vec::with_capacity(n);
        for item in trace.iter().take(n) {
            items_hashes.push(item.try_hash()?);
        }

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree
            .frontier()
            .cloned()
            .ok_or_else(|| BitPackerError::InvalidWindow("Failed to get frontier".to_string()))
    }

    fn header(&self) -> TraceCommitmentHeader {
        let revealed_bytes =
            postcard::to_allocvec(&self.revealed_items).expect("revealed items are serializable");
        let tail_roots_bytes = postcard::to_allocvec(&self.revealed_tail_roots)
            .expect("revealed tail roots are serializable");
        TraceCommitmentHeader {
            bits_packer: self.fingerprint.bits_packer,
            fingerprint_len: self.fingerprint.len() as u64,
            fingerprint_root: fingerprint_blocks_root(&self.fingerprint.bits),
            revealed_items_commitment: sha256_bytes(&revealed_bytes),
            window_size: self.window_size() as u64,
            revealed_tail_roots_commitment: sha256_bytes(&tail_roots_bytes),
        }
    }

    fn fingerprint_slice_witness(
        &self,
        window_start: usize,
        window_len: usize,
    ) -> FingerprintSliceWitness {
        assert!(window_len > 0, "empty fraud window");
        assert!(
            window_start + window_len <= self.fingerprint.len(),
            "fraud window [{}, {}) exceeds the committed fingerprint ({} items)",
            window_start,
            window_start + window_len,
            self.fingerprint.len()
        );
        let (first_block, last_block) =
            fingerprint_block_range(self.fingerprint.bits_per_item(), window_start, window_len);

        let mut tree = TraceTree::new(1);
        let mut marked = Vec::with_capacity(last_block - first_block + 1);
        for (index, block) in self.fingerprint.bits.iter().enumerate() {
            tree.append(fingerprint_block_leaf(*block));
            if (first_block..=last_block).contains(&index) {
                marked.push(tree.mark().expect("mark fingerprint block"));
            }
        }

        let blocks = (first_block..=last_block)
            .zip(marked)
            .map(|(index, position)| {
                let path = tree
                    .witness(position, 0)
                    .expect("fingerprint block witness");
                FingerprintBlockWitness {
                    block: self.fingerprint.bits[index],
                    position: u64::from(position),
                    path_elems: path.iter().map(|elem| elem.0.clone()).collect(),
                }
            })
            .collect();

        FingerprintSliceWitness { blocks }
    }

    fn diff(&self, other: &TraceCommitment) -> Option<usize> {
        assert!(
            self.fingerprint.len() == other.fingerprint.len(),
            "Trace commitetment length mismatch"
        );
        assert!(
            self.fingerprint.bits_per_item() == other.fingerprint.bits_per_item(),
            "Trace commitetment bit packing mismatch"
        );

        // TODO: did we actually need those diff parts in BitPacker?
        let Some((index, _, _)) = self
            .fingerprint
            .bits_packer
            .diff(&self.fingerprint.bits, &other.fingerprint.bits)
        else {
            return None;
        };

        Some(index)
    }
}

struct Window<T: Clone> {
    queue: VecDeque<Option<T>>,
    size: usize,
}

impl<T: Clone> Window<T> {
    fn new(size: usize) -> Self {
        Self {
            queue: VecDeque::from(vec![None; size]),
            size,
        }
    }

    fn push(&mut self, item: T) {
        self.queue.pop_front();
        self.queue.push_back(Some(item));
    }

    fn first(&self) -> Option<&T> {
        self.queue.iter().flatten().next()
    }

    fn to_vec(&self) -> Vec<T> {
        self.queue.clone().into_iter().flatten().collect()
    }

    /// Items actually held, which is `size` only once the buffer has filled.
    /// Not `self.size`: the queue always has `size` slots, pre-filled with
    /// `None` and flattened away on read, so before the window fills the
    /// capacity and the occupancy disagree — which is exactly the case a
    /// head window is.
    fn len(&self) -> usize {
        self.queue.iter().flatten().count()
    }
}

fn sequence_coordinates(step_record: &StepRecord) -> Option<(CfsCoordinates, CfsCoordinate)> {
    let coordinates = step_record.coordinates();
    let (&current_child_index, parent_coords) = coordinates.split_last()?;

    Some((CfsCoordinates(parent_coords.to_vec()), current_child_index))
}

/// Whether `record` is the step that produced `cfs_item`'s output — i.e.
/// whether a `PriorItemOutput` binding on `cfs_item` resolves to it.
fn record_produces_item(record: &StepRecord, cfs_item: &SequenceChildItem) -> bool {
    match (&record.kind, cfs_item) {
        // A tile run satisfies a recur-tile item too: an iteration of a
        // recur site is recorded as an ordinary tile run.
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(_),
                ..
            }),
            SequenceChildItem::Tile(_) | SequenceChildItem::RecurTile(_),
        ) => true,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurTile(_),
                ..
            }),
            SequenceChildItem::RecurTile(_),
        ) => true,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurSequence(_),
                ..
            }),
            SequenceChildItem::RecurSequence(_),
        ) => true,
        // A nested sequence's output is what it reported on the way out.
        (StepKind::SequenceEnd { .. }, SequenceChildItem::Sequence(_)) => true,
        _ => false,
    }
}

/// Flatten a binding into the leaf bindings a step actually depends on.
///
/// An `Indexed` binding is *two or more* dependencies, not one: the step reads
/// the value and every authorized index that located it. Flattening here rather
/// than special-casing below keeps a fraud window self-contained — omitting an
/// index's source record would leave the window unable to re-derive the read.
fn flatten_binding<'a>(binding: &'a InputBinding, out: &mut Vec<&'a InputBinding>) {
    match binding {
        InputBinding::Indexed { value, indexes } => {
            flatten_binding(value, out);
            for index in indexes {
                flatten_binding(index, out);
            }
        }
        leaf => out.push(leaf),
    }
}

/// Index of the program's `ProgramStart` step — the record that opens the root
/// frame and binds the authorized entry object at coordinates `[]`.
///
/// Unique and first by construction (`StepKind::ProgramStart`: "The trace's
/// first step"), so `position` and `rposition` agree here.
fn program_start_index(trace: &[StepRecord]) -> Option<usize> {
    trace
        .iter()
        .position(|record| matches!(record.kind, StepKind::ProgramStart(_)))
}

/// Index of the record that opened `frame`, bounding the search for sources
/// produced inside it to the invocation currently in flight.
///
/// Every frame but the root is opened by the `SequenceStart` carrying its
/// coordinates, and `rposition` is what picks the *current* invocation: a
/// sequence called more than once has one such record per entry, and only the
/// latest is open.
///
/// The root frame `[]` is the exception, and it is why scanning for a
/// `SequenceStart` alone used to fail every top-level step: no record ever
/// carries `SequenceStart` at `[]`. `ProgramStart` opens `main`'s frame
/// ("nothing else has yet" — `raster_runtime::tracing::recorder`), and the root
/// is entered exactly once, so first and last coincide.
fn frame_opening_index(trace: &[StepRecord], frame: &CfsCoordinates) -> Option<usize> {
    if frame.is_empty() {
        return program_start_index(trace);
    }

    trace.iter().rposition(|record| {
        matches!(record.kind, StepKind::SequenceStart { .. }) && record.coordinates == *frame
    })
}

/// The trace records that produced each of `declared_inputs`, so the fraud
/// window can carry a witness for every value the step read.
///
/// `step_record` must name a real CFS item in a real frame — the caller skips
/// the two kinds of step that do not (the program boundaries at `[]` and recur
/// iterations at `site ++ [i]`), because it is the caller that also has to
/// decide whether to look the item up at all. Everything below reads the last
/// coordinate as an item index within its parent frame
/// (`sequence_coordinates`), which is true exactly under that precondition.
fn resolve_inputs_sources(
    step_record: &StepRecord,
    trace: &[StepRecord],
    cfs_cursor: &CfsCursor,
    declared_inputs: &[InputBinding],
) -> Vec<(usize, StepRecord)> {
    let mut step_inputs: Vec<&InputBinding> = Vec::new();
    for binding in declared_inputs {
        flatten_binding(binding, &mut step_inputs);
    }

    if step_inputs
        .iter()
        .all(|input| matches!(input, InputBinding::Direct(InputSource::Inline)))
    {
        return Vec::new();
    }

    let Some((sequence_coordinates, item_coordinate)) = sequence_coordinates(&step_record) else {
        // Entrypoint SequenceStart/SequenceEnd
        return Vec::new();
    };

    // Find the record that opened this step's frame — `SequenceStart` for a
    // nested frame, `ProgramStart` for the root.
    let current_sequence_start_index = frame_opening_index(trace, &sequence_coordinates)
        .unwrap_or_else(|| {
            panic!(
                "Failed to resolve active sequence invocation for step {:?} in frame {:?}",
                step_record, sequence_coordinates
            )
        });

    let current_sequence_trace_suffix = &trace[current_sequence_start_index..];

    let mut source_records = Vec::new();

    for step_input in step_inputs {
        match step_input {
            InputBinding::Direct(InputSource::Inline) => {}
            InputBinding::Direct(InputSource::Storage) => {
                panic!("Direct storage bindings are not yet supported in trace source resolution");
            }
            InputBinding::EntryArgument => {
                // The source is the program's `ProgramStart` step, which bound
                // the authorized entry object at the sequence root `[]` — the
                // same record that opens the root frame above.
                let index = program_start_index(trace).unwrap_or_else(|| {
                    panic!(
                        "Failed to resolve ProgramStart source for entry-argument input of step {:?}",
                        step_record
                    )
                });
                source_records.push((index, trace[index].clone()));
            }
            InputBinding::SequenceScope { input_index } => {
                // A scope input is a value the frame's *caller* supplied, so the
                // root frame can never carry one: `main` has no caller, and the
                // compiler resolves its declared parameters to `EntryArgument`
                // instead (`FlowResolver::resolve_with_entry_arguments` — "since
                // `main` has no caller to supply them"). Asserting it names the
                // broken assumption; without it the lookup below would meet
                // `ProgramStart` where it expects a `SequenceStart` and report a
                // missing sequence input, which points at the wrong thing.
                assert!(
                    !sequence_coordinates.is_empty(),
                    "Step {:?} binds sequence-scope input {} at the root frame, but `main` \
                     has no caller to supply one; its parameters resolve to `EntryArgument`",
                    step_record,
                    input_index
                );

                let (parent_index, source_record) = current_sequence_trace_suffix
                    .first()
                    .filter(|record| {
                        matches!(record.kind, StepKind::SequenceStart { .. })
                            && record.coordinates == sequence_coordinates
                    })
                    .map(|record| (current_sequence_start_index, record.clone()))
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve sequence input {input_index} for step {:?} in frame {:?}",
                            step_record, sequence_coordinates
                        )
                    });

                source_records.push((parent_index, source_record));
            }
            InputBinding::PriorItemOutput {
                intra_sequence_item_index,
            } => {
                // 0-based index into `items` becomes a 1-based coordinate; the
                // guest crosses the same boundary in `checks::cfs`.
                let source_coordinate = CfsCoordinate::try_from(*intra_sequence_item_index)
                    .expect("Prior item output index exceeds CFS coordinate bounds")
                    + FIRST_COORDINATE;
                if source_coordinate >= item_coordinate {
                    panic!(
                        "Step {:?} cannot depend on sibling item {} from the same or a future index {}",
                        step_record, intra_sequence_item_index, item_coordinate
                    );
                }

                let mut source_record_coordinates = sequence_coordinates.clone();
                source_record_coordinates.push(source_coordinate);

                let source_record_cfs_item = cfs_cursor
                    .try_get_item(&source_record_coordinates)
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve prior item output {} for step {:?} in frame {:?}",
                            intra_sequence_item_index, step_record, sequence_coordinates
                        )
                    });

                let source_record = current_sequence_trace_suffix
                    .iter()
                    .enumerate()
                    .find(|(_, record)| {
                        record.coordinates == source_record_coordinates
                            && record_produces_item(record, source_record_cfs_item)
                    })
                    .map(|(intra_sequence_offset, record)| {
                        (
                            current_sequence_start_index + intra_sequence_offset,
                            record.clone(),
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve source record for step {:?} from source item {} at {:?}",
                            step_record,
                            intra_sequence_item_index,
                            source_record_coordinates
                        )
                    });

                source_records.push(source_record);
            }
            // Flattened away above: `Indexed` is never a leaf here.
            InputBinding::Indexed { .. } => {
                unreachable!("flatten_binding removes Indexed before this match")
            }
        }
    }

    source_records
}

fn witness_record_inputs(
    trace: &Trace,
    window_end_index: usize,
    fraud_window: &TraceWindow,
    cfs_cursor: &CfsCursor,
    seed: &[u8],
) -> HashMap<(u64, StepRecord), Vec<u8>> {
    let window_start_index = window_end_index + 1 - fraud_window.items.len();
    let mut source_records_witnesses: HashMap<(u64, StepRecord), Vec<u8>> = HashMap::new();

    for (offset, step_record) in fraud_window.items.iter().enumerate() {
        // The two steps that bind no CFS inputs, skipped in the order the guest
        // skips them (`checks::cfs::verify_step_record_inputs`). Both sides must
        // agree on which steps have inputs to resolve: a witness the guest never
        // reads is dead weight in the evidence, and a witness it reads and does
        // not get is a panic.
        if step_record.coordinates().is_empty() {
            // The root coordinate holds only the program boundaries —
            // `ProgramStart`, `ProgramEnd` and `main`'s `SequenceEnd`. None is a
            // CFS item, so none binds CFS inputs to resolve.
            continue;
        }

        if cfs_cursor
            .try_get_recur_iteration_coordinates(step_record.coordinates())
            .is_some()
        {
            // An iteration of a recur site is not a CFS item either: `site ++ [i]`
            // addresses a run of the site, and the site's own bindings are resolved
            // once at `[site]`. What an iteration must prove instead is chunking
            // and recur progress, which the guest checks against its replay journal
            // (`verify_recur_iteration_chunking`, `advance_recur_progress`) and
            // which needs no source record.
            //
            // Skipping *before* `try_get_item` is the point: that lookup folds
            // iteration coordinates back to the site (`CfsCursor::try_get_item`),
            // so resolving here would hand `resolve_inputs_sources` the site's
            // bindings under the iteration's coordinates — a frame of `[site]` and
            // an item index of `i`, which is an iteration counter, not a sibling
            // index.
            continue;
        }

        let cfs_item = cfs_cursor
            .try_get_item(step_record.coordinates())
            .unwrap_or_else(|| {
                panic!(
                    "Failed to resolve cfs item for fraud window coordinates: {:?}",
                    step_record.coordinates()
                )
            });
        let step_index = window_start_index + offset;

        for (trace_index, source_record) in resolve_inputs_sources(
            step_record,
            &trace[..step_index],
            cfs_cursor,
            cfs_item.inputs(),
        ) {
            let trace_prefix = Trace(trace[..step_index].to_vec());
            let merkle_path = TraceCommitment::witness(&trace_prefix, trace_index, seed)
                .expect("Failed to derive merkle path for source record");
            let witness_bytes = postcard::to_allocvec(&StepRecordWitness {
                position: u64::from(merkle_path.position()),
                path_elems: merkle_path
                    .path_elems()
                    .iter()
                    .map(|elem| elem.0.clone())
                    .collect(),
            })
            .expect("Failed to serialize source record witness");

            // Keyed by the *verifying* step as well as the source, because the
            // witness is only valid at that step's trace root. Two window steps
            // resolving the same source get two witnesses; keyed by the source
            // alone, this `insert` overwrote the earlier step's and left it
            // folding a proof built over a longer prefix than its own root.
            source_records_witnesses.insert((step_record.exec_index, source_record), witness_bytes);
        }
    }

    source_records_witnesses
}

pub struct TraceVerifier<'a> {
    pub trace_commitment: TraceCommitment,
    pub cfs: &'a ControlFlowSchema,
    pub seed: Vec<u8>,

    pub window_size: usize,
    pub fingerprint_acc: FingerprintAccumulator,
    pub latest_frontier: TraceTreeFrontier,

    pub window_frontiers: Window<TraceTreeFrontier>,
    pub window_items: Window<StepRecord>,
}

#[derive(Debug, Clone)]
pub struct FraudEvidence {
    pub window: TraceWindow,
    pub input_sources_witnesses: HashMap<(u64, StepRecord), Vec<u8>>,
}

pub enum VerificationResult {
    Ok,
    Fraud(FraudEvidence),
}

impl<'a> TraceVerifier<'a> {
    pub fn new(
        trace_commitment: TraceCommitment,
        seed: &[u8],
        cfs: &'a ControlFlowSchema,
    ) -> Result<Self> {
        trace_commitment.validate()?;

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let init_frontier = trace_tree.frontier().cloned().unwrap();

        let bit_packer = trace_commitment.fingerprint.bits_packer.clone();
        let fingerprint_acc = FingerprintAccumulator::new(bit_packer);

        // The commitment carries the fraud-proof window parameters it was
        // built with: the window size as the number of revealed items and the
        // bits per item inside the fingerprint's bit packer.
        let window_size = trace_commitment.window_size();

        let mut window_frontiers: Window<TraceTreeFrontier> = Window::new(window_size);
        let window_items: Window<StepRecord> = Window::new(window_size);

        window_frontiers.push(init_frontier.clone());

        Ok(Self {
            trace_commitment,
            cfs,
            seed: seed.to_vec(),

            window_size,
            fingerprint_acc,
            latest_frontier: init_frontier,

            window_frontiers,
            window_items,
        })
    }

    /// The committed trace root revealed for `index`, or `None` when `index`
    /// falls outside the final window.
    ///
    /// `validate` has already established that the commitment reveals exactly
    /// `window_size` roots and that the fingerprint is longer than the window,
    /// so the subtraction cannot underflow.
    fn revealed_tail_root_at(&self, index: usize) -> Option<&[u8]> {
        let tail_start = self
            .trace_commitment
            .fingerprint
            .len()
            .checked_sub(self.trace_commitment.revealed_tail_roots.len())?;
        index
            .checked_sub(tail_start)
            .and_then(|offset| self.trace_commitment.revealed_tail_roots.get(offset))
            .map(|root| root.as_slice())
    }

    pub fn verify(&mut self, trace: &Trace) -> VerificationResult {
        let cfs_cursor = CfsCursor::new(self.cfs.clone());

        for (step_index, step_record) in trace.iter().enumerate() {
            let item_frontier = self.latest_frontier.clone();

            self.window_frontiers.push(item_frontier);
            self.window_items.push(step_record.clone());

            let step_record_hash = step_record.hash();
            self.latest_frontier.append(Bytes(step_record_hash));

            let root = Bytes(frontier_root(&self.latest_frontier));

            self.fingerprint_acc.append(&root.0);

            let latest_fingerprint = self.fingerprint_acc.clone().into_fingerprint();

            let index = latest_fingerprint.len() - 1;

            // Across the final window the committed roots are revealed in full,
            // so compare those: detection there is exact rather than
            // `bits_per_item` bits, which at `window_size >= 128` was one bit.
            // Elsewhere the packed entry is all there is.
            //
            // Root equality implies entry equality — the entry is derived from
            // the root — so this strictly replaces the weaker test rather than
            // sitting alongside it.
            let diverges = match self.revealed_tail_root_at(index) {
                Some(committed_root) => committed_root != root.0.as_slice(),
                None => latest_fingerprint.bits_packer.diff_at_index(
                    index,
                    &latest_fingerprint.bits,
                    &self.trace_commitment.fingerprint.bits,
                ),
            };

            if diverges {
                // The rolling buffers *measure* the window; this slice has to
                // reproduce what they already know. `index.saturating_sub(w) + 1`
                // did not: below `w` the `saturating_sub` floors to 0 and the
                // `+ 1` lands on **1**, giving one entry too few *and* starting
                // one item late, so window item 0 was compared against
                // committed item 1. At `index == 0` the range was `1..1` and
                // the slice came back empty while still declaring `w` items.
                //
                // Clamping instead is identical for `index >= w` and yields 0
                // at the head, which is where the window genuinely starts.
                let window_len = self.window_items.len();
                let diff_bits = self
                    .trace_commitment
                    .fingerprint
                    .bits_packer
                    .get_range(
                        (index + 1).saturating_sub(self.window_size),
                        index + 1,
                        &self.trace_commitment.fingerprint.bits,
                    )
                    .unwrap();

                // Declare what the slice holds, not the window's capacity.
                // `Fingerprint::from` stores `len` verbatim without checking it
                // against `bits`, and the guest reads that declared length as
                // authoritative — which is how a head window used to travel all
                // the way there claiming items it did not carry.
                assert_eq!(
                    diff_bits.len(),
                    (window_len * self.trace_commitment.fingerprint.bits_per_item()).div_ceil(64),
                    "fraud window slice holds {} blocks but declares {} items",
                    diff_bits.len(),
                    window_len,
                );
                let window_fingerprint = Fingerprint::from(
                    diff_bits,
                    self.trace_commitment.fingerprint.bits_packer,
                    window_len,
                );

                let window_frontier = self.window_frontiers.first().unwrap().clone();
                let ser_window_frontier =
                    serializable_frontier_from_trace_frontier(window_frontier).to_bytes();

                // TODO: consider renaming Window struct and TraceWindow have different behavior but
                // similiar naming
                let fraud_window = TraceWindow {
                    frontier: ser_window_frontier,
                    items: self.window_items.to_vec(),
                    fingerprint: window_fingerprint,
                };

                let input_sources_witnesses = witness_record_inputs(
                    trace,
                    step_index,
                    &fraud_window,
                    &cfs_cursor,
                    &self.seed,
                );

                return VerificationResult::Fraud(FraudEvidence {
                    window: fraud_window,
                    input_sources_witnesses,
                });
            }
        }

        VerificationResult::Ok
    }

    /// The window covering the trace's final `window_size` items, paired with
    /// the *committed* fingerprint slice over that same range.
    ///
    /// Not a fraud path — no divergence is sought, and none is expected: the
    /// caller builds the commitment and the trace from the same run, so the
    /// slice matches by construction. What this produces is the window a
    /// **terminal-window receipt** is proven over: the one whose last step is
    /// `ProgramEnd`, and which the guest recognises as terminal because it
    /// ends exactly where the committed fingerprint ends (see
    /// `TransitionJournal::window_is_terminal` and
    /// `docs/proposals/chain-io-commitment.md`).
    ///
    /// Shares `verify`'s walk deliberately: the trailing `Window` buffers, the
    /// frontier alignment (`window_frontiers.first()` is the frontier *before*
    /// the first window item) and the input-source witnessing are the same
    /// mechanism, and both feed the same prover.
    ///
    /// They differ in exactly one way, and it is worth naming because this
    /// comment used to deny it: a terminal window is **always** `window_size`
    /// items — the trace is refused outright if it is shorter — while a fraud
    /// window is shorter than that whenever the divergence falls inside the
    /// trace's first `window_size` steps. Such a window opens at trace index 0,
    /// where the guest asserts the genesis opening state instead of relying on
    /// a pre-divergence margin it cannot have.
    pub fn terminal_window(&mut self, trace: &Trace) -> Result<FraudEvidence> {
        if trace.len() < self.window_size {
            return Err(BitPackerError::InvalidWindow(format!(
                "Trace has {} steps but a terminal window needs at least {}",
                trace.len(),
                self.window_size
            )));
        }

        let cfs_cursor = CfsCursor::new(self.cfs.clone());

        for step_record in trace.iter() {
            self.window_frontiers.push(self.latest_frontier.clone());
            self.window_items.push(step_record.clone());

            self.latest_frontier.append(Bytes(step_record.hash()));

            let root = Bytes(frontier_root(&self.latest_frontier));
            self.fingerprint_acc.append(&root.0);
        }

        let last_index = trace.len() - 1;
        let window_start = trace.len() - self.window_size;
        let committed_bits = self
            .trace_commitment
            .fingerprint
            .bits_packer
            .get_range(
                window_start,
                last_index + 1,
                &self.trace_commitment.fingerprint.bits,
            )
            .ok_or_else(|| {
                BitPackerError::InvalidWindow(
                    "Committed fingerprint is shorter than the trace".to_string(),
                )
            })?;
        let window_fingerprint = Fingerprint::from(
            committed_bits,
            self.trace_commitment.fingerprint.bits_packer,
            self.window_size,
        );

        let window_frontier = self
            .window_frontiers
            .first()
            .expect("a walked trace leaves at least one window frontier")
            .clone();
        let window = TraceWindow {
            frontier: serializable_frontier_from_trace_frontier(window_frontier).to_bytes(),
            items: self.window_items.to_vec(),
            fingerprint: window_fingerprint,
        };

        let input_sources_witnesses =
            witness_record_inputs(trace, last_index, &window, &cfs_cursor, &self.seed);

        Ok(FraudEvidence {
            window,
            input_sources_witnesses,
        })
    }
}

#[cfg(test)]
mod tests {
    use raster_core::cfs::{
        CfsCoordinate, CfsCoordinates, InputBinding, RecurTileItem, SequenceChildItem, SequenceDef,
        SequenceItem, TileDef, TileItem,
    };
    use raster_core::trace::{ProgramStartStep, StorageRoots};

    use super::*;
    use crate::precomputed;

    /// Window config used by the tests: wide enough fingerprint bits to make
    /// fraud detection deterministic on the small fixed traces below.
    fn test_fraud_proof_config() -> FraudProofConfig {
        FraudProofConfig {
            window_size: 2,
            bits_per_item: 16,
        }
    }

    #[test]
    fn test_fraud_proof_config_from_window_size() {
        for (window_size, expected_bits) in [(2, 64), (32, 4), (128, 1), (256, 1), (1024, 1)] {
            let config = FraudProofConfig::from_window_size(window_size)
                .expect("power-of-two window size within limit");
            assert_eq!(config.window_size, window_size);
            assert_eq!(config.bits_per_item, expected_bits);
            assert!(config.window_size * config.bits_per_item >= FRAUD_DETECTION_SECURITY_BITS);
        }

        // 1 is a power of two but would require 128 bits per item, beyond the
        // bit packer's u64 blocks.
        for window_size in [0, 1, 3, 100, 2048] {
            assert!(matches!(
                FraudProofConfig::from_window_size(window_size),
                Err(BitPackerError::InvalidWindow(_))
            ));
        }
    }

    /// Helper function to create a step record for testing.
    fn make_tile_trace_item(input: u64, output: u64) -> StepRecord {
        make_tile_trace_item_at(
            input,
            "test_sequence",
            input as CfsCoordinate,
            vec![1],
            format!("test_tile_{input}"),
            1,
            output,
        )
    }

    fn make_tile_trace_item_at(
        exec_index: u64,
        sequence_id: &str,
        intra_sequence_index: CfsCoordinate,
        coordinates: Vec<CfsCoordinate>,
        fn_name: String,
        _input_count: usize,
        output: u64,
    ) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(fn_name.to_string()),
                intra_sequence_index,
                input_commitment: Vec::new(),
                input_source_commitment: Vec::new(),
                output_commitment: output.to_le_bytes().to_vec(),
                storage: empty_storage_roots(),
            }),
            recur_progress_commitment: [0u8; 32],
            recur_state: None,
        }
    }

    fn empty_storage_roots() -> StorageRoots {
        StorageRoots {
            root_before: Vec::new(),
            root_after: Vec::new(),
            index_root_before: Vec::new(),
            index_root_after: Vec::new(),
        }
    }

    /// The step that opens `main`'s frame, as the recorder emits it: always
    /// first, always at the root coordinate `[]`, and never a `SequenceStart`.
    ///
    /// Fixtures must use this rather than a `SequenceStart` at `[]`, which the
    /// recorder does not produce — a trace shaped that way hides every defect
    /// keyed on how the root frame is opened.
    fn make_program_start_record(exec_index: u64, entry_arguments: Vec<String>) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::ProgramStart(ProgramStartStep {
                entry_arguments,
                output_commitment: Vec::new(),
                storage: empty_storage_roots(),
            }),
            recur_progress_commitment: [0u8; 32],
            recur_state: None,
        }
    }

    fn make_sequence_start_record(
        exec_index: u64,
        sequence_id: &str,
        coordinates: Vec<CfsCoordinate>,
        _input_count: usize,
    ) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::SequenceStart {
                input_commitment: Vec::new(),
                input_source_commitment: Vec::new(),
            },
            recur_progress_commitment: [0u8; 32],
            recur_state: None,
        }
    }

    fn make_sequence_end_record(
        exec_index: u64,
        sequence_id: &str,
        coordinates: Vec<CfsCoordinate>,
    ) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::SequenceEnd {
                output_commitment: Vec::new(),
            },
            recur_progress_commitment: [0u8; 32],
            recur_state: None,
        }
    }

    fn make_test_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("test_tile", 1, 1));
        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "test_tile".to_string(),
            sources: vec![InputBinding::inline()],
        }));
        cfs.sequences.push(main);
        cfs
    }

    fn make_producer_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("producer", 1, 1));
        cfs.tiles.push(TileDef::iter("consumer", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "producer".to_string(),
            sources: vec![InputBinding::inline()],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "consumer".to_string(),
            sources: vec![InputBinding::prior_item_output(0)],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::inline()],
        }));

        cfs.sequences.push(main);
        cfs
    }

    /// A scope binding where the compiler can actually put one: on an item of a
    /// *nested* frame, reading that frame's own parameter.
    ///
    /// It used to sit at `[0]` — a `SequenceScope` on an item of `main` — which
    /// the compiler never emits, because `main` has no caller and its
    /// parameters resolve to `EntryArgument`
    /// (`FlowResolver::resolve_with_entry_arguments`). Unreachable there, so it
    /// left the `SequenceScope` arm of `resolve_inputs_sources` untested while
    /// appearing to cover it.
    fn make_sequence_input_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("inner_tile", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.entry_arguments = vec!["arg".to_string()];
        main.items.push(SequenceChildItem::Sequence(SequenceItem {
            id: "inner".to_string(),
            sources: vec![InputBinding::entry_argument()],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::inline()],
        }));

        let mut inner = SequenceDef::new("inner");
        inner.input_sources = vec![InputBinding::inline()];
        inner.items.push(SequenceChildItem::Tile(TileItem {
            id: "inner_tile".to_string(),
            // `inner`'s own parameter 0 — supplied by its caller, `main`.
            sources: vec![InputBinding::seq_input(0)],
        }));

        cfs.sequences.push(main);
        cfs.sequences.push(inner);
        cfs
    }

    fn make_nested_sequence_output_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("inner_tile", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.input_sources = vec![InputBinding::inline()];
        main.items.push(SequenceChildItem::Sequence(SequenceItem {
            id: "inner".to_string(),
            sources: vec![InputBinding::seq_input(0)],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::prior_item_output(0)],
        }));

        let mut inner = SequenceDef::new("inner");
        inner.input_sources = vec![InputBinding::inline()];
        inner.items.push(SequenceChildItem::Tile(TileItem {
            id: "inner_tile".to_string(),
            sources: vec![InputBinding::inline()],
        }));

        cfs.sequences.push(main);
        cfs.sequences.push(inner);
        cfs
    }

    #[test]
    fn trace_should_be_not_equal() {
        let items = Trace(vec![
            make_tile_trace_item(0, 0),
            make_tile_trace_item(1, 1),
            make_tile_trace_item(2, 2),
            make_tile_trace_item(3, 3),
            make_tile_trace_item(4, 4),
        ]);

        let ref_items = Trace(vec![
            make_tile_trace_item(0, 0),
            make_tile_trace_item(1, 1),
            make_tile_trace_item(5, 5), // Different
            make_tile_trace_item(3, 3),
            make_tile_trace_item(4, 4),
        ]);

        let binded_trace = TraceCommitment::build(
            &items,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let ref_binded_trace = TraceCommitment::build(
            &ref_items,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );

        // Check that different items produce different commitments
        assert_ne!(binded_trace.fingerprint, ref_binded_trace.fingerprint);
    }

    #[test]
    fn test_trace_item_hash() {
        let item = make_tile_trace_item(1, 2);
        let hash = item.hash();
        assert_eq!(hash.len(), 32); // SHA256 produces 32 bytes
    }

    #[test]
    fn test_try_from_empty_trace() {
        let items = Trace::new();
        let result = TraceCommitment::try_build(
            &items,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        assert!(matches!(result, Err(BitPackerError::EmptyTrace)));
    }

    #[test]
    fn test_try_from_trace_shorter_than_window() {
        // The trace must be strictly longer than the window, so both a
        // shorter and an equal-length trace are rejected.
        for trace_len in [1, 2] {
            let items = Trace((0..trace_len).map(|i| make_tile_trace_item(i, i)).collect());
            let result = TraceCommitment::try_build(
                &items,
                &precomputed::EMPTY_TRIE_NODES[0],
                test_fraud_proof_config(),
            );
            assert!(matches!(result, Err(BitPackerError::InvalidWindow(_))));
        }
    }

    /// Test that guest-style compute_root matches bridgetree's root for various frontiers.
    #[test]
    fn test_compute_root_matches_bridgetree() {
        fn empty_at_level(level: u8) -> Vec<u8> {
            if level == 0 {
                return precomputed::EMPTY_TRIE_NODES[0].to_vec();
            }
            let child = empty_at_level(level - 1);
            combine_level(level - 1, &child, &child)
        }

        fn combine_level(level: u8, left: &[u8], right: &[u8]) -> Vec<u8> {
            let mut data = Vec::with_capacity(1 + 32 + 32);
            data.push(level);
            data.extend_from_slice(left);
            data.extend_from_slice(right);
            sha256_bytes(&data)
        }

        fn compute_root_guest(position: u64, leaf: &[u8], ommers: &[Vec<u8>]) -> Vec<u8> {
            let mut cur = leaf.to_vec();
            let mut ommer_idx = 0;
            for level in 0u8..32 {
                let bit = (position >> level) & 1;
                if bit == 0 {
                    cur = combine_level(level, &cur, &empty_at_level(level));
                } else {
                    let left = if ommer_idx < ommers.len() {
                        ommers[ommer_idx].clone()
                    } else {
                        empty_at_level(level)
                    };
                    cur = combine_level(level, &left, &cur);
                    ommer_idx += 1;
                }
            }
            cur
        }

        let seed = precomputed::EMPTY_TRIE_NODES[0];
        let items: Vec<StepRecord> = (0..10).map(|i| make_tile_trace_item(i, i)).collect();

        let mut tree = TraceTree::new(1);
        tree.append(Bytes(seed.to_vec()));

        for (i, item) in items.iter().enumerate() {
            tree.append(Bytes(item.hash()));
            let bridgetree_root = tree.root(0).expect("root").0.clone();

            let frontier = tree.frontier().expect("frontier").clone();
            let ser_frontier = serializable_frontier_from_trace_frontier(frontier.clone());
            let deser_frontier = serializable_frontier_into_trace_frontier(ser_frontier)
                .expect("Can't deserialize frontier");

            let pos = u64::from(deser_frontier.position());
            let leaf = deser_frontier.leaf().0.clone();
            let ommers: Vec<Vec<u8>> = deser_frontier
                .ommers()
                .iter()
                .map(|o| o.0.clone())
                .collect();

            let guest_root = compute_root_guest(pos, &leaf, &ommers);

            assert_eq!(
                bridgetree_root,
                guest_root,
                "Root mismatch at position {} (after {} items)",
                pos,
                i + 1
            );
        }
    }

    #[test]
    fn test_wittnes_returns_proof_for_requested_trace_item() {
        let seed = precomputed::EMPTY_TRIE_NODES[0];
        let trace = Trace((0..6).map(|i| make_tile_trace_item(i, i)).collect());

        let witness = TraceCommitment::witness(&trace, 2, &seed).expect("witness");

        assert_eq!(u64::from(witness.position()), 3);

        let mut tree = TraceTree::new(1);
        tree.append(Bytes(seed.to_vec()));
        for item in trace.iter() {
            tree.append(Bytes(item.hash()));
        }

        let expected_root = tree.root(0).expect("root");
        let witnessed_leaf = Bytes(trace[2].hash());

        assert_eq!(witness.root(witnessed_leaf), expected_root);
    }

    #[test]
    fn test_verify_trace_returns_ok_for_matching_trace() {
        let trace = Trace((0..5).map(|i| make_tile_trace_item(i, i)).collect());
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_test_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    /// The terminal window must satisfy, on the host, the exact equality the
    /// guest re-derives at `Init` to set `window_is_terminal`:
    /// `init_frontier.position + window.fingerprint.len() == fingerprint_len`.
    ///
    /// Asserting it here is what keeps `prove_terminal_window` honest without
    /// running a proof: if the frontier alignment or the slice range drifts,
    /// the guest would silently call the window non-terminal and every
    /// terminal-window receipt would stop being admissible.
    #[test]
    fn terminal_window_ends_where_the_committed_fingerprint_ends() {
        let trace = Trace((0..8).map(|i| make_tile_trace_item(i, i)).collect());
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let fingerprint_len = trace_commitment.fingerprint.len();
        let window_size = trace_commitment.window_size();
        let cfs = make_test_cfs();
        let mut trace_verifier = TraceVerifier::new(
            trace_commitment,
            &precomputed::EMPTY_TRIE_NODES[0],
            &cfs,
        )
        .expect("valid commitment");

        let evidence = trace_verifier
            .terminal_window(&trace)
            .expect("terminal window");

        // The window is the trace's tail.
        assert_eq!(evidence.window.items.len(), window_size);
        assert_eq!(
            evidence.window.items.last().expect("non-empty window"),
            trace.last().expect("non-empty trace"),
        );

        // The guest's terminality equality, computed the guest's way.
        let frontier = SerializableFrontier::from_bytes(&evidence.window.frontier)
            .expect("window frontier deserializes");
        let window_start = usize::try_from(frontier.position).expect("position fits");
        assert_eq!(
            window_start + evidence.window.fingerprint.len(),
            fingerprint_len,
            "terminal window must end where the committed fingerprint ends",
        );
    }

    /// A trace shorter than one window has no terminal window to prove.
    #[test]
    fn terminal_window_rejects_a_trace_shorter_than_the_window() {
        let trace = Trace((0..8).map(|i| make_tile_trace_item(i, i)).collect());
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_test_cfs();
        let mut trace_verifier = TraceVerifier::new(
            trace_commitment,
            &precomputed::EMPTY_TRIE_NODES[0],
            &cfs,
        )
        .expect("valid commitment");

        let short = Trace(trace.iter().take(1).cloned().collect());
        assert!(trace_verifier.terminal_window(&short).is_err());
    }

    /// Window geometry across the head boundary.
    ///
    /// Three channels must describe the same range of trace indices: the
    /// frontier says *where* the window starts, the declared length says *how
    /// many* items, and the bits say *what* the committed values are. The
    /// buffers measure the first two; the slice computes the third and has to
    /// agree.
    ///
    /// The offset assertion is the load-bearing one. A length-only check passes
    /// on a slice that is short *and* shifted, which is exactly what the old
    /// `index.saturating_sub(w) + 1` produced below `w`.
    #[test]
    fn fraud_window_geometry_agrees_across_the_head_boundary() {
        let seed = precomputed::EMPTY_TRIE_NODES[0];
        let config = test_fraud_proof_config();
        let window_size = config.window_size;
        let committed = Trace((0..12).map(|i| make_tile_trace_item(i, i)).collect());
        let commitment = TraceCommitment::build(&committed, &seed, config);
        let cfs = make_test_cfs();

        for divergence in 0..=window_size + 1 {
            let mut runtime = committed.clone();
            runtime.0[divergence] = make_tile_trace_item(divergence as u64, 900 + divergence as u64);

            let mut verifier = TraceVerifier::new(commitment.clone(), &seed, &cfs)
                .expect("valid commitment");
            let VerificationResult::Fraud(evidence) = verifier.verify(&runtime) else {
                panic!("expected a divergence at index {divergence}");
            };

            let window = &evidence.window;
            let expected_len = (divergence + 1).min(window_size);
            let window_start = SerializableFrontier::from_bytes(&window.frontier)
                .expect("window frontier")
                .position as usize;

            // How many: the declared length is the measured item count.
            assert_eq!(window.items.len(), expected_len, "items at {divergence}");
            assert_eq!(window.fingerprint.len(), expected_len, "declared at {divergence}");
            assert_eq!(
                window.fingerprint.bits.len(),
                (expected_len * config.bits_per_item).div_ceil(64),
                "blocks actually held at {divergence}",
            );

            // Where: the frontier and the slice agree on the first item.
            assert_eq!(window_start, divergence + 1 - expected_len, "start at {divergence}");
            let committed_at_start = commitment
                .fingerprint
                .bits_packer
                .try_get(window_start, &commitment.fingerprint.bits)
                .expect("committed entry at the window's start");
            let window_at_zero = window
                .fingerprint
                .bits_packer
                .try_get(0, &window.fingerprint.bits)
                .expect("window entry 0");
            assert_eq!(
                window_at_zero, committed_at_start,
                "window item 0 must be the committed entry at the frontier's position ({divergence})",
            );
        }
    }

    /// The cumulative root after every step of `trace`.
    fn final_trace_root(trace: &Trace, seed: &[u8]) -> Vec<u8> {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(seed.to_vec()));
        for item in trace.iter() {
            tree.append(Bytes(item.hash()));
        }
        tree.root(0).expect("trace root").0
    }

    #[test]
    fn build_reveals_the_final_window_of_trace_roots() {
        let trace = Trace((0..10).map(|i| make_tile_trace_item(i, i)).collect());
        let config = test_fraud_proof_config();
        let commitment =
            TraceCommitment::build(&trace, &precomputed::EMPTY_TRIE_NODES[0], config);

        assert_eq!(commitment.revealed_tail_roots.len(), config.window_size);
        // The last revealed root is the trace's final root.
        assert_eq!(
            *commitment.revealed_tail_roots.last().unwrap(),
            final_trace_root(&trace, &precomputed::EMPTY_TRIE_NODES[0]),
        );
        // And the commitment is internally consistent.
        commitment.validate().expect("freshly built commitment");
    }

    /// A commitment whose revealed roots contradict its own fingerprint is
    /// unrepresentable — the roots are strictly more information than the
    /// entries they squeeze to, so the two can be held against each other.
    #[test]
    fn validate_rejects_a_tail_root_that_contradicts_the_fingerprint() {
        let trace = Trace((0..10).map(|i| make_tile_trace_item(i, i)).collect());
        let mut commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );

        // Flip enough bits to change the squeezed value, not just the root.
        commitment.revealed_tail_roots[0] = vec![0xFF; 32];

        assert!(matches!(
            commitment.validate(),
            Err(BitPackerError::InvalidCommitment(_))
        ));
    }

    #[test]
    fn validate_rejects_a_tail_that_does_not_cover_the_window() {
        let trace = Trace((0..10).map(|i| make_tile_trace_item(i, i)).collect());
        let mut commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );

        commitment.revealed_tail_roots.pop();

        assert!(matches!(
            commitment.validate(),
            Err(BitPackerError::InvalidCommitment(_))
        ));
    }

    /// The case the packed fingerprint cannot see.
    ///
    /// At `window_size = 128`, `bits_per_item` is 1, so a divergence in the
    /// trace's final step has exactly one bit of evidence — it survived audit
    /// half the time. This deliberately searches for a tamper whose squeezed
    /// bit *collides* with the honest one, which is precisely the case the old
    /// detection missed, and asserts it is now caught.
    #[test]
    fn a_final_step_divergence_is_detected_when_its_fingerprint_bit_collides() {
        let seed = precomputed::EMPTY_TRIE_NODES[0];
        let config = FraudProofConfig::from_window_size(128).expect("power-of-two window");
        assert_eq!(config.bits_per_item, 1, "the degenerate case this is about");

        let honest = Trace((0..130).map(|i| make_tile_trace_item(i, i)).collect());
        let last = honest.len() - 1;
        let honest_bit = fingerprint_value(&final_trace_root(&honest, &seed), config.bits_per_item);

        // A tamper the fingerprint is blind to: same final bit, different root.
        let runtime = (1_000u64..1_100)
            .find_map(|candidate| {
                let mut candidate_trace = honest.clone();
                candidate_trace.0[last] = make_tile_trace_item(last as u64, candidate);
                let root = final_trace_root(&candidate_trace, &seed);
                (fingerprint_value(&root, config.bits_per_item) == honest_bit)
                    .then_some(candidate_trace)
            })
            .expect("a colliding tamper exists at 1 bit per item");

        assert_ne!(
            final_trace_root(&runtime, &seed),
            final_trace_root(&honest, &seed),
            "the traces must actually differ, or the test proves nothing"
        );

        let commitment = TraceCommitment::build(&honest, &seed, config);
        let cfs = make_test_cfs();
        let mut verifier =
            TraceVerifier::new(commitment, &seed, &cfs).expect("valid commitment");

        assert!(
            matches!(verifier.verify(&runtime), VerificationResult::Fraud(_)),
            "a final-step divergence must be detected even when its fingerprint bit collides"
        );
    }

    #[test]
    fn test_verify_trace_returns_fraud_for_mismatched_trace() {
        let committed_trace = Trace((0..5).map(|i| make_tile_trace_item(i, i)).collect());
        let mut runtime_trace = committed_trace.clone();
        runtime_trace[2] = make_tile_trace_item(2, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_test_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let verification_result = trace_verifier.verify(&runtime_trace);
        assert!(matches!(verification_result, VerificationResult::Fraud(_)));
    }

    #[test]
    fn test_verifier_rejects_structurally_malformed_commitment() {
        let trace = Trace((0..5).map(|i| make_tile_trace_item(i, i)).collect());
        let cfs = make_test_cfs();
        let valid = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );

        // More revealed items than the fingerprint covers.
        let mut oversized_window = valid.clone();
        oversized_window.revealed_items = trace.iter().cloned().collect();

        // No revealed items at all.
        let mut empty_window = valid.clone();
        empty_window.revealed_items.clear();

        // Bit packer outside the supported u64 block range.
        let mut bad_bit_packer = valid.clone();
        bad_bit_packer.fingerprint.bits_packer = BitPacker(65);

        // Fingerprint claims more items than its bits actually hold.
        let mut truncated_bits = valid.clone();
        truncated_bits.fingerprint.bits.pop();

        for malformed in [
            oversized_window,
            empty_window,
            bad_bit_packer,
            truncated_bits,
        ] {
            assert!(matches!(
                TraceVerifier::new(malformed, &precomputed::EMPTY_TRIE_NODES[0], &cfs),
                Err(BitPackerError::InvalidCommitment(_))
            ));
        }
    }

    #[test]
    fn test_verify_trace_returns_ok_for_producer_dependency() {
        let trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "producer".to_string(), 1, 10),
            make_tile_trace_item_at(3, "main", 2, vec![2], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(4, "main", 3, vec![3], "tail".to_string(), 1, 30),
            make_sequence_end_record(5, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_producer_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    #[test]
    fn test_verify_trace_returns_ok_for_sequence_step_seq_input_dependency() {
        let trace = Trace(vec![
            make_program_start_record(1, vec!["arg".to_string()]),
            make_sequence_start_record(2, "inner", vec![1], 1),
            make_tile_trace_item_at(3, "inner", 1, vec![1, 1], "inner_tile".to_string(), 1, 10),
            make_sequence_end_record(4, "inner", vec![1]),
            make_tile_trace_item_at(5, "main", 2, vec![2], "tail".to_string(), 1, 20),
            make_sequence_end_record(6, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_sequence_input_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    #[test]
    fn test_verify_trace_returns_ok_for_nested_sequence_output_dependency() {
        let trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_sequence_start_record(2, "inner", vec![1], 1),
            make_tile_trace_item_at(3, "inner", 1, vec![1, 1], "inner_tile".to_string(), 1, 10),
            make_sequence_end_record(4, "inner", vec![1]),
            make_tile_trace_item_at(5, "main", 2, vec![2], "tail".to_string(), 1, 20),
            make_sequence_end_record(6, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_nested_sequence_output_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    /// `main` whose *last* item is top-level and reads a prior sibling, so the
    /// terminal window's first entry is a depth-1 step with a non-inline input.
    fn make_top_level_tail_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("producer", 1, 1));
        cfs.tiles.push(TileDef::iter("consumer", 1, 1));

        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "producer".to_string(),
            sources: vec![InputBinding::inline()],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "consumer".to_string(),
            sources: vec![InputBinding::prior_item_output(0)],
        }));

        cfs.sequences.push(main);
        cfs
    }

    /// `main` declaring an entry argument, read by a top-level item.
    fn make_top_level_entry_argument_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("consumer", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.entry_arguments = vec!["arg".to_string()];
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "consumer".to_string(),
            sources: vec![InputBinding::entry_argument()],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::inline()],
        }));

        cfs.sequences.push(main);
        cfs
    }

    /// A top-level item binding `SequenceScope`, which the compiler never emits
    /// — `main` has no caller. Only constructible by hand, which is the point.
    fn make_root_sequence_scope_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("consumer", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.input_sources = vec![InputBinding::inline()];
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "consumer".to_string(),
            sources: vec![InputBinding::seq_input(0)],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::inline()],
        }));

        cfs.sequences.push(main);
        cfs
    }

    /// A divergence whose window holds a *top-level* step with a non-inline
    /// input used to panic in evidence construction, before any window was
    /// produced: `resolve_inputs_sources` scanned for the `SequenceStart` that
    /// opened frame `[]`, and `ProgramStart` opens it.
    #[test]
    fn fraud_window_resolves_top_level_step_inputs() {
        let committed_trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "producer".to_string(), 1, 10),
            make_tile_trace_item_at(3, "main", 2, vec![2], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(4, "main", 3, vec![3], "tail".to_string(), 1, 30),
            make_sequence_end_record(5, "main", vec![]),
        ]);
        // Diverges at index 2, so the window is [producer@[0], consumer@[1]] and
        // `consumer` — depth 1, reading a prior sibling — must resolve.
        let mut runtime_trace = committed_trace.clone();
        runtime_trace.0[2] =
            make_tile_trace_item_at(3, "main", 2, vec![2], "consumer".to_string(), 1, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_producer_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let VerificationResult::Fraud(evidence) = trace_verifier.verify(&runtime_trace) else {
            panic!("expected a divergence at the consumer step");
        };

        // The producer is the resolved source, and it is witnessed.
        // Keyed by (verifying step, source), so look the producer up under the
        // consumer that reads it rather than on its own.
        assert!(
            evidence
                .input_sources_witnesses
                .keys()
                .any(|(_, record)| *record == committed_trace.0[1]),
            "producer step should be witnessed as the consumer's input source",
        );
    }

    /// The same defect on the non-fraud path: `terminal_window` witnesses its
    /// window through the identical resolution.
    #[test]
    fn terminal_window_resolves_top_level_step_inputs() {
        let trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "producer".to_string(), 1, 10),
            make_tile_trace_item_at(3, "main", 2, vec![2], "consumer".to_string(), 1, 20),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::build(
            &trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_top_level_tail_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let evidence = trace_verifier
            .terminal_window(&trace)
            .expect("terminal window over a trace longer than the window");

        assert!(
            evidence
                .input_sources_witnesses
                .keys()
                .any(|(_, record)| *record == trace.0[1]),
            "producer step should be witnessed as the consumer's input source",
        );
    }

    /// A recur site whose own binding reads a prior sibling. The site's
    /// `sources` are deliberately **not** inline, because the point is that they
    /// are resolved once at the site and never per iteration.
    fn make_recur_site_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("producer", 1, 1));
        cfs.tiles.push(TileDef::iter("sweep", 1, 1));

        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "producer".to_string(),
            sources: vec![InputBinding::inline()],
        }));
        main.items
            .push(SequenceChildItem::RecurTile(RecurTileItem {
                id: "sweep".to_string(),
                sources: vec![InputBinding::prior_item_output(0)],
                chunk: Some(2),
                leaves_output_open: false,
                state_is_output: false,
            }));

        cfs.sequences.push(main);
        cfs
    }

    /// An iteration of a recur site binds no CFS inputs, so it contributes no
    /// source witness — the same classification the guest makes, in the same
    /// order (`checks::cfs::verify_step_record_inputs`: root coordinates first,
    /// then recur iterations, then the CFS item).
    ///
    /// The fixture is built so the skip is load-bearing rather than incidental.
    /// `try_get_item` folds `[1, i]` back to the site, so an iteration resolved
    /// as an ordinary step would be handed the *site's* `PriorItemOutput(0)`
    /// under a frame of `[1]` and an item coordinate of `i` — the iteration
    /// counter read as a sibling index. At `i = 0` that trips the
    /// same-or-future-index panic; at `i = 1` it silently resolves to whatever
    /// sits at `[1, 0]`, which is the previous *iteration*, not item 0.
    #[test]
    fn recur_iterations_contribute_no_input_source_witness() {
        let committed_trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "producer".to_string(), 1, 10),
            make_sequence_start_record(3, "main", vec![2], 1),
            make_tile_trace_item_at(4, "main", 1, vec![2, 1], "sweep".to_string(), 1, 20),
            make_tile_trace_item_at(5, "main", 2, vec![2, 2], "sweep".to_string(), 1, 30),
            make_sequence_end_record(6, "main", vec![]),
        ]);
        // Diverges at index 4, so the window is the two iteration steps.
        let mut runtime_trace = committed_trace.clone();
        runtime_trace.0[4] =
            make_tile_trace_item_at(5, "main", 2, vec![2, 2], "sweep".to_string(), 1, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_recur_site_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let VerificationResult::Fraud(evidence) = trace_verifier.verify(&runtime_trace) else {
            panic!("expected a divergence at the second iteration");
        };

        assert!(
            evidence.input_sources_witnesses.is_empty(),
            "recur iterations bind no CFS inputs, so the window carries no source witnesses: {:?}",
            evidence.input_sources_witnesses,
        );
    }

    /// `EntryArgument` at a top-level step resolves to `ProgramStart` — the same
    /// record that opens the root frame. Unreachable before the frame lookup was
    /// fixed: the scan panicked before this arm ran.
    #[test]
    fn entry_argument_at_top_level_resolves_to_program_start() {
        let committed_trace = Trace(vec![
            make_program_start_record(1, vec!["arg".to_string()]),
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(3, "main", 2, vec![2], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let mut runtime_trace = committed_trace.clone();
        runtime_trace.0[1] =
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_top_level_entry_argument_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let VerificationResult::Fraud(evidence) = trace_verifier.verify(&runtime_trace) else {
            panic!("expected a divergence at the consumer step");
        };

        assert!(
            evidence
                .input_sources_witnesses
                .keys()
                .any(|(_, record)| *record == committed_trace.0[0]),
            "ProgramStart should be witnessed as the entry-argument source",
        );
    }

    /// Tampering `exec_index` alone still *detects* as a divergence here, and
    /// should: the walker compares the replayed trace against the commitment,
    /// and a tampered record genuinely hashes to a different leaf — a different
    /// trace root, a different fingerprint entry. This side is a detector, not
    /// an authorizer, so it has no opinion on *why* the traces differ.
    ///
    /// What used to follow from that was a forged receipt. The window's earlier
    /// items are the honest ones and still match the commitment, so the margin
    /// is satisfied and only the last item "diverges" — `finalize`'s `Finished`
    /// condition — on a field the guest never read. The margin pins the
    /// window's *opening state*; it never pinned the *diverging item*.
    ///
    /// That is now closed on the guest side: `checks::cfs::verify_exec_index`
    /// fixes the field from the step's trace index, so a window built from this
    /// evidence is refused before `finalize` is reached. See
    /// `exec_index_tampering_is_refused` in the transition guest's tests.
    ///
    /// Kept host-side to pin the other half of that argument: the field really
    /// does reach the leaf, so it really does need verifying.
    #[test]
    fn exec_index_tampering_is_detected_but_no_longer_provable() {
        let honest = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "producer".to_string(), 1, 10),
            make_tile_trace_item_at(3, "main", 2, vec![2], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(4, "main", 3, vec![3], "tail".to_string(), 1, 30),
            make_sequence_end_record(5, "main", vec![]),
        ]);
        let cfs = make_producer_dependency_cfs();
        let trace_commitment = TraceCommitment::build(
            &honest,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );

        // Baseline: the honest trace agrees with its own commitment everywhere.
        let mut honest_verifier = TraceVerifier::new(
            trace_commitment.clone(),
            &precomputed::EMPTY_TRIE_NODES[0],
            &cfs,
        )
        .expect("valid commitment");
        assert!(matches!(
            honest_verifier.verify(&honest),
            VerificationResult::Ok
        ));

        // Bump `exec_index` on one step. Nothing else changes — same kind, same
        // coordinates, same commitments, so the same replay receipt would still
        // verify and every per-step check still passes.
        let mut forged = honest.clone();
        forged.0[3].exec_index += 1;
        assert_eq!(forged.0[3].kind, honest.0[3].kind);
        assert_eq!(forged.0[3].coordinates, honest.0[3].coordinates);
        assert_eq!(forged.0[3].sequence_id, honest.0[3].sequence_id);
        assert_ne!(
            forged.0[3].hash(),
            honest.0[3].hash(),
            "the tampered field must reach the trace leaf, or there is no attack"
        );

        let mut forged_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");
        let VerificationResult::Fraud(evidence) = forged_verifier.verify(&forged) else {
            panic!(
                "expected the tampered exec_index to manufacture a divergence; \
                 if this now returns Ok, exec_index no longer reaches the trace leaf"
            );
        };

        // The window is the `Finished` shape: earlier items are the honest ones
        // and match the commitment, the last is the tampered record.
        let window_items = &evidence.window.items;
        assert_eq!(*window_items.last().unwrap(), forged.0[3]);
        for (offset, item) in window_items.iter().rev().skip(1).enumerate() {
            let honest_index = 3 - 1 - offset;
            assert_eq!(
                *item, honest.0[honest_index],
                "every item before the divergence is the honest record"
            );
        }
    }

    /// The `SequenceScope` arm of `resolve_inputs_sources`, exercised for the
    /// first time.
    ///
    /// A scope value is not produced inside the frame — it arrives *with* it —
    /// so the only record holding it is the frame's opening `SequenceStart`.
    /// That is the record this must resolve to, and the one the guest's
    /// `verify_sequence_scope_parent` then binds the scope witness against.
    #[test]
    fn fraud_window_resolves_nested_sequence_scope_input() {
        let committed_trace = Trace(vec![
            make_program_start_record(1, vec!["arg".to_string()]),
            make_sequence_start_record(2, "inner", vec![1], 1),
            make_tile_trace_item_at(3, "inner", 1, vec![1, 1], "inner_tile".to_string(), 1, 10),
            make_sequence_end_record(4, "inner", vec![1]),
            make_tile_trace_item_at(5, "main", 2, vec![2], "tail".to_string(), 1, 20),
            make_sequence_end_record(6, "main", vec![]),
        ]);
        // Diverge at `inner_tile`, so the window holds the scope-binding step
        // and the `SequenceStart` that opened its frame.
        let mut runtime_trace = committed_trace.clone();
        runtime_trace.0[2] =
            make_tile_trace_item_at(3, "inner", 1, vec![1, 1], "inner_tile".to_string(), 1, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_sequence_input_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let VerificationResult::Fraud(evidence) = trace_verifier.verify(&runtime_trace) else {
            panic!("expected a divergence at the inner tile");
        };

        assert!(
            evidence
                .input_sources_witnesses
                .keys()
                .any(|(_, record)| *record == committed_trace.0[1]),
            "the frame-opening SequenceStart is the scope input's source, and must be witnessed",
        );
    }

    /// `main` has no caller, so a scope binding at the root frame is a broken
    /// assumption, not a missing record. It must say so.
    #[test]
    #[should_panic(expected = "has no caller to supply one")]
    fn sequence_scope_at_root_frame_is_refused() {
        let committed_trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(3, "main", 2, vec![2], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let mut runtime_trace = committed_trace.clone();
        runtime_trace.0[1] =
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 999);

        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_root_sequence_scope_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let _ = trace_verifier.verify(&runtime_trace);
    }

    #[test]
    #[should_panic(expected = "Failed to resolve source record")]
    fn test_verify_trace_returns_failure_for_unresolved_required_prior_item_output() {
        let runtime_trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 2, vec![2], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(3, "main", 3, vec![3], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let committed_trace = Trace(vec![
            make_program_start_record(1, Vec::new()),
            make_tile_trace_item_at(2, "main", 2, vec![2], "consumer".to_string(), 1, 999),
            make_tile_trace_item_at(3, "main", 3, vec![3], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::build(
            &committed_trace,
            &precomputed::EMPTY_TRIE_NODES[0],
            test_fraud_proof_config(),
        );
        let cfs = make_producer_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs)
                .expect("valid commitment");

        let _ = trace_verifier.verify(&runtime_trace);
    }
}

#[cfg(test)]
mod frontier_root_tests {
    use super::*;

    /// The memo must equal the fold it replaced at every level. It is built
    /// from that fold, so this guards a future edit that swaps in a table —
    /// which is exactly what `EMPTY_TRIE_NODES` looked like it could be, and
    /// is not.
    #[test]
    fn empty_root_memo_matches_fold() {
        for level in 0..=TRACE_TREE_DEPTH {
            let level = Level::from(level);
            assert_eq!(
                Bytes::empty_root(level).0,
                empty_root_fold(level).0,
                "memo diverges from the fold at level {}",
                u8::from(level)
            );
        }
    }

    /// The direct ommer fold must agree, byte for byte, with the
    /// clone-and-rebuild it replaced — at every length, not just one.
    ///
    /// `frontier_root` is on the hot path of every storage append and every
    /// trace step, so the rebuild was removed for cost; this pins that the
    /// removal changed no commitment.
    #[test]
    fn direct_fold_matches_bridge_tree_rebuild() {
        let leaf = |i: u64| Bytes(sha256_bytes(&i.to_le_bytes()));

        let mut frontier = TraceTreeFrontier::new(leaf(0));
        for i in 1..=257u64 {
            let rebuilt = TraceTree::from_frontier(1, frontier.clone())
                .root(0)
                .expect("rebuilt root should exist")
                .0;
            assert_eq!(
                frontier_root(&frontier),
                rebuilt,
                "root diverged at length {i}"
            );
            frontier.append(leaf(i));
        }
    }
}
