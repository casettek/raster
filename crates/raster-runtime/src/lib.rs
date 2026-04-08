//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracing;
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event,
    publisher::{Publisher, TraceEventPublisher},
    recorder::TraceRecorder,
    TRACE_EVENT_PREFIX,
};
