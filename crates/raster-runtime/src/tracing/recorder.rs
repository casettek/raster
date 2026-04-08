use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema, SequenceChildId};
use raster_core::trace::{
    SequenceEndRecord, SequenceStartRecord, StepRecord, TileExecRecord, TraceEvent,
};

use std::collections::{HashMap, VecDeque};

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
pub struct TraceIO {
    input_data: Option<Vec<u8>>,
    output_data: Option<Vec<u8>>,
}

#[derive(Debug, Default, Clone)]
pub struct TraceIOStore(HashMap<CfsCoordinates, TraceIO>);

impl TraceIOStore {
    fn new() -> Self {
        TraceIOStore(HashMap::new())
    }

    pub fn insert(&mut self, coordinates: CfsCoordinates, event: TraceEvent) {
        match event {
            TraceEvent::SequenceStart(trace_item) => {
                self.0.insert(
                    coordinates,
                    TraceIO {
                        input_data: trace_item.input.as_ref().map(|input| input.data.clone()),
                        output_data: None,
                    },
                );
            }
            TraceEvent::SequenceEnd(trace_item) => {
                self.0.get_mut(&coordinates).unwrap().output_data =
                    trace_item.output.as_ref().map(|output| output.data.clone());
            }
            TraceEvent::TileExec(trace_item) => {
                self.0.insert(
                    coordinates,
                    TraceIO {
                        input_data: trace_item.input.as_ref().map(|input| input.data.clone()),
                        output_data: trace_item.output.as_ref().map(|output| output.data.clone()),
                    },
                );
            }
        }
    }

    pub fn get(&self, coordinates: &CfsCoordinates) -> Option<&TraceIO> {
        self.0.get(coordinates)
    }
}

#[derive(Debug, Clone)]
pub struct TraceRecorder {
    exec_index: u64,
    sequence_callstack: SequenceCallstack,
    cfs_cursor: CfsCursor,
    io_store: TraceIOStore,
}

impl TraceRecorder {
    pub fn new(cfs: ControlFlowSchema) -> Self {
        Self {
            exec_index: 0,
            sequence_callstack: SequenceCallstack::new(),
            cfs_cursor: CfsCursor::new(cfs),
            io_store: TraceIOStore::new(),
        }
    }

    pub fn input_data_at(&self, coordinates: &CfsCoordinates) -> Option<Option<Vec<u8>>> {
        self.io_store
            .get(coordinates)
            .map(|trace_io| trace_io.input_data.clone())
    }

    pub fn output_data_at(&self, coordinates: &CfsCoordinates) -> Option<Option<Vec<u8>>> {
        self.io_store
            .get(coordinates)
            .map(|trace_io| trace_io.output_data.clone())
    }

    pub fn io_data_at(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        self.io_store
            .get(coordinates)
            .map(|trace_io| (trace_io.input_data.clone(), trace_io.output_data.clone()))
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

                let record = SequenceStartRecord {
                    exec_index,
                    sequence_id: fn_call_record.fn_name.clone(),
                    coordinates: coordinates.clone(),
                    input_commitment,
                };

                self.io_store.insert(coordinates, event.clone());

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

                self.io_store.insert(sequence_coordinates, event);

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

                let output = fn_call_record.output;
                let output_commitment = output
                    .as_ref()
                    .map(|output| Sha256Commitment::from(output).into())
                    .unwrap_or_default();

                let record = TileExecRecord {
                    exec_index,
                    tile_id: fn_call_record.fn_name,
                    sequence_id,
                    intra_sequence_index: parent_current_index,
                    coordinates: tile_coordinates.clone(),
                    input_commitment,
                    output_commitment,
                };

                self.io_store.insert(tile_coordinates, event.clone());

                StepRecord::TileExec(record)
            }
        };

        step_record
    }
}
