//! Host driver for the storage guest.
//!
//! The storage guest proves the storage transitions of a segment of
//! consecutive transition steps, and every transition step in the segment takes
//! that receipt as an assumption. Segments depend on no transition receipt: the
//! store before each step follows from the window's initial store and the
//! writes of the steps before it, all of which the host recorded. So every
//! segment can be proven before the transition chain starts.
//!
//! How many steps a segment covers is [`SegmentLength`]: the whole window by
//! default, or any fixed number — one being the per-step mode. The guests and
//! journals are the same either way. See `docs/proposals/storage-prover.md`.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use raster_core::storage_check::{
    entries_root, entry_path, StorageEvidence, StorageJournal, StorageSegmentHeader, StorageStep,
    StorageStepEntry,
};
use raster_core::trace::StepRecord;
use raster_core::transition::StorageEntry;

use crate::segments::SegmentLength;
use crate::trace::{
    serializable_frontier_from_trace_frontier, serializable_frontier_into_trace_frontier, Bytes,
    SerializableFrontier, TraceTreeFrontier,
};
use crate::transition::{write_postcard, StepIo};
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

/// One proven segment: its receipt, its journal, and the entries the journal's
/// root is over.
pub struct ProvenStorageSegment {
    pub receipt: risc0_zkvm::Receipt,
    pub journal: StorageJournal,
    pub entries: Vec<StorageStepEntry>,
}

impl ProvenStorageSegment {
    /// The evidence for the segment's entry `index`: the journal, the entry,
    /// and its path to the journal's root.
    pub fn evidence(&self, index: usize) -> StorageEvidence {
        let leaves: Vec<_> = self.entries.iter().map(StorageStepEntry::leaf_hash).collect();
        StorageEvidence {
            journal: self.journal.clone(),
            entry: self.entries[index].clone(),
            path: entry_path(&leaves, index),
        }
    }
}

/// Prove the storage check for one segment.
///
/// The guest commits only a root over its entries and writes the entries to
/// stdout. They are not trusted: this refuses them unless they fold to the
/// committed root, and every transition step checks its own against it again.
pub fn prove_storage_segment(segment: &StorageSegment) -> ProvenStorageSegment {
    let prover = risc0_zkvm::default_prover();
    let mut stdout = Vec::new();
    let receipt = {
        // Scoped: the builder borrows `stdout` until it is dropped.
        let mut builder = risc0_zkvm::ExecutorEnv::builder();
        write_postcard(&mut builder, &segment.header);
        for step in &segment.steps {
            write_postcard(&mut builder, step);
        }
        builder.stdout(&mut stdout);
        let env = builder.build().unwrap();
        prover
            .prove(env, &STORAGE_GUEST_ELF)
            .unwrap_or_else(|e| {
                let first = segment.steps.first().map(|step| &step.step_record.coordinates);
                panic!("storage guest refused the segment starting at {first:?}: {e}")
            })
            .receipt
    };
    let journal = StorageJournal::decode(&receipt.journal.bytes)
        .expect("the storage guest commits a postcard-encoded journal");
    let entries: Vec<StorageStepEntry> =
        postcard::from_bytes(&stdout).expect("the storage guest writes its entries to stdout");
    let leaves: Vec<_> = entries.iter().map(StorageStepEntry::leaf_hash).collect();
    assert_eq!(
        (entries.len() as u64, entries_root(&leaves)),
        (journal.entry_count, journal.entries_root),
        "the storage guest's entries are not the ones its journal commits",
    );
    ProvenStorageSegment {
        receipt,
        journal,
        entries,
    }
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

/// Split a window into storage segments, each starting from the store the
/// previous one ends in. `first_position` is the trace position of the
/// window's first step: the initial trace frontier's position.
pub fn plan_storage_segments(
    initial_storage_frontier: &SerializableFrontier,
    initial_storage_index_root: &[u8],
    first_position: u64,
    window: &[StepRecord],
    recorded_step_io: &HashMap<StepRecord, StepIo>,
    segments: SegmentLength,
) -> Vec<StorageSegment> {
    let mut walk = StorageWalk::new(initial_storage_frontier, initial_storage_index_root);
    let segment_len = segments.len_for(window.len());
    window
        .chunks(segment_len)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            let header = StorageSegmentHeader {
                start_frontier: serializable_frontier_from_trace_frontier(walk.frontier.clone()),
                start_index_root: walk.index_root.clone(),
                first_position: first_position + (chunk_index * segment_len) as u64,
                step_count: chunk.len() as u64,
            };
            let steps = chunk
                .iter()
                .map(|step_record| {
                    let step_io = recorded_step_io.get(step_record).unwrap_or_else(|| {
                        panic!("Missing recorded I/O for transition step {:?}", step_record)
                    });
                    let step = storage_step(step_record, step_io);
                    walk.advance(step_record, step_io);
                    step
                })
                .collect();
            StorageSegment { header, steps }
        })
        .collect()
}

/// The proven segments of a window, and which one covers each step.
pub struct ProvenStorage {
    segment_len: usize,
    segments: Vec<ProvenStorageSegment>,
}

impl ProvenStorage {
    /// Plan and prove every segment of `window`.
    pub fn prove(
        initial_storage_frontier: &SerializableFrontier,
        initial_storage_index_root: &[u8],
        first_position: u64,
        window: &[StepRecord],
        recorded_step_io: &HashMap<StepRecord, StepIo>,
        segments: SegmentLength,
    ) -> Self {
        let planned = plan_storage_segments(
            initial_storage_frontier,
            initial_storage_index_root,
            first_position,
            window,
            recorded_step_io,
            segments,
        );
        Self {
            segment_len: segments.len_for(window.len()),
            segments: planned.iter().map(prove_storage_segment).collect(),
        }
    }

    /// The receipt covering the window's step at `index`, and that step's
    /// evidence.
    pub fn for_step(&self, index: usize) -> (&risc0_zkvm::Receipt, StorageEvidence) {
        let segment = &self.segments[index / self.segment_len];
        (&segment.receipt, segment.evidence(index % self.segment_len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::CfsCoordinates;
    use raster_core::recur_progress::RecurProgressStack;
    use raster_core::trace::{ExecStep, ExecTarget, StepKind, StorageRoots};
    use raster_core::transition::{
        CoordinateIndexMembershipProof, CoordinateIndexNonMembershipProof, StorageIndexValue,
        StorageWitness, StorageWriteWitness,
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
                        siblings: Vec::new(),
                    },
                    index_membership_witness: CoordinateIndexMembershipProof {
                        coordinates: entry.coordinates.clone(),
                        value: StorageIndexValue {
                            log_position: 0,
                            object_commitment: entry.object_commitment.clone(),
                        },
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

    fn plan(segments: SegmentLength) -> Vec<StorageSegment> {
        let (steps, io) = window();
        plan_storage_segments(&empty_frontier(), &[0; 32], 20, &steps, &io, segments)
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
