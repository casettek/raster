//! The storage check over a segment of steps.

use guest_common::input_source::input_source_commitment;
use guest_common::merkle_tree::hash_trace_item;
use guest_common::storage::{StorageCursor, StorageInput};
use raster_core::storage_check::{
    selection_witnesses_commitment, StorageSegmentHeader, StorageStep, StorageStepEntry,
};

/// Check every step of a segment in order and return one entry per step.
///
/// Only the store before the first step is host-supplied; every later step
/// starts from the store the previous one left. Each entry's `before` is
/// therefore the previous entry's `after`, and the transition guest equates
/// every `before` with its own state, so the start is checked too. Steps are
/// pulled from `next_step` one at a time, so each is dropped before the next
/// is read.
pub fn verify_segment(
    header: StorageSegmentHeader,
    mut next_step: impl FnMut() -> StorageStep,
) -> Vec<StorageStepEntry> {
    assert!(header.step_count > 0, "a storage segment must cover at least one step");
    let mut store = StorageCursor::new(&header.start_frontier, header.start_index_root);
    let mut entries = Vec::with_capacity(header.step_count as usize);
    for offset in 0..header.step_count {
        let step = next_step();
        let before = store.state();
        store.verify_step(&StorageInput {
            step_record: &step.step_record,
            input_source_witness: step.input_source_witness.as_ref(),
            storage_selection_witnesses: &step.storage_selection_witnesses,
            storage_witness: step.storage_witness.as_ref(),
        });
        entries.push(StorageStepEntry {
            step_record_hash: hash_trace_item(&step.step_record),
            input_source_commitment: step.input_source_witness.as_ref().map(input_source_commitment),
            selection_witnesses_commitment: selection_witnesses_commitment(
                &step.storage_selection_witnesses,
            ),
            trace_position: header.first_position + offset,
            before,
            after: store.state(),
        });
    }
    entries
}
