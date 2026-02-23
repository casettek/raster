use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Mutex, RwLock};

use raster_core::trace::{FnCallRecord, StepRecord, TraceEvent};

use crate::tracing::subscriber::Subscriber;

// TODO: should be part of sequence entity
pub type SequenceId = String;
pub struct SequenceCallstack(VecDeque<SequenceExecutionState>);
pub struct SequenceExecutionState {
    id: SequenceId,
    current_index: u64,
}

impl SequenceCallstack {
    fn new() -> Self {
        SequenceCallstack(VecDeque::new())
    }

    fn push(&mut self, state: SequenceExecutionState) {
        self.0.push_back(state);
    }

    fn pop(&mut self) -> Option<SequenceExecutionState> {
        self.0.pop_back()
    }

    fn last(&self) -> Option<&SequenceExecutionState> {
        self.0.iter().last()
    }

    fn last_mut(&mut self) -> Option<&mut SequenceExecutionState> {
        self.0.iter_mut().last()
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

/// A JSON-formatting subscriber that writes to a writer.
pub struct JsonSubscriber<W: Write + Send> {
    writer: Mutex<W>,
    sequence_callstack: RwLock<SequenceCallstack>,
    exec_index: RwLock<u64>,
}

impl<W: Write + Send> JsonSubscriber<W> {
    /// Creates a new JSON subscriber that writes to the given writer.
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
            sequence_callstack: RwLock::new(SequenceCallstack::new()),
            exec_index: RwLock::new(0),
        }
    }
}

impl<W: Write + Send + Sync> Subscriber for JsonSubscriber<W> {
    fn on_trace(&self, event: TraceEvent) {
        match event {
            TraceEvent::SequenceStart(trace_item) => {
                self.sequence_callstack
                    .write()
                    .expect("RwLock poisoned")
                    .push(SequenceExecutionState {
                        id: trace_item.fn_name,
                        current_index: 0,
                    });
            }
            TraceEvent::SequenceEnd(_) => {
                let _ = self.sequence_callstack.write().expect("RwLock poisoned").pop();
            }
            TraceEvent::Tile(trace_item) => {
                let mut stack = self.sequence_callstack.write().expect("RwLock poisoned");
                let last_sequence_state = stack
                    .last_mut()
                    .expect("Tile can't be called without sequence context");
                last_sequence_state.current_index += 1;
                let sequence_id = last_sequence_state.id.clone();
                let intra_sequence_index = last_sequence_state.current_index;
                let sequence_callstack_depth = (stack.len() as u64).saturating_sub(1);
                drop(stack);

                let mut exec_index_guard = self.exec_index.write().expect("RwLock poisoned");
                *exec_index_guard += 1;
                let exec_index = *exec_index_guard;

                let step_record = StepRecord {
                    exec_index,
                    sequence_id,
                    intra_sequence_index,
                    sequence_callstack_depth,
                    fn_call_record: trace_item,
                };

                let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
                let json_str = serde_json::to_string(&step_record).expect("Failed to serialize");
                write!(writer_guard, "[trace]{}\n", json_str).expect("Failed to write");
            }
        }
    }

    fn on_complete(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}
