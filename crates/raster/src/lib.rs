//! Raster: A Rust-based developer toolchain for tile-based execution.
//!
//! This is the main entry point for user applications. It re-exports the core
//! functionality from other Raster crates.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

pub extern crate alloc;
use serde::{de::DeserializeOwned, Serialize};

#[cfg(feature = "std")]
extern crate std;

pub use raster_core as core;
pub use raster_core::external::{External, ExternalRef};
pub use raster_macros::{sequence, tile};

pub fn external<T>(name: &str) -> External<T> {
    External::new(name)
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: External<T>,
    expected_name: &str,
) -> raster_core::Result<raster_core::external::ExternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return utils::input::resolve_external_value(reference, expected_name);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        let _ = expected_name;
        Err(raster_core::Error::Other(alloc::format!(
            "External input resolution requires the `std` feature"
        )))
    }
}

// Runtime is only available with std feature
#[cfg(feature = "std")]
pub use raster_runtime::{finish, init, init_with, publish_trace_event};

#[cfg(feature = "std")]
pub mod utils;
#[cfg(feature = "std")]
pub use utils::input::parse_main_input_value;

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

/// Creates a typed external-input reference for `#[external(...)]` parameters.
#[macro_export]
macro_rules! external {
    ($name:literal) => {
        $crate::external($name)
    };
    ($name:expr) => {
        $crate::external($name)
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
        external::{External, ExternalRef},
        tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic},
        Result,
    };

    // These modules require std
    #[cfg(feature = "std")]
    pub use crate::core::{
        manifest::Manifest,
        schema::{ControlFlow, SequenceSchema},
        trace::{FnCallRecord, FnInput, FnInputArg, FnOutput, TileExecRecord},
    };

    // Registry is only available with std and on platforms that support linkme
    #[cfg(all(feature = "std", not(target_arch = "riscv32")))]
    pub use crate::core::registry::{
        find_sequence, find_tile, find_tile_by_str, iter_sequences, iter_tiles, sequence_count,
        tile_count, SequenceMetadataStatic, SequenceRegistration, TileRegistration,
    };

    pub use crate::{call, call_seq, external, sequence, tile};

    // TODO: Re-enable once Executor/Tracer types are implemented
    // #[cfg(feature = "std")]
    // pub use crate::{Executor, Tracer, FileTracer, NoOpTracer};

    #[cfg(feature = "std")]
    pub use crate::{parse_main_input_value, resolve_external_value};
}
