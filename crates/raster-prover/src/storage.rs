//! Host driver for the storage guest.
//!
//! The storage guest proves the storage transitions of a segment of
//! consecutive window steps and commits a `PartSegmentJournal`, producing the
//! two columns the transition part consumes. Segments depend on no other
//! receipt: the store before each step follows from the window's initial store
//! and the writes of the steps before it, all of which the host recorded. So
//! every segment can be proven at once.
//!
//! How many steps a segment covers is [`SegmentLength`]: the whole window by
//! default, or any fixed number — one being the per-step mode. The guests and
//! journals are the same either way. See `docs/proposals/storage-prover.md`.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use raster_core::fraud_window::{storage_state_digest, window_id, WindowChains};
use raster_core::storage_check::{
    selection_witnesses_commitment, StorageSegmentHeader, StorageState, StorageStep,
};
use raster_core::trace::StepRecord;
use raster_core::transition::{InitTransition, StorageEntry};

use crate::segments::{SegmentLength, ZkvmSegmentPo2};
use crate::trace::{
    frontier_root, serializable_frontier_from_trace_frontier,
    serializable_frontier_into_trace_frontier, Bytes, BytesHashable, SerializableFrontier,
    TraceTreeFrontier,
};
use crate::transition::{write_postcard, StepIo};
use crate::window::{prove_part_segment, ProvenPartSegment};
use crate::{STORAGE_GUEST_ELF, STORAGE_GUEST_ID};

/// The storage guest's image id, as the byte encoding journals carry.
pub fn storage_guest_image_id() -> Vec<u8> {
    STORAGE_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// One segment as the host writes it: the header frame, then one frame per
/// step, so the guest holds one step's witnesses at a time.
#[derive(Debug, Clone)]
pub struct StorageSegment {
    pub header: StorageSegmentHeader,
    pub steps: Vec<StorageStep>,
}

/// Prove the storage check for one segment: the header frame, then one frame
/// per step.
pub fn prove_storage_segment(
    segment: &StorageSegment,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> ProvenPartSegment {
    let mut builder = zkvm_segment_po2.env_builder();
    write_postcard(&mut builder, &segment.header);
    for step in &segment.steps {
        write_postcard(&mut builder, step);
    }
    let first = segment.steps.first().map(|step| step.step_record.coordinates.clone());
    prove_part_segment(
        STORAGE_GUEST_ELF,
        builder.build().unwrap(),
        format_args!("storage guest refused the segment starting at {first:?}"),
    )
}

/// `sha256(entry)`: the append log's leaf for a write, as the guest hashes it
/// (`guest_common::storage::storage_leaf_hash`).
fn storage_leaf_hash(entry: &StorageEntry) -> Vec<u8> {
    Sha256::digest(entry.to_bytes()).to_vec()
}

/// The store before each step of a window, tracked the way the storage guest
/// tracks it: the frontier starts at the window's initial storage frontier and
/// grows by one leaf per write, and the index root becomes each step's recorded
/// root after.
///
/// Nothing here is checked — the guest checks every step, and a host that
/// supplies the wrong start is refused at the first step's roots. This only
/// has to agree with an honest run.
struct StorageWalk {
    frontier: TraceTreeFrontier,
    index_root: Vec<u8>,
}

impl StorageWalk {
    fn new(initial_frontier: &SerializableFrontier, initial_index_root: &[u8]) -> Self {
        Self {
            frontier: serializable_frontier_into_trace_frontier(initial_frontier.clone())
                .expect("the window's initial storage frontier must deserialize"),
            index_root: initial_index_root.to_vec(),
        }
    }

    /// The store now, as the storage guest's entries and columns name it.
    fn state(&self) -> StorageState {
        StorageState {
            position: self.frontier.position().into(),
            root: frontier_root(&self.frontier),
            index_root: self.index_root.clone(),
        }
    }

    /// Move past `step_record`: append its write, if any, and take its index
    /// root after.
    fn advance(&mut self, step_record: &StepRecord, step_io: &StepIo) {
        if let Some(write) = step_io
            .storage_witness
            .as_ref()
            .and_then(|witness| witness.write.as_ref())
        {
            self.frontier.append(Bytes(storage_leaf_hash(&write.entry)));
        }
        if let Some(roots) = step_record.storage_roots() {
            self.index_root = roots.index_root_after.clone();
        }
    }
}

/// One step's storage input: the record and every witness the storage check
/// reads.
fn storage_step(step_record: &StepRecord, step_io: &StepIo) -> StorageStep {
    StorageStep {
        step_record: step_record.clone(),
        input_source_witness: step_io.input_source_witness.clone(),
        storage_selection_witnesses: step_io.storage_selection_witnesses.clone(),
        storage_witness: step_io.storage_witness.clone(),
    }
}

/// The store before each step of a window, as the storage part produces it
/// into the `StoreBefore` column: what the transition part is handed.
pub fn stores_before(
    init: &InitTransition,
    window: &[StepRecord],
    recorded_step_io: &HashMap<StepRecord, StepIo>,
) -> Vec<StorageState> {
    let mut walk = StorageWalk::new(&init.init_storage_frontier, &init.init_storage_index_root);
    window
        .iter()
        .map(|step_record| {
            let before = walk.state();
            walk.advance(step_record, step_io_for(recorded_step_io, step_record));
            before
        })
        .collect()
}

fn step_io_for<'a>(recorded_step_io: &'a HashMap<StepRecord, StepIo>, step_record: &StepRecord) -> &'a StepIo {
    recorded_step_io
        .get(step_record)
        .unwrap_or_else(|| panic!("Missing recorded I/O for transition step {:?}", step_record))
}

/// Split a window into storage segments, each starting from the store the
/// previous one ends in, and from the chains the steps before it fold to.
pub fn plan_storage_segments(
    init: &InitTransition,
    window: &[StepRecord],
    recorded_step_io: &HashMap<StepRecord, StepIo>,
    segments: SegmentLength,
) -> Vec<StorageSegment> {
    let mut walk = StorageWalk::new(&init.init_storage_frontier, &init.init_storage_index_root);
    let mut chains = WindowChains::new(&window_id(init));
    let first_position = init.init_frontier.position;
    let segment_len = segments.len_for(window.len());
    window
        .chunks(segment_len)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let header = StorageSegmentHeader {
                init: init.clone(),
                records_before: chains.records.value(),
                columns_before: [chains.store_before.value(), chains.selections.value()],
                start_frontier: serializable_frontier_from_trace_frontier(walk.frontier.clone()),
                start_index_root: walk.index_root.clone(),
                first_position: first_position + (chunk_index * segment_len) as u64,
                step_count: chunk.len() as u64,
            };
            let steps = chunk
                .iter()
                .map(|step_record| {
                    let step_io = step_io_for(recorded_step_io, step_record);
                    let step = storage_step(step_record, step_io);
                    chains.records.fold(&step_record.hash());
                    chains.store_before.fold(&storage_state_digest(&walk.state()));
                    chains
                        .selections
                        .fold(&selection_witnesses_commitment(&step_io.storage_selection_witnesses));
                    walk.advance(step_record, step_io);
                    step
                })
                .collect();
            StorageSegment { header, steps }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::CfsCoordinates;
    use raster_core::recur_progress::RecurProgressStack;
    use raster_core::trace::{ExecStep, ExecTarget, StepKind, StorageRoots};
    use raster_core::transition::{
        CoordinateIndexNonMembershipProof, IndexTerminal, StorageWitness, StorageWriteWitness,
    };

    fn empty_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: raster_core::merkle::EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        }
    }

    /// A tile step; `index_after` names its recorded index root after.
    fn tile_step(exec_index: u64, index_after: u8) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![exec_index as raster_core::cfs::CfsCoordinate]),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("tile".to_string()),
                intra_sequence_index: 1,
                input_commitment: Vec::new(),
                input_source_commitment: Vec::new(),
                output_commitment: Vec::new(),
                storage: StorageRoots {
                    root_before: Vec::new(),
                    root_after: Vec::new(),
                    index_root_before: Vec::new(),
                    index_root_after: vec![index_after; 32],
                },
            }),
            recur_progress_commitment: RecurProgressStack::new().commitment(),
            recur_state: None,
        }
    }

    fn writing(step: &StepRecord) -> StepIo {
        let entry = StorageEntry {
            coordinates: step.coordinates.clone(),
            object_commitment: vec![step.exec_index as u8; 32],
        };
        StepIo {
            storage_witness: Some(StorageWitness {
                reads: Vec::new(),
                write: Some(StorageWriteWitness {
                    entry: entry.clone(),
                    index_non_membership_witness: CoordinateIndexNonMembershipProof {
                        coordinates: entry.coordinates.clone(),
                        terminal: IndexTerminal::Empty,
                        siblings: Vec::new(),
                    },
                }),
            }),
            ..StepIo::default()
        }
    }

    /// Seven tile steps; the odd ones write.
    fn window() -> (Vec<StepRecord>, HashMap<StepRecord, StepIo>) {
        let steps: Vec<StepRecord> = (1..=7).map(|exec| tile_step(exec, exec as u8)).collect();
        let io = steps
            .iter()
            .map(|step| {
                let io = if step.exec_index % 2 == 1 { writing(step) } else { StepIo::default() };
                (step.clone(), io)
            })
            .collect();
        (steps, io)
    }

    /// A seven-step window opening at `frontier`, over `storage_frontier`.
    fn window_init(frontier: SerializableFrontier, storage_frontier: SerializableFrontier) -> InitTransition {
        InitTransition {
            init_frontier: frontier,
            init_storage_frontier: storage_frontier,
            init_storage_root: vec![0; 32],
            init_storage_index_root: vec![0; 32],
            fingerprint: raster_core::fingerprint::Fingerprint::from(
                vec![0],
                raster_core::fingerprint::BitPacker::new(8),
                7,
            ),
        }
    }

    fn plan(segments: SegmentLength) -> Vec<StorageSegment> {
        let (steps, io) = window();
        let mut frontier = empty_frontier();
        frontier.position = 20;
        plan_storage_segments(&window_init(frontier, empty_frontier()), &steps, &io, segments)
    }

    /// The modes differ only in how the window is cut: one segment, one per
    /// step, or chunks with a shorter tail.
    #[test]
    fn a_window_is_cut_into_segments_by_mode() {
        let lens = |segments| {
            plan(segments).iter().map(|s| s.steps.len()).collect::<Vec<_>>()
        };
        assert_eq!(lens(SegmentLength::Window), vec![7]);
        assert_eq!(lens(SegmentLength::from_len(1)), vec![1; 7]);
        assert_eq!(lens(SegmentLength::from_len(3)), vec![3, 3, 1]);
        for segment in plan(SegmentLength::from_len(3)) {
            assert_eq!(segment.header.step_count, segment.steps.len() as u64);
        }
    }

    /// Each segment starts where the previous one ended: the frontier has grown
    /// by the writes before it, the index root is the last step's root after,
    /// and the position is the trace position of its first step.
    #[test]
    fn each_segment_starts_where_the_previous_one_ended() {
        let segments = plan(SegmentLength::from_len(3));
        let starts: Vec<_> = segments
            .iter()
            .map(|segment| {
                (
                    segment.header.first_position,
                    segment.header.start_frontier.position,
                    segment.header.start_index_root.clone(),
                )
            })
            .collect();
        // Steps 1 and 3 write before the second segment; 5 after.
        assert_eq!(starts[0], (20, 0, vec![0; 32]));
        assert_eq!(starts[1], (23, 2, vec![3; 32]));
        assert_eq!(starts[2], (26, 3, vec![6; 32]));

        let whole = plan(SegmentLength::Window);
        assert_eq!(whole[0].header.start_frontier, segments[0].header.start_frontier);
        assert_eq!(whole[0].header.first_position, 20);
    }

    /// The host's leaf is the guest's: a write appended here folds to the root
    /// the guest computes from the same entry.
    #[test]
    fn a_write_appends_the_entry_hash() {
        let (steps, io) = window();
        let mut walk = StorageWalk::new(&empty_frontier(), &[0; 32]);
        walk.advance(&steps[0], &io[&steps[0]]);
        let entry = &io[&steps[0]].storage_witness.as_ref().unwrap().write.as_ref().unwrap().entry;
        assert_eq!(walk.frontier.leaf().0, Sha256::digest(postcard::to_allocvec(entry).unwrap()).to_vec());
        assert_eq!(u64::from(walk.frontier.position()), 1);
    }
}
