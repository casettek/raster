//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

pub extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub use raster_core as core;
pub use raster_macros::{sequence, tile};

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{publish_trace_event, finish, init, init_with};

#[cfg(feature = "std")]
pub mod utils;
#[cfg(feature = "std")]
pub use utils::exec_helper::{parse_main_input, try_execute_tile_from_args};

/// Canonical call primitive for invoking a tile inside a sequence.
///
/// `call!` is the explicit "step boundary" — use it instead of bare function calls
/// in sequences to enable reliable compiler call extraction, trace emission, and
/// CFS derivation.
///
/// # Usage
/// ```ignore
/// let greeting = call!(greet, name);
/// let result = call!(exclaim, greeting);
/// ```
///
/// On `std` + non-riscv32 targets, the underlying tile function's `#[tile]` wrapper
/// handles trace emission (`TraceEvent::Tile`). On `no_std` / riscv32 targets,
/// this expands to a plain function call with no overhead.
///
/// Bare function calls in sequences are soft-deprecated. Use `call!` for all
/// tile invocations to ensure the compiler can extract calls reliably.
#[macro_export]
macro_rules! call {
    ($tile:ident $(,)?) => {
        $tile()
    };
    ($tile:ident, $($args:expr),+ $(,)?) => {
        $tile($($args),+)
    };
}

/// Canonical call primitive for invoking a sub-sequence inside a sequence.
///
/// `call_seq!` is the explicit "sequence call boundary" — use it instead of bare
/// function calls when invoking another sequence from within a sequence.
///
/// # Usage
/// ```ignore
/// let result = call_seq!(wish_sequence, greeting);
/// ```
///
/// Semantically distinct from `call!`: invoking a sequence means the callee will
/// fire `SequenceStart` at entry and `SequenceEnd` on return (from the `#[sequence]`
/// macro on the callee side). `call_seq!` just invokes the function; the callee's
/// own `#[sequence]` wrapper handles sequence-level trace events.
///
/// On `no_std` / riscv32 targets, this expands to a plain function call.
///
/// Bare function calls to sequences are soft-deprecated. Use `call_seq!` for all
/// sequence invocations.
#[macro_export]
macro_rules! call_seq {
    ($seq:ident $(,)?) => {
        $seq()
    };
    ($seq:ident, $($args:expr),+ $(,)?) => {
        $seq($($args),+)
    };
}

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
        trace::{FnCallRecord, FnInput, FnInputArgs, FnOutput, TileExecRecord},
    };

    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        find_sequence, find_tile, find_tile_by_str, iter_sequences, iter_tiles, sequence_count,
        tile_count, SequenceMetadataStatic, SequenceRegistration, TileRegistration,
    };

    pub use crate::{call, call_seq, sequence, tile};

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    // Tile execution helper for native backend
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::try_execute_tile_from_args;
}
