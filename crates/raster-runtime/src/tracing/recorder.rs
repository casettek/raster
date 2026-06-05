use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema, SequenceChildId};
use raster_core::trace::{
    ExternalInput, FnInput, InternalInput, SequenceEndRecord, SequenceStartRecord, StepRecord,
    TileExecRecord, TraceEvent,
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
        };
        self.callstack.push_back(sequence_execution_state);
    }

    fn pop(&mut self) -> Option<SequenceState> {
        let mut parent_sequence_coordinates =
            VecDeque::from(self.current_sequence_coordinates.0.clone());

        parent_sequence_coordinates.pop_back();
        self.current_sequence_coordinates = CfsCoordinates(parent_sequence_coordinates.into());

        self.callstack.pop_back()
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
    external_input: ExternalInput,
    internal_input: InternalInput,
    internal_write: Option<InternalWriteRecord>,
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

    pub fn external_input(&self) -> ExternalInput {
        self.external_input.clone()
    }

    pub fn internal_input(&self) -> InternalInput {
        self.internal_input.clone()
    }

    pub fn internal_write(&self) -> Option<InternalWriteRecord> {
        self.internal_write.clone()
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
            TraceEvent::SequenceStart(trace_item) => {
                self.0.insert(
                    coordinates,
                    StepWitnessData {
                        input_data: trace_item.input.as_ref().map(|input| input.data().to_vec()),
                        input_source_witness: trace_item.input.clone(),
                        output_data: None,
                        external_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.external().clone())
                            .unwrap_or_default(),
                        internal_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.internal().clone())
                            .unwrap_or_default(),
                        internal_write,
                    },
                );
            }
            TraceEvent::SequenceEnd(trace_item) => {
                let trace_io = self.0.get_mut(&coordinates).unwrap_or_else(|| {
                    panic!(
                        "Missing step witness entry for SequenceEnd at coordinates {:?}. Expected a matching SequenceStart to be recorded first.",
                        coordinates
                    )
                });
                trace_io.output_data = trace_item.output.as_ref().map(|output| output.data.clone());
                trace_io.internal_write = internal_write;
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
                        external_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.external().clone())
                            .unwrap_or_default(),
                        internal_input: trace_item
                            .input
                            .as_ref()
                            .map(|input| input.internal().clone())
                            .unwrap_or_default(),
                        internal_write,
                    },
                );
            }
        }
    }

    pub fn get(&self, coordinates: &CfsCoordinates) -> Option<&StepWitnessData> {
        self.0.get(coordinates)
    }
}

fn external_input_commitment(external_input: &ExternalInput) -> Vec<u8> {
    let bytes = raster_core::postcard::to_allocvec(external_input).unwrap_or_default();
    Sha256::digest(bytes).to_vec()
}

fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    Sha256::digest(input.source_witness_bytes()).to_vec()
}

#[derive(Debug, Clone)]
pub struct TraceRecorder {
    exec_index: u64,
    sequence_callstack: SequenceCallstack,
    cfs_cursor: CfsCursor,
    witness_store: StepWitnessStore,
    internal_storage: InternalStorageManager,
}

impl TraceRecorder {
    pub fn new(cfs: ControlFlowSchema) -> Self {
        Self {
            exec_index: 0,
            sequence_callstack: SequenceCallstack::new(),
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

    pub fn io_data_at(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput)> {
        self.witness_store.get(coordinates).map(|trace_io| {
            (
                trace_io.input_data.clone(),
                trace_io.output_data.clone(),
                trace_io.external_input.clone(),
            )
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
                let external_input_commitment = input
                    .as_ref()
                    .map(|input| external_input_commitment(input.external()))
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
                    external_input_commitment,
                };

                self.witness_store.insert(coordinates, event.clone(), None);

                StepRecord::SequenceStart(record)
            }
            TraceEvent::SequenceEnd(fn_call_record) => {
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
                    .expect("Corrupted sequence stack");

                self.witness_store.insert(sequence_coordinates, event, None);

                StepRecord::SequenceEnd(record)
            }
            TraceEvent::TileExec(fn_call_record) => {
                let sequence_coordinates =
                    self.sequence_callstack.current_sequence_coordinates.clone();
                let current_sequence_state = self
                    .sequence_callstack
                    .last_mut()
                    .expect("Tile can't be called without sequence context");

                let sequence_id = current_sequence_state.id.clone();
                let parent_current_index = current_sequence_state.current_index;

                let tile_coordinates = self.cfs_cursor.get_child_coordinates(
                    &sequence_coordinates,
                    parent_current_index,
                    SequenceChildId::Tile(fn_call_record.fn_name.clone()),
                );

                current_sequence_state.current_index += 1;

                let input = fn_call_record.input;
                let input_commitment = input
                    .as_ref()
                    .map(|input| Sha256Commitment::from(input).into())
                    .unwrap_or_default();
                let external_input_commitment = input
                    .as_ref()
                    .map(|input| external_input_commitment(input.external()))
                    .unwrap_or_default();
                let input_source_commitment = input
                    .as_ref()
                    .map(input_source_commitment)
                    .unwrap_or_default();

                let output = fn_call_record.output;
                let internal_write = output
                    .as_ref()
                    .map(|output| {
                        self.internal_storage
                            .append_serialized_bytes(&output.data, tile_coordinates.clone())
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
                    external_input_commitment,
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
        };

        step_record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::trace::FnCallRecord;

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
            }),
            None,
        );
    }
}
