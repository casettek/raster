use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource, SequenceChildItem,
    SequenceDef, SequenceItem, TileDef, TileItem,
};
use raster_core::coordinate_index::{
    coordinate_index_membership_proof, coordinate_index_non_membership_proof, coordinate_index_root,
};
use raster_core::draft::{
    draft_root_from_witness, schema_hash as compute_schema_hash, DraftFieldValue, DraftOp,
    DraftReplayTransition, DraftStateWitness, DraftTransitionWitness, TileReplayJournal,
    TrackedDraftState,
};
use raster_core::input::{SchemaField, SchemaFieldMode, SchemaNode, Selectable};
use raster_core::trace::{
    ExternalData, ExternalInput, FnInput, FnInputArg, FnInputValue, InternalData,
    SequenceEndRecord, SequenceStartRecord, StepRecord, TileExecRecord,
};
use raster_core::transition::{
    InternalStoreEntry, InternalStoreLogWitness, InternalStoreReadWitness, InternalStoreWitness,
    InternalStoreWriteWitness, SerializableFrontier,
};

use crate::checks::cfs::verify_step_record_inputs;
use crate::checks::drafts::verify_draft_transition;
use crate::checks::io::{
    external_input_commitment, input_source_commitment, verify_external_inputs, verify_io_witness,
};
use crate::checks::store::{internal_store_leaf_hash, verify_internal_store_transition};
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
    StepRecord::TileExec(TileExecRecord {
        exec_index,
        tile_id: "collect_lines".into(),
        sequence_id: "main".into(),
        intra_sequence_index: exec_index as u32,
        coordinates: CfsCoordinates(vec![exec_index as u32]),
        input_commitment: vec![exec_index as u8; 32],
        input_source_commitment: vec![0; 32],
        output_commitment: vec![1; 32],
        external_input_commitment: Vec::new(),
        internal_store_root_before: EMPTY_LEAF.to_vec(),
        internal_store_root_after: EMPTY_LEAF.to_vec(),
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    })
}

fn external_input(binding_name: &str, commitment: &[u8], selected_bytes: &[u8]) -> ExternalInput {
    [(
        "arg".to_string(),
        ExternalData {
            name: binding_name.to_string(),
            commitment: commitment.to_vec(),
            tree_root: Vec::new(),
            selector: Default::default(),
            selection: raster_core::input::SelectionCommitment {
                selected_hash: raster_core::input::selection_payload_hash(selected_bytes),
                selected_len: selected_bytes.len() as u64,
                ..Default::default()
            },
        },
    )]
    .into_iter()
    .collect()
}

fn authorization_journal(binding_name: &str, commitment: &[u8]) -> AuthorizationJournal {
    AuthorizationJournal {
        external_inputs_commitments: [(binding_name.to_string(), commitment.to_vec())]
            .into_iter()
            .collect(),
        manifest_commitment: vec![7; 32],
    }
}

fn internal_input_witness(coordinates: CfsCoordinates, commitment: Vec<u8>) -> FnInput {
    FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::InternalBinding],
        args: vec![FnInputArg {
            name: "arg".to_string(),
            ty: "Vec<u8>".to_string(),
        }],
        external: ExternalInput::new(),
        internal: [(
            "arg".to_string(),
            InternalData {
                coordinates,
                commitment,
                selector: Default::default(),
                selection: Default::default(),
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
            },
            SequenceDef {
                id: "sub".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "producer".into(),
                    sources: vec![InputBinding::Direct(InputSource::Inline)],
                })],
            },
        ],
    })
}

#[test]
fn verify_tile_commitments_accept_matching_recorded_io() {
    let ext = ExternalInput::new();
    let step = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        intra_sequence_index: 0,
        input_commitment: sha(b"in"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
        output_commitment: sha(b"out"),
        internal_store_root_before: vec![0; 32],
        internal_store_root_after: vec![0; 32],
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });

    verify_io_witness(&step, Some(&b"in".to_vec()), Some(&b"out".to_vec()));
    verify_external_inputs(
        &step,
        &ext,
        &BTreeMap::new(),
        &AuthorizationJournal {
            external_inputs_commitments: BTreeMap::new(),
            manifest_commitment: vec![0; 32],
        }
        .external_inputs_commitments,
    );
}

#[test]
fn verify_step_record_inputs_accepts_sequence_descendant_producer_coordinates() {
    let cfs_cursor = producer_sequence_cfs();
    let step_record = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "consumer".into(),
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![1]),
        intra_sequence_index: 1,
        input_commitment: Vec::new(),
        input_source_commitment: Vec::new(),
        output_commitment: Vec::new(),
        external_input_commitment: Vec::new(),
        internal_store_root_before: Vec::new(),
        internal_store_root_after: Vec::new(),
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });
    let input_source_witness =
        internal_input_witness(CfsCoordinates(vec![0, 0]), sha(b"producer-output"));

    verify_step_record_inputs(&cfs_cursor, &step_record, Some(&input_source_witness), None);
}

#[test]
#[should_panic(expected = "Tile input commitment does not match recorded input bytes")]
fn verify_tile_commitments_reject_mismatched_input() {
    let ext = ExternalInput::new();
    let step = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        intra_sequence_index: 0,
        input_commitment: sha(b"expected"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
        output_commitment: sha(b"out"),
        internal_store_root_before: vec![0; 32],
        internal_store_root_after: vec![0; 32],
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });

    verify_io_witness(&step, Some(&b"actual".to_vec()), Some(&b"out".to_vec()));
}

#[test]
fn verify_sequence_boundary_commitments_accept_matching_recorded_io() {
    let ext = ExternalInput::new();
    let start = StepRecord::SequenceStart(SequenceStartRecord {
        exec_index: 1,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        input_commitment: sha(b"sequence-in"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
    });
    let end = StepRecord::SequenceEnd(SequenceEndRecord {
        exec_index: 2,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        output_commitment: sha(b"sequence-out"),
    });

    verify_io_witness(&start, Some(&b"sequence-in".to_vec()), None);
    verify_io_witness(&end, None, Some(&b"sequence-out".to_vec()));
    verify_external_inputs(
        &start,
        &ext,
        &BTreeMap::new(),
        &AuthorizationJournal {
            external_inputs_commitments: BTreeMap::new(),
            manifest_commitment: vec![0; 32],
        }
        .external_inputs_commitments,
    );
    verify_external_inputs(
        &end,
        &ExternalInput::new(),
        &BTreeMap::new(),
        &AuthorizationJournal {
            external_inputs_commitments: BTreeMap::new(),
            manifest_commitment: vec![0; 32],
        }
        .external_inputs_commitments,
    );
}

#[test]
fn verify_external_inputs_accept_matching_authorized_binding() {
    let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
    let step = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        intra_sequence_index: 0,
        input_commitment: sha(b"in"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
        output_commitment: sha(b"out"),
        internal_store_root_before: vec![0; 32],
        internal_store_root_after: vec![0; 32],
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });

    let authorization = authorization_journal("personal_data", sha256_hex(b"payload").as_slice());
    verify_external_inputs(
        &step,
        &ext,
        &BTreeMap::new(),
        &authorization.external_inputs_commitments,
    );
}

#[test]
#[should_panic(expected = "Missing authorized commitment for external input 'personal_data'")]
fn verify_external_inputs_reject_missing_authorized_binding() {
    let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
    let step = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        intra_sequence_index: 0,
        input_commitment: sha(b"in"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
        output_commitment: sha(b"out"),
        internal_store_root_before: vec![0; 32],
        internal_store_root_after: vec![0; 32],
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });

    verify_external_inputs(
        &step,
        &ext,
        &BTreeMap::new(),
        &AuthorizationJournal {
            external_inputs_commitments: BTreeMap::new(),
            manifest_commitment: vec![0; 32],
        }
        .external_inputs_commitments,
    );
}

#[test]
#[should_panic(
    expected = "External input 'personal_data' commitment does not match authorized source"
)]
fn verify_external_inputs_reject_mismatched_authorized_binding() {
    let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
    let step = StepRecord::TileExec(TileExecRecord {
        exec_index: 1,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![0]),
        intra_sequence_index: 0,
        input_commitment: sha(b"in"),
        input_source_commitment: Vec::new(),
        external_input_commitment: external_input_commitment(&ext),
        output_commitment: sha(b"out"),
        internal_store_root_before: vec![0; 32],
        internal_store_root_after: vec![0; 32],
        internal_store_index_root_before: Vec::new(),
        internal_store_index_root_after: Vec::new(),
    });

    let authorization = authorization_journal("personal_data", b"wrong");
    verify_external_inputs(
        &step,
        &ext,
        &BTreeMap::new(),
        &authorization.external_inputs_commitments,
    );
}

fn empty_internal_store_frontier_for_test() -> NonEmptyFrontier<Bytes> {
    deserialize_frontier(&SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    })
    .expect("empty internal store frontier should deserialize")
}

fn build_internal_store_context(
    entries: &[InternalStoreEntry],
) -> (
    NonEmptyFrontier<Bytes>,
    Vec<u8>,
    BTreeMap<CfsCoordinates, raster_core::transition::InternalStoreIndexValue>,
    Vec<u8>,
) {
    let mut frontier = empty_internal_store_frontier_for_test();
    let mut index = BTreeMap::new();
    for entry in entries {
        frontier.append(Bytes(internal_store_leaf_hash(entry)));
        let log_position: u64 = frontier.position().into();
        index.insert(
            entry.coordinates.clone(),
            raster_core::transition::InternalStoreIndexValue {
                log_position,
                object_commitment: entry.object_commitment.clone(),
            },
        );
    }
    let root = frontier_root(&frontier);
    let index_root = coordinate_index_root(&index);
    (frontier, root, index, index_root)
}

fn build_internal_store_log_witness_for_entries(
    entries: &[InternalStoreEntry],
    log_position: u64,
) -> InternalStoreLogWitness {
    let mut tree = TraceBridgeTree::new(1);
    tree.append(Bytes(EMPTY_LEAF.to_vec()));
    let mut marked_position = None;
    for (index, entry) in entries.iter().enumerate() {
        tree.append(Bytes(internal_store_leaf_hash(entry)));
        if u64::try_from(index).expect("index overflow") + 1 == log_position {
            marked_position = tree.mark();
        }
    }
    let marked_position = marked_position.expect("log position should exist in append tree");
    let auth_path = tree
        .witness(marked_position, 0)
        .expect("append-log witness should exist");
    InternalStoreLogWitness {
        position: u64::from(marked_position),
        path_elems: auth_path.iter().map(|elem| elem.0.clone()).collect(),
    }
}

fn build_read_witness(
    entries: &[InternalStoreEntry],
    entry: &InternalStoreEntry,
) -> InternalStoreReadWitness {
    let (_frontier, _root, index, _index_root) = build_internal_store_context(entries);
    let index_witness = coordinate_index_membership_proof(&index, &entry.coordinates)
        .expect("membership proof should exist");
    let log_witness =
        build_internal_store_log_witness_for_entries(entries, index_witness.value.log_position);
    InternalStoreReadWitness {
        entry: entry.clone(),
        log_witness,
        index_witness,
    }
}

fn build_write_witness(
    before_entries: &[InternalStoreEntry],
    new_entry: &InternalStoreEntry,
) -> InternalStoreWriteWitness {
    let (_frontier, _root, before_index, _before_index_root) =
        build_internal_store_context(before_entries);
    let mut after_entries = before_entries.to_vec();
    after_entries.push(new_entry.clone());
    let (_frontier, _root, after_index, _after_index_root) =
        build_internal_store_context(&after_entries);
    InternalStoreWriteWitness {
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
    StepRecord::TileExec(TileExecRecord {
        exec_index,
        tile_id: "tile".to_string(),
        sequence_id: "main".to_string(),
        coordinates,
        intra_sequence_index: 0,
        input_commitment: Vec::new(),
        input_source_commitment,
        external_input_commitment: Vec::new(),
        output_commitment,
        internal_store_root_before: root_before,
        internal_store_root_after: root_after,
        internal_store_index_root_before: index_root_before,
        internal_store_index_root_after: index_root_after,
    })
}

#[test]
fn verify_internal_store_transition_uses_output_commitment_as_keyed_entry() {
    let output_commitment = sha(b"out");
    let new_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: output_commitment.clone(),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_internal_store_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_internal_store_context(&[new_entry.clone()]);
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
    let witness = InternalStoreWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let (_next_frontier, next_root, next_index_root) = verify_internal_store_transition(
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
fn verify_internal_store_transition_rejects_duplicate_coordinates() {
    let existing_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"existing"),
    };
    let (mut before_frontier, root_before, before_index, index_root_before) =
        build_internal_store_context(&[existing_entry.clone()]);
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
    let witness = InternalStoreWitness {
        reads: Vec::new(),
        write: Some(InternalStoreWriteWitness {
            entry: InternalStoreEntry {
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

    let _ = verify_internal_store_transition(
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
#[should_panic(
    expected = "Missing internal store read witness for coordinates CfsCoordinates([0])"
)]
fn verify_internal_store_transition_rejects_wrong_coordinates_with_correct_bytes() {
    let prior_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![9]),
        object_commitment: sha(b"shared"),
    };
    let new_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"out"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_internal_store_context(&[prior_entry.clone()]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_internal_store_context(&[prior_entry.clone(), new_entry.clone()]);
    let input_source_witness = internal_input_witness(
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
    let witness = InternalStoreWitness {
        reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
        write: Some(build_write_witness(&[prior_entry], &new_entry)),
    };

    let _ = verify_internal_store_transition(
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
#[should_panic(
    expected = "TileExec internal store root before does not match current internal store root"
)]
fn verify_internal_store_transition_rejects_stale_root() {
    let new_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"out"),
    };
    let (_before_frontier, root_before, _before_index, index_root_before) =
        build_internal_store_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_internal_store_context(&[new_entry.clone()]);
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
    let stale_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![99]),
        object_commitment: sha(b"stale"),
    };
    let (mut stale_frontier, _stale_root, _stale_index, _stale_index_root) =
        build_internal_store_context(&[stale_entry]);
    let witness = InternalStoreWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let _ = verify_internal_store_transition(
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
    expected = "TileExec internal store index root before does not match current index root"
)]
fn verify_internal_store_transition_rejects_stale_index_root() {
    let new_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"out"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_internal_store_context(&[]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_internal_store_context(&[new_entry.clone()]);
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
    let witness = InternalStoreWitness {
        reads: Vec::new(),
        write: Some(build_write_witness(&[], &new_entry)),
    };

    let _ = verify_internal_store_transition(
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
fn verify_internal_store_transition_accepts_non_empty_initial_state() {
    let prior_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![0]),
        object_commitment: sha(b"prior"),
    };
    let new_entry = InternalStoreEntry {
        coordinates: CfsCoordinates(vec![1]),
        object_commitment: sha(b"next"),
    };
    let (mut before_frontier, root_before, _before_index, index_root_before) =
        build_internal_store_context(&[prior_entry.clone()]);
    let (_after_frontier, root_after, _after_index, index_root_after) =
        build_internal_store_context(&[prior_entry.clone(), new_entry.clone()]);
    let input_source_witness = internal_input_witness(
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
    let witness = InternalStoreWitness {
        reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
        write: Some(build_write_witness(&[prior_entry], &new_entry)),
    };

    let (_next_frontier, next_root, next_index_root) = verify_internal_store_transition(
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

#[test]
fn verify_draft_transition_tracks_multi_step_chain() {
    let empty_witness = DraftStateWitness {
        schema: DemoDraft::schema(),
        fields: Vec::new(),
    };
    let empty_root =
        draft_root_from_witness(&empty_witness.schema, &BTreeMap::new(), false).unwrap();
    let schema_hash = compute_schema_hash(&empty_witness.schema);
    let draft_id = [7; 32];
    let mut active_drafts = BTreeMap::new();

    let step_one = TileReplayJournal {
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
                DraftFieldValue::Set(raster_core::draft::DraftValue::String("collected".into())),
            ),
            (
                "items".into(),
                DraftFieldValue::Append(vec![raster_core::draft::DraftValue::String(
                    "first".into(),
                )]),
            ),
        ],
    };
    let step_two = TileReplayJournal {
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
    let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new(), false).unwrap();
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
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id,
                schema_hash,
                root_before: empty_root,
                ops: Vec::new(),
            }),
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
    let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new(), false).unwrap();
    let mut active_drafts = BTreeMap::new();

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id: [3; 32],
                schema_hash: [4; 32],
                root_before: empty_root,
                ops: Vec::new(),
            }),
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
        draft_root_from_witness(&DemoDraft::schema(), &BTreeMap::new(), false).unwrap();
    let mut active_drafts = BTreeMap::new();

    verify_draft_transition(
        &draft_tile_step(1),
        Some(&TileReplayJournal {
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id: [6; 32],
                schema_hash: compute_schema_hash(&DemoDraft::schema()),
                root_before: empty_root,
                ops: Vec::new(),
            }),
        }),
        Some(&DraftTransitionWitness {
            pre_state: DraftStateWitness {
                schema: DemoDraft::schema(),
                fields: vec![(
                    "title".into(),
                    DraftFieldValue::Set(raster_core::draft::DraftValue::String("tampered".into())),
                )],
            },
            native_transition: None,
        }),
        &mut active_drafts,
    );
}
