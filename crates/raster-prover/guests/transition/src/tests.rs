use std::collections::{BTreeMap, HashMap};

use bridgetree::NonEmptyFrontier;

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource, RecurSequenceItem,
    RecurTileItem, SequenceChildItem, SequenceDef, SequenceItem, TileDef, TileItem,
};
use raster_core::coordinate_index::{
    coordinate_index_membership_proof, coordinate_index_non_membership_proof, coordinate_index_root,
};
use raster_core::draft::{
    draft_root_from_witness, draft_value_root, schema_hash as compute_schema_hash, DraftOp,
    DraftReplayTransition, DraftStateWitness, DraftTransitionWitness, DraftWitnessField,
    RecurControlKind,
    RecurPosition, RecurStateTransition, RecurTileReplay, TileReplayJournal, TrackedDraftState,
};
use raster_core::input::{
    AppendFrontier, SchemaField, SchemaFieldMode, SchemaNode, Selectable,
};
use raster_core::input::Hash32;
use raster_core::recur_progress::{
    RecurProgressStack, RecurProgressViolation, RecurSiteKind,
};
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, FnInputArg, FnInputValue, ProgramEndStep, StepKind, StepRecord,
    StorageData, StorageRoots,
};
use raster_core::transition::{
    SerializableFrontier, StorageEntry, StorageLogWitness, StorageReadWitness, StorageWitness,
    StorageWriteWitness,
};

use crate::checks::cfs::{
    verify_exec_index, verify_sequence_id, verify_sequence_scope_parent,
    verify_step_record_inputs,
};
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
        recur_state: None,
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

/// `sub`'s item reads `sub`'s own parameter 0 — a `SequenceScope` binding at a
/// *nested* frame, which is the shape the compiler actually emits. (`main`'s
/// parameters resolve to `EntryArgument`; a scope binding at the root frame is
/// unreachable.)
fn scope_binding_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("consumer", 1, 1)],
        sequences: vec![
            SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Sequence(SequenceItem {
                    id: "sub".into(),
                    sources: vec![InputBinding::Direct(InputSource::Inline)],
                })],
                entry_arguments: vec![],
                produces_output: false,
            },
            SequenceDef {
                id: "sub".into(),
                input_sources: vec![InputBinding::Direct(InputSource::Inline)],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "consumer".into(),
                    sources: vec![InputBinding::seq_input(0)],
                })],
                entry_arguments: vec![],
                produces_output: false,
            },
        ],
    })
}

/// Build the trace tree over `prefix` — seed at leaf 0, item `n` at `n + 1` —
/// and return its root with a `StepRecordWitness` for `index`.
fn trace_root_and_witness(
    prefix: &[StepRecord],
    index: usize,
) -> (Vec<u8>, raster_core::transition::StepRecordWitness) {
    let mut tree = TraceBridgeTree::new(1);
    tree.append(Bytes(EMPTY_LEAF.to_vec()));
    let mut marked = None;
    for (i, record) in prefix.iter().enumerate() {
        tree.append(Bytes(crate::merkle_tree::hash_trace_item(record)));
        if i == index {
            marked = tree.mark();
        }
    }
    let position = marked.expect("marked position");
    let root = tree.root(0).expect("trace root").0;
    let path = tree.witness(position, 0).expect("trace witness");
    (
        root,
        raster_core::transition::StepRecordWitness {
            position: u64::from(position),
            path_elems: path.iter().map(|elem| elem.0.clone()).collect(),
        },
    )
}

/// The step at `[0, 0]` reading `sub`'s parameter 0, its own source witness,
/// and the parent `SequenceStart` at `[0]` carrying `parent_args`.
fn scope_binding_scenario(
    read_from: CfsCoordinates,
    commitment: Vec<u8>,
) -> (StepRecord, FnInput, StepRecord, FnInput) {
    let step_record = StepRecord {
        exec_index: 2,
        sequence_id: "sub".into(),
        coordinates: CfsCoordinates(vec![0, 0]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("consumer".into()),
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
        recur_state: None,
    };
    let step_source = storage_input_witness(read_from.clone(), commitment.clone());
    let parent_args = storage_input_witness(read_from, commitment);
    let parent_record = StepRecord {
        exec_index: 1,
        sequence_id: "sub".into(),
        coordinates: CfsCoordinates(vec![0]),
        kind: StepKind::SequenceStart {
            input_commitment: Vec::new(),
            // The record commits its own argument list; that commitment is
            // fingerprinted, which is what makes it an anchor.
            input_source_commitment: input_source_commitment(&parent_args),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    };
    (step_record, step_source, parent_record, parent_args)
}

#[test]
fn sequence_scope_parent_binding_accepts_the_real_parent() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![9, 9]), sha(b"the-caller-passed-this"));

    let (trace_root, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let mut witnesses = HashMap::new();
    witnesses.insert(
        parent_record,
        postcard::to_allocvec(&witness).expect("witness serializes"),
    );

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        &witnesses,
        &trace_root,
    );
}

/// The forgery the check exists to stop: a parent `FnInput` invented to agree
/// with the step, paired with the real parent record. It is refused because the
/// record commits its own argument list and the invention does not match it.
#[test]
#[should_panic(expected = "not the parent SequenceStart's recorded input source")]
fn sequence_scope_parent_binding_refuses_a_fabricated_witness() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, _parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![9, 9]), sha(b"the-caller-passed-this"));

    // A different story about what the caller passed, built to agree with a
    // step that read it.
    let fabricated = storage_input_witness(CfsCoordinates(vec![4, 2]), sha(b"a-different-story"));

    let (trace_root, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let mut witnesses = HashMap::new();
    witnesses.insert(
        parent_record,
        postcard::to_allocvec(&witness).expect("witness serializes"),
    );

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&fabricated),
        &witnesses,
        &trace_root,
    );
}

/// An invented parent *record*, self-consistent with its own invented argument
/// list, is refused a step earlier: it is not in the trace.
#[test]
#[should_panic(expected = "not in the trace at the claimed position")]
fn sequence_scope_parent_binding_refuses_a_parent_not_in_the_trace() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![9, 9]), sha(b"the-caller-passed-this"));

    // The witness proves inclusion in *some* trace — just not the one this step
    // is being appended to.
    let (_, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let (unrelated_root, _) = trace_root_and_witness(
        &[step_with_sequence_id(
            boundary_start_kind(),
            vec![0],
            "sub",
        )],
        0,
    );
    let mut witnesses = HashMap::new();
    witnesses.insert(
        parent_record,
        postcard::to_allocvec(&witness).expect("witness serializes"),
    );

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        &witnesses,
        &unrelated_root,
    );
}

/// Why [`verify_sequence_scope_parent`] has to exist.
///
/// `SequenceScope { i }` claims "my input is parameter `i` of the frame I am
/// in". The guest checks it by comparing the step's own resolved source against
/// argument `i` of the **parent's** `FnInput`, supplied as
/// `sequence_scope_witness`.
///
/// The step's own witness is pinned — `verify_step_record` holds it to the
/// record's `input_source_commitment`, which is fingerprinted. The parent's is
/// not pinned to anything: it arrives from the host and no check ties it to the
/// parent `SequenceStart`'s own recorded commitment. So `assert_same_source`
/// compares a value against a value the same party chose, and passes for any
/// claim at all.
///
/// Demonstrated by inventing two mutually exclusive parents. Each says the
/// caller passed something different; each is accepted, because each was built
/// to agree with the step. Their `input_source_commitment`s differ, so a check
/// that consulted the parent record would have separated them.
///
/// Invert once the parent record's trace inclusion is verified and its
/// `input_source_commitment` compared (`input_sources_witnesses`, currently
/// shipped to the guest and read by nothing).
#[test]
fn poc_the_sequence_scope_witness_is_bound_to_nothing() {
    let cfs_cursor = scope_binding_cfs();

    let step_at = |read_from: CfsCoordinates, commitment: Vec<u8>| {
        let step_record = StepRecord {
            exec_index: 3,
            sequence_id: "sub".into(),
            coordinates: CfsCoordinates(vec![0, 0]),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("consumer".into()),
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
            recur_state: None,
        };
        (step_record, storage_input_witness(read_from, commitment))
    };

    // Two invented parents, each claiming the caller passed a different object,
    // and each paired with a step that read exactly what it claims.
    let claims = [
        (CfsCoordinates(vec![9, 9]), sha(b"one-story")),
        (CfsCoordinates(vec![4, 2]), sha(b"a-different-story")),
    ];

    let mut parent_commitments = Vec::new();
    for (read_from, commitment) in claims {
        let (step_record, step_source) = step_at(read_from.clone(), commitment.clone());
        // Invented wholesale: not derived from any parent record in any trace.
        let fabricated_parent = storage_input_witness(read_from, commitment);

        verify_step_record_inputs(
            &cfs_cursor,
            &step_record,
            Some(&step_source),
            Some(&fabricated_parent),
            None,
        );

        parent_commitments.push(input_source_commitment(&fabricated_parent));
    }

    assert_ne!(
        parent_commitments[0], parent_commitments[1],
        "the two fabricated parents must be distinguishable, or the test proves nothing"
    );
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
        recur_state: None,
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
        recur_state: None,
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

/// A step at trace index `t` carries `exec_index == t + 1`.
fn exec_index_fixture(exec_index: u64) -> StepRecord {
    StepRecord {
        exec_index,
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
        recur_state: None,
    }
}

/// A recur **tile** site at `[0]` and a recur **sequence** site at `[1]`,
/// so both frame rules are reachable from one schema.
fn recur_frames_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("recur", 1, 1), TileDef::iter("inner", 1, 1)],
        sequences: vec![
            SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![
                    SequenceChildItem::RecurTile(RecurTileItem {
                        id: "recur".into(),
                        sources: vec![InputBinding::Direct(InputSource::Inline)],
                        chunk: None,
                        leaves_output_open: false,
                        state_is_output: false,
                    }),
                    SequenceChildItem::RecurSequence(RecurSequenceItem {
                        id: "child".into(),
                        sources: vec![InputBinding::Direct(InputSource::Inline)],
                        state_is_output: false,
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
            },
            SequenceDef {
                id: "child".into(),
                input_sources: vec![InputBinding::Direct(InputSource::Inline)],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "inner".into(),
                    sources: vec![InputBinding::Direct(InputSource::Inline)],
                })],
                entry_arguments: vec![],
                produces_output: false,
            },
        ],
    })
}

fn step_with_sequence_id(
    kind: StepKind,
    coordinates: Vec<u32>,
    sequence_id: &str,
) -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: sequence_id.into(),
        coordinates: CfsCoordinates(coordinates),
        kind,
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    }
}

fn exec_kind(target: ExecTarget) -> StepKind {
    StepKind::Exec(ExecStep {
        target,
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
    })
}

fn boundary_start_kind() -> StepKind {
    StepKind::SequenceStart {
        input_commitment: Vec::new(),
        input_source_commitment: Vec::new(),
    }
}

/// The rule `verify_sequence_id` derives, exercised on both recur kinds.
///
/// Mirrors the recorder's own
/// `sequence_id_names_the_callee_at_boundaries_and_the_frame_everywhere_else`,
/// so the guest's derivation and the producer's behaviour are pinned to the
/// same table from both sides.
#[test]
fn sequence_id_derivation_matches_the_recorder() {
    let cfs = recur_frames_cfs();

    // Boundary steps name the callee.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(boundary_start_kind(), vec![0], "recur"),
    );
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(boundary_start_kind(), vec![1], "child"),
    );
    // A recur sequence's iteration boundary resolves through the site.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(boundary_start_kind(), vec![1, 0], "child"),
    );

    // A recur *tile* pushes no frame: its iteration and its closing `Exec`
    // both stay in `main`.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(exec_kind(ExecTarget::Tile("recur".into())), vec![0, 0], "main"),
    );
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(
            exec_kind(ExecTarget::RecurTile("recur".into())),
            vec![0],
            "main",
        ),
    );

    // A recur *sequence* does push one: its body names the site, while the
    // site's own closing `Exec` sits back in `main`.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(
            exec_kind(ExecTarget::Tile("inner".into())),
            vec![1, 0, 0],
            "child",
        ),
    );
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(
            exec_kind(ExecTarget::RecurSequence("child".into())),
            vec![1],
            "main",
        ),
    );
}

#[test]
#[should_panic(expected = "but its kind and coordinates determine")]
fn sequence_id_tampering_is_refused_for_an_exec_step() {
    verify_sequence_id(
        &recur_frames_cfs(),
        &step_with_sequence_id(
            exec_kind(ExecTarget::Tile("inner".into())),
            vec![1, 0, 0],
            // The site's containing sequence, not the frame it runs in — the
            // plausible lie, and the one a tamperer reaches for.
            "main",
        ),
    );
}

#[test]
#[should_panic(expected = "but its kind and coordinates determine")]
fn sequence_id_tampering_is_refused_at_a_program_boundary() {
    verify_sequence_id(
        &recur_frames_cfs(),
        &step_with_sequence_id(
            StepKind::ProgramStart(ProgramStartStep {
                entry_arguments: Vec::new(),
                output_commitment: Vec::new(),
                storage: StorageRoots {
                    root_before: Vec::new(),
                    root_after: Vec::new(),
                    index_root_before: Vec::new(),
                    index_root_after: Vec::new(),
                },
            }),
            vec![],
            "not-main",
        ),
    );
}

/// A boundary step names its callee, so claiming the *containing* sequence is
/// the plausible lie here — and it is the one `record_matches_item` would have
/// caught. This pins that the new check catches it too, at a coordinate
/// `verify_step_record_inputs` skips.
#[test]
#[should_panic(expected = "but its kind and coordinates determine")]
fn sequence_id_tampering_is_refused_at_a_sequence_boundary() {
    verify_sequence_id(
        &recur_frames_cfs(),
        &step_with_sequence_id(
            StepKind::SequenceEnd {
                output_commitment: Vec::new(),
            },
            // The recur sequence's own iteration boundary: names "child",
            // never the frame that contains the site.
            vec![1, 0],
            "main",
        ),
    );
}

#[test]
#[should_panic(expected = "but its kind and coordinates determine")]
fn sequence_id_tampering_is_refused_at_the_program_end() {
    verify_sequence_id(
        &recur_frames_cfs(),
        &step_with_sequence_id(
            StepKind::ProgramEnd(ProgramEndStep {
                output: None,
                output_commitment: Vec::new(),
                storage: StorageRoots {
                    root_before: Vec::new(),
                    root_after: Vec::new(),
                    index_root_before: Vec::new(),
                    index_root_after: Vec::new(),
                },
            }),
            vec![],
            "child",
        ),
    );
}

/// Regression for the forged-divergence attack (was a proof of concept).
///
/// `exec_index` reaches the trace leaf — it is `StepRecord`'s first field and
/// `hash_trace_item` hashes the whole postcard encoding — so a record and its
/// `exec_index`-bumped twin hash to different leaves, different trace roots,
/// and different fingerprint entries. While nothing verified the field, putting
/// the twin last in an otherwise honest window left every earlier item matching
/// the commitment, so `finalize` reached `assert!(diverges)` and returned
/// `Finished` against an honest commitment.
///
/// `verify_exec_index` closes it: the field is fixed by the step's trace index.
#[test]
#[should_panic(expected = "but its position determines")]
fn exec_index_tampering_is_refused() {
    // The step at trace index 3 must carry 4; anything else is unauthorized
    // entropy in the leaf.
    verify_exec_index(3, &exec_index_fixture(999));
}

#[test]
fn exec_index_matching_its_trace_position_is_accepted() {
    for trace_index in [0u64, 1, 7, 1_996] {
        verify_exec_index(trace_index, &exec_index_fixture(trace_index + 1));
    }
}

/// Off-by-one in either direction is the cheap version of the attack: the
/// neighbouring values are the ones an attacker reaches for first.
#[test]
#[should_panic(expected = "but its position determines")]
fn exec_index_one_short_is_refused() {
    verify_exec_index(3, &exec_index_fixture(3));
}

/// The field still reaches the trace leaf — that is *why* it must be verified.
/// If this ever stops holding, `verify_exec_index` is guarding nothing.
#[test]
fn exec_index_reaches_the_trace_leaf() {
    let honest = exec_index_fixture(1);
    let mut tampered = honest.clone();
    tampered.exec_index = 999;
    assert_eq!(tampered.kind, honest.kind);
    assert_eq!(tampered.coordinates, honest.coordinates);
    assert_eq!(tampered.sequence_id, honest.sequence_id);

    let honest_leaf = crate::merkle_tree::hash_trace_item(&honest);
    let tampered_leaf = crate::merkle_tree::hash_trace_item(&tampered);
    assert_ne!(honest_leaf, tampered_leaf);

    let root_after = |leaf: Vec<u8>| {
        let mut tree = TraceBridgeTree::new(1);
        tree.append(Bytes(EMPTY_LEAF.to_vec()));
        tree.append(Bytes(leaf));
        tree.root(0).expect("trace root").0
    };
    assert_ne!(
        root_after(honest_leaf),
        root_after(tampered_leaf),
        "a field that moves the trace root must be verified, or it is forgeable"
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
                leaves_output_open: false,
                state_is_output: false,
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
        recur_state: None,
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
            state: None,
        }),
    }
}

// ---------------------------------------------------------------------------
// Window seeding — `window-seed-reconstruction.md`
//
// A window's first step has no previous journal, so its carried recur progress
// arrives as a host-supplied seed. These tests pin the property that makes that
// acceptable: the seed is never believed. It is advanced by the step's own
// facts and held to the step's recorded commitment, so a wrong seed fails
// exactly as an absent one does.
// ---------------------------------------------------------------------------

/// The stack as it stands after iteration `through` of a 3-iteration chunked
/// sweep over 6 elements — what the host reconstructs from the trace prefix.
fn seeded_stack(through: u64) -> RecurProgressStack {
    let mut stack = RecurProgressStack::new();
    stack.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Tile, 2, 6, false);
    for iteration in 0..=through {
        stack
            .advance_tile_iteration(
                &CfsCoordinates(vec![0, iteration as u32]),
                iteration,
                3,
                2,
                RecurControlKind::Continue,
                None,
            )
            .expect("honest prefix advances cleanly");
    }
    stack
}

/// Advance `seed` by iteration `iteration`, holding it to `recorded`.
fn advance_seeded(seed: &mut RecurProgressStack, iteration: u32, recorded: [u8; 32]) {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(u64::from(iteration), 3, 2, RecurControlKind::Continue);
    let mut step = recur_iteration_step(iteration);
    step.recur_progress_commitment = recorded;
    crate::checks::cfs::advance_recur_progress(
        &cfs_cursor,
        seed,
        &step,
        Some(&journal),
        None,
        None,
        &BTreeMap::new(),
    );
}

/// The case the change exists for: a window opening at iteration 1 of a live
/// sweep verifies, seeded from the prefix, at unchanged window size.
#[test]
fn a_seeded_mid_loop_window_verifies() {
    let mut seed = seeded_stack(0);
    // What the recorder stamped on the step this window opens with.
    let recorded = {
        let mut expected = seeded_stack(0);
        expected
            .advance_tile_iteration(
                &CfsCoordinates(vec![0, 1]),
                1,
                3,
                2,
                RecurControlKind::Continue,
                None,
            )
            .expect("honest advance");
        expected.commitment()
    };

    advance_seeded(&mut seed, 1, recorded);
}

/// The failure this replaces: with no seed the empty stack is advanced, which
/// is the claim "no loop in flight" — and iteration 1 contradicts it.
#[test]
#[should_panic(expected = "recur iteration has no active recur site")]
fn an_unseeded_mid_loop_window_is_rejected() {
    let mut empty = RecurProgressStack::new();
    advance_seeded(&mut empty, 1, RecurProgressStack::new().commitment());
}

/// Reconstruction does not weaken the check: a seed claiming a different
/// position advances to a different stack and fails the comparison.
#[test]
#[should_panic(expected = "Recur progress commitment does not match")]
fn a_forged_seed_is_rejected() {
    // The honest record for a window opening after iteration 0...
    let recorded = {
        let mut expected = seeded_stack(0);
        expected
            .advance_tile_iteration(
                &CfsCoordinates(vec![0, 1]),
                1,
                3,
                2,
                RecurControlKind::Continue,
                None,
            )
            .expect("honest advance");
        expected.commitment()
    };

    // ...against a seed claiming iteration 1 already happened. It advances to
    // a different stack, so the recorded commitment does not reproduce.
    let mut forged = seeded_stack(1);
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(2, 3, 2, RecurControlKind::Continue);
    let mut step = recur_iteration_step(2);
    step.recur_progress_commitment = recorded;
    crate::checks::cfs::advance_recur_progress(
        &cfs_cursor,
        &mut forged,
        &step,
        Some(&journal),
        None,
        None,
        &BTreeMap::new(),
    );
}

/// Nesting: a seed must carry **both** frames. A `call_recur!` inside a
/// recur-sequence iteration is stack depth 2, and depth is exactly what a seed
/// carries — a seed naming only the inner frame commits to something else.
#[test]
fn a_nested_seed_carries_both_frames() {
    let mut both = RecurProgressStack::new();
    both.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Sequence, 1, 2, false);
    both.advance_sequence_iteration(&CfsCoordinates(vec![0, 0]), 0)
        .expect("outer iteration 0");
    both.push_site(CfsCoordinates(vec![0, 0, 0]), RecurSiteKind::Tile, 2, 6, false);
    assert_eq!(both.depth(), 2);

    let mut inner_only = RecurProgressStack::new();
    inner_only.push_site(CfsCoordinates(vec![0, 0, 0]), RecurSiteKind::Tile, 2, 6, false);
    assert_eq!(inner_only.depth(), 1);

    // Both stacks agree on the innermost frame and still commit differently,
    // so a window seeded with only the inner frame is rejected.
    assert_eq!(
        both.innermost().map(|frame| frame.site.clone()),
        inner_only.innermost().map(|frame| frame.site.clone()),
    );
    assert_ne!(both.commitment(), inner_only.commitment());
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
        recur_state: None,
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
        recur_state: None,
    };
    let end = StepRecord {
        exec_index: 2,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::SequenceEnd {
            output_commitment: sha(b"sequence-out"),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
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
        recur_state: None,
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
        recur_state: None,
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
        recur_state: None,
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
        recur_state: None,
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
    /// A header whose declared window size matches the window being built —
    /// the ordinary case, and what every slice test other than the shape tests
    /// wants.
    fn build_header_and_witness(
        bits: &[u64],
        bits_per_item: usize,
        fingerprint_len: usize,
        window_start: usize,
        window_len: usize,
    ) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
        build_header_and_witness_for_window(
            bits,
            bits_per_item,
            fingerprint_len,
            window_start,
            window_len,
            window_len,
        )
    }

    /// A header whose tail-roots commitment matches `tail_roots`, so the
    /// revealed-tail binding can be exercised.
    fn build_header_and_witness_with_tail(
        bits: &[u64],
        bits_per_item: usize,
        fingerprint_len: usize,
        window_start: usize,
        window_len: usize,
        tail_roots: &[Vec<u8>],
    ) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
        let (mut header, witness) = build_header_and_witness_for_window(
            bits,
            bits_per_item,
            fingerprint_len,
            window_start,
            window_len,
            tail_roots.len(),
        );
        header.revealed_tail_roots_commitment = sha256_bytes(
            &postcard::to_allocvec(&tail_roots.to_vec()).expect("tail roots serialize"),
        );
        (header, witness)
    }

    /// As above, but the commitment declares `window_size` independently of the
    /// window actually presented — which is what the shape check is about.
    fn build_header_and_witness_for_window(
        bits: &[u64],
        bits_per_item: usize,
        fingerprint_len: usize,
        window_start: usize,
        window_len: usize,
        window_size: usize,
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
            window_size: window_size as u64,
            revealed_tail_roots_commitment: vec![8; 32],
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

    /// Regression for the unbound-shape hole (was a proof of concept).
    ///
    /// `window_len` comes from the challenger's `Fingerprint::len` — a metadata
    /// field `Fingerprint::from` stores verbatim without checking it against
    /// `bits` — and `window_start` from the challenger's frontier position. The
    /// only constraint used to be `window_start + window_len <=
    /// fingerprint_len`: an upper bound, not a shape.
    ///
    /// A two-item window is the degenerate case, not a merely unusual one. Over
    /// `L` items `finalize` never compares item 0, requires items `1..L-2` to
    /// match, and requires item `L-1` to diverge — so at `L = 2` there are
    /// **zero** matching comparisons, nothing pins the challenger's opening
    /// state to reality, and a window fabricated anywhere in the trace reaches
    /// `Finished`.
    ///
    /// Note the slice itself is *genuine* here: no bits are tampered, no
    /// witness is forged. The lie is only the window's shape, which is why no
    /// other check catches it.
    #[test]
    #[should_panic(expected = "against a commitment built with a window of")]
    fn a_short_window_is_refused_anywhere_in_the_commitment() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        // Mid-commitment, arbitrary, and nothing like a head or terminal window.
        let (window_start, window_len, window_size) = (20, 2, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness_for_window(
            &bits,
            bits_per_item,
            items,
            window_start,
            window_len,
            window_size,
        );
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
            None,
        );
    }

    /// The tail roots, when supplied, are bound to the commitment — and the
    /// root the terminal step will need is picked out at the same time.
    #[test]
    fn a_window_ending_in_the_tail_carries_its_committed_root() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        // The commitment's window is 4, so its tail covers items 36..40.
        // A window ending at item 39 ends inside it.
        let (window_start, window_len) = (36, 4);
        let tail_roots: Vec<Vec<u8>> = (36..40u8).map(|i| vec![i; 32]).collect();

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness_with_tail(
            &bits,
            bits_per_item,
            items,
            window_start,
            window_len,
            &tail_roots,
        );
        let binding = assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
            Some(&tail_roots),
        );

        assert!(binding.window_is_terminal);
        // Item 39 is the last of the tail.
        assert_eq!(binding.final_committed_root, Some(vec![39u8; 32]));
    }

    /// A window that ends before the revealed tail gets no root, and so keeps
    /// proving divergence the only way it can — on fingerprint entries.
    #[test]
    fn a_window_ending_before_the_tail_carries_no_committed_root() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (10, 4);
        let tail_roots: Vec<Vec<u8>> = (36..40u8).map(|i| vec![i; 32]).collect();

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness_with_tail(
            &bits,
            bits_per_item,
            items,
            window_start,
            window_len,
            &tail_roots,
        );
        let binding = assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
            Some(&tail_roots),
        );

        assert!(!binding.window_is_terminal);
        assert_eq!(binding.final_committed_root, None);
    }

    /// Supplying roots the commitment did not commit to is refused — otherwise
    /// a challenger could invent a root to "diverge" from.
    #[test]
    #[should_panic(expected = "do not match the commitment's tail-roots commitment")]
    fn fabricated_tail_roots_are_refused() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len) = (36, 4);
        let committed: Vec<Vec<u8>> = (36..40u8).map(|i| vec![i; 32]).collect();

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness_with_tail(
            &bits,
            bits_per_item,
            items,
            window_start,
            window_len,
            &committed,
        );

        // A different story about the tail, so the terminal step could claim a
        // divergence that never happened.
        let invented: Vec<Vec<u8>> = (36..40u8).map(|i| vec![i ^ 0xFF; 32]).collect();
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
            Some(&invented),
        );
    }

    /// A window opening at the genesis state — the one place a short window is
    /// legitimate.
    fn genesis_init_transition(window: Fingerprint) -> InitTransition {
        InitTransition {
            init_frontier: SerializableFrontier {
                position: 0,
                leaf: EMPTY_LEAF.to_vec(),
                ommers: Vec::new(),
            },
            init_storage_frontier: SerializableFrontier {
                position: 0,
                leaf: EMPTY_LEAF.to_vec(),
                ommers: Vec::new(),
            },
            init_storage_root: Vec::new(),
            init_storage_index_root: coordinate_index_root(&BTreeMap::new()),
            active_drafts: BTreeMap::new(),
            fingerprint: window,
        }
    }

    fn short_genesis_window(
        bits: &[u64],
        bits_per_item: usize,
        items: usize,
        window_len: usize,
        window_size: usize,
    ) -> (InitTransition, TraceCommitmentHeader, FingerprintSliceWitness) {
        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(0, window_len, bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);
        let (header, witness) = build_header_and_witness_for_window(
            bits, bits_per_item, items, 0, window_len, window_size,
        );
        (genesis_init_transition(window), header, witness)
    }

    /// A divergence inside the trace's first `window_size` steps has no room
    /// for a full window behind it, so the honest host emits a short one — and
    /// it can only ever do so at trace index 0.
    ///
    /// It carries no pre-divergence margin, and needs none: the margin exists
    /// to pin a *challenger-supplied* opening state, and at index 0 the opening
    /// state is the public genesis constant instead.
    #[test]
    fn a_short_window_at_genesis_is_accepted() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 2, 4);

        let binding = assert_window_is_commitment_slice(&init, &header, &witness, None);
        assert!(!binding.window_is_terminal);
    }

    /// Even a one-item window, which is what a divergence in the trace's very
    /// first step produces.
    #[test]
    fn a_one_item_genesis_window_is_accepted() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 1, 4);

        assert_window_is_commitment_slice(&init, &header, &witness, None);
    }

    /// Position 0 alone is not enough: the opening *state* has to be genesis,
    /// or the missing margin would be a hole rather than an irrelevance.
    #[test]
    #[should_panic(expected = "must open on an empty coordinate index")]
    fn a_short_window_claiming_genesis_with_dirty_storage_is_refused() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (mut init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 2, 4);

        // Storage that already holds something cannot be the state before the
        // program's first step.
        init.init_storage_index_root = vec![7u8; 32];

        assert_window_is_commitment_slice(&init, &header, &witness, None);
    }

    #[test]
    #[should_panic(expected = "cannot have a draft in flight")]
    fn a_short_window_claiming_genesis_with_a_live_draft_is_refused() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (mut init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 2, 4);

        init.active_drafts.insert(
            [9u8; 32],
            TrackedDraftState {
                schema_hash: [1u8; 32],
                root: [2u8; 32],
            },
        );

        assert_window_is_commitment_slice(&init, &header, &witness, None);
    }

    /// The other direction: claiming more items than the commitment's window.
    #[test]
    #[should_panic(expected = "against a commitment built with a window of")]
    fn a_window_longer_than_the_commitments_is_refused() {
        let (bits_per_item, items) = (4, 40);
        let bits = fixture_bits(bits_per_item, items);
        let (window_start, window_len, window_size) = (20, 8, 4);

        let packer = BitPacker::new(bits_per_item);
        let window_bits = packer
            .get_range(window_start, window_start + window_len, &bits)
            .expect("window slice");
        let window = Fingerprint::from(window_bits, packer, window_len);

        let (header, witness) = build_header_and_witness_for_window(
            &bits,
            bits_per_item,
            items,
            window_start,
            window_len,
            window_size,
        );
        assert_window_is_commitment_slice(
            &init_transition_with_window(window_start, window),
            &header,
            &witness,
            None,
        );
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
        )
        .window_is_terminal
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
            recur_state: None,
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

/// A recur *sequence* site, shaped exactly like the one in
/// `examples/hello-tiles`: `call_recur_seq!` declares `input`, `output` and one
/// entry in `args`, so `RecurSequenceItem.sources` holds three bindings.
fn recur_sequence_site_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("decorate", 0, 1)],
        sequences: vec![
            SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::RecurSequence(RecurSequenceItem {
                    id: "decorate_lines".into(),
                    // input, output, args.0 — one binding per `call_recur_seq!`
                    // argument, per `ast.rs`'s parse and `flow_resolver.rs`'s
                    // `resolve_call_inputs`.
                    sources: vec![
                        InputBinding::storage(),
                        InputBinding::inline(),
                        InputBinding::inline(),
                    ],
                    state_is_output: false,
                })],
                entry_arguments: Vec::new(),
                produces_output: false,
            },
            SequenceDef {
                id: "decorate_lines".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "decorate".into(),
                    sources: vec![InputBinding::seq_input(0)],
                })],
                entry_arguments: Vec::new(),
                produces_output: false,
            },
        ],
    })
}

/// The site step the recorder writes for that call: `RecurSequenceStart`
/// becomes a `SequenceStart` at the site coordinate `[0]`, carrying the site's
/// own id (`recorder.rs`'s `RecurTileStart | RecurSequenceStart` arm).
fn recur_sequence_site_step() -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "decorate_lines".into(),
        coordinates: CfsCoordinates(vec![0]),
        kind: StepKind::SequenceStart {
            input_commitment: sha(b"recur-seq-in"),
            input_source_commitment: Vec::new(),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    }
}

/// What the fixed site wrapper records: one value per declared argument, in
/// the CFS's order — input, output, args.0 — with the driving list's storage
/// data under its own key.
fn recur_sequence_site_witness() -> FnInput {
    let commitment = sha(b"lines");
    let source_root_hash: [u8; 32] = commitment
        .clone()
        .try_into()
        .expect("test commitments are 32 bytes");
    FnInput {
        data: Vec::new(),
        values: vec![
            FnInputValue::StorageBinding,
            FnInputValue::Inline(b"draft-handle".to_vec()),
            FnInputValue::Inline(b"*".to_vec()),
        ],
        args: vec![
            FnInputArg {
                name: "input".to_string(),
                ty: "AuthRef<List<String>>".to_string(),
            },
            FnInputArg {
                name: "output".to_string(),
                ty: "Draft<CollectiveGreeting>".to_string(),
            },
            FnInputArg {
                name: "decoration".to_string(),
                ty: "String".to_string(),
            },
        ],
        storage: [(
            "input".to_string(),
            StorageData {
                coordinates: CfsCoordinates(vec![0, 0]),
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

#[test]
fn a_recur_sequence_site_binds_every_declared_source() {
    // The site wrapper must record one value per `call_recur_seq!` argument,
    // because the CFS declares one `InputBinding` per argument and the guest
    // asserts the two arities agree. A recur *site* — unlike a recur
    // *iteration* — does not short-circuit before reaching that assert.
    let cfs_cursor = recur_sequence_site_cfs();

    verify_step_record_inputs(
        &cfs_cursor,
        &recur_sequence_site_step(),
        Some(&recur_sequence_site_witness()),
        None,
        None,
    );
}

#[test]
#[should_panic(expected = "CFS input count does not match input source witness arity")]
fn a_recur_sequence_site_recording_only_its_input_is_rejected() {
    // The regression this guards: the site used to record
    // `values: vec![input]` and nothing else, which made every recur sequence
    // carrying an `output` or any `args` unprovable.
    let cfs_cursor = recur_sequence_site_cfs();
    let mut witness = recur_sequence_site_witness();
    witness.values.truncate(1);
    witness.args.truncate(1);

    verify_step_record_inputs(
        &cfs_cursor,
        &recur_sequence_site_step(),
        Some(&witness),
        None,
        None,
    );
}

// ---------------------------------------------------------------------------
// Carried-state chaining — `loop-carried-state.md` §4
//
// A recur's carried state travels as `FnInputValue::Inline`, and the guest's
// whole obligation for an inline binding is that the source *is* inline. These
// tests pin the missing half: iteration N's incoming state must be the state
// iteration N-1 returned.
// ---------------------------------------------------------------------------

fn state(seed: &[u8]) -> Hash32 {
    raster_core::recur_progress::state_commitment(seed)
}

fn transition(state_in: Hash32, state_out: Hash32) -> RecurStateTransition {
    RecurStateTransition {
        state_in,
        state_out,
    }
}

/// A 3-iteration unchunked sweep whose carried state advances a -> b -> c -> d.
fn state_chain_stack() -> RecurProgressStack {
    let mut stack = RecurProgressStack::new();
    stack.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Tile, 1, 3, false);
    stack
}

#[test]
fn a_carried_state_chain_advances_across_iterations() {
    let mut stack = state_chain_stack();
    let steps = [(b"a", b"b"), (b"b", b"c"), (b"c", b"d")];
    for (index, (from, to)) in steps.iter().enumerate() {
        stack
            .advance_tile_iteration(
                &CfsCoordinates(vec![0, index as u32]),
                index as u64,
                3,
                1,
                RecurControlKind::Continue,
                Some(&transition(state(*from), state(*to))),
            )
            .expect("an honest chain advances");
    }
}

#[test]
fn a_substituted_iteration_state_is_rejected() {
    // The bug this whole change exists for: iteration 1 claims to have started
    // from a state iteration 0 never produced.
    let mut stack = state_chain_stack();
    stack
        .advance_tile_iteration(
            &CfsCoordinates(vec![0, 0]),
            0,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"a"), state(b"b"))),
        )
        .expect("iteration 0 adopts");

    assert_eq!(
        stack.advance_tile_iteration(
            &CfsCoordinates(vec![0, 1]),
            1,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"forged"), state(b"c"))),
        ),
        Err(RecurProgressViolation::CarriedStateMismatch {
            expected: state(b"b"),
            actual: state(b"forged"),
        }),
    );
}

#[test]
fn an_omitted_carried_state_is_rejected() {
    // Absence is the cheapest attack on a continuity check — the weakness
    // `checks/drafts.rs`'s permissive `if let Some(..)` still has for drafts.
    let mut stack = state_chain_stack();
    stack
        .advance_tile_iteration(
            &CfsCoordinates(vec![0, 0]),
            0,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"a"), state(b"b"))),
        )
        .expect("iteration 0 adopts");

    assert_eq!(
        stack.advance_tile_iteration(
            &CfsCoordinates(vec![0, 1]),
            1,
            3,
            1,
            RecurControlKind::Continue,
            None,
        ),
        Err(RecurProgressViolation::CarriedStateOmitted),
    );
}

#[test]
fn a_stateless_site_claiming_carried_state_is_rejected() {
    let mut stack = state_chain_stack();
    stack
        .advance_tile_iteration(
            &CfsCoordinates(vec![0, 0]),
            0,
            3,
            1,
            RecurControlKind::Continue,
            None,
        )
        .expect("a stateless iteration 0 is fine");

    assert_eq!(
        stack.advance_tile_iteration(
            &CfsCoordinates(vec![0, 1]),
            1,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"x"), state(b"y"))),
        ),
        Err(RecurProgressViolation::CarriedStateUnexpected),
    );
}

#[test]
fn a_seed_differing_only_in_carried_state_is_rejected() {
    // The window-open case: two stacks identical but for the state they claim
    // the loop reached commit differently, so a forged seed cannot reproduce
    // the recorded commitment.
    let mut honest = state_chain_stack();
    honest
        .advance_tile_iteration(
            &CfsCoordinates(vec![0, 0]),
            0,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"a"), state(b"b"))),
        )
        .expect("honest");

    let mut forged = state_chain_stack();
    forged
        .advance_tile_iteration(
            &CfsCoordinates(vec![0, 0]),
            0,
            3,
            1,
            RecurControlKind::Continue,
            Some(&transition(state(b"a"), state(b"elsewhere"))),
        )
        .expect("forged advances too — it just lands somewhere else");

    assert_ne!(honest.commitment(), forged.commitment());
}

#[test]
fn a_sequence_iterations_output_must_be_the_state_it_claims_to_have_produced() {
    // The terminal pin. Every `state_out` but the last is held by the next
    // iteration's bound `state_in`; the last is held here, against the bytes
    // the iteration actually emitted. It lives on the iteration rather than the
    // site because a site's recorded output is the raster-encoded stored
    // object, while the carried state is postcard — the iteration's output is
    // where the two encodings coincide.
    let mut stack = RecurProgressStack::new();
    stack.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Sequence, 1, 1, true);
    stack
        .advance_sequence_iteration(&CfsCoordinates(vec![0, 0]), 0)
        .expect("counted");

    let honest = stack.clone().fold_sequence_iteration_state(
        &CfsCoordinates(vec![0, 0]),
        Some(&transition(state(b"seed"), state(b"final"))),
        Some(b"final"),
    );
    assert!(honest.is_ok());

    assert_eq!(
        stack
            .fold_sequence_iteration_state(
                &CfsCoordinates(vec![0, 0]),
                Some(&transition(state(b"seed"), state(b"claimed"))),
                Some(b"actually-emitted"),
            )
            .unwrap_err(),
        RecurProgressViolation::TerminalStateMismatch {
            expected: state(b"claimed"),
            actual: state(b"actually-emitted"),
        },
    );
}

#[test]
fn a_state_returning_sequence_iteration_without_an_output_is_rejected() {
    let mut stack = RecurProgressStack::new();
    stack.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Sequence, 1, 1, true);
    stack
        .advance_sequence_iteration(&CfsCoordinates(vec![0, 0]), 0)
        .expect("counted");

    assert_eq!(
        stack
            .fold_sequence_iteration_state(
                &CfsCoordinates(vec![0, 0]),
                Some(&transition(state(b"seed"), state(b"final"))),
                None,
            )
            .unwrap_err(),
        RecurProgressViolation::TerminalStateUnwitnessed,
    );
}

#[test]
fn a_state_plus_output_sequence_iteration_is_not_pinned_by_its_output() {
    // A state+output site returns the draft, not the state, so its recorded
    // output is not the carried state and must not be compared against it.
    let mut stack = RecurProgressStack::new();
    stack.push_site(CfsCoordinates(vec![0]), RecurSiteKind::Sequence, 1, 1, false);
    stack
        .advance_sequence_iteration(&CfsCoordinates(vec![0, 0]), 0)
        .expect("counted");
    assert!(stack
        .fold_sequence_iteration_state(
            &CfsCoordinates(vec![0, 0]),
            Some(&transition(state(b"seed"), state(b"final"))),
            Some(b"a-draft-root-not-the-state"),
        )
        .is_ok());
}
