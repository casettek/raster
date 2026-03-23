//! Trace commitment utilities.
//!
//! This module provides types and functions for creating cryptographic
//! commitments to execution traces using incremental Merkle trees.

use bridgetree::{Hashable, Level, NonEmptyFrontier};
use incrementalmerkletree::{MerklePath, Position};
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource, SequenceChildItem,
};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;

use crate::error::{BitPackerError, Result};
use crate::precomputed::{EMPTY_TRIE_NODES, HASH_SIZE};

use raster_core::fingerprint::BitPacker;
use raster_core::trace::{StepRecord, Trace, TraceWindow};
use raster_core::transition::StepRecordWitness;

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
        let mut hasher = Sha256::new();
        hasher.update(&data);
        hasher.finalize().to_vec()
    }

    fn try_hash(&self) -> Result<Vec<u8>> {
        let data = postcard::to_allocvec(self)
            .map_err(|e| BitPackerError::SerializationError(e.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(&data);
        Ok(hasher.finalize().to_vec())
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

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);

        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);

        let mut hasher = Sha256::new();
        hasher.update(&data);

        Bytes(hasher.finalize().to_vec())
    }
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

pub const WINDOW_SIZE: u8 = 2;
pub const BITS_PER_ITEM: usize = 16;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraceCommitment {
    pub fingerprint: Fingerprint,
    pub revealed_items: Vec<StepRecord>,
}

impl TraceCommitment {
    pub fn from(trace: &Trace, seed: &[u8]) -> TraceCommitment {
        assert!(
            trace.len() > WINDOW_SIZE.into(),
            "Trace length can't be less than verification window"
        );

        let revealed_items = trace[..(WINDOW_SIZE as usize)].to_vec();

        let items_hashes: Vec<Vec<u8>> = trace.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(BITS_PER_ITEM);
        trace_tree.append(Bytes(seed.to_vec()));

        let mut fingerprint_acc = FingerprintAccumulator::new(BitPacker(BITS_PER_ITEM));

        for item_hash in &items_hashes {
            trace_tree.append(Bytes(item_hash.clone()));
            if let Some(root) = trace_tree.root(0) {
                fingerprint_acc.append(&root.0);
            }
        }

        let fingerprint = fingerprint_acc.into_fingerprint();

        TraceCommitment {
            fingerprint,
            revealed_items,
        }
    }

    /// Try to create a commitment from items, returning an error if the trace is empty.
    pub fn try_from(trace: &Trace, seed: &[u8]) -> Result<TraceCommitment> {
        if trace.is_empty() {
            return Err(BitPackerError::EmptyTrace);
        }
        Ok(Self::from(trace, seed))
    }

    /// Get the frontier (partial Merkle path) at position n.
    ///
    /// This can be used to continue building the tree from position n.
    pub fn frontier(trace: &Trace, n: usize, seed: &[u8]) -> Option<TraceTreeFrontier> {
        let items_hashes: Vec<Vec<u8>> = trace.iter().map(|item| item.hash()).collect();

        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        for item_hash in items_hashes.iter().take(n) {
            trace_tree.append(Bytes(item_hash.clone()));
        }

        trace_tree.frontier().cloned()
    }

    pub fn witness(trace: &Trace, n: usize, seed: &[u8]) -> Option<MerklePath<Bytes, 32>> {
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
    pub fn try_frontier(trace: &Trace, n: usize, seed: &[u8]) -> Result<TraceTreeFrontier> {
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

    /// Get the number of commitments.
    pub fn len(&self) -> usize {
        self.fingerprint.len()
    }

    /// Check if the commitment is empty.
    pub fn is_empty(&self) -> bool {
        self.fingerprint.is_empty()
    }

    pub fn diff(&self, other: &TraceCommitment) -> Option<usize> {
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
}

fn parent_coordinates(step_record: &StepRecord) -> Option<(CfsCoordinates, u32)> {
    let coordinates = step_record.coordinates();
    let (&current_child_index, parent_coords) = coordinates.split_last()?;

    Some((CfsCoordinates(parent_coords.to_vec()), current_child_index))
}

struct IndexedStepRecord {
    index: usize,
    record: StepRecord,
}

fn resolve_record_inputs(
    step_record: &StepRecord,
    trace: &[StepRecord],
    cfs_cursor: &CfsCursor,
    step_inputs: &[InputBinding],
) -> Vec<IndexedStepRecord> {
    if step_inputs 
        .iter()
        .all(|input| matches!(input.source, InputSource::External))
    {
        return Vec::new();
    }

    let Some((parent_sequence_coordinates, item_coordinate)) =
        parent_coordinates(&step_record)
    else {
        // Entrypoint SequenceStart/SequenceEnd
        return Vec::new();
    };

    // Find the parent sequence record start
    let parent_sequence_index = trace
        .iter()
        .rposition(|record| {
            matches!(
                record,
                StepRecord::SequenceStart(sequence_start_record)
                    if sequence_start_record.coordinates == parent_sequence_coordinates
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "Failed to resolve active sequence invocation for step {:?} in frame {:?}",
                step_record, parent_sequence_coordinates
            )
        });

    let frame_trace = &trace[parent_sequence_index..];

    let mut producer_records = Vec::new();

    for step_input in step_inputs {
        match &step_input.source {
            InputSource::External => {}
            InputSource::SeqInput { input_index } => {
                let producer_record = frame_trace
                    .first()
                    .cloned()
                    .and_then(|record| match record {
                        StepRecord::SequenceStart(sequence_start_record)
                            if sequence_start_record.coordinates == parent_sequence_coordinates =>
                        {
                            Some(IndexedStepRecord {
                                index: parent_sequence_index,
                                record: StepRecord::SequenceStart(sequence_start_record),
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve sequence input {input_index} for step {:?} in frame {:?}",
                            step_record, parent_sequence_coordinates 
                        )
                    });

                producer_records.push(producer_record);
            }
            InputSource::ItemOutput {
                item_index,
                output_index,
            } => {
                if *item_index >= item_coordinate as usize {
                    panic!(
                        "Step {:?} cannot depend on sibling item {} output {} from the same or a future index {}",
                        step_record, item_index, output_index, item_coordinate
                    );
                }

                let mut producer_coordinates = parent_sequence_coordinates.clone();
                producer_coordinates.push(
                    (*item_index)
                        .try_into()
                        .expect("Producer item index exceeds CFS coordinate bounds"),
                );

                let producer_item = cfs_cursor
                    .try_get_child_item(&producer_coordinates)
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve producer item {} for step {:?} in frame {:?}",
                            item_index, step_record, parent_sequence_coordinates 
                        )
                    });

                let producer_record = frame_trace
                    .iter()
                    .enumerate()
                    .find_map(|(frame_offset, record)| match (record, producer_item) {
                        (StepRecord::TileExec(tile_exec_record), SequenceChildItem::Tile(_))
                            if tile_exec_record.coordinates == producer_coordinates =>
                        {
                            Some(IndexedStepRecord {
                                index: parent_sequence_index + frame_offset,
                                record: record.clone(),
                            })
                        }
                        (StepRecord::SequenceEnd(sequence_end_record), SequenceChildItem::Sequence(_))
                            if sequence_end_record.coordinates == producer_coordinates =>
                        {
                            Some(IndexedStepRecord {
                                index: parent_sequence_index + frame_offset,
                                record: record.clone(),
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve producer record for step {:?} from source item {} output {} at {:?}",
                            step_record, item_index, output_index, producer_coordinates
                        )
                    });

                producer_records.push(producer_record);
            }
        }
    }

    producer_records
}

fn resolve_fraud_window_sources(
    trace: &Trace,
    window_end_index: usize,
    fraud_window: &TraceWindow,
    cfs_cursor: &CfsCursor,
    seed: &[u8],
) -> HashMap<StepRecord, Vec<u8>> {
    let window_start_index = window_end_index + 1 - fraud_window.items.len();
    let mut witness: HashMap<StepRecord, Vec<u8>> = HashMap::new();

    for (offset, step_record) in fraud_window.items.iter().enumerate() {
        if step_record.coordinates().is_empty() {
            // Empty coordinates mean SequenceStart/SequenceEnd of main function
            continue;
        }

        let cfs_item = cfs_cursor
            .try_get_child_item(step_record.coordinates())
            .unwrap_or_else(|| {
                panic!(
                    "Failed to resolve cfs item for fraud window coordinates: {:?}",
                    step_record.coordinates()
                )
            });
        let step_index = window_start_index + offset;

        for producer_record in resolve_record_inputs(
            step_record,
            &trace[..step_index],
            cfs_cursor,
            cfs_item.inputs(),
        ) {
            let trace_prefix = Trace(trace[..step_index].to_vec());
            let proof = TraceCommitment::witness(&trace_prefix, producer_record.index, seed)
                .expect("Failed to derive witness proof for source record");
            let witness_bytes = postcard::to_allocvec(&StepRecordWitness {
                position: u64::from(proof.position()),
                path_elems: proof.path_elems().iter().map(|elem| elem.0.clone()).collect(),
            })
            .expect("Failed to serialize source record witness");

            witness.insert(producer_record.record, witness_bytes);
        }
    }

    witness
}

pub struct TraceVerifier<'a> {
    pub trace_commitment: TraceCommitment,
    pub cfs: &'a ControlFlowSchema,
    pub seed: Vec<u8>,

    pub fingerprint_acc: FingerprintAccumulator,
    pub latest_frontier: TraceTreeFrontier,

    pub window_frontiers: Window<TraceTreeFrontier>,
    pub window_items: Window<StepRecord>,
}

#[derive(Debug, Clone)]
pub struct FraudEvidence {
    pub window: TraceWindow,
    pub witness: HashMap<StepRecord, Vec<u8>>,
}

pub enum VerificationResult {
    Ok,
    Fraud(FraudEvidence),
}

impl<'a> TraceVerifier<'a> {
    pub fn new(trace_commitment: TraceCommitment, seed: &[u8], cfs: &'a ControlFlowSchema) -> Self {
        let mut trace_tree = TraceTree::new(1);
        trace_tree.append(Bytes(seed.to_vec()));

        let init_frontier = trace_tree.frontier().cloned().unwrap();

        let bit_packer = trace_commitment.fingerprint.bits_packer.clone();
        let fingerprint_acc = FingerprintAccumulator::new(bit_packer);

        let mut window_frontiers: Window<TraceTreeFrontier> = Window::new(WINDOW_SIZE.into());
        let window_items: Window<StepRecord> = Window::new(WINDOW_SIZE.into());

        window_frontiers.push(init_frontier.clone());

        Self {
            trace_commitment,
            cfs,
            seed: seed.to_vec(),

            fingerprint_acc,
            latest_frontier: init_frontier,

            window_frontiers,
            window_items,
        }
    }

    pub fn verify(&mut self, trace: &Trace) -> VerificationResult {
        let cfs_cursor = CfsCursor::new(self.cfs.clone());

        for (step_index, step_record) in trace.iter().enumerate() {
            let item_frontier = self.latest_frontier.clone();

            self.window_frontiers.push(item_frontier);
            self.window_items.push(step_record.clone());

            let step_record_hash = step_record.hash();
            self.latest_frontier.append(Bytes(step_record_hash));

            let root = self
                .latest_frontier
                .root(Some(0.into()));

            self.fingerprint_acc.append(&root.0);

            let latest_fingerprint = self.fingerprint_acc.clone().into_fingerprint();

            let index = latest_fingerprint.len() - 1;

            if latest_fingerprint.bits_packer.diff_at_index(
                index,
                &latest_fingerprint.bits,
                &self.trace_commitment.fingerprint.bits,
            ) {
                let diff_bits = self
                    .trace_commitment
                    .fingerprint
                    .bits_packer
                    .get_range(
                        index.saturating_sub(WINDOW_SIZE.into()) + 1,
                        index + 1,
                        &self.trace_commitment.fingerprint.bits,
                    )
                    .unwrap();

                let window_fingerprint = Fingerprint::from(
                    diff_bits,
                    self.trace_commitment.fingerprint.bits_packer,
                    WINDOW_SIZE.into(),
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

                let witness = resolve_fraud_window_sources(
                    trace,
                    step_index,
                    &fraud_window,
                    &cfs_cursor,
                    &self.seed,
                );

                return VerificationResult::Fraud(FraudEvidence {
                    window: fraud_window,
                    witness,
                });
            }
        }

        VerificationResult::Ok
    }
}

#[cfg(test)]
mod tests {
    use raster_core::cfs::{
        CfsCoordinates, InputBinding, SequenceChildItem, SequenceDef, SequenceItem, TileDef,
        TileItem,
    };
    use raster_core::trace::{
        FnCallRecord, FnInputParam, SequenceEndRecord, SequenceStartRecord, TileExecRecord,
    };

    use super::*;
    use crate::precomputed;

    /// Helper function to create a step record for testing.
    fn make_tile_trace_item(input: u64, output: u64) -> StepRecord {
        make_tile_trace_item_at(
            input,
            "test_sequence",
            input as u32,
            vec![0],
            format!("test_tile_{input}"),
            1,
            output,
        )
    }

    fn make_tile_trace_item_at(
        exec_index: u64,
        sequence_id: &str,
        intra_sequence_index: u32,
        coordinates: Vec<u32>,
        fn_name: String,
        input_count: usize,
        output: u64,
    ) -> StepRecord {
        StepRecord::TileExec(TileExecRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            intra_sequence_index,
            coordinates: CfsCoordinates(coordinates),
            fn_call_record: FnCallRecord {
                fn_name,
                desc: None,
                inputs: (0..input_count)
                    .map(|index| FnInputParam {
                        name: format!("input_{index}"),
                        ty: "u64".to_string(),
                    })
                    .collect(),
                input_data: Vec::new(),
                output_type: Some("u64".to_string()),
                output_data: output.to_le_bytes().to_vec(),
            },
        })
    }

    fn make_sequence_start_record(
        exec_index: u64,
        sequence_id: &str,
        coordinates: Vec<u32>,
        input_count: usize,
    ) -> StepRecord {
        StepRecord::SequenceStart(SequenceStartRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            coordinates: CfsCoordinates(coordinates),
            inputs: (0..input_count)
                .map(|index| FnInputParam {
                    name: format!("input_{index}"),
                    ty: "u64".to_string(),
                })
                .collect(),
            input_data: Vec::new(),
        })
    }

    fn make_sequence_end_record(
        exec_index: u64,
        sequence_id: &str,
        coordinates: Vec<u32>,
    ) -> StepRecord {
        StepRecord::SequenceEnd(SequenceEndRecord {
            exec_index,
            sequence_id: sequence_id.to_string(),
            coordinates: CfsCoordinates(coordinates),
            output_type: Some("u64".to_string()),
            output_data: exec_index.to_le_bytes().to_vec(),
        })
    }

    fn make_test_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("test_tile", 1, 1));
        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "test_tile".to_string(),
            sources: vec![InputBinding::external()],
        }));
        cfs.sequences.push(main);
        cfs
    }

    fn make_item_output_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("producer", 1, 1));
        cfs.tiles.push(TileDef::iter("consumer", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "producer".to_string(),
            sources: vec![InputBinding::external()],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "consumer".to_string(),
            sources: vec![InputBinding::item_output(0, 0)],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::external()],
        }));

        cfs.sequences.push(main);
        cfs
    }

    fn make_sequence_input_dependency_cfs() -> ControlFlowSchema {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.tiles.push(TileDef::iter("inner_tile", 1, 1));
        cfs.tiles.push(TileDef::iter("tail", 1, 1));

        let mut main = SequenceDef::new("main");
        main.input_sources = vec![InputBinding::external()];
        main.items.push(SequenceChildItem::Sequence(SequenceItem {
            id: "inner".to_string(),
            sources: vec![InputBinding::seq_input(0)],
        }));
        main.items.push(SequenceChildItem::Tile(TileItem {
            id: "tail".to_string(),
            sources: vec![InputBinding::external()],
        }));

        let mut inner = SequenceDef::new("inner");
        inner.input_sources = vec![InputBinding::external()];
        inner.items.push(SequenceChildItem::Tile(TileItem {
            id: "inner_tile".to_string(),
            sources: vec![InputBinding::external()],
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

        let binded_trace = TraceCommitment::from(&items, &precomputed::EMPTY_TRIE_NODES[0]);
        let ref_binded_trace = TraceCommitment::from(&ref_items, &precomputed::EMPTY_TRIE_NODES[0]);

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
        let result = TraceCommitment::try_from(&items, &precomputed::EMPTY_TRIE_NODES[0]);
        assert!(matches!(result, Err(BitPackerError::EmptyTrace)));
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
            let mut hasher = sha2::Sha256::new();
            hasher.update(&data);
            hasher.finalize().to_vec()
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
        let trace_commitment = TraceCommitment::from(&trace, &precomputed::EMPTY_TRIE_NODES[0]);
        let cfs = make_test_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs);

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    #[test]
    fn test_verify_trace_returns_fraud_for_mismatched_trace() {
        let committed_trace = Trace((0..5).map(|i| make_tile_trace_item(i, i)).collect());
        let mut runtime_trace = committed_trace.clone();
        runtime_trace[2] = make_tile_trace_item(2, 999);

        let trace_commitment =
            TraceCommitment::from(&committed_trace, &precomputed::EMPTY_TRIE_NODES[0]);
        let cfs = make_test_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs);

        let verification_result = trace_verifier.verify(&runtime_trace);
        assert!(matches!(verification_result, VerificationResult::Fraud(_)));
    }

    #[test]
    fn test_verify_trace_returns_ok_for_item_output_dependency() {
        let trace = Trace(vec![
            make_sequence_start_record(1, "main", vec![], 0),
            make_tile_trace_item_at(2, "main", 0, vec![0], "producer".to_string(), 1, 10),
            make_tile_trace_item_at(3, "main", 1, vec![1], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(4, "main", 2, vec![2], "tail".to_string(), 1, 30),
            make_sequence_end_record(5, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::from(&trace, &precomputed::EMPTY_TRIE_NODES[0]);
        let cfs = make_item_output_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs);

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    #[test]
    fn test_verify_trace_returns_ok_for_sequence_step_seq_input_dependency() {
        let trace = Trace(vec![
            make_sequence_start_record(1, "main", vec![], 1),
            make_sequence_start_record(2, "inner", vec![0], 1),
            make_tile_trace_item_at(3, "inner", 0, vec![0, 0], "inner_tile".to_string(), 1, 10),
            make_sequence_end_record(4, "inner", vec![0]),
            make_tile_trace_item_at(5, "main", 1, vec![1], "tail".to_string(), 1, 20),
            make_sequence_end_record(6, "main", vec![]),
        ]);
        let trace_commitment = TraceCommitment::from(&trace, &precomputed::EMPTY_TRIE_NODES[0]);
        let cfs = make_sequence_input_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs);

        let verification_result = trace_verifier.verify(&trace);
        assert!(matches!(verification_result, VerificationResult::Ok));
    }

    #[test]
    #[should_panic(expected = "Failed to resolve producer record")]
    fn test_verify_trace_returns_failure_for_unresolved_required_producer() {
        let runtime_trace = Trace(vec![
            make_sequence_start_record(1, "main", vec![], 0),
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 20),
            make_tile_trace_item_at(3, "main", 2, vec![2], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let committed_trace = Trace(vec![
            make_sequence_start_record(1, "main", vec![], 0),
            make_tile_trace_item_at(2, "main", 1, vec![1], "consumer".to_string(), 1, 999),
            make_tile_trace_item_at(3, "main", 2, vec![2], "tail".to_string(), 1, 30),
            make_sequence_end_record(4, "main", vec![]),
        ]);
        let trace_commitment =
            TraceCommitment::from(&committed_trace, &precomputed::EMPTY_TRIE_NODES[0]);
        let cfs = make_item_output_dependency_cfs();
        let mut trace_verifier =
            TraceVerifier::new(trace_commitment, &precomputed::EMPTY_TRIE_NODES[0], &cfs);

        let _ = trace_verifier.verify(&runtime_trace);
    }
}
