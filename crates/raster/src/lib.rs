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
pub use raster_macros::{main, sequence, tile};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{emit_trace, emit_trace_event, finish, init, init_with, JsonSubscriber};

#[cfg(feature = "std")]
pub mod utils;
#[cfg(feature = "std")]
pub use utils::exec_helper::{parse_main_input, try_execute_tile_from_args};

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
        trace::{FnCallRecord, FnInputParam, StepRecord},
    };

    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        find_sequence, find_tile, find_tile_by_str, iter_sequences, iter_tiles, sequence_count,
        tile_count, SequenceMetadataStatic, SequenceRegistration, TileRegistration,
    };

    pub use crate::{sequence, tile};

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    // Tile execution helper for native backend
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::try_execute_tile_from_args;
}
