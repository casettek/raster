//! Checks the storage transition of an execution step: read witnesses
//! (append-log path + coordinate-index membership + selection witnesses) and
//! the optional write witness (non-membership before, membership after),
//! against the recorded before/after roots.

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::{
    verify_coordinate_index_membership, verify_coordinate_index_non_membership,
};
use raster_core::input::verify_selection_witness;
use raster_core::trace::{FnInput, StepRecord, StorageRoots};
use raster_core::transition::{
    SerializableFrontier, StorageEntry, StorageLogWitness, StorageReadWitness, StorageWitness,
    StorageWriteWitness,
};

use crate::merkle_tree::{
    combine_merkle_level, frontier_root, serialize_frontier, sha256_bytes, Bytes,
};

pub fn storage_leaf_hash(entry: &StorageEntry) -> Vec<u8> {
    sha256_bytes(&entry.to_bytes())
}

fn append_log_root_from_witness(
    entry: &StorageEntry,
    witness: &StorageLogWitness,
) -> Vec<u8> {
    let mut current = storage_leaf_hash(entry);
    for (level, sibling) in witness.path_elems.iter().enumerate() {
        current = if ((witness.position >> level) & 1) == 0 {
            combine_merkle_level(level, &current, sibling)
        } else {
            combine_merkle_level(level, sibling, &current)
        };
    }
    current
}

/// Verify a trace-inclusion (append-log + coordinate-index membership) proof
/// that `coordinates` commits to `commitment` in the store rooted at
/// `current_log_root`/`current_index_root`. Shared by ordinary per-step
/// storage reads and by the fraud-proof genesis check that a
/// window's initial storage state really contains an authorized
/// entry-argument binding (see `checks::entrypoint`).
pub(crate) fn verify_storage_read_witness(
    read_witness: &StorageReadWitness,
    current_log_root: &[u8],
    current_index_root: &[u8],
    coordinates: &CfsCoordinates,
    commitment: &[u8],
) {
    assert_eq!(
        read_witness.entry.coordinates, *coordinates,
        "Storage read witness coordinates do not match requested coordinates",
    );
    assert_eq!(
        read_witness.entry.object_commitment, commitment,
        "Storage read witness commitment does not match requested commitment",
    );
    assert!(
        verify_coordinate_index_membership(current_index_root, &read_witness.index_witness),
        "Storage coordinate-index membership proof is invalid",
    );
    assert_eq!(
        read_witness.index_witness.coordinates, *coordinates,
        "Coordinate-index witness coordinates do not match storage input",
    );
    assert_eq!(
        read_witness.index_witness.value.object_commitment, commitment,
        "Coordinate-index witness commitment does not match storage input commitment",
    );
    assert_eq!(
        read_witness.index_witness.value.log_position, read_witness.log_witness.position,
        "Coordinate-index witness log position does not match append-log witness position",
    );
    assert_eq!(
        append_log_root_from_witness(&read_witness.entry, &read_witness.log_witness),
        current_log_root,
        "Append-log witness does not match current storage root",
    );
}

fn verify_storage_write_witness(
    write_witness: &StorageWriteWitness,
    current_index_root: &[u8],
    next_index_root: &[u8],
    expected_entry: &StorageEntry,
    expected_log_position: u64,
) {
    assert_eq!(
        write_witness.entry, *expected_entry,
        "Storage write witness entry does not match expected append entry",
    );
    assert_eq!(
        write_witness.index_non_membership_witness.coordinates, expected_entry.coordinates,
        "Coordinate-index non-membership proof coordinates do not match write entry",
    );
    assert!(
        verify_coordinate_index_non_membership(
            current_index_root,
            &write_witness.index_non_membership_witness,
        ),
        "Coordinate-index non-membership proof is invalid before write",
    );
    assert_eq!(
        write_witness.index_membership_witness.coordinates, expected_entry.coordinates,
        "Coordinate-index membership proof coordinates do not match write entry",
    );
    assert_eq!(
        write_witness
            .index_membership_witness
            .value
            .object_commitment,
        expected_entry.object_commitment,
        "Coordinate-index membership proof commitment does not match write entry",
    );
    assert_eq!(
        write_witness.index_membership_witness.value.log_position, expected_log_position,
        "Coordinate-index membership proof log position does not match append-log position",
    );
    assert_eq!(
        write_witness.index_non_membership_witness.siblings,
        write_witness.index_membership_witness.siblings,
        "Coordinate-index update proof siblings changed across insertion",
    );
    assert!(
        verify_coordinate_index_membership(
            next_index_root,
            &write_witness.index_membership_witness
        ),
        "Coordinate-index membership proof is invalid after write",
    );
}

pub fn verify_storage_transition(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    _output_witness_bytes: Option<&Vec<u8>>,
    storage_witness: Option<&StorageWitness>,
    current_frontier: &mut NonEmptyFrontier<Bytes>,
    current_index_root: &[u8],
) -> (SerializableFrontier, Vec<u8>, Vec<u8>) {
    if let Some(StorageRoots {
        root_before: storage_root_before,
        root_after: storage_root_after,
        index_root_before: storage_index_root_before,
        index_root_after: storage_index_root_after,
    }) = step_record.storage_roots()
    {
        let current_root = frontier_root(current_frontier);
        assert_eq!(
            storage_root_before, &current_root,
            "Execution-step storage root before does not match current storage root",
        );
        assert_eq!(
            storage_index_root_before, &current_index_root,
            "Execution-step storage index root before does not match current index root",
        );

        if let Some(input_source_witness) = input_source_witness {
            for (binding_name, storage_meta) in input_source_witness.storage() {
                let read_witness = storage_witness
                    .and_then(|witness| {
                        witness.reads.iter().find(|read| {
                            read.entry.coordinates == storage_meta.coordinates
                                && read.entry.object_commitment == storage_meta.commitment
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing storage read witness for coordinates {:?}",
                            storage_meta.coordinates
                        )
                    });
                verify_storage_read_witness(
                    read_witness,
                    &current_root,
                    current_index_root,
                    &storage_meta.coordinates,
                    &storage_meta.commitment,
                );
                assert_eq!(
                    storage_meta.commitment, storage_meta.selection.source_root_hash,
                    "Storage input '{}' commitment must match raster selection root",
                    binding_name,
                );
                if storage_meta.selection.selected_len > 0 {
                    let witness = storage_selection_witnesses
                        .get(binding_name.as_str())
                        .unwrap_or_else(|| {
                            panic!(
                                "Missing storage selection witness for binding '{}'",
                                binding_name
                            )
                        });
                    assert!(
                        verify_selection_witness(&storage_meta.selection, witness),
                        "Storage input '{}' selection witness is invalid",
                        binding_name,
                    );
                }
            }
        }

        let write_witness = storage_witness.and_then(|witness| witness.write.as_ref());

        match write_witness {
            Some(write_witness) => {
                let object_commitment = step_record
                    .output_commitment()
                    .expect("Execution step must expose output commitment")
                    .clone();

                let expected_entry = StorageEntry {
                    coordinates: step_record.coordinates().clone(),
                    object_commitment,
                };
                current_frontier.append(Bytes(storage_leaf_hash(&expected_entry)));
                let next_root = frontier_root(current_frontier);
                let next_position: u64 = current_frontier.position().into();
                verify_storage_write_witness(
                    write_witness,
                    current_index_root,
                    storage_index_root_after,
                    &expected_entry,
                    next_position,
                );
                assert_eq!(
                    storage_root_after, &next_root,
                    "Execution-step storage root after does not match appended storage root",
                );

                (
                    serialize_frontier(current_frontier),
                    next_root,
                    storage_index_root_after.clone(),
                )
            }
            None => {
                assert_eq!(
                    storage_root_before, storage_root_after,
                    "Execution-step without storage write must leave append-log root unchanged",
                );
                assert_eq!(
                    storage_index_root_before, storage_index_root_after,
                    "Execution-step without storage write must leave index root unchanged",
                );
                (
                    serialize_frontier(current_frontier),
                    current_root,
                    current_index_root.to_vec(),
                )
            }
        }
    } else {
        assert!(
            storage_witness.is_none(),
            "Only execution steps may carry storage witnesses",
        );
        (
            serialize_frontier(current_frontier),
            frontier_root(current_frontier),
            current_index_root.to_vec(),
        )
    }
}
