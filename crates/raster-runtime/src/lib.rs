//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracing;
pub use tracing::{
    emit_trace_event, finish, init, init_with,
    subscriber::{ExecutionSubscriber, Subscriber},
};
