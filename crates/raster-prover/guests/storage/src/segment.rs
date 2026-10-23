//! The storage check over a segment of steps.

use guest_common::input_source::input_source_commitment;
use guest_common::merkle_tree::{deserialize_frontier, frontier_root, hash_trace_item};
use guest_common::storage::{StorageCursor, StorageInput};
use raster_core::fraud_window::{
    storage_state_digest, Chain, ChainKind, Column, PartKind, PartSegmentJournal, Span,
};
use raster_core::input::Hash32;
use raster_core::storage_check::{
    selection_witnesses_commitment, StorageSegmentHeader, StorageStep,
    StorageStepEntry,
};

/// Check every step of a segment in order and return one entry per step.
///
/// Only the store before the first step is host-supplied; every later step
/// starts from the store the previous one left. The opening segment checks its
/// start against the window; any other segment's start is its state before,
/// which FRAUD WINDOW stitches to the state the segment before it ended on.
/// Steps are
/// pulled from `next_step` one at a time, so each is dropped before the next
/// is read.
pub fn verify_segment(
    header: StorageSegmentHeader,
    mut next_step: impl FnMut() -> StorageStep,
) -> Vec<StorageStepEntry> {
    assert!(header.step_count > 0, "a storage segment must cover at least one step");
    let init = &header.init;
    if header.first_position == init.init_frontier.position {
        // The segment opening the window starts from the window's initial
        // store, and that store's root is the one the window names.
        assert!(
            header.start_frontier == init.init_storage_frontier
                && header.start_index_root == init.init_storage_index_root,
            "a segment opening the window must start from the window's initial store",
        );
        let frontier = deserialize_frontier(&init.init_storage_frontier)
            .expect("host must supply a valid storage frontier");
        assert_eq!(
            frontier_root(&frontier),
            init.init_storage_root,
            "Initial storage root does not match initial storage frontier",
        );
    }
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

/// What the segment commits: the record chain, the store before and after, the
/// two columns it produces — the store before each step and each step's
/// selection commitment.
pub fn segment_journal(
    window: Hash32,
    records_before: Hash32,
    columns_before: [Hash32; 2],
    entries: &[StorageStepEntry],
) -> PartSegmentJournal {
    let mut records = Chain::resume(ChainKind::Records, records_before);
    let [store_before_chain, selections_chain] = columns_before;
    let mut store_before = Chain::resume(ChainKind::Column(Column::StoreBefore), store_before_chain);
    let mut selections = Chain::resume(ChainKind::Column(Column::Selections), selections_chain);
    for entry in entries {
        records.fold(&entry.step_record_hash);
        store_before.fold(&storage_state_digest(&entry.before));
        selections.fold(&entry.selection_witnesses_commitment);
    }
    let first = entries.first().expect("a segment covers at least one step");
    let last = entries.last().expect("a segment covers at least one step");
    PartSegmentJournal {
        part: PartKind::Storage,
        window_id: window,
        program_commitment: None,
        first_position: first.trace_position,
        step_count: entries.len() as u64,
        records: Span {
            before: records_before,
            after: records.value(),
        },
        state: Span {
            before: storage_state_digest(&first.before),
            after: storage_state_digest(&last.after),
        },
        columns: vec![
            (
                Column::StoreBefore,
                Span {
                    before: store_before_chain,
                    after: store_before.value(),
                },
            ),
            (
                Column::Selections,
                Span {
                    before: selections_chain,
                    after: selections.value(),
                },
            ),
        ],
        out: None,
    }
}
