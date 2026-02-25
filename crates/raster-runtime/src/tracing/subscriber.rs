use raster_compiler::cfs_builder::CfsResolver;
use raster_compiler::{CfsBuilder, Project};
use raster_core::cfs::CfsCoordinates;
use raster_core::trace::TraceEvent;

use std::path::PathBuf;
use std::sync::OnceLock;

// TODO: consider adding linkme here
/// The global subscriber instance.
pub(crate) static GLOBAL_SUBSCRIBER: OnceLock<Box<dyn Subscriber>> = OnceLock::new();

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Mutex, RwLock};

use raster_core::trace::StepRecord;

/// A trait for receiving trace events.
pub trait Subscriber: Send + Sync {
    fn on_trace(&self, event: TraceEvent);

    fn on_complete(&self);
}

pub type SequenceId = String;
pub struct SequenceCallstack {
    callstack: VecDeque<SequenceExecutionState>,
    cfs_resolver: CfsResolver,
    last_sequence_coordinates: CfsCoordinates,
}

pub struct SequenceExecutionState {
    id: SequenceId,
    current_index: u64,
    current_sequence_index: u64,
}

impl SequenceCallstack {
    fn new(cfs_resolver: CfsResolver) -> Self {
        SequenceCallstack {
            callstack: VecDeque::new(),
            last_sequence_coordinates: CfsCoordinates(vec![]),
            cfs_resolver,
        }
    }

    fn push(&mut self, sequence_id: SequenceId) {
        let Some(parent_sequence) = self.last_mut() else {
            self.callstack.push_back(SequenceExecutionState {
                id: sequence_id.clone(),
                current_index: 0,
                current_sequence_index: 0,
            });

            return;
        };

        parent_sequence.current_sequence_index += 1;

        self.last_sequence_coordinates = self
            .cfs_resolver
            .get_coordinates(&self.last_sequence_coordinates, sequence_id.clone());

        self.callstack.push_back(SequenceExecutionState {
            id: sequence_id,
            current_index: 0,
            current_sequence_index: 0,
        });
    }

    fn pop(&mut self) -> Option<SequenceExecutionState> {
        let mut current_sequence_coordinates = self.last_sequence_coordinates.clone();

        let mut coordinates = VecDeque::from(current_sequence_coordinates.0);
        coordinates.pop_back();

        self.last_sequence_coordinates = CfsCoordinates(coordinates.into());
        println!(
            "[debug] current coordinates: {:?}",
            self.last_sequence_coordinates
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
    exec_index: RwLock<u64>,
    sequence_callstack: RwLock<SequenceCallstack>,
    cfs_resolver: RwLock<CfsResolver>,
}

impl<W: Write + Send> ExecutionSubscriber<W> {
    /// Creates a new JSON subscriber that writes to the given writer.
    pub fn new(writer: W) -> Self {
        let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let project = Project::new(project_path).expect("Failed to load project");

        let cfs = CfsBuilder::new(&project)
            .build()
            .expect("Failed to build CFS");
        let cfs_resolver = CfsResolver::new(cfs);

        Self {
            writer: Mutex::new(writer),
            exec_index: RwLock::new(0),
            cfs_resolver: RwLock::new(cfs_resolver.clone()),
            sequence_callstack: RwLock::new(SequenceCallstack::new(cfs_resolver)),
        }
    }
}

impl<W: Write + Send + Sync> Subscriber for ExecutionSubscriber<W> {
    fn on_trace(&self, event: TraceEvent) {
        let mut callstack_guard = self
            .sequence_callstack
            .write()
            .expect("Callstack RwLock poisoned");

        match event {
            TraceEvent::SequenceStart(trace_item) => {
                callstack_guard.push(trace_item.fn_name.clone());
            }
            TraceEvent::SequenceEnd(_) => {
                callstack_guard.pop();
            }
            TraceEvent::Tile(trace_item) => {
                let last_sequence_state = callstack_guard
                    .last_mut()
                    .expect("Tile can't be called without sequence context");
                last_sequence_state.current_index += 1;
                let sequence_id = last_sequence_state.id.clone();
                let intra_sequence_index = last_sequence_state.current_index;
                let sequence_callstack_depth = (callstack_guard.len() as u64).saturating_sub(1);

                let mut exec_index_guard = self.exec_index.write().expect("RwLock poisoned");
                *exec_index_guard += 1;
                let exec_index = *exec_index_guard;

                let step_record = StepRecord {
                    exec_index,
                    sequence_id,
                    intra_sequence_index,
                    sequence_coordinates: callstack_guard.last_sequence_coordinates.clone(),
                    sequence_callstack_depth,
                    fn_call_record: trace_item,
                };

                let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
                let json_str = serde_json::to_string(&step_record).expect("Failed to serialize");
                write!(writer_guard, "[trace]{}\n", json_str).expect("Failed to write");
            }
        }
        drop(callstack_guard);
    }

    fn on_complete(&self) {
        let mut writer_guard = self.writer.lock().expect("Writer mutex poisoned");
        writer_guard.flush().expect("Failed to flush writer");
    }
}
