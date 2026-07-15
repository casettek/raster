use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema, SequenceChildId};
use raster_core::draft::DraftTransitionWitness;
use raster_core::input::{InternalRef, SelectionWitness, SelectorPath};
use raster_core::trace::{
    FnInput, InternalInput, RecurSequenceExecRecord, RecurTileExecRecord,
    SequenceEndRecord, SequenceStartRecord, StepRecord, TileExecRecord, TraceEvent,
};
use sha2::{Digest, Sha256};

use std::collections::{HashMap, VecDeque};

use crate::internal_storage::{InternalStorageManager, InternalWriteRecord};
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
    current_index: u32,
    parent_coordinates: CfsCoordinates,
}

#[derive(Debug, Clone)]
struct RecurExecutionState {
    site_id: String,
    sequence_coordinates: CfsCoordinates,
    site_coordinates: CfsCoordinates,
    intra_sequence_index: u32,
    next_iteration_index: u32,
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
            current_index: 0,
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
            current_index: 0,
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
    internal_input: InternalInput,
    internal_write: Option<InternalWriteRecord>,
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

    pub fn internal_input(&self) -> InternalInput {
        self.internal_input.clone()
    }

    pub fn internal_write(&self) -> Option<InternalWriteRecord> {
        self.internal_write.clone()
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
        internal_write: Option<InternalWriteRecord>,
    ) {
        match event {
            TraceEvent::SequenceStart(trace_item) | TraceEvent::RecurSequenceStart(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: None,
                        internal_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.internal().clone())
                            .unwrap_or_default(),
                        internal_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::SequenceEnd(trace_item) | TraceEvent::RecurSequenceEnd(trace_item) => {
                let trace_io = self.0.get_mut(&coordinates).unwrap_or_else(|| {
                    panic!(
                        "Missing step witness entry for SequenceEnd at coordinates {:?}. Expected a matching SequenceStart to be recorded first.",
                        coordinates
                    )
                });
                trace_io.output_data = trace_item.output.as_ref().map(|output| output.data.clone());
                trace_io.internal_write = internal_write;
                trace_io.draft_transition_witness = trace_item.draft_transition_witness;
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
                        internal_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.internal().clone())
                            .unwrap_or_default(),
                        internal_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::RecurTileIterationExec(trace_item)
            | TraceEvent::RecurTileExec(trace_item)
            | TraceEvent::RecurSequenceExec(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: trace_item
                            .output
                            .as_ref()
                            .map(|output| output.data().to_vec()),
                        internal_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.internal().clone())
                            .unwrap_or_default(),
                        internal_write,
                        draft_transition_witness: trace_item.draft_transition_witness,
                    },
                );
            }
            TraceEvent::EntrypointBind(_) => {
                // A declaration, not a consumer: no CFS inputs, no input
                // witness bytes of its own (see `EntrypointRecord`'s
                // `input_source_commitment`, which commits to an empty
                // `FnInput` — matching `SequenceChildItem::Entrypoint`'s
                // empty `inputs()`).
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: None,
                        input_source_witness: Some(FnInput {
                            data: Vec::new(),
                            values: Vec::new(),
                            args: Vec::new(),
                            internal: InternalInput::new(),
                        }),
                        output_data: None,
                        internal_input: InternalInput::new(),
                        internal_write,
                        draft_transition_witness: None,
                    },
                );
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
    internal_storage: InternalStorageManager,
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
            internal_storage: InternalStorageManager::new(),
        }
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

    pub fn internal_store_snapshot(&self) -> crate::internal_storage::InternalStoreSnapshot {
        self.internal_storage.snapshot()
    }

    pub fn internal_selection_witness(
        &self,
        reference: &InternalRef,
        selector: &SelectorPath,
    ) -> raster_core::Result<SelectionWitness> {
        self.internal_storage.selection_witness(reference, selector)
    }

    pub fn io_data_at(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        self.witness_store.get(coordinates).map(|trace_io| {
            (trace_io.input_data.clone(), trace_io.output_data.clone())
        })
    }

    pub fn record(&mut self, event: TraceEvent) -> StepRecord {
        self.exec_index += 1;
        let exec_index = self.exec_index;

        let step_record = match event.clone() {
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

                let record = SequenceStartRecord {
                    exec_index,
                    sequence_id: fn_call_record.fn_name.clone(),
                    coordinates: coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                };

                self.witness_store.insert(coordinates, event.clone(), None);

                StepRecord::SequenceStart(record)
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

                let record = SequenceEndRecord {
                    exec_index,
                    coordinates: sequence_coordinates.clone(),
                    sequence_id: fn_call_record.fn_name.clone(),
                    output_commitment,
                };

                self.sequence_callstack
                    .pop()
                    .expect("Corrupted sequence stack");

                self.witness_store.insert(sequence_coordinates, event, None);

                StepRecord::SequenceEnd(record)
            }
            TraceEvent::RecurSequenceStart(fn_call_record) => {
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
                            next_iteration_index: 0,
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
                recur_state.next_iteration_index += 1;
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

                let record = SequenceStartRecord {
                    exec_index,
                    sequence_id: fn_call_record.fn_name.clone(),
                    coordinates: iteration_coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                };

                self.witness_store
                    .insert(iteration_coordinates, event.clone(), None);

                StepRecord::SequenceStart(record)
            }
            TraceEvent::RecurSequenceEnd(fn_call_record) => {
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

                let record = SequenceEndRecord {
                    exec_index,
                    coordinates: sequence_coordinates.clone(),
                    sequence_id: fn_call_record.fn_name.clone(),
                    output_commitment,
                };

                self.sequence_callstack
                    .pop()
                    .expect("Corrupted recur sequence stack");

                self.witness_store
                    .insert(sequence_coordinates, event.clone(), None);

                StepRecord::SequenceEnd(record)
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
                let input_commitment = input
                    .as_ref()
                    .map(|input| Sha256Commitment::from(input).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let output = fn_call_record.output;
                let internal_write = output.as_ref().map(|output| {
                    self.internal_storage.append_serialized_bytes(
                        &output.data,
                        tile_coordinates.clone(),
                        output.raster.clone(),
                    )
                });
                let output_commitment = internal_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();

                let record = TileExecRecord {
                    exec_index,
                    tile_id: fn_call_record.fn_name,
                    sequence_id,
                    intra_sequence_index: parent_current_index,
                    coordinates: tile_coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                    output_commitment,
                    internal_store_root_before: internal_write
                        .as_ref()
                        .map(|write| write.store_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_root_after: internal_write
                        .as_ref()
                        .map(|write| write.store_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_index_root_before: internal_write
                        .as_ref()
                        .map(|write| write.index_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                    internal_store_index_root_after: internal_write
                        .as_ref()
                        .map(|write| write.index_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                };

                self.witness_store
                    .insert(tile_coordinates, event.clone(), internal_write);

                StepRecord::TileExec(record)
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
                        next_iteration_index: 0,
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
                recur_state.next_iteration_index += 1;

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|input| Sha256Commitment::from(input).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let output = fn_call_record.output;
                let internal_write = output.as_ref().map(|output| {
                    self.internal_storage.append_serialized_bytes(
                        &output.data,
                        tile_coordinates.clone(),
                        output.raster.clone(),
                    )
                });
                let output_commitment = internal_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();

                let record = TileExecRecord {
                    exec_index,
                    tile_id: fn_call_record.fn_name,
                    sequence_id,
                    intra_sequence_index: recur_state.intra_sequence_index,
                    coordinates: tile_coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                    output_commitment,
                    internal_store_root_before: internal_write
                        .as_ref()
                        .map(|write| write.store_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_root_after: internal_write
                        .as_ref()
                        .map(|write| write.store_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_index_root_before: internal_write
                        .as_ref()
                        .map(|write| write.index_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                    internal_store_index_root_after: internal_write
                        .as_ref()
                        .map(|write| write.index_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                };

                self.witness_store
                    .insert(tile_coordinates, event.clone(), internal_write);

                StepRecord::TileExec(record)
            }
            TraceEvent::RecurTileExec(fn_call_record) => {
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
                        next_iteration_index: 0,
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

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|input| Sha256Commitment::from(input).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let output = fn_call_record.output;
                let internal_write = output.as_ref().map(|output| {
                    self.internal_storage.append_serialized_bytes(
                        &output.data,
                        recur_state.site_coordinates.clone(),
                        output.raster.clone(),
                    )
                });
                let output_commitment = internal_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();

                let record = RecurTileExecRecord {
                    exec_index,
                    recur_tile_id: fn_call_record.fn_name,
                    sequence_id,
                    intra_sequence_index: recur_state.intra_sequence_index,
                    coordinates: recur_state.site_coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                    output_commitment,
                    internal_store_root_before: internal_write
                        .as_ref()
                        .map(|write| write.store_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_root_after: internal_write
                        .as_ref()
                        .map(|write| write.store_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_index_root_before: internal_write
                        .as_ref()
                        .map(|write| write.index_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                    internal_store_index_root_after: internal_write
                        .as_ref()
                        .map(|write| write.index_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                };

                self.witness_store.insert(
                    recur_state.site_coordinates.clone(),
                    event.clone(),
                    internal_write,
                );

                StepRecord::RecurTileExec(record)
            }
            TraceEvent::RecurSequenceExec(fn_call_record) => {
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
                            next_iteration_index: 0,
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

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|input| Sha256Commitment::from(input).into())
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let output = fn_call_record.output;
                let internal_write = output.as_ref().map(|output| {
                    self.internal_storage.append_serialized_bytes(
                        &output.data,
                        recur_state.site_coordinates.clone(),
                        output.raster.clone(),
                    )
                });
                let output_commitment = internal_write
                    .as_ref()
                    .map(|write| write.entry.object_commitment.clone())
                    .unwrap_or_default();

                let record = RecurSequenceExecRecord {
                    exec_index,
                    recur_sequence_id: fn_call_record.fn_name,
                    sequence_id,
                    intra_sequence_index: recur_state.intra_sequence_index,
                    coordinates: recur_state.site_coordinates.clone(),
                    input_commitment,
                    input_source_commitment,
                    output_commitment,
                    internal_store_root_before: internal_write
                        .as_ref()
                        .map(|write| write.store_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_root_after: internal_write
                        .as_ref()
                        .map(|write| write.store_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().root),
                    internal_store_index_root_before: internal_write
                        .as_ref()
                        .map(|write| write.index_root_before.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                    internal_store_index_root_after: internal_write
                        .as_ref()
                        .map(|write| write.index_root_after.clone())
                        .unwrap_or_else(|| self.internal_storage.snapshot().index_root),
                };

                self.witness_store.insert(
                    recur_state.site_coordinates.clone(),
                    event.clone(),
                    internal_write,
                );

                StepRecord::RecurSequenceExec(record)
            }
            TraceEvent::EntrypointBind(bind_event) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("Entrypoint bind can't be recorded without sequence context");
                let sequence_id = current_sequence_state.id.clone();
                let parent_current_index = current_sequence_state.current_index;

                let coordinates = self.cfs_cursor.get_child_coordinates(
                    &sequence_coordinates,
                    parent_current_index,
                    SequenceChildId::Entrypoint,
                );
                current_sequence_state.current_index += 1;

                let names: Vec<String> = bind_event
                    .arguments
                    .iter()
                    .map(|argument| argument.name.clone())
                    .collect();
                let sources = bind_event
                    .arguments
                    .iter()
                    .map(|argument| {
                        let kind = match argument.encoding {
                            raster_core::input::ExternalEncoding::Raster => {
                                crate::internal_storage::ReferencedSourceKind::Raster
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
                        crate::internal_storage::ReferencedSource {
                            name: argument.name.clone(),
                            commitment: argument.commitment.clone(),
                            kind,
                        }
                    })
                    .collect();

                let resolver = crate::external_storage::ExternalStorageManager::from_cli_args()
                    .expect("Failed to read --input/--input-manifest CLI context")
                    .expect(
                        "Entrypoint bind requires CLI input context from --input and --input-manifest",
                    );
                self.internal_storage
                    .set_external_resolver(std::sync::Arc::new(resolver));

                let write = self.internal_storage.append_referenced_object(
                    crate::internal_storage::ReferencedObject { sources },
                    coordinates.clone(),
                );
                let output_commitment = write.entry.object_commitment.clone();

                let empty_input_source_commitment = input_source_commitment(&FnInput {
                    data: Vec::new(),
                    values: Vec::new(),
                    args: Vec::new(),
                    internal: InternalInput::new(),
                });

                let record = raster_core::trace::EntrypointRecord {
                    exec_index,
                    sequence_id,
                    coordinates: coordinates.clone(),
                    op: raster_core::trace::EntrypointOp::BindEntryArguments { names },
                    input_source_commitment: empty_input_source_commitment,
                    output_commitment,
                    internal_store_root_before: write.store_root_before.clone(),
                    internal_store_root_after: write.store_root_after.clone(),
                    internal_store_index_root_before: write.index_root_before.clone(),
                    internal_store_index_root_after: write.index_root_after.clone(),
                };

                self.witness_store.insert(coordinates, event.clone(), Some(write));

                StepRecord::Entrypoint(record)
            }
        };

        step_record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::{
        RecurSequenceItem, RecurTileItem, SequenceChildItem, SequenceDef, TileDef, TileItem,
    };
    use raster_core::trace::FnCallRecord;

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
                    }),
                    SequenceChildItem::Tile(TileItem {
                        id: "after".to_string(),
                        sources: vec![],
                    }),
                ],
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
                        }),
                        SequenceChildItem::Tile(TileItem {
                            id: "after".to_string(),
                            sources: vec![],
                        }),
                    ],
                },
                SequenceDef {
                    id: "child".to_string(),
                    input_sources: vec![],
                    items: vec![SequenceChildItem::Tile(TileItem {
                        id: "inner".to_string(),
                        sources: vec![],
                    })],
                },
            ],
        })
    }

    #[test]
    #[should_panic(
        expected = "Missing step witness entry for SequenceEnd at coordinates CfsCoordinates([]). Expected a matching SequenceStart to be recorded first."
    )]
    fn sequence_end_without_matching_start_reports_coordinates() {
        let mut store = StepWitnessStore::new();
        store.insert(
            CfsCoordinates(vec![]),
            TraceEvent::SequenceEnd(FnCallRecord {
                fn_name: "main".to_string(),
                input: None,
                output: None,
                draft_transition_witness: None,
            }),
            None,
        );
    }

    #[test]
    fn recur_iterations_and_site_completion_get_distinct_coordinates() {
        let mut recorder = recorder_with_recur_site();
        recorder.record(TraceEvent::SequenceStart(FnCallRecord {
            fn_name: "main".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));

        let iter0 = recorder.record(TraceEvent::RecurTileIterationExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter1 = recorder.record(TraceEvent::RecurTileIterationExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let site = recorder.record(TraceEvent::RecurTileExec(FnCallRecord {
            fn_name: "recur".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let after = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "after".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));

        assert_eq!(iter0.coordinates(), &CfsCoordinates(vec![0, 0]));
        assert_eq!(iter1.coordinates(), &CfsCoordinates(vec![0, 1]));
        assert_eq!(site.coordinates(), &CfsCoordinates(vec![0]));
        assert_eq!(after.coordinates(), &CfsCoordinates(vec![1]));
    }

    #[test]
    fn recur_sequence_iterations_restore_parent_coordinates_before_site_completion() {
        let mut recorder = recorder_with_recur_sequence_site();
        recorder.record(TraceEvent::SequenceStart(FnCallRecord {
            fn_name: "main".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));

        let iter0_start = recorder.record(TraceEvent::RecurSequenceStart(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter0_inner = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "inner".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter0_end = recorder.record(TraceEvent::RecurSequenceEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter1_start = recorder.record(TraceEvent::RecurSequenceStart(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter1_inner = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "inner".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let iter1_end = recorder.record(TraceEvent::RecurSequenceEnd(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let site = recorder.record(TraceEvent::RecurSequenceExec(FnCallRecord {
            fn_name: "child".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));
        let after = recorder.record(TraceEvent::TileExec(FnCallRecord {
            fn_name: "after".to_string(),
            input: None,
            output: None,
            draft_transition_witness: None,
        }));

        assert_eq!(iter0_start.coordinates(), &CfsCoordinates(vec![0, 0]));
        assert_eq!(iter0_inner.coordinates(), &CfsCoordinates(vec![0, 0, 0]));
        assert_eq!(iter0_end.coordinates(), &CfsCoordinates(vec![0, 0]));
        assert_eq!(iter1_start.coordinates(), &CfsCoordinates(vec![0, 1]));
        assert_eq!(iter1_inner.coordinates(), &CfsCoordinates(vec![0, 1, 0]));
        assert_eq!(iter1_end.coordinates(), &CfsCoordinates(vec![0, 1]));
        assert_eq!(site.coordinates(), &CfsCoordinates(vec![0]));
        assert_eq!(after.coordinates(), &CfsCoordinates(vec![1]));
    }
}
