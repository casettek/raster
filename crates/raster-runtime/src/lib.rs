//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracing;
pub use tracing::{
    recorder::TraceRecorder,
    commitment::Sha256Commitment,
    publish_trace_event, finish, init, init_with,
    publisher::{Publisher, TraceEventPublisher},
    TRACE_EVENT_PREFIX,
};
