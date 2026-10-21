//! Host driver for the control-flow guest.
//!
//! The control-flow guest proves the CFS conformance of a segment of
//! consecutive transition steps, and every transition step in the segment takes
//! that receipt as an assumption. Segments depend on no transition receipt: the
//! trace frontier advances by each record's hash, and the coordinates a step may
//! take follow from the previous record and the CFS. So every segment can be
//! proven before the transition chain starts.
//!
//! How many steps a segment covers is [`SegmentLength`]: the whole window
//! by default, or any fixed number — one being the per-step mode. The guests
//! and journals are the same either way. See
//! `docs/proposals/control-flow-prover.md` §Segments.

use std::collections::{BTreeMap, HashMap};

use raster_core::cfs::{CfsCoordinates, CfsCursor};
use raster_core::control_flow::{
    entries_root, entry_path, expected_successors, ControlFlowEntry, ControlFlowEvidence,
    ControlFlowJournal, ControlFlowSegment, ControlFlowStep,
};
use raster_core::program::ProgramDefinition;
use raster_core::trace::StepRecord;
use raster_core::transition::StepRecordWitness;

use crate::trace::{
    serializable_frontier_from_trace_frontier, serializable_frontier_into_trace_frontier, Bytes,
    BytesHashable, SerializableFrontier, TraceTreeFrontier,
};
use crate::segments::SegmentLength;
use crate::transition::{write_frame, write_postcard, StepIo};
use crate::{CONTROL_FLOW_GUEST_ELF, CONTROL_FLOW_GUEST_ID};

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// The control-flow guest's image id, as the byte encoding journals carry.
pub fn control_flow_guest_image_id() -> Vec<u8> {
    image_id_bytes(CONTROL_FLOW_GUEST_ID)
}

/// One proven segment: its receipt, its journal, and the entries the journal's
/// root is over.
pub struct ProvenSegment {
    pub receipt: risc0_zkvm::Receipt,
    pub journal: ControlFlowJournal,
    pub entries: Vec<ControlFlowEntry>,
}

impl ProvenSegment {
    /// The evidence for the segment's entry `index`: the journal, the entry,
    /// and its path to the journal's root.
    pub fn evidence(&self, index: usize) -> ControlFlowEvidence {
        let leaves: Vec<_> = self.entries.iter().map(ControlFlowEntry::leaf_hash).collect();
        ControlFlowEvidence {
            journal: self.journal.clone(),
            entry: self.entries[index].clone(),
            path: entry_path(&leaves, index),
        }
    }
}

/// Prove the control-flow check for one segment. The host write order is the
/// one the guest reads: the program frame, then the segment.
///
/// The guest commits only a root over its entries and writes the entries to
/// stdout. They are not trusted: this refuses them unless they fold to the
/// committed root, and every transition step checks its own against it again.
pub fn prove_control_flow_segment(
    program_frame: &[u8],
    segment: &ControlFlowSegment,
) -> ProvenSegment {
    let prover = risc0_zkvm::default_prover();
    let mut stdout = Vec::new();
    let receipt = {
        // Scoped: the builder borrows `stdout` until it is dropped.
        let mut builder = risc0_zkvm::ExecutorEnv::builder();
        write_frame(&mut builder, program_frame);
        write_postcard(&mut builder, segment);
        builder.stdout(&mut stdout);
        let env = builder.build().unwrap();
        prover
            .prove(env, &CONTROL_FLOW_GUEST_ELF)
            .unwrap_or_else(|e| {
                let first = segment.steps.first().map(|step| &step.step_record.coordinates);
                panic!("control-flow guest refused the segment starting at {first:?}: {e}")
            })
            .receipt
    };
    let journal = ControlFlowJournal::decode(&receipt.journal.bytes)
        .expect("the control-flow guest commits a postcard-encoded journal");
    let entries: Vec<ControlFlowEntry> = postcard::from_bytes(&stdout)
        .expect("the control-flow guest writes its entries to stdout");
    let leaves: Vec<_> = entries.iter().map(ControlFlowEntry::leaf_hash).collect();
    assert_eq!(
        (entries.len() as u64, entries_root(&leaves)),
        (journal.entry_count, journal.entries_root),
        "the control-flow guest's entries are not the ones its journal commits",
    );
    ProvenSegment {
        receipt,
        journal,
        entries,
    }
}

/// The trace state before each step of a window, tracked the way the
/// transition guest tracks it: the frontier starts at the window's initial
/// frontier and grows by one record hash per step, and the expected coordinates
/// start at `None` — the transition guest's state at `Init` — and become each
/// step's successor set.
struct ControlFlowWalk {
    cfs: CfsCursor,
    frontier: TraceTreeFrontier,
    expected: Option<Vec<CfsCoordinates>>,
}

impl ControlFlowWalk {
    fn new(program_frame: &[u8], initial_frontier: &SerializableFrontier) -> Self {
        let program = ProgramDefinition::decode(program_frame)
            .expect("the program frame must decode as a ProgramDefinition");
        Self {
            cfs: CfsCursor::new(program.cfs),
            frontier: serializable_frontier_into_trace_frontier(initial_frontier.clone())
                .expect("the window's initial frontier must deserialize"),
            expected: None,
        }
    }

    /// Move past `step_record`: append its hash and take its successors.
    fn advance(&mut self, step_record: &StepRecord) {
        self.expected = Some(expected_successors(&self.cfs, step_record).unwrap_or_else(|| {
            panic!(
                "step {:?} has no successors in the CFS",
                step_record.coordinates
            )
        }));
        self.frontier.append(Bytes(step_record.hash()));
    }
}

/// One step's control-flow input. Its scope parent is the one keyed by its own
/// `exec_index`: that witness folds to this step's trace root and no other.
fn control_flow_step(
    step_record: &StepRecord,
    step_io: &StepIo,
    sequence_scope_parents: &BTreeMap<u64, (StepRecord, StepRecordWitness)>,
) -> ControlFlowStep {
    ControlFlowStep {
        step_record: step_record.clone(),
        input_source_witness: step_io.input_source_witness.clone(),
        sequence_scope_witness: step_io.sequence_scope_witness.clone(),
        sequence_scope_parent: sequence_scope_parents.get(&step_record.exec_index).cloned(),
    }
}

/// Split a window into control-flow segments, each starting from the trace
/// state the previous one ends in.
pub fn plan_control_flow_segments(
    program_frame: &[u8],
    initial_frontier: &SerializableFrontier,
    window: &[StepRecord],
    recorded_step_io: &HashMap<StepRecord, StepIo>,
    sequence_scope_parents: &BTreeMap<u64, (StepRecord, StepRecordWitness)>,
    segments: SegmentLength,
) -> Vec<ControlFlowSegment> {
    let mut walk = ControlFlowWalk::new(program_frame, initial_frontier);
    window
        .chunks(segments.len_for(window.len()))
        .map(|chunk| {
            let start_frontier = serializable_frontier_from_trace_frontier(walk.frontier.clone());
            let expected_before = walk.expected.clone();
            let steps = chunk
                .iter()
                .map(|step_record| {
                    let step_io = recorded_step_io.get(step_record).unwrap_or_else(|| {
                        panic!("Missing recorded I/O for transition step {:?}", step_record)
                    });
                    let step = control_flow_step(step_record, step_io, sequence_scope_parents);
                    walk.advance(step_record);
                    step
                })
                .collect();
            ControlFlowSegment {
                start_frontier,
                expected_before,
                steps,
            }
        })
        .collect()
}

/// The proven segments of a window, and which one covers each step.
pub struct ProvenControlFlow {
    segment_len: usize,
    segments: Vec<ProvenSegment>,
}

impl ProvenControlFlow {
    /// Plan and prove every segment of `window`.
    pub fn prove(
        program_frame: &[u8],
        initial_frontier: &SerializableFrontier,
        window: &[StepRecord],
        recorded_step_io: &HashMap<StepRecord, StepIo>,
        sequence_scope_parents: &BTreeMap<u64, (StepRecord, StepRecordWitness)>,
        segments: SegmentLength,
    ) -> Self {
        let planned = plan_control_flow_segments(
            program_frame,
            initial_frontier,
            window,
            recorded_step_io,
            sequence_scope_parents,
            segments,
        );
        Self {
            segment_len: segments.len_for(window.len()),
            segments: planned
                .iter()
                .map(|segment| prove_control_flow_segment(program_frame, segment))
                .collect(),
        }
    }

    /// The receipt covering the window's step at `index`, and that step's
    /// evidence.
    pub fn for_step(&self, index: usize) -> (&risc0_zkvm::Receipt, ControlFlowEvidence) {
        let segment = &self.segments[index / self.segment_len];
        (&segment.receipt, segment.evidence(index % self.segment_len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::{CfsCoordinate, ControlFlowSchema, SequenceDef};
    use raster_core::program::ProgramManifest;
    use raster_core::recur_progress::RecurProgressStack;
    use raster_core::trace::{ExecStep, ExecTarget, StepKind, StorageRoots};

    fn tile_step(exec_index: u64, coordinates: Vec<CfsCoordinate>) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("shared_tile".to_string()),
                intra_sequence_index: 1,
                input_commitment: Vec::new(),
                input_source_commitment: Vec::new(),
                output_commitment: Vec::new(),
                storage: StorageRoots {
                    root_before: Vec::new(),
                    root_after: Vec::new(),
                    index_root_before: Vec::new(),
                    index_root_after: Vec::new(),
                },
            }),
            recur_progress_commitment: RecurProgressStack::new().commitment(),
            recur_state: None,
        }
    }

    /// A minimal program: no tiles, a unit `main`.
    fn program_frame() -> Vec<u8> {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.sequences.push(SequenceDef::new("main"));
        let manifest = ProgramManifest {
            name: "test".to_string(),
            version: "0.0.0".to_string(),
            inputs: Default::default(),
            output: None,
        };
        ProgramDefinition::assemble(manifest, cfs, Default::default())
            .expect("minimal program definition")
            .canonical_bytes()
    }

    fn seed_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: raster_core::merkle::EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        }
    }

    /// Seven steps at the root, each with recorded I/O.
    fn window() -> (Vec<StepRecord>, HashMap<StepRecord, StepIo>) {
        let steps: Vec<StepRecord> = (1..=7).map(|exec| tile_step(exec, vec![])).collect();
        let io = steps.iter().map(|step| (step.clone(), StepIo::default())).collect();
        (steps, io)
    }

    fn plan(segments: SegmentLength) -> Vec<ControlFlowSegment> {
        let (steps, io) = window();
        plan_control_flow_segments(
            &program_frame(),
            &seed_frontier(),
            &steps,
            &io,
            &BTreeMap::new(),
            segments,
        )
    }

    /// The modes differ only in how the window is cut: one segment, one per
    /// step, or chunks with a shorter tail.
    #[test]
    fn a_window_is_cut_into_segments_by_mode() {
        let lens = |segments| plan(segments).iter().map(|s| s.steps.len()).collect::<Vec<_>>();
        assert_eq!(lens(SegmentLength::Window), vec![7]);
        assert_eq!(lens(SegmentLength::from_len(1)), vec![1; 7]);
        assert_eq!(lens(SegmentLength::from_len(3)), vec![3, 3, 1]);
    }

    /// Each segment starts where the previous one ended: the frontier has grown
    /// by the records before it, and the expected coordinates are the last
    /// step's successors. The first starts at the window's own state.
    #[test]
    fn each_segment_starts_where_the_previous_one_ended() {
        let segments = plan(SegmentLength::from_len(3));
        assert_eq!(segments[0].start_frontier.position, 0);
        assert!(segments[0].expected_before.is_none());
        assert_eq!(segments[1].start_frontier.position, 3);
        assert!(segments[1].expected_before.is_some());
        assert_eq!(segments[2].start_frontier.position, 6);

        let whole = plan(SegmentLength::Window);
        assert_eq!(whole[0].start_frontier, segments[0].start_frontier);
    }

    /// A step's input carries only the scope parent keyed by its own
    /// `exec_index`.
    #[test]
    fn a_step_takes_only_its_own_scope_parent() {
        let step = tile_step(2, vec![1]);
        let parent = tile_step(0, vec![2]);
        let witness = |position: u64| StepRecordWitness {
            position,
            path_elems: vec![[position as u8; 32].into()],
        };
        let parents = BTreeMap::from([
            (1, (parent.clone(), witness(10))),
            (2, (parent.clone(), witness(20))),
            (4, (parent.clone(), witness(40))),
        ]);

        let input = control_flow_step(&step, &StepIo::default(), &parents);
        let (kept_parent, kept_witness) = input.sequence_scope_parent.expect("keyed by exec 2");
        assert_eq!(kept_parent, parent);
        assert_eq!(kept_witness.position, 20);

        let unrelated = tile_step(3, vec![4]);
        assert!(control_flow_step(&unrelated, &StepIo::default(), &parents)
            .sequence_scope_parent
            .is_none());
    }
}
