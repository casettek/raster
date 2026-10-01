//! Checks that a step record matches the control flow schema: that the
//! record's kind matches the item declared at its coordinates, that its
//! per-argument input bindings are honoured, and that its coordinates
//! follow the schema's ordering.

use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, InputBinding, InputSource, RecurOutputDecl,
    ResolveError, SequenceChildItem,
    FIRST_COORDINATE,
};
use raster_core::input::SelectorSegment;
use raster_core::transition::StepRecordWitness;
use std::collections::{BTreeMap, HashMap};

use crate::checks::io::input_source_commitment;
use crate::merkle_tree::{combine_merkle_level, hash_trace_item};

use raster_core::draft::TileReplayJournal;
use raster_core::input::{
    verify_selection_reference_witness, verify_selection_witness, Hash32, SelectionPayloadKind,
    SelectionWitness,
};
use raster_core::transition::StorageReadWitness;
use raster_core::recur_progress::{
    source_identity, DraftStep, RecurProgressStack, RecurSiteKind, SiteDraft,
};
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

/// Whether `frame` is one iteration `[s, i]` of a recur **sequence** site.
fn is_recur_sequence_iteration_frame(cfs_cursor: &CfsCursor, frame: &CfsCoordinates) -> bool {
    cfs_cursor
        .try_get_recur_iteration_coordinates(frame)
        .is_some_and(|(site, _)| {
            matches!(
                cfs_cursor.try_get_item(&site),
                Some(SequenceChildItem::RecurSequence(_))
            )
        })
}

/// How `RecurSequenceInput` records itself: an opaque handle, so the body sees
/// neither the value nor the position. Mirrors `raster::input`'s trace marker;
/// `kind` is a `&'static str` there, which postcard encodes as a `String`.
#[derive(serde::Deserialize)]
struct RecurSequenceInputMarker {
    kind: String,
    #[allow(dead_code)]
    index: u64,
    #[allow(dead_code)]
    len: u64,
    item: FnInputValue,
}

/// A recur-sequence iteration's parameter 0, resolved to the item it carries.
///
/// The iteration `Start` records the `RecurSequenceInput` handle as an inline
/// marker, and — when the item is stored — the item's storage data under the
/// parameter's name. A body step that reads the item (`into_ref!`) records that
/// storage binding directly, so the scope comparison has to look through the
/// handle to the item, or every honest recur-sequence body fails it.
///
/// Sound because the whole witness, marker included, is the one the parent
/// `Start` committed (`verify_sequence_scope_parent`), and the storage entry
/// resolved here is the one rule 8 held to the site's source at that `Start`.
fn recur_sequence_item_source(scope_witness: &FnInput) -> ResolvedSource<'_> {
    let arg = scope_witness
        .args()
        .first()
        .expect("A recur sequence iteration records its item parameter");
    let Some(FnInputValue::Inline(marker_bytes)) = scope_witness.values().first() else {
        panic!("A recur sequence iteration must record its item as a RecurSequenceInput handle");
    };
    let marker: RecurSequenceInputMarker = postcard::from_bytes(marker_bytes)
        .expect("A recur sequence iteration's item is not a RecurSequenceInput handle");
    assert_eq!(
        marker.kind, "raster::RecurSequenceInput",
        "A recur sequence iteration's item is not a RecurSequenceInput handle",
    );
    match marker.item {
        FnInputValue::StorageBinding => ResolvedSource::Storage(
            scope_witness
                .storage()
                .get(&arg.name)
                .unwrap_or_else(|| panic!("Missing storage input metadata for arg '{}'", arg.name)),
        ),
        // Rule 8 already refuses an iteration whose item is not stored
        // (`recur_sequence_item_selection`), so no verified trace reaches this.
        FnInputValue::Inline(_) => panic!(
            "A recur sequence iteration's item '{}' is not stored",
            arg.name
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
        // A nested sequence is entered and left at its own item coordinate
        // (`[s]` / `[-s]`). The entered sequence's name is carried on the
        // step record. A recur-sequence *iteration*'s boundary steps never
        // reach this match: iterations return before it.
        (StepKind::SequenceStart { .. }, SequenceChildItem::Sequence(item)) => {
            !step_record.coordinates().is_closing() && step_record.sequence_id == item.id
        }
        (StepKind::SequenceEnd { .. }, SequenceChildItem::Sequence(item)) => {
            step_record.coordinates().is_closing() && step_record.sequence_id == item.id
        }
        // A recur site opens with `RecurStart` at `[s]` and closes with
        // `RecurEnd` at `[-s]`, whichever family the CFS item is.
        (StepKind::RecurStart(start), SequenceChildItem::RecurTile(item)) => {
            !step_record.coordinates().is_closing() && start.site_id == item.id
        }
        (StepKind::RecurStart(start), SequenceChildItem::RecurSequence(item)) => {
            !step_record.coordinates().is_closing() && start.site_id == item.id
        }
        (StepKind::RecurEnd(end), SequenceChildItem::RecurTile(item)) => {
            step_record.coordinates().is_closing() && end.site_id == item.id
        }
        (StepKind::RecurEnd(end), SequenceChildItem::RecurSequence(item)) => {
            step_record.coordinates().is_closing() && end.site_id == item.id
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
    if matches!(
        step_record.kind,
        StepKind::SequenceEnd { .. } | StepKind::RecurEnd(_)
    ) {
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
        // A tile is not a frame, and a recur tile pushes none, so no sequence
        // boundary step can sit at either; a recur site opens and closes with
        // `RecurStart`/`RecurEnd`, which name their enclosing sequence.
        SequenceChildItem::Tile(_) | SequenceChildItem::RecurTile(_) => None,
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
        StepKind::Exec(_)
        | StepKind::ProgramStart(_)
        | StepKind::ProgramEnd(_)
        | StepKind::RecurStart(_)
        | StepKind::RecurEnd(_) => enclosing_sequence_id(cfs_cursor, coordinates),
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
    // The program boundaries and every close bind no CFS inputs, and a recur
    // iteration's inputs are checked by the chunking rules instead — both
    // mirror the guards in `verify_step_record_inputs`.
    if step_record.input_source_commitment().is_none()
        || coordinates.is_empty()
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

/// The fallback for a source item whose value `CfsCursor::resolve_value`
/// cannot follow: hold the value's coordinates to the item itself.
///
/// `source_coordinate` is the producing item's 1-based position in the
/// sequence at `parent_sequence_coordinates`. A tile has one output, at its own
/// coordinate, so the value must sit exactly there. For a sequence item whose
/// return the CFS could not bind, or a chain ending at a recur body's
/// parameter, this can only require the value to lie *inside* the item — the
/// residual gap `docs/issues/program-output-unbound.md` records.
fn assert_prior_item_output_coordinates(
    cfs_cursor: &CfsCursor,
    parent_sequence_coordinates: &CfsCoordinates,
    source_coordinate: CfsCoordinate,
    value_coordinates: &CfsCoordinates,
) {
    let mut source_coordinates = parent_sequence_coordinates.clone();
    source_coordinates.push(source_coordinate);
    match cfs_cursor
        .try_get_item(&source_coordinates)
        .expect("Expected prior item output coordinates to resolve in CFS")
    {
        raster_core::cfs::SequenceChildItem::Sequence(_)
        | raster_core::cfs::SequenceChildItem::RecurSequence(_) => {
            assert!(
                has_coordinate_prefix(value_coordinates, &source_coordinates),
                "Storage input prior-item-output coordinates do not descend from expected sequence source",
            );
        }
        raster_core::cfs::SequenceChildItem::Tile(_)
        | raster_core::cfs::SequenceChildItem::RecurTile(_) => {
            assert_eq!(
                value_coordinates, &source_coordinates,
                "Storage input prior-item-output coordinates do not match expected CFS source",
            );
        }
    }
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
            let scope_source = if *input_index == 0
                && is_recur_sequence_iteration_frame(cfs_cursor, parent_sequence_coordinates)
            {
                recur_sequence_item_source(sequence_scope_witness)
            } else {
                resolved_source_at(sequence_scope_witness, *input_index)
            };
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

            let storage_meta = match resolved_source {
                ResolvedSource::Storage(meta) => meta,
                _ => {
                    panic!(
                        "Expected storage input source for step {:?} arg {}",
                        step_record, input_index
                    )
                }
            };
            // Follow the source item down to the step that wrote its value: a
            // nested sequence returns one object of the many it writes, and the
            // CFS records which. Where the chain cannot be followed — a nested
            // return the CFS could not bind, or a recur body's parameter — fall
            // back to the looser "inside the source item" rule rather than
            // refuse an honest program.
            match cfs_cursor.resolve_value(
                parent_sequence_coordinates,
                &InputBinding::prior_item_output(*intra_sequence_item_index),
                &[],
            ) {
                Ok(resolved) => assert_eq!(
                    storage_meta.coordinates, resolved.coordinates,
                    "Storage input prior-item-output coordinates do not match expected CFS source",
                ),
                Err(ResolveError::UnboundReturn(_) | ResolveError::RecurBodyParameter) => {
                    assert_prior_item_output_coordinates(
                        cfs_cursor,
                        parent_sequence_coordinates,
                        source_coordinate,
                        &storage_meta.coordinates,
                    )
                }
                Err(error) => panic!(
                    "Step {:?} arg {} names a source the CFS cannot follow: {:?}",
                    step_record, input_index, error
                ),
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
                StepKind::RecurStart(_) => "RecurStart",
                StepKind::RecurEnd(_) => "RecurEnd",
            },
            current_expected_coordinates,
        );
    }

    cfs_cursor
        .try_get_next_coordinates(coordinates)
        .expect("Wrong tile coordinates")
}

/// The source length carried by a recur site's `Start` step.
///
/// `Start` records the source under the binding name `"input"`, whose selection
/// is the `0x0A` list-metadata payload (`lazy-list-recur.md` §1–§2). `L` has to
/// exist before iteration 0 is checked against it, which is why the site needs
/// a `Start` at all.
///
/// **Authenticated by the same step, not by this function.** A site `Start`
/// must claim read-only storage roots (`advance_recur_progress`), so
/// `checks::store` reads the source object against the store and folds this
/// metadata witness to its commitment. Those checks run later in the step, but
/// any failure rejects the step, so the `L` read here only stands if they pass.
/// Before the roots, a boundary step got neither check, and a fabricated empty
/// list swept zero times passed rule 7. See
/// `docs/proposals/tile-io-structural-roots.md` §Step 1.
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

/// A recur-sequence iteration's state, at its `Start`: the body's second
/// parameter (`input, state, output?, args…`), the order the CFS records the
/// call's sources in.
///
/// Held by reference after iteration 0 (D5b), so it must be a binding naming
/// exactly the state the chain has reached — the previous iteration's returned
/// object, or a stored seed — as a whole object. Only iteration 0 of a site
/// seeded inline reads its state inline, and nothing commits it: the chain
/// adopts it at that iteration's `End`, as an inline seed always was.
///
/// The binding is not read here: a sequence boundary has no storage roots. A
/// body tile that consumes the state reads it from storage, and
/// `verify_sequence_scope_parent` ties that read to this binding.
fn check_iteration_state_binding(
    step_record: &StepRecord,
    progress: &RecurProgressStack,
    input_source_witness: Option<&FnInput>,
) {
    let witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} carries state but records no input witness",
            step_record
        )
    });
    let expected = progress.innermost().and_then(|frame| frame.state_commitment);
    match (witness.values().get(1), expected) {
        (Some(FnInputValue::StorageBinding), Some(expected)) => {
            let name = witness
                .args()
                .get(1)
                .map(|arg| arg.name.as_str())
                .expect("a recorded value has an argument");
            let binding = witness.storage().get(name).unwrap_or_else(|| {
                panic!(
                    "Recur sequence iteration {:?} is missing its state binding '{}'",
                    step_record, name
                )
            });
            assert!(
                binding.selection.path.segments.is_empty(),
                "Recur sequence iteration {:?} must carry its state as a whole object",
                step_record,
            );
            assert_eq!(
                binding.commitment.as_slice(),
                expected.as_slice(),
                "Recur sequence iteration {:?} reads a state that is not the one its sweep reached",
                step_record,
            );
        }
        (Some(FnInputValue::Inline(_)), None) => {}
        (Some(FnInputValue::Inline(_)), Some(_)) => panic!(
            "Recur sequence iteration {:?} carries its state inline after the chain started; \
             it must be the previous iteration's returned object",
            step_record
        ),
        (Some(FnInputValue::StorageBinding), None) => panic!(
            "Recur sequence iteration {:?} reads a stored state its site's chain never opened at",
            step_record
        ),
        (None, _) => panic!(
            "Recur sequence iteration {:?} carries state but records no state value",
            step_record
        ),
    }
}

/// A recur-sequence iteration's returned state, at its `End` (D5b): the
/// object the CFS's `returns` for the body names inside this iteration, read
/// from the current storage state, whose commitment is the transition's
/// `state_out`. That ties every link of the chain to what a body tile wrote —
/// the tile's write is itself bound to its replay (`tile-io-structural-roots`
/// step 2) — and the last link to the site's stored result, at `RecurEnd`.
///
/// A body returning its state parameter unchanged returns the state it read.
fn verify_returned_state(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    body_id: &str,
    transition: &raster_core::draft::RecurStateTransition,
    output_witness: Option<&Vec<u8>>,
    read_witness: Option<&StorageReadWitness>,
    current_storage_root: &[u8],
    current_index_root: &[u8],
) {
    let returns = cfs_cursor.sequence_returns(body_id).unwrap_or_else(|| {
        panic!(
            "Recur sequence `{}` carries state but the CFS binds no returned state",
            body_id
        )
    });
    if matches!(returns.source, InputBinding::SequenceScope { input_index: 1 })
        && returns.path.is_empty()
    {
        assert_eq!(
            transition.state_out, transition.state_in,
            "Recur sequence iteration {:?} returns its state unchanged but claims another",
            step_record,
        );
        return;
    }
    let iteration = step_record.coordinates().opened();
    let resolved = cfs_cursor
        .resolve_value(&iteration, &returns.source, &returns.path)
        .unwrap_or_else(|error| {
            panic!(
                "Recur sequence `{}`'s returned state does not resolve to a step's output: {:?}",
                body_id, error
            )
        });
    assert!(
        resolved.path.is_empty() && resolved.path_complete,
        "Recur sequence `{}` must return its state as a whole object",
        body_id,
    );
    let returned: Option<StorageData> = output_witness
        .and_then(|bytes| postcard::from_bytes(bytes).ok())
        .unwrap_or_else(|| {
            panic!(
                "Recur sequence iteration {:?} records no returned state binding",
                step_record
            )
        });
    let returned = returned.unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} returned a state that is not a stored object",
            step_record
        )
    });
    assert_eq!(
        returned.coordinates, resolved.coordinates,
        "Recur sequence iteration {:?} returned a state its body did not produce",
        step_record,
    );
    let read_witness = read_witness.unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} is missing the read witness of its returned state",
            step_record
        )
    });
    crate::checks::store::verify_storage_read_witness(
        read_witness,
        current_storage_root,
        current_index_root,
        &returned.coordinates,
        &returned.commitment,
    );
    assert_eq!(
        transition.state_out.as_slice(),
        returned.commitment.as_slice(),
        "Recur sequence iteration {:?} claims a state_out that is not its returned object",
        step_record,
    );
}

/// A recur tile iteration's item: its first parameter, the `RecurInput`,
/// which the driver records as a storage binding under the parameter's own
/// name — so it is found by position, never by a fixed name — together with
/// its selection witness, already verified by `checks::store`.
fn recur_item_selection<'a>(
    step_record: &StepRecord,
    input_source_witness: Option<&'a FnInput>,
    storage_selection_witnesses: &'a BTreeMap<String, SelectionWitness>,
) -> (&'a StorageData, &'a SelectionWitness) {
    let witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Recur iteration {:?} records no input witness for its item",
            step_record
        )
    });
    let name = match (witness.args().first(), witness.values().first()) {
        (Some(arg), Some(FnInputValue::StorageBinding)) => arg.name.as_str(),
        _ => panic!(
            "Recur iteration {:?} does not read its item from storage",
            step_record
        ),
    };
    let item = witness.storage().get(name).unwrap_or_else(|| {
        panic!(
            "Recur iteration {:?} is missing its item binding '{}'",
            step_record, name
        )
    });
    let item_witness = storage_selection_witnesses.get(name).unwrap_or_else(|| {
        panic!(
            "Recur iteration {:?} is missing its item selection witness '{}'",
            step_record, name
        )
    });
    (item, item_witness)
}

/// A recur **sequence** iteration's item, with its selection witness verified
/// here.
///
/// The body's parameter 0 is the `RecurSequenceInput`. Its recorded value is
/// an inline marker (index, len — host-written, not read here), and the item's
/// storage data sits under the parameter's name. The iteration `Start` only
/// forwards the item, so its witness is normally the reference form; either
/// form is folded to the recorded commitment before rule 8 reads its steps.
fn recur_sequence_item_selection<'a>(
    step_record: &StepRecord,
    input_source_witness: Option<&'a FnInput>,
    storage_selection_witnesses: &'a BTreeMap<String, SelectionWitness>,
) -> (&'a StorageData, &'a SelectionWitness) {
    let witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} records no input witness for its item",
            step_record
        )
    });
    let name = witness
        .args()
        .first()
        .map(|arg| arg.name.as_str())
        .unwrap_or_else(|| {
            panic!(
                "Recur sequence iteration {:?} records no item parameter",
                step_record
            )
        });
    let item = witness.storage().get(name).unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} does not read its item '{}' from storage",
            step_record, name
        )
    });
    let item_witness = storage_selection_witnesses.get(name).unwrap_or_else(|| {
        panic!(
            "Recur sequence iteration {:?} is missing its item selection witness '{}'",
            step_record, name
        )
    });
    let verified = if item_witness.is_reference_only() {
        verify_selection_reference_witness(&item.selection, item_witness)
    } else {
        verify_selection_witness(&item.selection, item_witness)
    };
    assert!(
        verified,
        "Recur sequence iteration {:?} item selection witness does not fold to its commitment",
        step_record
    );
    assert_eq!(
        item.commitment, item.selection.source_root_hash,
        "Recur sequence iteration {:?} item commitment must match its selection root",
        step_record
    );
    (item, item_witness)
}

/// Whether a recur site's own output is its carried state, from the CFS.
fn site_state_is_output(item: &SequenceChildItem) -> bool {
    match item {
        SequenceChildItem::RecurTile(tile) => tile.state_is_output,
        SequenceChildItem::RecurSequence(sequence) => sequence.state_is_output,
        _ => false,
    }
}

/// A recur site's stored seed, at its `Start` (D5b): the commitment of its
/// second argument when the CFS says it carries state and the seed is a whole
/// stored object — read from storage by `checks::store`, since `RecurStart`
/// carries storage roots. `None` for an inline seed, which the chain adopts
/// from iteration 0, and for a selection inside an object, whose value root
/// the binding does not carry.
fn stored_seed(item: &SequenceChildItem, input_source_witness: Option<&FnInput>) -> Option<Hash32> {
    let carries_state = match item {
        SequenceChildItem::RecurTile(tile) => tile.carries_state,
        SequenceChildItem::RecurSequence(sequence) => sequence.carries_state,
        _ => false,
    };
    if !carries_state {
        return None;
    }
    let witness = input_source_witness?;
    if !matches!(witness.values().get(1), Some(FnInputValue::StorageBinding)) {
        return None;
    }
    let binding = witness.storage().get(witness.args().get(1)?.name.as_str())?;
    if !binding.selection.path.segments.is_empty() {
        return None;
    }
    binding.commitment.as_slice().try_into().ok()
}

/// The object a recur site owns, from the CFS: `None` for a state-only site.
fn site_output_decl(item: &SequenceChildItem) -> Option<&RecurOutputDecl> {
    match item {
        SequenceChildItem::RecurTile(tile) => tile.output.as_ref(),
        SequenceChildItem::RecurSequence(sequence) => sequence.output.as_ref(),
        _ => None,
    }
}

/// The root a site's object opens at: the CFS's empty root for a creating
/// site; for a deriving one, the commitment of the base its `Start` reads as
/// `"output"` — bound to its producer by the CFS input check and read from
/// storage by `checks::store`, since `RecurStart` carries storage roots.
///
/// A whole object only: a base selected out of a larger object would need the
/// selected value's root, which the binding does not carry.
fn opening_draft(
    step_record: &StepRecord,
    decl: &RecurOutputDecl,
    input_source_witness: Option<&FnInput>,
) -> SiteDraft {
    if !decl.derives {
        return SiteDraft {
            schema_hash: decl.schema_hash,
            root: decl.empty_root,
        };
    }
    let base = input_source_witness
        .and_then(|witness| witness.storage().get("output"))
        .unwrap_or_else(|| {
            panic!(
                "Deriving recur site {:?} does not read its base as `output`",
                step_record.coordinates
            )
        });
    assert!(
        base.selection.path.segments.is_empty(),
        "Deriving recur site {:?} must derive from a whole object, not a selection inside one",
        step_record.coordinates,
    );
    SiteDraft {
        schema_hash: decl.schema_hash,
        root: base
            .commitment
            .as_slice()
            .try_into()
            .expect("an object commitment is 32 bytes"),
    }
}

pub fn advance_recur_progress(
    cfs_cursor: &CfsCursor,
    progress: &mut RecurProgressStack,
    step_record: &StepRecord,
    replay_journal: Option<&TileReplayJournal>,
    draft_step: Option<&DraftStep>,
    input_source_witness: Option<&FnInput>,
    output_witness: Option<&Vec<u8>>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
    returned_state_read_witness: Option<&StorageReadWitness>,
    current_storage_root: &[u8],
    current_index_root: &[u8],
) {
    let coordinates = step_record.coordinates();
    // Set for a recur tile's iteration: its return is the site's draft, so a
    // site owning an object requires a transition from every one.
    let mut is_tile_iteration = false;

    if let Some((site_coordinates, iteration_index)) =
        cfs_cursor.try_get_recur_iteration_coordinates(coordinates)
    {
        match cfs_cursor.try_get_item(&site_coordinates) {
            Some(SequenceChildItem::RecurTile(tile)) => {
                is_tile_iteration = true;
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
                // Rule 8 first: it reads the sweep position this iteration
                // starts from, which `advance_tile_iteration` moves past.
                let (item, item_witness) = recur_item_selection(
                    step_record,
                    input_source_witness,
                    storage_selection_witnesses,
                );
                if let Err(violation) = progress.check_iteration_item(
                    coordinates,
                    item,
                    item_witness,
                    tile.chunk.is_some(),
                    recur.position.consumed_elements,
                ) {
                    panic!(
                        "Recur progress violation at step {:?}: {}",
                        step_record, violation
                    );
                }
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
            Some(SequenceChildItem::RecurSequence(sequence_item)) => {
                // A recur sequence emits no journal; its iterations are read
                // from trace structure. Only the boundary *start* advances the
                // count, so an iteration is never counted twice.
                if matches!(step_record.kind, StepKind::SequenceStart { .. }) {
                    // Rule 8 for a sequence iteration: its item is element
                    // `consumed_total` of the site's source. Before advancing,
                    // which moves `consumed_total` past it.
                    //
                    // An iteration `Start` is a sequence boundary, so it has
                    // no storage roots and `checks::store` never folds its
                    // witnesses — this check verifies the one it reads. The
                    // object the item names is authenticated when a body tile
                    // reads it, through `verify_sequence_scope_parent`.
                    let (item, item_witness) = recur_sequence_item_selection(
                        step_record,
                        input_source_witness,
                        storage_selection_witnesses,
                    );
                    if let Err(violation) =
                        progress.check_iteration_item(coordinates, item, item_witness, false, 1)
                    {
                        panic!(
                            "Recur progress violation at step {:?}: {}",
                            step_record, violation
                        );
                    }
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
                    if sequence_item.carries_state {
                        check_iteration_state_binding(step_record, progress, input_source_witness);
                    }
                }
                // The transition itself arrives with the iteration's `End`,
                // which is the first step at which what it produced is known.
                if matches!(step_record.kind, StepKind::SequenceEnd { .. }) {
                    if let Some(transition) = step_record.recur_state.as_ref() {
                        verify_returned_state(
                            cfs_cursor,
                            step_record,
                            &sequence_item.id,
                            transition,
                            output_witness,
                            returned_state_read_witness,
                            current_storage_root,
                            current_index_root,
                        );
                    }
                    if let Err(violation) = progress.fold_sequence_iteration_state(
                        coordinates,
                        step_record.recur_state.as_ref(),
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
                // `RecurStart`: open the frame with the authenticated `L`. The
                // kind carries read-only storage roots, so `checks::store`
                // reads the source object and folds its metadata witness in
                // this same step.
                StepKind::RecurStart(_) => {
                    let chunk = match item {
                        SequenceChildItem::RecurTile(tile) => tile.chunk.unwrap_or(1),
                        _ => 1,
                    };
                    let source_len = authenticated_source_len(
                        step_record,
                        input_source_witness,
                        storage_selection_witnesses,
                    );
                    // `authenticated_source_len` has already required the
                    // binding; this is the object and path it names.
                    let source = input_source_witness
                        .and_then(|witness| witness.storage().get("input"))
                        .map(source_identity)
                        .expect("the source binding was required above");
                    progress.push_site(
                        coordinates.clone(),
                        kind,
                        chunk,
                        source_len,
                        site_state_is_output(item),
                        source,
                    );
                    if let Some(decl) = site_output_decl(item) {
                        progress.open_draft(opening_draft(step_record, decl, input_source_witness));
                    }
                    if let Some(seed) = stored_seed(item, input_source_witness) {
                        progress.seed_state(seed);
                    }
                }
                // `End`: the terminal rules — 5 and 7 for a tile site, S4 for a
                // sequence site.
                // The frame is keyed by the site `[s]`; the close sits at `[-s]`.
                // Then the object it wrote: the one its steps built, or its
                // final carried state.
                StepKind::RecurEnd(end) => {
                    if let Err(violation) =
                        progress.close_site(&coordinates.opened(), &end.output_commitment)
                    {
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

    // Any tile step: its draft transition, if any, advances the innermost
    // site's object — a recur tile's iteration or a recur sequence's body tile.
    if step_record.requires_replay_proof() {
        if let Err(violation) =
            progress.advance_draft(coordinates, draft_step, is_tile_iteration)
        {
            panic!(
                "Recur progress violation at step {:?}: {}",
                step_record, violation
            );
        }
    }

    assert_eq!(
        progress.commitment(),
        step_record.recur_progress_commitment,
        "Recur progress commitment does not match the state this step advances to: {:?}",
        step_record,
    );
}
