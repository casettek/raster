//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub use raster_core as core;
pub use raster_macros::{tile, sequence};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{Executor, Tracer, FileTracer, NoOpTracer};

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::core::{
        tile::{TileId, TileMetadata, TileIdStatic, TileMetadataStatic},
        Result,
    };
    
    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        schema::{SequenceSchema, ControlFlow},
        manifest::Manifest,
        trace::{Trace, TraceEvent},
    };
    
    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        TileRegistration, iter_tiles, find_tile, find_tile_by_str, tile_count,
        SequenceRegistration, SequenceMetadataStatic, iter_sequences, find_sequence, sequence_count,
    };
    
    pub use crate::{tile, sequence};
    
    #[cfg(feature = "std")]
    pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};
}
