//! Live comparison and a bounded window of proof-ready execution evidence.
use super::Publisher;
use crate::tracing::proof_input::{prepare_step_io, ProofStorageState};
use crate::TraceRecorder;
use raster_compiler::{CfsBuilder, Project};
use raster_core::{cfs::CfsCursor, merkle::EMPTY_LEAF, trace::TraceEvent};
use raster_prover::{
    commitment_file::CommitmentFile,
    trace::{SourceTracker, TraceVerifier},
    transition::{AuditHandoff, PreparedFault, StepIo},
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Mutex,
};

pub const AUDIT_PATH_ENV: &str = "RASTER_AUDIT_PATH";
pub const AUDIT_OUTPUT_ENV: &str = "RASTER_AUDIT_OUTPUT";
pub const AUDIT_BINDING_ENV: &str = "RASTER_AUDIT_BINDING";
pub const AUDIT_PROJECT_ENV: &str = "RASTER_AUDIT_PROJECT";
/// Only accepted with a validated, current-attempt handoff containing evidence.
pub const DIVERGENCE_EXIT_CODE: i32 = 86;

struct WindowEntry {
    io: StepIo,
    sources:
        HashMap<(u64, raster_core::trace::StepRecord), raster_core::transition::StepRecordWitness>,
    storage_frontier: raster_prover::trace::SerializableFrontier,
    index_root: Vec<u8>,
    entrypoint: Option<raster_core::transition::StorageReadWitness>,
    recur: raster_core::recur_progress::RecurProgressStack,
}
struct State {
    recorder: TraceRecorder,
    verifier: TraceVerifier<CommitmentFile>,
    sources: SourceTracker,
    cursor: CfsCursor,
    storage: ProofStorageState,
    window: VecDeque<WindowEntry>,
    steps: usize,
    completed: bool,
    finished: bool,
}
pub struct AuditPublisher {
    state: Mutex<State>,
    output: PathBuf,
    binding: String,
}
impl AuditPublisher {
    pub fn new(
        project: PathBuf,
        disputed: PathBuf,
        output: PathBuf,
        binding: String,
    ) -> raster_core::Result<Self> {
        let cfs = CfsBuilder::new(&Project::new(project)?).build()?;
        let disputed = CommitmentFile::read_memory(&disputed).map_err(raster_core::Error::Io)?;
        let verifier = TraceVerifier::new(disputed, &EMPTY_LEAF, &cfs)
            .map_err(|e| raster_core::Error::Other(e.to_string()))?;
        let cursor = CfsCursor::new(cfs.clone());
        let mut recorder = TraceRecorder::for_live_audit(cfs);
        crate::storage::THREAD_STORAGE
            .with(|storage| recorder.set_source_resolver(storage.borrow().source_resolver()));
        Ok(Self {
            output,
            binding,
            state: Mutex::new(State {
                recorder,
                verifier,
                cursor,
                sources: SourceTracker::new(),
                storage: ProofStorageState::new(),
                window: VecDeque::new(),
                steps: 0,
                completed: false,
                finished: false,
            }),
        })
    }
    fn write(&self, state: &State, fault: Option<PreparedFault>) {
        let handoff = AuditHandoff {
            version: 1,
            binding: self.binding.clone(),
            commitment_digest: state.verifier.trace_commitment.header().digest().to_vec(),
            steps: state.steps,
            fault,
        };
        let bytes = raster_core::bincode::serialize(&handoff).expect("encode audit evidence");
        std::fs::write(&self.output, bytes).expect("write pending audit evidence");
    }
}
impl Publisher for AuditPublisher {
    fn publish(&self, event: TraceEvent) {
        let mut state = self.state.lock().expect("audit lock");
        assert!(
            !state.completed && !state.finished,
            "event after completion"
        );
        assert_eq!(
            matches!(&event, TraceEvent::ProgramStart(_)),
            state.steps == 0,
            "missing or repeated program start"
        );
        assert!(
            state.steps < state.verifier.trace_commitment.len(),
            "execution exceeds commitment length"
        );
        state.completed = matches!(&event, TraceEvent::ProgramEnd(_));
        let recur = state.recorder.live_recur_progress();
        let (storage_frontier, index_root, entrypoint) = state
            .storage
            .initial(state.cursor.main_entrypoint_names().is_some());
        let step = state.recorder.record(event);
        let sources = state.sources.witnesses(&step, &state.cursor);
        let State {
            recorder, storage, ..
        } = &mut *state;
        let io = prepare_step_io(&step, recorder, storage);
        recorder.release_live_step(&step);
        state.window.push_back(WindowEntry {
            io,
            sources,
            storage_frontier,
            index_root,
            entrypoint,
            recur,
        });
        if state.window.len() > state.verifier.window_size {
            state.window.pop_front();
        }
        let index = state.steps;
        if std::env::var_os("RASTER_AUDIT_DEBUG").is_some() && (index < 20 || index % 4096 == 0) {
            eprintln!("Audit prepared step {}", index + 1);
        }
        let divergence = state.verifier.push(index, &step);
        state.sources.append(&step);
        state.steps += 1;
        if let Some(window) = divergence {
            let first = state.window.front().expect("proof window");
            let fault = PreparedFault {
                window,
                io: state.window.iter().map(|e| e.io.clone()).collect(),
                input_sources_witnesses: state
                    .window
                    .iter()
                    .flat_map(|e| e.sources.clone())
                    .collect(),
                initial_storage_frontier: first.storage_frontier.clone(),
                initial_index_root: first.index_root.clone(),
                entrypoint_membership_witness: first.entrypoint.clone(),
                window_start_recur_progress: (state.steps > state.verifier.window_size)
                    .then(|| first.recur.clone()),
            };
            self.write(&state, Some(fault));
            // Never return to the next tile. The parent owns cleanup and proof
            // generation; a non-success exit alone is insufficient authorization.
            std::process::exit(DIVERGENCE_EXIT_CODE);
        }
    }
    fn finish(&self) {
        let mut state = self.state.lock().expect("audit lock");
        assert!(
            state.completed && !state.finished,
            "program did not complete"
        );
        assert_eq!(
            state.steps,
            state.verifier.trace_commitment.len(),
            "execution and commitment lengths differ"
        );
        state.finished = true;
        self.write(&state, None);
    }
}
