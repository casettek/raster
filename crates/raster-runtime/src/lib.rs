//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

mod external_storage;
mod internal_storage;
pub mod input;
mod raster_index;
pub mod tracing;
pub use input::{
    encode_raster_value, resolve_external_value, resolve_typed_external_value, write_raster_files,
};
pub use internal_storage::{
    global_internal_store_snapshot, resolve_internal_value, store_internal_value,
    internal_write_witness, InternalStorageManager, InternalStoreSnapshot, InternalWriteRecord,
    StoredInternalObject,
};
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event,
    publisher::{Publisher, TraceEventPublisher},
    recorder::TraceRecorder,
    TRACE_EVENT_PREFIX,
};
