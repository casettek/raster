//! Checks the internal store transition of an execution step: read witnesses
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
use raster_core::trace::{FnInput, StepRecord};
use raster_core::transition::{
    InternalStoreEntry, InternalStoreLogWitness, InternalStoreReadWitness, InternalStoreWitness,
    InternalStoreWriteWitness, SerializableFrontier,
};

use crate::merkle_tree::{
    combine_merkle_level, frontier_root, serialize_frontier, sha256_bytes, Bytes,
};

pub fn internal_store_leaf_hash(entry: &InternalStoreEntry) -> Vec<u8> {
    sha256_bytes(&entry.to_bytes())
}

fn append_log_root_from_witness(
    entry: &InternalStoreEntry,
    witness: &InternalStoreLogWitness,
) -> Vec<u8> {
    let mut current = internal_store_leaf_hash(entry);
    for (level, sibling) in witness.path_elems.iter().enumerate() {
        current = if ((witness.position >> level) & 1) == 0 {
            combine_merkle_level(level, &current, sibling)
        } else {
            combine_merkle_level(level, sibling, &current)
        };
    }
    current
}

fn verify_internal_store_read_witness(
    read_witness: &InternalStoreReadWitness,
    current_log_root: &[u8],
    current_index_root: &[u8],
    coordinates: &CfsCoordinates,
    commitment: &[u8],
) {
    assert_eq!(
        read_witness.entry.coordinates, *coordinates,
        "Internal store read witness coordinates do not match requested coordinates",
    );
    assert_eq!(
        read_witness.entry.object_commitment, commitment,
        "Internal store read witness commitment does not match requested commitment",
    );
    assert!(
        verify_coordinate_index_membership(current_index_root, &read_witness.index_witness),
        "Internal store coordinate-index membership proof is invalid",
    );
    assert_eq!(
        read_witness.index_witness.coordinates, *coordinates,
        "Coordinate-index witness coordinates do not match internal input",
    );
    assert_eq!(
        read_witness.index_witness.value.object_commitment, commitment,
        "Coordinate-index witness commitment does not match internal input commitment",
    );
    assert_eq!(
        read_witness.index_witness.value.log_position, read_witness.log_witness.position,
        "Coordinate-index witness log position does not match append-log witness position",
    );
    assert_eq!(
        append_log_root_from_witness(&read_witness.entry, &read_witness.log_witness),
        current_log_root,
        "Append-log witness does not match current internal store root",
    );
}

fn verify_internal_store_write_witness(
    write_witness: &InternalStoreWriteWitness,
    current_index_root: &[u8],
    next_index_root: &[u8],
    expected_entry: &InternalStoreEntry,
    expected_log_position: u64,
) {
    assert_eq!(
        write_witness.entry, *expected_entry,
        "Internal store write witness entry does not match expected append entry",
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

pub fn verify_internal_store_transition(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    internal_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    _output_witness_bytes: Option<&Vec<u8>>,
    internal_store_witness: Option<&InternalStoreWitness>,
    current_frontier: &mut NonEmptyFrontier<Bytes>,
    current_index_root: &[u8],
) -> (SerializableFrontier, Vec<u8>, Vec<u8>) {
    if let Some((
        internal_store_root_before,
        internal_store_root_after,
        internal_store_index_root_before,
        internal_store_index_root_after,
    )) = step_record.internal_store_roots()
    {
        let current_root = frontier_root(current_frontier);
        assert_eq!(
            internal_store_root_before, &current_root,
            "Execution-step internal store root before does not match current internal store root",
        );
        assert_eq!(
            internal_store_index_root_before, &current_index_root,
            "Execution-step internal store index root before does not match current index root",
        );

        if let Some(input_source_witness) = input_source_witness {
            for (binding_name, internal_meta) in input_source_witness.internal() {
                let read_witness = internal_store_witness
                    .and_then(|witness| {
                        witness.reads.iter().find(|read| {
                            read.entry.coordinates == internal_meta.coordinates
                                && read.entry.object_commitment == internal_meta.commitment
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing internal store read witness for coordinates {:?}",
                            internal_meta.coordinates
                        )
                    });
                verify_internal_store_read_witness(
                    read_witness,
                    &current_root,
                    current_index_root,
                    &internal_meta.coordinates,
                    &internal_meta.commitment,
                );
                assert_eq!(
                    internal_meta.commitment, internal_meta.selection.source_root_hash,
                    "Internal input '{}' commitment must match raster selection root",
                    binding_name,
                );
                if internal_meta.selection.selected_len > 0 {
                    let witness = internal_selection_witnesses
                        .get(binding_name.as_str())
                        .unwrap_or_else(|| {
                            panic!(
                                "Missing internal selection witness for binding '{}'",
                                binding_name
                            )
                        });
                    assert!(
                        verify_selection_witness(&internal_meta.selection, witness),
                        "Internal input '{}' selection witness is invalid",
                        binding_name,
                    );
                }
            }
        }

        let write_witness = internal_store_witness.and_then(|witness| witness.write.as_ref());

        match write_witness {
            Some(write_witness) => {
                let object_commitment = step_record
                    .output_commitment()
                    .expect("Execution step must expose output commitment")
                    .clone();

                let expected_entry = InternalStoreEntry {
                    coordinates: step_record.coordinates().clone(),
                    object_commitment,
                };
                current_frontier.append(Bytes(internal_store_leaf_hash(&expected_entry)));
                let next_root = frontier_root(current_frontier);
                let next_position: u64 = current_frontier.position().into();
                verify_internal_store_write_witness(
                    write_witness,
                    current_index_root,
                    internal_store_index_root_after,
                    &expected_entry,
                    next_position,
                );
                assert_eq!(
                    internal_store_root_after, &next_root,
                    "Execution-step internal store root after does not match appended internal store root",
                );

                (
                    serialize_frontier(current_frontier),
                    next_root,
                    internal_store_index_root_after.clone(),
                )
            }
            None => {
                assert_eq!(
                    internal_store_root_before, internal_store_root_after,
                    "Execution-step without internal store write must leave append-log root unchanged",
                );
                assert_eq!(
                    internal_store_index_root_before, internal_store_index_root_after,
                    "Execution-step without internal store write must leave index root unchanged",
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
            internal_store_witness.is_none(),
            "Only execution steps may carry internal store witnesses",
        );
        (
            serialize_frontier(current_frontier),
            frontier_root(current_frontier),
            current_index_root.to_vec(),
        )
    }
}
