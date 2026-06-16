//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

mod external_storage;
pub mod input;
mod internal_storage;
mod raster_index;
pub mod tracing;
pub use input::{
    encode_raster_value, resolve_external_value, resolve_typed_external_value, select_external_arg,
    select_internal_value, write_raster_files,
};
pub use internal_storage::{
    apply_draft_push, apply_draft_set, begin_draft_step_capture, create_draft,
    enter_recur_site_scope, enter_sequence_scope, exit_recur_site_scope, exit_sequence_scope,
    finalize_draft, finalize_empty_draft, finish_draft_step_capture,
    global_internal_store_snapshot, resolve_internal_ok_value, resolve_internal_value,
    publish_pending_output_coordinates, store_execution_output_value, store_internal_value,
    DraftCaptureSnapshot, InternalStorageManager, InternalStoreSnapshot, InternalWriteRecord,
    StoredInternalObject, TileExecutionScopeGuard,
};
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event, RecurTraceScopeGuard,
    publisher::{Publisher, TraceEventPublisher},
    recorder::TraceRecorder,
    TRACE_EVENT_PREFIX,
};
