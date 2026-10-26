//! Fixtures for tests of the storage check and its consumers: stores built
//! from entries, their read and write witnesses, and a tile step claiming
//! roots. Shared by the storage and transition guests' tests; behind the
//! `test-fixtures` feature, so no guest binary carries them.

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::cfs::{CfsCoordinates, FIRST_COORDINATE};
use raster_core::coordinate_index::{
    coordinate_index_membership_proof, coordinate_index_non_membership_proof, coordinate_index_root,
};
use raster_core::merkle::to_node_hash;
use raster_core::recur_progress::RecurProgressStack;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord, StorageRoots};
use raster_core::transition::{
    SerializableFrontier, StorageEntry, StorageLogWitness, StorageReadWitness, StorageWriteWitness,
};

use crate::merkle_tree::{deserialize_frontier, frontier_root, Bytes, TraceBridgeTree, EMPTY_LEAF};
use crate::storage::storage_leaf_hash;

pub fn empty_storage_frontier() -> NonEmptyFrontier<Bytes> {
    deserialize_frontier(&SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    })
    .expect("empty storage frontier should deserialize")
}

pub fn build_storage_context(
    entries: &[StorageEntry],
) -> (
    NonEmptyFrontier<Bytes>,
    Vec<u8>,
    BTreeMap<CfsCoordinates, raster_core::transition::StorageIndexValue>,
    Vec<u8>,
) {
    let mut frontier = empty_storage_frontier();
    let mut index = BTreeMap::new();
    for entry in entries {
        frontier.append(Bytes(storage_leaf_hash(entry)));
        let log_position: u64 = frontier.position().into();
        index.insert(
            entry.coordinates.clone(),
            raster_core::transition::StorageIndexValue {
                log_position,
                object_commitment: entry.object_commitment.clone(),
            },
        );
    }
    let root = frontier_root(&frontier);
    let index_root = coordinate_index_root(&index);
    (frontier, root, index, index_root)
}

pub fn build_storage_log_witness_for_entries(
    entries: &[StorageEntry],
    log_position: u64,
) -> StorageLogWitness {
    let mut tree = TraceBridgeTree::new(1);
    tree.append(Bytes(EMPTY_LEAF.to_vec()));
    let mut marked_position = None;
    for (index, entry) in entries.iter().enumerate() {
        tree.append(Bytes(storage_leaf_hash(entry)));
        if u64::try_from(index).expect("index overflow") + 1 == log_position {
            marked_position = tree.mark();
        }
    }
    let marked_position = marked_position.expect("log position should exist in append tree");
    let auth_path = tree
        .witness(marked_position, 0)
        .expect("append-log witness should exist");
    StorageLogWitness {
        position: u64::from(marked_position),
        path_elems: auth_path.iter().map(|elem| to_node_hash(&elem.0)).collect(),
    }
}

pub fn build_read_witness(entries: &[StorageEntry], entry: &StorageEntry) -> StorageReadWitness {
    let (_frontier, _root, index, _index_root) = build_storage_context(entries);
    let index_witness = coordinate_index_membership_proof(&index, &entry.coordinates)
        .expect("membership proof should exist");
    let log_witness =
        build_storage_log_witness_for_entries(entries, index_witness.value.log_position);
    StorageReadWitness {
        entry: entry.clone(),
        log_witness,
        index_witness,
    }
}

pub fn build_write_witness(
    before_entries: &[StorageEntry],
    new_entry: &StorageEntry,
) -> StorageWriteWitness {
    let (_frontier, _root, before_index, _before_index_root) =
        build_storage_context(before_entries);
    StorageWriteWitness {
        entry: new_entry.clone(),
        index_non_membership_witness: coordinate_index_non_membership_proof(
            &before_index,
            &new_entry.coordinates,
        ),
    }
}

pub fn tile_step_with_store_roots(
    exec_index: u64,
    coordinates: CfsCoordinates,
    input_source_commitment: Vec<u8>,
    output_commitment: Vec<u8>,
    root_before: Vec<u8>,
    root_after: Vec<u8>,
    index_root_before: Vec<u8>,
    index_root_after: Vec<u8>,
) -> StepRecord {
    StepRecord {
        exec_index,
        sequence_id: "main".to_string(),
        coordinates,
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("tile".to_string()),
            intra_sequence_index: FIRST_COORDINATE,
            input_commitment: Vec::new(),
            input_source_commitment,
            output_commitment,
            storage: StorageRoots {
                root_before,
                root_after,
                index_root_before,
                index_root_after,
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    }
}
