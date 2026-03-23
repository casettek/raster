//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

use std::cmp::Ordering;
use std::collections::HashMap;

use bridgetree::{Hashable, Level, NonEmptyFrontier, Position};
use risc0_zkvm::guest::env;

use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema, InputSource, SequenceChildItem};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use raster_core::trace::StepRecord;
use raster_core::transition::{
    SerializableFrontier, StepRecordWitness, Transition, TransitionInput, TransitionJournal,
    TransitionState,
};

use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

pub type TraceBridgeTree = bridgetree::BridgeTree<Bytes, u64, 32>;

// ============================================================================
// Bytes + Hashable for bridgetree (matches prover's empty leaf and combine)
// ============================================================================

const HASH_SIZE: usize = 32;

/// Empty leaf hash (precomputed SHA256 of "empty"); matches prover EMPTY_TRIE_NODES[0].
const EMPTY_LEAF: [u8; 32] =
    hex_literal::hex!("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a");

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
        Bytes(EMPTY_LEAF.to_vec())
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

// ============================================================================
// SerializableFrontier <-> NonEmptyFrontier<Bytes> (guest-local conversion)
// ============================================================================

fn ser_frontier_to_frontier(ser: &SerializableFrontier) -> Option<NonEmptyFrontier<Bytes>> {
    NonEmptyFrontier::from_parts(
        Position::from(ser.position),
        Bytes(ser.leaf.clone()),
        ser.ommers.iter().map(|o| Bytes(o.clone())).collect(),
    )
    .ok()
}

fn frontier_to_ser_frontier(frontier: &NonEmptyFrontier<Bytes>) -> SerializableFrontier {
    SerializableFrontier {
        position: frontier.position().into(),
        leaf: frontier.leaf().0.clone(),
        ommers: frontier.ommers().iter().map(|o| o.0.clone()).collect(),
    }
}

// ============================================================================
// Hashing
// ============================================================================

/// Hash a TileExecRecord using SHA256 of its postcard-serialized form.
fn hash_trace_item(item: &StepRecord) -> Vec<u8> {
    let data = postcard::to_allocvec(item).expect("Failed to serialize TileExecRecord");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    hasher.finalize().to_vec()
}

fn frontier_root(frontier: &NonEmptyFrontier<Bytes>) -> Bytes {
    let tree = TraceBridgeTree::from_frontier(1, frontier.clone());
    tree.root(0).expect("Can't get current frontier root")
}

fn parent_coordinates(step_record: &StepRecord) -> Option<(CfsCoordinates, u32)> {
    let coordinates = step_record.coordinates();
    let (&current_child_index, parent_coords) = coordinates.split_last()?;

    Some((CfsCoordinates(parent_coords.to_vec()), current_child_index))
}

fn decode_step_record_witness(bytes: &[u8]) -> StepRecordWitness {
    let witness: StepRecordWitness =
        postcard::from_bytes(bytes).expect("Failed to deserialize step record witness");

    witness
}

fn verify_source_record_witness(
    source_record: &StepRecord,
    witness_bytes: &[u8],
    current_frontier_root: &Bytes,
) {
    let witness = decode_step_record_witness(witness_bytes);
    let witnessed_root = witness
        .path_elems
        .into_iter()
        .zip(0u8..)
        .fold(Bytes(hash_trace_item(source_record)), |root, (path_elem, level)| {
            let sibling = Bytes(path_elem);
            if (witness.position >> level) & 0x1 == 0 {
                Bytes::combine(level.into(), &root, &sibling)
            } else {
                Bytes::combine(level.into(), &sibling, &root)
            }
        });

    assert!(
        &witnessed_root == current_frontier_root,
        "Step record witness does not match current frontier root"
    );
}

fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    frontier: &NonEmptyFrontier<Bytes>,
    step_record: &StepRecord,
    witness: &HashMap<StepRecord, Vec<u8>>,
) {
    if step_record.coordinates().is_empty() {
        return;
    }

    let cfs_item = cfs_cursor
        .try_get_child_item(step_record.coordinates())
        .unwrap_or_else(|| panic!("Failed to resolve cfs item for step record {:?}", step_record));
    let sources = cfs_item.sources();

    if sources
        .iter()
        .all(|binding| matches!(binding.source, InputSource::External))
    {
        return;
    }

    let current_frontier_root = frontier_root(frontier);
    let Some((parent_sequence_coordinates, item_coordinate)) = parent_coordinates(step_record) else {
        return;
    };

    for source in sources {
        match source.source {
            InputSource::External => {}
            InputSource::SeqInput { .. } => {
                let (source_record, witness_bytes) = witness
                    .iter()
                    .find(|(record, _)| {
                        matches!(
                            record,
                            StepRecord::SequenceStart(sequence_start_record)
                                if sequence_start_record.coordinates == parent_sequence_coordinates
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing sequence input witness for step record {:?}",
                            step_record
                        )
                    });

                verify_source_record_witness(source_record, witness_bytes, &current_frontier_root);
            }
            InputSource::ItemOutput {
                item_index,
                output_index: _,
            } => {
                assert!(
                    item_index < item_coordinate as usize,
                    "Step {:?} cannot depend on source item {} from the same or a future position {}",
                    step_record,
                    item_index,
                    item_coordinate
                );

                let mut producer_coordinates = parent_sequence_coordinates.clone();
                producer_coordinates.push(
                    item_index
                        .try_into()
                        .expect("Producer item index exceeds CFS coordinate bounds"),
                );

                let producer_item = cfs_cursor
                    .try_get_child_item(&producer_coordinates)
                    .unwrap_or_else(|| {
                        panic!(
                            "Failed to resolve producer item {} for step {:?}",
                            item_index, step_record
                        )
                    });

                let (source_record, witness_bytes) = witness
                    .iter()
                    .find(|(record, _)| match (record, producer_item) {
                        (StepRecord::TileExec(tile_exec_record), SequenceChildItem::Tile(_)) => {
                            tile_exec_record.coordinates == producer_coordinates
                        }
                        (
                            StepRecord::SequenceEnd(sequence_end_record),
                            SequenceChildItem::Sequence(_),
                        ) => sequence_end_record.coordinates == producer_coordinates,
                        _ => false,
                    })
                    .unwrap_or_else(|| {
                        panic!("Missing item-output witness for step record {:?}", step_record)
                    });

                verify_source_record_witness(source_record, witness_bytes, &current_frontier_root);
            }
        }
    }
}

fn verify_step_record(step: &StepRecord, replay_image_id: Option<&Vec<u8>>) {
    match step {
        StepRecord::TileExec(tile_exec_record) => {
            let replay_image_id = replay_image_id
                .expect("replay image id should be provided for tile execution should ");
            let replay_image_id_digest = risc0_zkvm::sha::Digest::try_from(replay_image_id.as_slice())
                .expect("image_id must be 32 bytes");

            env::verify(
                replay_image_id_digest,
                &tile_exec_record.fn_call_record.output_data,
            )
            .expect("Failed to verify trace replay image id");
        }
        StepRecord::SequenceStart(_) => {}
        StepRecord::SequenceEnd(_) => {}
    }
}

fn advance_expected_next_coordinates(
    cfs_cursor: &CfsCursor,
    step: &StepRecord,
    expected: Option<&Vec<CfsCoordinates>>,
) -> Vec<CfsCoordinates> {
    let coordinates = step.coordinates();
    if let Some(expected_next_coordinates) = expected {
        assert!(
            expected_next_coordinates.contains(coordinates),
            "Step coordinates are not in expected next coordinates"
        );
    }

    cfs_cursor
        .try_get_next_coordinates(coordinates)
        .expect("Wrong tile coordinates")
}

fn advance_frontier_root_fingerprint(
    frontier: &mut NonEmptyFrontier<Bytes>,
    step_record: &StepRecord,
    fingerprint_acc: &mut FingerprintAccumulator,
) -> SerializableFrontier {
    let item_hash = hash_trace_item(step_record);
    frontier.append(Bytes(item_hash));

    let tree = TraceBridgeTree::from_frontier(1, frontier.clone());
    let Some(tree_root) = tree.root(0) else {
        panic!("Can't get tree root");
    };
    fingerprint_acc.append(&tree_root.0);

    frontier_to_ser_frontier(frontier)
}

// ============================================================================
// Main
// ============================================================================
fn main() {
    let cfs: ControlFlowSchema = env::read();
    let self_image_id: Vec<u8> = env::read();

    let input: TransitionInput = env::read();
    let state: TransitionState = env::read();

    let cfs_cursor = CfsCursor::new(cfs);

    match state {
        TransitionState::Init(init_transition) => {
            let mut init_frontier = ser_frontier_to_frontier(&init_transition.init_frontier)
                .expect("Invalid frontier in input");

            verify_step_record_inputs(
                &cfs_cursor,
                &init_frontier,
                &input.step_record,
                &input.witness,
            );
            verify_step_record(&input.step_record, input.replay_image_id.as_ref());
            let expected_next_coordinates =
                advance_expected_next_coordinates(&cfs_cursor, &input.step_record, None);

            let mut actual_fingerprint_acc =
                FingerprintAccumulator::new(init_transition.fingerprint.bits_packer);
            let new_frontier = advance_frontier_root_fingerprint(
                &mut init_frontier,
                &input.step_record,
                &mut actual_fingerprint_acc,
            );

            let current_state = TransitionState::Next(Transition {
                frontier: new_frontier,
                actual_fingerprint_acc,
                expected_next_coordinates,
            });

            let journal = TransitionJournal {
                init_state: init_transition,
                current_state,
                self_image_id,
            };

            env::commit(&journal);
        }
        TransitionState::Next(transition) => {
            let prev_journal: TransitionJournal = env::read();

            let self_image_id_digest = risc0_zkvm::sha::Digest::try_from(self_image_id.as_slice())
                .expect("image_id must be 32 bytes");
            // TODO: Did as_bytes actually available?
            env::verify(
                self_image_id_digest,
                &risc0_zkvm::serde::to_vec(&prev_journal).unwrap(),
            )
            .expect("Failed to verify trace replay image id");

            assert!(
                self_image_id == prev_journal.self_image_id,
                "Self image id does not match"
            );

            let TransitionState::Next(prev_transition) = prev_journal.current_state else {
                panic!("Provided Transition in Next State does not match the expected Transition State in Journal");
            };
            assert!(
                prev_transition == transition.clone(),
                "Transition Expected to be the same as the previous journal Transition"
            );

            let mut current_frontier =
                ser_frontier_to_frontier(&transition.frontier).expect("Invalid frontier in input");

            verify_step_record_inputs(
                &cfs_cursor,
                &current_frontier,
                &input.step_record,
                &input.witness,
            );
            verify_step_record(&input.step_record, input.replay_image_id.as_ref());
            let expected_next_coordinates = advance_expected_next_coordinates(
                &cfs_cursor,
                &input.step_record,
                Some(&transition.expected_next_coordinates),
            );

            let mut actual_fingerprint_acc = transition.actual_fingerprint_acc.clone();
            let new_frontier = advance_frontier_root_fingerprint(
                &mut current_frontier,
                &input.step_record,
                &mut actual_fingerprint_acc,
            );

            let actual_fingerprint: Fingerprint = actual_fingerprint_acc.into_fingerprint();

            let current_state =
                if actual_fingerprint.len() == prev_journal.init_state.fingerprint.len() {
                    assert!(actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Finished
                } else {
                    assert!(!actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Next(Transition {
                        frontier: new_frontier,
                        actual_fingerprint_acc: FingerprintAccumulator::from(actual_fingerprint),
                        expected_next_coordinates,
                    })
                };
            let journal = TransitionJournal {
                init_state: prev_journal.init_state,
                current_state,
                self_image_id,
            };

            env::commit(&journal);
        }

        TransitionState::Finished => {
            panic!("Finished Transition");
        }
    }
}
