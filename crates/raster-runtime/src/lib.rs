//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

mod backing;
mod entry_arguments;
pub mod input;
pub mod profiling;
mod raster_index;
mod source;
mod storage;
pub mod tracing;
pub use entry_arguments::{
    bind_entry_arguments, entry_argument_spec, EntryArgumentSpec, EntryArgumentsBinding,
};
pub use input::{
    encode_raster_value, postcard_structural_commitment, select_storage_value, write_raster_files,
};
pub use profiling::{
    begin_sequence_profile, finish_sequence_profile, record_tile_output_store_profile,
    record_tile_profile, ExecutionProfile, ProfileRecord, ProfileStreamEvent,
    SequenceProfileRecord, SequenceProfileSelfBreakdown, TileProfileOverheadBreakdown,
    TileProfileRecord, PROFILE_PATH_ENV, PROFILE_RUN_ID_ENV, PROFILE_STREAM_PATH_ENV,
};
pub use storage::{
    apply_draft_push, apply_draft_set, begin_draft_step_capture, create_draft,
    enter_recur_sequence_iteration_scope, enter_recur_site_scope, enter_sequence_scope,
    exit_recur_sequence_iteration_scope, exit_recur_site_scope, exit_sequence_scope,
    finalize_draft, finalize_empty_draft, finish_draft_step_capture, global_storage_snapshot,
    publish_pending_output_coordinates, resolve_storage_ok_value, resolve_storage_value,
    select_stored_value, store_execution_output_value, store_value, DraftCaptureSnapshot,
    StorageManager, StorageSnapshot, StorageWriteRecord, StoredObject, TileExecutionScopeGuard,
};
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event,
    publishers::{BinaryTraceEventPublisher, JsonTraceEventPublisher, Publisher},
    recorder::TraceRecorder,
    RecurTraceScopeGuard, TraceFormat, TRACE_FORMAT_ENV, TRACE_PATH_ENV,
};
