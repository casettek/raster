//! Native execution and tracing runtime for Raster.
//!
//! This crate provides:
//! - Tile execution in native mode
//! - Optional execution tracing
//! - Trace capture and storage

pub mod tracer;

pub use tracer::{JsonSubscriber, Subscriber};

// Re-export tile trace emission function for use by generated macro code
#[doc(hidden)]
pub use tracer::emit_trace;
