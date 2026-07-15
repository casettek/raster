use raster_core::cfs::CfsCoordinates;
use serde::{Deserialize, Serialize};

#[cfg(feature = "profiling")]
use std::cell::RefCell;
#[cfg(feature = "profiling")]
use std::fs;
#[cfg(feature = "profiling")]
use std::io::{BufWriter, Write};
use std::path::PathBuf;
#[cfg(feature = "profiling")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "profiling")]
use std::sync::mpsc::{self, SyncSender};
#[cfg(feature = "profiling")]
use std::thread::JoinHandle;
#[cfg(feature = "profiling")]
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROFILE_PATH_ENV: &str = "RASTER_PROFILE_PATH";
pub const PROFILE_STREAM_PATH_ENV: &str = "RASTER_PROFILE_STREAM_PATH";
pub const PROFILE_RUN_ID_ENV: &str = "RASTER_PROFILE_RUN_ID";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionProfile {
    pub version: u32,
    #[serde(default)]
    pub run_id: Option<String>,
    pub program_total_duration_ns: Option<u64>,
    pub records: Vec<ProfileRecord>,
}

impl ExecutionProfile {
    pub fn new(
        records: Vec<ProfileRecord>,
        program_total_duration_ns: Option<u64>,
        run_id: Option<String>,
    ) -> Self {
        Self {
            version: 3,
            run_id,
            program_total_duration_ns,
            records,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProfileRecord {
    Sequence(SequenceProfileRecord),
    Tile(TileProfileRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceProfileRecord {
    pub invocation_index: u64,
    pub sequence_id: String,
    pub depth: u32,
    pub total_duration_ns: u64,
    pub self_duration_ns: u64,
    pub child_duration_ns: u64,
    #[serde(default)]
    pub self_breakdown: SequenceProfileSelfBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileProfileRecord {
    pub invocation_index: u64,
    pub tile_id: String,
    pub depth: u32,
    pub coordinates: CfsCoordinates,
    pub total_duration_ns: u64,
    pub user_duration_ns: u64,
    pub raster_overhead_ns: u64,
    #[serde(default)]
    pub external_input_resolve_ns: u64,
    #[serde(default)]
    pub storage_input_resolve_ns: u64,
    #[serde(default)]
    pub output_store_ns: u64,
    #[serde(default)]
    pub trace_serialize_ns: u64,
    #[serde(default)]
    pub draft_capture_ns: u64,
    #[serde(default)]
    pub scope_enter_ns: u64,
    #[serde(default)]
    pub output_record_build_ns: u64,
    #[serde(default)]
    pub trace_event_publish_ns: u64,
    #[serde(default)]
    pub output_coordinate_publish_ns: u64,
    #[serde(default)]
    pub other_wrapper_ns: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SequenceProfileSelfBreakdown {
    #[serde(default)]
    pub body_self_ns: u64,
    #[serde(default)]
    pub scope_enter_ns: u64,
    #[serde(default)]
    pub synthetic_coordinate_alloc_ns: u64,
    #[serde(default)]
    pub input_trace_ns: u64,
    #[serde(default)]
    pub start_event_publish_ns: u64,
    #[serde(default)]
    pub output_trace_ns: u64,
    #[serde(default)]
    pub end_event_publish_ns: u64,
    #[serde(default)]
    pub other_wrapper_ns: u64,
}

impl SequenceProfileSelfBreakdown {
    #[cfg(feature = "profiling")]
    fn measured_non_body_ns(&self) -> u64 {
        self.scope_enter_ns
            .saturating_add(self.synthetic_coordinate_alloc_ns)
            .saturating_add(self.input_trace_ns)
            .saturating_add(self.start_event_publish_ns)
            .saturating_add(self.output_trace_ns)
            .saturating_add(self.end_event_publish_ns)
            .saturating_add(self.other_wrapper_ns)
    }

    #[cfg(feature = "profiling")]
    fn normalized_for_self(mut self, self_duration_ns: u64) -> Self {
        self.body_self_ns = self
            .body_self_ns
            .saturating_add(self_duration_ns.saturating_sub(self.measured_non_body_ns()));
        self
    }

    #[cfg(test)]
    fn total_self_ns(&self) -> u64 {
        self.body_self_ns
            .saturating_add(self.scope_enter_ns)
            .saturating_add(self.synthetic_coordinate_alloc_ns)
            .saturating_add(self.input_trace_ns)
            .saturating_add(self.start_event_publish_ns)
            .saturating_add(self.output_trace_ns)
            .saturating_add(self.end_event_publish_ns)
            .saturating_add(self.other_wrapper_ns)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TileProfileOverheadBreakdown {
    pub external_input_resolve_ns: u64,
    pub storage_input_resolve_ns: u64,
    pub output_store_ns: u64,
    pub trace_serialize_ns: u64,
    pub draft_capture_ns: u64,
    pub scope_enter_ns: u64,
    pub output_record_build_ns: u64,
    pub trace_event_publish_ns: u64,
    pub output_coordinate_publish_ns: u64,
    pub other_wrapper_ns: u64,
}

impl TileProfileOverheadBreakdown {
    #[cfg(feature = "profiling")]
    fn measured_total_ns(&self) -> u64 {
        self.external_input_resolve_ns
            .saturating_add(self.storage_input_resolve_ns)
            .saturating_add(self.output_store_ns)
            .saturating_add(self.trace_serialize_ns)
            .saturating_add(self.draft_capture_ns)
            .saturating_add(self.scope_enter_ns)
            .saturating_add(self.output_record_build_ns)
            .saturating_add(self.trace_event_publish_ns)
            .saturating_add(self.output_coordinate_publish_ns)
            .saturating_add(self.other_wrapper_ns)
    }

    #[cfg(feature = "profiling")]
    fn normalized_for_total(mut self, total_duration_ns: u64, user_duration_ns: u64) -> Self {
        let expected_overhead_ns = total_duration_ns.saturating_sub(user_duration_ns);
        let measured_overhead_ns = self.measured_total_ns();
        self.other_wrapper_ns = self
            .other_wrapper_ns
            .saturating_add(expected_overhead_ns.saturating_sub(measured_overhead_ns));
        self
    }

    #[cfg(feature = "profiling")]
    fn total_overhead_ns(&self) -> u64 {
        self.measured_total_ns()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProfileStreamEvent {
    RunStarted {
        run_id: String,
    },
    Record(ProfileRecord),
    TileOutputStore {
        invocation_index: u64,
        output_store_ns: u64,
    },
    RunFinished {
        run_id: String,
        program_total_duration_ns: Option<u64>,
    },
}

#[cfg(feature = "profiling")]
enum ProfileStreamMessage {
    Event(ProfileStreamEvent),
    Shutdown,
}

#[cfg(feature = "profiling")]
struct ProfileStreamHandle {
    sender: SyncSender<ProfileStreamMessage>,
    join_handle: JoinHandle<std::io::Result<()>>,
}

#[cfg(feature = "profiling")]
struct ProfilerState {
    output_path: Option<PathBuf>,
    stream: Option<ProfileStreamHandle>,
    run_id: Option<String>,
    next_invocation_index: u64,
    active_sequences: Vec<ActiveSequenceFrame>,
    records: Vec<ProfileRecord>,
    program_total_duration_ns: Option<u64>,
}

#[cfg(feature = "profiling")]
#[derive(Debug)]
struct ActiveSequenceFrame {
    sequence_id: String,
    depth: u32,
    child_duration_ns: u64,
    self_breakdown: SequenceProfileSelfBreakdown,
}

#[cfg(feature = "profiling")]
impl Default for ProfilerState {
    fn default() -> Self {
        Self {
            output_path: None,
            stream: None,
            run_id: None,
            next_invocation_index: 0,
            active_sequences: Vec::new(),
            records: Vec::new(),
            program_total_duration_ns: None,
        }
    }
}

#[cfg(feature = "profiling")]
impl ProfilerState {
    fn reset(
        &mut self,
        output_path: Option<PathBuf>,
        stream_path: Option<PathBuf>,
        run_id: Option<String>,
    ) -> std::io::Result<()> {
        self.shutdown_stream()?;
        let profiling_enabled = output_path.is_some() || stream_path.is_some();
        self.output_path = output_path;
        self.run_id = profiling_enabled.then(|| run_id.unwrap_or_else(generate_run_id));
        self.next_invocation_index = 0;
        self.active_sequences.clear();
        self.records.clear();
        self.program_total_duration_ns = None;

        if let Some(path) = stream_path {
            let stream = spawn_stream_writer(path)?;
            self.stream = Some(stream);
            if let Some(run_id) = self.run_id.clone() {
                self.send_stream_event(ProfileStreamEvent::RunStarted { run_id })?;
            }
        }

        Ok(())
    }

    fn enabled(&self) -> bool {
        self.output_path.is_some() || self.stream.is_some()
    }

    fn next_invocation_index(&mut self) -> u64 {
        self.next_invocation_index += 1;
        self.next_invocation_index
    }

    fn send_stream_event(&self, event: ProfileStreamEvent) -> std::io::Result<()> {
        let Some(stream) = &self.stream else {
            return Ok(());
        };
        stream
            .sender
            .send(ProfileStreamMessage::Event(event))
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!("Failed to send profile stream event: {}", error),
                )
            })
    }

    fn shutdown_stream(&mut self) -> std::io::Result<()> {
        let Some(stream) = self.stream.take() else {
            return Ok(());
        };
        let sender = stream.sender;
        let join_handle = stream.join_handle;
        sender
            .send(ProfileStreamMessage::Shutdown)
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    format!("Failed to shut down profile stream writer: {}", error),
                )
            })?;
        match join_handle.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other(
                "Profile stream writer thread panicked",
            )),
        }
    }
}

#[cfg(feature = "profiling")]
thread_local! {
    static PROFILER_STATE: RefCell<ProfilerState> = RefCell::new(ProfilerState::default());
}

#[cfg(feature = "profiling")]
pub(crate) fn profiling_enabled() -> bool {
    PROFILER_STATE.with(|state| state.borrow().enabled())
}

#[cfg(not(feature = "profiling"))]
pub(crate) fn profiling_enabled() -> bool {
    false
}

#[cfg(feature = "profiling")]
pub fn init_from_env() {
    let output_path = std::env::var_os(PROFILE_PATH_ENV).map(PathBuf::from);
    let stream_path = std::env::var_os(PROFILE_STREAM_PATH_ENV).map(PathBuf::from);
    let run_id = std::env::var(PROFILE_RUN_ID_ENV).ok();
    PROFILER_STATE.with(|state| {
        state
            .borrow_mut()
            .reset(output_path, stream_path, run_id)
            .unwrap_or_else(|error| panic!("Failed to initialize profiler state: {}", error));
    });
}

#[cfg(not(feature = "profiling"))]
pub fn init_from_env() {}

#[cfg(feature = "profiling")]
pub fn begin_sequence_profile(sequence_id: &str) {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.enabled() {
            return;
        }
        let depth = state.active_sequences.len() as u32;
        state.active_sequences.push(ActiveSequenceFrame {
            sequence_id: sequence_id.to_string(),
            depth,
            child_duration_ns: 0,
            self_breakdown: SequenceProfileSelfBreakdown::default(),
        });
    });
}

#[cfg(not(feature = "profiling"))]
pub fn begin_sequence_profile(_: &str) {}

#[cfg(feature = "profiling")]
pub(crate) fn record_sequence_synthetic_coordinate_alloc(duration_ns: u64) {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.enabled() || duration_ns == 0 {
            return;
        }

        if let Some(frame) = state.active_sequences.last_mut() {
            frame.self_breakdown.synthetic_coordinate_alloc_ns = frame
                .self_breakdown
                .synthetic_coordinate_alloc_ns
                .saturating_add(duration_ns);
        }
    });
}

#[cfg(not(feature = "profiling"))]
pub(crate) fn record_sequence_synthetic_coordinate_alloc(_: u64) {}

#[cfg(feature = "profiling")]
pub fn finish_sequence_profile(
    sequence_id: &str,
    total_duration_ns: u64,
    self_breakdown: SequenceProfileSelfBreakdown,
) {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.enabled() {
            return;
        }

        let frame = state.active_sequences.pop().unwrap_or_else(|| {
            panic!(
                "Missing active sequence profile frame while finishing '{}'",
                sequence_id
            )
        });
        assert_eq!(
            frame.sequence_id, sequence_id,
            "Mismatched sequence profile frame teardown"
        );

        let self_duration_ns = total_duration_ns.saturating_sub(frame.child_duration_ns);
        let self_breakdown = SequenceProfileSelfBreakdown {
            synthetic_coordinate_alloc_ns: frame.self_breakdown.synthetic_coordinate_alloc_ns,
            ..self_breakdown
        }
        .normalized_for_self(self_duration_ns);
        let invocation_index = state.next_invocation_index();

        if let Some(parent) = state.active_sequences.last_mut() {
            parent.child_duration_ns = parent.child_duration_ns.saturating_add(total_duration_ns);
        } else {
            state.program_total_duration_ns = Some(total_duration_ns);
        }

        let record = ProfileRecord::Sequence(SequenceProfileRecord {
            invocation_index,
            sequence_id: frame.sequence_id,
            depth: frame.depth,
            total_duration_ns,
            self_duration_ns,
            child_duration_ns: frame.child_duration_ns,
            self_breakdown,
        });
        state.records.push(record.clone());
        state
            .send_stream_event(ProfileStreamEvent::Record(record))
            .unwrap_or_else(|error| panic!("Failed to stream sequence profile: {}", error));
    });
}

#[cfg(not(feature = "profiling"))]
pub fn finish_sequence_profile(_: &str, _: u64, _: SequenceProfileSelfBreakdown) {}

#[cfg(feature = "profiling")]
pub fn record_tile_profile(
    tile_id: &str,
    coordinates: CfsCoordinates,
    total_duration_ns: u64,
    user_duration_ns: u64,
    overhead_breakdown: TileProfileOverheadBreakdown,
) {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.enabled() {
            return;
        }

        let invocation_index = state.next_invocation_index();
        let depth = state.active_sequences.len() as u32;
        let overhead_breakdown =
            overhead_breakdown.normalized_for_total(total_duration_ns, user_duration_ns);
        let raster_overhead_ns = overhead_breakdown.total_overhead_ns();

        if let Some(parent) = state.active_sequences.last_mut() {
            parent.child_duration_ns = parent.child_duration_ns.saturating_add(total_duration_ns);
        }

        let record = ProfileRecord::Tile(TileProfileRecord {
            invocation_index,
            tile_id: tile_id.to_string(),
            depth,
            coordinates,
            total_duration_ns,
            user_duration_ns,
            raster_overhead_ns,
            external_input_resolve_ns: overhead_breakdown.external_input_resolve_ns,
            storage_input_resolve_ns: overhead_breakdown.storage_input_resolve_ns,
            output_store_ns: overhead_breakdown.output_store_ns,
            trace_serialize_ns: overhead_breakdown.trace_serialize_ns,
            draft_capture_ns: overhead_breakdown.draft_capture_ns,
            scope_enter_ns: overhead_breakdown.scope_enter_ns,
            output_record_build_ns: overhead_breakdown.output_record_build_ns,
            trace_event_publish_ns: overhead_breakdown.trace_event_publish_ns,
            output_coordinate_publish_ns: overhead_breakdown.output_coordinate_publish_ns,
            other_wrapper_ns: overhead_breakdown.other_wrapper_ns,
        });
        state.records.push(record.clone());
        state
            .send_stream_event(ProfileStreamEvent::Record(record))
            .unwrap_or_else(|error| panic!("Failed to stream tile profile: {}", error));
    });
}

#[cfg(not(feature = "profiling"))]
pub fn record_tile_profile(
    _: &str,
    _: CfsCoordinates,
    _: u64,
    _: u64,
    _: TileProfileOverheadBreakdown,
) {
}

#[cfg(feature = "profiling")]
pub fn record_tile_output_store_profile(output_store_ns: u64) {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.enabled() || output_store_ns == 0 {
            return;
        }

        let invocation_index = {
            let Some(ProfileRecord::Tile(record)) = state.records.last_mut() else {
                panic!("Missing tile profile record while recording output-store overhead");
            };
            record.total_duration_ns = record.total_duration_ns.saturating_add(output_store_ns);
            record.raster_overhead_ns = record.raster_overhead_ns.saturating_add(output_store_ns);
            record.output_store_ns = record.output_store_ns.saturating_add(output_store_ns);
            record.invocation_index
        };

        if let Some(parent) = state.active_sequences.last_mut() {
            parent.child_duration_ns = parent.child_duration_ns.saturating_add(output_store_ns);
        }

        state
            .send_stream_event(ProfileStreamEvent::TileOutputStore {
                invocation_index,
                output_store_ns,
            })
            .unwrap_or_else(|error| {
                panic!("Failed to stream tile output-store overhead: {}", error)
            });
    });
}

#[cfg(not(feature = "profiling"))]
pub fn record_tile_output_store_profile(_: u64) {}

#[cfg(feature = "profiling")]
pub fn finish() -> std::io::Result<Option<PathBuf>> {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();

        if let Some(run_id) = state.run_id.clone() {
            state.send_stream_event(ProfileStreamEvent::RunFinished {
                run_id,
                program_total_duration_ns: state.program_total_duration_ns,
            })?;
        }
        state.shutdown_stream()?;

        let Some(path) = state.output_path.clone() else {
            return Ok(None);
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let profile = ExecutionProfile::new(
            state.records.clone(),
            state.program_total_duration_ns,
            state.run_id.clone(),
        );
        let bytes = serde_json::to_vec_pretty(&profile).map_err(std::io::Error::other)?;
        fs::write(&path, bytes)?;

        Ok(Some(path))
    })
}

#[cfg(not(feature = "profiling"))]
pub fn finish() -> std::io::Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(feature = "profiling")]
fn generate_run_id() -> String {
    static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{}", std::process::id(), timestamp_ns, sequence)
}

#[cfg(feature = "profiling")]
fn spawn_stream_writer(path: PathBuf) -> std::io::Result<ProfileStreamHandle> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let (sender, receiver) = mpsc::sync_channel(4096);
    let join_handle = std::thread::spawn(move || -> std::io::Result<()> {
        let file = fs::File::create(&path)?;
        let mut writer = BufWriter::new(file);
        while let Ok(message) = receiver.recv() {
            match message {
                ProfileStreamMessage::Event(event) => {
                    serde_json::to_writer(&mut writer, &event).map_err(std::io::Error::other)?;
                    writer.write_all(b"\n")?;
                    writer.flush()?;
                }
                ProfileStreamMessage::Shutdown => break,
            }
        }
        writer.flush()?;
        Ok(())
    });

    Ok(ProfileStreamHandle {
        sender,
        join_handle,
    })
}

#[cfg(all(test, feature = "profiling"))]
mod tests {
    use super::*;
    use crate::storage::{enter_sequence_scope, exit_sequence_scope, store_value};

    fn reset_profiler() {
        PROFILER_STATE.with(|state| {
            state
                .borrow_mut()
                .reset(Some(PathBuf::from("test-profile.json")), None, None)
                .unwrap();
        });
    }

    #[test]
    fn sequence_self_time_excludes_nested_tiles_and_sequences() {
        reset_profiler();

        begin_sequence_profile("main");
        record_tile_profile(
            "tile_a",
            CfsCoordinates(vec![0]),
            30,
            20,
            TileProfileOverheadBreakdown::default(),
        );
        begin_sequence_profile("nested");
        record_tile_profile(
            "tile_b",
            CfsCoordinates(vec![1, 0]),
            10,
            7,
            TileProfileOverheadBreakdown::default(),
        );
        finish_sequence_profile("nested", 25, SequenceProfileSelfBreakdown::default());
        finish_sequence_profile("main", 100, SequenceProfileSelfBreakdown::default());

        let profile = PROFILER_STATE.with(|state| {
            let state = state.borrow();
            ExecutionProfile::new(
                state.records.clone(),
                state.program_total_duration_ns,
                state.run_id.clone(),
            )
        });

        let mut main_self_time = None;
        let mut nested_self_time = None;
        for record in profile.records {
            match record {
                ProfileRecord::Sequence(record) if record.sequence_id == "main" => {
                    main_self_time = Some(record.self_duration_ns);
                }
                ProfileRecord::Sequence(record) if record.sequence_id == "nested" => {
                    nested_self_time = Some(record.self_duration_ns);
                }
                _ => {}
            }
        }

        assert_eq!(nested_self_time, Some(15));
        assert_eq!(main_self_time, Some(45));
        assert_eq!(profile.program_total_duration_ns, Some(100));
    }

    #[test]
    fn output_store_overhead_updates_latest_tile_record() {
        reset_profiler();

        begin_sequence_profile("main");
        record_tile_profile(
            "tile_a",
            CfsCoordinates(vec![0]),
            30,
            20,
            TileProfileOverheadBreakdown {
                external_input_resolve_ns: 4,
                trace_serialize_ns: 2,
                draft_capture_ns: 1,
                ..TileProfileOverheadBreakdown::default()
            },
        );
        record_tile_output_store_profile(7);
        finish_sequence_profile("main", 50, SequenceProfileSelfBreakdown::default());

        let profile = PROFILER_STATE.with(|state| {
            let state = state.borrow();
            ExecutionProfile::new(
                state.records.clone(),
                state.program_total_duration_ns,
                state.run_id.clone(),
            )
        });

        let mut tile_record = None;
        let mut sequence_self_time = None;
        for record in profile.records {
            match record {
                ProfileRecord::Tile(record) => tile_record = Some(record),
                ProfileRecord::Sequence(record) => {
                    sequence_self_time = Some(record.self_duration_ns)
                }
            }
        }

        let tile_record = tile_record.expect("missing tile record");
        assert_eq!(tile_record.total_duration_ns, 37);
        assert_eq!(tile_record.raster_overhead_ns, 17);
        assert_eq!(tile_record.output_store_ns, 7);
        assert_eq!(tile_record.external_input_resolve_ns, 4);
        assert_eq!(tile_record.trace_serialize_ns, 2);
        assert_eq!(tile_record.draft_capture_ns, 1);
        assert_eq!(tile_record.other_wrapper_ns, 3);
        assert_eq!(sequence_self_time, Some(13));
    }

    #[test]
    fn sequence_breakdown_includes_synthetic_coordinate_allocation() {
        reset_profiler();

        enter_sequence_scope("main");
        begin_sequence_profile("main");
        let _ = store_value(&123u64).expect("store should allocate synthetic coordinates");
        finish_sequence_profile(
            "main",
            1_000_000,
            SequenceProfileSelfBreakdown {
                scope_enter_ns: 5,
                input_trace_ns: 7,
                start_event_publish_ns: 3,
                output_trace_ns: 2,
                end_event_publish_ns: 1,
                ..SequenceProfileSelfBreakdown::default()
            },
        );
        exit_sequence_scope();

        let profile = PROFILER_STATE.with(|state| {
            let state = state.borrow();
            ExecutionProfile::new(
                state.records.clone(),
                state.program_total_duration_ns,
                state.run_id.clone(),
            )
        });

        let record = profile
            .records
            .into_iter()
            .find_map(|record| match record {
                ProfileRecord::Sequence(record) if record.sequence_id == "main" => Some(record),
                _ => None,
            })
            .expect("missing sequence record");

        assert!(record.self_breakdown.synthetic_coordinate_alloc_ns > 0);
        assert_eq!(
            record.self_breakdown.total_self_ns(),
            record.self_duration_ns
        );
        assert_eq!(record.self_duration_ns, 1_000_000);
    }

    #[test]
    fn older_sequence_records_deserialize_with_default_breakdown() {
        let bytes = br#"{
          "version": 2,
          "program_total_duration_ns": 40,
          "records": [
            {
              "Sequence": {
                "invocation_index": 1,
                "sequence_id": "main",
                "depth": 0,
                "total_duration_ns": 40,
                "self_duration_ns": 10,
                "child_duration_ns": 30
              }
            }
          ]
        }"#;

        let profile: ExecutionProfile =
            serde_json::from_slice(bytes).expect("profile should parse");
        let record = match &profile.records[0] {
            ProfileRecord::Sequence(record) => record,
            _ => panic!("expected sequence record"),
        };

        assert_eq!(record.self_breakdown.total_self_ns(), 0);
        assert_eq!(record.self_breakdown.synthetic_coordinate_alloc_ns, 0);
    }
}
