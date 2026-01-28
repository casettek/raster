//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracer;

pub use tracer::{JsonSubscriber, Subscriber, init, init_with, __emit_trace};