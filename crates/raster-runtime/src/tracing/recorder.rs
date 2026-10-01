use raster_core::cfs::{
    CfsCoordinate, CfsCoordinates, CfsCursor, ControlFlowSchema, SequenceChildId,
    SequenceChildItem, FIRST_COORDINATE,
};
use raster_core::recur_progress::{DraftStep, RecurProgressStack, RecurSiteKind, SiteDraft};
use raster_core::draft::DraftTransitionWitness;
use raster_core::input::{SelectionPayloadKind, SelectionWitness, SelectorPath, StorageRef};
use raster_core::trace::{
    ExecStep, ExecTarget, FnInput, ProgramEndStep, ProgramStartStep, RecurEndStep, RecurStartStep,
    StepKind, StepRecord,
    StorageData, StorageInput, StorageRoots, TraceEvent,
};
use sha2::{Digest, Sha256};

use std::collections::{HashMap, VecDeque};

use crate::storage::{
    AuthorizedSource, AuthorizedSourceLoad, AuthenticatedObjectStore, StorageSnapshot, StorageWriteRecord,
};
use crate::tracing::commitment::Sha256Commitment;

pub type SequenceId = String;

#[derive(Debug, Clone)]
pub struct SequenceCallstack {
    callstack: VecDeque<SequenceState>,
    current_sequence_coordinates: CfsCoordinates,
}

#[derive(Debug, Clone)]
pub struct SequenceState {
    id: SequenceId,
    current_index: CfsCoordinate,
    parent_coordinates: CfsCoordinates,
}

#[derive(Debug, Clone)]
struct RecurExecutionState {
    site_id: String,
    sequence_coordinates: CfsCoordinates,
    site_coordinates: CfsCoordinates,
    intra_sequence_index: CfsCoordinate,
    next_iteration_index: CfsCoordinate,
}

impl SequenceCallstack {
    fn new() -> Self {
        SequenceCallstack {
            callstack: VecDeque::new(),
            current_sequence_coordinates: CfsCoordinates(vec![]),
        }
    }

    fn push(&mut self, sequence_id: SequenceId, cfs_cursor: &CfsCursor) {
        let parent_current_index = self
            .callstack
            .back()
            .map(|p| p.current_index.try_into().expect("Index too large"))
            .unwrap_or(0);

        let parent_sequence_coords = self.current_sequence_coordinates.clone();

        self.current_sequence_coordinates = cfs_cursor.get_child_coordinates(
            &parent_sequence_coords,
            parent_current_index,
            SequenceChildId::Sequence(sequence_id.clone()),
        );

        if let Some(parent) = self.callstack.back_mut() {
            parent.current_index += 1;
        }

        let sequence_execution_state = SequenceState {
            id: sequence_id,
            current_index: FIRST_COORDINATE,
            parent_coordinates: parent_sequence_coords,
        };
        self.callstack.push_back(sequence_execution_state);
    }

    fn pop(&mut self) -> Option<SequenceState> {
        let popped = self.callstack.pop_back()?;
        self.current_sequence_coordinates = popped.parent_coordinates.clone();
        Some(popped)
    }

    fn push_at_coordinates(&mut self, sequence_id: SequenceId, coordinates: CfsCoordinates) {
        let parent_coordinates = self.current_sequence_coordinates.clone();
        self.current_sequence_coordinates = coordinates;
        self.callstack.push_back(SequenceState {
            id: sequence_id,
            current_index: FIRST_COORDINATE,
            parent_coordinates,
        });
    }

    fn last_mut(&mut self) -> Option<&mut SequenceState> {
        self.callstack.iter_mut().last()
    }
}

#[derive(Debug, Clone)]
pub struct StepWitnessData {
    input_data: Option<Vec<u8>>,
    input_source_witness: Option<FnInput>,
    output_data: Option<Vec<u8>>,
    storage_input: StorageInput,
    storage_write: Option<StorageWriteRecord>,
    draft_transition_witness: Option<DraftTransitionWitness>,
}

impl StepWitnessData {
    pub fn input_data(&self) -> Option<Vec<u8>> {
        self.input_data.clone()
    }

    pub fn output_data(&self) -> Option<Vec<u8>> {
        self.output_data.clone()
    }

    pub fn input_source_witness(&self) -> Option<FnInput> {
        self.input_source_witness.clone()
    }

    pub fn storage_input(&self) -> StorageInput {
        self.storage_input.clone()
    }

    pub fn storage_write(&self) -> Option<StorageWriteRecord> {
        self.storage_write.clone()
    }

    pub fn draft_transition_witness(&self) -> Option<DraftTransitionWitness> {
        self.draft_transition_witness.clone()
    }
}

#[derive(Debug, Default, Clone)]
pub struct StepWitnessStore(HashMap<CfsCoordinates, StepWitnessData>);

impl StepWitnessStore {
    fn new() -> Self {
        StepWitnessStore(HashMap::new())
    }

    pub fn insert(
        &mut self,
        coordinates: CfsCoordinates,
        event: TraceEvent,
        storage_write: Option<StorageWriteRecord>,
    ) {
        match event {
            TraceEvent::SequenceStart(trace_item)
            | TraceEvent::RecurSequenceIterationStart(trace_item)
            | TraceEvent::RecurTileStart(trace_item)
            | TraceEvent::RecurSequenceStart(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: None,
                        storage_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.storage().clone())
                            .unwrap_or_default(),
                        storage_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            // Every scope closes at its own coordinate `[-s]` now, so an
            // `End` gets its own entry instead of filling in its `Start`'s. It
            // declares no input source, and while the two shared a key it
            // inherited the `Start`'s anyway — which the fraud-proof guest
            // refuses outright. A site's `RecurEnd` is the same: its inputs
            // were bound at `RecurStart`, and it carries only its write.
            TraceEvent::RecurSequenceIterationEnd(trace_item)
            | TraceEvent::SequenceEnd(trace_item)
            | TraceEvent::RecurTileEnd(trace_item)
            | TraceEvent::RecurSequenceEnd(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: None,
                        input_source_witness: None,
                        output_data: trace_item.output.as_ref().map(|output| output.data.clone()),
                        storage_input: StorageInput::new(),
                        storage_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::TileExec(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: trace_item
                            .output
                            .as_ref()
                            .map(|output| output.data().to_vec()),
                        storage_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.storage().clone())
                            .unwrap_or_default(),
                        storage_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::RecurTileIterationExec(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: trace_item
                            .output
                            .as_ref()
                            .map(|output| output.data().to_vec()),
                        storage_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.storage().clone())
                            .unwrap_or_default(),
                        storage_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::ProgramStart(_) => {
                // The program's first step binds authorized external data; it
                // consumes no CFS inputs and makes no input commitment of its
                // own (`StepRecord::input_source_commitment` is `None` for
                // `ProgramStart`), so it carries no input source witness.
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: None,
                        input_source_witness: None,
                        output_data: None,
                        storage_input: StorageInput::new(),
                        storage_write,
                        draft_transition_witness: None,
                    },
                );
            }
            TraceEvent::ProgramEnd(_) => {
                // The program's last step shares coordinates `[]` with
                // `ProgramStart`. Its output read is verified from the step
                // record's `output` binding, not from a witness-store entry,
                // so this leaves the `ProgramStart` entry (its storage write)
                // untouched.
            }
        }
    }

    pub fn get(&self, coordinates: &CfsCoordinates) -> Option<&StepWitnessData> {
        self.0.get(coordinates)
    }
}

fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    Sha256::digest(input.source_witness_bytes()).to_vec()
}

#[derive(Debug, Clone)]
pub struct TraceRecorder {
    exec_index: u64,
    sequence_callstack: SequenceCallstack,
    active_recur: Option<RecurExecutionState>,
    active_recur_sequence: HashMap<(CfsCoordinates, String), RecurExecutionState>,
    cfs_cursor: CfsCursor,
    witness_store: StepWitnessStore,
    storage: AuthenticatedObjectStore,
    /// Live recur sites, advanced in step with the guest so both compute the
    /// same `recur_progress_commitment`. Revision 1 of
    /// `recur-progress-commitment.md` failed because two of the frame's fields
    /// were reachable only from the replay journal, which the recorder never
    /// sees; every field here is derived from the CFS, the trace event, or the
    /// authenticated source metadata.
    recur_progress: RecurProgressStack,
    /// The stack as it stood *after* each recorded step, retained so a
    /// fraud-proof window opening mid-loop can be seeded from the trace
    /// prefix instead of from the empty stack. Written at the same tail that
    /// stamps `recur_progress_commitment`, so the two cannot disagree.
    ///
    /// Keyed by `exec_index`, **not** by coordinates: a recur site's `Start`
    /// and `End` share the bare site coordinate (a site coordinate is a
    /// *scope* — `recur-progress-commitment.md` §3.2.1), and those two steps
    /// hold opposite stacks. Keying by coordinate lets the close overwrite the
    /// open, which is the one case a seed most needs to distinguish.
    ///
    /// Retained rather than re-derived: `last_control` is not recoverable
    /// from a step's coordinates, which is the same field that forced a trace
    /// bit in `recur-progress-commitment.md` §3.1. See
    /// `window-seed-reconstruction.md` §2 — a second implementation of these
    /// rules would fail as a commitment mismatch, indistinguishable from the
    /// bug it exists to fix.
    recur_progress_store: HashMap<u64, RecurProgressStack>,
}

impl TraceRecorder {
    pub fn new(cfs: ControlFlowSchema) -> Self {
        Self {
            exec_index: 0,
            sequence_callstack: SequenceCallstack::new(),
            active_recur: None,
            active_recur_sequence: HashMap::new(),
            cfs_cursor: CfsCursor::new(cfs),
            witness_store: StepWitnessStore::new(),
            storage: AuthenticatedObjectStore::new(),
            recur_progress: RecurProgressStack::new(),
            recur_progress_store: HashMap::new(),
        }
    }

    /// Give the recorder the input context to resolve `main`'s entry
    /// arguments against.
    ///
    /// The recorder runs in a different process from the one that executed
    /// the trace, so it cannot inherit the runtime's resolver — but the
    /// caller has already parsed the same `--input` / `--input-manifest`
    /// arguments, so it passes them in rather than the recorder rediscovering
    /// them from `std::env::args`. Required before replaying a trace whose
    /// `main` declares entry arguments.
    pub fn set_external_input(
        &mut self,
        raw_input: Option<&str>,
        raw_manifest: Option<&str>,
    ) -> raster_core::Result<()> {
        let manager =
            crate::source::FileInputSourceResolver::from_input_args(raw_input, raw_manifest)?;
        self.storage
            .set_source_resolver(std::sync::Arc::new(manager));
        Ok(())
    }

    pub fn input_data_at(&self, coordinates: &CfsCoordinates) -> Option<Option<Vec<u8>>> {
        self.witness_store
            .get(coordinates)
            .map(|trace_io| trace_io.input_data.clone())
    }

    pub fn output_data_at(&self, coordinates: &CfsCoordinates) -> Option<Option<Vec<u8>>> {
        self.witness_store
            .get(coordinates)
            .map(|trace_io| trace_io.output_data.clone())
    }

    pub fn step_witness_at(&self, coordinates: &CfsCoordinates) -> Option<StepWitnessData> {
        self.witness_store.get(coordinates).cloned()
    }

    /// The recur-progress stack as it stood **after** the step with this
    /// `exec_index` — the state the *next* step's guest must start from.
    ///
    /// `None` for a step this recorder never recorded. The empty stack is a
    /// *value*, not an absence: a step outside any loop returns
    /// `Some(RecurProgressStack::new())`, which is the positive claim "no loop
    /// in flight" that the guest checks like any other.
    pub fn recur_progress_after(&self, exec_index: u64) -> Option<RecurProgressStack> {
        self.recur_progress_store.get(&exec_index).cloned()
    }

    pub fn storage_snapshot(&self) -> StorageSnapshot {
        self.storage.snapshot()
    }

    pub fn storage_selection_witness(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
        payload_kind: SelectionPayloadKind,
    ) -> raster_core::Result<SelectionWitness> {
        self.storage
            .selection_witness(reference, selector, payload_kind)
    }

    pub fn io_data_at(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        self.witness_store
            .get(coordinates)
            .map(|trace_io| (trace_io.input_data.clone(), trace_io.output_data.clone()))
    }

    /// The storage roots to record for a step that did (or did not)
    /// write. A step without a write leaves the store where it found it, so
    /// both sides are the current roots.
    fn storage_roots(&self, storage_write: Option<&StorageWriteRecord>) -> StorageRoots {
        match storage_write {
            Some(write) => StorageRoots {
                root_before: write.store_root_before.clone(),
                root_after: write.store_root_after.clone(),
                index_root_before: write.index_root_before.clone(),
                index_root_after: write.index_root_after.clone(),
            },
            None => {
                let snapshot = self.storage.snapshot();
                StorageRoots {
                    root_before: snapshot.root.clone(),
                    root_after: snapshot.root,
                    index_root_before: snapshot.index_root.clone(),
                    index_root_after: snapshot.index_root,
                }
            }
        }
    }

    /// The commitments an execution step makes. Every exec target commits to
    /// the same things, so they are computed in exactly one place — a target
    /// cannot end up committing to less than its siblings.
    fn exec_step(
        &self,
        target: ExecTarget,
        intra_sequence_index: CfsCoordinate,
        input: Option<&FnInput>,
        storage_write: Option<&StorageWriteRecord>,
    ) -> ExecStep {
        ExecStep {
            target,
            intra_sequence_index,
            input_commitment: input
                .map(|input| Sha256Commitment::from(input).into())
                .unwrap_or_default(),
            input_source_commitment: input.map(input_source_commitment).unwrap_or_default(),
            output_commitment: storage_write
                .map(|write| write.entry.object_commitment.clone())
                .unwrap_or_default(),
            storage: self.storage_roots(storage_write),
        }
    }

    pub fn record(&mut self, event: TraceEvent) -> StepRecord {
        self.exec_index += 1;
        let exec_index = self.exec_index;

        let step_record = match event.clone() {
            // A recur site opens here, *before* its first iteration. This is
            // the only point at which the loop bound `L` — carried by the
            // source's `0x0A` metadata selection in this event's input — is
            // available to the iterations that will be checked against it.
            // The site opens at `[s]` and closes at `[-s]`.
            TraceEvent::RecurTileStart(fn_call_record)
            | TraceEvent::RecurSequenceStart(fn_call_record) => {
                let is_tile = matches!(event, TraceEvent::RecurTileStart(_));
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("Recur site can't open without sequence context");
                let parent_current_index = current_sequence_state.current_index;
                let current_sequence_id = current_sequence_state.id.clone();

                let child_id = if is_tile {
                    SequenceChildId::RecurTile(fn_call_record.fn_name.clone())
                } else {
                    SequenceChildId::RecurSequence(fn_call_record.fn_name.clone())
                };
                let site_coordinates = self.cfs_cursor.get_child_coordinates(
                    &sequence_coordinates,
                    parent_current_index,
                    child_id,
                );

                let input = fn_call_record.input;

                // Open the site here, where `L` first exists. `chunk` is a CFS
                // literal and `kind` follows the event, so every frame field is
                // producer-visible — the property revision 1 violated.
                self.recur_progress.push_site(
                    site_coordinates.clone(),
                    if is_tile {
                        RecurSiteKind::Tile
                    } else {
                        RecurSiteKind::Sequence
                    },
                    self.recur_site_chunk(&site_coordinates),
                    self.recur_source_len(input.as_ref()),
                    self.recur_site_state_is_output(&site_coordinates),
                    recur_source_identity(input.as_ref()),
                );
                if let Some(draft) = self.recur_site_opening_draft(&site_coordinates, input.as_ref()) {
                    self.recur_progress.open_draft(draft);
                }
                if let Some(seed) = self.recur_site_stored_seed(&site_coordinates, input.as_ref()) {
                    self.recur_progress.seed_state(seed);
                }

                let record = StepRecord {
                    exec_index,
                    // The enclosing sequence, as on every step that is not a
                    // sequence boundary; the site's own id is `site_id`.
                    sequence_id: current_sequence_id,
                    coordinates: site_coordinates.clone(),
                    kind: StepKind::RecurStart(RecurStartStep {
                        site_id: fn_call_record.fn_name.clone(),
                        input_commitment: input
                            .as_ref()
                            .map(|input| Sha256Commitment::from(input).into())
                            .unwrap_or_default(),
                        input_source_commitment: input
                            .as_ref()
                            .map(input_source_commitment)
                            .unwrap_or_default(),
                        // A site reads its source — `L` comes from its
                        // metadata — and writes nothing, so it claims the
                        // current roots on both sides, as `ProgramEnd` does.
                        storage: self.storage_roots(None),
                    }),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store
                    .insert(site_coordinates, event.clone(), None);

                record
            }
            TraceEvent::SequenceStart(fn_call_record) => {
                self.sequence_callstack
                    .push(fn_call_record.fn_name.clone(), &self.cfs_cursor);

                let coordinates = self.sequence_callstack.current_sequence_coordinates.clone();

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|output| Sha256Commitment::from(output).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let record = StepRecord {
                    exec_index,
                    sequence_id: fn_call_record.fn_name.clone(),
                    coordinates: coordinates.clone(),
                    kind: StepKind::SequenceStart {
                        input_commitment,
                        input_source_commitment,
                    },
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store.insert(coordinates, event.clone(), None);

                record
            }
            TraceEvent::SequenceEnd(fn_call_record) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                assert!(
                    self.active_recur.is_none(),
                    "Sequence ended while RecurTile site trace was still active"
                );
                assert!(
                    !self
                        .active_recur_sequence
                        .keys()
                        .any(|(coordinates, _)| coordinates == &sequence_coordinates),
                    "Sequence ended while RecurSequence site trace was still active"
                );

                let output = fn_call_record.output;
                let output_commitment = output
                    .as_ref()
                    .map(|output| Sha256Commitment::from(output).into())
                    .unwrap_or_default();

                // The sequence closes at its own coordinate `[-s]` (D4). While
                // `Start` and `End` shared `[s]`, the ordering check accepted a
                // skipped `End` and a restarted body.
                let closing_coordinates = self
                    .cfs_cursor
                    .closing_coordinates_of(&sequence_coordinates)
                    .unwrap_or_else(|| {
                        panic!("Sequence at {:?} has no closing coordinate", sequence_coordinates)
                    });

                let record = StepRecord {
                    exec_index,
                    coordinates: closing_coordinates.clone(),
                    sequence_id: fn_call_record.fn_name.clone(),
                    kind: StepKind::SequenceEnd { output_commitment },
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.sequence_callstack
                    .pop()
                    .expect("Corrupted sequence stack");

                self.witness_store.insert(closing_coordinates, event, None);

                record
            }
            TraceEvent::ProgramEnd(end_event) => {
                // The program's last step: `main` returned its authorized
                // output. Recorded at `main`'s frame coordinates (`[]`); reads
                // its output object but writes nothing.
                let coordinates = self.sequence_callstack.current_sequence_coordinates.clone();
                assert!(
                    self.active_recur.is_none(),
                    "Program ended while a RecurTile site trace was still active"
                );
                assert!(
                    !self
                        .active_recur_sequence
                        .keys()
                        .any(|(recur_coordinates, _)| recur_coordinates == &coordinates),
                    "Program ended while a RecurSequence site trace was still active"
                );
                let sequence_id = self
                    .sequence_callstack
                    .last_mut()
                    .expect("ProgramEnd requires main's sequence frame")
                    .id
                    .clone();

                // Independently re-derive the output selection from our own
                // storage replica, so the recorded output commitment reflects
                // committed storage rather than a claim from the user process.
                if let Some(output) = &end_event.output {
                    let reference =
                        StorageRef::new(output.coordinates.clone(), output.commitment.clone());
                    let witness = self
                        .storage
                        .selection_witness(
                            &reference,
                            &output.selector,
                            output.selection.payload_kind,
                        )
                        .unwrap_or_else(|error| {
                            panic!("Failed to replay program output selection: {}", error)
                        });
                    let recomputed = raster_core::input::selection_payload_hash(&witness.bytes);
                    assert_eq!(
                        recomputed, output.selection.selected_hash,
                        "Program output selection hash does not match the replayed selection",
                    );
                    assert_eq!(
                        output.selection.source_root_hash.as_slice(),
                        output.commitment.as_slice(),
                        "Program output source-root hash does not match the output object commitment",
                    );
                }

                let output: Option<StorageData> = end_event.output;
                let output_commitment = output
                    .as_ref()
                    .map(|output| output.selection.selected_hash.to_vec())
                    .unwrap_or_default();

                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: coordinates.clone(),
                    kind: StepKind::ProgramEnd(ProgramEndStep {
                        output,
                        output_commitment,
                        storage: self.storage_roots(None),
                    }),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.sequence_callstack
                    .pop()
                    .expect("Corrupted sequence stack");

                self.witness_store.insert(coordinates, event.clone(), None);

                record
            }
            TraceEvent::RecurSequenceIterationStart(fn_call_record) => {
                let parent_sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let parent_current_index = self
                    .sequence_callstack
                    .last_mut()
                    .expect("RecurSequence can't start without sequence context")
                    .current_index;

                let recur_key = (
                    parent_sequence_coordinates.clone(),
                    fn_call_record.fn_name.clone(),
                );
                if !self.active_recur_sequence.contains_key(&recur_key) {
                    let site_coordinates = self.cfs_cursor.get_child_coordinates(
                        &parent_sequence_coordinates,
                        parent_current_index,
                        SequenceChildId::RecurSequence(fn_call_record.fn_name.clone()),
                    );
                    self.active_recur_sequence.insert(
                        recur_key.clone(),
                        RecurExecutionState {
                            site_id: fn_call_record.fn_name.clone(),
                            sequence_coordinates: parent_sequence_coordinates.clone(),
                            site_coordinates,
                            intra_sequence_index: parent_current_index,
                            next_iteration_index: FIRST_COORDINATE,
                            // Chunking is not supported for recur sequences yet.
                        },
                    );
                }
                let recur_state = self
                    .active_recur_sequence
                    .get_mut(&recur_key)
                    .expect("RecurSequence state should exist after insertion");
                assert_eq!(
                    recur_state.sequence_coordinates, parent_sequence_coordinates,
                    "RecurSequence iteration switched parent sequence coordinates mid-stream",
                );
                assert_eq!(
                    recur_state.site_id, fn_call_record.fn_name,
                    "RecurSequence iteration switched site id mid-stream",
                );

                let mut iteration_coordinates = recur_state.site_coordinates.clone();
                iteration_coordinates.push(recur_state.next_iteration_index);
                // Coordinates are 1-based; the progress rules count from 0.
                // Same seam the guest crosses in `advance_recur_progress`.
                let iteration_index =
                    u64::try_from(recur_state.next_iteration_index - FIRST_COORDINATE)
                        .expect("a recur iteration coordinate is at least the first");
                recur_state.next_iteration_index += 1;

                // Only the iteration's *Start* advances the frame; its End is
                // the same iteration closing, not a second one.
                if let Err(violation) = self
                    .recur_progress
                    .advance_sequence_iteration(&iteration_coordinates, iteration_index)
                {
                    panic!(
                        "Recur progress violation at {:?}: {}",
                        iteration_coordinates, violation
                    );
                }
                self.sequence_callstack.push_at_coordinates(
                    fn_call_record.fn_name.clone(),
                    iteration_coordinates.clone(),
                );

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|output| Sha256Commitment::from(output).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let record = StepRecord {
                    exec_index,
                    sequence_id: fn_call_record.fn_name.clone(),
                    coordinates: iteration_coordinates.clone(),
                    kind: StepKind::SequenceStart {
                        input_commitment,
                        input_source_commitment,
                    },
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store
                    .insert(iteration_coordinates, event.clone(), None);

                record
            }
            TraceEvent::RecurSequenceIterationEnd(fn_call_record) => {
                assert!(
                    self.active_recur.is_none(),
                    "RecurSequence iteration ended while RecurTile trace was still active"
                );
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();

                let output = fn_call_record.output;
                let output_commitment = output
                    .as_ref()
                    .map(|output| Sha256Commitment::from(output).into())
                    .unwrap_or_default();

                // The iteration closes at its *own* coordinate, not the one it
                // opened on. While the two were the same, nothing keyed or
                // dispatched on position could tell a `Start` from its `End`:
                // the witness store served the Start's input to the End, and
                // `try_get_next_coordinates` answered a close with the open's
                // successors, rejecting the next iteration of an honest sweep.
                // `frame.site` is still a prefix of the close, so the recur
                // rules below read it exactly as before.
                let closing_coordinates = self
                    .cfs_cursor
                    .closing_coordinates_of(&sequence_coordinates)
                    .unwrap_or_else(|| sequence_coordinates.clone());

                // A recur sequence iteration closes here, and this is the
                // first point at which what it *produced* is known: the count
                // moved at its `Start`, the carried state folds now.
                if matches!(
                    self.recur_progress.innermost().map(|frame| frame.kind),
                    Some(raster_core::recur_progress::RecurSiteKind::Sequence)
                ) {
                    if let Err(violation) = self.recur_progress.fold_sequence_iteration_state(
                        &closing_coordinates,
                        fn_call_record.recur_state.as_ref(),
                    ) {
                        panic!(
                            "Recur progress violation at {:?}: {}",
                            closing_coordinates, violation
                        );
                    }
                }

                let record = StepRecord {
                    exec_index,
                    coordinates: closing_coordinates.clone(),
                    sequence_id: fn_call_record.fn_name.clone(),
                    kind: StepKind::SequenceEnd { output_commitment },
                    recur_progress_commitment: [0u8; 32],
                    recur_state: fn_call_record.recur_state,
                };

                self.sequence_callstack
                    .pop()
                    .expect("Corrupted recur sequence stack");

                self.witness_store
                    .insert(closing_coordinates, event.clone(), None);

                record
            }
            TraceEvent::TileExec(fn_call_record) => {
                assert!(
                    self.active_recur.is_none(),
                    "Ordinary tile execution cannot occur while recur iterations are active"
                );
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("Tile can't be called without sequence context");

                let sequence_id = current_sequence_state.id.clone();
                let parent_current_index = current_sequence_state.current_index;

                let mut candidate_coordinates = sequence_coordinates.clone();
                candidate_coordinates.push(
                    parent_current_index
                        .try_into()
                        .expect("Sequence coordinate out of bound u8"),
                );
                let child_id = match self.cfs_cursor.try_get_item(&candidate_coordinates) {
                    Some(raster_core::cfs::SequenceChildItem::RecurTile(item))
                        if item.id == fn_call_record.fn_name =>
                    {
                        SequenceChildId::RecurTile(fn_call_record.fn_name.clone())
                    }
                    _ => SequenceChildId::Tile(fn_call_record.fn_name.clone()),
                };

                let tile_coordinates = self.cfs_cursor.get_child_coordinates(
                    &sequence_coordinates,
                    parent_current_index,
                    child_id,
                );

                current_sequence_state.current_index += 1;

                let input = fn_call_record.input;
                let output = fn_call_record.output;
                let storage_write = output.as_ref().map(|output| {
                    self.storage.append_serialized_bytes(
                        &output.data,
                        tile_coordinates.clone(),
                        output.raster.clone(),
                    )
                });

                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: tile_coordinates.clone(),
                    kind: StepKind::Exec(self.exec_step(
                        ExecTarget::Tile(fn_call_record.fn_name),
                        parent_current_index,
                        input.as_ref(),
                        storage_write.as_ref(),
                    )),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                // A recur sequence's body tile may advance its site's object.
                self.advance_recur_draft(
                    &tile_coordinates,
                    fn_call_record.draft_transition_witness.as_ref(),
                    false,
                );

                self.witness_store
                    .insert(tile_coordinates, event.clone(), storage_write);

                record
            }
            TraceEvent::RecurTileIterationExec(fn_call_record) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("RecurTile can't be called without sequence context");

                let sequence_id = current_sequence_state.id.clone();
                let recur_state = self.active_recur.get_or_insert_with(|| {
                    let parent_current_index = current_sequence_state.current_index;
                    let site_coordinates = self.cfs_cursor.get_child_coordinates(
                        &sequence_coordinates,
                        parent_current_index,
                        SequenceChildId::RecurTile(fn_call_record.fn_name.clone()),
                    );
                    RecurExecutionState {
                        site_id: fn_call_record.fn_name.clone(),
                        sequence_coordinates: sequence_coordinates.clone(),
                        site_coordinates,
                        intra_sequence_index: parent_current_index,
                        next_iteration_index: FIRST_COORDINATE,
                    }
                });
                assert_eq!(
                    recur_state.sequence_coordinates, sequence_coordinates,
                    "RecurTile iteration switched parent sequence coordinates mid-stream",
                );
                assert_eq!(
                    recur_state.site_id, fn_call_record.fn_name,
                    "RecurTile iteration switched site id mid-stream",
                );

                let mut tile_coordinates = recur_state.site_coordinates.clone();
                tile_coordinates.push(recur_state.next_iteration_index);
                // Coordinates are 1-based; the progress rules count iterations
                // from 0 — the same 0 the tile's replay journal reports, which
                // is why the counter is not renumbered with the coordinates.
                let iteration_index =
                    u64::try_from(recur_state.next_iteration_index - FIRST_COORDINATE)
                        .expect("a recur iteration coordinate is at least the first");
                recur_state.next_iteration_index += 1;
                let intra_sequence_index = recur_state.intra_sequence_index;

                // Chunk shape is checked in the transition guest, against the
                // iteration's replay-proven `consumed_elements` rather than a
                // varint read off host-supplied bytes. Re-checking it here
                // would re-derive a weaker form of the same rule from data the
                // recorder cannot authenticate. See `lazy-list-recur.md` §5.

                let input = fn_call_record.input;
                let output = fn_call_record.output;
                let storage_write = output.as_ref().map(|output| {
                    self.storage.append_serialized_bytes(
                        &output.data,
                        tile_coordinates.clone(),
                        output.raster.clone(),
                    )
                });

                // An iteration of a recur site is an ordinary tile run: it is
                // replayed and verified exactly like one.
                let recur_state_for_record = fn_call_record.recur_state;
                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: tile_coordinates.clone(),
                    kind: StepKind::Exec(self.exec_step(
                        ExecTarget::Tile(fn_call_record.fn_name),
                        intra_sequence_index,
                        input.as_ref(),
                        storage_write.as_ref(),
                    )),
                    recur_progress_commitment: [0u8; 32],
                    // Also on the step record, not only in the replay
                    // journal: a recur *sequence* has no journal, so the guest
                    // reads the transition from here uniformly, and binds a
                    // tile's copy against the replay-proven one.
                    recur_state: recur_state_for_record,
                };

                // Advance with the values rule 3 and rule 4 *require*: the
                // recorder derives them from `(C, L, index)`, the guest reads
                // them from the replay journal, and the guest's check is that
                // the two agree. Only `next_iteration_index` and `last_control`
                // actually move the frame.
                {
                    let frame_chunk = self.recur_progress.innermost().map(|f| f.chunk).unwrap_or(1);
                    let frame_len = self
                        .recur_progress
                        .innermost()
                        .map(|f| f.source_len)
                        .unwrap_or(0);
                    let consumed_before = self
                        .recur_progress
                        .innermost()
                        .map(|f| f.consumed_total())
                        .unwrap_or(0);
                    let declared = frame_len.div_ceil(frame_chunk.max(1));
                    let consumed =
                        core::cmp::min(frame_chunk, frame_len.saturating_sub(consumed_before));
                    // No default. Defaulting an absent control to `Continue`
                    // is what made every `Break`-terminated sweep unrecordable
                    // while looking like a rule-5 violation: the recorder folded
                    // `Continue`, `close_site` saw an incomplete prefix, and the
                    // panic named the sweep rather than the missing field. The
                    // tile wrapper sets it on every recur iteration.
                    let control = fn_call_record.recur_control.unwrap_or_else(|| {
                        panic!(
                            "Recur iteration at {:?} carries no control; the tile wrapper must record one",
                            tile_coordinates
                        )
                    });
                    if let Err(violation) = self.recur_progress.advance_tile_iteration(
                        &tile_coordinates,
                        iteration_index,
                        declared,
                        consumed,
                        control,
                        recur_state_for_record.as_ref(),
                    ) {
                        panic!(
                            "Recur progress violation at {:?}: {}",
                            tile_coordinates, violation
                        );
                    }
                }
                self.advance_recur_draft(
                    &tile_coordinates,
                    fn_call_record.draft_transition_witness.as_ref(),
                    true,
                );

                self.witness_store
                    .insert(tile_coordinates, event.clone(), storage_write);

                record
            }
            TraceEvent::RecurTileEnd(fn_call_record) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("RecurTile site can't be recorded without sequence context");
                let sequence_id = current_sequence_state.id.clone();
                let parent_current_index = current_sequence_state.current_index;

                let recur_state = self.active_recur.take().unwrap_or_else(|| {
                    let site_coordinates = self.cfs_cursor.get_child_coordinates(
                        &sequence_coordinates,
                        parent_current_index,
                        SequenceChildId::RecurTile(fn_call_record.fn_name.clone()),
                    );
                    RecurExecutionState {
                        site_id: fn_call_record.fn_name.clone(),
                        sequence_coordinates: sequence_coordinates.clone(),
                        site_coordinates,
                        intra_sequence_index: parent_current_index,
                        next_iteration_index: FIRST_COORDINATE,
                    }
                });

                assert_eq!(
                    recur_state.sequence_coordinates, sequence_coordinates,
                    "RecurTile completion switched parent sequence coordinates mid-stream",
                );
                assert_eq!(
                    recur_state.site_id, fn_call_record.fn_name,
                    "RecurTile completion site id does not match active RecurTile stream",
                );

                current_sequence_state.current_index += 1;

                let site_coordinates = recur_state.site_coordinates.clone();

                // A site's close binds no inputs: they were bound once, at
                // `RecurStart`.
                let output = fn_call_record.output;
                let storage_write = output.as_ref().map(|output| {
                    self.storage.append_serialized_bytes(
                        &output.data,
                        site_coordinates.clone(),
                        output.raster.clone(),
                    )
                });


                // Close the site: this is where the terminal rules run —
                // rule 5/7 for a tile site, S4 for a sequence site.
                let site_output_commitment = storage_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();
                if let Err(violation) =
                    self.recur_progress.close_site(&site_coordinates, &site_output_commitment)
                {
                    panic!("Recur progress violation at site {:?}: {}", site_coordinates, violation);
                }
                // The close is recorded at `[-s]`; the object stays at `[s]`.
                let closing_coordinates = self
                    .cfs_cursor
                    .closing_coordinates_of(&site_coordinates)
                    .expect("a recur site closes at its own coordinate");
                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: closing_coordinates.clone(),
                    kind: StepKind::RecurEnd(RecurEndStep {
                        site_id: fn_call_record.fn_name,
                        output_commitment: storage_write
                            .as_ref()
                            .map(|write| write.entry.object_commitment.clone())
                            .unwrap_or_default(),
                        storage: self.storage_roots(storage_write.as_ref()),
                    }),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store
                    .insert(closing_coordinates, event.clone(), storage_write);

                record
            }
            TraceEvent::RecurSequenceEnd(fn_call_record) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("RecurSequence site can't be recorded without sequence context");
                let sequence_id = current_sequence_state.id.clone();
                let parent_current_index = current_sequence_state.current_index;

                let recur_key = (sequence_coordinates.clone(), fn_call_record.fn_name.clone());
                let recur_state = self
                    .active_recur_sequence
                    .remove(&recur_key)
                    .unwrap_or_else(|| {
                        let site_coordinates = self.cfs_cursor.get_child_coordinates(
                            &sequence_coordinates,
                            parent_current_index,
                            SequenceChildId::RecurSequence(fn_call_record.fn_name.clone()),
                        );
                        RecurExecutionState {
                            site_id: fn_call_record.fn_name.clone(),
                            sequence_coordinates: sequence_coordinates.clone(),
                            site_coordinates,
                            intra_sequence_index: parent_current_index,
                            next_iteration_index: FIRST_COORDINATE,
                        }
                    });

                assert_eq!(
                    recur_state.sequence_coordinates, sequence_coordinates,
                    "RecurSequence completion switched parent sequence coordinates mid-stream",
                );
                assert_eq!(
                    recur_state.site_id, fn_call_record.fn_name,
                    "RecurSequence completion site id does not match active RecurSequence stream",
                );

                current_sequence_state.current_index += 1;

                let site_coordinates = recur_state.site_coordinates.clone();

                // A site's close binds no inputs: they were bound once, at
                // `RecurStart`.
                let output = fn_call_record.output;
                let storage_write = output.as_ref().map(|output| {
                    self.storage.append_serialized_bytes(
                        &output.data,
                        site_coordinates.clone(),
                        output.raster.clone(),
                    )
                });

                // Close the site: S4 (`count == L`) runs here.
                let site_output_commitment = storage_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();
                if let Err(violation) =
                    self.recur_progress.close_site(&site_coordinates, &site_output_commitment)
                {
                    panic!(
                        "Recur progress violation at site {:?}: {}",
                        site_coordinates, violation
                    );
                }

                // The close is recorded at `[-s]`; the object stays at `[s]`.
                let closing_coordinates = self
                    .cfs_cursor
                    .closing_coordinates_of(&site_coordinates)
                    .expect("a recur site closes at its own coordinate");
                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: closing_coordinates.clone(),
                    kind: StepKind::RecurEnd(RecurEndStep {
                        site_id: fn_call_record.fn_name,
                        output_commitment: storage_write
                            .as_ref()
                            .map(|write| write.entry.object_commitment.clone())
                            .unwrap_or_default(),
                        storage: self.storage_roots(storage_write.as_ref()),
                    }),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store
                    .insert(closing_coordinates, event.clone(), storage_write);

                record
            }
            TraceEvent::ProgramStart(start_event) => {
                // The program's first step. It opens `main`'s frame (nothing
                // else has yet) and binds its entry arguments at the sequence
                // root coordinate `[]` — not a reserved child slot, so the
                // frame's child index is left at 0 for the first real item.
                self.sequence_callstack
                    .push("main".to_string(), &self.cfs_cursor);
                let coordinates = self.sequence_callstack.current_sequence_coordinates.clone();
                let sequence_id = self
                    .sequence_callstack
                    .last_mut()
                    .expect("ProgramStart just pushed main's frame")
                    .id
                    .clone();

                let names: Vec<String> = start_event
                    .arguments
                    .iter()
                    .map(|argument| argument.name.clone())
                    .collect();
                let sources: Vec<AuthorizedSource> = start_event
                    .arguments
                    .iter()
                    .map(|argument| {
                        let kind = match argument.encoding {
                            raster_core::input::ExternalEncoding::Raster => {
                                crate::backing::ReferencedSourceKind::Raster {
                                    schema: || {
                                        raster_core::input::SchemaNode::Leaf {
                                            type_name: String::new(),
                                        }
                                    },
                                }
                            }
                            raster_core::input::ExternalEncoding::Postcard => {
                                // `TraceRecorder` runs in `raster-cli`'s own
                                // process (spawned generically, over any user
                                // project — see `commands/run.rs`), never the
                                // user program's. Postcard sources aren't
                                // self-describing (unlike raster's
                                // `.rindex`), so selecting into one requires
                                // the argument's concrete Rust type — which a
                                // generic, cross-process recorder cannot have.
                                // This mirrors the pre-existing constraint on
                                // ordinary internal objects (`OwnedObject::select`
                                // requires a raster payload too) and on the
                                // old `external!()` design
                                // (`external_selection_witness` only ever
                                // supported raster external inputs). Use
                                // raster encoding in `input_manifest.json` for
                                // any entry argument that needs a selection
                                // witness built by the commit/audit pipeline;
                                // postcard entry arguments remain fully usable
                                // in-process (plain `cargo run`) or as whole
                                // values.
                                panic!(
                                    "Cannot build a cross-process selection witness for postcard-encoded entry argument '{}': \
                                     postcard sources are not self-describing and this recorder runs in a separate process from \
                                     the one that resolved it. Use raster encoding in input_manifest.json for entry arguments \
                                     that need --commit/--audit support.",
                                    argument.name
                                );
                            }
                        };
                        AuthorizedSource {
                            name: argument.name.clone(),
                            encoding: argument.encoding,
                            commitment: argument.commitment.clone(),
                            kind,
                        }
                    })
                    .collect();

                // No arguments means no storage write and no manifest lookup:
                // the program still starts, binding nothing.
                let (storage_write, output_commitment) = if sources.is_empty() {
                    (None, Vec::new())
                } else {
                    assert!(
                        self.storage.source_resolver().is_some(),
                        "Replaying a program start requires input context; call \
                         TraceRecorder::set_external_input with the same --input/--input-manifest \
                         the trace was produced with",
                    );
                    let write = self.storage.load_authorized_sources(
                        AuthorizedSourceLoad { sources },
                        coordinates.clone(),
                    );
                    let output_commitment = write.entry.object_commitment.clone();
                    (Some(write), output_commitment)
                };

                let record = StepRecord {
                    exec_index,
                    sequence_id,
                    coordinates: coordinates.clone(),
                    kind: StepKind::ProgramStart(ProgramStartStep {
                        entry_arguments: names,
                        output_commitment,
                        storage: self.storage_roots(storage_write.as_ref()),
                    }),
                    recur_progress_commitment: [0u8; 32],
                    recur_state: None,
                };

                self.witness_store
                    .insert(coordinates, event.clone(), storage_write);

                record
            }
        };

        // The commitment is over the stack *after* this step, which is what
        // lets a fraud-proof window validate a seed by reproducing it rather
        // than by matching a predecessor record the window does not contain.
        let mut step_record = step_record;
        step_record.recur_progress_commitment = self.recur_progress.commitment();
        self.recur_progress_store
            .insert(step_record.exec_index, self.recur_progress.clone());
        step_record
    }

    /// `L` for a recur site, from the authenticated `0x0A` metadata selection
    /// the site `Start` event carries as its `input` binding.
    ///
    /// Rebuilt from storage rather than read off the recorded commitment: the
    /// commitment carries `selected_hash`/`selected_len`, not the length
    /// itself. This is the only point where `L` is available before iteration
    /// 0, which is why the site needs a `Start` event at all.
    fn recur_source_len(&self, input: Option<&FnInput>) -> u64 {
        let binding = input
            .and_then(|input| input.storage().get("input"))
            .expect("Recur site Start must record its source binding");
        let reference = StorageRef::new(binding.coordinates.clone(), binding.commitment.clone());
        self.storage
            .list_metadata_selection(&reference, &binding.selector)
            .unwrap_or_else(|error| {
                panic!("Failed to read recur source metadata at site open: {}", error)
            })
            .len
    }

    /// Where a site's object opens, from the CFS — as the guest's
    /// `opening_draft`: the empty root for a creating site, the base's
    /// commitment for a deriving one. `None` for a state-only site.
    fn recur_site_opening_draft(
        &self,
        site_coordinates: &CfsCoordinates,
        input: Option<&FnInput>,
    ) -> Option<SiteDraft> {
        let decl = match self.cfs_cursor.try_get_item(site_coordinates) {
            Some(SequenceChildItem::RecurTile(item)) => item.output.as_ref(),
            Some(SequenceChildItem::RecurSequence(item)) => item.output.as_ref(),
            _ => None,
        }?;
        let root = if decl.derives {
            let base = input
                .and_then(|input| input.storage().get("output"))
                .unwrap_or_else(|| {
                    panic!("Deriving recur site {:?} records no `output` base", site_coordinates)
                });
            assert!(
                base.selection.path.segments.is_empty(),
                "Deriving recur site {:?} must derive from a whole object, not a selection inside one",
                site_coordinates,
            );
            base.commitment
                .as_slice()
                .try_into()
                .expect("an object commitment is 32 bytes")
        } else {
            decl.empty_root
        };
        Some(SiteDraft {
            schema_hash: decl.schema_hash,
            root,
        })
    }

    /// A site's stored seed, as the guest's `stored_seed`: the commitment of
    /// its second argument when the CFS says it carries state and that is a
    /// whole stored object (D5b).
    fn recur_site_stored_seed(
        &self,
        site_coordinates: &CfsCoordinates,
        input: Option<&FnInput>,
    ) -> Option<raster_core::input::Hash32> {
        let carries_state = match self.cfs_cursor.try_get_item(site_coordinates) {
            Some(SequenceChildItem::RecurTile(item)) => item.carries_state,
            Some(SequenceChildItem::RecurSequence(item)) => item.carries_state,
            _ => false,
        };
        if !carries_state {
            return None;
        }
        let input = input?;
        if !matches!(input.values().get(1), Some(raster_core::trace::FnInputValue::StorageBinding)) {
            return None;
        }
        let binding = input.storage().get(input.args().get(1)?.name.as_str())?;
        if !binding.selection.path.segments.is_empty() {
            return None;
        }
        binding.commitment.as_slice().try_into().ok()
    }

    /// Chain a tile's draft transition onto the innermost site's object, as
    /// the guest does — so a seal or schema disagreement fails here, at record
    /// time, rather than producing a trace no guest accepts.
    fn advance_recur_draft(
        &mut self,
        coordinates: &CfsCoordinates,
        witness: Option<&DraftTransitionWitness>,
        is_tile_iteration: bool,
    ) {
        let step = DraftStep::from_native_witness(witness).unwrap_or_else(|error| {
            panic!("Draft ops at {:?} do not apply to their witness: {}", coordinates, error)
        });
        if let Err(violation) =
            self.recur_progress
                .advance_draft(coordinates, step.as_ref(), is_tile_iteration)
        {
            panic!("Recur progress violation at {:?}: {}", coordinates, violation);
        }
    }

    /// The CFS-declared chunk size for a site, 1 when unchunked.
    fn recur_site_chunk(&self, site_coordinates: &CfsCoordinates) -> u64 {
        match self.cfs_cursor.try_get_item(site_coordinates) {
            Some(SequenceChildItem::RecurTile(item)) => item.chunk.unwrap_or(1),
            _ => 1,
        }
    }

    /// Whether the site at these coordinates returns its carried state, from
    /// the CFS — the fact `close_site` needs to pin a sweep's final state.
    fn recur_site_state_is_output(&self, site_coordinates: &CfsCoordinates) -> bool {
        match self.cfs_cursor.try_get_item(site_coordinates) {
            Some(SequenceChildItem::RecurTile(item)) => item.state_is_output,
            Some(SequenceChildItem::RecurSequence(item)) => item.state_is_output,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::{
        closing_coordinate, RecurSequenceItem, RecurTileItem, SequenceChildItem, SequenceDef,
        TileDef, TileItem,
    };
    use raster_core::trace::FnCallRecord;

    /// A site `Start` event carrying a real source binding.
    ///
    /// A recur site opens against an authenticated source: `Start` records the
    /// `0x0A` metadata selection, and the recorder reads `L` back from storage.
    /// These fixtures therefore have to seed an actual list — a `Start` with no
    /// source binding is a malformed trace, and the recorder says so rather
    /// than defaulting `L`, which is the failure mode revision 1 of
    /// `recur-progress-commitment.md` was built to avoid.
    fn seed_recur_source(recorder: &mut TraceRecorder, site: &str, elements: usize) -> FnCallRecord {
        let value: Vec<String> = (0..elements).map(|i| format!("item-{}", i)).collect();
        let bytes = raster_core::postcard::to_allocvec(&value).unwrap();
        let (raster_bytes, index_bytes, root_hex) =
            crate::input::encode_raster_value(&value).unwrap();
        let coordinates = CfsCoordinates(vec![200]);
        let root_hash: [u8; 32] = crate::storage::decode_hex_bytes(&root_hex)
            .unwrap()
            .try_into()
            .unwrap();
        let write = recorder.storage.append_serialized_bytes(
            &bytes,
            coordinates.clone(),
            Some(raster_core::trace::RasterPayload {
                bytes: raster_bytes,
                index_bytes,
                root_hash,
            }),
        );
        let reference = StorageRef::new(coordinates.clone(), write.entry.object_commitment.clone());
        let metadata = recorder
            .storage
            .list_metadata_selection(&reference, &SelectorPath::default())
            .expect("seeded source should expose list metadata");

        let mut storage = raster_core::trace::StorageInput::new();
        storage.insert(
            "input".to_string(),
            StorageData {
                coordinates,
                commitment: write.entry.object_commitment,
                selector: SelectorPath::default(),
                selection: metadata.selected.commitment,
            },
        );
        FnCallRecord {
            fn_name: site.to_string(),
            input: Some(FnInput {
                data: Vec::new(),
                values: vec![raster_core::trace::FnInputValue::StorageBinding],
                args: vec![raster_core::trace::FnInputArg {
                    name: "input".to_string(),
                    ty: "AuthRef<List<String>>".to_string(),
                }],
                storage,
            }),
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }
    }

    fn recorder_with_recur_site() -> TraceRecorder {
        TraceRecorder::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![TileDef::iter("recur", 0, 0), TileDef::iter("after", 0, 0)],
            sequences: vec![SequenceDef {
                id: "main".to_string(),
                input_sources: vec![],
                items: vec![
                    SequenceChildItem::RecurTile(RecurTileItem {
                        id: "recur".to_string(),
                        sources: vec![],
                        chunk: None,
                        output: None,
                        state_is_output: false,
                        carries_state: false,
                    }),
                    SequenceChildItem::Tile(TileItem {
                        id: "after".to_string(),
                        sources: vec![],
                    }),
                ],
                entry_arguments: vec![],
                produces_output: false,
                returns: None,
            }],
        })
    }

    fn recorder_with_recur_sequence_site() -> TraceRecorder {
        TraceRecorder::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![TileDef::iter("inner", 0, 0), TileDef::iter("after", 0, 0)],
            sequences: vec![
                SequenceDef {
                    id: "main".to_string(),
                    input_sources: vec![],
                    items: vec![
                        SequenceChildItem::RecurSequence(RecurSequenceItem {
                            id: "child".to_string(),
                            sources: vec![],
                            state_is_output: false,
                            carries_state: false,
                            output: None,
                        }),
                        SequenceChildItem::Tile(TileItem {
                            id: "after".to_string(),
                            sources: vec![],
                        }),
                    ],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
                SequenceDef {
                    id: "child".to_string(),
                    input_sources: vec![],
                    items: vec![SequenceChildItem::Tile(TileItem {
                        id: "inner".to_string(),
                        sources: vec![],
                    })],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
            ],
        })
    }

    /// A `SequenceEnd` gets its own entry, with no input: it used to fill in
    /// its `Start`'s, and panicked here when there was none.
    #[test]
    fn a_sequence_end_gets_its_own_entry_with_no_input() {
        let mut store = StepWitnessStore::new();
        store.insert(
            CfsCoordinates(vec![3, -1]),
            TraceEvent::SequenceEnd(FnCallRecord {
                fn_name: "child".to_string(),
                input: None,
                output: None,
                draft_transition_witness: None,
                recur_control: None,
                recur_state: None,
            }),
            None,
        );
        assert!(store
            .0
            .get(&CfsCoordinates(vec![3, -1]))
            .expect("the End has its own entry")
            .input_source_witness
            .is_none());
    }

    fn start_main(recorder: &mut TraceRecorder) {
        recorder.record(TraceEvent::ProgramStart(
            raster_core::trace::ProgramStartEvent {
                arguments: Vec::new(),
            },
        ));
    }


    /// `main` containing one ordinary nested sequence — the shape that makes
    /// the `SequenceStart`/`SequenceEnd` coordinate collision observable.
    fn recorder_with_nested_sequence() -> TraceRecorder {
        TraceRecorder::new(ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![],
            sequences: vec![
                SequenceDef {
                    id: "main".to_string(),
                    input_sources: vec![],
                    items: vec![SequenceChildItem::Sequence(raster_core::cfs::SequenceItem {
                        id: "child".to_string(),
                        sources: vec![],
                    })],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
                SequenceDef {
                    id: "child".to_string(),
                    input_sources: vec![],
                    items: vec![],
                    entry_arguments: vec![],
                    produces_output: false,
                    returns: None,
                },
            ],
        })
    }

    fn call_with_input(fn_name: &str) -> FnCallRecord {
        let mut record = call(fn_name);
        record.input = Some(FnInput {
            data: vec![1, 2, 3],
            values: vec![],
            args: vec![],
            storage: Default::default(),
        });
        record
    }

    #[test]
    fn program_start_is_what_creates_mains_root_entry() {
        // `main` publishes no `SequenceStart` (`raster-macros/src/lib.rs`
        // suppresses it), so `ProgramStart` both pushes main's frame and
        // creates the `[]` witness-store entry. It records no input source,
        // because main's arguments are entry arguments rather than CFS inputs.
        let mut recorder = recorder_with_nested_sequence();
        assert!(recorder.step_witness_at(&CfsCoordinates(vec![])).is_none());

        start_main(&mut recorder);

        let entry = recorder
            .step_witness_at(&CfsCoordinates(vec![]))
            .expect("ProgramStart creates the root entry");
        assert!(entry.input_source_witness().is_none());
    }

    #[test]
    fn program_end_leaves_the_root_entry_untouched() {
        // `main` publishes neither a `SequenceStart` nor a `SequenceEnd`: the
        // macro routes it to `gen_main_wrapped_body`, whose boundaries are
        // `ProgramStart` and `ProgramEnd` (`raster-macros/src/lib.rs` — the
        // dispatch at `if item_fn.sig.ident == "main"`). So exactly two steps
        // sit at `[]`, and `ProgramEnd` writes nothing: the root entry is
        // `ProgramStart`'s alone, and `output_data` there stays `None`.
        //
        // This is why the `SequenceEnd` inheritance bug needs a *nested*
        // sequence — at the root there is no `SequenceEnd` to inherit
        // anything.
        let mut recorder = recorder_with_nested_sequence();
        start_main(&mut recorder);
        let before = recorder
            .step_witness_at(&CfsCoordinates(vec![]))
            .expect("ProgramStart created the root entry");

        recorder.record(TraceEvent::ProgramEnd(raster_core::trace::ProgramEndEvent {
            output: None,
        }));

        let after = recorder
            .step_witness_at(&CfsCoordinates(vec![]))
            .expect("root entry still present");
        assert!(after.input_source_witness().is_none());
        assert_eq!(after.output_data(), before.output_data());
        assert!(after.output_data().is_none());
    }

    #[test]
    fn a_nested_sequence_end_closes_at_its_own_coordinate() {
        // D4: `SequenceStart` and `SequenceEnd` no longer share coordinates.
        // The End closes at `[-s]` with an entry of its own and no input; it
        // used to `get_mut` the Start's entry and so carried the Start's input
        // source, which the fraud host then had to filter by step kind.
        let mut recorder = recorder_with_nested_sequence();
        start_main(&mut recorder);

        let start = recorder.record(TraceEvent::SequenceStart(call_with_input("child")));
        let child_coordinates = start.coordinates().clone();
        let at_start = recorder
            .step_witness_at(&child_coordinates)
            .expect("SequenceStart creates the child entry")
            .input_source_witness();
        assert!(at_start.is_some());

        let end = recorder.record(TraceEvent::SequenceEnd(call("child")));
        let closing = CfsCoordinates(vec![closing_coordinate(child_coordinates[0])]);
        assert_eq!(end.coordinates(), &closing);
        assert!(recorder
            .step_witness_at(&closing)
            .expect("the End has its own entry")
            .input_source_witness()
            .is_none());
        assert_eq!(
            recorder
                .step_witness_at(&child_coordinates)
                .and_then(|entry| entry.input_source_witness()),
            at_start,
            "the Start's entry is untouched",
        );
        assert!(end.input_source_commitment().is_none());
    }

    #[test]
    fn recur_iterations_and_site_completion_get_distinct_coordinates() {
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);

        let __start = seed_recur_source(&mut recorder, "recur", 2);
        recorder.record(TraceEvent::RecurTileStart(__start));
        let iter0 = recorder.record(TraceEvent::RecurTileIterationExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: Some(raster_core::draft::RecurControlKind::Continue),
            recur_state: None,
        }));
        let iter1 = recorder.record(TraceEvent::RecurTileIterationExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: Some(raster_core::draft::RecurControlKind::Continue),
            recur_state: None,
        }));
        let site = recorder.record(TraceEvent::RecurTileEnd(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: Some(site_object()),
            draft_transition_witness: None,
            recur_control: Some(raster_core::draft::RecurControlKind::Continue),
            recur_state: None,
        }));
        let after = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "after".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));

        assert_eq!(iter0.coordinates(), &CfsCoordinates(vec![1, 1]));
        assert_eq!(iter1.coordinates(), &CfsCoordinates(vec![1, 2]));
        // The site closes at `[-1]`; its object is still written at `[1]`.
        assert_eq!(site.coordinates(), &CfsCoordinates(vec![-1]));
        assert_eq!(after.coordinates(), &CfsCoordinates(vec![2]));
    }

    #[test]
    fn recur_sequence_iterations_restore_parent_coordinates_before_site_completion() {
        let mut recorder = recorder_with_recur_sequence_site();
        start_main(&mut recorder);

        let __start = seed_recur_source(&mut recorder, "child", 2);
        recorder.record(TraceEvent::RecurSequenceStart(__start));
        let iter0_start = recorder.record(TraceEvent::RecurSequenceIterationStart(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iter0_inner = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "inner".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iter0_end = recorder.record(TraceEvent::RecurSequenceIterationEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iter1_start = recorder.record(TraceEvent::RecurSequenceIterationStart(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iter1_inner = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "inner".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iter1_end = recorder.record(TraceEvent::RecurSequenceIterationEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let site = recorder.record(TraceEvent::RecurSequenceEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: Some(site_object()),
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let after = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "after".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));

        // An iteration's `End` closes at its own coordinate, distinct from the
        // `Start`'s, so nothing keyed on position can confuse the two. The
        // bracket is symmetric because positions are 1-based: open `1` closes
        // at `-1`, with no offset to carry.
        assert_eq!(iter0_start.coordinates(), &CfsCoordinates(vec![1, 1]));
        assert_eq!(iter0_inner.coordinates(), &CfsCoordinates(vec![1, 1, 1]));
        assert_eq!(iter0_end.coordinates(), &CfsCoordinates(vec![1, -1]));
        assert_eq!(iter1_start.coordinates(), &CfsCoordinates(vec![1, 2]));
        assert_eq!(iter1_inner.coordinates(), &CfsCoordinates(vec![1, 2, 1]));
        assert_eq!(iter1_end.coordinates(), &CfsCoordinates(vec![1, -2]));
        assert_ne!(iter0_end.coordinates(), iter0_start.coordinates());
        assert_ne!(iter1_end.coordinates(), iter1_start.coordinates());
        assert_eq!(site.coordinates(), &CfsCoordinates(vec![-1]));
        assert_eq!(after.coordinates(), &CfsCoordinates(vec![2]));
    }

    /// `sequence_id`'s vocabulary, made executable.
    ///
    /// The field means two different things depending on the step kind, and the
    /// transition guest derives it to stop it being free entropy in the trace
    /// leaf (`checks::cfs::verify_sequence_id`), so the rule it derives against
    /// belongs in a test rather than in a reader's head:
    ///
    /// - a **boundary** step (`SequenceStart`/`SequenceEnd`) names the sequence
    ///   it enters or leaves — the callee;
    /// - every **other** step names the frame it executes in.
    ///
    /// The two recur kinds part company here, which is the row worth the test:
    /// a recur *tile* pushes no frame, so its iterations stay in the containing
    /// sequence, while a recur *sequence* does, so its body's steps name the
    /// site.
    #[test]
    fn sequence_id_names_the_callee_at_boundaries_and_the_frame_everywhere_else() {
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);
        let __start = seed_recur_source(&mut recorder, "recur", 1);
        let site_start = recorder.record(TraceEvent::RecurTileStart(__start));
        let iter = recorder.record(TraceEvent::RecurTileIterationExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: Some(raster_core::draft::RecurControlKind::Continue),
            recur_state: None,
        }));
        let site_end = recorder.record(TraceEvent::RecurTileEnd(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: Some(site_object()),
            draft_transition_witness: None,
            recur_control: Some(raster_core::draft::RecurControlKind::Continue),
            recur_state: None,
        }));

        // A site's own steps name the enclosing sequence (the site is
        // `site_id`), and a recur *tile* pushes no frame, so its iterations
        // stay in `main` too.
        assert_eq!(site_start.sequence_id, "main");
        assert_eq!(iter.sequence_id, "main");
        assert_eq!(site_end.sequence_id, "main");

        let mut recorder = recorder_with_recur_sequence_site();
        start_main(&mut recorder);
        let __start = seed_recur_source(&mut recorder, "child", 1);
        let site_start = recorder.record(TraceEvent::RecurSequenceStart(__start));
        let iteration_start =
            recorder.record(TraceEvent::RecurSequenceIterationStart(FnCallRecord {
                fn_name: "child".to_string(),
                input: None,
                output: None,
                draft_transition_witness: None,
                recur_control: None,
                recur_state: None,
            }));
        let inner = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "inner".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let iteration_end = recorder.record(TraceEvent::RecurSequenceIterationEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));
        let site_end = recorder.record(TraceEvent::RecurSequenceEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: Some(site_object()),
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }));

        assert_eq!(site_start.sequence_id, "main");
        assert_eq!(iteration_start.sequence_id, "child");
        // A recur *sequence* does push a frame, so its body names the site.
        assert_eq!(inner.sequence_id, "child");
        assert_eq!(iteration_end.sequence_id, "child");
        // The site's closing `Exec` sits back in the containing sequence.
        assert_eq!(site_end.sequence_id, "main");
    }

    /// The vocabulary table in `TraceEvent`'s doc comment, made executable:
    /// which `StepKind` each event becomes, and where it lands.
    ///
    /// The row worth the test is `RecurTileIterationExec` — an iteration of a
    /// recur *tile* records as `Exec(Tile)`, never `Exec(RecurTile)`, because
    /// `RecurTile` names the site only.
    ///
    /// `ProgramStart` / `ProgramEnd` are left to the entrypoint suite: they
    /// need entry-argument and storage-root setup these fixtures don't carry.
    /// A site's close: every site writes its object (`close_site`).
    fn site_object() -> raster_core::trace::FnOutput {
        raster_core::trace::FnOutput::new(vec![1, 2, 3], "SiteObject".to_string())
    }

    fn site_end(fn_name: &str) -> FnCallRecord {
        FnCallRecord {
            output: Some(site_object()),
            ..call(fn_name)
        }
    }

    fn call(fn_name: &str) -> FnCallRecord {
        FnCallRecord {
            fn_name: fn_name.to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
            recur_control: None,
            recur_state: None,
        }
    }

    /// A recur iteration's record. Distinct from [`call`] because a recur
    /// iteration always carries a control — the recorder rejects one that does
    /// not, so a fixture that omits it is not a smaller version of a real
    /// event, it is an impossible one.
    fn recur_call(fn_name: &str, control: raster_core::draft::RecurControlKind) -> FnCallRecord {
        FnCallRecord {
            recur_control: Some(control),
            ..call(fn_name)
        }
    }

    /// The recorder actually advances a progress stack, rather than stamping a
    /// placeholder.
    ///
    /// This is the property revision 1 of `recur-progress-commitment.md` could
    /// not satisfy: two of the frame's fields were reachable only from the
    /// replay journal, which the recorder never sees, so no honest producer
    /// could compute the value the guest would demand. Asserting that the
    /// commitment *moves* across a sweep — and returns to the empty-stack value
    /// once the site closes — is the cheapest check that the producer half is
    /// real.
    #[test]
    fn recorder_stamps_a_live_recur_progress_commitment() {
        let empty = RecurProgressStack::new().commitment();

        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);

        let start = seed_recur_source(&mut recorder, "recur", 2);
        let site_start = recorder.record(TraceEvent::RecurTileStart(start));
        let iter0 = recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));
        let iter1 = recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));
        let site_end = recorder.record(TraceEvent::RecurTileEnd(site_end("recur")));

        // Open, and each iteration, must move it.
        assert_ne!(site_start.recur_progress_commitment, empty, "site open");
        assert_ne!(iter0.recur_progress_commitment, site_start.recur_progress_commitment);
        assert_ne!(iter1.recur_progress_commitment, iter0.recur_progress_commitment);

        // Closing pops the frame, so the stack is empty again — "no loop in
        // flight" as a positive statement, not an absent field.
        assert_eq!(site_end.recur_progress_commitment, empty, "site close");
    }

    /// Every step's retained stack hashes to that step's own stamped
    /// commitment.
    ///
    /// This is the seed's correctness asserted *at its source*. A fraud-proof
    /// window seeded from `recur_progress_after` is validated by the guest
    /// advancing it and comparing against the recorded commitment, so a
    /// divergence here is precisely a wrong seed — and would otherwise surface
    /// only as a proof that fails for unclear reasons.
    /// See `window-seed-reconstruction.md` §Verification.
    #[test]
    fn recur_progress_after_agrees_with_every_stamped_commitment() {
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);

        let start = seed_recur_source(&mut recorder, "recur", 2);
        let steps = vec![
            recorder.record(TraceEvent::RecurTileStart(start)),
            recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue))),
            recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue))),
            recorder.record(TraceEvent::RecurTileEnd(site_end("recur"))),
        ];

        for step in &steps {
            let retained = recorder
                .recur_progress_after(step.exec_index)
                .unwrap_or_else(|| panic!("no retained stack for exec_index {}", step.exec_index));
            assert_eq!(
                retained.commitment(),
                step.recur_progress_commitment,
                "retained stack disagrees with the stamped commitment at {:?}",
                step.coordinates(),
            );
        }
    }

    /// The same agreement across a recur *sequence* site, including a plain
    /// tile executing inside an iteration — the shape a window is most likely
    /// to open on, since the loop is live but the step itself is ordinary.
    #[test]
    fn recur_progress_after_agrees_across_a_recur_sequence_site() {
        let mut recorder = recorder_with_recur_sequence_site();
        start_main(&mut recorder);

        let start = seed_recur_source(&mut recorder, "child", 2);
        let mut steps = vec![recorder.record(TraceEvent::RecurSequenceStart(start))];
        for _ in 0..2 {
            steps.push(recorder.record(TraceEvent::RecurSequenceIterationStart(call("child"))));
            steps.push(recorder.record(TraceEvent::TileExec(call("inner"))));
            steps.push(recorder.record(TraceEvent::RecurSequenceIterationEnd(call("child"))));
        }
        steps.push(recorder.record(TraceEvent::RecurSequenceEnd(site_end("child"))));

        for step in &steps {
            let retained = recorder
                .recur_progress_after(step.exec_index)
                .unwrap_or_else(|| panic!("no retained stack for exec_index {}", step.exec_index));
            assert_eq!(
                retained.commitment(),
                step.recur_progress_commitment,
                "retained stack disagrees with the stamped commitment at {:?}",
                step.coordinates(),
            );
        }
    }

    /// The seed a mid-loop window would open with is a *live* stack, not the
    /// empty one — the whole point of retaining it.
    ///
    /// Reading the stack after iteration 0 is exactly what
    /// `prove()` does for a window whose first step is iteration 1.
    #[test]
    fn recur_progress_after_a_live_iteration_is_not_the_empty_stack() {
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);

        let start = seed_recur_source(&mut recorder, "recur", 2);
        recorder.record(TraceEvent::RecurTileStart(start));
        let iter0 = recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));
        let site_end_before_close = recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));

        let seed = recorder
            .recur_progress_after(iter0.exec_index)
            .expect("iteration 0 retained a stack");
        assert!(!seed.is_empty(), "a window opening after iteration 0 is mid-loop");
        assert_eq!(seed.depth(), 1);

        // And the last iteration is still inside the site: only the site's
        // `End` pops the frame.
        let seed = recorder
            .recur_progress_after(site_end_before_close.exec_index)
            .expect("iteration 1 retained a stack");
        assert!(!seed.is_empty());
    }

    /// A sweep that stops short of `L` is rejected at `close_site` by rule 5.
    #[test]
    #[should_panic(expected = "Recur progress violation")]
    fn recorder_rejects_a_sweep_that_does_not_cover_its_source() {
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);
        let start = seed_recur_source(&mut recorder, "recur", 3);
        recorder.record(TraceEvent::RecurTileStart(start));
        recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));
        // Only one of three elements covered, and no Break.
        recorder.record(TraceEvent::RecurTileEnd(site_end("recur")));
    }

    #[test]
    fn vocabulary_table_holds_for_the_recorder() {
        fn site_end(fn_name: &str) -> FnCallRecord {
            FnCallRecord {
                output: Some(site_object()),
                ..call(fn_name)
            }
        }

        fn call(fn_name: &str) -> FnCallRecord {
            FnCallRecord {
                fn_name: fn_name.to_string(),
                input: None,
                output: None,
                draft_transition_witness: None,
                recur_control: None,
                recur_state: None,
            }
        }

        /// The `becomes` column, as a string so a mismatch reads plainly.
        fn becomes(record: &StepRecord) -> &'static str {
            match &record.kind {
                StepKind::ProgramStart(_) => "ProgramStart",
                StepKind::ProgramEnd(_) => "ProgramEnd",
                StepKind::SequenceStart { .. } => "SequenceStart",
                StepKind::SequenceEnd { .. } => "SequenceEnd",
                StepKind::Exec(step) => match &step.target {
                    ExecTarget::Tile(_) => "Exec(Tile)",
                },
                StepKind::RecurStart(_) => "RecurStart",
                StepKind::RecurEnd(_) => "RecurEnd",
            }
        }

        fn assert_row(record: &StepRecord, kind: &str, coordinates: &[CfsCoordinate]) {
            assert_eq!(becomes(record), kind, "event became the wrong StepKind");
            let expected = CfsCoordinates(coordinates.to_vec());
            assert_eq!(record.coordinates(), &expected, "event landed wrong");
        }

        // Tile family. The site's own event comes after its iterations.
        let mut recorder = recorder_with_recur_site();
        start_main(&mut recorder);
        let __start = seed_recur_source(&mut recorder, "recur", 1);
        recorder.record(TraceEvent::RecurTileStart(__start));
        let tile_iteration = recorder.record(TraceEvent::RecurTileIterationExec(recur_call("recur", raster_core::draft::RecurControlKind::Continue)));
        let tile_site = recorder.record(TraceEvent::RecurTileEnd(site_end("recur")));
        let tile = recorder.record(TraceEvent::TileExec(call("after")));

        // Sequence family, same shape: iterations first, then the site.
        let mut recorder = recorder_with_recur_sequence_site();
        start_main(&mut recorder);
        let __start = seed_recur_source(&mut recorder, "child", 1);
        recorder.record(TraceEvent::RecurSequenceStart(__start));
        let iter_start = recorder.record(TraceEvent::RecurSequenceIterationStart(call("child")));
        let iter_tile = recorder.record(TraceEvent::TileExec(call("inner")));
        let iter_end = recorder.record(TraceEvent::RecurSequenceIterationEnd(call("child")));
        let seq_site = recorder.record(TraceEvent::RecurSequenceEnd(site_end("child")));

        // Items land at their sequence's coordinates, [s].
        assert_row(&tile, "Exec(Tile)", &[2]);
        assert_row(&tile_site, "RecurEnd", &[closing_coordinate(1)]);
        assert_row(&seq_site, "RecurEnd", &[closing_coordinate(1)]);
        // A tile inside a recur-sequence iteration is an item of that
        // iteration's own sequence: [s] relative to it, [1,1][1] absolute.
        assert_row(&iter_tile, "Exec(Tile)", &[1, 1, 1]);

        // Iterations land one level deeper, at [s][i].
        assert_row(&tile_iteration, "Exec(Tile)", &[1, 1]);
        assert_row(&iter_start, "SequenceStart", &[1, 1]);
        // The close of iteration 0, not the open again.
        assert_row(&iter_end, "SequenceEnd", &[1, closing_coordinate(1)]);
    }
}

/// Which list a recur site sweeps, as the guest will re-derive it: the site
/// `Start`'s `"input"` binding, through the shared
/// [`raster_core::recur_progress::source_identity`].
fn recur_source_identity(input: Option<&FnInput>) -> raster_core::input::Hash32 {
    let binding = input
        .and_then(|input| input.storage().get("input"))
        .expect("Recur site Start must record its source binding");
    raster_core::recur_progress::source_identity(binding)
}
