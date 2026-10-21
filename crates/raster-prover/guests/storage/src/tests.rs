//! Tests of the storage check: one step's storage transition, and a segment of
//! them. The single-step tests moved from the transition guest with the check
//! (`storage-prover` step 4); each step now also carries the input source
//! witness its record commits to, because the check anchors it itself.

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::coordinate_index_membership_proof;
use raster_core::input::SelectionWitness;
use raster_core::recur_progress::RecurProgressStack;
use raster_core::storage_check::{
    selection_witnesses_commitment, StorageSegmentHeader, StorageState, StorageStep,
    StorageStepEntry,
};
use raster_core::trace::{FnInput, FnInputArg, FnInputValue, StepKind, StepRecord, StorageData};
use raster_core::transition::{SerializableFrontier, StorageEntry, StorageWitness, StorageWriteWitness};

use guest_common::input_source::input_source_commitment;
use guest_common::merkle_tree::{frontier_root, serialize_frontier, sha256_bytes, Bytes, EMPTY_LEAF};
use guest_common::storage::{StorageCursor, StorageInput};
use guest_common::storage_fixtures::{
    build_read_witness, build_storage_context, build_write_witness, tile_step_with_store_roots,
};

use crate::segment::verify_segment;

fn sha(bytes: &[u8]) -> Vec<u8> {
    sha256_bytes(bytes)
}

/// The input source of a step that binds nothing: present, because every
/// `Exec` record commits to one, and empty.
fn no_inputs() -> FnInput {
    FnInput {
        data: Vec::new(),
        values: Vec::new(),
        args: Vec::new(),
        storage: BTreeMap::new(),
    }
}

fn storage_input_witness(coordinates: CfsCoordinates, commitment: Vec<u8>) -> FnInput {
    let source_root_hash: [u8; 32] = commitment
        .clone()
        .try_into()
        .expect("test commitments are 32 bytes");
    FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::StorageBinding],
        args: vec![FnInputArg {
            name: "arg".to_string(),
            ty: "Vec<u8>".to_string(),
        }],
        storage: [(
            "arg".to_string(),
            StorageData {
                coordinates,
                commitment,
                selector: Default::default(),
                selection: raster_core::input::SelectionCommitment {
                    source_root_hash,
                    ..Default::default()
                },
            },
        )]
        .into_iter()
        .collect(),
    }
}

/// A storage binding with a *real* selection, unlike `storage_input_witness`
/// whose `selected_len` is 0 and so never reaches the selection branch at all.
/// Returns the input witness plus both witness shapes for it.
fn selecting_input_witness(
    binding: &str,
    coordinates: CfsCoordinates,
) -> (FnInput, Vec<u8>, SelectionWitness, SelectionWitness) {
    use raster_core::input::{
        encode_index_leaf_payload, selected_subtree_root, selection_payload_hash, IndexWidth,
        SelectionProof,
    };
    let payload = encode_index_leaf_payload(7, IndexWidth::U32).unwrap();
    let root = selected_subtree_root(&payload).unwrap();
    let selection = raster_core::input::SelectionCommitment {
        path: Default::default(),
        source_root_hash: root,
        selected_hash: selection_payload_hash(&payload),
        selected_len: payload.len() as u64,
        payload_kind: Default::default(),
    };
    let proof = SelectionProof {
        path: Default::default(),
        root_hash: root,
        steps: Vec::new(),
    };
    let with_payload = SelectionWitness::from_payload(payload, proof);
    let as_reference = with_payload.clone().into_reference().unwrap();
    let input = FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::StorageBinding],
        args: vec![FnInputArg {
            name: binding.to_string(),
            ty: "Vec<u8>".to_string(),
        }],
        storage: [(
            binding.to_string(),
            StorageData {
                coordinates,
                commitment: root.to_vec(),
                selector: Default::default(),
                selection,
            },
        )]
        .into_iter()
        .collect(),
    };
    (input, root.to_vec(), with_payload, as_reference)
}

fn sequence_start_step(coordinates: CfsCoordinates, input_source_commitment: Vec<u8>) -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates,
        kind: StepKind::SequenceStart {
            input_commitment: Vec::new(),
            input_source_commitment,
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    }
}

/// Run the storage check on one step from `frontier`/`index_root`, and return
/// the store after it.
fn verify(
    step: &StepRecord,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
    storage_witness: Option<&StorageWitness>,
    frontier: &NonEmptyFrontier<Bytes>,
    index_root: &[u8],
) -> StorageState {
    let mut store = StorageCursor::new(&serialize_frontier(frontier), index_root.to_vec());
    store.verify_step(&StorageInput {
        step_record: step,
        input_source_witness,
        storage_selection_witnesses,
        storage_witness,
    });
    store.state()
}

/// Drive only the witness-shape rule, which runs before the storage-roots
/// guard and so is the only part of the storage check a `SequenceStart`
/// reaches at all.
fn check_witness_shape(step: &StepRecord, input: &FnInput, witness: SelectionWitness, name: &str) {
    let (frontier, _root, _index, index_root) = build_storage_context(&[]);
    let witnesses: BTreeMap<String, SelectionWitness> =
        [(name.to_string(), witness)].into_iter().collect();
    let _ = verify(step, Some(input), &witnesses, None, &frontier, &index_root);
}

#[test]
fn a_forwarded_sequence_argument_may_omit_its_payload() {
    // The `merge_buckets` shape: a sequence receives the binding as a
    // reference and never consumes its bytes, so the root is all the guest
    // needs and the payload never crosses into the zkVM.
    let (input, _root, _payload, reference) =
        selecting_input_witness("merge_buckets", CfsCoordinates(vec![]));
    let step = sequence_start_step(CfsCoordinates(vec![3, 1, 15]), input_source_commitment(&input));
    check_witness_shape(&step, &input, reference, "merge_buckets");
}

#[test]
#[should_panic(expected = "carries the wrong selection witness shape")]
fn a_forwarded_sequence_argument_may_not_smuggle_a_payload() {
    let (input, _root, payload, _reference) =
        selecting_input_witness("merge_buckets", CfsCoordinates(vec![]));
    let step = sequence_start_step(CfsCoordinates(vec![3, 1, 15]), input_source_commitment(&input));
    check_witness_shape(&step, &input, payload, "merge_buckets");
}

#[test]
#[should_panic(expected = "carries the wrong selection witness shape")]
fn a_recur_source_must_still_carry_its_payload() {
    // `authenticated_source_len` reads the sweep bound `L` out of this
    // binding's metadata payload, so it is the one sequence argument a
    // reference cannot replace.
    let (input, _root, _payload, reference) =
        selecting_input_witness("input", CfsCoordinates(vec![]));
    let step = sequence_start_step(CfsCoordinates(vec![3, 1]), input_source_commitment(&input));
    check_witness_shape(&step, &input, reference, "input");
}

#[test]
#[should_panic(expected = "carries the wrong selection witness shape")]
fn a_tile_step_may_not_replace_a_consumed_value_with_a_reference() {
    // An `Exec` step's tile ran on these bytes, so the payload is the thing
    // being proved and a root says nothing about it.
    let (input, _root, _payload, reference) =
        selecting_input_witness("arg", CfsCoordinates(vec![1]));
    let step = tile_step_with_store_roots(
        1,
        CfsCoordinates(vec![2]),
        input_source_commitment(&input),
        sha(b"out"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    check_witness_shape(&step, &input, reference, "arg");
}

/// The check anchors the input source witness itself: a witness whose
/// argument names differ from the committed ones is refused here, not only in
/// the transition guest.
#[test]
#[should_panic(expected = "does not match recorded source commitment")]
fn the_storage_check_anchors_its_input_source_witness() {
    let (input, _root, payload, _reference) =
        selecting_input_witness("merge_buckets", CfsCoordinates(vec![]));
    let step = sequence_start_step(CfsCoordinates(vec![3, 1, 15]), input_source_commitment(&input));
    let mut renamed = input.clone();
    renamed.args[0].name = "other".to_string();
    let witnesses: BTreeMap<String, SelectionWitness> =
        [("merge_buckets".to_string(), payload)].into_iter().collect();
    let (frontier, _root, _index, index_root) = build_storage_context(&[]);
    let _ = verify(&step, Some(&renamed), &witnesses, None, &frontier, &index_root);
}

#[test]
fn verify_storage_transition_uses_output_commitment_as_keyed_entry() {
    let output_commitment = sha(b"out");
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: output_commitment.clone(),
    };
    let (before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let inputs = no_inputs();
    let step = tile_step_with_store_roots(
        1,
        new_entry.coordinates.clone(),
        input_source_commitment(&inputs),
        output_commitment,
        root_before.clone(),
        root_after.clone(),
        index_root_before.clone(),
        index_root_after.clone(),
    );
    let witness = StorageWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let after = verify(
        &step,
        Some(&inputs),
        &BTreeMap::new(),
        Some(&witness),
        &before_frontier,
        &index_root_before,
    );

    assert_eq!(after.root, root_after);
    assert_eq!(after.index_root, index_root_after);
    assert_eq!(after.position, 1);
}

#[test]
#[should_panic(expected = "Coordinate-index non-membership proof is invalid before write")]
fn verify_storage_transition_rejects_duplicate_coordinates() {
    let existing_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"existing"),
    };
    let (before_frontier, root_before, before_index, index_root_before) =
        build_storage_context(&[existing_entry.clone()]);
    let inputs = no_inputs();
    let step = tile_step_with_store_roots(
        1,
        existing_entry.coordinates.clone(),
        input_source_commitment(&inputs),
        sha(b"out"),
        root_before.clone(),
        root_before,
        index_root_before.clone(),
        index_root_before.clone(),
    );
    let witness = StorageWitness {
        reads: Vec::new(),
        write: Some(StorageWriteWitness {
            entry: StorageEntry {
                coordinates: existing_entry.coordinates.clone(),
                object_commitment: sha(b"out"),
            },
            index_non_membership_witness:
                raster_core::transition::CoordinateIndexNonMembershipProof {
                    coordinates: existing_entry.coordinates.clone(),
                    siblings: vec![[0; 32].into(); 256],
                },
            index_membership_witness: coordinate_index_membership_proof(
                &before_index,
                &existing_entry.coordinates,
            )
            .expect("existing coordinate should have membership proof"),
        }),
    };

    let _ = verify(
        &step,
        Some(&inputs),
        &BTreeMap::new(),
        Some(&witness),
        &before_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(expected = "Missing storage read witness for coordinates CfsCoordinates([1])")]
fn verify_storage_transition_rejects_wrong_coordinates_with_correct_bytes() {
    let prior_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![10]),
        object_commitment: sha(b"shared"),
    };
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![2]),
        object_commitment: sha(b"out"),
    };
    let (before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[prior_entry.clone()]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[prior_entry.clone(), new_entry.clone()]);
    let input_source_witness = storage_input_witness(
        CfsCoordinates(vec![1]),
        prior_entry.object_commitment.clone(),
    );
    let step = tile_step_with_store_roots(
        2,
        new_entry.coordinates.clone(),
        input_source_commitment(&input_source_witness),
        new_entry.object_commitment.clone(),
        root_before.clone(),
        root_after,
        index_root_before.clone(),
        index_root_after,
    );
    let witness = StorageWitness {
        reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
        write: Some(build_write_witness(&[prior_entry], &new_entry)),
    };

    let _ = verify(
        &step,
        Some(&input_source_witness),
        &BTreeMap::new(),
        Some(&witness),
        &before_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(expected = "Execution-step storage root before does not match current storage root")]
fn verify_storage_transition_rejects_stale_root() {
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"out"),
    };
    let (_before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let inputs = no_inputs();
    let step = tile_step_with_store_roots(
        3,
        new_entry.coordinates.clone(),
        input_source_commitment(&inputs),
        new_entry.object_commitment.clone(),
        root_before,
        root_after,
        index_root_before.clone(),
        index_root_after,
    );
    let stale_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![100]),
        object_commitment: sha(b"stale"),
    };
    let (stale_frontier, _stale_root, _stale_index, _stale_index_root) =
        build_storage_context(&[stale_entry]);
    let witness = StorageWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let _ = verify(
        &step,
        Some(&inputs),
        &BTreeMap::new(),
        Some(&witness),
        &stale_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(
    expected = "Execution-step storage index root before does not match current index root"
)]
fn verify_storage_transition_rejects_stale_index_root() {
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"out"),
    };
    let (before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let inputs = no_inputs();
    let step = tile_step_with_store_roots(
        33,
        new_entry.coordinates.clone(),
        input_source_commitment(&inputs),
        new_entry.object_commitment.clone(),
        root_before,
        root_after,
        index_root_before.clone(),
        index_root_after,
    );
    let witness = StorageWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let _ = verify(
        &step,
        Some(&inputs),
        &BTreeMap::new(),
        Some(&witness),
        &before_frontier,
        &[9; 32],
    );
}

#[test]
fn verify_storage_transition_accepts_non_empty_initial_state() {
    let prior_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"prior"),
    };
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![2]),
        object_commitment: sha(b"next"),
    };
    let (before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[prior_entry.clone()]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[prior_entry.clone(), new_entry.clone()]);
    let input_source_witness = storage_input_witness(
        prior_entry.coordinates.clone(),
        prior_entry.object_commitment.clone(),
    );
    let step = tile_step_with_store_roots(
        4,
        new_entry.coordinates.clone(),
        input_source_commitment(&input_source_witness),
        new_entry.object_commitment.clone(),
        root_before.clone(),
        root_after.clone(),
        index_root_before.clone(),
        index_root_after.clone(),
    );
    let witness = StorageWitness {
        reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
        write: Some(build_write_witness(&[prior_entry], &new_entry)),
    };

    let after = verify(
        &step,
        Some(&input_source_witness),
        &BTreeMap::new(),
        Some(&witness),
        &before_frontier,
        &index_root_before,
    );

    assert_eq!(after.root, root_after);
    assert_eq!(after.index_root, index_root_after);
}

/// A writing step kind that wrote nothing records no commitment (moved from
/// the transition guest, whose io half — a tile that returned nothing records
/// none — stays there).
#[test]
#[should_panic(expected = "wrote nothing, so it must record no output commitment")]
fn a_step_that_wrote_nothing_may_not_record_a_commitment() {
    let (frontier, root, _index, index_root) = build_storage_context(&[]);
    let inputs = no_inputs();
    let step = tile_step_with_store_roots(
        1,
        CfsCoordinates(vec![1]),
        input_source_commitment(&inputs),
        sha(b"a write that never happened"),
        root.clone(),
        root,
        index_root.clone(),
        index_root.clone(),
    );
    let _ = verify(&step, Some(&inputs), &BTreeMap::new(), None, &frontier, &index_root);
}

// ============================================================================
// Segments
// ============================================================================

/// An honest window of three steps over an empty store: a tile writes `[1]`, a
/// sequence start between them touches nothing, and a second tile reads `[1]`
/// and writes `[2]`.
fn three_step_window() -> (StorageSegmentHeader, Vec<StorageStep>) {
    let first = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"first"),
    };
    let second = StorageEntry {
        coordinates: CfsCoordinates(vec![3, 1]),
        object_commitment: sha(b"second"),
    };
    let (empty, root_0, _, index_0) = build_storage_context(&[]);
    let (_, root_1, _, index_1) = build_storage_context(&[first.clone()]);
    let (_, root_2, _, index_2) = build_storage_context(&[first.clone(), second.clone()]);

    let inputs = no_inputs();
    let write_first = tile_step_with_store_roots(
        10,
        first.coordinates.clone(),
        input_source_commitment(&inputs),
        first.object_commitment.clone(),
        root_0,
        root_1.clone(),
        index_0.clone(),
        index_1.clone(),
    );
    let open = sequence_start_step(CfsCoordinates(vec![3]), input_source_commitment(&inputs));
    let reads_first = storage_input_witness(first.coordinates.clone(), first.object_commitment.clone());
    let write_second = tile_step_with_store_roots(
        12,
        second.coordinates.clone(),
        input_source_commitment(&reads_first),
        second.object_commitment.clone(),
        root_1,
        root_2,
        index_1,
        index_2,
    );
    let steps = vec![
        StorageStep {
            step_record: write_first,
            input_source_witness: Some(inputs.clone()),
            storage_selection_witnesses: BTreeMap::new(),
            storage_witness: Some(StorageWitness {
                reads: Vec::new(),
                write: Some(build_write_witness(&[], &first)),
            }),
        },
        StorageStep {
            step_record: open,
            input_source_witness: Some(inputs),
            storage_selection_witnesses: BTreeMap::new(),
            storage_witness: None,
        },
        StorageStep {
            step_record: write_second,
            input_source_witness: Some(reads_first),
            storage_selection_witnesses: BTreeMap::new(),
            storage_witness: Some(StorageWitness {
                reads: vec![build_read_witness(&[first.clone()], &first)],
                write: Some(build_write_witness(&[first], &second)),
            }),
        },
    ];
    let header = StorageSegmentHeader {
        start_frontier: serialize_frontier(&empty),
        start_index_root: index_0,
        first_position: 10,
        step_count: steps.len() as u64,
    };
    (header, steps)
}

fn run(header: StorageSegmentHeader, steps: Vec<StorageStep>) -> Vec<StorageStepEntry> {
    let mut steps = steps.into_iter();
    verify_segment(header, || steps.next().expect("the header counts the steps"))
}

#[test]
fn a_segment_carries_the_store_from_step_to_step() {
    let (header, steps) = three_step_window();
    let records: Vec<_> = steps.iter().map(|step| step.step_record.clone()).collect();
    let entries = run(header, steps);

    assert_eq!(entries.len(), 3);
    for (offset, entry) in entries.iter().enumerate() {
        assert_eq!(entry.trace_position, 10 + offset as u64);
        assert_eq!(entry.step_record_hash, guest_common::merkle_tree::hash_trace_item(&records[offset]));
        assert_eq!(entry.selection_witnesses_commitment, selection_witnesses_commitment(&BTreeMap::new()));
    }
    for pair in entries.windows(2) {
        assert_eq!(pair[0].after, pair[1].before, "each step starts where the last ended");
    }
    assert_eq!(entries[0].before.position, 0);
    assert_eq!(entries[0].after.position, 1);
    assert_eq!(entries[1].before, entries[1].after, "a sequence start touches nothing");
    assert_eq!(entries[2].after.position, 2);
    assert_eq!(entries[2].after.root, records[2].storage_roots().unwrap().root_after);
}

/// The modes agree by construction: the window as one segment, and as
/// one-step segments each started where the last ended, commit identical
/// entries.
#[test]
fn one_segment_and_one_step_segments_commit_the_same_entries() {
    let (header, steps) = three_step_window();
    let whole = run(header.clone(), steps.clone());

    let mut per_step = Vec::new();
    let mut frontier = header.start_frontier.clone();
    let mut index_root = header.start_index_root.clone();
    for (offset, step) in steps.into_iter().enumerate() {
        let write = step
            .storage_witness
            .as_ref()
            .and_then(|witness| witness.write.as_ref())
            .map(|write| write.entry.clone());
        let entry = run(
            StorageSegmentHeader {
                start_frontier: frontier.clone(),
                start_index_root: index_root.clone(),
                first_position: header.first_position + offset as u64,
                step_count: 1,
            },
            vec![step],
        )
        .remove(0);
        // The host's walk: append each write, take the index root after.
        if let Some(entry) = write {
            let mut live = guest_common::merkle_tree::deserialize_frontier(&frontier).unwrap();
            live.append(Bytes(guest_common::storage::storage_leaf_hash(&entry)));
            frontier = serialize_frontier(&live);
        }
        index_root = entry.after.index_root.clone();
        per_step.push(entry);
    }
    assert_eq!(whole, per_step);
}

/// A frontier one position further on, ending in an empty leaf, folds to the
/// same root as the empty store — which is why the entry commits the position
/// too. Here the guest refuses it on its own: the first write lands at log
/// position 2, and the honest index witness says 1.
#[test]
#[should_panic(expected = "Coordinate-index membership proof log position does not match append-log position")]
fn a_start_frontier_with_the_right_root_at_the_wrong_position_shifts_every_write() {
    let (mut header, steps) = three_step_window();
    let shifted = SerializableFrontier {
        position: 1,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: vec![raster_core::merkle::to_node_hash(&EMPTY_LEAF)],
    };
    let honest = guest_common::merkle_tree::deserialize_frontier(&header.start_frontier).unwrap();
    let forged = guest_common::merkle_tree::deserialize_frontier(&shifted).unwrap();
    assert_eq!(frontier_root(&honest), frontier_root(&forged), "same root, different position");
    header.start_frontier = shifted;
    let _ = run(header, steps);
}

#[test]
#[should_panic(expected = "Execution-step storage root before does not match current storage root")]
fn a_segment_started_from_another_store_is_refused() {
    let (mut header, steps) = three_step_window();
    let (elsewhere, ..) = build_storage_context(&[StorageEntry {
        coordinates: CfsCoordinates(vec![9]),
        object_commitment: sha(b"elsewhere"),
    }]);
    header.start_frontier = serialize_frontier(&elsewhere);
    let _ = run(header, steps);
}

#[test]
#[should_panic(expected = "a storage segment must cover at least one step")]
fn an_empty_segment_is_refused() {
    let (mut header, _) = three_step_window();
    header.step_count = 0;
    let _ = run(header, Vec::new());
}
