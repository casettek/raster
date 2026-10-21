//! The control-flow check: does a step record sit where the program's control
//! flow schema says it can, and do its arguments come from where the schema
//! says they do?
//!
//! [`verify_step`] is the whole check, a pure function of the CFS and a
//! [`ControlFlowInput`]: the record, its trace position and the root of the
//! trace before it, the coordinates the previous step allowed, and witnesses
//! anchored to the record or to that root. It reads no storage, replay journal,
//! draft or recur state; those stay in the transition guest. See
//! `docs/proposals/control-flow-prover.md`.

use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, InputBinding, InputSource, ResolveError,
    SequenceChildItem, FIRST_COORDINATE,
};
use raster_core::control_flow::{ControlFlowEntry, ControlFlowInput, ControlFlowSegment};
use raster_core::input::SelectorSegment;
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, FnInputValue, StepKind, StepRecord, StorageData,
};
use raster_core::transition::StepRecordWitness;

use guest_common::input_source::{input_source_commitment, verify_input_source_anchored};
use guest_common::merkle_tree::{
    combine_merkle_level, deserialize_frontier, frontier_root, hash_trace_item, Bytes,
};

/// The control-flow check for a segment of consecutive steps. Returns one entry
/// per step.
///
/// Only the segment's start — the trace frontier and the expected coordinates
/// before its first step — comes from the host. Every later step's trace
/// position and root come from appending the records before it, the way the
/// transition guest's frontier grows, and its expected coordinates are the
/// previous step's successors. Each entry still carries all of them, because
/// each transition step equates its own entry by itself.
pub fn verify_segment(cfs: &CfsCursor, segment: ControlFlowSegment) -> Vec<ControlFlowEntry> {
    assert!(!segment.steps.is_empty(), "a control-flow segment must cover at least one step");
    let mut frontier = deserialize_frontier(&segment.start_frontier)
        .expect("host must supply a valid segment start frontier");
    let mut expected_before = segment.expected_before;
    let mut entries = Vec::with_capacity(segment.steps.len());
    for step in segment.steps {
        let step_record_hash = hash_trace_item(&step.step_record);
        let input = ControlFlowInput {
            step_record: step.step_record,
            trace_position: frontier.position().into(),
            trace_root_before: frontier_root(&frontier),
            expected_before: expected_before.take(),
            input_source_witness: step.input_source_witness,
            sequence_scope_witness: step.sequence_scope_witness,
            sequence_scope_parent: step.sequence_scope_parent,
        };
        let expected_after = verify_step(cfs, &input);
        frontier.append(Bytes(step_record_hash.clone()));
        expected_before = Some(expected_after.clone());
        entries.push(ControlFlowEntry {
            step_record_hash,
            input_source_commitment: input.input_source_witness.as_ref().map(input_source_commitment),
            trace_position: input.trace_position,
            trace_root_before: input.trace_root_before,
            expected_before: input.expected_before,
            expected_after,
        });
    }
    entries
}

/// The control-flow check for one step. Returns the coordinates the next step
/// may take: empty after `ProgramEnd`, which nothing may follow.
///
/// Reads nothing but its arguments, so a future check that needs more has to
/// add it to [`ControlFlowInput`], where the transition guest has to supply it
/// and bind it through the journal.
pub fn verify_step(cfs: &CfsCursor, input: &ControlFlowInput) -> Vec<CfsCoordinates> {
    let record = &input.step_record;
    // Every binding check below reads the input source witness. The transition
    // guest anchors its own copy; this guest anchors the one it checks.
    verify_input_source_anchored(record, input.input_source_witness.as_ref());
    verify_exec_index(input.trace_position, record);
    verify_sequence_id(cfs, record);
    verify_sequence_scope_parent(
        cfs,
        record,
        input.sequence_scope_witness.as_ref(),
        input.sequence_scope_parent.as_ref(),
        &input.trace_root_before,
    );
    verify_step_record_inputs(
        cfs,
        record,
        input.input_source_witness.as_ref(),
        input.sequence_scope_witness.as_ref(),
    );
    // Both checks that this step was expected and computes its successors.
    // `ProgramEnd` is the unique terminal step: it must be expected, but nothing
    // may follow it.
    let next = get_next_expected_coordinates(cfs, record, input.expected_before.as_ref());
    if matches!(record.kind, StepKind::ProgramEnd(_)) {
        Vec::new()
    } else {
        next
    }
}

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

/// Hold a scope argument to the caller's: the same stored object, read through
/// the caller's selection followed by the path the CFS declares.
///
/// `selection.path` is compared, not `selector`: it is the path the selection
/// proof is pinned to (`verify_selection_witness`), and each side's proof folds
/// to the shared `source_root_hash`, so an exact path is what makes the
/// argument's value the declared part of the caller's — any other selection
/// under the same root (a sibling field) is refused. An index-sourced argument
/// declares no static path; it is held to the caller's prefix, and its index
/// values to their own citations.
///
/// A path with nothing to narrow (`path` empty, no index) is the old identity
/// check, unchanged. Narrowing an inline value cannot be checked here — the
/// guest has no selector over inline bytes — so it fails closed.
fn assert_scope_source(
    consumer: ResolvedSource<'_>,
    scope: ResolvedSource<'_>,
    path: &[SelectorSegment],
    indexed: bool,
) {
    if path.is_empty() && !indexed {
        return assert_same_source(consumer, scope);
    }
    let (ResolvedSource::Storage(consumer), ResolvedSource::Storage(scope)) = (consumer, scope)
    else {
        panic!(
            "Sequence scope input narrows its parameter by {:?}, but narrowing is only \
             verifiable on a stored value",
            path
        );
    };
    assert_eq!(
        (&consumer.coordinates, &consumer.commitment, &consumer.selection.source_root_hash),
        (&scope.coordinates, &scope.commitment, &scope.selection.source_root_hash),
        "Narrowed sequence scope input reads a different object than the caller passed",
    );
    let scope_path = scope.selection.path.segments.as_slice();
    let consumer_path = consumer.selection.path.segments.as_slice();
    if indexed {
        assert!(
            consumer_path.starts_with(scope_path),
            "Index-sourced sequence scope input selects {:?}, outside the caller's {:?}",
            consumer_path,
            scope_path,
        );
    } else {
        let expected: Vec<SelectorSegment> =
            scope_path.iter().chain(path.iter()).cloned().collect();
        assert_eq!(
            consumer_path,
            expected.as_slice(),
            "Narrowed sequence scope input selects a different part of the caller's value \
             than the CFS declares",
        );
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

pub fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    sequence_scope_witness: Option<&FnInput>,
) {
    // The program-boundary steps sit at the sequence root `[]`, which is not
    // itself a CFS item and binds no CFS inputs: `ProgramStart` binds
    // authorized external data and `ProgramEnd` commits the authorized output,
    // both checked in the transition guest's `checks::entrypoint` against
    // storage and the journal rather than against CFS input bindings.
    if step_record.coordinates().is_empty() {
        return;
    }

    // An iteration `site ++ [i]` is not a CFS item and binds no inputs of its
    // own; the site's bindings were checked at `[site]`. What it must satisfy
    // instead — chunking and recur progress — is checked at the recur call
    // site (`verify_recur_iteration_chunking`, `advance_recur_progress`).
    if cfs_cursor
        .try_get_recur_iteration_coordinates(step_record.coordinates())
        .is_some()
    {
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
    // guards for `storage_write`. `verify_input_source_anchored` refuses that witness outright,
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
            combine_merkle_level(level, &current, sibling.as_slice())
        } else {
            combine_merkle_level(level, sibling.as_slice(), &current)
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
/// fingerprinted `input_source_commitment` (`verify_input_source_anchored`),
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
/// The host supplies the one parent this reads
/// (`raster-prover::trace::witness_sequence_scope_parents`), and nothing else:
/// it used to ship a witness for every source a step's bindings resolved to, of
/// which this was the only one any check read.
pub fn verify_sequence_scope_parent(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    sequence_scope_witness: Option<&FnInput>,
    sequence_scope_parent: Option<&(StepRecord, StepRecordWitness)>,
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

    let (parent_record, witness) = sequence_scope_parent.unwrap_or_else(|| {
        panic!(
            "Step {:?} binds a sequence-scope input but no SequenceStart witness for frame {:?} \
             was supplied",
            step_record, parent_coordinates
        )
    });
    assert!(
        matches!(parent_record.kind, StepKind::SequenceStart { .. })
            && *parent_record.coordinates() == parent_coordinates,
        "Step {:?} binds a sequence-scope input but no SequenceStart witness for frame {:?} was \
         supplied: the parent names {:?} at {:?}",
        step_record,
        parent_coordinates,
        parent_record.kind,
        parent_record.coordinates,
    );

    // This step's own witness, not merely one for this parent: it folds to the
    // trace root of the step it was built for, and the frontier has grown by
    // then for any other step.
    assert_record_in_trace(parent_record, witness, trace_root);

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
    // An index-sourced value declares no static path (`InputBinding`): its
    // selector is not fixed by the program, so the path checks below hold it
    // only to its source's prefix.
    let indexed = !declared_indexes.is_empty();
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
        InputBinding::EntryArgument { path } => {
            // One of `main`'s entry arguments: it must be sourced from
            // the authorized entry object at the sequence root `[]` that
            // the `ProgramStart` step bound, and read through exactly the
            // path the CFS declares — the argument's name, then whatever it
            // selects. The selection proof itself is verified separately by
            // the storage checks; it pins `selection.path`, which is why that
            // is the path compared.
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
            if !indexed {
                assert_eq!(
                    storage_meta.selection.path.segments, *path,
                    "Entry-argument input for step {:?} arg {} selects a different part of the \
                     entry object than the CFS declares",
                    step_record, input_index,
                );
            }
        }
        InputBinding::SequenceScope { input_index, path } => {
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
            assert_scope_source(resolved_source, scope_source, path, indexed);
        }
        InputBinding::PriorItemOutput {
            intra_sequence_item_index,
            path,
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
            //
            // The walk composes the whole selector: what the written value is
            // read through (a nested return's path), then the argument's own
            // `path`. Where it cannot be followed only the argument's own path
            // is known, as the selection's trailing segments.
            let selected_path = storage_meta.selection.path.segments.as_slice();
            match cfs_cursor.resolve_value(
                parent_sequence_coordinates,
                step_input.value_binding(),
                &[],
            ) {
                Ok(resolved) => {
                    assert_eq!(
                        storage_meta.coordinates, resolved.coordinates,
                        "Storage input prior-item-output coordinates do not match expected CFS source",
                    );
                    if !indexed {
                        assert_eq!(
                            selected_path,
                            resolved.path.as_slice(),
                            "Prior-item-output input for step {:?} arg {} selects a different \
                             part of the source than the CFS declares",
                            step_record,
                            input_index,
                        );
                    }
                }
                Err(ResolveError::UnboundReturn(_) | ResolveError::RecurBodyParameter) => {
                    assert_prior_item_output_coordinates(
                        cfs_cursor,
                        parent_sequence_coordinates,
                        source_coordinate,
                        &storage_meta.coordinates,
                    );
                    if !indexed {
                        assert!(
                            selected_path.ends_with(path),
                            "Prior-item-output input for step {:?} arg {} selects {:?}, which \
                             does not end with the CFS path {:?}",
                            step_record,
                            input_index,
                            selected_path,
                            path,
                        );
                    }
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

