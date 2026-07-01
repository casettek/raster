//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

mod external_storage;
pub mod input;
mod internal_storage;
pub mod profiling;
mod raster_index;
pub mod tracing;
pub use input::{
    encode_raster_value, external_selection_witness, resolve_external_value,
    resolve_typed_external_value, select_external_arg, select_internal_value,
    trace_raster_external_binding, write_raster_files,
};
pub use internal_storage::{
    apply_draft_push, apply_draft_set, begin_draft_step_capture, create_draft,
    enter_recur_sequence_iteration_scope, enter_recur_site_scope, enter_sequence_scope,
    exit_recur_sequence_iteration_scope, exit_recur_site_scope, exit_sequence_scope,
    finalize_draft, finalize_empty_draft, finish_draft_step_capture,
    global_internal_store_snapshot, publish_pending_output_coordinates, resolve_internal_ok_value,
    resolve_internal_value, select_stored_internal_value, store_execution_output_value,
    store_internal_value, DraftCaptureSnapshot, InternalStorageManager, InternalStoreSnapshot,
    InternalWriteRecord, StoredInternalObject, TileExecutionScopeGuard,
};
pub use profiling::{
    begin_sequence_profile, finish_sequence_profile, record_tile_output_store_profile,
    record_tile_profile, ExecutionProfile, ProfileRecord, ProfileStreamEvent,
    SequenceProfileRecord, SequenceProfileSelfBreakdown, TileProfileOverheadBreakdown,
    TileProfileRecord, PROFILE_PATH_ENV, PROFILE_RUN_ID_ENV, PROFILE_STREAM_PATH_ENV,
};
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event,
    publishers::{BinaryTraceEventPublisher, JsonTraceEventPublisher, Publisher},
    recorder::TraceRecorder,
    RecurTraceScopeGuard, TraceFormat, TRACE_FORMAT_ENV, TRACE_PATH_ENV,
};
