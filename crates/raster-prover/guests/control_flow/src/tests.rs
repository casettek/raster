//! Tests of the control-flow check: the CFS conformance of one step. Moved
//! from the transition guest with the check (`control-flow-prover` step 6).

use std::collections::BTreeMap;

use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource,
    RecurSequenceItem, RecurTileItem, SequenceChildItem, SequenceDef, SequenceItem, TileDef,
    TileItem, FIRST_COORDINATE,
};
use raster_core::control_flow::{ControlFlowInput, ControlFlowSegment, ControlFlowStep};
use raster_core::transition::SerializableFrontier;
use raster_core::input::{Hash32, SelectionWitness, SelectorSegment};
use raster_core::merkle::to_node_hash;
use raster_core::recur_progress::RecurProgressStack;
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, FnInputArg, FnInputValue, ProgramEndStep, ProgramStartStep,
    StepKind, StepRecord, StorageData, StorageRoots,
};

use guest_common::input_source::input_source_commitment;
use bridgetree::NonEmptyFrontier;
use guest_common::merkle_tree::{
    deserialize_frontier, frontier_root, hash_trace_item, serialize_frontier, sha256_bytes, Bytes,
    TraceBridgeTree, EMPTY_LEAF,
};

use crate::checks::{
    verify_exec_index, verify_segment, verify_sequence_id, verify_sequence_scope_parent,
    verify_step, verify_step_record_inputs,
};

fn sha(bytes: &[u8]) -> Vec<u8> {
    sha256_bytes(bytes)
}

/// A `ControlFlowInput` carrying no witnesses, for steps that read none.
fn bare_input(
    record: &StepRecord,
    trace_position: u64,
    expected_before: Option<Vec<CfsCoordinates>>,
) -> ControlFlowInput {
    ControlFlowInput {
        step_record: record.clone(),
        trace_position,
        trace_root_before: EMPTY_LEAF.to_vec(),
        expected_before,
        input_source_witness: None,
        sequence_scope_witness: None,
        sequence_scope_parent: None,
    }
}

fn program_end_record(exec_index: u64) -> StepRecord {
    StepRecord {
        exec_index,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![]),
        kind: StepKind::ProgramEnd(ProgramEndStep {
            output: None,
            output_commitment: Vec::new(),
            storage: dummy_storage_roots(),
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    }
}

/// `main` running one tile `t` on an inline argument.
fn one_tile_cfs() -> CfsCursor {
    let mut cfs = ControlFlowSchema::new("test");
    cfs.tiles.push(TileDef::iter("t", 1, 1));
    let mut main = SequenceDef::new("main");
    main.items.push(SequenceChildItem::Tile(TileItem {
        id: "t".to_string(),
        sources: vec![InputBinding::inline()],
    }));
    cfs.sequences.push(main);
    CfsCursor::new(cfs)
}

/// The three steps an honest run of [`one_tile_cfs`] records, as segment steps.
fn one_tile_run() -> Vec<ControlFlowStep> {
    let argument = FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::Inline(vec![7])],
        args: vec![FnInputArg {
            name: "x".to_string(),
            ty: "u8".to_string(),
        }],
        storage: Default::default(),
    };
    let tile = StepRecord {
        exec_index: 2,
        sequence_id: "main".to_string(),
        coordinates: CfsCoordinates(vec![1]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("t".to_string()),
            intra_sequence_index: FIRST_COORDINATE,
            input_commitment: Vec::new(),
            input_source_commitment: input_source_commitment(&argument),
            output_commitment: Vec::new(),
            storage: empty_roots(),
        }),
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    };
    let step = |step_record: StepRecord, input_source_witness: Option<FnInput>| ControlFlowStep {
        step_record,
        input_source_witness,
        sequence_scope_witness: None,
        sequence_scope_parent: None,
    };
    vec![
        step(program_start_record(program_start_step(Vec::new(), Vec::new())), None),
        step(tile, Some(argument)),
        step(program_end_record(3), None),
    ]
}

/// The trace frontier before any step: the seed leaf alone.
fn seed_frontier() -> NonEmptyFrontier<Bytes> {
    deserialize_frontier(&SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    })
    .unwrap()
}

fn segment(
    start: &NonEmptyFrontier<Bytes>,
    expected_before: Option<Vec<CfsCoordinates>>,
    steps: Vec<ControlFlowStep>,
) -> ControlFlowSegment {
    ControlFlowSegment {
        start_frontier: serialize_frontier(start),
        expected_before,
        steps,
    }
}

/// A segment derives every step's trace facts from its start: positions
/// advance by one, each root is the frontier after the records before it, and
/// each step expects what the previous step allowed.
#[test]
fn a_segment_derives_each_steps_trace_facts() {
    let steps = one_tile_run();
    let entries = verify_segment(&one_tile_cfs(), segment(&seed_frontier(), None, steps.clone()));

    let mut frontier = seed_frontier();
    for (k, (entry, step)) in entries.iter().zip(&steps).enumerate() {
        assert_eq!(entry.trace_position, k as u64);
        assert_eq!(entry.trace_root_before, frontier_root(&frontier));
        assert_eq!(entry.step_record_hash, hash_trace_item(&step.step_record));
        frontier.append(Bytes(hash_trace_item(&step.step_record)));
    }
    assert!(entries[0].expected_before.is_none());
    assert_eq!(entries[1].expected_before.as_ref(), Some(&entries[0].expected_after));
    assert_eq!(entries[2].expected_before.as_ref(), Some(&entries[1].expected_after));
    assert!(entries[2].expected_after.is_empty(), "nothing follows ProgramEnd");
}

/// The modes agree: the window as one segment and as one-step segments — each
/// started where the previous one left off — commit the same entries. Which
/// mode the host picks changes the number of receipts, never what they say.
#[test]
fn one_segment_and_one_step_segments_commit_the_same_entries() {
    let cfs = one_tile_cfs();
    let steps = one_tile_run();
    let whole = verify_segment(&cfs, segment(&seed_frontier(), None, steps.clone()));

    let mut frontier = seed_frontier();
    let mut expected = None;
    let mut per_step = Vec::new();
    for step in steps {
        let record_hash = hash_trace_item(&step.step_record);
        let entry = verify_segment(&cfs, segment(&frontier, expected.clone(), vec![step]))
            .pop()
            .unwrap();
        expected = Some(entry.expected_after.clone());
        frontier.append(Bytes(record_hash));
        per_step.push(entry);
    }
    assert_eq!(whole, per_step);
}

/// Inside a segment the guest holds each step to the previous one's
/// successors itself: skipping the tile is refused at `ProgramEnd`.
#[test]
#[should_panic(expected = "is not among the coordinates the previous step allows")]
fn a_segment_skipping_a_step_is_refused() {
    let mut steps = one_tile_run();
    steps.remove(1);
    steps[1].step_record.exec_index = 2;
    verify_segment(&one_tile_cfs(), segment(&seed_frontier(), None, steps));
}

#[test]
#[should_panic(expected = "must cover at least one step")]
fn an_empty_segment_is_refused() {
    verify_segment(&one_tile_cfs(), segment(&seed_frontier(), None, Vec::new()));
}

/// `verify_step` is the whole control-flow check behind one function
/// (`control-flow-prover` step 5): its outcome is the successor set the CFS
/// declares for the step.
#[test]
fn verify_step_hands_back_the_steps_cfs_successors() {
    let cfs = no_entrypoint_cfs();
    let record = program_start_record(program_start_step(Vec::new(), Vec::new()));
    let expected_after = verify_step(&cfs, &bare_input(&record, 0, None));
    assert_eq!(
        Some(expected_after),
        cfs.try_get_next_coordinates(record.coordinates()),
    );
}

/// Nothing may follow `ProgramEnd`, whatever the CFS cursor would compute for
/// the root coordinate.
#[test]
fn verify_step_allows_no_successor_after_program_end() {
    let cfs = no_entrypoint_cfs();
    let record = program_end_record(5);
    let expected = vec![CfsCoordinates(vec![])];
    let expected_after = verify_step(&cfs, &bare_input(&record, 4, Some(expected)));
    assert!(expected_after.is_empty());
}

#[test]
#[should_panic(expected = "is not among the coordinates the previous step allows")]
fn verify_step_refuses_a_step_the_previous_one_does_not_allow() {
    let cfs = no_entrypoint_cfs();
    let record = program_end_record(5);
    let expected = vec![CfsCoordinates(vec![7])];
    verify_step(&cfs, &bare_input(&record, 4, Some(expected)));
}

#[test]
#[should_panic(expected = "carries exec_index")]
fn verify_step_holds_exec_index_to_the_trace_position() {
    let cfs = no_entrypoint_cfs();
    let record = program_start_record(program_start_step(Vec::new(), Vec::new()));
    verify_step(&cfs, &bare_input(&record, 3, None));
}

/// The anchor runs inside `verify_step` (`control-flow-prover` step 3), so this
/// guest refuses an unbound witness on its own rather than relying on the
/// transition guest — here on a root step, which no binding check reads at all.
#[test]
#[should_panic(expected = "must not carry an input source witness")]
fn verify_step_anchors_the_input_source_witness_itself() {
    let cfs = no_entrypoint_cfs();
    let record = program_start_record(program_start_step(Vec::new(), Vec::new()));
    let (unbound, ..) = selecting_input_witness("arg", CfsCoordinates(vec![1]));
    let input = ControlFlowInput {
        input_source_witness: Some(unbound),
        ..bare_input(&record, 0, None)
    };
    verify_step(&cfs, &input);
}

#[test]
fn sequence_scope_parent_binding_accepts_the_real_parent() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));

    let (trace_root, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let parent = (parent_record, witness);

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        Some(&parent),
        &trace_root,
    );
}

/// Two steps in the same frame, verifying at different points in the trace.
///
/// This is the shape that made `hello-tiles` unprovable: window offsets 9 and
/// 10 both resolved the scope parent `[21]`, so the host built two witnesses —
/// one against a 74-item prefix, one against 75 — and keyed both by the parent
/// record alone. The second `insert` replaced the first, and offset 9 was left
/// folding a proof over a prefix one item longer than its own trace root.
///
/// The fix is the key: each step gets *its* witness. A test keyed only by the
/// parent cannot catch this, because with one step there is nothing to
/// overwrite.
#[test]
fn two_steps_sharing_a_scope_parent_each_verify_at_their_own_trace_root() {
    let cfs_cursor = scope_binding_cfs();
    let (first_step, _, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));

    // A second step in the same frame, one item later in the trace.
    let mut second_step = first_step.clone();
    second_step.exec_index = first_step.exec_index + 1;

    // Each verifies against a different prefix: the parent, then the parent
    // plus one intervening step.
    let filler = parent_record.clone();
    let (root_at_first, witness_at_first) = trace_root_and_witness(&[parent_record.clone()], 0);
    let (root_at_second, witness_at_second) =
        trace_root_and_witness(&[parent_record.clone(), filler], 0);
    assert_ne!(
        root_at_first, root_at_second,
        "the trace root must grow between the two steps, or this proves nothing",
    );

    // The host keys each step's parent by the step's own `exec_index`, and
    // each step's input carries only its own.
    let parent_at_first = (parent_record.clone(), witness_at_first);
    let parent_at_second = (parent_record, witness_at_second);

    verify_sequence_scope_parent(
        &cfs_cursor,
        &first_step,
        Some(&parent_args),
        Some(&parent_at_first),
        &root_at_first,
    );
    verify_sequence_scope_parent(
        &cfs_cursor,
        &second_step,
        Some(&parent_args),
        Some(&parent_at_second),
        &root_at_second,
    );
}

/// A step handed *another* step's witness for the same parent is refused
/// rather than accidentally accepted — the failure the old map key produced.
/// The witness was built for the first step's trace root, and the second step
/// verifies against its own, one item longer.
#[test]
#[should_panic(expected = "not in the trace at the claimed position")]
fn a_step_cannot_borrow_another_steps_witness_for_the_same_parent() {
    let cfs_cursor = scope_binding_cfs();
    let (first_step, _, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));
    let mut second_step = first_step.clone();
    second_step.exec_index = first_step.exec_index + 1;

    let (_, witness_at_first) = trace_root_and_witness(&[parent_record.clone()], 0);
    let (root_at_second, _) =
        trace_root_and_witness(&[parent_record.clone(), parent_record.clone()], 0);
    let borrowed = (parent_record, witness_at_first);

    verify_sequence_scope_parent(
        &cfs_cursor,
        &second_step,
        Some(&parent_args),
        Some(&borrowed),
        &root_at_second,
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
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));

    // A different story about what the caller passed, built to agree with a
    // step that read it.
    let fabricated = storage_input_witness(CfsCoordinates(vec![5, 3]), sha(b"a-different-story"));

    let (trace_root, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let parent = (parent_record, witness);

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&fabricated),
        Some(&parent),
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
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));

    // The witness proves inclusion in *some* trace — just not the one this step
    // is being appended to.
    let (_, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let (unrelated_root, _) = trace_root_and_witness(
        &[step_with_sequence_id(
            boundary_start_kind(),
            vec![2],
            "sub",
        )],
        0,
    );
    let parent = (parent_record, witness);

    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        Some(&parent),
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
/// The binding check alone still accepts both: separating them is
/// [`verify_sequence_scope_parent`]'s job, which this test deliberately does not
/// call. The tests above show that it does.
#[test]
fn poc_the_sequence_scope_witness_is_bound_to_nothing() {
    let cfs_cursor = scope_binding_cfs();

    let step_at = |read_from: CfsCoordinates, commitment: Vec<u8>| {
        let step_record = StepRecord {
            exec_index: 3,
            sequence_id: "sub".into(),
            coordinates: CfsCoordinates(vec![1, 1]),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("consumer".into()),
                intra_sequence_index: FIRST_COORDINATE,
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
        (CfsCoordinates(vec![10, 10]), sha(b"one-story")),
        (CfsCoordinates(vec![5, 3]), sha(b"a-different-story")),
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
        );

        parent_commitments.push(input_source_commitment(&fabricated_parent));
    }

    assert_ne!(
        parent_commitments[0], parent_commitments[1],
        "the two fabricated parents must be distinguishable, or the test proves nothing"
    );
}

#[test]
fn the_honest_two_parameter_scope_witness_verifies() {
    let (cfs_cursor, _, parent_args, trace_root, parent) = two_parameter_scope_scenario();
    let (step_record, step_source) = two_parameter_consumer(&[("x", "A"), ("y", "B")]);

    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&step_source),
        Some(&parent_args),
    );
    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        Some(&parent),
        &trace_root,
    );
}

/// `docs/issues/fn-input-arg-names-unbound.md`: a consumer that ran on
/// `(B, A)` passes the binding check when the caller's argument names are
/// supplied as `[q, p]`. `values` and `storage` are untouched, so before the
/// names were committed the reordered witness hashed to the parent's own
/// commitment and `verify_sequence_scope_parent` accepted it too.
#[test]
#[should_panic(expected = "Sequence-scope witness is not the parent SequenceStart's recorded input source")]
fn a_scope_witness_with_reordered_arg_names_is_refused() {
    let (cfs_cursor, _, parent_args, trace_root, parent) = two_parameter_scope_scenario();
    let (step_record, step_source) = two_parameter_consumer(&[("x", "B"), ("y", "A")]);
    let reordered = renaming_args(parent_args, &["q", "p"]);

    // The binding check alone accepts the swap: position 0 is `B` on both sides.
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&step_source),
        Some(&reordered),
    );
    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&reordered),
        Some(&parent),
        &trace_root,
    );
}

/// The same gap with one binding named twice: a consumer that read `A` for
/// both arguments, checked against a caller whose names are `[p, p]`.
#[test]
#[should_panic(expected = "Sequence-scope witness is not the parent SequenceStart's recorded input source")]
fn a_scope_witness_naming_one_binding_twice_is_refused() {
    let (cfs_cursor, _, parent_args, trace_root, parent) = two_parameter_scope_scenario();
    let (step_record, step_source) = two_parameter_consumer(&[("x", "A"), ("y", "A")]);
    let duplicated = renaming_args(parent_args, &["p", "p"]);

    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&step_source),
        Some(&duplicated),
    );
    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&duplicated),
        Some(&parent),
        &trace_root,
    );
}

/// Regression for `docs/issues/sequence-scope-forbids-narrowing.md`: the read
/// is the caller's `[personal_data]` followed by the declared `[name]`. This
/// panicked with "Storage sequence scope input does not match consumer
/// binding" while scope bindings carried no path.
#[test]
fn a_scope_parameter_narrowed_by_its_declared_path_verifies() {
    verify_narrowed_scope_read(
        &["name"],
        &["personal_data"],
        &["personal_data", "name"],
        b"personal-data",
    );
}

/// Same object, same root, a sibling field: only the path tells them apart.
#[test]
#[should_panic(expected = "selects a different part of the caller's value than the CFS declares")]
fn a_scope_parameter_narrowed_to_another_field_is_refused() {
    verify_narrowed_scope_read(
        &["name"],
        &["personal_data"],
        &["personal_data", "addresses"],
        b"personal-data",
    );
}

/// The CFS declares a narrowing the step did not perform.
#[test]
#[should_panic(expected = "selects a different part of the caller's value than the CFS declares")]
fn a_declared_narrowing_the_step_skipped_is_refused() {
    verify_narrowed_scope_read(&["name"], &["personal_data"], &["personal_data"], b"personal-data");
}

/// The step narrowed where the CFS declares the parameter passed whole.
#[test]
#[should_panic(expected = "Storage sequence scope input does not match consumer binding")]
fn an_undeclared_narrowing_is_refused() {
    verify_narrowed_scope_read(&[], &["personal_data"], &["personal_data", "name"], b"personal-data");
}

/// The declared path, read out of a different object.
#[test]
#[should_panic(expected = "reads a different object than the caller passed")]
fn a_narrowed_read_of_another_object_is_refused() {
    verify_narrowed_scope_read(
        &["name"],
        &["personal_data"],
        &["personal_data", "name"],
        b"another-object",
    );
}

/// There is no selector over inline bytes, so a narrowed inline parameter
/// cannot be checked: fail closed.
#[test]
#[should_panic(expected = "narrowing is only verifiable on a stored value")]
fn narrowing_an_inline_scope_parameter_fails_closed() {
    let cfs_cursor =
        scope_binding_cfs_reading(InputBinding::seq_input(0).with_path(field_path(&["name"])));
    let (step_record, _, _, _) =
        scope_binding_scenario(CfsCoordinates(vec![]), sha(b"personal-data"));
    let inline = |bytes: &[u8]| FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::Inline(bytes.to_vec())],
        args: vec![FnInputArg {
            name: "arg".to_string(),
            ty: "Vec<u8>".to_string(),
        }],
        storage: Default::default(),
    };
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&inline(b"John")),
        Some(&inline(b"{name: John}")),
    );
}

#[test]
fn an_entry_argument_read_through_its_declared_path_verifies() {
    verify_entry_argument_read(&["personal_data", "name"], &["personal_data", "name"]);
}

/// Before argument paths the guest checked only the coordinate `[]`, so any
/// part of the entry object passed as any argument.
#[test]
#[should_panic(expected = "selects a different part of the entry object than the CFS declares")]
fn an_entry_argument_read_through_another_path_is_refused() {
    verify_entry_argument_read(&["personal_data", "name"], &["seed"]);
}

#[test]
fn a_prior_item_read_through_its_declared_path_verifies() {
    verify_prior_item_read(&["title"], &["title"]);
}

/// Before argument paths the guest checked only the source coordinates, so
/// any field of the producer's output passed.
#[test]
#[should_panic(expected = "selects a different part of the source than the CFS declares")]
fn a_prior_item_read_through_another_path_is_refused() {
    verify_prior_item_read(&["title"], &["lines"]);
}

/// `sub` records no `returns` (the CFS could not bind it), so the consumer is
/// held by the fallback rule: anything inside the source item.
#[test]
fn verify_step_record_inputs_accepts_sequence_descendant_producer_coordinates() {
    let cfs_cursor = producer_sequence_cfs();
    let step_record = StepRecord {
        exec_index: 1,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![2]),
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
        storage_input_witness(CfsCoordinates(vec![1, 1]), sha(b"producer-output"));

    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&input_source_witness),
        None,
    );
}

/// A consumer of a sequence's output reads the object the sequence returns.
#[test]
fn a_sequence_argument_resolves_to_the_returned_object() {
    verify_consumer_input(vec![call_item("sub", vec![])], vec![sub_returning_other()], 0, vec![1, 2]);
}

/// `producer`'s output at `[1,1]` is written by `sub` but not returned by it.
/// The old "inside the source item" rule accepted it.
#[test]
#[should_panic(expected = "do not match expected CFS source")]
fn a_sequence_argument_rejects_an_object_the_sequence_did_not_return() {
    verify_consumer_input(vec![call_item("sub", vec![])], vec![sub_returning_other()], 0, vec![1, 1]);
}

/// `main = [producer, pass(producer), consumer(pass's result)]`: `pass`
/// returns its parameter, so the consumer honestly reads `producer`'s object
/// at `[1]` — outside the call at `[2]`, which the old rule refused.
#[test]
fn a_sequence_argument_follows_a_returned_parameter_to_its_producer() {
    let pass = SequenceDef {
        id: "pass".into(),
        input_sources: vec![InputBinding::seq_input(0)],
        items: vec![],
        entry_arguments: vec![],
        produces_output: false,
        returns: Some(raster_core::cfs::SequenceReturn {
            source: InputBinding::seq_input(0),
            path: vec![],
        }),
    };
    verify_consumer_input(
        vec![
            tile_item("producer", vec![InputBinding::Direct(InputSource::Inline)]),
            call_item("pass", vec![InputBinding::prior_item_output(0)]),
        ],
        vec![pass],
        1,
        vec![1],
    );
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

    // A recur site's own steps name the *enclosing* sequence; the site is
    // `site_id`. (Before `RecurStart`, a site's `Start` borrowed
    // `SequenceStart` and put the tile's id in the sequence slot.)
    verify_sequence_id(&cfs, &step_with_sequence_id(recur_start_kind("recur"), vec![1], "main"));
    verify_sequence_id(&cfs, &step_with_sequence_id(recur_end_kind("recur"), vec![-1], "main"));
    verify_sequence_id(&cfs, &step_with_sequence_id(recur_start_kind("child"), vec![2], "main"));
    verify_sequence_id(&cfs, &step_with_sequence_id(recur_end_kind("child"), vec![-2], "main"));
    // A recur sequence's iteration boundary resolves through the site.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(boundary_start_kind(), vec![2, 1], "child"),
    );

    // A recur *tile* pushes no frame: its iterations stay in `main`.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(exec_kind(ExecTarget::Tile("recur".into())), vec![1, 1], "main"),
    );

    // A recur *sequence* does push one: its body names the site.
    verify_sequence_id(
        &cfs,
        &step_with_sequence_id(
            exec_kind(ExecTarget::Tile("inner".into())),
            vec![2, 1, 1],
            "child",
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
            vec![2, 1, 1],
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
            vec![2, 1],
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

    let honest_leaf = guest_common::merkle_tree::hash_trace_item(&honest);
    let tampered_leaf = guest_common::merkle_tree::hash_trace_item(&tampered);
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
    );
}

#[test]
fn a_recur_sequence_body_reading_its_item_is_accepted() {
    for index in 0..2 {
        let (_, scope, _) = recur_sequence_iteration(index);
        let (step, input) = decorate_reading(index, index);
        verify_step_record_inputs(&recur_sequence_site_cfs(), &step, Some(&input), Some(&scope));
    }
}

#[test]
#[should_panic(expected = "Storage sequence scope input does not match consumer binding")]
fn a_recur_sequence_body_reading_another_element_is_rejected() {
    let (_, scope, _) = recur_sequence_iteration(1);
    let (step, input) = decorate_reading(1, 0);
    verify_step_record_inputs(&recur_sequence_site_cfs(), &step, Some(&input), Some(&scope));
}

/// Looking through the handle is only for a handle: a scope value that does
/// not decode as `RecurSequenceInput` is refused rather than read as one.
#[test]
#[should_panic(expected = "is not a RecurSequenceInput handle")]
fn a_recur_sequence_iteration_without_a_handle_is_rejected() {
    let (_, mut scope, _) = recur_sequence_iteration(0);
    scope.values[0] = FnInputValue::Inline(b"not a handle".to_vec());
    let (step, input) = decorate_reading(0, 0);
    verify_step_record_inputs(&recur_sequence_site_cfs(), &step, Some(&input), Some(&scope));
}

/// The companion to the PoC: what the `Start` *is* held to. Naming the wrong
/// producer is rejected — coordinates are the one fact the CFS check pins.
#[test]
#[should_panic(expected = "prior-item-output coordinates do not match expected CFS source")]
fn a_site_start_naming_the_wrong_producer_is_rejected() {
    let (mut start, mut input, _) = site_start_claiming([0xEE; 32], 0);
    input.storage.get_mut("input").unwrap().coordinates = CfsCoordinates(vec![5]);
    if let StepKind::RecurStart(recur_start) = &mut start.kind {
        recur_start.input_source_commitment = input_source_commitment(&input);
    }
    verify_step_record_inputs(&produced_list_sweep_cfs(), &start, Some(&input), None);
}

/// A site `Start` that names its producer satisfies its bindings, whatever
/// object it claims to have read: that is the storage check's question, in the
/// transition guest. The `run_site_start` tests there used to make this call
/// themselves; the check now lives in this guest.
#[test]
fn a_site_start_naming_its_producer_satisfies_its_bindings() {
    let (start, input, _) = site_start_claiming([0xEE; 32], 0);
    verify_step_record_inputs(&produced_list_sweep_cfs(), &start, Some(&input), None);
}

/// A site opens with `RecurStart` at `[s]` and closes with `RecurEnd` at
/// `[-s]`; the borrowed shapes are refused. The site `[1]` of
/// `chunked_recur_cfs` is a recur tile.
#[test]
fn a_recur_site_accepts_only_its_own_step_kinds() {
    let cfs = chunked_recur_cfs(Some(2));
    let rejects = |record: StepRecord| {
        std::panic::catch_unwind(|| {
            verify_step_record_inputs(&cfs, &record, None, None)
        })
        .is_err()
    };
    // The borrowed `SequenceStart` at the site.
    assert!(rejects(kind_at(boundary_start_kind(), vec![1])));
    // `RecurEnd` at the open coordinate; `RecurStart` at the close.
    assert!(rejects(kind_at(recur_end_kind("collect"), vec![1])));
    assert!(rejects(kind_at(recur_start_kind("collect"), vec![-1])));
    // A site id that is not the item's.
    assert!(rejects(kind_at(recur_end_kind("other"), vec![-1])));
    // The honest close binds no inputs and passes.
    verify_step_record_inputs(&cfs, &kind_at(recur_end_kind("collect"), vec![-1]), None, None);
}

/// A `RecurStart`/`RecurEnd` where the CFS has no recur site is refused.
#[test]
#[should_panic(expected = "Step record kind does not match the CFS item kind")]
fn a_recur_start_at_a_plain_tile_is_rejected() {
    let cfs = produced_list_sweep_cfs();
    // `[1]` is the plain tile `produce`.
    verify_step_record_inputs(
        &cfs,
        &kind_at(recur_start_kind("produce"), vec![1]),
        Some(&FnInput {
            data: Vec::new(),
            values: Vec::new(),
            args: Vec::new(),
            storage: Default::default(),
        }),
        None,
    );
}

/// Ordering: the only way into a site is its `RecurStart`, and the only way
/// out is its `RecurEnd`. Both used to be skippable.
#[test]
fn a_recur_site_cannot_be_entered_or_left_around_its_boundary_steps() {
    let cfs = produced_list_sweep_cfs();
    let after = |coordinates: Vec<CfsCoordinate>| {
        crate::checks::get_next_expected_coordinates(
            &cfs,
            &kind_at(exec_kind(ExecTarget::Tile("produce".into())), coordinates),
            None,
        )
    };
    // After the tile at `[1]`: only the site's `RecurStart` at `[2]`.
    assert_eq!(after(vec![1]), vec![CfsCoordinates(vec![2])]);

    let from_start = crate::checks::get_next_expected_coordinates(
        &cfs,
        &kind_at(recur_start_kind("collect"), vec![2]),
        None,
    );
    // First iteration, or zero iterations straight to the close — never past it.
    assert_eq!(
        from_start,
        vec![CfsCoordinates(vec![2, 1]), CfsCoordinates(vec![-2])]
    );
}

/// A sequence-scope witness must be the frame's `SequenceStart`. A
/// `RecurStart` at the same coordinates — in the trace, carrying the same
/// arguments — is refused: a site's inputs bind its iterations through the
/// iteration `Start`s, never directly.
#[test]
#[should_panic(expected = "no SequenceStart witness for frame")]
fn a_sequence_scope_witness_naming_a_recur_start_is_rejected() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));
    let StepKind::SequenceStart {
        input_source_commitment,
        ..
    } = parent_record.kind.clone()
    else {
        unreachable!()
    };
    let recur_start = StepRecord {
        kind: StepKind::RecurStart(raster_core::trace::RecurStartStep {
            site_id: "sub".into(),
            input_commitment: Vec::new(),
            input_source_commitment,
            storage: StorageRoots {
                root_before: Vec::new(),
                root_after: Vec::new(),
                index_root_before: Vec::new(),
                index_root_after: Vec::new(),
            },
        }),
        ..parent_record
    };
    let (trace_root, witness) = trace_root_and_witness(&[recur_start.clone()], 0);
    let parent = (recur_start, witness);
    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        Some(&parent),
        &trace_root,
    );
}

/// A sequence-scope witness must open *this* step's frame. Another frame's
/// `SequenceStart` — in the trace, committing the same arguments — is refused:
/// the host used to send a map the guest filtered by coordinates, and the
/// filter is now an assertion on the one parent it sends.
#[test]
#[should_panic(expected = "no SequenceStart witness for frame")]
fn a_sequence_scope_witness_opening_another_frame_is_rejected() {
    let cfs_cursor = scope_binding_cfs();
    let (step_record, _step_source, parent_record, parent_args) =
        scope_binding_scenario(CfsCoordinates(vec![10, 10]), sha(b"the-caller-passed-this"));
    let other_frame = StepRecord {
        coordinates: CfsCoordinates(vec![2]),
        ..parent_record
    };
    let (trace_root, witness) = trace_root_and_witness(&[other_frame.clone()], 0);
    let parent = (other_frame, witness);
    verify_sequence_scope_parent(
        &cfs_cursor,
        &step_record,
        Some(&parent_args),
        Some(&parent),
        &trace_root,
    );
}

/// A site's close names its enclosing sequence, like every non-boundary
/// step; naming the site itself is refused.
#[test]
#[should_panic(expected = "but its kind and coordinates determine")]
fn a_recur_end_naming_the_site_as_its_sequence_is_rejected() {
    verify_sequence_id(
        &recur_frames_cfs(),
        &step_with_sequence_id(recur_end_kind("recur"), vec![-1], "recur"),
    );
}

/// Chunking left the CFS binding check (`control-flow-prover` step 2): an
/// iteration of a chunked site passes it with no replay journal at all, and
/// the rule is enforced by `verify_recur_iteration_chunking` alone.
#[test]
fn verify_step_record_inputs_leaves_chunking_to_the_recur_call_site() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    verify_step_record_inputs(&cfs_cursor, &recur_iteration_step(0), None, None);
}

fn boundary_start_kind() -> StepKind {
    StepKind::SequenceStart {
        input_commitment: Vec::new(),
        input_source_commitment: Vec::new(),
    }
}

fn call_item(id: &str, sources: Vec<InputBinding>) -> SequenceChildItem {
    SequenceChildItem::Sequence(SequenceItem {
        id: id.into(),
        sources,
    })
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
                output: None,
                state_is_output: false,
                carries_state: false,
            })],
            entry_arguments: Vec::new(),
            produces_output: false,
            returns: None,
        }],
    })
}

// ---------------------------------------------------------------------------
// A recur-sequence body reading its item
//
// The iteration `Start` records `RecurSequenceInput` as an inline handle; the
// body tile that reads the item records the storage binding itself. The scope
// check looks through the handle.
// ---------------------------------------------------------------------------

/// `decorate` at `[1, index + 1, 1]` reading the element `item_of` stored
/// under its parameter.
fn decorate_reading(index: u64, item_of: u64) -> (StepRecord, FnInput) {
    let (_, iteration_input, _) = recur_sequence_iteration(item_of);
    let item = iteration_input.storage["line"].clone();
    let step = StepRecord {
        exec_index: 20 + index,
        sequence_id: "decorate_lines".into(),
        coordinates: CfsCoordinates(vec![
            1,
            index as CfsCoordinate + FIRST_COORDINATE,
            FIRST_COORDINATE,
        ]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("decorate".into()),
            intra_sequence_index: FIRST_COORDINATE,
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
    let input = FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::StorageBinding],
        args: vec![FnInputArg {
            name: "line".to_string(),
            ty: "AuthRef<String>".to_string(),
        }],
        storage: [("line".to_string(), item)].into_iter().collect(),
    };
    (step, input)
}

fn dummy_storage_roots() -> StorageRoots {
    StorageRoots {
        root_before: EMPTY_LEAF.to_vec(),
        root_after: EMPTY_LEAF.to_vec(),
        index_root_before: Vec::new(),
        index_root_after: Vec::new(),
    }
}

/// A step at trace index `t` carries `exec_index == t + 1`.
fn exec_index_fixture(exec_index: u64) -> StepRecord {
    StepRecord {
        exec_index,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![2]),
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

fn exec_kind(target: ExecTarget) -> StepKind {
    StepKind::Exec(ExecStep {
        target,
        intra_sequence_index: FIRST_COORDINATE,
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

fn field_path(names: &[&str]) -> Vec<SelectorSegment> {
    names.iter().map(|name| SelectorSegment::Field(name.to_string())).collect()
}

// ---------------------------------------------------------------------------
// Recur site step kinds and `[-s]` closes — `incremental-draft-materialization`
// §A recur site gets its own step kinds (batch A).
// ---------------------------------------------------------------------------

fn kind_at(kind: StepKind, coordinates: Vec<CfsCoordinate>) -> StepRecord {
    step_with_sequence_id(kind, coordinates, "main")
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

// ---------------------------------------------------------------------------
// The site `Start`'s `L` — `tile-io-structural-roots.md` §Step 1, "Found, not
// fixed".
// ---------------------------------------------------------------------------

/// `main`: a tile at `[1]` producing a list, and a chunked recur tile site at
/// `[2]` sweeping it.
fn produced_list_sweep_cfs() -> CfsCursor {
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("produce", 0, 1), TileDef::iter("collect", 1, 1)],
        sequences: vec![SequenceDef {
            id: "main".into(),
            input_sources: vec![],
            items: vec![
                SequenceChildItem::Tile(TileItem {
                    id: "produce".into(),
                    sources: vec![],
                }),
                SequenceChildItem::RecurTile(RecurTileItem {
                    id: "collect".into(),
                    sources: vec![InputBinding::prior_item_output(0)],
                    chunk: Some(2),
                    output: None,
                    state_is_output: false,
                    carries_state: false,
                }),
            ],
            entry_arguments: Vec::new(),
            produces_output: false,
            returns: None,
        }],
    })
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
                            path: Vec::new(),
                        }],
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
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
                returns: None,
            },
        ],
    })
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

fn recur_end_kind(site: &str) -> StepKind {
    StepKind::RecurEnd(raster_core::trace::RecurEndStep {
        site_id: site.into(),
        output_commitment: Vec::new(),
        storage: empty_roots(),
    })
}

/// A recur **tile** site at `[1]` and a recur **sequence** site at `[2]`,
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
                        output: None,
                        state_is_output: false,
                        carries_state: false,
                    }),
                    SequenceChildItem::RecurSequence(RecurSequenceItem {
                        id: "child".into(),
                        sources: vec![InputBinding::Direct(InputSource::Inline)],
                        state_is_output: false,
                        carries_state: false,
                        output: None,
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
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
                returns: None,
            },
        ],
    })
}

fn recur_iteration_step(iteration: CfsCoordinate) -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![FIRST_COORDINATE, iteration + FIRST_COORDINATE]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("collect".into()),
            intra_sequence_index: FIRST_COORDINATE,
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

/// What iteration `Start` `[1, index + 1]` records: the body's `input`
/// parameter as an inline marker, the item's storage data under its name, and
/// a reference witness proving element `index`.
fn recur_sequence_iteration(index: u64) -> (StepRecord, FnInput, BTreeMap<String, SelectionWitness>) {
    let (root, elements) = swept_lines();
    let path = raster_core::input::SelectorPath::new(vec![SelectorSegment::Index(index)]);
    let (direction, sibling) = if index == 0 {
        (raster_core::input::ListProofDirection::Right, elements[1])
    } else {
        (raster_core::input::ListProofDirection::Left, elements[0])
    };
    let witness = SelectionWitness {
        bytes: Vec::new(),
        proof: raster_core::input::SelectionProof {
            path: path.clone(),
            root_hash: root,
            steps: vec![raster_core::input::SelectionProofStep::List {
                index,
                len: 2,
                siblings: vec![raster_core::input::ListProofSibling {
                    direction,
                    hash: sibling,
                }],
            }],
        },
        selected_root: Some(elements[index as usize]),
    };
    let source = swept_lines_source();
    let item = StorageData {
        selector: path.clone(),
        selection: raster_core::input::SelectionCommitment {
            path,
            selected_len: 1,
            ..source.selection.clone()
        },
        ..source
    };
    let input = FnInput {
        data: Vec::new(),
        values: vec![
            FnInputValue::Inline(recur_sequence_input_marker(index, FnInputValue::StorageBinding)),
            FnInputValue::Inline(b"draft-handle".to_vec()),
        ],
        args: vec![
            FnInputArg {
                name: "line".to_string(),
                ty: "RecurSequenceInput<String>".to_string(),
            },
            FnInputArg {
                name: "output".to_string(),
                ty: "RecurSequenceOutput<CollectiveGreeting>".to_string(),
            },
        ],
        storage: [("line".to_string(), item)].into_iter().collect(),
    };
    let step = StepRecord {
        exec_index: 10 + index,
        sequence_id: "decorate_lines".into(),
        coordinates: CfsCoordinates(vec![1, index as CfsCoordinate + FIRST_COORDINATE]),
        kind: StepKind::SequenceStart {
            input_commitment: Vec::new(),
            input_source_commitment: Vec::new(),
        },
        recur_progress_commitment: [0u8; 32],
        recur_state: None,
    };
    (step, input, [("line".to_string(), witness)].into_iter().collect())
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
                    carries_state: false,
                    output: None,
                })],
                entry_arguments: Vec::new(),
                produces_output: false,
                returns: None,
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
                returns: None,
            },
        ],
    })
}

/// The site step the recorder writes for that call: `RecurSequenceStart`
/// becomes a `SequenceStart` at the site coordinate `[1]`, carrying the site's
/// own id (`recorder.rs`'s `RecurTileStart | RecurSequenceStart` arm).
fn recur_sequence_site_step() -> StepRecord {
    StepRecord {
        exec_index: 1,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![1]),
        kind: StepKind::RecurStart(raster_core::trace::RecurStartStep {
            site_id: "decorate_lines".into(),
            input_commitment: sha(b"recur-seq-in"),
            input_source_commitment: Vec::new(),
            storage: empty_roots(),
        }),
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
                coordinates: CfsCoordinates(vec![1, 1]),
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

fn recur_start_kind(site: &str) -> StepKind {
    StepKind::RecurStart(raster_core::trace::RecurStartStep {
        site_id: site.into(),
        input_commitment: Vec::new(),
        input_source_commitment: Vec::new(),
        storage: empty_roots(),
    })
}

/// `witness` with its argument names replaced and nothing else touched: the
/// same `values` and `storage`, pointed at differently.
fn renaming_args(mut witness: FnInput, names: &[&str]) -> FnInput {
    assert_eq!(witness.args.len(), names.len(), "one name per argument");
    for (arg, name) in witness.args.iter_mut().zip(names) {
        arg.name = name.to_string();
    }
    witness
}

/// `sub`'s item reads `sub`'s own parameter 0 — a `SequenceScope` binding at a
/// *nested* frame, which is the shape the compiler actually emits. (`main`'s
/// parameters resolve to `EntryArgument`; a scope binding at the root frame is
/// unreachable.)
fn scope_binding_cfs() -> CfsCursor {
    scope_binding_cfs_reading(InputBinding::seq_input(0))
}

/// [`scope_binding_cfs`] with the consumer's binding replaced — a parameter
/// narrowed by a path, say.
fn scope_binding_cfs_reading(consumer_source: InputBinding) -> CfsCursor {
    scope_binding_cfs_reading_all(vec![consumer_source])
}

/// The step at `[1, 1]` reading `sub`'s parameter 0, its own source witness,
/// and the parent `SequenceStart` at `[1]` carrying `parent_args`.
fn scope_binding_scenario(
    read_from: CfsCoordinates,
    commitment: Vec<u8>,
) -> (StepRecord, FnInput, StepRecord, FnInput) {
    let step_record = StepRecord {
        exec_index: 2,
        sequence_id: "sub".into(),
        coordinates: CfsCoordinates(vec![1, 1]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("consumer".into()),
            intra_sequence_index: FIRST_COORDINATE,
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
        coordinates: CfsCoordinates(vec![1]),
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

/// A storage binding with a *real* selection, unlike `storage_input_witness`
/// whose `selected_len` is 0 and so never reaches the selection branch at all.
/// Returns the input witness plus both witness shapes for it.
fn selecting_input_witness(
    binding: &str,
    coordinates: CfsCoordinates,
) -> (
    FnInput,
    Vec<u8>,
    raster_core::input::SelectionWitness,
    raster_core::input::SelectionWitness,
) {
    use raster_core::input::{
        encode_index_leaf_payload, selected_subtree_root, selection_payload_hash, IndexWidth,
        SelectionProof, SelectionWitness,
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

fn site_start_claiming(
    commitment: Hash32,
    len: u64,
) -> (StepRecord, FnInput, BTreeMap<String, SelectionWitness>) {
    let mut metadata = vec![0x0A];
    metadata.extend_from_slice(&len.to_le_bytes());
    site_start_reading(commitment, metadata)
}

fn step_with_sequence_id(
    kind: StepKind,
    coordinates: Vec<CfsCoordinate>,
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

fn storage_input_witness(coordinates: CfsCoordinates, commitment: Vec<u8>) -> FnInput {
    storage_inputs_witness(&[("arg", coordinates, commitment)])
}

/// `sub = [producer, other]`, returning `other`'s output.
fn sub_returning_other() -> SequenceDef {
    SequenceDef {
        id: "sub".into(),
        input_sources: vec![],
        items: vec![
            tile_item("producer", vec![InputBinding::Direct(InputSource::Inline)]),
            tile_item("other", vec![InputBinding::Direct(InputSource::Inline)]),
        ],
        entry_arguments: vec![],
        produces_output: false,
        returns: Some(raster_core::cfs::SequenceReturn {
            source: InputBinding::prior_item_output(1),
            path: vec![],
        }),
    }
}

fn tile_item(id: &str, sources: Vec<InputBinding>) -> SequenceChildItem {
    SequenceChildItem::Tile(TileItem {
        id: id.into(),
        sources,
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
        tree.append(Bytes(guest_common::merkle_tree::hash_trace_item(record)));
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
            path_elems: path.iter().map(|elem| to_node_hash(&elem.0)).collect(),
        },
    )
}

/// The consumer at `[1, 1]` reading `reads` as its arguments, in order.
fn two_parameter_consumer(reads: &[(&str, &str)]) -> (StepRecord, FnInput) {
    let object = |label: &str| match label {
        "A" => (CfsCoordinates(vec![10, 10]), sha(b"object-a")),
        "B" => (CfsCoordinates(vec![10, 11]), sha(b"object-b")),
        other => panic!("unknown object {other}"),
    };
    let bindings: Vec<(&str, CfsCoordinates, Vec<u8>)> = reads
        .iter()
        .map(|(name, label)| {
            let (coordinates, commitment) = object(label);
            (*name, coordinates, commitment)
        })
        .collect();
    let step_source = storage_inputs_witness(&bindings);
    let step_record = StepRecord {
        exec_index: 2,
        sequence_id: "sub".into(),
        coordinates: CfsCoordinates(vec![1, 1]),
        kind: StepKind::Exec(ExecStep {
            target: ExecTarget::Tile("consumer".into()),
            intra_sequence_index: FIRST_COORDINATE,
            input_commitment: Vec::new(),
            input_source_commitment: input_source_commitment(&step_source),
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
    (step_record, step_source)
}

/// `sub(p, q)` called with two stored objects `A` and `B`, and its consumer
/// declared `[SequenceScope{0}, SequenceScope{1}]`, so it must read `(A, B)`.
///
/// Returns the CFS, the parent `SequenceStart` at `[1]` committing the honest
/// arguments `[p, q]`, those arguments, the trace root after the parent, and
/// the parent record with its inclusion witness against that root.
fn two_parameter_scope_scenario() -> (
    CfsCursor,
    StepRecord,
    FnInput,
    Vec<u8>,
    (StepRecord, raster_core::transition::StepRecordWitness),
) {
    let cfs_cursor = scope_binding_cfs_reading_all(vec![
        InputBinding::seq_input(0),
        InputBinding::seq_input(1),
    ]);
    let parent_args = storage_inputs_witness(&[
        ("p", CfsCoordinates(vec![10, 10]), sha(b"object-a")),
        ("q", CfsCoordinates(vec![10, 11]), sha(b"object-b")),
    ]);
    let parent_record = StepRecord {
        exec_index: 1,
        sequence_id: "sub".into(),
        coordinates: CfsCoordinates(vec![1]),
        kind: StepKind::SequenceStart {
            input_commitment: Vec::new(),
            input_source_commitment: input_source_commitment(&parent_args),
        },
        recur_progress_commitment: RecurProgressStack::new().commitment(),
        recur_state: None,
    };
    let (trace_root, witness) = trace_root_and_witness(&[parent_record.clone()], 0);
    let parent = (parent_record.clone(), witness);
    (cfs_cursor, parent_record, parent_args, trace_root, parent)
}

/// `main = [<items>…, consumer(item k)]` over callee sequences `defs`; the
/// consumer at `consumer_at` reads `input_coordinates`.
fn verify_consumer_input(
    main_items: Vec<SequenceChildItem>,
    defs: Vec<SequenceDef>,
    source_index: usize,
    input_coordinates: Vec<CfsCoordinate>,
) {
    let mut items = main_items;
    items.push(SequenceChildItem::Tile(TileItem {
        id: "consumer".into(),
        sources: vec![InputBinding::prior_item_output(source_index)],
    }));
    let consumer_at = items.len() as CfsCoordinate;
    let mut sequences = vec![SequenceDef {
        id: "main".into(),
        input_sources: vec![],
        items,
        entry_arguments: vec![],
        produces_output: false,
        returns: None,
    }];
    sequences.extend(defs);
    let cfs_cursor = CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![
            TileDef::iter("producer", 0, 1),
            TileDef::iter("other", 0, 1),
            TileDef::iter("consumer", 1, 1),
        ],
        sequences,
    });
    let mut step_record = exec_index_fixture(1);
    step_record.coordinates = CfsCoordinates(vec![consumer_at]);
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&storage_input_witness(
            CfsCoordinates(input_coordinates),
            sha(b"source-output"),
        )),
        None,
    );
}

/// `main = [consumer(entry argument)]`, the consumer reading `read`.
fn verify_entry_argument_read(declared: &[&str], read: &[&str]) {
    let cfs_cursor = CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("consumer", 1, 1)],
        sequences: vec![SequenceDef {
            id: "main".into(),
            input_sources: vec![],
            items: vec![tile_item(
                "consumer",
                vec![InputBinding::entry_argument().with_path(field_path(declared))],
            )],
            entry_arguments: vec![],
            produces_output: false,
            returns: None,
        }],
    });
    let mut step_record = exec_index_fixture(1);
    step_record.coordinates = CfsCoordinates(vec![1]);
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&selecting(
            storage_input_witness(CfsCoordinates(vec![]), sha(b"entry-object")),
            field_path(read),
        )),
        None,
    );
}

/// `sub`'s consumer reads `sub`'s parameter 0 narrowed by `declared` — the
/// shape of `greet_sequence` in `examples/hello-tiles`
/// (`select!(String, personal_data.name)` on its own parameter). The caller
/// passed the entry object's `caller` part; the step recorded a read of
/// `read` from an object with commitment `read_from`.
fn verify_narrowed_scope_read(declared: &[&str], caller: &[&str], read: &[&str], read_from: &[u8]) {
    let cfs_cursor = scope_binding_cfs_reading(InputBinding::seq_input(0).with_path(field_path(declared)));
    let (step_record, _, _, _) =
        scope_binding_scenario(CfsCoordinates(vec![]), sha(b"personal-data"));
    let caller_argument = selecting(
        storage_input_witness(CfsCoordinates(vec![]), sha(b"personal-data")),
        field_path(caller),
    );
    let step_source = selecting(
        storage_input_witness(CfsCoordinates(vec![]), sha(read_from)),
        field_path(read),
    );
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&step_source),
        Some(&caller_argument),
    );
}

/// `main = [producer, consumer(select!(produced.<declared>))]`, the consumer
/// reading `read` of the producer's output at `[1]`.
fn verify_prior_item_read(declared: &[&str], read: &[&str]) {
    let cfs_cursor = CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("producer", 0, 1), TileDef::iter("consumer", 1, 1)],
        sequences: vec![SequenceDef {
            id: "main".into(),
            input_sources: vec![],
            items: vec![
                tile_item("producer", vec![]),
                tile_item(
                    "consumer",
                    vec![InputBinding::prior_item_output(0).with_path(field_path(declared))],
                ),
            ],
            entry_arguments: vec![],
            produces_output: false,
            returns: None,
        }],
    });
    let mut step_record = exec_index_fixture(1);
    step_record.coordinates = CfsCoordinates(vec![2]);
    verify_step_record_inputs(
        &cfs_cursor,
        &step_record,
        Some(&selecting(
            storage_input_witness(CfsCoordinates(vec![1]), sha(b"producer-output")),
            field_path(read),
        )),
        None,
    );
}

fn empty_roots() -> StorageRoots {
    StorageRoots {
        root_before: Vec::new(),
        root_after: Vec::new(),
        index_root_before: Vec::new(),
        index_root_after: Vec::new(),
    }
}

/// The bytes `RecurSequenceInput` records for itself (`raster::input`'s
/// `RecurSequenceInputTraceMarker`).
fn recur_sequence_input_marker(index: u64, item: FnInputValue) -> Vec<u8> {
    #[derive(serde::Serialize)]
    struct Marker {
        kind: &'static str,
        index: u64,
        len: u64,
        item: FnInputValue,
    }
    postcard::to_allocvec(&Marker {
        kind: "raster::RecurSequenceInput",
        index,
        len: 2,
        item,
    })
    .unwrap()
}

/// [`scope_binding_cfs`] with `sub` taking one parameter per consumer
/// binding, and the consumer declaring `consumer_sources` in order.
fn scope_binding_cfs_reading_all(consumer_sources: Vec<InputBinding>) -> CfsCursor {
    let parameters = vec![InputBinding::Direct(InputSource::Inline); consumer_sources.len()];
    CfsCursor::new(ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![TileDef::iter("consumer", consumer_sources.len(), 1)],
        sequences: vec![
            SequenceDef {
                id: "main".into(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Sequence(SequenceItem {
                    id: "sub".into(),
                    sources: parameters.clone(),
                })],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
            },
            SequenceDef {
                id: "sub".into(),
                input_sources: parameters,
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "consumer".into(),
                    sources: consumer_sources,
                })],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
            },
        ],
    })
}

/// `witness` with every storage argument read through `path` — the recorded
/// `selector` and the `selection.path` its proof is pinned to, which agree on
/// every honest run.
fn selecting(mut witness: FnInput, path: Vec<SelectorSegment>) -> FnInput {
    for data in witness.storage.values_mut() {
        data.selector = raster_core::input::SelectorPath::new(path.clone());
        data.selection.path = raster_core::input::SelectorPath::new(path.clone());
    }
    witness
}

/// A site `Start` at `[2]` whose `"input"` names `[1]` — the right producer —
/// with any commitment and any `0x0A` metadata bytes.
fn site_start_reading(
    commitment: Hash32,
    metadata: Vec<u8>,
) -> (StepRecord, FnInput, BTreeMap<String, SelectionWitness>) {
    let input = FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::StorageBinding],
        args: vec![FnInputArg {
            name: "input".to_string(),
            ty: "AuthRef<List<String>>".to_string(),
        }],
        storage: [(
            "input".to_string(),
            StorageData {
                coordinates: CfsCoordinates(vec![1]),
                commitment: commitment.to_vec(),
                selector: Default::default(),
                selection: raster_core::input::SelectionCommitment {
                    source_root_hash: commitment,
                    selected_hash: raster_core::input::selection_payload_hash(&metadata),
                    selected_len: metadata.len() as u64,
                    payload_kind: raster_core::input::SelectionPayloadKind::List,
                    ..Default::default()
                },
            },
        )]
        .into_iter()
        .collect(),
    };
    let witness = SelectionWitness {
        bytes: metadata,
        proof: raster_core::input::SelectionProof {
            path: Default::default(),
            root_hash: commitment,
            steps: Vec::new(),
        },
        selected_root: None,
    };
    let step = StepRecord {
        exec_index: 2,
        sequence_id: "main".into(),
        coordinates: CfsCoordinates(vec![2]),
        kind: StepKind::RecurStart(raster_core::trace::RecurStartStep {
            site_id: "collect".into(),
            input_commitment: Vec::new(),
            input_source_commitment: input_source_commitment(&input),
            storage: empty_roots(),
        }),
        recur_progress_commitment: [0u8; 32],
        recur_state: None,
    };
    (step, input, [("input".to_string(), witness)].into_iter().collect())
}

/// A witness whose argument `i` is the storage binding `bindings[i]`, named
/// `bindings[i].0`.
fn storage_inputs_witness(bindings: &[(&str, CfsCoordinates, Vec<u8>)]) -> FnInput {
    FnInput {
        data: Vec::new(),
        values: vec![FnInputValue::StorageBinding; bindings.len()],
        args: bindings
            .iter()
            .map(|(name, _, _)| FnInputArg {
                name: name.to_string(),
                ty: "Vec<u8>".to_string(),
            })
            .collect(),
        storage: bindings
            .iter()
            .map(|(name, coordinates, commitment)| {
                // A whole-object storage binding's commitment *is* its raster
                // selection root (see `checks::store`'s structural-consistency
                // assertion), so the fixture mirrors that invariant.
                let source_root_hash: [u8; 32] = commitment
                    .clone()
                    .try_into()
                    .expect("test commitments are 32 bytes");
                (
                    name.to_string(),
                    StorageData {
                        coordinates: coordinates.clone(),
                        commitment: commitment.clone(),
                        selector: Default::default(),
                        selection: raster_core::input::SelectionCommitment {
                            source_root_hash,
                            ..Default::default()
                        },
                    },
                )
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Rule 8 for recur-sequence iterations
//
// An iteration `Start` is a sequence boundary: no storage roots, so
// `checks::store` folds none of its witnesses. The item check verifies the one
// it reads, then holds it to the site's source and the sweep position.
// ---------------------------------------------------------------------------

/// A real two-element `List<String>` stored whole at `[9]`: its commitment,
/// and the element roots its proofs fold from.
fn swept_lines() -> (Hash32, [Hash32; 2]) {
    let root_of = |value: &str| {
        raster_core::tree::subtree_payload_and_root(
            &raster_core::tree::tree_value_from_serialize(&value.to_string()).unwrap(),
        )
        .unwrap()
        .1
    };
    let elements = [root_of("first"), root_of("second")];
    let root = raster_core::tree::list_root_from_hashes(&elements);
    (root, elements)
}

fn swept_lines_source() -> StorageData {
    let (root, _) = swept_lines();
    StorageData {
        coordinates: CfsCoordinates(vec![9]),
        commitment: root.to_vec(),
        selector: Default::default(),
        selection: raster_core::input::SelectionCommitment {
            source_root_hash: root,
            ..Default::default()
        },
    }
}
