//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracing;
pub use tracing::{
    finish, init, init_with,
    __emit_trace,
    subscriber::{
        commit::CommitSubscriber, json::JsonSubscriber, verify::VerifySubscriber, Subscriber,
    },
};
