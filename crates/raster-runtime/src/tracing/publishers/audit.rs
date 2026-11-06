//! Live comparison and a bounded window of execution evidence. Each step keeps
//! only what the recorder releases; the window's proof evidence is built once,
//! at a divergence.
use super::Publisher;
use crate::tracing::proof_input::{
    build_step_io, capture_step_io, empty_storage_frontier, CapturedStepIo, ProofStorageState,
};
use crate::TraceRecorder;
use raster_compiler::{CfsBuilder, Project};
use raster_core::{cfs::CfsCursor, merkle::EMPTY_LEAF, trace::TraceEvent};
use raster_prover::{
    commitment_file::CommitmentFile,
    trace::{BytesHashable, SourceTracker, TraceVerifier},
    trace::SerializableFrontier,
    transition::{AuditHandoff, PreparedFault},
};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const AUDIT_PATH_ENV: &str = "RASTER_AUDIT_PATH";
pub const AUDIT_OUTPUT_ENV: &str = "RASTER_AUDIT_OUTPUT";
pub const AUDIT_BINDING_ENV: &str = "RASTER_AUDIT_BINDING";
pub const AUDIT_PROJECT_ENV: &str = "RASTER_AUDIT_PROJECT";
/// Only accepted with a validated, current-attempt handoff containing evidence.
pub const DIVERGENCE_EXIT_CODE: i32 = 86;

struct WindowEntry {
    captured: CapturedStepIo,
    sources:
        HashMap<(u64, raster_core::trace::StepRecord), raster_core::transition::StepRecordWitness>,
    /// How many storage writes, and which frontier, precede this step: enough
    /// to rebuild the proof state the window opens with if it opens here.
    writes_before: usize,
    frontier_before: Arc<SerializableFrontier>,
    recur: raster_core::recur_progress::RecurProgressStack,
}
struct State {
    recorder: TraceRecorder,
    verifier: TraceVerifier<CommitmentFile>,
    sources: SourceTracker,
    cursor: CfsCursor,
    /// Every storage write so far, as the proof state applies it.
    writes: Vec<(raster_core::transition::StorageEntry, u64)>,
    /// The frontier after the last write.
    frontier: Arc<SerializableFrontier>,
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
        let mut recorder = TraceRecorder::for_live_audit(cfs).reading_program_objects();
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
                writes: Vec::new(),
                frontier: Arc::new(empty_storage_frontier()),
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
        let writes_before = state.writes.len();
        let frontier_before = Arc::clone(&state.frontier);
        let step = state.recorder.record(event);
        let sources = state.sources.witnesses(&step, &state.cursor);
        let captured = capture_step_io(&step, &state.recorder);
        state.recorder.release_live_step(&step);
        if let Some(write) = captured.storage_write() {
            state.frontier = Arc::new(write.frontier_after);
            state.writes.push((write.entry, write.log_position));
        }
        state.window.push_back(WindowEntry {
            captured,
            sources,
            writes_before,
            frontier_before,
            recur,
        });
        if state.window.len() > state.verifier.window_size {
            state.window.pop_front();
        }
        let index = state.steps;
        if std::env::var_os("RASTER_AUDIT_DEBUG").is_some() && (index < 20 || index % 4096 == 0) {
            eprintln!("Audit prepared step {}", index + 1);
        }
        // One trace tree serves both: the source tracker's, which can witness
        // any earlier step, also yields the root the verifier compares.
        let State {
            verifier, sources, ..
        } = &mut *state;
        sources.append_hashed(&step, step.hash());
        let root = sources.tree().root().expect("trace root").0;
        let divergence = verifier.push_root(index, &step, &root, |size| {
            sources.tree().frontier_at(size).expect("window frontier")
        });
        state.steps += 1;
        if let Some(window) = divergence {
            let first = state.window.front().expect("proof window");
            // The proof state as the window opens, then each step's evidence
            // against it, advancing past the step's write: what building it on
            // every step would have produced for these steps.
            let mut storage = ProofStorageState::rebuilt(
                &state.writes[..first.writes_before],
                (*first.frontier_before).clone(),
            );
            let (initial_storage_frontier, initial_index_root, entrypoint_membership_witness) =
                storage.initial(state.cursor.main_entrypoint_names().is_some());
            let io = state
                .window
                .iter()
                .map(|e| build_step_io(&e.captured, &state.recorder, &mut storage))
                .collect();
            let fault = PreparedFault {
                window,
                io,
                input_sources_witnesses: state
                    .window
                    .iter()
                    .flat_map(|e| e.sources.clone())
                    .collect(),
                initial_storage_frontier,
                initial_index_root,
                entrypoint_membership_witness,
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
