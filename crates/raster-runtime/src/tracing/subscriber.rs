use raster_compiler::{sequence, CfsBuilder, Project};
use raster_core::cfs::{CfsCoordinates, CfsCursor, SequenceChildId};
use raster_core::trace::{TileExecRecord, TraceEvent};

use std::path::PathBuf;
use std::sync::OnceLock;

// TODO: consider adding linkme here
/// The global subscriber instance.
pub(crate) static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Mutex, RwLock};

use raster_core::trace::{SequenceEndRecord, SequenceStartRecord, StepRecord};

/// A trait for receiving trace events.
pub trait Subscriber: Send + Sync {
    fn on_trace(&self, event: TraceEvent);

    fn on_complete(&self);
}

pub type SequenceId = String;

#[derive(Debug, Clone)]
pub struct SequenceCallstack {
    callstack: VecDeque<SequenceExecutionState>,
    current_sequence_coordinates: CfsCoordinates,
}

#[derive(Debug, Clone)]
pub struct SequenceExecutionState {
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
        } else {
            println!("[debug] push empty {sequence_id}");
        }

        let sequence_execution_state = SequenceExecutionState {
            id: sequence_id,
            current_index: 0,
        };
        self.callstack.push_back(sequence_execution_state);
    }

    fn pop(&mut self) -> Option<SequenceExecutionState> {
        let mut current_sequence_coordinates = self.current_sequence_coordinates.clone();

        let mut parent_sequence_coordinates = VecDeque::from(current_sequence_coordinates.0);

        parent_sequence_coordinates.pop_back();
        self.current_sequence_coordinates = CfsCoordinates(parent_sequence_coordinates.into());

        println!(
            "[debug] current coordinates: {:?}",
            self.current_sequence_coordinates
        );

        self.callstack.pop_back()
    }

    fn last(&self) -> Option<&SequenceExecutionState> {
        self.callstack.iter().last()
    }

    fn last_mut(&mut self) -> Option<&mut SequenceExecutionState> {
        self.callstack.iter_mut().last()
    }

    fn len(&self) -> usize {
        self.callstack.len()
    }
}

pub struct ExecutionSubscriber<W: Write + Send> {
    writer: Mutex<W>,
    exec_index: Mutex<u64>,
    sequence_callstack: Mutex<SequenceCallstack>,
    cfs_cursor: Mutex<CfsCursor>,
}

impl<W: Write + Send> ExecutionSubscriber<W> {
    /// Creates a new JSON subscriber that writes to the given writer.
    pub fn new(writer: W) -> Self {
        let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let project = Project::new(project_path).expect("Failed to load project");

        let cfs = CfsBuilder::new(&project)
            .build()
            .expect("Failed to build CFS");
        let cfs_cursor = CfsCursor::new(cfs);

        Self {
            writer: Mutex::new(writer),
            exec_index: Mutex::new(0),
            sequence_callstack: Mutex::new(SequenceCallstack::new()),
            cfs_cursor: Mutex::new(cfs_cursor),
        }
    }

    fn write_record(&self, record: &StepRecord) {
        let json_str = serde_json::to_string(record).expect("Failed to serialize");
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writeln!(writer_guard, "[trace]{}", json_str).expect("Failed to write");
    }
}

impl<W: Write + Send + Sync> Subscriber for ExecutionSubscriber<W> {
    fn on_trace(&self, event: TraceEvent) {
        println!("[debug] on_trace ------------------------------------ ",);
        println!("[debug] {:?}", event);

        let mut exec_index_guard = self.exec_index.lock().expect("Mutex poisoned");
        *exec_index_guard += 1;
        let exec_index = *exec_index_guard;
        drop(exec_index_guard);

        match event {
            TraceEvent::SequenceStart(trace_item) => {
                println!("[debug] TraceEvent::SequenceStart ---------------------- ");
                let mut callstack_guard = self
                    .sequence_callstack
                    .lock()
                    .expect("Callstack Mutex poisoned");

                let cfs_cursor_guard = self.cfs_cursor.lock().unwrap();
                callstack_guard.push(trace_item.fn_name.clone(), &cfs_cursor_guard);
                drop(cfs_cursor_guard);

                let sequence_coordinates = callstack_guard.current_sequence_coordinates.clone();
                let current_sequence_state = callstack_guard
                    .last_mut()
                    .expect("Tile can't be called without sequence context");
                let sequence_id = current_sequence_state.id.clone();
                println!(
                    "[debug] TraceEvent::SequenceStart parent sequence {}[{:?}]",
                    sequence_id, sequence_coordinates
                );
                let sequence_start_record = StepRecord::SequenceStart(SequenceStartRecord {
                    exec_index,
                    sequence_id: trace_item.fn_name.clone(),
                    sequence_coordinates: callstack_guard.current_sequence_coordinates.clone(),
                    inputs: trace_item.inputs,
                    input_data: trace_item.input_data,
                });

                drop(callstack_guard);

                let mut callstack_guard = self
                    .sequence_callstack
                    .lock()
                    .expect("Callstack Mutex poisoned");
                if let Some(current_sequence_state) = callstack_guard.last() {
                    println!(
                        "[debug] TraceEvent::SequenceStart current new state : {:?}",
                        current_sequence_state
                    );
                } else {
                    println!("[debug] TraceEvent::SequenceStart no parent");
                }
                self.write_record(&sequence_start_record);
            }
            TraceEvent::SequenceEnd(trace_item) => {
                println!("[debug] TraceEvent::SequenceEnd");
                let mut callstack_guard = self
                    .sequence_callstack
                    .lock()
                    .expect("Callstack Mutex poisoned");

                let sequence_coordinates = callstack_guard.current_sequence_coordinates.clone();

                let sequence_end_record = StepRecord::SequenceEnd(SequenceEndRecord {
                    exec_index,
                    sequence_coordinates,
                    sequence_id: trace_item.fn_name.clone(),
                    output_type: trace_item.output_type,
                    output_data: trace_item.output_data,
                });

                callstack_guard.pop().expect("Corrupted sequence stack");

                drop(callstack_guard);
                self.write_record(&sequence_end_record);
            }
            TraceEvent::TileExec(trace_item) => {
                println!("[debug] TraceEvent::TileExec");
                let mut callstack_guard = self
                    .sequence_callstack
                    .lock()
                    .expect("Callstack Mutex poisoned");

                let sequence_coordinates = callstack_guard.current_sequence_coordinates.clone();
                let current_sequence_state = callstack_guard
                    .last_mut()
                    .expect("Tile can't be called without sequence context");

                let sequence_id = current_sequence_state.id.clone();
                println!(
                    "[debug] TraceEvent::TileExec parent sequence {}[{:?}]",
                    sequence_id, sequence_coordinates
                );

                let parent_current_index = current_sequence_state.current_index;

                println!(
                    "[debug] TraceEvent::TileExec parent current index: {}",
                    parent_current_index
                );
                let cfs_cursor_guard = self.cfs_cursor.lock().unwrap();
                let tile_coordinates = cfs_cursor_guard.get_child_coordinates(
                    &sequence_coordinates,
                    parent_current_index,
                    SequenceChildId::Tile(trace_item.fn_name.clone()),
                );

                current_sequence_state.current_index += 1;

                println!(
                    "[debug] TraceEvent::TileExec parent new index: {}",
                    current_sequence_state.current_index
                );

                drop(callstack_guard);

                let step_record = StepRecord::TileExec(TileExecRecord {
                    exec_index,
                    sequence_id,
                    intra_sequence_index: parent_current_index,
                    coordinates: tile_coordinates,
                    fn_call_record: trace_item,
                });

                self.write_record(&step_record);
            }
        }
    }

    fn on_complete(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}
