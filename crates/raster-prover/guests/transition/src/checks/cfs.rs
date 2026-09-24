//! Checks that a step record matches the control flow schema: that the
//! record's kind matches the item declared at its coordinates, that its
//! per-argument input bindings are honoured, and that its coordinates
//! follow the schema's ordering.

use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, InputBinding, InputSource, SequenceChildItem,
    FIRST_COORDINATE,
};
use raster_core::input::SelectorSegment;
use raster_core::transition::StepRecordWitness;
use std::collections::{BTreeMap, HashMap};

use crate::checks::io::input_source_commitment;
use crate::merkle_tree::{combine_merkle_level, hash_trace_item};

use raster_core::draft::TileReplayJournal;
use raster_core::input::{SelectionPayloadKind, SelectionWitness};
use raster_core::recur_progress::{RecurProgressStack, RecurSiteKind};
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, FnInputValue, StepKind, StepRecord, StorageData,
};

enum ResolvedSource<'a> {
    Inline(&'a Vec<u8>),
    Storage(&'a StorageData),
}

fn resolved_source_at<'a>(input: &'a FnInput, index: usize) -> ResolvedSource<'a> {
    let arg = input
        .args()
        .get(index)
        .unwrap_or_else(|| panic!("Missing input arg metadata at index {}", index));
    let value = input
        .values()
        .get(index)
        .unwrap_or_else(|| panic!("Missing input source value at index {}", index));

    match value {
        FnInputValue::Inline(bytes) => ResolvedSource::Inline(bytes),
        FnInputValue::StorageBinding => ResolvedSource::Storage(
            input
                .storage()
                .get(&arg.name)
                .unwrap_or_else(|| panic!("Missing storage input metadata for arg '{}'", arg.name)),
        ),
    }
}

fn assert_same_source(left: ResolvedSource<'_>, right: ResolvedSource<'_>) {
    match (left, right) {
        (ResolvedSource::Inline(left_bytes), ResolvedSource::Inline(right_bytes)) => {
            assert_eq!(
                left_bytes, right_bytes,
                "Inline sequence scope input does not match consumer binding",
            );
        }
        (ResolvedSource::Storage(left_meta), ResolvedSource::Storage(right_meta)) => {
            assert_eq!(
                left_meta, right_meta,
                "Storage sequence scope input does not match consumer binding",
            );
        }
        _ => {
            panic!("Sequence scope source kind does not match consumer binding");
        }
    }
}

fn has_coordinate_prefix(coordinates: &CfsCoordinates, prefix: &CfsCoordinates) -> bool {
    coordinates.len() >= prefix.len()
        && coordinates
            .iter()
            .zip(prefix.iter())
            .all(|(coordinate, expected)| coordinate == expected)
}

/// Whether a step record of this kind may occupy a CFS item of this kind.
///
/// Input bindings alone cannot tell these apart, and the kinds differ in how
/// their output is verified — a tile's by replay proof. Without this, a
/// record could take a coordinate whose verification rules are weaker than
/// its own. The program-boundary steps (`ProgramStart` and `main`'s
/// `SequenceEnd`) never reach this check: they sit at the sequence root,
/// which is not a CFS item.
fn record_matches_item(step_record: &StepRecord, cfs_item: &SequenceChildItem) -> bool {
    // The trace carries names (fingerprinted, so tamper-evident) but the guest
    // must also *bind* them: the recorded target name must equal the CFS item
    // id at the step's coordinates. Without this the coordinate → tile-id
    // resolution the registry lookup relies on could be steered by a
    // mislabelled record. See program-identity.md.
    match (&step_record.kind, cfs_item) {
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(name),
                ..
            }),
            SequenceChildItem::Tile(item),
        ) => name == &item.id,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurTile(name),
                ..
            }),
            SequenceChildItem::RecurTile(item),
        ) => name == &item.id,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurSequence(name),
                ..
            }),
            SequenceChildItem::RecurSequence(item),
        ) => name == &item.id,
        // A nested sequence is entered and left at its own item coordinate,
        // whether it is an ordinary or a recur sequence. The entered
        // sequence's name is carried on the step record.
        (
            StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. },
            SequenceChildItem::Sequence(item),
        ) => step_record.sequence_id == item.id,
        (
            StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. },
            SequenceChildItem::RecurSequence(item),
        ) => step_record.sequence_id == item.id,
        // A recur *tile* site opens with a boundary step too: `RecurTileStart`
        // becomes `SequenceStart` at `[s]`, carrying the site's own id so it
        // binds to the item the same way a sequence's boundary steps do. Its
        // `End` half stays `Exec(RecurTile)`, matched above.
        (StepKind::SequenceStart { .. }, SequenceChildItem::RecurTile(item)) => {
            step_record.sequence_id == item.id
        }
        _ => false,
    }
}

/// For an iteration of a recur site with a CFS-declared chunk size, verify the
/// iteration consumed a chunk of `1..=declared` elements.
///
/// The count comes from the iteration's **replay journal** — `consumed_elements`
/// — rather than from inspecting the leading postcard varint of its ABI bytes.
/// The varint trick worked only because a chunked tile's first argument happened
/// to be `RecurInput<Vec<T>>`, whose first field is the chunk vector; it was a
/// layout assumption about user types, and it could not read anything else the
/// audit needs (an *unchunked* iteration's index, or how the tile terminated).
/// The tile now commits the number directly and the replay receipt covers it.
/// See `docs/proposals/lazy-list-recur.md` §5.
fn verify_recur_iteration_chunking(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    site_coordinates: &CfsCoordinates,
    replay_journal: Option<&TileReplayJournal>,
) {
    let Some(raster_core::cfs::SequenceChildItem::RecurTile(item)) =
        cfs_cursor.try_get_item(site_coordinates)
    else {
        return;
    };
    let Some(declared) = item.chunk else {
        return;
    };

    let recur = replay_journal
        .and_then(|journal| journal.recur.as_ref())
        .unwrap_or_else(|| {
            panic!(
                "Chunked recur iteration {:?} is missing its replay-proven recur facts",
                step_record
            )
        });
    let consumed = recur.position.consumed_elements;
    if let Err(violation) = raster_core::chunking::check_iteration_chunk_len(declared, consumed) {
        panic!(
            "Recur chunking violation at step {:?}: {}",
            step_record, violation
        );
    }

    // Only the *final* chunk may be short. The native recorder used to enforce
    // this by remembering the previous iteration's length; the journal makes it
    // a stateless, per-iteration fact, because `declared_iterations` already
    // says whether this iteration is the last one. Same rule, replay-proven
    // instead of host-remembered — and it is what rejects a `4,1,4,1` shape at
    // `C = 4` on the iteration that goes short, rather than on the one after.
    let is_final_iteration =
        recur.position.iteration_index + 1 >= recur.position.declared_iterations;
    if !is_final_iteration {
        if let Err(violation) =
            raster_core::chunking::check_previous_chunk_was_full(declared, consumed)
        {
            panic!(
                "Recur chunking violation at step {:?}: {}",
                step_record, violation
            );
        }
    }
}

pub fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    sequence_scope_witness: Option<&FnInput>,
    replay_journal: Option<&TileReplayJournal>,
) {
    // The program-boundary steps sit at the sequence root `[]`, which is not
    // itself a CFS item and binds no CFS inputs: `ProgramStart` binds
    // authorized external data and `ProgramEnd` commits the authorized output,
    // both checked in `checks::entrypoint` against storage/the journal rather
    // than against CFS input bindings.
    if step_record.coordinates().is_empty() {
        return;
    }

    if let Some((site_coordinates, _)) =
        cfs_cursor.try_get_recur_iteration_coordinates(step_record.coordinates())
    {
        verify_recur_iteration_chunking(cfs_cursor, step_record, &site_coordinates, replay_journal);
        return;
    }

    let cfs_item = cfs_cursor
        .try_get_item(step_record.coordinates())
        .unwrap_or_else(|| {
            panic!(
                "Failed to resolve cfs item for step record {:?}",
                step_record
            )
        });
    assert!(
        record_matches_item(step_record, cfs_item),
        "Step record kind does not match the CFS item kind at its coordinates: {:?}",
        step_record,
    );
    // A `SequenceEnd` reports what the sequence produced; the sequence's input
    // bindings belong to its `SequenceStart`, which was verified at the same
    // coordinates. Verifying them again here was not a second check but a
    // vacuous one: `StepRecord::input_source_commitment` is `None` for a
    // `SequenceEnd` (`trace.rs`), so nothing ties the witness to this record
    // and anything it "proved" could have been fabricated. It only ever ran
    // because the witness store is keyed by coordinates alone, so the End
    // inherited the Start's entry — the same sharing `recorder.rs` already
    // guards for `storage_write`. `checks::io` refuses that witness outright,
    // so requiring it here was also self-contradictory.
    //
    // Placed after `record_matches_item` so the step is still held to the CFS
    // item at its coordinates; only the input-binding half is skipped.
    if matches!(step_record.kind, StepKind::SequenceEnd { .. }) {
        return;
    }

    let step_inputs = cfs_item.inputs();

    let input_source_witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Missing input source witness for step record {:?}",
            step_record
        )
    });
    assert_eq!(
        step_inputs.len(),
        input_source_witness.values().len(),
        "CFS input count does not match input source witness arity",
    );

    let Some((parent_sequence_coordinates, item_coordinate)) =
        step_record.coordinates().try_parent()
    else {
        return;
    };

    for (input_index, step_input) in step_inputs.iter().enumerate() {
        let resolved_source = resolved_source_at(input_source_witness, input_index);
        verify_one_binding(
            step_input,
            resolved_source,
            input_index,
            step_record,
            cfs_cursor,
            input_source_witness,
            sequence_scope_witness,
            &parent_sequence_coordinates,
            item_coordinate,
        );
    }
}

/// Count the `BoundIndex` segments in a resolved source's selection path.
///
/// Zero for an inline source: an inline value has no path, so it can carry no
/// dynamic index.
fn bound_index_count(resolved_source: &ResolvedSource<'_>) -> usize {
    match resolved_source {
        ResolvedSource::Inline(_) => 0,
        ResolvedSource::Storage(meta) => meta
            .selection
            .path
            .segments
            .iter()
            .filter(|segment| matches!(segment, SelectorSegment::BoundIndex { .. }))
            .count(),
    }
}

/// The storage bindings a resolved source cites through its `BoundIndex`
/// segments, in selector order.
///
/// Resolving the citation here is what lets the CFS check reach the index's own
/// binding: the step's storage map is the only place the cited value appears.
fn cited_index_sources<'a>(
    resolved_source: &ResolvedSource<'a>,
    input_source_witness: &'a FnInput,
) -> Vec<&'a StorageData> {
    let ResolvedSource::Storage(meta) = resolved_source else {
        return Vec::new();
    };
    meta.selection
        .path
        .segments
        .iter()
        .filter_map(|segment| match segment {
            SelectorSegment::BoundIndex { source, .. } => {
                Some(input_source_witness.storage().get(source).unwrap_or_else(|| {
                    panic!("Bound index cites storage binding '{}', which the step does not record", source)
                }))
            }
            _ => None,
        })
        .collect()
}

/// Hold a step to the `exec_index` its position in the trace determines.
///
/// **Why this is a soundness check, not a sanity check.** The trace leaf is
/// `sha256(postcard(StepRecord))` over the *whole* record, so every field
/// reaches the leaf, the trace root, and the fingerprint entry. A field that
/// reaches the leaf but is verified by nothing is free entropy: take an honest
/// window, change only that field on the last item, and the earlier items still
/// match the commitment while the last one "diverges" — which is exactly
/// `finalize`'s `Finished` condition. That forges a fraud receipt against an
/// honest prover, with no short window and no fabricated frontier. The window's
/// margin pins the *opening state*; it has never pinned the *diverging item*.
///
/// `exec_index` was such a field. It is fully determined by position:
/// `TraceRecorder::new` starts the counter at 0 and `record` increments before
/// use, and the single production call site pushes every returned record
/// unconditionally (`raster-cli::commands::run::record_trace_event`), so the
/// step at trace index `t` carries `exec_index == t + 1`.
///
/// `trace_position` is the frontier's position *before* this step is appended.
/// The window's initial frontier holds the seed plus one leaf per pre-window
/// step, so its position is the trace index of the window's first item, and it
/// advances in step with the walk — it is the trace index, not a window offset.
pub fn verify_exec_index(trace_position: u64, step_record: &StepRecord) {
    let expected = trace_position + 1;
    assert_eq!(
        step_record.exec_index, expected,
        "Step at trace index {} carries exec_index {} but its position determines {}",
        trace_position, step_record.exec_index, expected,
    );
}

/// The entrypoint sequence's id. A literal here for the same reason it is one
/// in `CfsCursor::new` ("Missing main entrypoint") and in the recorder, which
/// pushes `main`'s frame by name at `ProgramStart`.
const MAIN_SEQUENCE_ID: &str = "main";

/// The sequence declared *at* `coordinates` — what a boundary step names.
///
/// `try_get_item` already folds a recur *iteration* coordinate to its site, so
/// `RecurSequenceIterationStart`/`End` at `site ++ [i]` resolve to the site's
/// own item, which is exactly the sequence they enter and leave.
fn declared_sequence_id<'a>(
    cfs_cursor: &'a CfsCursor,
    coordinates: &CfsCoordinates,
) -> Option<&'a str> {
    if coordinates.is_empty() {
        // Only `main`'s own boundary sits at the root coordinate.
        return Some(MAIN_SEQUENCE_ID);
    }
    match cfs_cursor.try_get_item(coordinates)? {
        SequenceChildItem::Sequence(item) => Some(item.id.as_str()),
        SequenceChildItem::RecurSequence(item) => Some(item.id.as_str()),
        SequenceChildItem::RecurTile(item) => Some(item.id.as_str()),
        // A tile is not a frame, so no boundary step can sit at one.
        SequenceChildItem::Tile(_) => None,
    }
}

/// The sequence frame `coordinates` execute *in* — what every non-boundary step
/// names.
///
/// Walks outward rather than resolving in one step, because the two recur kinds
/// differ: a recur **sequence** pushes a frame, so its body names the site; a
/// recur **tile** pushes none, so its iterations stay in the sequence that
/// contains the site. Pinned by `sequence_id_names_the_callee_at_boundaries_and_
/// the_frame_everywhere_else` in the recorder's tests.
fn enclosing_sequence_id<'a>(
    cfs_cursor: &'a CfsCursor,
    coordinates: &CfsCoordinates,
) -> &'a str {
    let mut frame = coordinates.clone();
    loop {
        let Some((parent, _)) = frame.try_parent() else {
            return MAIN_SEQUENCE_ID;
        };
        if parent.is_empty() {
            return MAIN_SEQUENCE_ID;
        }
        match cfs_cursor.try_get_item(&parent) {
            // A nested sequence, ordinary or recur, is a frame of its own.
            Some(SequenceChildItem::Sequence(item)) => return item.id.as_str(),
            Some(SequenceChildItem::RecurSequence(item)) => return item.id.as_str(),
            // A recur tile pushes no frame, and a tile has no children at all:
            // in both cases the frame is further out.
            _ => frame = parent,
        }
    }
}

/// Hold a step to the `sequence_id` the schema and its coordinates determine.
///
/// Same class as [`verify_exec_index`]: the field reaches the trace leaf — the
/// leaf is `sha256(postcard(StepRecord))` over the whole record — so leaving it
/// unverified leaves free entropy an attacker can use to manufacture a
/// divergence on the window's last item.
///
/// It was only partly covered. [`record_matches_item`] compares it for
/// `SequenceStart`/`SequenceEnd`, but the `Exec` arms there compare the *target
/// name* instead, and the program boundaries never reach that check at all —
/// `verify_step_record_inputs` returns early on empty coordinates. This closes
/// both, and covers recur-iteration steps, which that check also skips.
///
/// The field carries two different things, which is why this is not one lookup:
/// a boundary step names the sequence it enters or leaves, everything else
/// names the frame it runs in.
pub fn verify_sequence_id(cfs_cursor: &CfsCursor, step_record: &StepRecord) {
    let coordinates = step_record.coordinates();
    let expected = match &step_record.kind {
        StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. } => {
            declared_sequence_id(cfs_cursor, coordinates).unwrap_or_else(|| {
                panic!(
                    "Boundary step {:?} sits at coordinates that declare no sequence",
                    step_record
                )
            })
        }
        StepKind::Exec(_) | StepKind::ProgramStart(_) | StepKind::ProgramEnd(_) => {
            enclosing_sequence_id(cfs_cursor, coordinates)
        }
    };

    assert_eq!(
        step_record.sequence_id, expected,
        "Step at {:?} carries sequence_id {:?} but its kind and coordinates determine {:?}",
        coordinates, step_record.sequence_id, expected,
    );
}

/// Whether any of `item`'s declared inputs is a scope binding, flattening
/// `Indexed` so an index sourced from the caller's scope counts too.
fn binds_sequence_scope(item: &SequenceChildItem) -> bool {
    fn any_scope(binding: &InputBinding) -> bool {
        match binding {
            InputBinding::SequenceScope { .. } => true,
            InputBinding::Indexed { value, indexes } => {
                any_scope(value) || indexes.iter().any(any_scope)
            }
            _ => false,
        }
    }
    item.inputs().iter().any(any_scope)
}

/// Fold a step record's trace-inclusion proof and hold it to `trace_root`.
///
/// Same fold as the fingerprint-block witnesses in
/// `fraud_proof::assert_window_is_commitment_slice`, and the same convention as
/// the host's `Hashable for Bytes` — `sha256(level || left || right)`, sibling
/// order chosen by the position bit at each level. Trace item `n` sits at
/// Merkle position `n + 1`, since position 0 is the seed leaf.
fn assert_record_in_trace(record: &StepRecord, witness: &StepRecordWitness, trace_root: &[u8]) {
    let mut current = hash_trace_item(record);
    for (level, sibling) in witness.path_elems.iter().enumerate() {
        current = if ((witness.position >> level) & 1) == 0 {
            combine_merkle_level(level, &current, sibling)
        } else {
            combine_merkle_level(level, sibling, &current)
        };
    }
    assert!(
        current == trace_root,
        "Sequence-scope parent record is not in the trace at the claimed position: folding \
         record {:?} from position {} through {} path elements gives {:?}, but the trace root \
         here is {:?}",
        record.coordinates,
        witness.position,
        witness.path_elems.len(),
        current,
        trace_root,
    );
}

/// Bind a `SequenceScope` witness to the frame-opening record it claims to be.
///
/// `verify_one_binding` compares the step's own source against argument `i` of
/// the parent's `FnInput`. That comparison is only worth anything if the parent
/// `FnInput` is the parent's: the step's own witness is pinned to its record's
/// fingerprinted `input_source_commitment` (`checks::io::verify_step_record`),
/// but the parent's arrived from the host bound to nothing, so the check
/// compared a value against a value the same party chose and passed for any
/// claim at all.
///
/// Three things together pin it:
///
/// 1. the supplied parent record is really in the trace, proven against the
///    root of the prefix this step is appended to;
/// 2. it is the `SequenceStart` at this step's parent frame coordinates —
///    coordinates carry the iteration index inside recur sites, so they name
///    one invocation rather than a set;
/// 3. its recorded `input_source_commitment` is the commitment of the supplied
///    `FnInput` — which is what makes the preimage the parent's own arguments.
///
/// This is what `TransitionInput::input_sources_witnesses` was built for. The
/// host has always shipped it (`raster-prover::trace::witness_record_inputs`)
/// and nothing read it.
pub fn verify_sequence_scope_parent(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    sequence_scope_witness: Option<&FnInput>,
    input_sources_witnesses: &HashMap<(u64, StepRecord), Vec<u8>>,
    trace_root: &[u8],
) {
    let coordinates = step_record.coordinates();
    // The program boundaries bind no CFS inputs, and a recur iteration's inputs
    // are checked by the chunking rules instead — both mirror the guards in
    // `verify_step_record_inputs`.
    if coordinates.is_empty()
        || cfs_cursor
            .try_get_recur_iteration_coordinates(coordinates)
            .is_some()
    {
        return;
    }

    let Some(cfs_item) = cfs_cursor.try_get_item(coordinates) else {
        return;
    };
    if !binds_sequence_scope(cfs_item) {
        return;
    }

    let scope_witness = sequence_scope_witness.unwrap_or_else(|| {
        panic!(
            "Step {:?} binds a sequence-scope input but supplied no scope witness",
            step_record
        )
    });
    let Some((parent_coordinates, _)) = coordinates.try_parent() else {
        panic!(
            "Step {:?} binds a sequence-scope input at the root frame, which has no caller",
            step_record
        )
    };

    let (parent_record, witness_bytes) = input_sources_witnesses
        .iter()
        .find_map(|((verifier_exec_index, record), bytes)| {
            // This step's own witness, not merely one for this parent: a
            // witness folds to the trace root of the step it was built for,
            // and the frontier has grown by then for any other step.
            (*verifier_exec_index == step_record.exec_index
                && matches!(record.kind, StepKind::SequenceStart { .. })
                && *record.coordinates() == parent_coordinates)
                .then_some((record, bytes))
        })
        .unwrap_or_else(|| {
            panic!(
                "Step {:?} binds a sequence-scope input but no SequenceStart witness for frame \
                 {:?} was supplied",
                step_record, parent_coordinates
            )
        });

    let witness: StepRecordWitness = postcard::from_bytes(witness_bytes)
        .expect("Sequence-scope parent witness is not a StepRecordWitness");
    assert_record_in_trace(parent_record, &witness, trace_root);

    let recorded = parent_record
        .input_source_commitment()
        .expect("a SequenceStart record always commits its input source");
    assert!(
        *recorded == input_source_commitment(scope_witness),
        "Sequence-scope witness is not the parent SequenceStart's recorded input source",
    );
}

/// Hold one recorded argument to one CFS input binding.
///
/// Split out of the loop so [`InputBinding::Indexed`] can delegate to it for
/// both the value it wraps and each index it cites.
#[allow(clippy::too_many_arguments)]
fn verify_one_binding(
    step_input: &InputBinding,
    resolved_source: ResolvedSource<'_>,
    input_index: usize,
    step_record: &StepRecord,
    cfs_cursor: &CfsCursor,
    input_source_witness: &FnInput,
    sequence_scope_witness: Option<&FnInput>,
    parent_sequence_coordinates: &CfsCoordinates,
    item_coordinate: CfsCoordinate,
) {
    // A binding the schema did *not* declare as index-sourced must not have
    // become one in the recording, and vice versa. Without this pairing the
    // schema would describe dynamic indexing without constraining it: a prover
    // could add a `BoundIndex` to an argument the program wrote with a literal
    // index (or drop one the program wrote), and every other check would still
    // pass because each is satisfied by *some* consistent index.
    let declared_indexes = step_input.index_bindings();
    let recorded_indexes = bound_index_count(&resolved_source);
    assert_eq!(
        declared_indexes.len(),
        recorded_indexes,
        "Step {:?} arg {} records {} data-sourced index(es) but the CFS declares {}",
        step_record,
        input_index,
        recorded_indexes,
        declared_indexes.len(),
    );

    if !declared_indexes.is_empty() {
        // Each cited index is itself a value with provenance; hold it to the
        // binding the schema declares for it, the same way the wrapped value is
        // held to its own.
        let cited = cited_index_sources(&resolved_source, input_source_witness);
        assert_eq!(
            cited.len(),
            declared_indexes.len(),
            "Step {:?} arg {} cites {} index binding(s) but the CFS declares {}",
            step_record,
            input_index,
            cited.len(),
            declared_indexes.len(),
        );
        for (index_binding, index_source) in declared_indexes.iter().zip(cited) {
            verify_one_binding(
                index_binding,
                ResolvedSource::Storage(index_source),
                input_index,
                step_record,
                cfs_cursor,
                input_source_witness,
                sequence_scope_witness,
                parent_sequence_coordinates,
                item_coordinate,
            );
        }
    }

    match step_input.value_binding() {
        InputBinding::Direct(InputSource::Inline) => {
            assert!(
                matches!(resolved_source, ResolvedSource::Inline(_)),
                "Expected inline input source for step {:?} arg {}",
                step_record,
                input_index,
            );
        }
        InputBinding::Direct(InputSource::Storage) => {
            assert!(
                matches!(resolved_source, ResolvedSource::Storage(_)),
                "Expected storage input source for step {:?} arg {}",
                step_record,
                input_index,
            );
        }
        InputBinding::EntryArgument => {
            // One of `main`'s entry arguments: it must be sourced from
            // the authorized entry object at the sequence root `[]` that
            // the `ProgramStart` step bound. The selector into that
            // object (and its selection proof) is verified separately by
            // the storage checks; here we hold the binding to the one
            // coordinate the entry object can legitimately come from.
            let storage_meta = match resolved_source {
                ResolvedSource::Storage(meta) => meta,
                _ => panic!(
                    "Expected storage input source for entry-argument step {:?} arg {}",
                    step_record, input_index
                ),
            };
            assert!(
                storage_meta.coordinates.is_empty(),
                "Entry-argument input for step {:?} arg {} must come from the sequence root",
                step_record,
                input_index,
            );
        }
        InputBinding::SequenceScope { input_index } => {
            let sequence_scope_witness = sequence_scope_witness.unwrap_or_else(|| {
                panic!(
                    "Missing sequence scope witness for step record {:?}",
                    step_record
                )
            });
            let scope_source = resolved_source_at(sequence_scope_witness, *input_index);
            assert_same_source(resolved_source, scope_source);
        }
        InputBinding::PriorItemOutput {
            intra_sequence_item_index,
        } => {
            // `intra_sequence_item_index` is a 0-based index into the
            // sequence's `items`, which is what the CFS stores; coordinates are
            // 1-based positions derived from it. Converting here keeps the two
            // schemes apart at the one place they meet.
            let source_coordinate = CfsCoordinate::try_from(*intra_sequence_item_index)
                .expect("Prior item output index exceeds CFS coordinate bounds")
                + FIRST_COORDINATE;
            assert!(
                source_coordinate < item_coordinate,
                "Step {:?} cannot depend on source item {} from the same or a future position {}",
                step_record,
                intra_sequence_item_index,
                item_coordinate
            );

            let mut source_coordinates = parent_sequence_coordinates.clone();
            source_coordinates.push(source_coordinate);
            let storage_meta = match resolved_source {
                ResolvedSource::Storage(meta) => meta,
                _ => {
                    panic!(
                        "Expected storage input source for step {:?} arg {}",
                        step_record, input_index
                    )
                }
            };
            match cfs_cursor
                .try_get_item(&source_coordinates)
                .expect("Expected prior item output coordinates to resolve in CFS")
            {
                raster_core::cfs::SequenceChildItem::Sequence(_)
                | raster_core::cfs::SequenceChildItem::RecurSequence(_) => {
                    assert!(
                            has_coordinate_prefix(&storage_meta.coordinates, &source_coordinates),
                            "Storage input prior-item-output coordinates do not descend from expected sequence source",
                        );
                }
                raster_core::cfs::SequenceChildItem::Tile(_)
                | raster_core::cfs::SequenceChildItem::RecurTile(_) => {
                    assert_eq!(
                            storage_meta.coordinates, source_coordinates,
                            "Storage input prior-item-output coordinates do not match expected CFS source",
                        );
                }
            }
        }
        // `value_binding()` looks through every wrapper, so an `Indexed`
        // binding can never reach this match.
        InputBinding::Indexed { .. } => {
            unreachable!("value_binding() unwraps Indexed before this match")
        }
    }
}

// Verify that current step record coordinates are in previous expected next coordinates and with
// CfsCursor iterate to next expected coordiantes
pub fn get_next_expected_coordinates(
    cfs_cursor: &CfsCursor,
    step: &StepRecord,
    current_expected_coordinates: Option<&Vec<CfsCoordinates>>,
) -> Vec<CfsCoordinates> {
    let coordinates = step.coordinates();
    if let Some(current_expected_coordinates) = current_expected_coordinates {
        assert!(
            current_expected_coordinates.contains(coordinates),
            "Step {:?} (kind {}) is not among the coordinates the previous step allows: {:?}",
            coordinates,
            match &step.kind {
                StepKind::ProgramStart(_) => "ProgramStart",
                StepKind::ProgramEnd(_) => "ProgramEnd",
                StepKind::SequenceStart { .. } => "SequenceStart",
                StepKind::SequenceEnd { .. } => "SequenceEnd",
                StepKind::Exec(_) => "Exec",
            },
            current_expected_coordinates,
        );
    }

    cfs_cursor
        .try_get_next_coordinates(coordinates)
        .expect("Wrong tile coordinates")
}

/// The authenticated source length carried by a recur site's `Start` step.
///
/// `Start` records the source under the binding name `"input"`, whose selection
/// is the `0x0A` list-metadata payload (`lazy-list-recur.md` §1–§2). By the time
/// this runs, `checks::store` has already folded that witness to the committed
/// root, so the length read here is authenticated rather than index-trusted —
/// which is the whole reason the site needs a `Start` at all: `L` has to exist
/// before iteration 0 is checked against it.
fn authenticated_source_len(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
) -> u64 {
    let binding = input_source_witness
        .and_then(|witness| witness.storage().get("input"))
        .unwrap_or_else(|| {
            panic!(
                "Recur site start {:?} does not record its source binding",
                step_record
            )
        });
    assert_eq!(
        binding.selection.payload_kind,
        SelectionPayloadKind::List,
        "Recur site start {:?} must commit to list metadata, not a raw payload",
        step_record,
    );
    let witness = storage_selection_witnesses.get("input").unwrap_or_else(|| {
        panic!(
            "Recur site start {:?} is missing its source selection witness",
            step_record
        )
    });
    decode_list_metadata_len(&witness.bytes).unwrap_or_else(|| {
        panic!(
            "Recur site start {:?} carries a malformed list metadata payload",
            step_record
        )
    })
}

/// `len` out of a `0x0A` payload: `[0x0A][len: u64 LE]`, then a 32-byte
/// elements root when `len > 0`.
fn decode_list_metadata_len(bytes: &[u8]) -> Option<u64> {
    if *bytes.first()? != 0x0A {
        return None;
    }
    let len = u64::from_le_bytes(bytes.get(1..9)?.try_into().ok()?);
    let expected = if len == 0 { 9 } else { 41 };
    (bytes.len() == expected).then_some(len)
}

/// Advance the carried recur progress by this step's facts and hold the result
/// to the commitment the step recorded.
///
/// One rule, uniform for every step. The recorder reached its commitment by
/// advancing with the values rules 3 and 4 *require* — derived from
/// `(chunk, source_len, next_iteration_index)`, all of which it holds. The
/// guest advances with the journal's actual values. Both mutate only
/// `next_iteration_index` and `last_control`, so both land on the same frame
/// **iff** the journal agrees with the derivation; where it disagrees, a rule
/// fires before any hash is compared.
///
/// That asymmetry is the point: a journal value *compared against* the frame's
/// inputs is reachable for a producer that has no journal, whereas revision 1's
/// `consumed_total` — folded *into* the frame — was not. See
/// `docs/proposals/recur-progress-commitment.md` §1 and §4.
/// Bind a recur iteration's claimed incoming state to what it actually read.
///
/// The transition on the step record is host-written. On its own that would
/// make the chain a set of equalities between prover-chosen values; this ties
/// `state_in` to the iteration's own recorded input witness, which is already
/// bound to the step by `input_source_commitment`. A recur sequence's carried
/// state is the second recorded value — the body's parameters are
/// `input, state?, output?, args...`, which is also the order the CFS records
/// the call's sources in.
fn assert_carried_state_matches_input(step_record: &StepRecord, input_source_witness: Option<&FnInput>) {
    let Some(transition) = step_record.recur_state.as_ref() else {
        return;
    };
    let witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Recur iteration claims a carried state with no input witness: {:?}",
            step_record
        )
    });
    let Some(FnInputValue::Inline(bytes)) = witness.values().get(1) else {
        panic!(
            "Recur iteration claims a carried state but records no inline state value: {:?}",
            step_record
        )
    };
    assert_eq!(
        transition.state_in,
        raster_core::recur_progress::state_commitment(bytes),
        "Recur iteration's claimed incoming state is not the state it read: {:?}",
        step_record,
    );
}

/// Whether a recur site's own output is its carried state, from the CFS.
fn site_state_is_output(item: &SequenceChildItem) -> bool {
    match item {
        SequenceChildItem::RecurTile(tile) => tile.state_is_output,
        SequenceChildItem::RecurSequence(sequence) => sequence.state_is_output,
        _ => false,
    }
}

pub fn advance_recur_progress(
    cfs_cursor: &CfsCursor,
    progress: &mut RecurProgressStack,
    step_record: &StepRecord,
    replay_journal: Option<&TileReplayJournal>,
    input_source_witness: Option<&FnInput>,
    output_witness: Option<&Vec<u8>>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
) {
    let coordinates = step_record.coordinates();

    if let Some((site_coordinates, iteration_index)) =
        cfs_cursor.try_get_recur_iteration_coordinates(coordinates)
    {
        match cfs_cursor.try_get_item(&site_coordinates) {
            Some(SequenceChildItem::RecurTile(_)) => {
                // The step record carries a host copy of the same transition,
                // for uniformity with recur sequences. Duplicating a fact is
                // only safe where an equality makes the duplicate
                // non-load-bearing — this is that equality.
                assert_eq!(
                    step_record.recur_state.as_ref(),
                    replay_journal
                        .and_then(|journal| journal.recur.as_ref())
                        .and_then(|recur| recur.state.as_ref()),
                    "Recur tile iteration's recorded carried state disagrees with its replay-proven one: {:?}",
                    step_record,
                );

                let recur = replay_journal
                    .and_then(|journal| journal.recur.as_ref())
                    .unwrap_or_else(|| {
                        panic!(
                            "Recur iteration {:?} is missing its replay-proven recur facts",
                            step_record
                        )
                    });
                if let Err(violation) = progress.advance_tile_iteration(
                    coordinates,
                    recur.position.iteration_index,
                    recur.position.declared_iterations,
                    recur.position.consumed_elements,
                    recur.control,
                    // The replay-proven copy is the authority. The step record
                    // carries a host copy too, for uniformity with recur
                    // sequences; bind them so the duplicate is not
                    // load-bearing.
                    recur.state.as_ref(),
                ) {
                    panic!(
                        "Recur progress violation at step {:?}: {}",
                        step_record, violation
                    );
                }
            }
            Some(SequenceChildItem::RecurSequence(_)) => {
                // A recur sequence emits no journal; its iterations are read
                // from trace structure. Only the boundary *start* advances the
                // count, so an iteration is never counted twice.
                if matches!(step_record.kind, StepKind::SequenceStart { .. }) {
                    if let Err(violation) = progress
                        // The only seam between the two numbering schemes:
                        // coordinates are 1-based positions, while the progress
                        // rules count iterations from 0 — the same 0 a recur
                        // *tile*'s replay journal reports, so the counter stays
                        // one convention for both families.
                        .advance_sequence_iteration(
                            coordinates,
                            u64::try_from(iteration_index - FIRST_COORDINATE)
                                .expect("a recur iteration coordinate is at least the first"),
                        )
                    {
                        panic!(
                            "Recur progress violation at step {:?}: {}",
                            step_record, violation
                        );
                    }
                    // `state_in` is bound to what this iteration actually read,
                    // so the chain cannot be advanced through a value the step
                    // never consumed. `state_out` needs no separate anchor: the
                    // next iteration's bound `state_in` pins it through the
                    // fold rule, and the last one is pinned when the site
                    // closes.
                    assert_carried_state_matches_input(step_record, input_source_witness);
                }
                // The transition itself arrives with the iteration's `End`,
                // which is the first step at which what it produced is known.
                if matches!(step_record.kind, StepKind::SequenceEnd { .. }) {
                    if let Err(violation) = progress.fold_sequence_iteration_state(
                        coordinates,
                        step_record.recur_state.as_ref(),
                        output_witness.map(|bytes| bytes.as_slice()),
                    ) {
                        panic!(
                            "Recur progress violation at step {:?}: {}",
                            step_record, violation
                        );
                    }
                }
            }
            _ => {}
        }
    } else if let Some(item) = cfs_cursor.try_get_item(coordinates) {
        let kind = match item {
            SequenceChildItem::RecurTile(_) => Some(RecurSiteKind::Tile),
            SequenceChildItem::RecurSequence(_) => Some(RecurSiteKind::Sequence),
            _ => None,
        };
        if let Some(kind) = kind {
            match &step_record.kind {
                // `Start`: open the frame with the authenticated `L`.
                StepKind::SequenceStart { .. } => {
                    let chunk = match item {
                        SequenceChildItem::RecurTile(tile) => tile.chunk.unwrap_or(1),
                        _ => 1,
                    };
                    let source_len = authenticated_source_len(
                        step_record,
                        input_source_witness,
                        storage_selection_witnesses,
                    );
                    progress.push_site(
                        coordinates.clone(),
                        kind,
                        chunk,
                        source_len,
                        site_state_is_output(item),
                    );
                }
                // `End`: the terminal rules — 5 and 7 for a tile site, S4 for a
                // sequence site.
                StepKind::Exec(_) => {
                    if let Err(violation) = progress.close_site(coordinates) {
                        panic!(
                            "Recur progress violation at step {:?}: {}",
                            step_record, violation
                        );
                    }
                }
                _ => {}
            }
        }
    }

    assert_eq!(
        progress.commitment(),
        step_record.recur_progress_commitment,
        "Recur progress commitment does not match the state this step advances to: {:?}",
        step_record,
    );
}
