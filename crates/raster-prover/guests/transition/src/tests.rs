use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource, RecurTileItem,
    SequenceChildItem, SequenceDef, SequenceItem, TileDef, TileItem,
};
use raster_core::coordinate_index::{
    coordinate_index_membership_proof, coordinate_index_non_membership_proof, coordinate_index_root,
};
use raster_core::draft::{
    draft_root_from_witness, draft_value_root, schema_hash as compute_schema_hash, DraftOp,
    DraftReplayTransition, DraftStateWitness, DraftTransitionWitness, DraftWitnessField,
    RecurControlKind,
    RecurPosition, RecurTileReplay, TileReplayJournal, TrackedDraftState,
};
use raster_core::input::{
    AppendFrontier, SchemaField, SchemaFieldMode, SchemaNode, Selectable,
};
use raster_core::recur_progress::RecurProgressStack;
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, FnInputArg, FnInputValue, StepKind, StepRecord, StorageData,
    StorageRoots,
};
use raster_core::transition::{
    SerializableFrontier, StorageEntry, StorageLogWitness, StorageReadWitness, StorageWitness,
    StorageWriteWitness,
};

use crate::checks::cfs::verify_step_record_inputs;
use crate::checks::drafts::verify_draft_transition;
use crate::checks::io::{input_source_commitment, verify_io_witness};
use crate::checks::store::{storage_leaf_hash, verify_storage_transition};
use crate::merkle_tree::{
    deserialize_frontier, frontier_root, sha256_bytes, sha256_hex, Bytes, TraceBridgeTree,
    EMPTY_LEAF,
};

fn sha(bytes: &[u8]) -> Vec<u8> {
    sha256_bytes(bytes)
}

struct DemoDraft;

impl Selectable for DemoDraft {
    fn schema() -> SchemaNode {
        SchemaNode::Struct {
            type_name: "DemoDraft".into(),
            fields: vec![
                SchemaField {
                    name: "title".into(),
                    label: "Title".into(),
                    mode: SchemaFieldMode::SetOnce,
                    schema: Box::new(SchemaNode::Leaf {
                        type_name: "String".into(),
                    }),
                },
                SchemaField {
                    name: "items".into(),
                    label: "Items".into(),
                    mode: SchemaFieldMode::AppendOnlyVec,
                    schema: Box::new(SchemaNode::List {
                        type_name: "Vec<String>".into(),
                        element: Box::new(SchemaNode::Leaf {
                            type_name: "String".into(),
                        }),
                    }),
                },
            ],
        }
    }
}

fn draft_tile_step(exec_index: u64) -> StepRecord {
    StepRecord {
        exec_index,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![exec_index as u32]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("collect_lines".into()),
            intra_sequence_index: exec_index as u32,
            input_commitment: vec![exec_index as u8; 32],
            input_source_commitment: vec![0; 32],
            output_commitment: vec![1; 32],
            storage: StorageRoots {
                root_before: EMPTY_LEAF.to_vec(),
                root_after: EMPTY_LEAF.to_vec(),
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    }
}

fn authorization_journal(binding_name: &str, commitment: &[u8]) -> AuthorizationJournal {
    AuthorizationJournal {
        external_inputs_commitments: [(binding_name.to_string(), commitment.to_vec())]
            .into_iter()
            .collect(),
        input_manifest_commitment: vec![7; 32],
    }
}

fn storage_input_witness(coordinates: CfsCoordinates, commitment: Vec<u8>) -> FnInput {
    // A whole-object storage binding's commitment *is* its raster selection
    // root (see `checks::store`'s structural-consistency assertion), so the
    // fixture mirrors that invariant.
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

fn producer_sequence_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![
            TileDef::iter("producer", 0, 1),
            TileDef::iter("consumer", 1, 1),
        ],
        sequences: vec![
            SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![
                    SequenceChildItem::Sequence(SequenceItem {
                        id: "sub".into(),
                        sources: vec![],
                    }),
                    SequenceChildItem::Tile(TileItem {
                        id: "consumer".into(),
                        sources: vec![InputBinding::PriorItemOutput {
                            intra_sequence_item_index: 0,
                        }],
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
            },
            SequenceDef {
                id: "sub".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "producer".into(),
                    sources: vec![InputBinding::Direct(InputSource::Inline)],
                })],
                entry_arguments: vec![],
                produces_output: false,
            },
        ],
    })
}

#[test]
fn verify_tile_commitments_accept_matching_recorded_io() {
    let step = StepRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("tile".to_string()),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            input_source_commitment: Vec::new(),
            output_commitment: sha(b"out"),
            storage: StorageRoots {
                root_before: vec![0; 32],
                root_after: vec![0; 32],
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };

    verify_io_witness(&step, Some(&b"in".to_vec()), Some(&b"out".to_vec()));
}

#[test]
fn verify_step_record_inputs_accepts_sequence_descendant_producer_coordinates() {
    let cfs_cursor = producer_sequence_cfs();
    let step_record = StepRecord {
        exec_index: 1,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![1]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("consumer".into()),
            intra_sequence_index: 1,
            input_commitment: Vec::new(),
            input_source_commitment: Vec::new(),
            output_commitment: Vec::new(),
            storage: StorageRoots {
                root_before: Vec::new(),
                root_after: Vec::new(),
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };
    let input_source_witness =
        storage_input_witness(CfsCoordinates(vec![0, 0]), sha(b"producer-output"));

    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&input_source_witness),
        None,
        None,
    );
}

fn chunked_recur_cfs(chunk: Option<u64>) -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("collect", 0, 1)],
        sequences: vec![SequenceDef {
            id: "main".into(),
            input_sources: vec![],
            items: vec![SequenceChildItem::RecurTile(RecurTileItem {
                id: "collect".into(),
                sources: vec![],
                chunk,
            })],
            entry_arguments: Vec::new(),
            produces_output: false,
        }],
    })
}

fn recur_iteration_step(iteration: u32) -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![0, iteration]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::RecurTile("collect".into()),
            intra_sequence_index: 0,
            input_commitment: Vec::new(),
            input_source_commitment: Vec::new(),
            output_commitment: Vec::new(),
            storage: StorageRoots {
                root_before: Vec::new(),
                root_after: Vec::new(),
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    }
}

/// A chunked iteration's replay-proven recur facts.
///
/// The element count used to be inferred from the leading postcard varint of
/// the iteration's ABI bytes; it is now a field the tile commits and the replay
/// receipt covers, so these tests hand the checker a journal rather than a
/// byte-layout puzzle.
fn recur_journal(
    iteration_index: u64,
    declared_iterations: u64,
    consumed_elements: u64,
    control: RecurControlKind,
) -> TileReplayJournal {
    TileReplayJournal {
        input_commitment: [0u8; 32],
        output_bytes: Vec::new(),
        draft_transition: None,
        recur: Some(RecurTileReplay {
            position: RecurPosition {
                iteration_index,
                declared_iterations,
                consumed_elements,
            },
            control,
        }),
    }
}

#[test]
fn verify_step_record_inputs_accepts_declared_chunk_sizes() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    // A full chunk mid-sweep, and a short chunk on the final iteration.
    for (iteration, declared_iterations, consumed) in [(0u64, 3u64, 2u64), (2, 3, 1)] {
        let journal = recur_journal(
            iteration,
            declared_iterations,
            consumed,
            RecurControlKind::Continue,
        );
        verify_step_record_inputs(
            &cfs_cursor,
            &recur_iteration_step(iteration as u32),
            None,
            None,
            Some(&journal),
        );
    }
}

#[test]
fn verify_step_record_inputs_ignores_chunking_when_not_declared() {
    let cfs_cursor = chunked_recur_cfs(None);
    // Without a declared chunk an iteration step carries no chunk obligation.
    verify_step_record_inputs(&cfs_cursor, &recur_iteration_step(0), None, None, None);
}

#[test]
#[should_panic(expected = "exceeds declared chunk size")]
fn verify_step_record_inputs_rejects_oversized_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 3, RecurControlKind::Continue);
    verify_step_record_inputs(
        &cfs_cursor,
        &recur_iteration_step(0),
        None,
        None,
        Some(&journal),
    );
}

#[test]
#[should_panic(expected = "empty chunk")]
fn verify_step_record_inputs_rejects_empty_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 0, RecurControlKind::Continue);
    verify_step_record_inputs(
        &cfs_cursor,
        &recur_iteration_step(0),
        None,
        None,
        Some(&journal),
    );
}

/// Only the final chunk may be short — the `4,1,4,1` shape, rejected on the
/// iteration that actually goes short.
///
/// The native recorder used to enforce this by remembering the previous
/// iteration's length. `declared_iterations` makes it a stateless fact of the
/// iteration itself, so the guest can check it without carrying state across
/// steps.
#[test]
#[should_panic(expected = "smaller than declared chunk size")]
fn verify_step_record_inputs_rejects_short_non_final_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 1, RecurControlKind::Continue);
    verify_step_record_inputs(
        &cfs_cursor,
        &recur_iteration_step(0),
        None,
        None,
        Some(&journal),
    );
}

#[test]
#[should_panic(expected = "missing its replay-proven recur facts")]
fn verify_step_record_inputs_requires_recur_facts_for_declared_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    verify_step_record_inputs(&cfs_cursor, &recur_iteration_step(0), None, None, None);
}

#[test]
#[should_panic(expected = "Step input commitment does not match recorded input bytes")]
fn verify_tile_commitments_reject_mismatched_input() {
    let step = StepRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("tile".to_string()),
            intra_sequence_index: 0,
            input_commitment: sha(b"expected"),
            input_source_commitment: Vec::new(),
            output_commitment: sha(b"out"),
            storage: StorageRoots {
                root_before: vec![0; 32],
                root_after: vec![0; 32],
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };

    verify_io_witness(&step, Some(&b"actual".to_vec()), Some(&b"out".to_vec()));
}

#[test]
fn verify_sequence_boundary_commitments_accept_matching_recorded_io() {
    let start = StepRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::SequenceStart {
            input_commitment: sha(b"sequence-in"),
            input_source_commitment: Vec::new(),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };
    let end = StepRecord {
        exec_index: 2,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::SequenceEnd {
            output_commitment: sha(b"sequence-out"),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };

    verify_io_witness(&start, Some(&b"sequence-in".to_vec()), None);
    verify_io_witness(&end, None, Some(&b"sequence-out".to_vec()));
}

fn empty_storage_frontier_for_test() -> NonEmptyFrontier<Bytes> {
    deserialize_frontier(&SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    })
    .expect("empty storage frontier should deserialize")
}

fn build_storage_context(
    entries: &[StorageEntry],
) -> (
    NonEmptyFrontier<Bytes>,
    Vec<u8>,
    BTreeMap<CfsCoordinates, raster_core::transition::StorageIndexValue>,
    Vec<u8>,
) {
    let mut frontier = empty_storage_frontier_for_test();
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

fn build_storage_log_witness_for_entries(
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
        path_elems: auth_path.iter().map(|elem| elem.0.clone()).collect(),
    }
}

fn build_read_witness(entries: &[StorageEntry], entry: &StorageEntry) -> StorageReadWitness {
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

fn build_write_witness(
    before_entries: &[StorageEntry],
    new_entry: &StorageEntry,
) -> StorageWriteWitness {
    let (_frontier, _root, before_index, _before_index_root) =
        build_storage_context(before_entries);
    let mut after_entries = before_entries.to_vec();
    after_entries.push(new_entry.clone());
    let (_frontier, _root, after_index, _after_index_root) = build_storage_context(&after_entries);
    StorageWriteWitness {
        entry: new_entry.clone(),
        index_non_membership_witness: coordinate_index_non_membership_proof(
            &before_index,
            &new_entry.coordinates,
        ),
        index_membership_witness: coordinate_index_membership_proof(
            &after_index,
            &new_entry.coordinates,
        )
        .expect("membership proof should exist after write"),
    }
}

fn tile_step_with_store_roots(
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
            intra_sequence_index: 0,
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
    }
}

#[test]
fn verify_storage_transition_uses_output_commitment_as_keyed_entry() {
    let output_commitment = sha(b"out");
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: output_commitment.clone(),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let step = tile_step_with_store_roots(
        1,
        new_entry.coordinates.clone(),
        Vec::new(),
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

    let (_next_frontier, next_root, next_index_root) = verify_storage_transition(
        &step,
        None,
        &BTreeMap::new(),
        Some(&b"out".to_vec()),
        Some(&witness),
        &mut before_frontier,
        &index_root_before,
    );

    assert_eq!(next_root, root_after);
    assert_eq!(next_index_root, index_root_after);
}

#[test]
#[should_panic(expected = "Coordinate-index non-membership proof is invalid before write")]
fn verify_storage_transition_rejects_duplicate_coordinates() {
    let existing_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"existing"),
    };
    let (mut before_frontier, root_before, before_index, index_root_before) =
        build_storage_context(&[existing_entry.clone()]);
    let step = tile_step_with_store_roots(
        1,
        existing_entry.coordinates.clone(),
        Vec::new(),
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
                    siblings: vec![vec![0; 32]; 256],
                },
            index_membership_witness: coordinate_index_membership_proof(
                &before_index,
                &existing_entry.coordinates,
            )
            .expect("existing coordinate should have membership proof"),
        }),
    };

    let _ = verify_storage_transition(
        &step,
        None,
        &BTreeMap::new(),
        Some(&b"out".to_vec()),
        Some(&witness),
        &mut before_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(expected = "Missing storage read witness for coordinates CfsCoordinates([0])")]
fn verify_storage_transition_rejects_wrong_coordinates_with_correct_bytes() {
    let prior_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![9]),
        object_commitment: sha(b"shared"),
    };
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"out"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[prior_entry.clone()]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[prior_entry.clone(), new_entry.clone()]);
    let input_source_witness = storage_input_witness(
        CfsCoordinates(vec![0]),
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

    let _ = verify_storage_transition(
        &step,
        Some(&input_source_witness),
        &BTreeMap::new(),
        Some(&b"out".to_vec()),
        Some(&witness),
        &mut before_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(expected = "Execution-step storage root before does not match current storage root")]
fn verify_storage_transition_rejects_stale_root() {
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"out"),
    };
    let (_before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let step = tile_step_with_store_roots(
        3,
        new_entry.coordinates.clone(),
        Vec::new(),
        new_entry.object_commitment.clone(),
        root_before,
        root_after,
        index_root_before.clone(),
        index_root_after,
    );
    let stale_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![99]),
        object_commitment: sha(b"stale"),
    };
    let (mut stale_frontier, _stale_root, _stale_index, _stale_index_root) =
        build_storage_context(&[stale_entry]);
    let witness = StorageWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let _ = verify_storage_transition(
        &step,
        None,
        &BTreeMap::new(),
        Some(&b"out".to_vec()),
        Some(&witness),
        &mut stale_frontier,
        &index_root_before,
    );
}

#[test]
#[should_panic(
    expected = "Execution-step storage index root before does not match current index root"
)]
fn verify_storage_transition_rejects_stale_index_root() {
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"out"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_storage_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_storage_context(&[new_entry.clone()]);
    let step = tile_step_with_store_roots(
        33,
        new_entry.coordinates.clone(),
        Vec::new(),
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

    let _ = verify_storage_transition(
        &step,
        None,
        &BTreeMap::new(),
        Some(&b"out".to_vec()),
        Some(&witness),
        &mut before_frontier,
        &[9; 32],
    );
}

#[test]
fn verify_storage_transition_accepts_non_empty_initial_state() {
    let prior_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"prior"),
    };
    let new_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"next"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
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

    let (_next_frontier, next_root, next_index_root) = verify_storage_transition(
        &step,
        Some(&input_source_witness),
        &BTreeMap::new(),
        Some(&b"next".to_vec()),
        Some(&witness),
        &mut before_frontier,
        &index_root_before,
    );

    assert_eq!(next_root, root_after);
    assert_eq!(next_index_root, index_root_after);
}

/// The witness form of an append field: the right edge of the list's Merkle
/// tree, built from the element roots the guest would derive from `ops`.
fn append_frontier_of(items: &[&str]) -> AppendFrontier {
    let roots: Vec<[u8; 32]> = items
        .iter()
        .map(|item| {
            draft_value_root(&raster_core::draft::DraftValue::String((*item).into())).unwrap()
        })
        .collect();
    AppendFrontier::from_leaf_roots(&roots)
}

#[test]
fn verify_draft_transition_tracks_multi_step_chain() {
    let empty_witness = DraftStateWitness {
        schema: DemoDraft::schema(),
        fields: Vec::new(),
    };
    let empty_root =
        draft_root_from_witness(&empty_witness.schema, &BTreeMap::new()).unwrap();
    let schema_hash = compute_schema_hash(&empty_witness.schema);
    let draft_id = [7; 32];
    let mut active_drafts = BTreeMap::new();

    let step_one = TileReplayJournal {
        input_commitment: [0u8; 32],
        output_bytes: Vec::new(),
        draft_transition: Some(DraftReplayTransition {
            draft_id,
            schema_hash,
            root_before: empty_root,
            ops: vec![
                DraftOp::Set {
                    field: "title".into(),
                    value: raster_core::draft::DraftValue::String("collected".into()),
                },
                DraftOp::Push {
                    field: "items".into(),
                    value: raster_core::draft::DraftValue::String("first".into()),
                },
            ],
        }),
        recur: None,
    };
    verify_draft_transition(
        &draft_tile_step(1),
        Some(&step_one),
        Some(&DraftTransitionWitness {
            pre_state: empty_witness.clone(),
            native_transition: step_one.draft_transition.clone(),
        }),
        &mut active_drafts,
    );
    let step_one_root = active_drafts.get(&draft_id).unwrap().root;

    let step_two_witness = DraftStateWitness {
        schema: empty_witness.schema.clone(),
        fields: vec![
            (
                "title".into(),
                DraftWitnessField::Set(raster_core::draft::DraftValue::String("collected".into())),
            ),
            (
                "items".into(),
                DraftWitnessField::Append(append_frontier_of(&["first"])),
            ),
        ],
    };
    let step_two = TileReplayJournal {
        input_commitment: [0u8; 32],
        output_bytes: Vec::new(),
        draft_transition: Some(DraftReplayTransition {
            draft_id,
            schema_hash,
            root_before: step_one_root,
            ops: vec![DraftOp::Push {
                field: "items".into(),
                value: raster_core::draft::DraftValue::String("second".into()),
            }],
        }),
        recur: None,
    };
    verify_draft_transition(
        &draft_tile_step(2),
        Some(&step_two),
        Some(&DraftTransitionWitness {
            pre_state: step_two_witness,
            native_transition: step_two.draft_transition.clone(),
        }),
        &mut active_drafts,
    );

    assert_ne!(active_drafts.get(&draft_id).unwrap().root, step_one_root);
}

#[test]
#[should_panic(expected = "root_before does not match tracked draft root")]
fn verify_draft_transition_rejects_wrong_root_before() {
    let witness = DraftStateWitness {
        schema: DemoDraft::schema(),
        fields: Vec::new(),
    };
    let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new()).unwrap();
    let schema_hash = compute_schema_hash(&witness.schema);
    let draft_id = [9; 32];
    let mut active_drafts = BTreeMap::from([(
        draft_id,
        TrackedDraftState {
            schema_hash,
            root: [1; 32],
        },
    )]);

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            input_commitment: [0u8; 32],
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id,
                schema_hash,
                root_before: empty_root,
                ops: Vec::new(),
            }),
            recur: None,
        }),
        Some(&DraftTransitionWitness {
            pre_state: witness,
            native_transition: None,
        }),
        &mut active_drafts,
    );
}

#[test]
#[should_panic(expected = "schema hash")]
fn verify_draft_transition_rejects_wrong_schema_hash() {
    let witness = DraftStateWitness {
        schema: DemoDraft::schema(),
        fields: Vec::new(),
    };
    let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new()).unwrap();
    let mut active_drafts = BTreeMap::new();

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            input_commitment: [0u8; 32],
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id: [3; 32],
                schema_hash: [4; 32],
                root_before: empty_root,
                ops: Vec::new(),
            }),
            recur: None,
        }),
        Some(&DraftTransitionWitness {
            pre_state: witness,
            native_transition: None,
        }),
        &mut active_drafts,
    );
}

#[test]
#[should_panic(expected = "witness root")]
fn verify_draft_transition_rejects_tampered_pre_state_witness() {
    let empty_root =
        draft_root_from_witness(&DemoDraft::schema(), &BTreeMap::new()).unwrap();
    let mut active_drafts = BTreeMap::new();

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            input_commitment: [0u8; 32],
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id: [6; 32],
                schema_hash: compute_schema_hash(&DemoDraft::schema()),
                root_before: empty_root,
                ops: Vec::new(),
            }),
            recur: None,
        }),
        Some(&DraftTransitionWitness {
            pre_state: DraftStateWitness {
                schema: DemoDraft::schema(),
                fields: vec![(
                    "title".into(),
                    DraftWitnessField::Set(raster_core::draft::DraftValue::String(
                        "tampered".into(),
                    )),
                )],
            },
            native_transition: None,
        }),
        &mut active_drafts,
    );
}

/// The frontier twin of the tampered-witness test above.
///
/// A witness now proves the *shape* of the accumulated list rather than
/// exhibiting it, so the thing a forger reaches for is a frontier claiming a
/// different length. It must fail exactly where a wrong element value fails:
/// the root is recomputed from `(len, edge)`, so a forged length yields a
/// different root and never matches `root_before`.
#[test]
#[should_panic(expected = "witness root")]
fn verify_draft_transition_rejects_a_frontier_claiming_the_wrong_length() {
    let honest = DraftStateWitness {
        schema: DemoDraft::schema(),
        fields: vec![(
            "items".into(),
            DraftWitnessField::Append(append_frontier_of(&["first", "second"])),
        )],
    };
    let root_before = draft_root_from_witness(
        &honest.schema,
        &raster_core::draft::witness_fields_map(&honest.fields),
    )
    .unwrap();

    let mut forged = honest;
    let DraftWitnessField::Append(frontier) = &mut forged.fields[0].1 else {
        unreachable!("the fixture field is an append field");
    };
    frontier.len += 1;

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            input_commitment: [0u8; 32],
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id: [8; 32],
                schema_hash: compute_schema_hash(&DemoDraft::schema()),
                root_before,
                ops: Vec::new(),
            }),
            recur: None,
        }),
        Some(&DraftTransitionWitness {
            pre_state: forged,
            native_transition: None,
        }),
        &mut BTreeMap::new(),
    );
}

use crate::checks::entrypoint::{combined_root, verify_genesis_authorization, verify_step};
use raster_core::trace::ProgramStartStep;
use raster_core::transition::EntrypointAuthorization;

fn entrypoint_cfs(names: Vec<String>) -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![],
        sequences: vec![SequenceDef {
            id: "main".into(),
            input_sources: vec![],
            items: vec![],
            entry_arguments: names,
            produces_output: false,
        }],
    })
}

fn no_entrypoint_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![],
        sequences: vec![SequenceDef::new("main")],
    })
}

fn two_arg_authorization_journal(commitment_a: &[u8], commitment_b: &[u8]) -> AuthorizationJournal {
    AuthorizationJournal {
        external_inputs_commitments: [
            ("personal_data".to_string(), commitment_a.to_vec()),
            ("seed".to_string(), commitment_b.to_vec()),
        ]
        .into_iter()
        .collect(),
        input_manifest_commitment: vec![7; 32],
    }
}

fn dummy_storage_roots() -> StorageRoots {
    StorageRoots {
        root_before: EMPTY_LEAF.to_vec(),
        root_after: EMPTY_LEAF.to_vec(),
        index_root_before: Vec::new(),
        index_root_after: Vec::new(),
    }
}

fn program_start_step(
    entry_arguments: Vec<String>,
    output_commitment: Vec<u8>,
) -> ProgramStartStep {
    ProgramStartStep {
        entry_arguments,
        output_commitment,
        storage: dummy_storage_roots(),
    }
}

fn program_start_record(program_start: ProgramStartStep) -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::ProgramStart(program_start),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    }
}

#[test]
fn combined_root_matches_struct_hash_convention_over_declared_commitments() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);

    let actual = combined_root(&["personal_data".to_string(), "seed".to_string()], &journal);

    // The binding is an ordinary struct node over (name, commitment) pairs —
    // the same convention the selection tree uses, which is what lets a
    // selection into one argument be one ordinary proof step.
    let expected = raster_core::input::struct_commitments_root([
        ("personal_data", commitment_a.as_slice()),
        ("seed", commitment_b.as_slice()),
    ])
    .to_vec();

    assert_eq!(actual, expected);

    // Order matters: declaring the same two arguments in the opposite order
    // must produce a different root.
    let swapped = combined_root(&["seed".to_string(), "personal_data".to_string()], &journal);
    assert_ne!(actual, swapped);
}

#[test]
fn verify_step_accepts_matching_binding_and_establishes_authorization() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let cfs_cursor = entrypoint_cfs(names.clone());
    let program_start = program_start_step(names.clone(), combined_root(&names, &journal));
    let record = program_start_record(program_start.clone());

    assert_eq!(
        verify_step(&cfs_cursor, &record, &program_start, &journal),
        EntrypointAuthorization::Established,
    );
}

#[test]
fn verify_step_establishes_nothing_required_when_no_entry_arguments_declared() {
    let journal = two_arg_authorization_journal(&sha(b"a"), &sha(b"b"));
    let cfs_cursor = no_entrypoint_cfs();
    let program_start = program_start_step(Vec::new(), Vec::new());
    let record = program_start_record(program_start.clone());

    assert_eq!(
        verify_step(&cfs_cursor, &record, &program_start, &journal),
        EntrypointAuthorization::NotRequired,
    );
}

#[test]
#[should_panic(expected = "does not match the authorized entry-argument commitments")]
fn verify_step_rejects_tampered_output_commitment() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let cfs_cursor = entrypoint_cfs(names.clone());
    let program_start = program_start_step(names, vec![0xff; 32]);
    let record = program_start_record(program_start.clone());

    verify_step(&cfs_cursor, &record, &program_start, &journal);
}

#[test]
#[should_panic(expected = "binds different entry arguments than the CFS declares")]
fn verify_step_rejects_binding_a_subset_of_the_declared_entry_arguments() {
    // Every individual commitment here is authorized — the manifest declares
    // both — so only the CFS can say that dropping `seed` from the binding
    // makes it a different program than the one being proven.
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let cfs_cursor = entrypoint_cfs(vec!["personal_data".to_string(), "seed".to_string()]);

    let subset = vec!["personal_data".to_string()];
    let program_start = program_start_step(subset.clone(), combined_root(&subset, &journal));
    let record = program_start_record(program_start.clone());

    verify_step(&cfs_cursor, &record, &program_start, &journal);
}

#[test]
#[should_panic(expected = "binds different entry arguments than the CFS declares")]
fn verify_step_rejects_reordered_entry_arguments() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let cfs_cursor = entrypoint_cfs(vec!["personal_data".to_string(), "seed".to_string()]);

    let swapped = vec!["seed".to_string(), "personal_data".to_string()];
    let program_start = program_start_step(swapped.clone(), combined_root(&swapped, &journal));
    let record = program_start_record(program_start.clone());

    verify_step(&cfs_cursor, &record, &program_start, &journal);
}

#[test]
#[should_panic(expected = "binds entry arguments the CFS does not declare")]
fn verify_step_rejects_program_start_binding_when_cfs_declares_none() {
    let journal = two_arg_authorization_journal(&sha(b"a"), &sha(b"b"));
    let cfs_cursor = no_entrypoint_cfs();
    let names = vec!["personal_data".to_string()];
    let program_start = program_start_step(names.clone(), combined_root(&names, &journal));
    let record = program_start_record(program_start.clone());

    verify_step(&cfs_cursor, &record, &program_start, &journal);
}

#[test]
fn genesis_authorization_is_not_required_when_cfs_declares_no_entry_arguments() {
    let cfs_cursor = no_entrypoint_cfs();
    let journal = two_arg_authorization_journal(&sha(b"a"), &sha(b"b"));
    let first_step = program_start_record(program_start_step(Vec::new(), Vec::new()));

    assert_eq!(
        verify_genesis_authorization(&cfs_cursor, &EMPTY_LEAF, &[], &journal, None, &first_step),
        EntrypointAuthorization::NotRequired,
    );
}

#[test]
#[should_panic(expected = "membership witness must not be provided")]
fn genesis_authorization_rejects_unnecessary_witness_when_no_entry_arguments_declared() {
    let cfs_cursor = no_entrypoint_cfs();
    let journal = two_arg_authorization_journal(&sha(b"a"), &sha(b"b"));
    let entry = StorageEntry {
        coordinates: CfsCoordinates(vec![]),
        object_commitment: sha(b"unused"),
    };
    let witness = build_read_witness(&[entry.clone()], &entry);
    let first_step = program_start_record(program_start_step(Vec::new(), Vec::new()));

    verify_genesis_authorization(
        &cfs_cursor,
        &EMPTY_LEAF,
        &[],
        &journal,
        Some(&witness),
        &first_step,
    );
}

#[test]
fn genesis_authorization_accepts_valid_trace_inclusion_witness() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let cfs_cursor = entrypoint_cfs(names.clone());
    let expected_root = combined_root(&names, &journal);

    let entry = StorageEntry {
        coordinates: CfsCoordinates(vec![]),
        object_commitment: expected_root,
    };
    let (_frontier, root, _index, index_root) = build_storage_context(&[entry.clone()]);
    let witness = build_read_witness(&[entry.clone()], &entry);
    // Unused when a witness is supplied: the window opened after the start.
    let first_step = program_start_record(program_start_step(names.clone(), Vec::new()));

    assert_eq!(
        verify_genesis_authorization(
            &cfs_cursor,
            &root,
            &index_root,
            &journal,
            Some(&witness),
            &first_step,
        ),
        EntrypointAuthorization::Established,
    );
}

#[test]
fn genesis_authorization_is_established_at_genesis_when_first_step_is_program_start() {
    // A window whose trace starts at the beginning has an empty initial
    // store, so no membership witness can exist. That is fine: its first step
    // is the `ProgramStart` that binds and authorizes the entry arguments in
    // the same guest run, so authorization is established immediately.
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let journal = two_arg_authorization_journal(&sha(b"personal_data-file"), &sha(b"seed-file"));
    let cfs_cursor = entrypoint_cfs(names.clone());
    let first_step = program_start_record(program_start_step(
        names.clone(),
        combined_root(&names, &journal),
    ));

    assert_eq!(
        verify_genesis_authorization(&cfs_cursor, &EMPTY_LEAF, &[], &journal, None, &first_step),
        EntrypointAuthorization::Established,
    );
}

#[test]
#[should_panic(expected = "first step is not ProgramStart")]
fn genesis_authorization_rejects_a_late_window_missing_its_membership_witness() {
    // A window that opens *after* the start (its first step is not
    // ProgramStart) must supply a membership witness; without one there is
    // nothing tying its storage to the manifest.
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let journal = two_arg_authorization_journal(&sha(b"personal_data-file"), &sha(b"seed-file"));
    let cfs_cursor = entrypoint_cfs(names);
    let first_step = StepRecord {
        exec_index: 9,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        kind: StepKind::SequenceEnd {
            output_commitment: Vec::new(),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };

    verify_genesis_authorization(&cfs_cursor, &EMPTY_LEAF, &[], &journal, None, &first_step);
}

#[test]
#[should_panic(expected = "Storage read witness commitment does not match requested commitment")]
fn genesis_authorization_rejects_forged_entry_object_commitment() {
    let commitment_a = sha(b"personal_data-file");
    let commitment_b = sha(b"seed-file");
    let names = vec!["personal_data".to_string(), "seed".to_string()];
    let journal = two_arg_authorization_journal(&commitment_a, &commitment_b);
    let cfs_cursor = entrypoint_cfs(names);

    // Forge the entry-argument object at coordinates [] with a commitment that
    // is not the journal-authorized combined root. The genesis check reads []
    // and compares against `combined_root`, so it rejects on commitment
    // mismatch.
    let forged_entry = StorageEntry {
        coordinates: CfsCoordinates(vec![]),
        object_commitment: sha(b"forged-combined-root"),
    };
    let (_frontier, root, _index, index_root) = build_storage_context(&[forged_entry.clone()]);
    let witness = build_read_witness(&[forged_entry.clone()], &forged_entry);

    // A membership witness is supplied, so the witness path is taken and
    // `first_step` is not inspected; a placeholder suffices.
    let first_step = StepRecord {
        exec_index: 0,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::SequenceEnd {
            output_commitment: Vec::new(),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
    };

    verify_genesis_authorization(
        &cfs_cursor,
        &root,
        &index_root,
        &journal,
        Some(&witness),
        &first_step,
    );
}

// ============================================================================
// Fingerprint slice binding (`assert_window_is_commitment_slice`)
// ============================================================================

mod fingerprint_slice {
    use super::*;
    use raster_core::fingerprint::{BitPacker, Fingerprint};
    use raster_core::transition::InitTransition;
    use raster_core::transition::{
        FingerprintBlockWitness, FingerprintSliceWitness, TraceCommitmentHeader,
    };

    use crate::fraud_proof::assert_window_is_commitment_slice;

    /// Mirror of the prover's fingerprint-block tree: leaf `i` =
    /// `sha256(bits[i].to_le_bytes())`, combined by the shared trace-tree
    /// convention. Rebuilt here so the guest-side check is exercised against
    /// an independently constructed witness.
    fn build_header_and_witness(
        bits: &[u64],
        bits_per_item: usize,
        fingerprint_len: usize,
        window_start: usize,
        window_len: usize,
    ) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
        let first_block = (window_start * bits_per_item) / 64;
        let last_block = ((window_start + window_len) * bits_per_item - 1) / 64;

        let mut tree = TraceBridgeTree::new(1);
        let mut marked = Vec::new();
        for (index, block) in bits.iter().enumerate() {
            tree.append(Bytes(sha256_bytes(&block.to_le_bytes())));
            if (first_block..=last_block).contains(&index) {
                marked.push(tree.mark().expect("mark fingerprint block"));
            }
        }
        let fingerprint_root = tree.root(0).expect("fingerprint root").0;

        let blocks = (first_block..=last_block)
            .zip(marked)
            .map(|(index, position)| {
                let path = tree.witness(position, 0).expect("block witness");
                FingerprintBlockWitness {
                    block: bits[index],
                    position: u64::from(position),
                    path_elems: path.iter().map(|elem| elem.0.clone()).collect(),
                }
            })
            .collect();

        let header = TraceCommitmentHeader {
            bits_packer: BitPacker::new(bits_per_item),
            fingerprint_len: fingerprint_len as u64,
            fingerprint_root,
            revealed_items_commitment: vec![9; 32],
        };
        (header, FingerprintSliceWitness { blocks })
    }

    fn init_transition_with_window(window_start: usize, window: Fingerprint) -> InitTransition {
        InitTransition {
            // Only the frontier's position matters to the slice check: it is
            // what fixes the window's offset in the committed fingerprint.
            init_frontier: SerializableFrontier {
                position: window_start as u64,
                leaf: EMPTY_LEAF.to_vec(),
                ommers: Vec::new(),
            },
            init_storage_frontier: SerializableFrontier {
                position: 0,
                leaf: EMPTY_LEAF.to_vec(),
                ommers: Vec::new(),
            },
            init_storage_root: Vec::new(),
            init_storage_index_root: Vec::new(),
            active_drafts: BTreeMap::new(),
            fingerprint: window,
        }
    }

    /// 40 items at 4 bits each = 160 bits = 3 packed blocks, with per-item
    /// values `i & 0xf` so every offset mistake changes some value.
    fn fixture_bits(bits_per_item: usize, items: usize) -> Vec<u64> {
        let packer = BitPacker::new(bits_per_item);
        let hashes: Vec<Vec<u8>> = (0..items).map(|i| vec![i as u8; 32]).collect();
        packer.pack(&hashes)
    }

    #[test]
    fn accepts_a_window_slice_crossing_a_block_boundary() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        // Items [14, 18) span bits [56, 72) — blocks 0 and 1.
        let (window_start, window_len) = (14, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) =
            build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
        );
    }

    #[test]
    #[should_panic(expected = "diverges from the committed fingerprint slice")]
    fn rejects_a_window_fingerprint_not_in_the_commitment() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (14, 4);

        let packer = BitPacker::new(bits_per_item);
        let mut window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        // A challenger-fabricated "committed" fingerprint: one flipped value.
        window_bits[0] ^= 0b1;
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) =
            build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
        );
    }

    #[test]
    #[should_panic(expected = "diverges from the committed fingerprint slice")]
    fn rejects_a_genuine_slice_claimed_at_the_wrong_offset() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (14, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        // The frontier (and thus the derived offset) says one item later:
        // the same committed blocks yield different values there.
        let (header, witness) =
            build_header_and_witness(&bits, bits_per_item, items, window_start + 1, window_len);
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start + 1, window),
            &header,
            &witness,
        );
    }

    #[test]
    #[should_panic(expected = "block inclusion proof is invalid")]
    fn rejects_a_tampered_block_value() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (14, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, mut witness) =
            build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
        witness.blocks[0].block ^= 0b1;
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
        );
    }

    #[test]
    #[should_panic(expected = "Window range exceeds the committed fingerprint")]
    fn rejects_a_window_past_the_committed_fingerprint() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (38, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer.get_range(36, 40, &bits).expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness(&bits, bits_per_item, items, 36, 4);
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
        );
    }

    /// Build a valid window at `[window_start, window_start + window_len)` of
    /// a `items`-long commitment and report whether the check calls it
    /// terminal.
    fn terminality_of(window_start: usize, window_len: usize) -> bool {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) =
            build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
        )
    }

    /// A window ending exactly at the committed fingerprint's end is terminal:
    /// a `ProgramEnd` inside it is the commitment's last item.
    #[test]
    fn a_window_ending_at_the_commitment_end_is_terminal() {
        // Items [36, 40) of a 40-item commitment.
        assert!(terminality_of(36, 4));
    }

    /// The case the flag exists for. This window is a perfectly valid slice —
    /// the check passes — but it sits mid-commitment, so a `ProgramEnd` found
    /// inside it would *not* be the trace's result.
    #[test]
    fn a_valid_mid_commitment_window_is_not_terminal() {
        assert!(!terminality_of(14, 4));
    }

    /// Off by exactly one item. Terminality is an equality, not a bound.
    #[test]
    fn a_window_one_item_short_of_the_end_is_not_terminal() {
        assert!(!terminality_of(35, 4));
    }
}

// ============================================================================
// Program output binding (`verify_program_end`)
// ============================================================================

mod program_end {
    use super::*;
    use raster_core::input::SelectionCommitment;
    use raster_core::trace::ProgramEndStep;
    use raster_core::transition::OutputAuthorization;

    use crate::checks::entrypoint::verify_program_end;

    /// `main` returns a value, so a `ProgramEnd` owes an output binding.
    fn producing_cfs() -> CfsCursor {
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".into(),
            project: "test".into(),
            encoding: "postcard".into(),
            tiles: vec![],
            sequences: vec![SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![],
                entry_arguments: vec![],
                produces_output: true,
            }],
        })
    }

    fn program_end_record(program_end: ProgramEndStep) -> StepRecord {
        StepRecord {
            exec_index: 9,
            sequence_id: "main".to_string(),
            // `entrypoint_coordinates()` — the sequence root.
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::ProgramEnd(program_end),
            recur_progress_commitment: RecurProgressStack::new().commitment(),
        }
    }

    /// A program output living at the sequence root, selected whole.
    ///
    /// `selected_len == 0` keeps the selection *witness* out of the picture —
    /// `verify_program_end` only verifies one for a non-empty selection, and
    /// the selection machinery is covered elsewhere. What is under test here
    /// is the binding between the step's `output_commitment` and what the
    /// function returns.
    fn fixture(
        object_commitment: Vec<u8>,
        selected_hash: Vec<u8>,
        declared_output_commitment: Vec<u8>,
    ) -> (StorageEntry, Vec<u8>, Vec<u8>, StorageReadWitness, StepRecord) {
        let entry = StorageEntry {
            coordinates: CfsCoordinates(vec![]),
            object_commitment: object_commitment.clone(),
        };
        let (_frontier, root, _index, index_root) = build_storage_context(&[entry.clone()]);
        let witness = build_read_witness(&[entry.clone()], &entry);

        let source_root_hash: [u8; 32] = object_commitment
            .clone()
            .try_into()
            .expect("test commitments are 32 bytes");
        let selected: [u8; 32] = selected_hash
            .try_into()
            .expect("test commitments are 32 bytes");

        let record = program_end_record(ProgramEndStep {
            output: Some(StorageData {
                coordinates: CfsCoordinates(vec![]),
                commitment: object_commitment,
                selector: Default::default(),
                selection: SelectionCommitment {
                    source_root_hash,
                    selected_hash: selected,
                    selected_len: 0,
                    ..Default::default()
                },
            }),
            output_commitment: declared_output_commitment,
            storage: dummy_storage_roots(),
        });

        (entry, root, index_root, witness, record)
    }

    /// The phase-1 property: the value the check already verified is the value
    /// it hands back, so the journal can name *which* output this trace
    /// produced rather than only that one exists.
    #[test]
    fn establishes_with_the_committed_output_value() {
        let object_commitment = sha(b"program-output-object");
        let selected_hash = sha(b"program-output-value");
        let (_entry, root, index_root, witness, record) = fixture(
            object_commitment,
            selected_hash.clone(),
            selected_hash.clone(),
        );
        let StepKind::ProgramEnd(program_end) = record.kind.clone() else {
            unreachable!("fixture builds a ProgramEnd step");
        };

        assert_eq!(
            verify_program_end(
                &producing_cfs(),
                &record,
                &program_end,
                &root,
                &index_root,
                Some(&witness),
                None,
            ),
            OutputAuthorization::Established {
                output_commitment: selected_hash,
            },
        );
    }

    /// The invariant the carried value rests on. Without this, `Established`
    /// could name a value the selection never produced.
    #[test]
    #[should_panic(expected = "ProgramEnd output commitment does not match the selected output")]
    fn rejects_an_output_commitment_that_is_not_the_selected_hash() {
        let (_entry, root, index_root, witness, record) = fixture(
            sha(b"program-output-object"),
            sha(b"program-output-value"),
            sha(b"a-different-value"),
        );
        let StepKind::ProgramEnd(program_end) = record.kind.clone() else {
            unreachable!("fixture builds a ProgramEnd step");
        };

        verify_program_end(
            &producing_cfs(),
            &record,
            &program_end,
            &root,
            &index_root,
            Some(&witness),
            None,
        );
    }

    /// A unit `main` binds nothing and carries no value — the variant stays
    /// payload-free, so nothing downstream can read an output out of it.
    #[test]
    fn unit_output_is_not_required() {
        let record = program_end_record(ProgramEndStep {
            output: None,
            output_commitment: Vec::new(),
            storage: dummy_storage_roots(),
        });
        let StepKind::ProgramEnd(program_end) = record.kind.clone() else {
            unreachable!("fixture builds a ProgramEnd step");
        };

        assert_eq!(
            verify_program_end(
                &no_entrypoint_cfs(),
                &record,
                &program_end,
                &EMPTY_LEAF.to_vec(),
                &Vec::new(),
                None,
                None,
            ),
            OutputAuthorization::NotRequired,
        );
    }
}
