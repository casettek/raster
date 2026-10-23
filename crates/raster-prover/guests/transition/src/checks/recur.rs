//! The recur checks: chunking, and the recur-progress stack every recur step
//! advances — rule 8's item selection, the carried-state chain, the site draft,
//! the returned state. Unlike the control-flow check these read the replay
//! journal, storage roots and the draft step, which is why they live in the
//! transition part rather than the control-flow guest. See
//! `docs/proposals/control-flow-prover.md` step 5.

use std::collections::BTreeMap;

use raster_core::cfs::{
    CfsCursor, InputBinding, RecurOutputDecl, SequenceChildItem, FIRST_COORDINATE,
};
use raster_core::draft::TileReplayJournal;
use raster_core::input::{
    verify_selection_reference_witness, verify_selection_witness, Hash32, SelectionPayloadKind,
    SelectionWitness,
};
use raster_core::recur_progress::{
    source_identity, DraftStep, RecurProgressStack, RecurSiteKind, SiteDraft,
};
use raster_core::storage_check::StorageState;
use raster_core::trace::{FnInput, FnInputValue, StepKind, StepRecord, StorageData};
use raster_core::transition::StorageReadWitness;

use crate::checks::columns::{Consumed, Selections};

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
///
/// A recur rule, not a binding rule: it reads the replay journal, which only
/// `checks::io` authenticates, so it runs at the recur call site next to
/// `advance_recur_progress` rather than inside the CFS binding check. A step
/// that is not a recur iteration passes untouched.
pub fn verify_recur_iteration_chunking(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    replay_journal: Option<&TileReplayJournal>,
) {
    let Some((site_coordinates, _)) =
        cfs_cursor.try_get_recur_iteration_coordinates(step_record.coordinates())
    else {
        return;
    };
    let Some(raster_core::cfs::SequenceChildItem::RecurTile(item)) =
        cfs_cursor.try_get_item(&site_coordinates)
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

/// The source length carried by a recur site's `Start` step.
///
/// `Start` records the source under the binding name `"input"`, whose selection
/// is the `0x0A` list-metadata payload (`lazy-list-recur.md` §1–§2). `L` has to
/// exist before iteration 0 is checked against it, which is why the site needs
/// a `Start` at all.
///
/// **Authenticated by the same step, not by this function.** A site `Start`
/// must claim read-only storage roots (`advance_recur_progress`), so the
/// storage guest reads the source object against the store and folds this
/// metadata witness to its commitment, and the `Selections` column ties this
/// part's witness map to the one it folded. Any failure rejects the step, so the `L`
/// read here only stands if they pass.
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
pub(crate) fn check_iteration_state_binding(
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
pub(crate) fn verify_returned_state(
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
    if matches!(returns.source, InputBinding::SequenceScope { input_index: 1, .. })
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
        resolved.path.is_empty(),
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
    guest_common::storage::verify_storage_read_witness(
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
/// its selection witness, folded by the storage guest — the `Selections` column
/// ties this part's witness map to the one it folded.
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
/// stored object — read from storage by the storage guest, since `RecurStart`
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
/// storage by the storage guest, since `RecurStart` carries storage roots.
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
            derived: false,
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
        derived: true,
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
    // Both consumed from the storage check (`checks::columns`).
    selections: &Selections<'_>,
    returned_state_read_witness: Option<&StorageReadWitness>,
    // No step that reads a returned state writes, so the store before the
    // step is the one the read is against.
    store_before: &Consumed<StorageState>,
) {
    let storage_selection_witnesses: &BTreeMap<String, SelectionWitness> = selections.get();
    let current_storage_root: &[u8] = &store_before.get().root;
    let current_index_root: &[u8] = &store_before.get().index_root;
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
                    // no storage roots and the storage guest never folds its
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
                // kind carries read-only storage roots, so the storage guest
                // reads the source object and folds its metadata witness for
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
