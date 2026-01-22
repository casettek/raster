//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod executor;
pub mod tracer;

pub use executor::Executor;
pub use tracer::{FileTracer, NoOpTracer, Tracer};
