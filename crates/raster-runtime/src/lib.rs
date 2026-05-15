//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod input;
pub mod tracing;
pub use input::{
    parse_program_input, parse_program_input_value, resolve_external_value,
    resolve_typed_external_value,
};
pub use tracing::{
    commitment::Sha256Commitment,
    finish, init, init_with, publish_trace_event,
    publisher::{Publisher, TraceEventPublisher},
    recorder::TraceRecorder,
    TRACE_EVENT_PREFIX,
};
