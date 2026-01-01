//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.

pub use raster_core as core;
pub use raster_macros::{tile, sequence};
pub use raster_runtime::{Executor, Tracer, FileTracer, NoOpTracer};

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::core::{
        tile::{TileId, TileMetadata, TileIdStatic, TileMetadataStatic},
        schema::{SequenceSchema, ControlFlow},
        manifest::Manifest,
        trace::{Trace, TraceEvent},
        registry::{TileRegistration, iter_tiles, find_tile, find_tile_by_str, tile_count},
        Result,
    };
    pub use crate::{tile, sequence};
    pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};
}
