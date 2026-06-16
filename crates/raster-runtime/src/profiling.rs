use raster_core::cfs::CfsCoordinates;
use serde::{Deserialize, Serialize};

use std::cell::RefCell;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PROFILE_PATH_ENV: &str = "RASTER_PROFILE_PATH";
pub const PROFILE_STREAM_PATH_ENV: &str = "RASTER_PROFILE_STREAM_PATH";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionProfile {
    pub version: u32,
    pub program_total_duration_ns: Option<u64>,
    pub records: Vec<ProfileRecord>,
}

impl ExecutionProfile {
    pub fn new(records: Vec<ProfileRecord>, program_total_duration_ns: Option<u64>) -> Self {
        Self {
            version: 2,
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
    pub internal_input_resolve_ns: u64,
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
pub struct TileProfileOverheadBreakdown {
    pub external_input_resolve_ns: u64,
    pub internal_input_resolve_ns: u64,
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
    fn measured_total_ns(&self) -> u64 {
        self.external_input_resolve_ns
            .saturating_add(self.internal_input_resolve_ns)
            .saturating_add(self.output_store_ns)
            .saturating_add(self.trace_serialize_ns)
            .saturating_add(self.draft_capture_ns)
            .saturating_add(self.scope_enter_ns)
            .saturating_add(self.output_record_build_ns)
            .saturating_add(self.trace_event_publish_ns)
            .saturating_add(self.output_coordinate_publish_ns)
            .saturating_add(self.other_wrapper_ns)
    }

    fn normalized_for_total(mut self, total_duration_ns: u64, user_duration_ns: u64) -> Self {
        let expected_overhead_ns = total_duration_ns.saturating_sub(user_duration_ns);
        let measured_overhead_ns = self.measured_total_ns();
        self.other_wrapper_ns = self
            .other_wrapper_ns
            .saturating_add(expected_overhead_ns.saturating_sub(measured_overhead_ns));
        self
    }

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

enum ProfileStreamMessage {
    Event(ProfileStreamEvent),
    Shutdown,
}

struct ProfileStreamHandle {
    sender: SyncSender<ProfileStreamMessage>,
    join_handle: JoinHandle<std::io::Result<()>>,
    run_id: String,
}

struct ProfilerState {
    output_path: Option<PathBuf>,
    stream: Option<ProfileStreamHandle>,
    next_invocation_index: u64,
    active_sequences: Vec<ActiveSequenceFrame>,
    records: Vec<ProfileRecord>,
    program_total_duration_ns: Option<u64>,
}

#[derive(Debug)]
struct ActiveSequenceFrame {
    sequence_id: String,
    depth: u32,
    child_duration_ns: u64,
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self {
            output_path: None,
            stream: None,
            next_invocation_index: 0,
            active_sequences: Vec::new(),
            records: Vec::new(),
            program_total_duration_ns: None,
        }
    }
}

impl ProfilerState {
    fn reset(
        &mut self,
        output_path: Option<PathBuf>,
        stream_path: Option<PathBuf>,
    ) -> std::io::Result<()> {
        self.shutdown_stream()?;
        self.output_path = output_path;
        self.next_invocation_index = 0;
        self.active_sequences.clear();
        self.records.clear();
        self.program_total_duration_ns = None;

        if let Some(path) = stream_path {
            let stream = spawn_stream_writer(path)?;
            let run_id = stream.run_id.clone();
            self.stream = Some(stream);
            self.send_stream_event(ProfileStreamEvent::RunStarted { run_id })?;
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

thread_local! {
    static PROFILER_STATE: RefCell<ProfilerState> = RefCell::new(ProfilerState::default());
}

pub fn init_from_env() {
    let output_path = std::env::var_os(PROFILE_PATH_ENV).map(PathBuf::from);
    let stream_path = std::env::var_os(PROFILE_STREAM_PATH_ENV).map(PathBuf::from);
    PROFILER_STATE.with(|state| {
        state
            .borrow_mut()
            .reset(output_path, stream_path)
            .unwrap_or_else(|error| panic!("Failed to initialize profiler state: {}", error));
    });
}

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
        });
    });
}

pub fn finish_sequence_profile(sequence_id: &str, total_duration_ns: u64) {
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
        });
        state.records.push(record.clone());
        state
            .send_stream_event(ProfileStreamEvent::Record(record))
            .unwrap_or_else(|error| panic!("Failed to stream sequence profile: {}", error));
    });
}

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
            internal_input_resolve_ns: overhead_breakdown.internal_input_resolve_ns,
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

pub fn finish() -> std::io::Result<Option<PathBuf>> {
    PROFILER_STATE.with(|state| {
        let mut state = state.borrow_mut();

        if let Some(run_id) = state.stream.as_ref().map(|stream| stream.run_id.clone()) {
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

        let profile = ExecutionProfile::new(state.records.clone(), state.program_total_duration_ns);
        let bytes = serde_json::to_vec_pretty(&profile).map_err(std::io::Error::other)?;
        fs::write(&path, bytes)?;

        Ok(Some(path))
    })
}

fn generate_run_id() -> String {
    let timestamp_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), timestamp_ns)
}

fn spawn_stream_writer(path: PathBuf) -> std::io::Result<ProfileStreamHandle> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let run_id = generate_run_id();
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
        run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_profiler() {
        PROFILER_STATE.with(|state| {
            state
                .borrow_mut()
                .reset(Some(PathBuf::from("test-profile.json")), None)
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
        finish_sequence_profile("nested", 25);
        finish_sequence_profile("main", 100);

        let profile = PROFILER_STATE.with(|state| {
            let state = state.borrow();
            ExecutionProfile::new(state.records.clone(), state.program_total_duration_ns)
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
        finish_sequence_profile("main", 50);

        let profile = PROFILER_STATE.with(|state| {
            let state = state.borrow();
            ExecutionProfile::new(state.records.clone(), state.program_total_duration_ns)
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
}
