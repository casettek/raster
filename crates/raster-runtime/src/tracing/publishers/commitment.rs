//! Process native events directly, retaining only commitment state.
use super::Publisher;
use crate::{TraceRecorder, WorkingStorage};
use raster_compiler::{CfsBuilder, Project};
use raster_core::{merkle::EMPTY_LEAF, trace::TraceEvent};
use raster_prover::{commitment_file::CommitmentBuilder, trace::FraudProofConfig};
use std::{path::PathBuf, sync::Mutex};

pub struct CommitmentPublisher {
    state: Mutex<State>,
    output: PathBuf,
    trace: Option<Box<dyn Publisher>>,
}
struct State {
    recorder: TraceRecorder,
    builder: Option<CommitmentBuilder>,
    started: bool,
    completed: bool,
}
impl CommitmentPublisher {
    pub fn new(
        project: PathBuf,
        output: PathBuf,
        config: FraudProofConfig,
        trace: Option<Box<dyn Publisher>>,
    ) -> raster_core::Result<Self> {
        let project = Project::new(project)?;
        let cfs = CfsBuilder::new(&project).build()?;
        let mut recorder = TraceRecorder::for_commitment_with_storage(cfs, WorkingStorage::Memory);
        crate::storage::THREAD_STORAGE.with(|storage| {
            recorder.set_source_resolver(storage.borrow().source_resolver());
        });
        Ok(Self {
            output,
            trace,
            state: Mutex::new(State {
                recorder,
                builder: Some(CommitmentBuilder::in_memory(&EMPTY_LEAF, config)),
                started: false,
                completed: false,
            }),
        })
    }
}
impl Publisher for CommitmentPublisher {
    fn publish(&self, event: TraceEvent) {
        let mut state = self.state.lock().expect("commitment publisher lock");
        assert!(
            state.builder.is_some() && !state.completed,
            "event after program completion"
        );
        let start = matches!(&event, TraceEvent::ProgramStart(_));
        assert_eq!(start, !state.started, "missing or repeated program start");
        if let Some(trace) = &self.trace {
            trace.publish(event.clone());
        }
        state.started = true;
        state.completed = matches!(&event, TraceEvent::ProgramEnd(_));
        let step = state.recorder.record(event);
        state
            .builder
            .as_mut()
            .unwrap()
            .push(step)
            .expect("update commitment");
    }
    fn finish(&self) {
        let mut state = self.state.lock().expect("commitment publisher lock");
        assert!(
            state.started && state.completed,
            "program did not complete; no commitment published"
        );
        if let Some(trace) = &self.trace {
            trace.finish();
        }
        let commitment = state
            .builder
            .take()
            .expect("commitment already finished")
            .finish()
            .expect("finish commitment");
        // This is a pending artifact. Only the parent CLI publishes it after a successful exit.
        commitment
            .write(&self.output)
            .expect("write pending commitment");
    }
}
