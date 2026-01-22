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
pub use raster_macros::{sequence, tile};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{Executor, FileTracer, NoOpTracer, Tracer};

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::core::{
        tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic},
        Result,
    };

    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        manifest::Manifest,
        schema::{ControlFlow, SequenceSchema},
        trace::{Trace, TraceEvent},
    };

    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        find_sequence, find_tile, find_tile_by_str, iter_sequences, iter_tiles, sequence_count,
        tile_count, SequenceMetadataStatic, SequenceRegistration, TileRegistration,
    };

    pub use crate::{sequence, tile};

    #[cfg(feature = "std")]
    pub use crate::{Executor, FileTracer, NoOpTracer, Tracer};
}
