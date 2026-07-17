//! Checks that a step record matches the control flow schema: that the
//! record's kind matches the item declared at its coordinates, that its
//! per-argument input bindings are honoured, and that its coordinates
//! follow the schema's ordering.

use raster_core::cfs::{CfsCoordinates, CfsCursor, InputBinding, InputSource, SequenceChildItem};
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
        FnInputValue::StorageBinding => {
            ResolvedSource::Storage(input.storage().get(&arg.name).unwrap_or_else(|| {
                panic!("Missing storage input metadata for arg '{}'", arg.name)
            }))
        }
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
    match (&step_record.kind, cfs_item) {
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(_),
                ..
            }),
            SequenceChildItem::Tile(_),
        ) => true,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurTile(_),
                ..
            }),
            SequenceChildItem::RecurTile(_),
        ) => true,
        (
            StepKind::Exec(ExecStep {
                target: ExecTarget::RecurSequence(_),
                ..
            }),
            SequenceChildItem::RecurSequence(_),
        ) => true,
        // A nested sequence is entered and left at its own item coordinate,
        // whether it is an ordinary or a recur sequence.
        (
            StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. },
            SequenceChildItem::Sequence(_) | SequenceChildItem::RecurSequence(_),
        ) => true,
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
    // both checked in `checks::entrypoint` against storage/the journal rather
    // than against CFS input bindings.
    if step_record.coordinates().is_empty() {
        return;
    }

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
        match step_input {
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
                assert!(
                    *intra_sequence_item_index < item_coordinate as usize,
                    "Step {:?} cannot depend on source item {} from the same or a future position {}",
                    step_record,
                    intra_sequence_item_index,
                    item_coordinate
                );

                let mut source_coordinates = parent_sequence_coordinates.clone();
                source_coordinates.push(
                    (*intra_sequence_item_index)
                        .try_into()
                        .expect("Prior item output index exceeds CFS coordinate bounds"),
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
            "Step coordinates are not in expected next coordinates"
        );
    }

    cfs_cursor
        .try_get_next_coordinates(coordinates)
        .expect("Wrong tile coordinates")
}
