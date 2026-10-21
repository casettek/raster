//! Host driver for the control-flow guest.
//!
//! The control-flow guest proves one transition step's CFS conformance, and the
//! transition step takes that receipt as an assumption. Every step's
//! [`ControlFlowInput`] is known before any proving starts: the trace frontier
//! advances by each record's hash, and the coordinates a step may take follow
//! from the previous record and the CFS. So the control-flow receipts depend on
//! no transition receipt. [`ControlFlowWalk`] produces the inputs in window
//! order, and [`prove_control_flow_step`] proves one. See
//! `docs/proposals/control-flow-prover.md` step 6.

use std::collections::BTreeMap;

use raster_core::cfs::{CfsCoordinates, CfsCursor};
use raster_core::control_flow::{expected_successors, ControlFlowInput, ControlFlowJournal};
use raster_core::program::ProgramDefinition;
use raster_core::trace::StepRecord;
use raster_core::transition::StepRecordWitness;

use crate::trace::{
    frontier_root, serializable_frontier_into_trace_frontier, Bytes, BytesHashable,
    SerializableFrontier, TraceTreeFrontier,
};
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

/// Prove the control-flow check for one step. The host write order is the one
/// the guest reads: the program frame, then the input.
pub fn prove_control_flow_step(
    program_frame: &[u8],
    input: &ControlFlowInput,
) -> (risc0_zkvm::Receipt, ControlFlowJournal) {
    let prover = risc0_zkvm::default_prover();
    let mut builder = risc0_zkvm::ExecutorEnv::builder();
    write_frame(&mut builder, program_frame);
    write_postcard(&mut builder, input);
    let env = builder.build().unwrap();
    let receipt = prover
        .prove(env, &CONTROL_FLOW_GUEST_ELF)
        .unwrap_or_else(|e| {
            panic!(
                "control-flow guest refused step {:?}: {e}",
                input.step_record.coordinates
            )
        })
        .receipt;
    let journal: ControlFlowJournal = receipt.journal.decode().unwrap();
    (receipt, journal)
}

/// The trace facts the control-flow check needs at each step of a window,
/// tracked the way the transition guest tracks them.
///
/// The frontier starts at the window's initial frontier and grows by one
/// record hash per step, so its position and root before each step are the
/// ones the transition guest equates. `expected` starts at `None` — the
/// transition guest's state at `Init` — and becomes each step's successor set.
pub struct ControlFlowWalk {
    cfs: CfsCursor,
    frontier: TraceTreeFrontier,
    expected: Option<Vec<CfsCoordinates>>,
}

impl ControlFlowWalk {
    pub fn new(program_frame: &[u8], initial_frontier: &SerializableFrontier) -> Self {
        let program = ProgramDefinition::decode(program_frame)
            .expect("the program frame must decode as a ProgramDefinition");
        Self {
            cfs: CfsCursor::new(program.cfs),
            frontier: serializable_frontier_into_trace_frontier(initial_frontier.clone())
                .expect("the window's initial frontier must deserialize"),
            expected: None,
        }
    }

    /// The control-flow input for `step_record`, the next step of the window.
    ///
    /// The step's scope parent is the one keyed by its own `exec_index`: its
    /// witness folds to this step's trace root and no other.
    pub fn input_for(
        &self,
        step_record: &StepRecord,
        step_io: &StepIo,
        sequence_scope_parents: &BTreeMap<u64, (StepRecord, StepRecordWitness)>,
    ) -> ControlFlowInput {
        ControlFlowInput {
            step_record: step_record.clone(),
            trace_position: u64::from(self.frontier.position()),
            trace_root_before: frontier_root(&self.frontier),
            expected_before: self.expected.clone(),
            input_source_witness: step_io.input_source_witness.clone(),
            sequence_scope_witness: step_io.sequence_scope_witness.clone(),
            sequence_scope_parent: sequence_scope_parents.get(&step_record.exec_index).cloned(),
        }
    }

    /// Move past `step_record`: append its hash and take its successors.
    pub fn advance(&mut self, step_record: &StepRecord) {
        self.expected = Some(expected_successors(&self.cfs, step_record).unwrap_or_else(|| {
            panic!(
                "step {:?} has no successors in the CFS",
                step_record.coordinates
            )
        }));
        self.frontier.append(Bytes(step_record.hash()));
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

    /// A walk over a minimal program: no tiles, a unit `main`.
    fn walk() -> ControlFlowWalk {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.sequences.push(SequenceDef::new("main"));
        let manifest = ProgramManifest {
            name: "test".to_string(),
            version: "0.0.0".to_string(),
            inputs: Default::default(),
            output: None,
        };
        let program = ProgramDefinition::assemble(manifest, cfs, Default::default())
            .expect("minimal program definition");
        ControlFlowWalk::new(
            &program.canonical_bytes(),
            &SerializableFrontier {
                position: 0,
                leaf: raster_core::merkle::EMPTY_LEAF.to_vec(),
                ommers: Vec::new(),
            },
        )
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
        let walk = walk();

        let input = walk.input_for(&step, &StepIo::default(), &parents);
        let (kept_parent, kept_witness) = input.sequence_scope_parent.expect("keyed by exec 2");
        assert_eq!(kept_parent, parent);
        assert_eq!(kept_witness.position, 20);

        let unrelated = tile_step(3, vec![4]);
        assert!(walk
            .input_for(&unrelated, &StepIo::default(), &parents)
            .sequence_scope_parent
            .is_none());
    }

    /// The walk starts where the transition guest's `Init` state does — no
    /// expectations, the initial frontier — and each step moves the trace
    /// position and root on by exactly one record.
    #[test]
    fn the_walk_tracks_the_trace_frontier_step_by_step() {
        let mut walk = walk();
        let first = tile_step(1, vec![]);
        let at_first = walk.input_for(&first, &StepIo::default(), &BTreeMap::new());
        assert_eq!(at_first.trace_position, 0);
        assert!(at_first.expected_before.is_none());

        walk.advance(&first);
        let second = tile_step(2, vec![1]);
        let at_second = walk.input_for(&second, &StepIo::default(), &BTreeMap::new());
        assert_eq!(at_second.trace_position, 1);
        assert_ne!(at_second.trace_root_before, at_first.trace_root_before);
        assert!(at_second.expected_before.is_some());
    }
}
