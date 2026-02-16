//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracing;
pub use tracing::{
    __emit_trace, finish, init, init_with,
    subscriber::{
        audit::AuditSubscriber, commit::CommitSubscriber, json::JsonSubscriber, Subscriber,
    },
};
