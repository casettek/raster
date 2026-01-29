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
pub use raster_macros::{tile, sequence, main};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{JsonSubscriber, init, init_with, __emit_trace, ExecutionCommitmentSubscriber};
// Tile execution helper for native backend subprocess communication
#[cfg(feature = "std")]
mod exec_helper;

#[cfg(feature = "std")]
pub use exec_helper::{try_execute_tile_from_args, parse_main_input};

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
        trace::{Trace, TraceEvent, TileTraceItem, TraceInputParam},
    };
    
    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        TileRegistration, iter_tiles, find_tile, find_tile_by_str, tile_count,
        SequenceRegistration, SequenceMetadataStatic, iter_sequences, find_sequence, sequence_count,
    };
    
    pub use crate::{tile, sequence};
    
    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};
    
    // Tile execution helper for native backend
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::try_execute_tile_from_args;
}
