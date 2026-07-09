//! Checks that a step record matches the control flow schema: coordinate
//! ordering and per-argument input bindings.

use raster_core::cfs::{CfsCoordinates, CfsCursor, InputBinding, InputSource};
use raster_core::trace::{ExternalData, FnInput, FnInputValue, InternalData, StepRecord};

enum ResolvedSource<'a> {
    Inline(&'a Vec<u8>),
    External(&'a ExternalData),
    Internal(&'a InternalData),
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
        FnInputValue::ExternalBinding => {
            ResolvedSource::External(input.external().get(&arg.name).unwrap_or_else(|| {
                panic!("Missing external input metadata for arg '{}'", arg.name)
            }))
        }
        FnInputValue::InternalBinding => {
            ResolvedSource::Internal(input.internal().get(&arg.name).unwrap_or_else(|| {
                panic!("Missing internal input metadata for arg '{}'", arg.name)
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
        (ResolvedSource::External(left_meta), ResolvedSource::External(right_meta)) => {
            assert_eq!(
                left_meta, right_meta,
                "External sequence scope input does not match consumer binding",
            );
        }
        (ResolvedSource::Internal(left_meta), ResolvedSource::Internal(right_meta)) => {
            assert_eq!(
                left_meta, right_meta,
                "Internal sequence scope input does not match consumer binding",
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

pub fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    sequence_scope_witness: Option<&FnInput>,
) {
    // TODO: SequenceStart/SequenceEnd entrypoint case. In case of SequenceStart input is External Kind from
    // cli or from file. SequenceEnd just binding latest executed tile output.
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
            InputBinding::Direct(InputSource::External) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::External(_)),
                    "Expected external input source for step {:?} arg {}",
                    step_record,
                    input_index,
                );
            }
            InputBinding::Direct(InputSource::Inline) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::Inline(_)),
                    "Expected inline input source for step {:?} arg {}",
                    step_record,
                    input_index,
                );
            }
            InputBinding::Direct(InputSource::Internal) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::Internal(_)),
                    "Expected internal input source for step {:?} arg {}",
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
                let internal_meta = match resolved_source {
                    ResolvedSource::Internal(meta) => meta,
                    _ => {
                        panic!(
                            "Expected internal input source for step {:?} arg {}",
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
                            has_coordinate_prefix(&internal_meta.coordinates, &source_coordinates),
                            "Internal input prior-item-output coordinates do not descend from expected sequence source",
                        );
                    }
                    raster_core::cfs::SequenceChildItem::Tile(_)
                    | raster_core::cfs::SequenceChildItem::RecurTile(_) => {
                        assert_eq!(
                            internal_meta.coordinates, source_coordinates,
                            "Internal input prior-item-output coordinates do not match expected CFS source",
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
